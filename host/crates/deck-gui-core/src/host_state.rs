//! The mirror and the reducer (§8.1): `handle_event(AppEvent) -> Vec<HostEffect>`.
//!
//! The GUI holds no state of its own about the deck. It holds a `Snapshot`
//! the daemon owns, folded forward from the daemon's events and **rev-checked**
//! — `rev` increases by exactly one per event (`Store::emit`), so a jump is a
//! hole, and a hole is answered with `state.get` rather than with a mirror
//! that is quietly wrong. UI actions never write here; they call the daemon
//! (`HostState::send`, which takes `&self`) and the events that come back do
//! the writing.
//!
//! [`HostState::detached`] builds one with no connection behind it, which is
//! what makes the reducer suite in `tests/reducer.rs` hermetic: scripted
//! `ApiEvent`s in, effects and a mirror out, no socket and no daemon.
//!
//! The window is the deck's companion and never a session's. The mirror
//! holds no agent: every snapshot is installed with its agents and the focus
//! on one of them dropped (`without_sessions`), and no agent, card, reply,
//! order, focus, confirmation or transcript event is folded. Each of those
//! still advances `rev`, because the rev check runs before the fold: the
//! subscription stays whole, so the rule that a jump is a hole keeps holding.
use crate::events::{ApiEvent, AppEvent};
use crate::ApiClient;
use deck_model::{
    EventKind, FaceEntry, Focus, LastLinked, Link, LinkDrop, MacroKey, MacroLayout, Permission,
    Principal, Route, SessionState, SetKey, Settings, Snapshot, Speaking,
    Transport, Ts, Usage, VoiceTransport,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// §8.1's reconnect schedule, and `deck-client`'s: one definition, so what
/// the yellow bar counts down and what the socket actually does cannot drift.
pub use deck_client::{BACKOFF_MAX, BACKOFF_MIN};

/// The reason `service.shutdown` folds into `service.degraded`. The service
/// never sends it as a degradation of its own: it marks a service that is
/// going away, which the next `Disconnected` says in full.
pub const SHUTTING_DOWN: &str = "shutdown";

/// `x-apple.systempreferences:` URLs for the panes a *denied* permission can
/// only be fixed in: macOS will not prompt twice, so System Settings is the
/// only recourse (§7.8). The Voice page's **Open Settings** goes there when
/// the owner presses it; nothing opens them unasked.
pub const SPEECH_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition";
/// Where a Bluetooth denial is lifted (the BLE voice transport needs it).
pub const BLUETOOTH_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth";

/// Which sheet [`HostEffect::RaisePermissionSheet`] asks for.
///
/// Speech recognition alone. The Mac's microphone is no longer a voice
/// path of this app (owner decision 11), so a microphone that has not been
/// asked for is nothing to raise a sheet about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetKind {
    /// macOS has not been asked for speech recognition yet (§7.8).
    Speech,
}

/// What the reducer asks the app to do. Everything here is an *action on the
/// app*, never a write to the mirror.
///
/// `StateGet` is the one the design's §8.1 list does not name but its text
/// requires ("a gap triggers `state.get`"): the reducer is pure, so the only
/// way it can ask for a snapshot is to say so and let the caller make the
/// call. The gpui crate must handle it (`HostState::send(&UiAction::StateGet)`
/// is exactly what it should run).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEffect {
    RaisePermissionSheet(SheetKind),
    Toast(String),
    /// Connect again after this long (250 ms doubling to 5 s).
    Reconnect(Duration),
    /// The mirror is stale: fetch the whole snapshot again.
    StateGet,
}

/// The last capture RMS the daemon reported (`voice.level`, §7.1), for a
/// microphone meter. Not part of the `Snapshot`: it is a per-buffer stream,
/// not state.
///
/// **An `rms` of 0.0 is not "no microphone".** V2 measured that a dead mic
/// and a warming SCO link report exactly the same digital zero for about a
/// second, so the meter draws what it was told and the screen never turns
/// a quiet buffer into a verdict about the hardware.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub rms: f32,
    /// The event's own stamp. The reducer reads no clock, so how old this
    /// is, is the view's question to ask.
    pub at: Ts,
}

/// §8.1's reconnect schedule: 250 ms, doubling, ceiling 5 s, reset by a
/// connection that came up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff(Duration);

impl Backoff {
    pub const fn new() -> Backoff {
        Backoff(BACKOFF_MIN)
    }

    /// How long to wait before the next attempt, stepping the schedule on.
    pub fn wait(&mut self) -> Duration {
        let wait = self.0;
        self.0 = (self.0 * 2).min(BACKOFF_MAX);
        wait
    }

    /// The next wait without stepping.
    pub const fn peek(&self) -> Duration {
        self.0
    }

    pub fn reset(&mut self) {
        self.0 = BACKOFF_MIN;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new()
    }
}

/// One value `config.set` can be asked to write.
///
/// It is a closed enum and not a `serde_json::Value` for two reasons. The
/// first is that [`UiAction`] is `Eq`, which a `Value` is not. The second is
/// the one that matters: the config is validated whole-file against a schema
/// (§4.11), and a GUI that could hand it an arbitrary JSON tree would be
/// able to compose a patch no screen has a control for. The three shapes
/// week 6's screens actually set — a port, a flag, a name — are these, and
/// the companion pages add the fourth: a list of names
/// (`usage.providers`, `sources.apps_paused`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Text(String),
    /// A list of names, written whole: the service keeps no list it could
    /// add one name to, so a control that switches one name sends them all.
    List(Vec<String>),
    /// Remove the key, so the documented default applies again (§4.11):
    /// what "Remove key" on the Voice screen says.
    Null,
}

impl ConfigValue {
    pub fn json(&self) -> Value {
        match self {
            ConfigValue::Bool(b) => Value::Bool(*b),
            ConfigValue::Int(n) => Value::from(*n),
            ConfigValue::Text(s) => Value::String(s.clone()),
            ConfigValue::List(names) => {
                Value::Array(names.iter().cloned().map(Value::String).collect())
            }
            ConfigValue::Null => Value::Null,
        }
    }
}

