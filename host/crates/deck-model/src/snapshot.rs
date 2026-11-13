//! The one `Snapshot` (§4.13) and its parts.
use crate::{Agent, AgentId, FaceEntry, MacroKey, SetKey, Ts, UsageItem};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FocusSource {
    Device,
    Desktop,
    Api,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Focus {
    pub agent: Option<AgentId>,
    /// The last device `channel.seq`.
    pub seq: Option<u16>,
    pub source: Option<FocusSource>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    #[default]
    Offline,
    Connected,
    Degraded,
}

/// §6.3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    #[default]
    Absent,
    Linked,
    AwaitHello,
    Syncing,
    Live,
    Degraded,
}

/// The `set` keys the host persists and restores on `boot:1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    /// UI language shared with capable decks: en, zh-Hans, zh-Hant, ja.
    #[serde(default)]
    pub language: u16,
    #[serde(default)]
    pub language_revision: u32,
    /// 0 off, 1 effects only, 2 music and effects; changed in the Arcade menu.
    #[serde(default)]
    pub arcadesound: u16,
    pub speaker: u16,
    pub mic: u16,
    pub announce: u16,
    pub chime: u16,
    pub autojump: u16,
    pub brightness: u16,
    pub idlehome: u16,
    pub demo: u16,
    /// 0 small, 1 medium, 2 large. The **host** wraps documents with the face
    /// this picks, so it is not just a device preference.
    #[serde(default = "default_textsize")]
    pub textsize: u16,
}

fn default_textsize() -> u16 {
    1
}

impl Default for Settings {
    /// The firmware's power-on defaults (`src/deck/model.rs`).
    fn default() -> Self {
        Settings {
            language: 0,
            language_revision: 0,
            speaker: 6,
            mic: 6,
            announce: 1,
            chime: 1,
            autojump: 0,
            brightness: 5,
            idlehome: 120,
            // Off, as the firmware powers on: without a host the deck shows
            // itself offline, not made-up agents.
            demo: 0,
            textsize: 1,
            arcadesound: 0,
        }
    }
}

impl Settings {
    pub const fn get(&self, key: SetKey) -> u16 {
        match key {
            SetKey::Language => self.language,
            SetKey::Speaker => self.speaker,
            SetKey::Mic => self.mic,
            SetKey::Announce => self.announce,
            SetKey::Chime => self.chime,
            SetKey::Autojump => self.autojump,
            SetKey::Brightness => self.brightness,
            SetKey::Idlehome => self.idlehome,
            SetKey::Demo => self.demo,
            SetKey::Textsize => self.textsize,
            SetKey::Arcadesound => self.arcadesound,
        }
    }

    /// Stores `value` when the device would accept it; returns whether it changed.
    pub fn set(&mut self, key: SetKey, value: u16) -> bool {
        if !key.accepts(value) {
            return false;
        }
        let slot = match key {
            SetKey::Language => &mut self.language,
            SetKey::Speaker => &mut self.speaker,
            SetKey::Mic => &mut self.mic,
            SetKey::Announce => &mut self.announce,
            SetKey::Chime => &mut self.chime,
            SetKey::Autojump => &mut self.autojump,
            SetKey::Brightness => &mut self.brightness,
            SetKey::Idlehome => &mut self.idlehome,
            SetKey::Demo => &mut self.demo,
            SetKey::Textsize => &mut self.textsize,
            SetKey::Arcadesound => &mut self.arcadesound,
        };
        let changed = *slot != value;
        *slot = value;
        changed
    }

    /// Every key with its value, in `boot:1` push order.
    pub fn iter(&self) -> impl Iterator<Item = (SetKey, u16)> + '_ {
        SetKey::ALL.into_iter().map(move |k| (k, self.get(k)))
    }

    pub fn normalize_language(&mut self) {
        if self.language > 3 { self.language = 0; }
        self.language_revision = self.language_revision.max(u32::from(self.language != 0));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrCounts {
    pub range: u32,
    pub parse: u32,
    pub size: u32,
    pub unknown: u32,
}

/// How the deck is linked to this computer: the bus the hid thread opened
/// it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Usb,
    Bluetooth,
}

/// The deck as it was the last time it was linked, kept in `state.json`:
/// while the deck is away, even across a restart, the service can still say
/// when it was last seen and what it ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LastLinked {
    /// When the link went live while it is up; when it ended after that.
    pub at: Ts,
    pub fw: Option<String>,
    pub board: Option<String>,
    pub transport: Option<Transport>,
}

