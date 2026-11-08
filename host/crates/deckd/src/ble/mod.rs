//! The deck's voice link: its microphone and speaker as our own packets over
//! a custom GATT service, on the same BLE link as its HID device
//! (`docs/deck-protocol.md`, "Voice (BLE audio service, Rev A Rust
//! firmware)").
//!
//! The deck stopped being a Bluetooth Classic audio device. HFP and A2DP
//! made the deck the Mac's default input and output, so every app that
//! recorded flipped it into SCO and back (the whole of `audio::route` exists
//! to survive that), and bring-up took a second before the first non-zero
//! sample. Over GATT the audio is ours: 16 kHz IMA ADPCM frames that nobody
//! else opens, on a link the system already keeps up for HID. The price is
//! that the host attaches to the service itself, with CoreBluetooth, and
//! that is what this module does.
//!
//! The layout follows the rest of deckd: the logic is small pure state
//! machines with their tests beside them, and the platform is a thin layer
//! behind a seam.
//!
//! * [`codec`] is the firmware's own `src/audio/codec.rs`, compiled here by
//!   path so both ends share one implementation of every bit.
//! * [`mic`] folds notification values into [`MicEvent`]s; [`sendq`] is the
//!   bounded outbox in front of CoreBluetooth's flow control; [`attach`] is
//!   the attach policy (find, connect, discover, subscribe, retry).
//! * [`link`] is the handle, [`BleAudio`]: state, channels and the outbox,
//!   shared between the caller's threads and the platform. [`FakeDeck`]
//!   drives the same handle without a radio.
//! * `cb` and `central` (macOS only) are the hand-written CoreBluetooth
//!   bindings and the engine that runs them on a private serial queue, plus
//!   `look`, which only looks (it never connects).
//! * [`signal`] and [`probe`] are `deckd ble probe` and `deckd ble scan`,
//!   the bench tools.

// The firmware's file, byte for byte: never copy it and never edit it from
// here. `rustfmt::skip` keeps `rustfmt` on this crate's files from
// re-formatting a file this crate does not own, and its style is the
// firmware's: `as_chunks` is newer than what the deck's `no_std` code is
// written against, so that one lint stays off here.
#[rustfmt::skip]
#[allow(clippy::chunks_exact_to_as_chunks)]
#[path = "../../../../../src/audio/codec.rs"]
pub mod codec;

pub mod attach;
#[cfg(target_os = "linux")]
mod bluez;
#[cfg(target_os = "macos")]
mod cb;
#[cfg(target_os = "macos")]
mod central;
pub mod link;
pub mod mic;
pub mod probe;
pub mod sendq;
pub mod signal;

pub use attach::Reach;
pub use link::{BleAudio, FakeDeck, LinkState, Peer, SendError, Stats, Transport, UsbPort};
pub use mic::{MicDecoder, MicEvent, MicStats};
pub use sendq::SendQueue;

#[cfg(target_os = "macos")]
pub use central::{look, Look, Sighting};

/// The audio service (primary).
pub const AUDIO_SERVICE_UUID: &str = "e9895507-bb72-45e5-a802-ab38fe1e4bc1";
/// Mic, deck to host: notify, encrypted CCCD. Every value is one frame.
pub const MIC_UUID: &str = "6e25df5b-b48a-4203-bb04-f3d4caeb1b7a";
/// Speaker, host to deck: write without response (and write), encrypted.
/// MIC requests travel here too.
pub const SPEAKER_UUID: &str = "84de7298-ab42-46e4-9c02-f6a65d990807";
/// Info: read, five bytes (see [`Info`]).
pub const INFO_UUID: &str = "a2259f3a-8d16-4c1e-a243-78f7206e35f5";
/// The Battery service, which macOS discovers for every HID-over-GATT
/// device it connects. The audio service is only in the system's GATT cache
/// if something already discovered it, so a connected deck is looked up by
/// this service and its name as a second try.
pub const BATTERY_SERVICE_UUID: &str = "180F";
/// The deck's GAP name (the same name `audio::coreaudio` matched on).
pub const DECK_NAME: &str = "OpenWalkieTalkie";