/// What a click asks the daemon to do (§8.2's Actions column). Turning one
/// into a `(method, params)` pair is a pure function and sending one takes
/// `&self`, so there is no path from a UI action to a write on the mirror —
/// the events the call produces are the only writer.
///
/// None of them is a session's: the window answers no card, sends no order
/// and opens no image (never a session). Most produce no event at all, so
/// nothing folds their answers into the mirror and the pages keep what they
/// asked for in their own view state ([`crate::admin`], [`crate::care`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    /// The whole snapshot again (§4.5). What a [`HostEffect::StateGet`] runs.
    StateGet,

    // ---- the Integrations screen (§8.2) --------------------------------
    /// `integrations.status{detect}`. `detect:false` makes it pure file I/O;
    /// with it on, the daemon runs `--version` and Codex's `hooks/list`,
    /// which is the only thing that can tell a stale trust hash from a
    /// current one (§5.7) — and the only thing that can be slow.
    IntegrationsStatus {
        detect: bool,
    },
    /// `integrations.{install,uninstall,repair}{target, trust, dry_run}`.
    /// With `dry_run` it is "What this writes" and changes nothing.
    Integrations {
        verb: crate::admin::Verb,
        target: String,
        trust: bool,
        dry_run: bool,
    },
    TokenList,
    /// `token.create{name, scopes, expires?}` — the one call whose reply
    /// carries a plaintext, once ([`crate::admin::Secret`]).
    TokenCreate {
        name: String,
        scopes: Vec<String>,
        expires: Option<String>,
    },
    TokenRevoke {
        name: String,
    },
    TokenRotate {
        name: String,
    },
    /// `perm.status`: the grants the service holds (microphone, speech,
    /// Bluetooth, Accessibility). It is not an `integrations.*` target:
    /// nothing is written for it. A 0.13 service also reported Ghostty's
    /// Automation grant, which went with its tab path.
    PermStatus,

    // ---- the Service screen (§8.2) -------------------------------------
    ServiceInfo,
    ConfigGet,
    /// One key of `config.set`'s `{patch}` (§4.11). The action stays a pair
    /// because every control this GUI has moves a single key; `request()`
    /// wraps it into the one-entry patch the daemon answers. A screen that
    /// needed two keys to move together would build the map itself — a patch
    /// is atomic and two calls are not.
    ConfigSet {
        key: String,
        value: ConfigValue,
    },
    /// `service.stop` / `service.restart` (§4.11), both scope `admin`.
    ServiceStop,
    ServiceRestart,
    ServiceRestartWhenIdle,
    LogTail {
        lines: u32,
        level: Option<String>,
    },

    // ---- the Voice screen (§8.2) ---------------------------------------
    /// `voice.engines` (§4.9) — the STT/TTS badges. Answered from the one
    /// report the audio side pushed when its engines came up, so a daemon
    /// with no engine answers `-32013` and the screen says *that* rather
    /// than drawing an empty badge.
    VoiceEngines,
    /// `voice.test{what}` — [`crate::voice::TESTS`]. Only `speaker` makes a
    /// sound; the rest report what the daemon can observe without opening
    /// anything.
    VoiceTest {
        what: String,
    },
    /// `stt.status` — the local recogniser's model: selected, on disk,
    /// loaded, and the download in flight (§7.6).
    SttStatus,
    FirmwareStatus,
    FirmwareCheck { force: bool },
    FirmwareInstall {
        board: String,
        serial: String,
        version: String,
        allow_bootloader_update: bool,
    },
    /// `stt.install{model?}` — the opt-in: fetch the model's weights. The
    /// daemon downloads in the background; the screen polls `stt.status`.
    SttInstall {
        model: Option<String>,
    },
    /// `stt.remove` — delete the weights (and stop a download).
    SttRemove,
    /// `tts.status` / `tts.install{model?}` / `tts.remove` — the local
    /// voice's package, the same way.
    TtsStatus,
    TtsInstall {
        model: Option<String>,
    },
    TtsRemove,
    /// `perm.request{which}` (§7.8). Stubbed at the prompt until the signed
    /// bundle of §10.1 (§10.4), which is why the reply's `limits` are shown
    /// verbatim wherever this is offered.
    PermRequest {
        which: String,
    },
    /// `device.set{k, v}` — one deck setting: the Deck page's controls and
    /// the menu's **Auto-Announce**.
    DeviceSet {
        key: deck_model::SetKey,
        value: u16,
    },

    // ---- the companion's pages ------------------------------------------
    /// `device.faces.set{list}` — the deck's whole strip, in order. The
    /// service adds no check of its own (an existing method may not start
    /// refusing), so the Faces page sends only a strip it built valid and
    /// confirms it by the `device.faces` that comes back.
    DeviceFacesSet {
        list: Vec<FaceEntry>,
    },
    /// `device.macro.set{f, layout, src, keys}` — one Macropad face's keys,
    /// whole: the deck takes no single key. `src` is the tag the deck shows
    /// in the face's header, sent back as it was read.
    DeviceMacroSet {
        f: u8,
        layout: String,
        src: Option<String>,
        keys: Vec<MacroKey>,
    },
    /// `config.set{patch}` with several keys that must land together: the
    /// patch is atomic, two `ConfigSet` calls are not (a language and its
    /// locale; resuming the sources and the hooks at once). A key named
    /// twice is sent once, with the later value.
    ConfigPatch {
        entries: Vec<(String, ConfigValue)>,
    },
    /// `sources.list` — what feeds the deck, as counts and times per source,
    /// never a session.
    SourcesList,
    /// `care.test.question` — a test question on the deck. Answers at once
    /// with `sent` or `no_deck`; the outcome is read later with
    /// [`UiAction::TestStatus`].
    TestQuestion,
    /// `care.test.voice` — a card asking the owner to hold PTT and speak;
    /// what was heard is read later with [`UiAction::TestStatus`].
    TestVoice,
    /// `care.test.say` — a sentence spoken on the deck.
    TestSay,
    /// `care.test.status` — the last test question and voice test.
    TestStatus,
    /// `care.checks` — the service's own setup checks.
    CareChecks,
    /// `care.footprint` — what the app has put on this Mac, item by item.
    CareFootprint,
    /// `care.support_file{to}` — `"desktop"`, or an absolute directory.
    CareSupportFile {
        to: String,
    },
    /// `care.cache.clear{what}` — `"firmware"` is the one cache there is.
    CareCacheClear {
        what: String,
    },
}

