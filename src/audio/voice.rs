//! Who may capture, and where the frames go.
//!
//! The link side (`link`) and the audio tasks never touch each other: a
//! link task fills [`SPEAKER_FRAMES`] from the host's speaker frames and
//! drains its own [`MIC_FRAMES`] queue into its link (mic notifications of
//! the audio service over BLE, channel-7 reports over the cable); the
//! microphone task waits on [`MIC`]. Nothing here blocks or allocates, so a
//! link task can call it without awaiting.
//!
//! A host listens per link: the BLE host by subscribing to the mic
//! characteristic, the USB host with a LISTEN frame. A stream goes to one
//! link, chosen when it starts ([`audio_link`]: the cable first, as the deck
//! events go), and ends with `END_ERROR` if that link stops listening.
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

use super::codec;
use super::policy;
use crate::deck::msg::Event;
use crate::link::Link;

/// One frame on its way to or from a link.
#[derive(Clone)]
pub struct Frame {
    pub len: u8,
    pub bytes: [u8; codec::MAX_FRAME],
    /// Preserve the transport through the queue; only USB may use 48 kHz.
    pub usb: bool,
}

impl Frame {
    pub fn new(bytes: &[u8]) -> Option<Self> {
        let len = bytes.len();
        if len == 0 || len > codec::MAX_FRAME {
            return None;
        }
        let mut frame = Self {
            len: len as u8,
            bytes: [0; codec::MAX_FRAME],
            usb: false,
        };
        frame.bytes[..len].copy_from_slice(bytes);
        Some(frame)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

type FrameQueue = Channel<CriticalSectionRawMutex, Frame, 32>;

/// Encoded microphone frames (and, over the cable, the INFO answer), one
/// queue per link: 640 ms of 20 ms frames over BLE, enough to cover the
/// link's switch to its fast parameters at the start of a stream
/// (`ACTIVITY`); 220 ms of the cable's shorter frames.
pub static MIC_FRAMES: [FrameQueue; Link::COUNT] = [Channel::new(), Channel::new()];
/// Speaker jitter/startup buffer: 293 ms at 48 kHz with 110-sample USB
/// frames. Covers the 113 ms amplifier startup plus host lead and bursts.
pub static SPEAKER_FRAMES: Channel<CriticalSectionRawMutex, Frame, 128> = Channel::new();

pub use super::policy::MicRequest;

/// What the microphone should do next, in order: a release and a quick
/// re-press must come out as END then a new START, never as one stream
/// running across two PTT sessions.
pub static MIC: Channel<CriticalSectionRawMutex, MicRequest, 8> = Channel::new();

/// Queue a request for the microphone task. The task drains the queue all
/// the time, so it is only full if that task is wedged; then the stale
/// requests go and the newest stays, so a Stop is never lost.
fn request(request: MicRequest) {
    if MIC.try_send(request).is_err() {
        MIC.clear();
        let _ = MIC.try_send(request);
    }
}

/// Audio is flowing or about to over Bluetooth (a microphone stream
/// starting, a speaker frame arriving). The radio side answers by asking
/// the host for a fast link (no peripheral latency) and relaxes it again
/// once audio stops: at the latency macOS picks for an idle deck (22 events
/// of 15 ms), the deck's radio sleeps through a third of a second at a
/// time, which neither direction of a 50-frame-a-second stream survives.
/// The cable has no such thing, so audio there never signals it.
pub static ACTIVITY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Per link: the host is listening (subscribed to mic notifications, or
/// LISTEN on the cable). Nothing is captured for a link nobody listens on:
/// there would be nobody to hear it.
static SUBSCRIBED: [AtomicBool; Link::COUNT] = [AtomicBool::new(false), AtomicBool::new(false)];
/// Per link: the largest frame it carries (`ATT_MTU - 3` over BLE, a
/// report's payload over the cable); 20 until the link says more.
static MAX_VALUE: [AtomicU16; Link::COUNT] = [AtomicU16::new(20), AtomicU16::new(20)];
/// A microphone stream is live. The speaker discards frames meanwhile
/// (walkie-talkie): the deck's own speaker reaches its microphone at about
/// +17 dB and nothing cancels the echo.
pub static CAPTURING: AtomicBool = AtomicBool::new(false);
/// ...except in loopback mode, which is exactly that echo, on purpose.
pub static LOOPBACK: AtomicBool = AtomicBool::new(false);
/// The speaker is playing (the amplifier is on).
pub static PLAYING: AtomicBool = AtomicBool::new(false);

/// The host on `link` started (or stopped) listening; `max_value` is the
/// largest frame that link takes.
pub fn subscribe(link: Link, on: bool, max_value: u16) {
    MAX_VALUE[link.index()].store(max_value, Ordering::Release);
    SUBSCRIBED[link.index()].store(on, Ordering::Release);
}

/// The link's frame limit changed (a BLE MTU exchange).
pub fn set_max_value(link: Link, max_value: u16) {
    MAX_VALUE[link.index()].store(max_value, Ordering::Release);
}

/// Whether the host on `link` is listening.
pub fn subscribed(link: Link) -> bool {
    SUBSCRIBED[link.index()].load(Ordering::Acquire)
}

/// The link a microphone stream starting now goes to: the first listening
/// link in the deck's order of preference (the cable first), if any.
pub fn audio_link() -> Option<Link> {
    Link::PREFERENCE.into_iter().find(|link| subscribed(*link))
}

/// A deck event from the UI (`policy::ptt`): a PTT press opens the
/// microphone for a present host, its release always closes it.
pub fn on_deck_event(event: &Event, online: bool) {
    if let Some(r) = policy::ptt(event, online) {
        request(r);
    }
}

/// A frame the host on `link` sent (a value written to the speaker
/// characteristic, a channel-7 report). MIC requests are acted on here;
/// everything else queues for the speaker. Returns false when a frame had
/// to be dropped.
pub fn host_frame(link: Link, bytes: &[u8]) -> bool {
    if crate::link::charging_only() {
        return false;
    }
    match policy::host_frame(&codec::parse(bytes)) {
        policy::HostFrame::Mic(r) => {
            request(r);
            true
        }
        policy::HostFrame::Ignore => true,
        policy::HostFrame::Speaker => {
            if link == Link::Ble {
                ACTIVITY.signal(());
            }
            match Frame::new(bytes) {
                Some(mut frame) => {
                    frame.usb = link == Link::Usb;
                    let sent = SPEAKER_FRAMES.try_send(frame).is_ok();
                    #[cfg(feature = "audio-usb-rate-test")]
                    if !sent && link == Link::Usb { super::usb_test::queue_drop(); }
                    sent
                }
                None => false,
            }
        }
    }
}

/// `link` went away: its host is no longer listening, and whatever was
/// queued for it has nowhere to go. A stream bound to it notices between
/// frames and ends with `END_ERROR`; a stream on the other link carries on.
pub fn link_lost(link: Link) {
    SUBSCRIBED[link.index()].store(false, Ordering::Release);
    MAX_VALUE[link.index()].store(20, Ordering::Release);
    MIC_FRAMES[link.index()].clear();
}

/// End every microphone owner and discard pending playback before a
/// charging-only shutdown. The microphone task closes its own DMA session.
#[cfg(feature = "hw-b")]
pub fn power_down() {
    MIC.clear();
    request(MicRequest::Stop { reason: codec::END_ABORT, host: false });
    SPEAKER_FRAMES.clear();
    link_lost(Link::Usb);
    link_lost(Link::Ble);
    super::arcade::publish(0);
}

/// How many samples a microphone frame may carry on `link`.
pub fn samples_per_frame(link: Link) -> usize {
    codec::samples_per_frame(usize::from(MAX_VALUE[link.index()].load(Ordering::Acquire)))
}

/// Bench only (`audio-debug`): a capture that stays on the deck. The
/// microphone records into RAM instead of the radio, and the speaker can
/// play its own silence (or a quiet tone) with no host stream, so the
/// speaker path's noise can be measured with the Bluetooth link idle.
#[cfg(feature = "audio-debug")]
pub mod bench {
    use super::*;
    use core::sync::atomic::AtomicU32;

    /// Four seconds at 16 kHz.
    pub const SAMPLES: usize = 64_000;
    static mut REC: [i16; SAMPLES] = [0; SAMPLES];
    /// The microphone records here rather than streaming.
    pub static LOCAL: AtomicBool = AtomicBool::new(false);
    /// The speaker keeps playing (silence, or the tone) with no host frames.
    pub static HOLD: AtomicBool = AtomicBool::new(false);
    /// The local tone's peak, in 16-bit sample units; 0 is silence.
    pub static TONE: AtomicU16 = AtomicU16::new(0);
    pub static WANT: AtomicU32 = AtomicU32::new(0);
    pub static HAVE: AtomicU32 = AtomicU32::new(0);

    /// One cycle of 16 kHz / 13 = 1230.8 Hz at full scale: between the
    /// lines radio activity puts at multiples of 8.33 Hz.
    const SINE: [i16; 13] = [0, 15228, 26967, 32528, 30638, 21729, 7842, -7842, -21729, -30638, -32528, -26967, -15228];

    /// Fill `pcm` with the local tone at `peak`, carrying `phase` on.
    pub fn tone(pcm: &mut [i16], peak: u16, phase: &mut usize) {
        for sample in pcm {
            *sample = ((i32::from(SINE[*phase]) * i32::from(peak)) >> 15) as i16;
            *phase = (*phase + 1) % SINE.len();
        }
    }

    /// Start a local capture of `samples`; with `speaker`, the speaker
    /// plays `tone` (0: silence) for as long as it runs.
    pub fn start(samples: u32, speaker: bool, tone: u16) {
        HAVE.store(0, Ordering::Release);
        WANT.store(samples.min(SAMPLES as u32), Ordering::Release);
        TONE.store(tone, Ordering::Release);
        LOCAL.store(true, Ordering::Release);
        if speaker {
            HOLD.store(true, Ordering::Release);
            let mut bytes = [0u8; 5];
            let n = codec::start_frame(0xDB, &mut bytes);
            if let Some(frame) = Frame::new(&bytes[..n]) {
                let _ = SPEAKER_FRAMES.try_send(frame);
            }
        }
        request(MicRequest::Start {
            mode: codec::MIC_LOOPBACK,
            limit_s: 60,
            host: true,
        });
    }

    /// Append captured samples; `true` once the capture is complete.
    pub fn record(pcm: &[i16]) -> bool {
        let have = HAVE.load(Ordering::Acquire) as usize;
        let want = WANT.load(Ordering::Acquire) as usize;
        let n = pcm.len().min(want.saturating_sub(have));
        // SAFETY: the microphone task is the only writer, and the reader
        // (`read`) only looks below `HAVE`, which moves after the copy.
        unsafe {
            let rec = &mut *core::ptr::addr_of_mut!(REC);
            rec[have..have + n].copy_from_slice(&pcm[..n]);
        }
        HAVE.store((have + n) as u32, Ordering::Release);
        have + n >= want
    }

    /// The capture is over: release the speaker too.
    pub fn finish() {
        LOCAL.store(false, Ordering::Release);
        HOLD.store(false, Ordering::Release);
        TONE.store(0, Ordering::Release);
    }

    /// Captured samples from `offset`, as many as fit `out`.
    pub fn read(offset: usize, out: &mut [i16]) -> usize {
        let have = HAVE.load(Ordering::Acquire) as usize;
        let n = out.len().min(have.saturating_sub(offset));
        // SAFETY: below `HAVE`, which the writer has finished with.
        unsafe {
            let rec = &*core::ptr::addr_of!(REC);
            out[..n].copy_from_slice(&rec[offset..offset + n]);
        }
        n
    }
}