/// The info characteristic's version this host understands.
pub const INFO_VERSION: u8 = 1;
/// Info codec bit: IMA ADPCM.
pub const INFO_CODEC_IMA_ADPCM: u8 = 0x01;
/// Info feature bits.
pub const FEATURE_MIC: u8 = 1 << 0;
pub const FEATURE_SPEAKER: u8 = 1 << 1;

/// The info characteristic, decoded: `[version, codec bits, rate kHz,
/// frame ms, features]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub version: u8,
    pub codecs: u8,
    pub rate_khz: u8,
    pub frame_ms: u8,
    pub features: u8,
}

impl Info {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 5]) -> Info {
        let [version, codecs, rate_khz, frame_ms, features] = bytes;
        Info {
            version,
            codecs,
            rate_khz,
            frame_ms,
            features,
        }
    }

    #[must_use]
    pub fn mic(&self) -> bool {
        self.features & FEATURE_MIC != 0
    }

    #[must_use]
    pub fn speaker(&self) -> bool {
        self.features & FEATURE_SPEAKER != 0
    }

    /// The deck speaks this host's dialect: version 1, IMA ADPCM, 16 kHz.
    #[must_use]
    pub fn compatible(&self) -> bool {
        self.version == INFO_VERSION
            && self.codecs & INFO_CODEC_IMA_ADPCM != 0
            && self.rate_khz == codec::RATE_KHZ
    }
}

impl std::fmt::Display for Info {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let codec = if self.codecs & INFO_CODEC_IMA_ADPCM != 0 {
            "IMA ADPCM"
        } else {
            "no known codec"
        };
        let features = match (self.mic(), self.speaker()) {
            (true, true) => "mic + speaker",
            (true, false) => "mic only",
            (false, true) => "speaker only",
            (false, false) => "no mic, no speaker",
        };
        write!(
            f,
            "version {}, {codec}, {} kHz, {} ms, {features}",
            self.version, self.rate_khz, self.frame_ms
        )
    }
}

/// Bytes as `01 01 10 14 03`, the way the probe prints a value.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn well_formed(uuid: &str) -> bool {
        uuid.len() == 36
            && uuid.char_indices().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit() && !c.is_ascii_uppercase(),
            })
    }

    #[test]
    fn the_uuids_are_the_ones_the_protocol_document_publishes() {
        let all = [AUDIO_SERVICE_UUID, MIC_UUID, SPEAKER_UUID, INFO_UUID];
        for uuid in all {
            assert!(well_formed(uuid), "{uuid}");
        }
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len());

        // One fact written twice: the firmware engineer reads the document,
        // deckd reads these constants. Scope the search to the voice section.
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/deck-protocol.md");
        let doc = std::fs::read_to_string(&path).expect("docs/deck-protocol.md");
        let at = doc
            .find("## Voice (BLE audio service")
            .expect("the protocol document still has its voice section");
        let voice = &doc[at..];
        for (uuid, row) in [
            (AUDIO_SERVICE_UUID, "Audio service"),
            (MIC_UUID, "Mic (deck to host)"),
            (SPEAKER_UUID, "Speaker (host to deck)"),
            (INFO_UUID, "Info"),
        ] {
            let line = voice
                .lines()
                .find(|l| l.starts_with(&format!("| {row} |")))
                .unwrap_or_else(|| panic!("no `{row}` row in the voice section"));
            assert!(line.contains(uuid), "{row}: {line}");
        }
    }

    #[test]
    fn info_reads_the_documented_layout() {
        let info = Info::from_bytes([1, 0x01, 16, 20, 0x03]);
        assert!(info.compatible() && info.mic() && info.speaker());
        assert_eq!(
            info.to_string(),
            "version 1, IMA ADPCM, 16 kHz, 20 ms, mic + speaker"
        );
        let mic_only = Info::from_bytes([1, 0x01, 16, 20, FEATURE_MIC]);
        assert!(mic_only.mic() && !mic_only.speaker());
        assert!(!Info::from_bytes([2, 0x01, 16, 20, 3]).compatible());
        assert!(!Info::from_bytes([1, 0x02, 16, 20, 3]).compatible());
        assert!(!Info::from_bytes([1, 0x01, 8, 20, 3]).compatible());
        assert_eq!(hex(&[1, 1, 16, 20, 3]), "01 01 10 14 03");
    }
}