impl UiAction {
    /// The JSON-RPC call this action is.
    pub fn request(&self) -> (&'static str, Value) {
        match self {
            UiAction::StateGet => ("state.get", serde_json::json!({})),
            UiAction::IntegrationsStatus { detect } => {
                ("integrations.status", serde_json::json!({"detect": detect}))
            }
            UiAction::Integrations {
                verb,
                target,
                trust,
                dry_run,
            } => (
                verb.method(),
                // `trust` is Codex's opt-in and `deny_unknown_fields` is on,
                // so both keys are always sent and both are always booleans.
                serde_json::json!({"target": target, "trust": trust, "dry_run": dry_run}),
            ),
            UiAction::TokenList => ("token.list", serde_json::json!({})),
            UiAction::TokenCreate {
                name,
                scopes,
                expires,
            } => (
                "token.create",
                serde_json::json!({"name": name, "scopes": scopes, "expires": expires}),
            ),
            UiAction::TokenRevoke { name } => ("token.revoke", serde_json::json!({"name": name})),
            UiAction::TokenRotate { name } => ("token.rotate", serde_json::json!({"name": name})),
            UiAction::PermStatus => ("perm.status", serde_json::json!({})),
            UiAction::ServiceInfo => ("service.info", serde_json::json!({})),
            UiAction::ConfigGet => ("config.get", serde_json::json!({})),
            // The wire shape is §4.11's `{patch}`, a map of the dotted names
            // `config.get` publishes. The GUI keeps a one-key action because
            // every control it has moves one key, and a screen that wanted two
            // to move together would build the map itself rather than send two
            // calls — a patch is atomic, two calls are not.
            UiAction::ConfigSet { key, value } => (
                "config.set",
                serde_json::json!({"patch": {key: value.json()}}),
            ),
            UiAction::ServiceStop => ("service.stop", serde_json::json!({})),
            UiAction::ServiceRestartWhenIdle => ("service.restart", serde_json::json!({"when_idle":true})),
            UiAction::ServiceRestart => ("service.restart", serde_json::json!({})),
            UiAction::LogTail { lines, level } => (
                "log.tail",
                serde_json::json!({"lines": lines, "level": level}),
            ),
            UiAction::VoiceEngines => ("voice.engines", serde_json::json!({})),
            UiAction::VoiceTest { what } => ("voice.test", serde_json::json!({"what": what})),
            UiAction::FirmwareStatus => ("firmware.status", serde_json::json!({})),
            UiAction::FirmwareCheck { force } => ("firmware.check", serde_json::json!({"force": force})),
            UiAction::FirmwareInstall {
                board,
                serial,
                version,
                allow_bootloader_update,
            } => (
                "firmware.install",
                serde_json::json!({"board":board,"serial":serial,"version":version,"allow_bootloader_update":allow_bootloader_update}),
            ),
            UiAction::SttStatus => ("stt.status", serde_json::json!({})),
            UiAction::SttInstall { model } => match model {
                Some(model) => ("stt.install", serde_json::json!({"model": model})),
                None => ("stt.install", serde_json::json!({})),
            },
            UiAction::SttRemove => ("stt.remove", serde_json::json!({})),
            UiAction::TtsStatus => ("tts.status", serde_json::json!({})),
            UiAction::TtsInstall { model } => match model {
                Some(model) => ("tts.install", serde_json::json!({"model": model})),
                None => ("tts.install", serde_json::json!({})),
            },
            UiAction::TtsRemove => ("tts.remove", serde_json::json!({})),
            UiAction::PermRequest { which } => {
                ("perm.request", serde_json::json!({"which": which}))
            }
            // §4.10's `{k, v}`, and the key goes out as the wire word
            // `SetKey` serialises to rather than as a name this crate spells
            // a second time.
            UiAction::DeviceSet { key, value } => {
                ("device.set", serde_json::json!({"k": key, "v": value}))
            }
            // The strip and the keys go out as the store's own types
            // serialise, so the params are the `Request` the service parses
            // and not a second spelling of it.
            UiAction::DeviceFacesSet { list } => {
                ("device.faces.set", serde_json::json!({"list": list}))
            }
            UiAction::DeviceMacroSet {
                f,
                layout,
                src,
                keys,
            } => (
                "device.macro.set",
                serde_json::json!({"f": f, "layout": layout, "src": src, "keys": keys}),
            ),
            UiAction::ConfigPatch { entries } => {
                let patch: serde_json::Map<String, Value> = entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.json()))
                    .collect();
                ("config.set", serde_json::json!({"patch": patch}))
            }
            UiAction::SourcesList => ("sources.list", serde_json::json!({})),
            UiAction::TestQuestion => ("care.test.question", serde_json::json!({})),
            UiAction::TestVoice => ("care.test.voice", serde_json::json!({})),
            UiAction::TestSay => ("care.test.say", serde_json::json!({})),
            UiAction::TestStatus => ("care.test.status", serde_json::json!({})),
            UiAction::CareChecks => ("care.checks", serde_json::json!({})),
            UiAction::CareFootprint => ("care.footprint", serde_json::json!({})),
            UiAction::CareSupportFile { to } => {
                ("care.support_file", serde_json::json!({"to": to}))
            }
            UiAction::CareCacheClear { what } => {
                ("care.cache.clear", serde_json::json!({"what": what}))
            }
        }
    }
}

/// The mirror of the daemon's `Snapshot`, without its sessions, plus the one
/// stream the snapshot does not carry (the microphone level).
pub struct HostState {
    mirror: Snapshot,
    /// `None` on a [`HostState::detached`]: the reducer runs, nothing calls.
    client: Option<ApiClient>,
    connected: bool,
    /// False until the first `Connected`, so the first one does not toast
    /// "reconnected" at a GUI that has only just started.
    seen: bool,
    backoff: Backoff,
    /// True between a detected hole and the snapshot that fills it. While it
    /// is set no further `StateGet` is asked for, and nothing is folded onto
    /// the stale base: a fold across a hole is the silently-wrong mirror this
    /// whole mechanism exists to refuse. (A *contiguous* event is still
    /// folded — after a lag resync the stream is unbroken and the mirror is
    /// not stale; see `on_event`.) Only a snapshot clears it, so the screen
    /// that asked has to ask again if the reply was not one.
    resyncing: bool,
    /// Holes seen, for the Service screen and the tests.
    gaps: u64,
    /// Events folded (a duplicate or a hole is not one).
    folded: u64,
    level: Option<Level>,
}

impl std::fmt::Debug for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostState")
            .field("rev", &self.mirror.rev)
            .field("connected", &self.connected)
            .field("resyncing", &self.resyncing)
            .finish()
    }
}

impl HostState {
    fn ui_language(&self) -> crate::i18n::Language {
        crate::i18n::Language::from_id(self.mirror.settings.language.min(255) as u8)
    }
    /// With a client behind it: the app's one `HostState`.
    pub fn new(client: ApiClient) -> HostState {
        HostState {
            client: Some(client),
            ..HostState::detached()
        }
    }

    /// No client, no socket, no daemon: `handle_event` is the only way in.
    /// The reducer suite is built on this.
    pub fn detached() -> HostState {
        HostState {
            mirror: Snapshot::default(),
            client: None,
            connected: false,
            seen: false,
            backoff: Backoff::new(),
            resyncing: false,
            gaps: 0,
            folded: 0,
            level: None,
        }
    }

