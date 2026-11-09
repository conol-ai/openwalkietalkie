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
use crate::events::{ApiEvent, AppEvent};
use crate::ApiClient;
use deck_model::{
    Agent, AgentId, Card, CardId, CardStatus, EventKind, FaceEntry, Focus, FocusSource, Link,
    Permission, Principal, Route, SessionState, SetKey, Slot, Snapshot, Speaking, Ts, Usage,
    VoiceTransport,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// §8.1's reconnect schedule, and `deck-client`'s: one definition, so what
/// the yellow bar counts down and what the socket actually does cannot drift.
pub use deck_client::{BACKOFF_MAX, BACKOFF_MIN};

/// Orders kept per agent for the Deck screen's order history — enough to
/// show "the exact string typed or pushed" for a session's recent work
/// without the mirror growing without bound.
pub const ORDER_HISTORY: usize = 32;

/// `x-apple.systempreferences:` URLs for the two panes a *denied* permission
/// can only be fixed in: macOS will not prompt twice, so the sheet is
/// useless and System Settings is the only recourse (§7.8).
pub const MICROPHONE_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
pub const SPEECH_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition";
/// Where a Bluetooth denial is lifted (the BLE voice transport needs it).
pub const BLUETOOTH_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth";

/// §4.2's guarded operation: an untrusted principal asked for a permission
/// allow or an `inject` order and the daemon said `-32002 {need:"trusted",
/// how:"gui_confirm"}`. The operator's click on the sheet re-issues the call
/// from here, which *is* the trusted principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub agent: Option<AgentId>,
    pub card: Option<CardId>,
    /// The exact order text, when the guarded operation was an `op.order`.
    pub order: Option<String>,
    /// Who asked and was refused.
    pub principal: Option<Principal>,
}

/// Which sheet [`HostEffect::RaisePermissionSheet`] asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetKind {
    /// macOS has not been asked for the microphone yet (§7.8).
    Microphone,
    /// …nor for speech recognition.
    Speech,
    /// §4.2's confirmation sheet.
    Confirm(ConfirmRequest),
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
    /// Bring the window forward: something needs the operator's eyes now.
    ShowWindow,
    RaisePermissionSheet(SheetKind),
    Toast(String),
    /// Connect again after this long (250 ms doubling to 5 s).
    Reconnect(Duration),
    OpenUrl(String),
    /// The mirror is stale: fetch the whole snapshot again.
    StateGet,
}

/// One entry of the Deck screen's order history: "the exact string typed or
/// pushed, shown before an `inject` executes" (§8.2).
///
/// It is kept here rather than derived on the fly because `order.failed`
/// carries `{agent, reason, text_kept:true}` and **no text** (§4.12) — the
/// text was in the `order.dispatched` before it. Without this history the
/// Deck screen's Copy button on a failed order would have nothing to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderEntry {
    pub text: Option<String>,
    /// `"inject"`, `"push"`, `"poll"`; `None` when the order never got a route.
    pub via: Option<String>,
    /// `Some` once an `order.failed` landed on this entry.
    pub reason: Option<String>,
    pub at: Ts,
}

/// The last capture RMS the daemon reported (`voice.level`, §7.1), for the
/// Voice screen's meter. Not part of the `Snapshot` for the same reason the
/// transcript is not: it is a per-buffer stream, not state.
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

/// The live dictation, for the PTT indicator. Not part of the `Snapshot`:
/// transcripts are a stream, not state (§4.3 keeps none of them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub agent: Option<AgentId>,
    pub utterance: String,
    pub text: String,
    /// §5.6's matched intent (`allow`, `deny`, `choice:n`, …), when there was one.
    pub intent: Option<String>,
    pub final_: bool,
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
/// week 6's screens actually set — a port, a flag, a name — are these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Text(String),
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
            ConfigValue::Null => Value::Null,
        }
    }
}