/// A link drop: when it happened, and the hid thread's reason (`read`,
/// `write`, `write_stall`, `removed`, `misses`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkDrop {
    pub at: Ts,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceState {
    #[serde(default)]
    pub language_supported: bool,
    pub link: Link,
    pub session: SessionState,
    /// The session id the link uses: this daemon's, chosen at start (0 until
    /// `Store::new` is told one), and a fresh one whenever another deck than
    /// the last link's is in the hand.
    pub sid: u32,
    pub fw: Option<String>,
    pub boot: Option<bool>,
    /// Unknown until the first `face` event.
    pub face: Option<u8>,
    /// The strip as the device reports it.
    pub faces: Vec<FaceEntry>,
    pub host_online: bool,
    /// The device's settings as last reported/pushed.
    pub settings: Settings,
    pub pace_ms: u16,
    pub outbox_depth: u32,
    pub err_counts: ErrCounts,
    /// The sessions the deck holds (its `hello`'s `agents`); `None` until a
    /// deck has said, and a deck that predates the field holds 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u8>,
    // What the deck says about itself while it is linked. All of it is
    // optional on the wire, so a reader built before these fields and a
    // service that predates them both keep working.
    /// The bus the linked deck is on; `None` while unlinked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// Charge in percent as the deck last reported it. `None` while
    /// unlinked and while the deck does not know, which it sends as 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<u8>,
    /// Whether the cell is taking current, as the deck last reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    /// The board as the deck names it (`Rev A`, `Rev B`, `LilyGo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    /// The last deck that went live here (the linked one, while it is
    /// linked); kept across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<LastLinked>,
    /// The link's recent drops, oldest first: at most 60, none more than an
    /// hour older than the newest. Not kept across a restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drops: Vec<LinkDrop>,
    /// The deck whose strip, settings and Macropad keys the snapshot holds
    /// (the linked one, else the last one identified), as an opaque label:
    /// eight hex digits, the same for one deck every time and never its
    /// address. Each deck has its own; `device.profile` says when another
    /// one's are taken. Absent until a deck has been identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<String>,
}

impl DeviceState {
    /// How many slots the store may assign: the deck's own table.
    #[must_use]
    pub fn capacity(&self) -> u8 {
        self.sessions.unwrap_or(crate::Slot::DEFAULT)
    }
}

/// §7.2's output route state machine.
///
/// B7: the variant was `Sco` where §7.2 says **`ScoOurs`**, and there was no
/// `Reopening` at all — so the state the machine spends the two and a half
/// seconds after every press in (V1: A2DP resume is **2307 ms median**) could
/// not be named, and `voice.route` could not report it. The distinction
/// `ScoOurs` draws is the whole point of the machine: SCO because *we* opened
/// the mic is a state we leave on the release, and SCO because some other app
/// did (V7: the deck is the system default input here, so this is the routine
/// path and not an edge case) is a state we can only wait out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    #[default]
    None,
    /// The output is open at 44.1 kHz and stays open (§7.2).
    A2dp,
    /// Our own PTT capture: lanes held, output closed.
    ScoOurs,
    /// Someone else's capture flipped the rate under us.
    ScoExternal,
    /// Between our capture and A2DP again. V3: it opens at the device's
    /// **current** nominal rate immediately (48 ms to the first render
    /// callback) instead of waiting for 44100 (2591 ms) — a 54× difference
    /// on every post-PTT announcement, and the 2.3 s is a fixed macOS
    /// SCO-teardown timer that cannot be hurried.
    Reopening,
    /// Two failed reopens (§7.2): the output is open at the wrong rate and
    /// resampled, `voice.route{stuck:true}`, recovering on the next rate
    /// change.
    ScoStuck,
    /// `[voice] fallback_output = "system_default"`: not the deck at all.
    SystemDefault,
    /// The BLE voice link (`[voice] transport = "ble"`): the deck's speaker
    /// characteristic is open, at 16 kHz, and stays open. The settled state
    /// of that transport, as `A2dp` is of a Classic deck.
    Ble,
    /// Our own PTT capture over the BLE voice link: the deck's speaker is
    /// closed for the press (half-duplex: no echo cancellation, V5) and
    /// reopens on the release. The BLE transport's `ScoOurs`.
    BleOurs,
}

/// Which way the deck's audio travels (`[voice] transport`): the deck's own
/// GATT audio service on its BLE link (`deckd::ble`), or the pre-Rev-A
/// Classic HFP/A2DP device pair addressed through CoreAudio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VoiceTransport {
    /// The Rev A firmware is no longer a Bluetooth Classic audio device, so
    /// this is the default.
    #[default]
    Ble,
    Coreaudio,
}