    /// The daemon's state as the GUI last saw it. `&` only: see `send`.
    pub fn mirror(&self) -> &Snapshot {
        &self.mirror
    }

    /// The socket is up. False → the yellow "Service not running" bar and
    /// the last mirror greyed (§8.2).
    pub fn connected(&self) -> bool {
        self.connected
    }

    /// The service has answered at least once since this app started:
    /// false until the first `Connected`, then true for good, through any
    /// number of drops. It is what tells a service that was never there (a
    /// first run, nothing installed yet) from one that went away, whose
    /// last mirror is still worth showing.
    pub fn seen(&self) -> bool {
        self.seen
    }

    /// Whether the last hello was answered as §4.2's trusted principal.
    /// False on an ad-hoc dev build without `[trust] dev_requirement`, and on
    /// a detached state — never an error: this window allows nothing and
    /// injects nothing, so it only ever reports it.
    pub fn trusted(&self) -> bool {
        self.client.as_ref().is_some_and(ApiClient::trusted)
    }

    /// A `state.get` is outstanding: the mirror is stale and says so.
    pub fn resyncing(&self) -> bool {
        self.resyncing
    }

    /// How long the next reconnect attempt waits.
    pub fn backoff(&self) -> Duration {
        self.backoff.peek()
    }

    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    pub fn folded(&self) -> u64 {
        self.folded
    }

    /// The last capture level the daemon reported, with the stamp it
    /// reported it at. `None` until the first `voice.level` of this session
    /// — which is not the same as silence, and the meter says so.
    pub fn level(&self) -> Option<Level> {
        self.level
    }

    pub fn client(&self) -> Option<&ApiClient> {
        self.client.as_ref()
    }

    /// Send a UI action to the daemon and hand back where its reply will
    /// land (a view awaits it on gpui's own executor). `None` when detached.
    ///
    /// `&self`, deliberately: this is the whole GUI → daemon path, and it
    /// cannot touch the mirror.
    pub fn send(&self, action: &UiAction) -> Option<crate::api_client::Reply> {
        let client = self.client.as_ref()?;
        let (method, params) = action.request();
        Some(client.call(method, params))
    }