/// What a click asks the daemon to do (§8.2's Actions column). Turning one
/// into a `(method, params)` pair is a pure function and sending one takes
/// `&self`, so there is no path from a UI action to a write on the mirror —
/// the events the call produces are the only writer.
///
/// The Deck screen's six come first; the Integrations and Service screens'
/// follow. The second group differs in one way worth naming: none of them
/// produces an event, so nothing folds their answers into the mirror and the
/// screens keep what they asked for in their own view state
/// ([`crate::admin`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    ImageGet { agent: AgentId, image: String, offset: usize },
    Answer {
        agent: AgentId,
        card: CardId,
        choice: Option<u8>,
        text: Option<String>,
    },
    Snooze {
        agent: AgentId,
        card: CardId,
    },
    Ack {
        agent: AgentId,
        card: CardId,
    },
    Focus {
        agent: AgentId,
    },
    Order {
        agent: AgentId,
        text: String,
    },
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
    /// `perm.status` — where §8.2's **Ghostty Automation** card comes from.
    /// It is not an `integrations.*` target: nothing is written for it, and
    /// what there is to report is a TCC grant the injector learned by
    /// trying (§5.5).
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

    // ---- the Voice screen and the tray (§8.2) --------------------------
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
    /// `device.set{k, v}` — the tray's **Mute announcements**. The only
    /// device write any week-6 screen makes, and the one key it writes.
    DeviceSet {
        key: deck_model::SetKey,
        value: u16,
    },
}

impl UiAction {
    /// The JSON-RPC call this action is.
    pub fn request(&self) -> (&'static str, Value) {
        match self {
            UiAction::Answer {
                agent,
                card,
                choice,
                text,
            } => (
                "op.answer",
                serde_json::json!({"agent": agent, "card": card, "choice": choice, "text": text}),
            ),
            UiAction::Snooze { agent, card } => (
                "op.snooze",
                serde_json::json!({"agent": agent, "card": card}),
            ),
            UiAction::Ack { agent, card } => {
                ("op.ack", serde_json::json!({"agent": agent, "card": card}))
            }
            UiAction::Focus { agent } => ("op.focus", serde_json::json!({"agent": agent})),
            // §4.7: a typed order is `source:"gui"`, and `ctx:"talk"` is the
            // default the reducer would have filled in anyway.
            UiAction::Order { agent, text } => (
                "op.order",
                serde_json::json!({"agent": agent, "text": text, "ctx": "talk", "source": "gui"}),
            ),
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
            UiAction::ImageGet { agent, image, offset } => ("image.get", serde_json::json!({"agent":agent,"image":image,"offset":offset})),
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
        }
    }
}

/// The mirror of the daemon's `Snapshot` plus the little the GUI needs that
/// the snapshot does not carry (the live transcript, the order history).
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
    orders: BTreeMap<AgentId, Vec<OrderEntry>>,
    transcript: Option<Transcript>,
    level: Option<Level>,
}