impl VoiceTransport {
    /// The config spelling, for logs and `voice.test`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VoiceTransport::Ble => "ble",
            VoiceTransport::Coreaudio => "coreaudio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PttInfo {
    /// Device-local press identity, echoed only on the device wire.
    #[serde(skip)]
    #[schemars(skip)]
    pub device_press: Option<u16>,
    pub agent: Option<AgentId>,
    pub ctx: crate::PttCtx,
    pub card: Option<crate::CardId>,
    /// Who started this session: `device` for a deck `ptt` edge, else the
    /// caller of `voice.listen` (§7.4).
    pub principal: crate::Principal,
    /// The **session** that caller was on, which `principal` alone does not
    /// give (B2). §4.9 requires both on every `op.answer`/`op.order` the
    /// dispatch derives, and ownership in this daemon is per session and
    /// never per principal (§4.2) — so without it a dictated answer could
    /// not be attributed to the connection that asked for it, and an
    /// `agent.wait` on the owning session could not be handed the result.
    /// `None` for a deck edge, which has no session at all.
    #[serde(default)]
    pub session: Option<crate::SessionId>,
    /// The id every transcript of this session carries (`ut_<n>`, §4.12's
    /// `transcript.partial|final{utterance}` and B12's `order.dispatched`).
    /// Allocated when the capture opens, because the partials start
    /// arriving before anything else has a chance to name them.
    #[serde(default)]
    pub utterance: String,
    /// Whether the capture has produced a **non-zero sample** yet — §7.4's
    /// `Arming` (false) vs `Listening` (true).
    ///
    /// The distinction is a second of the operator's life, not a detail:
    /// SCO bring-up on this hardware is 1031 ms median (V1, measured
    /// 2026-09-21), during which callbacks arrive, `AudioUnitRender`
    /// returns `noErr` and every sample is an exact digital zero. A GUI
    /// that shows "listening" for that second is lying to the operator,
    /// and a liveness check that believes the callbacks never fires on a
    /// dead mic at all (V2).
    #[serde(default)]
    pub live: bool,
    pub since: Ts,
    /// The agent's owner transcribes this press (`caps.transcribe`); the
    /// local recogniser is only the fallback, so its partials are not
    /// shown on the deck.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub companion: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Speaking {
    pub utterance: String,
    pub lane: String,
    pub agent: Option<AgentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SttInfo {
    pub engine: String,
    pub locale: String,
    pub on_device: bool,
}

impl Default for SttInfo {
    fn default() -> Self {
        SttInfo {
            engine: "apple".into(),
            locale: "en-US".into(),
            on_device: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TtsInfo {
    pub engine: String,
}

impl Default for TtsInfo {
    fn default() -> Self {
        TtsInfo {
            engine: "apple".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    #[default]
    Unknown,
    NotDetermined,
    Granted,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Permissions {
    pub microphone: Permission,
    pub speech: Permission,
    /// CoreBluetooth's authorization for this process, as the BLE voice
    /// link last read it (`[voice] transport = "ble"`): what the deck's
    /// microphone and speaker need there, instead of Microphone. `unknown`
    /// on the CoreAudio transport and until the link has started.
    #[serde(default)]
    pub bluetooth: Permission,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VoiceState {
    /// The transport the audio engine was built for (`[voice] transport`);
    /// null until the engine reports it, and in a build with no engine.
    #[serde(default)]
    pub transport: Option<VoiceTransport>,
    pub route: Route,
    pub output_uid: Option<String>,
    pub input_uid: Option<String>,
    pub ptt: Option<PttInfo>,
    pub speaking: Option<Speaking>,
    pub stt: SttInfo,
    pub tts: TtsInfo,
    pub permissions: Permissions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub period: String,
    pub items: Vec<UsageItem>,
    pub at: Option<Ts>,
}

/// Who holds the TCP port when the daemon could not take it (§4.1's `lsof
/// -nP -iTCP:<port> -sTCP:LISTEN` lookup, best effort).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Listener {
    pub pid: u64,
    pub command: Option<String>,
}

/// The service's dormancy (`service.dormancy`): with no deck linked for
/// `window_s` seconds it stops collecting from agent sessions (no session
/// polling, no transcript watchers, no usage reads) and starts again the
/// moment a deck links.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DormancyState {
    /// How long the collectors keep running after the deck was last linked,
    /// in seconds.
    pub window_s: u32,
    /// Whether they run now.
    pub collecting: bool,
    /// When they stopped: the deck's last link plus the window. `null` while
    /// they run, and when no deck was ever linked to this service.
    pub dormant_since: Option<Ts>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceState {
    pub version: String,
    pub degraded: Option<String>,
    pub port: u16,
    /// Set while `degraded` is `port_in_use`/`hooks_hijack_risk`: the
    /// process the port belongs to, for `service.degraded` and
    /// `service.info` (§4.1, §4.12).
    #[serde(default)]
    pub listener: Option<Listener>,
    /// The collectors' dormancy; `null` in a service that has none (one
    /// started with `--no-device`, or one that predates it).
    #[serde(default)]
    pub dormancy: Option<DormancyState>,
}

/// A Macropad face's keys as the host holds them, set by `device.macro.set`
/// or by the deck's own editor, and kept in `state.json` so a restart brings
/// them back. The deck reports single edits and never a whole set, so this
/// is the only full copy there is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MacroLayout {
    /// The tag the deck shows in the Macropad's header, as last given;
    /// `None` when none was.
    pub src: Option<String>,
    /// Slot 0 first, at most nine. An empty label is an empty slot, which
    /// keeps the keys after it in their places.
    pub keys: Vec<MacroKey>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    /// Increases only when something observable changed.
    pub rev: u64,
    pub agents: BTreeMap<AgentId, Agent>,
    pub focus: Focus,
    pub device: DeviceState,
    pub voice: VoiceState,
    pub usage: Option<Usage>,
    /// Host-persisted settings (restored on `boot:1`).
    pub settings: Settings,
    /// The last adopted strip (restored on `boot:1`).
    pub faces: Vec<FaceEntry>,
    /// Each Macropad face's keys, by face id; `device.macros` says every
    /// change. Absent while no face has any, so a reader that predates the
    /// field meets nothing new.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub macros: BTreeMap<u8, MacroLayout>,
    pub service: ServiceState,
}

impl Snapshot {
    /// Slotted agents in ascending slot order.
    pub fn by_slot(&self) -> Vec<(crate::Slot, &Agent)> {
        let mut out: Vec<_> = self
            .agents
            .values()
            .filter_map(|a| a.slot.map(|s| (s, a)))
            .collect();
        out.sort_by_key(|(s, _)| *s);
        out
    }

    pub fn agent_at(&self, slot: crate::Slot) -> Option<&Agent> {
        self.agents.values().find(|a| a.slot == Some(slot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings a deck has at power-on, as the firmware sets them
    /// (`src/deck/model.rs` `Settings::new`): what a new deck starts from and
    /// what a barrier sends for a key the host never set. Demo data is off
    /// there ("without a host the deck shows itself offline").
    #[test]
    fn the_default_settings_are_the_firmwares_power_on_settings() {
        let s = Settings::default();
        assert_eq!(
            (
                s.speaker,
                s.mic,
                s.announce,
                s.chime,
                s.autojump,
                s.brightness,
                s.idlehome,
                s.demo,
                s.textsize,
                s.arcadesound
            ),
            (6, 6, 1, 1, 0, 5, 120, 0, 1, 0)
        );
    }

    #[test]
    fn old_settings_default_arcade_sound_off_and_new_choice_round_trips() {
        let mut json = serde_json::to_value(Settings::default()).unwrap();
        json.as_object_mut().unwrap().remove("arcadesound");
        let mut settings: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(settings.arcadesound, 0);
        assert!(settings.set(SetKey::Arcadesound, 2));
        assert!(!settings.set(SetKey::Arcadesound, 3));
        let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.arcadesound, 2);
    }

    #[test]
    fn settings_default_to_the_firmware_and_validate_ranges() {
        let mut s = Settings::default();
        assert_eq!(s.get(SetKey::Idlehome), 120);
        assert!(s.set(SetKey::Brightness, 3));
        assert!(!s.set(SetKey::Brightness, 3));
        assert!(!s.set(SetKey::Brightness, 9));
        assert_eq!(s.get(SetKey::Brightness), 3);
        assert_eq!(s.iter().count(), 10);
        assert_eq!(s.iter().next(), Some((SetKey::Speaker, 6)));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"idlehome\":120"));
        let partial: Settings = serde_json::from_str(r#"{"speaker":2}"#).unwrap();
        assert_eq!(partial.speaker, 2);
        assert_eq!(partial.mic, 6);
    }

    #[test]
    fn an_empty_snapshot_serializes_with_the_design_keys() {
        let json = serde_json::to_value(Snapshot::default()).unwrap();
        for key in [
            "rev", "agents", "focus", "device", "voice", "usage", "settings", "faces", "service",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["device"]["link"], "offline");
        assert_eq!(json["device"]["session"], "absent");
        assert_eq!(json["voice"]["route"], "none");
    }

    /// An app built before the deck's status fields reads a new service's
    /// snapshot, and a new app reads an old service's: the fields are absent
    /// until there is something to say, and absent reads as unknown.
    #[test]
    fn a_device_state_without_the_status_fields_reads_and_an_unknown_one_writes_none_of_them() {
        let unknown = serde_json::to_value(DeviceState::default()).unwrap();
        for key in ["transport", "battery", "charging", "board", "last", "drops"] {
            assert!(unknown.get(key).is_none(), "{key} written while unknown");
        }
        let old = r#"{"language_supported":true,"link":"connected","session":"live","sid":7,
            "fw":"0.13.0","boot":false,"face":1,"faces":[],"host_online":true,
            "settings":{"speaker":6,"mic":6,"announce":1,"chime":1,"autojump":0,
                        "brightness":5,"idlehome":120,"demo":1},
            "pace_ms":8,"outbox_depth":0,"err_counts":{"range":0,"parse":0,"size":0,"unknown":0}}"#;
        let device: DeviceState = serde_json::from_str(old).unwrap();
        assert_eq!(device.link, Link::Connected);
        assert_eq!(
            (device.transport, device.battery, device.charging, device.board.as_deref()),
            (None, None, None, None)
        );
        assert_eq!((device.last.as_ref(), device.drops.len()), (None, 0));
        let known = DeviceState {
            transport: Some(Transport::Usb),
            battery: Some(85),
            charging: Some(false),
            board: Some("Rev B".into()),
            last: Some(LastLinked {
                at: Ts(1_789_760_523_000),
                fw: Some("0.13.0".into()),
                board: Some("Rev B".into()),
                transport: Some(Transport::Bluetooth),
            }),
            drops: vec![LinkDrop { at: Ts(1_789_760_523_000), why: "read".into() }],
            ..DeviceState::default()
        };
        let json = serde_json::to_value(&known).unwrap();
        assert_eq!(json["transport"], "usb");
        assert_eq!(json["last"]["transport"], "bluetooth");
        assert_eq!(json["last"]["at"], "2026-09-18T19:42:03.000Z");
        assert_eq!(json["drops"][0]["why"], "read");
        assert_eq!(serde_json::from_value::<DeviceState>(json).unwrap(), known);
    }

    /// The keys travel by face id, and only once there are some: a service
    /// that predates them and one that holds none send the same snapshot,
    /// and both read as no keys at all.
    #[test]
    fn a_snapshot_carries_the_macro_layouts_by_face_id_and_nothing_while_there_are_none() {
        let none = serde_json::to_value(Snapshot::default()).unwrap();
        assert!(
            none.get("macros").is_none(),
            "nothing to say, nothing written"
        );
        let old: Snapshot = serde_json::from_value(none).unwrap();
        assert!(old.macros.is_empty());
        let mut snapshot = Snapshot::default();
        snapshot.macros.insert(
            3,
            MacroLayout {
                src: Some("deck.toml".into()),
                keys: vec![
                    MacroKey {
                        label: String::new(),
                        binding: None,
                        keys: None,
                    },
                    MacroKey {
                        label: "build".into(),
                        binding: Some(crate::Binding::Left),
                        keys: Some("cmd+b".into()),
                    },
                ],
            },
        );
        let json = serde_json::to_value(&snapshot).unwrap();
        // The empty first slot stays, so `build` keeps its place.
        assert_eq!(
            json["macros"],
            serde_json::json!({"3": {"src": "deck.toml", "keys": [
                {"label": "", "binding": null},
                {"label": "build", "binding": "left", "keys": "cmd+b"}
            ]}})
        );
        assert_eq!(serde_json::from_value::<Snapshot>(json).unwrap(), snapshot);
    }

    /// A service that predates dormancy sends no `dormancy` and is read as
    /// one with none; a new one says whether its collectors run and since
    /// when they stopped.
    #[test]
    fn the_services_dormancy_is_optional_and_round_trips() {
        let old: ServiceState =
            serde_json::from_str(r#"{"version":"0.13.0","degraded":null,"port":47201}"#).unwrap();
        assert_eq!(old.dormancy, None);
        let quiet = ServiceState {
            dormancy: Some(DormancyState {
                window_s: 600,
                collecting: false,
                dormant_since: Some(Ts(1_789_760_523_000)),
            }),
            ..ServiceState::default()
        };
        let json = serde_json::to_value(&quiet).unwrap();
        assert_eq!(
            json["dormancy"],
            serde_json::json!({"window_s": 600, "collecting": false,
                "dormant_since": "2026-09-18T19:42:03.000Z"})
        );
        assert_eq!(serde_json::from_value::<ServiceState>(json).unwrap(), quiet);
    }
}