    /// Fold one event from the bus and say what the app should do about it.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<HostEffect> {
        match event {
            AppEvent::Api(ApiEvent::Connected { snapshot }) => self.on_connected(snapshot),
            AppEvent::Api(ApiEvent::Snapshot { snapshot }) => self.on_snapshot(snapshot),
            AppEvent::Api(ApiEvent::Event(event)) => self.on_event(&event),
            AppEvent::Api(ApiEvent::Disconnected) => self.on_disconnected(),
            AppEvent::Api(ApiEvent::Resync) => self.on_resync(),
            // A menu click is an *action*, and the reducer issues none: it
            // folds what the daemon reported and says what the app should
            // do about it. The window turns the click into the same
            // `UiAction` a control on a page would send (the one
            // `MenuModel` names), so the menu and the pages take one path to
            // the daemon and the mirror has one writer — the events that
            // come back.
            AppEvent::Menu(_) => Vec::new(),
        }
    }

    /// A whole snapshot: the mirror is replaced, the hole (if there was one)
    /// is closed and the reconnect schedule starts over. It arrives from the
    /// hello and from a `state.get` reply alike
    /// ([`ApiEvent::snapshot_reply`]) — the mirror is only ever replaced
    /// wholesale, so there is one way in for both.
    fn on_connected(&mut self, snapshot: Snapshot) -> Vec<HostEffect> {
        // A snapshot that closes a resync while the socket was up is not a
        // reconnection and must not claim to be one.
        let reconnected = self.seen && !self.connected;
        self.mirror = without_sessions(snapshot);
        self.connected = true;
        self.resyncing = false;
        self.backoff.reset();
        self.seen = true;
        if reconnected {
            vec![HostEffect::Toast(self.ui_language().text("Service reconnected").into())]
        } else {
            Vec::new()
        }
    }

    /// A `state.get` reply (§4.5): the mirror is replaced and the hole it
    /// was asked for is closed. Nothing else is touched — the socket's state
    /// is the `ApiClient`'s to report through `Connected`/`Disconnected`, and
    /// a reply that raced a drop must not un-grey a window whose service has
    /// gone away.
    fn on_snapshot(&mut self, snapshot: Snapshot) -> Vec<HostEffect> {
        // A state.get reply can cross a newer live event on the UI bus.
        // Preserve that event's language (and the rest of the newer mirror).
        if snapshot.rev < self.mirror.rev {
            return if self.resyncing { vec![HostEffect::StateGet] } else { Vec::new() };
        }
        self.mirror = without_sessions(snapshot);
        self.resyncing = false;
        Vec::new()
    }

    /// The connection ended or an attempt failed. The last mirror is kept —
    /// greyed, not thrown away: the deck as it was two seconds ago is worth
    /// more to the owner than an empty window.
    fn on_disconnected(&mut self) -> Vec<HostEffect> {
        self.connected = false;
        vec![HostEffect::Reconnect(self.backoff.wait())]
    }

    fn on_resync(&mut self) -> Vec<HostEffect> {
        if self.resyncing {
            return Vec::new();
        }
        self.resyncing = true;
        vec![HostEffect::StateGet]
    }

    /// The rev check, then the fold. `Store::emit` increments `rev` by
    /// exactly one per event, so `rev == mirror.rev + 1` is the only
    /// contiguous case; below it is a replay (a `since` reconnect resends
    /// what we already folded) and above it is a hole.
    ///
    /// A hole does not advance `rev`, so every event behind it is a hole too
    /// and the mirror stays frozen at the last rev it was actually true at,
    /// until the snapshot lands. That is the point: a mirror that has stopped
    /// is something the window can say out loud ([`HostState::resyncing`]);
    /// a mirror folded across a hole is one that cannot.
    fn on_event(&mut self, event: &deck_model::Event) -> Vec<HostEffect> {
        if event.rev <= self.mirror.rev {
            return Vec::new();
        }
        if event.rev > self.mirror.rev + 1 {
            self.gaps += 1;
            return self.on_resync();
        }
        // Contiguous, so it is folded even with a `state.get` outstanding:
        // this is exactly the next event and cannot make the mirror wrong,
        // and the snapshot that answers will replace it anyway. (It happens
        // after a `resync` notification, where the client lagged its own
        // ring and the daemon is still sending.)
        self.mirror.rev = event.rev;
        self.folded += 1;
        self.fold(event)
    }

    fn fold(&mut self, event: &deck_model::Event) -> Vec<HostEffect> {
        let data = &event.data;
        match event.kind {
            EventKind::VoiceLevel => {
                // Per-buffer and opt-in on the bus (`api_client::subscribe`
                // names it). It is folded and nothing else happens: a meter
                // that raised an effect would raise one per audio buffer.
                if let Some(rms) = data.get("rms").and_then(Value::as_f64) {
                    self.level = Some(Level {
                        rms: rms as f32,
                        at: event.ts,
                    });
                }
            }
            EventKind::VoiceState => self.fold_voice_state(event),
            EventKind::VoiceRoute => {
                let voice = &mut self.mirror.voice;
                if let Ok(route) = serde_json::from_value::<Route>(
                    data.get("route").cloned().unwrap_or(Value::Null),
                ) {
                    voice.route = route;
                }
                if let Some(uid) = data.get("output_uid") {
                    voice.output_uid = uid.as_str().map(str::to_string);
                }
                if let Some(uid) = data.get("input_uid") {
                    voice.input_uid = uid.as_str().map(str::to_string);
                }
                if let Some(transport) = data.get("transport") {
                    if let Ok(transport) =
                        serde_json::from_value::<Option<VoiceTransport>>(transport.clone())
                    {
                        voice.transport = transport;
                    }
                }
            }
            EventKind::VoiceUtterance => {
                let phase = data.get("phase").and_then(Value::as_str).unwrap_or("");
                match phase {
                    "started" => {
                        self.mirror.voice.speaking = Some(Speaking {
                            utterance: string_of(data, "utterance").unwrap_or_default(),
                            lane: string_of(data, "lane").unwrap_or_default(),
                            // §4.12's `voice.utterance` has no `agent`; the
                            // next `state.get` fills it in.
                            agent: None,
                        });
                    }
                    "ended" | "cut" | "dropped" => self.mirror.voice.speaking = None,
                    // `queued`: nothing is being spoken yet.
                    _ => {}
                }
            }
            EventKind::PermChanged => return self.fold_permissions(data),
            EventKind::DeviceLink => {
                let device = &mut self.mirror.device;
                let link =
                    serde_json::from_value::<Link>(data.get("link").cloned().unwrap_or(Value::Null))
                        .ok();
                if let Some(link) = link {
                    device.link = link;
                }
                if let Ok(session) = serde_json::from_value::<SessionState>(
                    data.get("session").cloned().unwrap_or(Value::Null),
                ) {
                    device.session = session;
                }
                if let Some(sid) = data.get("sid").and_then(Value::as_u64) {
                    device.sid = sid as u32;
                }
                if let Some(fw) = data.get("fw") {
                    device.fw = fw.as_str().map(str::to_string);
                }
                if let Some(boot) = data.get("boot") {
                    device.boot = boot.as_bool();
                }
                if let Some(online) = data.get("host_online").and_then(Value::as_bool) {
                    device.host_online = online;
                }
                // A link that went down: the store emits this event and only
                // then clears what described the deck on it, the firmware
                // this event still carries included (`Store::on_link`). The
                // same clear here, so the mirror agrees with the next
                // snapshot instead of naming a deck that has gone.
                if link == Some(Link::Offline) {
                    device.fw = None;
                    device.face = None;
                    device.language_supported = false;
                    device.transport = None;
                    device.battery = None;
                    device.charging = None;
                    device.board = None;
                }
            }
            EventKind::DeviceStatus => {
                // The six travel together, every time, so they are replaced
                // together or not at all: a status that does not read whole
                // is not applied in half.
                if let Some(status) = DeviceStatus::parse(data) {
                    let device = &mut self.mirror.device;
                    device.transport = status.transport;
                    device.battery = status.battery;
                    device.charging = status.charging;
                    device.board = status.board;
                    device.last = status.last;
                    device.drops = status.drops;
                }
            }
            EventKind::DeviceSession => {
                let device = &mut self.mirror.device;
                if let Some(boot) = data.get("boot") {
                    device.boot = boot.as_bool();
                }
                if let Some(fw) = data.get("fw") {
                    device.fw = fw.as_str().map(str::to_string);
                }
            }
            EventKind::DeviceFace => {
                if let Some(f) = data.get("f").and_then(Value::as_u64) {
                    self.mirror.device.face = u8::try_from(f).ok();
                }
            }
            EventKind::DeviceFaces => {
                if let Ok(list) = serde_json::from_value::<Vec<FaceEntry>>(
                    data.get("list").cloned().unwrap_or(Value::Null),
                ) {
                    // The event is emitted where the host *adopts* the strip,
                    // so both copies move together (`Store::on_device`).
                    self.mirror.device.faces = list.clone();
                    self.mirror.faces = list;
                }
            }
            EventKind::DeviceMacros => {
                // One face's keys, whole, as the snapshot now holds them:
                // replaced, never merged, so a slot the deck emptied is gone
                // here too.
                if let Some((f, layout)) = macros_of(data) {
                    self.mirror.macros.insert(f, layout);
                }
            }
            EventKind::DeviceProfile => {
                // Another deck's strip, settings and keys, all four parts
                // whole or none of them. The language is the Mac's and stays.
                if let Some(profile) = Profile::parse(data) {
                    let keep = |held: &Settings| Settings {
                        language: held.language,
                        language_revision: held.language_revision,
                        ..profile.settings
                    };
                    self.mirror.settings = keep(&self.mirror.settings);
                    self.mirror.device.settings = keep(&self.mirror.device.settings);
                    self.mirror.faces = profile.faces;
                    self.mirror.macros = profile.macros;
                    self.mirror.device.deck = profile.deck;
                }
            }
            EventKind::DeviceSet => {
                let key =
                    serde_json::from_value::<SetKey>(data.get("k").cloned().unwrap_or(Value::Null));
                let value = data
                    .get("v")
                    .and_then(Value::as_u64)
                    .and_then(|v| u16::try_from(v).ok());
                if let (Ok(key), Some(value)) = (key, value) {
                    self.mirror.device.settings.set(key, value);
                    self.mirror.settings.set(key, value);
                    if key == SetKey::Language && key.accepts(value) {
                        if let Some(revision) = data.get("rev").and_then(Value::as_u64).and_then(|rev| u32::try_from(rev).ok()) {
                            self.mirror.device.settings.language_revision = revision;
                            self.mirror.settings.language_revision = revision;
                        }
                    }
                }
            }
            EventKind::UsageUpdated => {
                if let Ok(usage) = serde_json::from_value::<Usage>(
                    data.get("usage").cloned().unwrap_or(Value::Null),
                ) {
                    self.mirror.usage = Some(usage);
                }
            }
            EventKind::ServiceDegraded => return self.fold_degraded(data),
            // The service's collectors stopped or started again: the whole
            // state, or `null` for a service with no dormancy. Nothing to
            // raise; the not-linked page and the Usage panel read it.
            EventKind::ServiceDormancy => {
                self.mirror.service.dormancy = serde_json::from_value(data.clone()).unwrap_or(None);
            }
            EventKind::ServiceShutdown => {
                self.mirror.service.degraded = Some(SHUTTING_DOWN.into());
                return vec![HostEffect::Toast(self.ui_language().text("The service is shutting down").into())];
            }
            // Never a session: an agent and its slot, card, reply and chat,
            // an order and its failure, a confirmation, the focus on an
            // agent and the dictation addressed to one are not this window's
            // to hold, so nothing of them is folded and nothing is raised.
            // With them, the owner-only events (the GUI owns no agent),
            // opt-in device edges, the raw hook stream, and the kinds whose
            // whole content is already in the objects above. They all advance
            // `rev` and change nothing here — which is why the rev check runs
            // before the fold and not inside it.
            _ => {}
        }
        Vec::new()
    }

    /// §7.4's PTT session, as far as an event can carry it: `state` gives
    /// `live`, the event's own `ts` gives `since`, and the rest is in the
    /// data. `finalizing`/`aborted`/`ended` close the session, exactly where
    /// the store's own `finish_listen`/`abort_live` take it.
    ///
    /// Whom the press is for — the agent, its card, the caller's session —
    /// is a session's, and is not kept ([`without_sessions`]).
    fn fold_voice_state(&mut self, event: &deck_model::Event) {
        let data = &event.data;
        let state = data.get("state").and_then(Value::as_str).unwrap_or("");
        match state {
            "arming" | "listening" => {
                let ctx = serde_json::from_value::<deck_model::PttCtx>(
                    data.get("ctx").cloned().unwrap_or(Value::Null),
                );
                let Ok(ctx) = ctx else { return };
                self.mirror.voice.ptt = Some(deck_model::PttInfo {
                    agent: None,
                    ctx,
                    card: None,
                    principal: string_of(data, "principal")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(Principal::Device),
                    session: None,
                    utterance: string_of(data, "utterance").unwrap_or_default(),
                    live: state == "listening",
                    since: event.ts, device_press: None,
                    companion: data.get("companion").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            "finalizing" | "aborted" | "ended" => {
                self.mirror.voice.ptt = None;
                // The meter has nothing to meter once the capture is over.
                // Leaving the last buffer's RMS on screen would draw a live
                // level for a microphone that is shut.
                if state != "finalizing" {
                    self.level = None;
                }
            }
            _ => {}
        }
    }

    /// `perm.changed{microphone, speech, bluetooth}` (§7.8). All three are
    /// folded; the one effect is a sheet for speech recognition that has
    /// not been asked for yet.
    ///
    /// The microphone raises nothing: the Mac's microphone is no longer a
    /// voice path of this app (owner decision 11). Bluetooth has no sheet:
    /// macOS asks by itself when the service's BLE link first starts. And a
    /// denial opens nothing by itself: System Settings is where it is
    /// lifted, and the Voice page's **Open Settings** goes there when the
    /// owner presses it, never unasked.
    fn fold_permissions(&mut self, data: &Value) -> Vec<HostEffect> {
        let read = |key: &str| {
            serde_json::from_value::<Permission>(data.get(key).cloned().unwrap_or(Value::Null)).ok()
        };
        let before = self.mirror.voice.permissions;
        let mut after = before;
        if let Some(p) = read("microphone") {
            after.microphone = p;
        }
        if let Some(p) = read("speech") {
            after.speech = p;
        }
        if let Some(p) = read("bluetooth") {
            after.bluetooth = p;
        }
        self.mirror.voice.permissions = after;
        if before.speech != after.speech && after.speech == Permission::NotDetermined {
            vec![HostEffect::RaisePermissionSheet(SheetKind::Speech)]
        } else {
            Vec::new()
        }
    }

    /// `service.degraded{reason, port, listener}` (§4.1). The toast is
    /// one-time: the daemon re-probes the port every 30 s and re-emits, and
    /// a banner the operator cannot dismiss is not a notification.
    fn fold_degraded(&mut self, data: &Value) -> Vec<HostEffect> {
        let reason = string_of(data, "reason");
        let listener = data.get("listener").cloned().unwrap_or(Value::Null);
        let changed = self.mirror.service.degraded != reason;
        self.mirror.service.degraded = reason.clone();
        if let Some(port) = data
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
        {
            self.mirror.service.port = port;
        }
        self.mirror.service.listener = serde_json::from_value(listener).unwrap_or(None);
        if !changed {
            return Vec::new();
        }
        let Some(reason) = reason else {
            return vec![HostEffect::Toast(self.ui_language().text("The service recovered").into())];
        };
        // §4.1's two port words: another program holds the TCP door's port,
        // or holds it and answers on it. Either way the local API is closed
        // until the service gets its port back, and that is all: the deck,
        // the plugin's hooks and the connected apps are on the socket. Who
        // holds it is said by name when the service could tell.
        if crate::admin::port_clash(&reason) {
            let who = self
                .mirror
                .service
                .listener
                .as_ref()
                .and_then(|l| l.command.clone())
                .unwrap_or_else(|| self.ui_language().text("another process").into());
            let port = self.mirror.service.port;
            return vec![HostEffect::Toast(self.ui_language().format(
                "Port {0} is held by {1}, so the local API is closed until the port is free.",
                &[&port.to_string(), &who],
            ))];
        }
        vec![HostEffect::Toast(self.ui_language().format("Service degraded: {0}", &[&reason]))]
    }
}

/// A snapshot as this window keeps it: the deck, its voice, the service and
/// the settings, and never a session. The agents (with their cards, replies
/// and chat) and the focus that names one of them are dropped on the way in,
/// as every event about them is dropped by the fold, and so is whom a press
/// or a spoken reply is for — the agent, its card, the caller's session —
/// so nothing about a session is ever held here to be shown.
fn without_sessions(mut snapshot: Snapshot) -> Snapshot {
    snapshot.agents.clear();
    snapshot.focus = Focus::default();
    if let Some(ptt) = snapshot.voice.ptt.as_mut() {
        ptt.agent = None;
        ptt.card = None;
        ptt.session = None;
    }
    if let Some(speaking) = snapshot.voice.speaking.as_mut() {
        speaking.agent = None;
    }
    snapshot
}

/// A content string that may be present, absent, or `null` because the
/// reader's scope redacted it (§4.3).
fn string_of(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `device.status` as the store emits it (`deck_model`'s `device_status`):
/// all six keys every time, `null` for what is unknown and `[]` for no
/// drops.
struct DeviceStatus {
    transport: Option<Transport>,
    battery: Option<u8>,
    charging: Option<bool>,
    board: Option<String>,
    last: Option<LastLinked>,
    drops: Vec<LinkDrop>,
}

impl DeviceStatus {
    /// `None` when a key is missing or does not read as the store's own
    /// type: a newer service's word for a transport, say. The status is then
    /// left as it was rather than half replaced.
    fn parse(data: &Value) -> Option<DeviceStatus> {
        let field = |key: &str| data.get(key).cloned();
        Some(DeviceStatus {
            transport: serde_json::from_value(field("transport")?).ok()?,
            battery: serde_json::from_value(field("battery")?).ok()?,
            charging: serde_json::from_value(field("charging")?).ok()?,
            board: serde_json::from_value(field("board")?).ok()?,
            last: serde_json::from_value(field("last")?).ok()?,
            drops: serde_json::from_value(field("drops")?).ok()?,
        })
    }
}

/// `device.profile{deck, faces, settings, macros}`: the deck that linked,
/// and the strip, settings and keys the service keeps for it.
struct Profile {
    deck: Option<String>,
    faces: Vec<FaceEntry>,
    settings: Settings,
    macros: BTreeMap<u8, MacroLayout>,
}

impl Profile {
    /// `None` unless every part reads, so a malformed event replaces
    /// nothing.
    fn parse(data: &Value) -> Option<Profile> {
        let field = |key: &str| data.get(key).cloned();
        Some(Profile {
            deck: serde_json::from_value(field("deck")?).ok()?,
            faces: serde_json::from_value(field("faces")?).ok()?,
            settings: serde_json::from_value(field("settings")?).ok()?,
            macros: serde_json::from_value(field("macros")?).ok()?,
        })
    }
}

/// `device.macros{f, src, keys}` as the face id and its layout. `None` when
/// the face or the keys do not read, so a malformed event replaces nothing.
fn macros_of(data: &Value) -> Option<(u8, MacroLayout)> {
    let f = u8::try_from(data.get("f")?.as_u64()?).ok()?;
    let src =
        serde_json::from_value::<Option<String>>(data.get("src").cloned().unwrap_or(Value::Null))
            .ok()?;
    let keys = serde_json::from_value::<Vec<MacroKey>>(data.get("keys")?.clone()).ok()?;
    Some((f, MacroLayout { src, keys }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_from_250_ms_and_stops_at_5_s() {
        let mut backoff = Backoff::new();
        let waits: Vec<u64> = (0..8).map(|_| backoff.wait().as_millis() as u64).collect();
        assert_eq!(waits, vec![250, 500, 1000, 2000, 4000, 5000, 5000, 5000]);
        backoff.reset();
        assert_eq!(backoff.peek(), BACKOFF_MIN);
    }

    #[test]
    fn a_ui_action_is_the_json_rpc_call_of_the_design() {
        // §4.10's `{k, v}`, the key going out as the wire word `SetKey`
        // serialises to.
        let (method, params) = UiAction::DeviceSet {
            key: SetKey::Announce,
            value: 0,
        }
        .request();
        assert_eq!(method, "device.set");
        assert_eq!(params, serde_json::json!({"k": "announce", "v": 0}));
        assert_eq!(
            UiAction::StateGet.request(),
            ("state.get", serde_json::json!({}))
        );
    }

    #[test]
    fn the_admin_screens_actions_are_the_methods_the_contract_names_and_nothing_else() {
        use crate::admin::Verb;
        let call = |action: UiAction| {
            let (method, params) = action.request();
            (method, params)
        };
        let (method, params) = call(UiAction::IntegrationsStatus { detect: true });
        assert_eq!(method, "integrations.status");
        assert_eq!(params["detect"], true);
        let (method, params) = call(UiAction::Integrations {
            verb: Verb::Install,
            target: "claude".into(),
            trust: false,
            dry_run: true,
        });
        assert_eq!(method, "integrations.install");
        assert_eq!(params["target"], "claude");
        assert_eq!(
            params["dry_run"], true,
            "\"What this writes\" must never be an apply"
        );
        assert_eq!(params["trust"], false);
        assert_eq!(
            call(UiAction::Integrations {
                verb: Verb::Uninstall,
                target: "codex".into(),
                trust: false,
                dry_run: false,
            })
            .0,
            "integrations.uninstall"
        );
        let (method, params) = call(UiAction::TokenCreate {
            name: "script".into(),
            scopes: vec!["observe".into(), "operate".into()],
            expires: None,
        });
        assert_eq!(method, "token.create");
        assert_eq!(params["name"], "script");
        assert_eq!(params["scopes"][1], "operate");
        assert_eq!(params["expires"], Value::Null);
        assert_eq!(call(UiAction::TokenList).0, "token.list");
        assert_eq!(
            call(UiAction::TokenRevoke {
                name: "hooks".into()
            })
            .1["name"],
            "hooks"
        );
        assert_eq!(
            call(UiAction::TokenRotate {
                name: "hooks".into()
            })
            .0,
            "token.rotate"
        );
        assert_eq!(call(UiAction::PermStatus).0, "perm.status");
        assert_eq!(call(UiAction::ServiceInfo).0, "service.info");
        assert_eq!(call(UiAction::ConfigGet).0, "config.get");
        // §4.11's `{patch}`: a map of the dotted names `config.get` publishes.
        // The daemon refuses `{key, value}` outright — one shape, not two — so
        // a flat pair here would be `-32602` on every port change.
        let (method, params) = call(UiAction::ConfigSet {
            key: "api.port".into(),
            value: ConfigValue::Int(47_202),
        });
        assert_eq!(method, "config.set");
        assert_eq!(params["patch"]["api.port"], 47_202);
        assert!(
            params.get("key").is_none(),
            "the flat pair is not the wire shape"
        );
        assert!(params.get("value").is_none());
        assert_eq!(
            call(UiAction::ConfigSet {
                key: "hooks.enabled".into(),
                value: ConfigValue::Bool(false),
            })
            .1["patch"]["hooks.enabled"],
            false
        );
        assert_eq!(call(UiAction::ServiceStop).0, "service.stop");
        assert_eq!(call(UiAction::ServiceRestart).0, "service.restart");
        let (method, params) = call(UiAction::LogTail {
            lines: 200,
            level: Some("WARN".into()),
        });
        assert_eq!(method, "log.tail");
        assert_eq!(params["lines"], 200);
        assert_eq!(params["level"], "WARN");
        assert_eq!(
            call(UiAction::LogTail {
                lines: 200,
                level: None
            })
            .1["level"],
            Value::Null,
            "no filter is no level, never the string \"none\""
        );
    }

    /// The service builds the store's `Request` out of `{method, params}`
    /// (`api::dispatch`), so a round trip through it proves the service
    /// reads exactly the strip and the keys the page meant to send.
    #[test]
    fn the_strip_and_the_keys_go_out_as_the_requests_the_service_parses() {
        use deck_model::{Binding, Request, Template};
        let parsed = |action: &UiAction| {
            let (method, params) = action.request();
            let call = serde_json::json!({"method": method, "params": params});
            serde_json::from_value::<Request>(call).unwrap_or_else(|e| panic!("{method}: {e}"))
        };
        let list = vec![
            FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "Agents".into(),
            },
            FaceEntry {
                f: 3,
                t: Template::Macropad,
                n: "Macropad".into(),
            },
            FaceEntry {
                f: 5,
                t: Template::Settings,
                n: "Settings".into(),
            },
        ];
        let faces = UiAction::DeviceFacesSet { list: list.clone() };
        assert_eq!(faces.request().0, "device.faces.set");
        assert_eq!(
            faces.request().1["list"][1],
            serde_json::json!({"f": 3, "t": "macropad", "n": "Macropad"})
        );
        assert_eq!(parsed(&faces), Request::DeviceFacesSet { list });

        let keys = vec![
            MacroKey {
                label: "build".into(),
                binding: Some(Binding::Left),
                keys: Some("cmd+b".into()),
            },
            // An empty slot in the middle keeps the key after it in place.
            MacroKey {
                label: String::new(),
                binding: None,
                keys: None,
            },
            MacroKey {
                label: "shot".into(),
                binding: None,
                keys: Some("cmd+shift+4".into()),
            },
        ];
        let set = UiAction::DeviceMacroSet {
            f: 3,
            layout: "app".into(),
            src: None,
            keys: keys.clone(),
        };
        let (method, params) = set.request();
        assert_eq!(method, "device.macro.set");
        assert_eq!(
            params,
            serde_json::json!({"f": 3, "layout": "app", "src": null, "keys": [
                {"label": "build", "binding": "left", "keys": "cmd+b"},
                {"label": "", "binding": null},
                {"label": "shot", "binding": null, "keys": "cmd+shift+4"}
            ]})
        );
        assert_eq!(
            parsed(&set),
            Request::DeviceMacroSet {
                f: 3,
                layout: "app".into(),
                src: None,
                keys: keys.clone(),
            }
        );
        // The deck's tag goes back as it was read.
        let tagged = UiAction::DeviceMacroSet {
            f: 3,
            layout: "app".into(),
            src: Some("deck.toml".into()),
            keys: keys.clone(),
        };
        assert_eq!(tagged.request().1["src"], "deck.toml");
        assert_eq!(
            parsed(&tagged),
            Request::DeviceMacroSet {
                f: 3,
                layout: "app".into(),
                src: Some("deck.toml".into()),
                keys,
            }
        );
    }

    #[test]
    fn a_config_patch_moves_its_keys_in_one_call_and_a_list_goes_out_whole() {
        let names = ConfigValue::List(vec!["codex".into(), "claude".into()]);
        assert_eq!(names.json(), serde_json::json!(["codex", "claude"]));
        assert_eq!(ConfigValue::List(Vec::new()).json(), serde_json::json!([]));
        let (method, params) = UiAction::ConfigPatch {
            entries: vec![
                ("voice.stt.language".into(), ConfigValue::Text("en".into())),
                ("voice.stt.locale".into(), ConfigValue::Text("en-GB".into())),
                ("usage.providers".into(), names.clone()),
                ("sources.apps_paused".into(), ConfigValue::List(Vec::new())),
                ("voice.tts.model".into(), ConfigValue::Null),
            ],
        }
        .request();
        assert_eq!(method, "config.set");
        assert_eq!(
            params,
            serde_json::json!({"patch": {
                "voice.stt.language": "en",
                "voice.stt.locale": "en-GB",
                "usage.providers": ["codex", "claude"],
                "sources.apps_paused": [],
                "voice.tts.model": null
            }})
        );
        // One map: a key named twice goes out once, with the later value.
        let (_, twice) = UiAction::ConfigPatch {
            entries: vec![
                ("hooks.enabled".into(), ConfigValue::Bool(false)),
                ("hooks.enabled".into(), ConfigValue::Bool(true)),
            ],
        }
        .request();
        assert_eq!(twice, serde_json::json!({"patch": {"hooks.enabled": true}}));
        // A one-key `ConfigSet` with a list is the same shape.
        assert_eq!(
            UiAction::ConfigSet {
                key: "usage.providers".into(),
                value: names,
            }
            .request()
            .1,
            serde_json::json!({"patch": {"usage.providers": ["codex", "claude"]}})
        );
    }

    #[test]
    fn the_care_actions_are_the_methods_the_contract_names_and_carry_only_their_own_params() {
        for (action, method, params) in [
            (UiAction::SourcesList, "sources.list", serde_json::json!({})),
            (UiAction::TestQuestion, "care.test.question", serde_json::json!({})),
            (UiAction::TestVoice, "care.test.voice", serde_json::json!({})),
            (UiAction::TestSay, "care.test.say", serde_json::json!({})),
            (UiAction::TestStatus, "care.test.status", serde_json::json!({})),
            (UiAction::CareChecks, "care.checks", serde_json::json!({})),
            (UiAction::CareFootprint, "care.footprint", serde_json::json!({})),
            // The two that write or delete are refused `{}` by the service,
            // so they always name what they act on.
            (
                UiAction::CareSupportFile {
                    to: "desktop".into(),
                },
                "care.support_file",
                serde_json::json!({"to": "desktop"}),
            ),
            (
                UiAction::CareCacheClear {
                    what: "firmware".into(),
                },
                "care.cache.clear",
                serde_json::json!({"what": "firmware"}),
            ),
        ] {
            assert_eq!(action.request(), (method, params), "{action:?}");
        }
    }

    #[test]
    fn a_config_value_is_the_three_shapes_a_screen_has_a_control_for() {
        assert_eq!(ConfigValue::Bool(true).json(), Value::Bool(true));
        assert_eq!(ConfigValue::Int(47_201).json(), Value::from(47_201));
        assert_eq!(
            ConfigValue::Text("apple".into()).json(),
            Value::String("apple".into())
        );
    }
}