impl std::fmt::Debug for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostState")
            .field("rev", &self.mirror.rev)
            .field("agents", &self.mirror.agents.len())
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
            orders: BTreeMap::new(),
            transcript: None,
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

    /// Whether the GUI was verified as §4.2's trusted principal. False on an
    /// ad-hoc dev build without `[trust] dev_requirement` — a red badge, not
    /// an error: an untrusted GUI still observes and still operates, it just
    /// cannot allow a permission or inject without the confirmation path.
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

    /// The Deck screen's order history for one agent, oldest first.
    pub fn orders(&self, agent: &AgentId) -> &[OrderEntry] {
        self.orders
            .get(agent)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The dictation in flight, if any.
    pub fn transcript(&self) -> Option<&Transcript> {
        self.transcript.as_ref()
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
            // A tray click is an *action*, and the reducer issues none: it
            // folds what the daemon reported and says what the app should
            // do about it. The window turns the command into the same
            // `UiAction` a button on a screen would send, so the tray and
            // the screens take one path to the daemon and the mirror has
            // one writer — the events that come back.
            AppEvent::Tray(_) => Vec::new(),
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
        self.mirror = snapshot;
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
        self.mirror = snapshot;
        self.resyncing = false;
        Vec::new()
    }

    /// The connection ended or an attempt failed. The last mirror is kept —
    /// greyed, not thrown away: a roster from two seconds ago is worth more
    /// to the operator than an empty window.
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
            EventKind::AgentAdded => {
                if let Ok(agent) = serde_json::from_value::<Agent>(data["agent"].clone()) {
                    self.mirror.agents.insert(agent.id.clone(), agent);
                }
            }
            EventKind::AgentChanged => {
                // `changed` carries only the fields that changed (§4.12), so
                // the merge goes through JSON: the daemon's own field names
                // are the contract and this crate does not restate them.
                if let (Some(id), Some(fields)) = (agent_of(data), data.get("fields")) {
                    if let Some(agent) = self.mirror.agents.get_mut(&id) {
                        merge_fields(agent, fields);
                    }
                }
            }
            EventKind::AgentRemoved => {
                if let Some(id) = agent_of(data) {
                    self.mirror.agents.remove(&id);
                    self.orders.remove(&id);
                    if self.mirror.focus.agent.as_ref() == Some(&id) {
                        self.mirror.focus = Focus::default();
                    }
                }
            }
            EventKind::AgentSlot => {
                if let Some(agent) = agent_of(data).and_then(|id| self.mirror.agents.get_mut(&id)) {
                    agent.slot = data
                        .get("slot")
                        .and_then(Value::as_u64)
                        .and_then(|s| u8::try_from(s).ok())
                        .map(Slot);
                }
            }
            EventKind::CardSet => {
                if let Some(id) = agent_of(data) {
                    if let Ok(card) = serde_json::from_value::<Card>(data["card"].clone()) {
                        if let Some(agent) = self.mirror.agents.get_mut(&id) {
                            agent.card = Some(card);
                        }
                    }
                }
            }
            EventKind::CardAnswered => {
                let choice = data
                    .get("choice")
                    .and_then(Value::as_u64)
                    .and_then(|c| u8::try_from(c).ok());
                let text = string_of(data, "text");
                self.with_card(data, |card| {
                    card.status = CardStatus::Answering { choice, text };
                });
            }
            EventKind::CardDelivered => {
                self.with_card(data, |card| card.status = CardStatus::Delivered);
            }
            EventKind::CardFailed => {
                let reason = string_of(data, "reason").unwrap_or_default();
                self.with_card(data, |card| card.status = CardStatus::Failed { reason });
            }
            EventKind::CardSnoozed => {
                self.with_card(data, |card| card.status = CardStatus::Snoozed);
            }
            EventKind::CardDegraded => {
                // The same id re-pushed as a `⌨ Focus terminal` card (§4.8);
                // `deliver:"none"` is the neutralised, informational case.
                let none = data.get("deliver").and_then(Value::as_str) == Some("none");
                self.with_card(data, |card| {
                    if none {
                        card.deliver = deck_model::Deliver::None;
                        card.choices.clear();
                        card.rec = None;
                    } else {
                        card.status = CardStatus::Degraded { focused_at: None };
                        card.deliver = deck_model::Deliver::Focus;
                        card.expires = None;
                    }
                });
            }
            EventKind::CardFocused => {
                // A degraded card is kept with its "focused at" until the
                // hook flow ends; anything else is cleared by the
                // `card.cleared` that follows this event.
                let at = event.ts;
                self.with_card(data, |card| {
                    if matches!(card.status, CardStatus::Degraded { .. }) {
                        card.status = CardStatus::Degraded {
                            focused_at: Some(at),
                        };
                    }
                });
            }
            EventKind::CardAcked => {
                self.with_card(data, |card| card.status = CardStatus::Cleared);
            }
            EventKind::CardExpired | EventKind::CardCleared => {
                if let Some(id) = agent_of(data) {
                    let card = card_of(data);
                    if let Some(agent) = self.mirror.agents.get_mut(&id) {
                        if agent.card.as_ref().is_some_and(|c| Some(c.id) == card) {
                            agent.card = None;
                        }
                    }
                }
            }
            EventKind::ReplySet => {
                if let Some(agent) = agent_of(data).and_then(|id| self.mirror.agents.get_mut(&id)) {
                    let meta = data.get("message").and_then(|v| serde_json::from_value::<deck_model::MessageMeta>(v.clone()).ok());
                    if let (Some(message), Some(text)) = (&meta, string_of(data, "text")) {
                        deck_model::workflow::observe(agent, message);
                        let entry = deck_model::ChatMessage { message: message.clone(), text: deck_model::sanitize::cut(&text, 64 * 1024), at: event.ts };
                        if let Some(old) = agent.messages.iter_mut().find(|m| m.message.id == message.id) { *old = entry; }
                        else { agent.messages.push(entry); }
                        while agent.messages.len() > 64 || agent.messages.iter().map(|m| m.text.len()).sum::<usize>() > 512 * 1024 { agent.messages.remove(0); }
                    }
                    if meta.as_ref().is_none_or(|m| m.role == deck_model::ChatRole::Assistant && !m.streaming) {
                    agent.reply = Some(deck_model::Reply {
                        text: string_of(data, "text"),
                        at: event.ts,
                        announce: string_of(data, "announce"),
                        redacted: data.get("text").is_some_and(Value::is_null),
                    });
                    }
                }
            }
            EventKind::OrderDispatched => {
                let entry = OrderEntry {
                    text: string_of(data, "text"),
                    via: string_of(data, "via"),
                    reason: None,
                    at: event.ts,
                };
                if let Some(id) = agent_of(data) {
                    self.push_order(id, entry);
                }
            }
            EventKind::OrderFailed => {
                let reason = string_of(data, "reason").unwrap_or_else(|| "failed".into());
                if let Some(id) = agent_of(data) {
                    let history = self.orders.entry(id).or_default();
                    match history.last_mut() {
                        // The text was in the `order.dispatched` before this.
                        Some(last) if last.reason.is_none() => {
                            last.reason = Some(reason.clone());
                        }
                        // Refused before it was ever dispatched (`dialog_open`,
                        // `not_foreground`): there is no text to keep.
                        _ => history.push(OrderEntry {
                            text: None,
                            via: None,
                            reason: Some(reason.clone()),
                            at: event.ts,
                        }),
                    }
                    let len = history.len();
                    if len > ORDER_HISTORY {
                        history.drain(..len - ORDER_HISTORY);
                    }
                }
                return vec![HostEffect::Toast(self.ui_language().format("Order not delivered: {0}", &[&reason]))];
            }
            EventKind::OpConfirmRequested => {
                return vec![
                    HostEffect::ShowWindow,
                    HostEffect::RaisePermissionSheet(SheetKind::Confirm(ConfirmRequest {
                        agent: agent_of(data),
                        card: card_of(data),
                        order: string_of(data, "order"),
                        principal: string_of(data, "principal")
                            .and_then(|p| p.parse::<Principal>().ok()),
                    })),
                ];
            }
            EventKind::TranscriptPartial | EventKind::TranscriptFinal => {
                self.transcript = Some(Transcript {
                    agent: agent_of(data),
                    utterance: string_of(data, "utterance").unwrap_or_default(),
                    text: string_of(data, "text").unwrap_or_default(),
                    intent: string_of(data, "intent"),
                    final_: event.kind == EventKind::TranscriptFinal,
                });
            }
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
                if let Ok(link) =
                    serde_json::from_value::<Link>(data.get("link").cloned().unwrap_or(Value::Null))
                {
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
            EventKind::FocusChanged => {
                self.mirror.focus = Focus {
                    agent: agent_of(data),
                    seq: data
                        .get("seq")
                        .and_then(Value::as_u64)
                        .and_then(|s| u16::try_from(s).ok()),
                    source: serde_json::from_value::<FocusSource>(
                        data.get("source").cloned().unwrap_or(Value::Null),
                    )
                    .ok(),
                };
            }
            EventKind::UsageUpdated => {
                if let Ok(usage) = serde_json::from_value::<Usage>(
                    data.get("usage").cloned().unwrap_or(Value::Null),
                ) {
                    self.mirror.usage = Some(usage);
                }
            }
            EventKind::ServiceDegraded => return self.fold_degraded(data),
            EventKind::ServiceShutdown => {
                self.mirror.service.degraded = Some("shutdown".into());
                return vec![HostEffect::Toast(self.ui_language().text("The service is shutting down").into())];
            }
            // Owner-only events (the GUI owns no agent), opt-in device edges,
            // the raw hook stream, and the kinds whose whole content is
            // already in the objects above. They advance `rev` and change
            // nothing here — which is why the rev check runs before the fold
            // and not inside it.
            _ => {}
        }
        Vec::new()
    }

    /// §7.4's PTT session, as far as an event can carry it: `state` gives
    /// `live`, the event's own `ts` gives `since`, and the rest is in the
    /// data. `finalizing`/`aborted`/`ended` close the session, exactly where
    /// the store's own `finish_listen`/`abort_live` take it.
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
                    agent: agent_of(data),
                    ctx,
                    card: card_of(data),
                    principal: string_of(data, "principal")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(Principal::Device),
                    session: serde_json::from_value(
                        data.get("session").cloned().unwrap_or(Value::Null),
                    )
                    .unwrap_or(None),
                    utterance: string_of(data, "utterance").unwrap_or_default(),
                    live: state == "listening",
                    since: event.ts, device_press: None,
                    companion: data.get("companion").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            "finalizing" | "aborted" | "ended" => {
                self.mirror.voice.ptt = None;
                if state != "finalizing" {
                    self.transcript = None;
                }
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

    /// `perm.changed{microphone, speech, bluetooth}` (§7.8). A permission
    /// that has not been asked for yet is a sheet; one that was *denied*
    /// cannot be asked again, so the only honest offer is System Settings.
    /// Bluetooth has no sheet: macOS asks by itself when the service's BLE
    /// link first starts, so there is nothing for a sheet to do.
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
        let mut out = Vec::new();
        for (was, now, sheet, url) in [
            (
                before.microphone,
                after.microphone,
                Some(SheetKind::Microphone),
                MICROPHONE_SETTINGS,
            ),
            (
                before.speech,
                after.speech,
                Some(SheetKind::Speech),
                SPEECH_SETTINGS,
            ),
            (before.bluetooth, after.bluetooth, None, BLUETOOTH_SETTINGS),
        ] {
            if was == now {
                continue;
            }
            match (now, sheet) {
                (Permission::NotDetermined, Some(sheet)) => {
                    out.push(HostEffect::RaisePermissionSheet(sheet));
                }
                (Permission::Denied, _) => out.push(HostEffect::OpenUrl(url.to_string())),
                _ => {}
            }
        }
        out
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
        // §4.1: the installed Claude http hooks keep POSTing prompts, tool
        // inputs and the `hooks` bearer token to whoever holds the port, and
        // each held prompt blocks Claude for its timeout. That is worth the
        // operator's attention by name.
        if reason == "hooks_hijack_risk" {
            let who = self
                .mirror
                .service
                .listener
                .as_ref()
                .and_then(|l| l.command.clone())
                .unwrap_or_else(|| self.ui_language().text("another process").into());
            let port = self.mirror.service.port;
            return vec![HostEffect::Toast(self.ui_language().format(
                "Port {0} is held by {1}; Claude hooks may stall", &[&port.to_string(), &who]
            ))];
        }
        vec![HostEffect::Toast(self.ui_language().format("Service degraded: {0}", &[&reason]))]
    }

    /// Run `f` on the named agent's current card, and only when the event's
    /// `card` is that card: a lifecycle event for a card the agent has
    /// already replaced belongs to nothing.
    fn with_card(&mut self, data: &Value, f: impl FnOnce(&mut Card)) {
        let Some(id) = agent_of(data) else { return };
        let card = card_of(data);
        let Some(agent) = self.mirror.agents.get_mut(&id) else {
            return;
        };
        if let Some(current) = agent.card.as_mut() {
            if Some(current.id) == card {
                f(current);
            }
        }
    }

    fn push_order(&mut self, agent: AgentId, entry: OrderEntry) {
        let history = self.orders.entry(agent).or_default();
        history.push(entry);
        let len = history.len();
        if len > ORDER_HISTORY {
            history.drain(..len - ORDER_HISTORY);
        }
    }
}

/// `data.agent` as an `AgentId` (`null` on the one `order.failed` that has
/// no agent at all — `reason:"no_agent"`).
fn agent_of(data: &Value) -> Option<AgentId> {
    data.get("agent")
        .and_then(Value::as_str)
        .map(|id| AgentId(id.to_string()))
}

/// `data.card` as a `CardId`. `card.set` carries the whole card object, so
/// this reads the nested `id` there too.
fn card_of(data: &Value) -> Option<CardId> {
    let card = data.get("card")?;
    let id = if card.is_object() {
        card.get("id")?.as_u64()?
    } else {
        card.as_u64()?
    };
    u16::try_from(id).ok().map(CardId)
}

/// A content string that may be present, absent, or `null` because the
/// reader's scope redacted it (§4.3).
fn string_of(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Merge an `agent.changed{fields}` object into an agent. The round trip
/// through JSON is the point: the field names are the daemon's, and keys
/// that are not `Agent` fields at all (`orphaned`, `reason`) are ignored by
/// serde exactly as every other client ignores them.
fn merge_fields(agent: &mut Agent, fields: &Value) {
    let Some(fields) = fields.as_object() else {
        return;
    };
    let Ok(mut value) = serde_json::to_value(&*agent) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for (key, field) in fields {
        object.insert(key.clone(), field.clone());
    }
    if let Ok(merged) = serde_json::from_value::<Agent>(value) {
        *agent = merged;
    }
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
        let agent = AgentId("ag_1".into());
        let (method, params) = UiAction::Order {
            agent: agent.clone(),
            text: "run the smoke tests".into(),
        }
        .request();
        assert_eq!(method, "op.order");
        assert_eq!(params["source"], "gui");
        assert_eq!(params["ctx"], "talk");
        assert_eq!(params["text"], "run the smoke tests");
        let (method, params) = UiAction::Answer {
            agent,
            card: CardId(17),
            choice: Some(0),
            text: None,
        }
        .request();
        assert_eq!(method, "op.answer");
        assert_eq!(params["card"], 17);
        assert_eq!(params["choice"], 0);
        assert_eq!(params["text"], Value::Null);
        assert_eq!(UiAction::StateGet.request().0, "state.get");
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
