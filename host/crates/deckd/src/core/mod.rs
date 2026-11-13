//! The core actor (design §2.2, §3; contract §3.2): a std thread named
//! `deckd-core` that owns the `Store`, applies every `Command` in arrival
//! order and executes the resulting effects. Its inputs are the hid thread
//! (drained first: device priority), the doors' `CoreMsg`s through
//! `CoreHandle`, and the 250 ms tick; its outputs are device ops, the bus
//! (every event, and the snapshot cell) and the replies. `--demo` is an
//! in-process API client that registers the mockup roster and reacts to
//! the deck like the reference host.
pub mod dispatch;
pub mod guard;
pub mod handle;
pub mod holds;
mod images;
pub mod sessions;
mod test_agent;
mod updates;

pub use handle::{
    CallCtx, ClientInfo, CoreHandle, CoreMsg, Delivery, HoldGuard, HoldResult, HoldRx, OpenSession,
    Outbound, Reply, SessionKind, WaitResult,
};

use crate::audio::{AudioHandle, FromAudio};
use crate::bus::{Bus, Published};
use crate::config::{Config, Paths};
use crate::demo::{self, Act, Demo};
use crate::desktop::{DesktopHandle, DesktopJob, FromDesktop};
use crate::hid::{FromHid, HidDebug, HidHandle};
use crate::metrics::{Metric, Status};
use crate::state::State;
use crossbeam_channel::TrySendError;
use crossbeam_channel::{select, tick, Receiver};
use deck_model::{
    AgentId, Answer, ApiError, ApiResult, CardId, CardKind, Command, Control, Deliver, DeviceLine,
    DeviceOp, DeviceReport, Effect, Event, EventKind, FailureLine, HoldId, Key, LinkEvent,
    Principal, Request, Scope, SessionId, Shown, Slot, Store, StoreConfig, TimerKey, Ts, VoiceIn,
};
use guard::{Guard, Refusal};
use holds::Holds;
use sessions::{Mail, MailKind, Sessions, WaitReply, Waiter};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const TICK: Duration = Duration::from_millis(250);
/// The main thread polls the stop flag this often while the doors serve.
const STOP_POLL: Duration = Duration::from_millis(50);
/// Logical sessions are expired on the tick at most this often.
const EXPIRY_EVERY: Duration = Duration::from_secs(1);
/// The implicit heartbeat of every connected session's agents (§3.2).
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
/// `agent.wait{timeout_s}` when the caller names none (§4.6).
pub const WAIT_DEFAULT_S: u64 = 600;
pub const STATE_DEBOUNCE: Duration = Duration::from_millis(500);
/// `metrics.get` waits this long for a fresh report from the hid thread.
const METRIC_WAIT: Duration = Duration::from_millis(1500);
/// Design §4.9: `op.answer` answers "after delivery or 4 s". The store's own
/// `DeliveryTimeout` fails the card at `DELIVERY_BUDGET` and that failure is
/// what resolves the parked reply; this is the backstop for a card that is
/// neither delivered nor failed (it was replaced, expired or removed).
const ANSWER_BUDGET: Duration = deck_model::DELIVERY_BUDGET.saturating_add(Duration::from_secs(1));
/// Removals and controls refused during an overflow wait here for the
/// drain: the re-projection is `Put`s only and cannot replace them.
pub const DROPPED_CAPACITY: usize = 512;
/// What a caller refused for `LOGICAL_TOTAL_MAX` is told to wait.
pub const LOGICAL_FULL_RETRY_MS: u32 = 5_000;

/// `client.name` within `CLIENT_NAME_MAX` (§3.3): the name keys the
/// logical-session map and is echoed back, so it is bounded at the seam
/// rather than wherever it is stored.
fn check_client_name(name: &str) -> Result<(), ApiError> {
    if name.len() > sessions::CLIENT_NAME_MAX {
        return Err(ApiError::invalid(
            "client.name",
            &format!("at most {} bytes", sessions::CLIENT_NAME_MAX),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Service(#[from] crate::service::ServiceError),
    #[error("{0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("{0}")]
    State(#[from] crate::state::StateError),
    #[error("cannot start the tokio runtime: {0}")]
    Runtime(std::io::Error),
}

/// The agent an event is about: `data.agent` as an id, or the whole agent
/// object of `agent.added`.
pub(crate) fn event_agent(event: &Event) -> Option<AgentId> {
    match event.data.get("agent")? {
        serde_json::Value::String(id) => Some(AgentId(id.clone())),
        serde_json::Value::Object(agent) => Some(AgentId(agent.get("id")?.as_str()?.to_string())),
        _ => None,
    }
}

/// The app a hand-off raises for `agent`: the bundle identifier its hook
/// helper reported (`terminal.app`), and only for a hook-registered session
/// — a connected app's own `terminal` is informational (§4.6) and never
/// raised. Checked again here, because the value becomes an argument of
/// `/usr/bin/open`.
pub(crate) fn handoff_app(agent: &deck_model::Agent) -> Option<String> {
    if agent.source != deck_model::Source::Hook {
        return None;
    }
    agent
        .session
        .terminal
        .as_ref()?
        .app
        .clone()
        .filter(|id| crate::hook::app::valid_bundle_id(id))
}

/// `data.card` as a card id (`card.cleared`, `card.snoozed`, …).
fn event_card(event: &Event) -> Option<CardId> {
    serde_json::from_value(event.data.get("card")?.clone()).ok()
}

/// `data.card.id` of a `card.set`.
fn event_card_id(event: &Event) -> Option<CardId> {
    serde_json::from_value(event.data.get("card")?.get("id")?.clone()).ok()
}

/// A nonzero session id, random per start (`std`'s per-process hasher seed).
pub fn new_sid() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    );
    let v = h.finish() as u32;
    if v == 0 {
        1
    } else {
        v
    }
}

pub fn wall_now() -> Ts {
    Ts::from_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    )
}

/// The actor's mutable parts, kept together so `apply`+execute is one step.
pub struct Core {
    /// Capture the launch path before Sparkle moves the old app aside.
    /// Re-querying the running image after replacement can report that
    /// temporary location and prevent the new app from restarting us.
    executable: Option<std::path::PathBuf>,
    pub store: Store,
    pub state: State,
    pub hid: HidHandle,
    /// The audio seam (B1). `AudioHandle::none()` until something installs
    /// an engine, which is exactly how this build ships: `Effect::Audio` is
    /// logged and forgotten, and the whole §7.4 machine still runs.
    pub audio: AudioHandle,
    /// `Stop`/`Finish` refused by a full audio queue, retried on the next
    /// tick — losing one strands a live capture with the mic open.
    audio_retry: VecDeque<deck_model::AudioCmd>,
    /// `Say`/`Chime`/`Gain` a full queue refused, for `metrics.get`.
    audio_dropped: u64,
    /// The device press and Claude talk target, retained through finalization.
    device_channel_tip_target: Option<(u16, AgentId)>,
    /// One bounded preview retained while the device pulls its chunks.
    device_image: Option<images::Transfer>,
    /// The `[voice]` device selection last pushed (B14), so a reload that
    /// changed something else does not re-select a device mid-session.
    audio_devices: Option<deck_model::AudioCmd>,
    /// The `[voice]` timing policy last pushed (V1/V2's `mic_warm`,
    /// `speak_over_sco`, `slow_sco_ms`), for the same reason.
    audio_policy: Option<deck_model::AudioCmd>,
    /// `[voice.tts]` as the cloud synthesiser reads it at every utterance
    /// (`audio::cloud`): refreshed by `set_config`, handed to
    /// `audio::spawn`, so a key pasted into the config speaks at the next
    /// announcement and never needs a restart.
    pub tts_settings: Arc<arc_swap::ArcSwap<crate::audio::cloud::Settings>>,
    /// Where the cloud voice's key lives and what the store holds
    /// (`crate::secrets`): `set_config` puts a stored key into the config in
    /// force, so everything that reads the key from there reads it as before.
    voice_key: crate::secrets::VoiceKey,
    /// The `deckd-keychain` thread, which makes every store write of this
    /// run. The core never calls the store itself, because a Keychain call
    /// can wait on a dialog.
    keeper: crate::secrets::Keeper<dispatch::KeyWork>,
    /// `[voice.stt]` as the local recogniser reads it at every press
    /// (`audio::whisper`), refreshed the same way.
    pub stt_settings: Arc<arc_swap::ArcSwap<crate::stt::Settings>>,
    /// The one model download at a time (`stt.install`, `stt.status`).
    pub stt_install: crate::download::Installer,
    pub firmware: crate::firmware::Updater,
    /// The local recogniser, when this build has one, for `stt.status`
    /// and for loading a model the moment it is on disk.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub whisper: Option<Arc<crate::audio::whisper::WhisperStt>>,
    /// `[voice.tts]` as the local voice reads it at every utterance
    /// (`audio::kokoro`), refreshed the same way as the others.
    pub kokoro_settings: Arc<arc_swap::ArcSwap<crate::kokoro::Settings>>,
    /// The voice package download (`tts.install`, `tts.status`).
    pub tts_install: crate::download::Installer,
    /// The local voice, when this build has one, for `tts.status` and for
    /// loading a package the moment it is on disk.
    #[cfg(target_os = "macos")]
    pub kokoro: Option<Arc<crate::audio::kokoro::KokoroTts>>,
    /// Recordings offered to `caps.transcribe` companions, shared with the
    /// audio engine (`audio::companion`).
    pub transcribe: Arc<crate::audio::companion::CompanionHub>,
    /// The desktop seam (§5.5): the hand-off and the Macropad keys.
    /// `DesktopHandle::none()` until something installs a worker, which is
    /// what `--no-device`, a non-macOS build and every unit test leave in
    /// place: a hand-off or a keystroke is logged and forgotten, and the
    /// inbox and Claude Channels still report through its outcome channel.
    pub desktop: DesktopHandle,
    /// The Accessibility grant as the desktop side last learned it
    /// (`perm.status.accessibility`): `None` until a Macropad key was
    /// pressed or `perm.request` asked.
    accessibility_granted: Option<bool>,
    /// The `(stt, tts)` the audio side reported once at start-up, which is
    /// what `voice.engines` answers from (M5). `None` in every build with
    /// no engine — `--no-device`, a non-macOS build, or the seconds before
    /// the posted read lands — and that is a `-32013`, never a guess.
    engines: Option<(
        crate::audio::traits::EngineInfo,
        crate::audio::traits::EngineInfo,
    )>,
    /// How `service.restart`/`service.stop` reach the supervisor on `main`.
    /// The default cell is nobody's: a core with no `set_exit` (every test,
    /// every `--demo` rig) answers the methods and nothing comes down.
    exit: Arc<crate::service::Exit>,
    paths: Paths,
    timers: BTreeMap<(Instant, u64), TimerKey>,
    timer_seq: u64,
    state_dirty: Option<Instant>,
    needs_resync: bool,
    /// Non-`Put` ops refused while `needs_resync`, replayed before the re-projection.
    dropped: VecDeque<DeviceOp>,
    /// The side queue overflowed too: every slot is reset on the hid side instead.
    dropped_overflow: bool,
    overflows: u32,
    ops_dropped: u64,
    /// Commands an executor produced for after the current step.
    followups: VecDeque<Command>,
    demo: Option<Demo>,
    metric: Metric,
    metric_at: Option<Instant>,
    started: Instant,
    started_wall: Ts,
    /// The doors' view: every event, and the snapshot cell.
    pub bus: Arc<Bus>,
    /// Events of the current iteration, not yet on the bus.
    pending: Vec<Event>,
    pub sessions: Sessions,
    pub holds: Holds,
    pub guard: Guard,
    config: Arc<Config>,
    /// The UDS peer verifier the doors share, so a hot `[trust]` reload
    /// reaches it (§3.7): every config change — the file watcher's and
    /// `config.reload`'s alike — goes through `set_config`.
    verifier: Option<Arc<dyn crate::trust::PeerVerifier>>,
    expired_at: Option<Instant>,
    /// Parked `agent.wait` deadlines → the waiter they belong to. A
    /// replaced waiter's timer is harmless: the sequence number no longer
    /// matches (the core's own wheel, next to the store's timers).
    waits: BTreeMap<(Instant, u64), (SessionId, AgentId)>,
    wait_seq: u64,
    /// The last implicit heartbeat and mailbox pruning.
    beat_at: Option<Instant>,
    /// `HoldId`s are handed out per held card (the store keeps the card).
    hold_seq: u64,
    /// `op.answer` replies parked until their delivery is known (§4.9).
    answers: Vec<AnswerWait>,
    /// The adapters' per-daemon state, shared with every `CoreHandle` the
    /// doors hold. The core never reads it; it owns it so that a
    /// `[hooks.<cli>] hold_s` reload lands on the same object the next
    /// held card is built from (B2).
    adapters: Arc<crate::adapters::rt::Adapters>,
    /// The Usage face's feeders' latest windows (`crate::usage`), and the
    /// line last pushed from them.
    usage: crate::usage::Feeder,
    /// Which source each session is, when each was last heard from, and the
    /// sessions the owner's `[sources]` switches mute (`care::sources`).
    sources: crate::care::sources::Ledger,
    /// What became of every card and how the deck's reader was used
    /// (`crate::deliveries`), read off every step's events and every deck
    /// line, and kept in `state.json`.
    deliveries: crate::deliveries::Ledger,
    /// The test agent (`care::test`): the daemon's own care session, the
    /// agent "Test" while a test is on the deck, and how the last test of
    /// each kind went; and the agent that asks before a firmware install,
    /// while its card waits.
    test_agent: crate::care::test::TestAgent,
    /// When the deck confirmed the install that waits for the voice to go
    /// quiet before it starts (`Core::start_confirmed_install`).
    install_pressed: Option<Instant>,
    /// `--no-device`: the deck is never opened here, nor looked for —
    /// `care.checks` leaves the doctor's HID enumeration out (O16).
    pub no_device: bool,
    /// `[api] tcp` was left out of the file when `run` started and Claude's
    /// settings still carried the pre-plugin hooks, so the TCP door was
    /// opened for them (`config::Api::tcp_door`). What the key left out
    /// resolves to here ([`Core::tcp_door`]); `false` in a core `run` did
    /// not start, and in one whose file named the key, where nothing looked.
    pub legacy_hooks: bool,
    /// Whether the collectors run (`crate::dormancy`). Off, so always
    /// collecting, until `run` starts it for a daemon that can have a deck.
    dormancy: crate::dormancy::Dormancy,
    /// The deck's reader set with its linger (`crate::reader`): the lazy
    /// feed is on exactly while dormancy is, so a core without it reads
    /// every session.
    readers: crate::reader::Readers,
    /// The service's own looks for a release (`Core::check_for_updates`).
    update_schedule: crate::firmware::Schedule,
}

/// A parked `op.answer` reply. `deck-model` answers the method before the
/// delivery executor has run, so the core holds the reply until the card's
/// outcome is on the events (`card.delivered` / `card.failed` / …) or the
/// budget runs out — which is what design §4.9 promises the caller.
struct AnswerWait {
    agent: AgentId,
    card: CardId,
    reply: tokio::sync::oneshot::Sender<ApiResult>,
    /// The store's own answer (`{rev, delivered, via}`), once it returned.
    store: Option<serde_json::Value>,
    /// `(delivered, reason)`, once the card's outcome was seen.
    outcome: Option<(bool, Option<String>)>,
    deadline: Instant,
}

impl Core {
    pub fn new(store: Store, state: State, hid: HidHandle, paths: Paths) -> Self {
        let bus = Arc::new(Bus::default());
        bus.set_snapshot(store.snapshot().clone());
        // A unit-test core never reaches out (O16). The API tests call every
        // advertised method with `{}`: `firmware.check` then opened whatever
        // deck was on USB and fetched the release manifest, and `stt.install`
        // and `tts.install` started the default models' downloads. `run`
        // takes the firmware side offline for `--no-device` as well.
        let (firmware, stt_install, tts_install) = if cfg!(test) {
            (
                crate::firmware::Updater::offline(),
                crate::download::Installer::offline(),
                crate::download::Installer::offline(),
            )
        } else {
            (
                crate::firmware::Updater::default(),
                crate::download::Installer::default(),
                crate::download::Installer::default(),
            )
        };
        // The record `state.json` kept goes on counting.
        let deliveries = crate::deliveries::Ledger::new(
            state.deliveries.clone(),
            &store.snapshot().service.version,
            wall_now(),
        );
        Core {
            store,
            state,
            hid,
            audio: AudioHandle::none(),
            audio_retry: VecDeque::new(),
            audio_dropped: 0,
            device_channel_tip_target: None,
            device_image: None,
            audio_devices: None,
            audio_policy: None,
            tts_settings: Arc::new(arc_swap::ArcSwap::from_pointee(crate::audio::cloud::Settings::default())),
            voice_key: crate::secrets::VoiceKey::file(),
            keeper: crate::secrets::Keeper::none(),
            stt_settings: Arc::new(arc_swap::ArcSwap::from_pointee(crate::stt::Settings::default())),
            stt_install,
            firmware,
            executable: std::env::current_exe().ok(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            whisper: None,
            kokoro_settings: Arc::new(arc_swap::ArcSwap::from_pointee(crate::kokoro::Settings::default())),
            tts_install,
            #[cfg(target_os = "macos")]
            kokoro: None,
            transcribe: crate::audio::companion::CompanionHub::new(),
            desktop: DesktopHandle::none(),
            accessibility_granted: None,
            engines: None,
            exit: Arc::default(),
            paths,
            timers: BTreeMap::new(),
            timer_seq: 0,
            state_dirty: None,
            needs_resync: false,
            dropped: VecDeque::new(),
            dropped_overflow: false,
            overflows: 0,
            ops_dropped: 0,
            followups: VecDeque::new(),
            demo: None,
            metric: Metric::default(),
            metric_at: None,
            started: Instant::now(),
            started_wall: wall_now(),
            bus,
            pending: Vec::new(),
            sessions: Sessions::new(),
            holds: Holds::new(),
            guard: Guard::new(),
            config: Arc::new(Config::default()),
            verifier: None,
            expired_at: None,
            waits: BTreeMap::new(),
            wait_seq: 0,
            beat_at: None,
            hold_seq: 0,
            answers: Vec::new(),
            adapters: Arc::default(),
            usage: crate::usage::Feeder::default(),
            sources: crate::care::sources::Ledger::default(),
            deliveries,
            test_agent: crate::care::test::TestAgent::default(),
            install_pressed: None,
            no_device: false,
            legacy_hooks: false,
            dormancy: crate::dormancy::Dormancy::off(),
            readers: crate::reader::Readers::default(),
            update_schedule: crate::firmware::Schedule::default(),
        }
    }

    /// The effective config (`service.info`, the logical-session TTL) and
    /// every hot key of §3.7 the core owns: `[trust]
    /// require_trusted_for_allow` here, and the store's own timings
    /// (`agents.grace_s`, `cards.ready_ttl_s`, `hooks.claude.hold_s`, the
    /// pins) through `Store::reconfigure`, which leaves the store's
    /// identity and `port` alone.
    ///
    /// `config` is the file's. On a store home the cloud voice's key is not
    /// in the file, so it is put in here ([`crate::secrets::VoiceKey::overlay`]),
    /// and the config in force is the one with the key: the synthesiser's
    /// cell, the voice pool, `voice.engines` and `config.get`'s hint read it
    /// from there as they always did.
    pub fn set_config(&mut self, config: Arc<Config>) {
        let config = self.voice_key.overlay(config);
        self.guard.require_trusted = config.trust.require_trusted_for_allow;
        // The rest of `[trust]` lives in the verifier, which the doors hold
        // behind an `Arc` — it re-reads its requirements rather than being
        // rebuilt, so `trust.gui_requirement`/`dev_requirement` really are
        // hot and `config.reload` does not report a lie.
        if let Some(verifier) = &self.verifier {
            verifier.reconfigure(
                &config.trust.gui_requirement,
                config.trust.dev_requirement.as_deref(),
            );
        }
        let mut cfg = self.store.config().clone();
        cfg.grace_s = config.agents.grace_s;
        cfg.ready_ttl_s = config.cards.ready_ttl_s;
        cfg.hold_s = config.hooks.claude.hold_s;
        // The store has one budget and there are three CLIs, so each
        // adapter carries its own on the card it builds (B2); the store's
        // value stays the fallback for anything that names none.
        // `[hooks.gemini]` has no `hold_s` of its own yet (the adapter is
        // v1.1, §5.3), so it follows Claude's until it does.
        self.adapters.set_hold_s(
            config.hooks.claude.hold_s,
            config.hooks.codex.hold_s,
            config.hooks.claude.hold_s,
        );
        cfg.pins = config.pins();
        // B14: `[voice]` is hot too, and there are exactly two things in it
        // the daemon has to *do* something about rather than simply read
        // where it is used.
        //
        // The locale goes onto the store config, because `AudioCmd::Listen`
        // carries it and a recogniser is built per locale (B3) — so a
        // reload takes effect from the next press on, and `voice.get` says
        // so at once.
        cfg.locale.clone_from(&config.voice.stt.locale);
        // §7.7's override for an operator who wants a question read out
        // even while they are looking at the card: read at the next card,
        // so a reload takes effect from then on.
        cfg.announce_questions = config.voice.announce_questions;
        cfg.voice_pool = config.voice_pool();
        cfg.announce_max_chars = config.voice.tts.announce_max_chars;
        cfg.companion_stt = config.voice.stt.companion;
        self.store.reconfigure(&cfg);
        // The cloud synthesiser's keys are read live from this cell, which
        // is why none of them is a restart key.
        let tts = crate::audio::cloud::Settings::from_config(&config);
        if **self.tts_settings.load() != tts {
            tracing::info!(
                engine = config.voice.tts.effective_engine(),
                key = %config.voice.tts.api_key_hint(),
                "[voice.tts] applied"
            );
            self.tts_settings.store(Arc::new(tts));
        }
        let stt = crate::stt::Settings::from_config(&config);
        if **self.stt_settings.load() != stt {
            tracing::info!(engine = %config.voice.stt.engine, model = %stt.model, "[voice.stt] applied");
            self.stt_settings.store(Arc::new(stt));
        }
        let voice = crate::kokoro::Settings::from_config(&config);
        if **self.kokoro_settings.load() != voice {
            tracing::info!(engine = %config.voice.tts.engine, model = %voice.model, "[voice.tts] local voice applied");
            self.kokoro_settings.store(Arc::new(voice));
        }
        // A model that is wanted and on disk is loaded now, not by the
        // next press or utterance.
        #[cfg(target_os = "linux")]
        if let Some(whisper) = &self.whisper {
            whisper.ensure_loaded();
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(whisper) = &self.whisper {
                whisper.ensure_loaded();
            }
            if let Some(kokoro) = &self.kokoro {
                kokoro.ensure_loaded();
            }
        }
        // The device selection has to be pushed, because §7 selects by UID
        // and never touches the system default: nothing on the audio side
        // can re-derive it. Only on a change — this runs on every reload,
        // and re-selecting a device mid-session would close the stream the
        // operator is listening to. An unset `device_uid` is `None` here,
        // and the engine keeps the deck it discovered at start for that.
        let devices = deck_model::AudioCmd::Devices {
            device_uid: config.voice.device_uid.clone(),
            fallback_output: config.voice.fallback_output.clone(),
        };
        if self.audio_devices.as_ref() != Some(&devices) {
            self.audio_devices = Some(devices.clone());
            self.send_audio(devices);
        }
        // …and the timing policy beside it (V1/V2). Pushed rather than
        // read on the audio side, because these are operator settings and
        // nothing over there can re-derive them; kept apart from the
        // selection so a changed threshold never closes an open stream.
        let policy = deck_model::AudioCmd::Policy {
            mic_warm: config.voice.mic_warm.clone(),
            speak_over_sco: config.voice.speak_over_sco,
            slow_sco_ms: config.voice.slow_sco_ms,
        };
        if self.audio_policy.as_ref() != Some(&policy) {
            self.audio_policy = Some(policy.clone());
            self.send_audio(policy);
        }
        self.config = config;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Where the cloud voice's key lives (`crate::secrets::home_for`), its
    /// store already read once off this thread. A store home gets its
    /// `deckd-keychain` thread here. Set before the first `set_config`.
    pub fn set_voice_key(&mut self, key: crate::secrets::VoiceKey) {
        self.keeper = match key.store() {
            Some(store) => crate::secrets::Keeper::spawn(store),
            None => crate::secrets::Keeper::none(),
        };
        self.voice_key = key;
    }

    /// The verifier the UDS door consults, handed to the core so every
    /// `[trust]` reload reaches it (§3.7). Set before `set_config`.
    pub fn set_verifier(&mut self, verifier: Arc<dyn crate::trust::PeerVerifier>) {
        verifier.reconfigure(
            &self.config.trust.gui_requirement,
            self.config.trust.dev_requirement.as_deref(),
        );
        self.verifier = Some(verifier);
    }

    /// Spawn the `deckd-core` thread around this core. The thread returns
    /// the core after `CoreMsg::Shutdown` (holds released, status written)
    /// so the caller can join the hid thread.
    pub fn spawn(self) -> (CoreHandle, std::thread::JoinHandle<Core>) {
        let (tx, rx) = crossbeam_channel::bounded(handle::MSGS_CAPACITY);
        let handle = CoreHandle::with_adapters(tx, self.bus.clone(), self.adapters.clone());
        let join = std::thread::Builder::new()
            .name("deckd-core".into())
            .spawn(move || run_core(self, rx))
            .expect("spawning the core thread");
        (handle, join)
    }

    /// The agents `session` owns (`CallCtx::own`).
    pub fn owned_by(&self, session: SessionId) -> BTreeSet<AgentId> {
        self.store
            .snapshot()
            .agents
            .values()
            .filter(|a| a.owner.session == session)
            .map(|a| a.id.clone())
            .collect()
    }

    /// The owning session of the agent an event is about, if any.
    fn owner_of(&self, event: &Event) -> Option<SessionId> {
        let data = event.data.as_object()?;
        match data.get("agent")? {
            serde_json::Value::String(id) => self
                .store
                .snapshot()
                .agents
                .get(&AgentId(id.clone()))
                .map(|a| a.owner.session),
            serde_json::Value::Object(agent) => agent
                .get("owner")
                .and_then(|o| o.get("session"))
                .and_then(|s| serde_json::from_value::<SessionId>(s.clone()).ok())
                .or_else(|| {
                    let id = agent.get("id")?.as_str()?;
                    self.store
                        .snapshot()
                        .agents
                        .get(&AgentId(id.to_string()))
                        .map(|a| a.owner.session)
                }),
            _ => None,
        }
    }

    /// Every event since the last call goes to the bus (owner-only kinds
    /// excepted: `agent.answer`/`agent.order` reach their session directly)
    /// and the snapshot cell is swapped when `rev` moved.
    pub fn publish(&mut self) {
        if !self.pending.is_empty() {
            let events = std::mem::take(&mut self.pending);
            for event in events {
                if matches!(event.kind, EventKind::AgentAnswer | EventKind::AgentOrder) {
                    continue;
                }
                let owner = self.owner_of(&event);
                self.bus.publish(Published {
                    event: Arc::new(event),
                    owner,
                });
            }
        }
        if self.bus.rev() != self.store.snapshot().rev {
            self.bus.set_snapshot(self.store.snapshot().clone());
        }
    }

    /// An event the core itself raises, with no reducer behind it and so no
    /// `rev` bump: `config.changed` and the `error{code:"config_invalid"}`
    /// of a reload (§3.7). It reaches the ring and the fan-out like any
    /// other event.
    pub fn publish_event(
        &self,
        kind: EventKind,
        principal: Principal,
        data: serde_json::Value,
        now: Instant,
    ) {
        self.bus.publish(Published {
            event: Arc::new(Event {
                rev: self.store.snapshot().rev,
                ts: self.store.ts(now),
                kind,
                principal,
                data,
            }),
            owner: None,
        });
    }

    /// The bus the doors read; replaceable before the core starts (the
    /// ring's length is `[privacy] event_ring`, and the tests want a small
    /// one). The current snapshot is carried over.
    pub fn set_bus(&mut self, bus: Arc<Bus>) {
        bus.set_snapshot(self.store.snapshot().clone());
        self.bus = bus;
    }

    /// One reducer request from a door, exactly as the demo client runs
    /// its own: the guard (rate limits, then guarded operations), the
    /// `push`-caps rule of a logical session, `store.call`, effects,
    /// follow-ups, the result.
    pub fn api_call(
        &mut self,
        principal: Principal,
        session: SessionId,
        trusted: bool,
        req: Request,
        now: Instant,
    ) -> ApiResult {
        self.sessions.touch(session, now);
        // Every source's agents call in through here (the hooks' plans, the
        // Codex client, a connected app): this is when each was last heard
        // from (`sources.list`), by something it did, never by the lease
        // renewals and reclaims that only keep its agents alive.
        if crate::care::sources::activity(&req) {
            self.sources.heard(&self.sessions, session, self.store.ts(now));
        }
        if matches!(req, Request::DeviceWsSet { .. }) {
            self.sources.drew_face(&self.sessions, session);
        }
        self.logical_push_check(session, &req)?;
        if let Err(refusal) =
            self.guard
                .check(&principal, trusted, &req, self.store.snapshot(), now)
        {
            return Err(self.refuse(principal, refusal, now));
        }
        let claiming = matches!(req, Request::AgentClaim { .. });
        let (result, mut events, effects) = self.store.call(principal, session, req, now);
        self.execute_all(effects, &events, now);
        self.drain_followups(&mut events, now);
        // §12: an `agent.claim` takes the old owner's undelivered answers
        // over with the agent, so the reconnecting client's next
        // `agent.wait` returns them instead of a bare timeout.
        if claiming {
            if let Some(agent) = result
                .as_ref()
                .ok()
                .and_then(|v| serde_json::from_value::<AgentId>(v["agent"].clone()).ok())
            {
                self.adopt_orphans(session, &agent);
            }
        }
        result
    }

    /// The owner's `[sources]` switches, applied: every session of a paused
    /// source is muted (`Command::Mute`), kept and served but off the deck
    /// (`care::sources`). Worked out again on every config change and every
    /// session open or close, so a paused app's new connection is muted
    /// before it can register anything; a set that did not move sends
    /// nothing.
    pub(crate) fn apply_pauses(&mut self, now: Instant) {
        let muted = self
            .sources
            .mute(&self.sessions, self.store.snapshot(), &self.config.sources);
        if let Some(sessions) = muted {
            tracing::debug!(sessions = sessions.len(), "muted sessions");
            self.step(Command::Mute { sessions }, now);
        }
    }

    /// `sources.list` (`care::sources`): what puts sessions on the deck, as
    /// counts and times.
    pub(crate) fn sources_list(&mut self) -> serde_json::Value {
        let around = crate::care::sources::Around {
            channels: &self.adapters.claude_channels,
            channels_on: self.channels_on(),
            codex_connected: self.adapters.codex_connected(),
        };
        self.sources
            .list(&self.sessions, self.store.snapshot(), &self.config, &around)
    }

    /// Whether the owner has Claude Channels on (`[sources]
    /// claude_channels`, resolved against this daemon's plugin when the
    /// file leaves it unset): what `config.get` publishes, the Claude
    /// Channels row's switch, and what keeps the channel door open.
    pub(crate) fn channels_on(&self) -> bool {
        crate::integrations::claude::channels_on(
            self.config.sources.claude_channels,
            &self.paths.data,
        )
    }

    /// The TCP door as this daemon resolves `[api] tcp`: the file's value,
    /// or — left out — open only for the pre-plugin Claude hooks `run`
    /// found at start ([`Core::legacy_hooks`]). Its `on` is what `config.get`
    /// publishes, the Local API row's switch; `care.checks` reads its rule.
    pub(crate) fn tcp_door(&self) -> crate::config::TcpDoor {
        self.config.api.tcp_door(|| self.legacy_hooks)
    }

    /// What a care method needs from here (`crate::care`): copies, so the
    /// door can walk the disk on the blocking pool while this thread goes
    /// on.
    pub(crate) fn care_context(&self) -> crate::care::Context {
        crate::care::Context {
            paths: self.paths.clone(),
            executable: self.executable.clone(),
            config: self.config.clone(),
            no_device: self.no_device,
            tcp: self.tcp_door(),
            codex_connected: self.adapters.codex_connected(),
            firmware_busy: self.firmware.status().busy(),
            keychain: self.voice_key.present(),
        }
    }

    /// A guard refusal: the error for the caller, and for an untrusted
    /// guarded operation the `op.confirm_requested` event as well (§4.2).
    fn refuse(&mut self, principal: Principal, refusal: Refusal, now: Instant) -> ApiError {
        let error = refusal.error();
        if let Refusal::Untrusted { agent, card, order } = refusal {
            tracing::debug!(%principal, agent = %agent.0, "guarded operation: confirmation requested");
            self.step(
                Command::ConfirmRequested {
                    principal,
                    agent,
                    card,
                    order,
                },
                now,
            );
        }
        error
    }

    /// `push` caps need a WS/UDS session (§4.2): a logical HTTP session
    /// asking for them is refused, it must poll (§9: keyed on `is_logical`).
    fn logical_push_check(&self, session: SessionId, req: &Request) -> Result<(), ApiError> {
        let Request::AgentRegister(r) = req else {
            return Ok(());
        };
        if !self
            .sessions
            .get(session)
            .is_some_and(sessions::SessionEntry::is_logical)
        {
            return Ok(());
        }
        let why = "push needs a WS/UDS session";
        if r.caps.answer == deck_model::AnswerCap::Push {
            return Err(ApiError::invalid("caps.answer", why));
        }
        if r.caps.order == deck_model::OrderCap::Push {
            return Err(ApiError::invalid("caps.order", why));
        }
        Ok(())
    }

    /// The store answered `op.answer`: give the parked reply its half and
    /// fire it if the delivery is already known. An error (a guard refusal,
    /// an unknown card) is the whole answer — nothing is ever delivered.
    fn settle_answer(&mut self, agent: &AgentId, card: CardId, result: ApiResult) {
        let Some(i) = self
            .answers
            .iter()
            .rposition(|w| w.store.is_none() && w.agent == *agent && w.card == card)
        else {
            return;
        };
        match result {
            Err(e) => {
                let _ = self.answers.remove(i).reply.send(Err(e));
            }
            Ok(value) => {
                // A group step is delivered into the accumulator on arrival,
                // and `focus`/`none` are done by the time the call returns.
                if value["delivered"] == serde_json::Value::Bool(true) {
                    self.answers[i].outcome = Some((true, None));
                }
                self.answers[i].store = Some(value);
                self.flush_answers();
            }
        }
    }

    /// The card's outcome for every parked `op.answer` that named it.
    fn note_answer_outcomes(&mut self, events: &[Event]) {
        if self.answers.is_empty() {
            return;
        }
        for event in events {
            let data = &event.data;
            let Some(agent) = data.get("agent").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let card = data.get("card").and_then(serde_json::Value::as_u64);
            let outcome = match event.kind {
                EventKind::CardDelivered => (true, None),
                EventKind::CardFailed => (
                    false,
                    Some(reason_of(data).unwrap_or_else(|| "undeliverable".into())),
                ),
                EventKind::CardCleared => (
                    data.get("delivered")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                    reason_of(data),
                ),
                EventKind::CardExpired => (false, Some("expired".into())),
                EventKind::CardSnoozed => (false, Some("snoozed".into())),
                EventKind::AgentRemoved => (false, Some("agent_gone".into())),
                _ => continue,
            };
            for wait in &mut self.answers {
                if wait.outcome.is_some() || wait.agent.0 != agent {
                    continue;
                }
                // `agent.removed` carries no card: it ends every wait on it.
                if card.is_some_and(|c| c != u64::from(wait.card.0)) {
                    continue;
                }
                wait.outcome = Some(outcome.clone());
            }
        }
        self.flush_answers();
    }

    /// The delivery record (`crate::deliveries`) reads every step's events.
    /// A card's source is its agent's session's, asked of `care::sources`
    /// as the card is set; a card that ended is copied into `state.json`.
    fn note_deliveries(&mut self, events: &[Event], now: Instant) {
        if events.is_empty() {
            return;
        }
        let (snapshot, sources, sessions) = (self.store.snapshot(), &self.sources, &self.sessions);
        let from = |agent: &AgentId| {
            snapshot
                .agents
                .get(agent)
                .map_or("unknown", |a| sources.word(sessions, a.owner.session))
        };
        if self.deliveries.observe(events, snapshot, from) {
            self.persist_deliveries(now);
        }
    }

    /// The delivery record into `state.json`, on the state file's own
    /// debounce.
    fn persist_deliveries(&mut self, now: Instant) {
        self.state.deliveries = self.deliveries.persisted(now);
        self.state_dirty.get_or_insert(now);
    }

    /// What the settle window did since the last look, into the delivery
    /// record.
    fn take_asks(&mut self) {
        let asks = self.adapters.asks.take();
        self.deliveries.add_asks(asks);
    }

    /// Send every parked `op.answer` whose two halves are in.
    fn flush_answers(&mut self) {
        let mut i = 0;
        while i < self.answers.len() {
            if self.answers[i].store.is_none() || self.answers[i].outcome.is_none() {
                i += 1;
                continue;
            }
            let wait = self.answers.remove(i);
            let (delivered, reason) = wait.outcome.expect("checked above");
            let mut value = wait.store.expect("checked above");
            value["delivered"] = delivered.into();
            match reason {
                Some(reason) if !delivered => value["reason"] = reason.into(),
                _ => {}
            }
            tracing::debug!(
                agent = %wait.agent.0, card = wait.card.0, delivered,
                "op.answer answered after the delivery"
            );
            let _ = wait.reply.send(Ok(value));
        }
    }

    /// The §4.9 budget: a card that was neither delivered nor failed (it was
    /// replaced or its agent went away without an event this waiter saw).
    fn expire_answers(&mut self, now: Instant) {
        for wait in &mut self.answers {
            if wait.deadline <= now && wait.outcome.is_none() {
                tracing::debug!(agent = %wait.agent.0, card = wait.card.0, "op.answer: delivery budget");
                wait.outcome = Some((false, Some("timeout".into())));
            }
        }
        self.flush_answers();
        // A parked reply whose call never returned cannot exist, but a
        // dropped sender is the door's `-32603`, never a hang.
        self.answers
            .retain(|w| w.store.is_some() || w.deadline + ANSWER_BUDGET > now);
    }

    /// One message from a door.
    pub fn on_msg(&mut self, msg: CoreMsg, now: Instant) {
        match msg {
            CoreMsg::SystemStatus(status) => { self.step(Command::SystemStatus(status), now); },
            CoreMsg::SessionControls { agent, session, settled, controls } => {
                self.step(Command::SessionControls { agent, session, settled, controls }, now);
            }
            CoreMsg::Api {
                principal,
                session,
                trusted,
                req,
                reply,
            } => {
                let req = *req;
                // `op.answer` answers after delivery or 4 s (§4.9): its reply
                // is parked *before* the call, because the executor often
                // finishes inside it and its events must find the waiter.
                let Some((agent, card)) = answer_target(&req) else {
                    let result = self.api_call(principal, session, trusted, req, now);
                    let _ = reply.send(result);
                    return;
                };
                self.answers.push(AnswerWait {
                    agent: agent.clone(),
                    card,
                    reply,
                    store: None,
                    outcome: None,
                    deadline: now + ANSWER_BUDGET,
                });
                let result = self.api_call(principal, session, trusted, req, now);
                self.settle_answer(&agent, card, result);
            }
            CoreMsg::Call {
                ctx,
                method,
                params,
                reply,
            } => {
                // `agent.wait` may park: it owns the reply channel itself.
                if method == "agent.wait" {
                    self.wait_call(ctx, params, WaitReply::Call(reply), now);
                    return;
                }
                // So does a `config.set` that names the cloud voice's key on
                // a store home: it is answered when the keychain thread has
                // written the key (`dispatch::KeyWork`).
                if method == "config.set"
                    && self.voice_key.is_store()
                    && dispatch::names_voice_key(&params)
                {
                    self.config_set_keyed(ctx, &params, reply);
                    return;
                }
                let answer = self.call(ctx, &method, params, now);
                let _ = reply.send(answer);
            }
            CoreMsg::Wait {
                session,
                agent,
                timeout,
                reply,
            } => self.wait(session, agent, timeout, WaitReply::Result(reply), now),
            CoreMsg::SetCardHeld {
                principal,
                session,
                cmd,
                reply,
            } => {
                let _ = reply.send(self.set_card_held(principal, session, cmd, now));
            }
            CoreMsg::HoldWritten { agent, card } => {
                self.step(Command::HoldWritten { agent, card }, now);
            }
            CoreMsg::HoldGone { agent, card, token } => {
                // Only the hold this guard actually parked (B6): after a
                // same-id re-push, or after the store already released it,
                // the report is stale and the card belongs to somebody else.
                if self.holds.forget(&agent, card, token) {
                    self.step(Command::HoldGone { agent, card }, now);
                } else {
                    tracing::debug!(agent = %agent.0, card = card.0, ?token, "a stale hold_gone");
                }
            }
            CoreMsg::Delivered { agent, card, ok } => {
                self.step(Command::Delivered { agent, card, ok }, now);
            }
            CoreMsg::OpenSession { open, reply } => {
                let opened = check_client_name(&open.client.name).map(|()| {
                    let id = self.sessions.open(open, now);
                    tracing::debug!(session = %id, sessions = self.sessions.len(), "session opened");
                    id
                });
                // A session of a paused source is muted before its owner
                // even has its id, so nothing it registers takes a slot.
                if opened.is_ok() {
                    self.apply_pauses(now);
                }
                let _ = reply.send(opened);
            }
            CoreMsg::LogicalSession {
                token,
                client,
                principal,
                scopes,
                reply,
            } => {
                // Every HTTP call names its logical session; only a new
                // one can change what is muted.
                let fresh = !self.sessions.has_logical(&token, &client);
                let id = self.logical_session(token, client, principal, scopes, now);
                if fresh && id.is_ok() {
                    self.apply_pauses(now);
                }
                let _ = reply.send(id);
            }
            CoreMsg::CloseSession(id) => self.close_session(id, now),
            CoreMsg::Usage { provider, windows, account, partial, resets } => {
                if partial { self.usage.set_partial(provider, windows, account, crate::usage::unix_now()); }
                else { self.usage.set(provider, windows, account); }
                if let Some(resets) = resets { self.usage.set_resets(provider, resets); }
                self.push_usage(now);
            }
            CoreMsg::Config(config) => {
                tracing::info!(
                    require_trusted_for_allow = config.trust.require_trusted_for_allow,
                    mailbox_ttl_s = config.api.mailbox_ttl_s,
                    "config applied"
                );
                self.take_config(config, now);
            }
            CoreMsg::CareContext(reply) => {
                let _ = reply.send(self.care_context());
            }
            CoreMsg::ChatReplace {
                session,
                agent,
                messages,
                reply,
            } => {
                let _ = reply.send(self.chat_replace(session, agent, messages, now));
            }
            CoreMsg::Shutdown => {}
        }
    }

    /// A session's history read again for the lazy reader feed
    /// (`crate::reader`), in place of the one kept: only from the session
    /// that owns the agent, the rule every write of an agent's goes by. Not
    /// the source doing anything, so its `last_event_at` is not stamped.
    fn chat_replace(
        &mut self,
        session: SessionId,
        agent: AgentId,
        messages: Vec<(deck_model::MessageMeta, String)>,
        now: Instant,
    ) -> ApiResult {
        let owner = self
            .store
            .snapshot()
            .agents
            .get(&agent)
            .map(|a| a.owner.clone())
            .ok_or_else(|| ApiError::not_found("agent"))?;
        if owner.session != session {
            return Err(ApiError::conflict(&owner));
        }
        tracing::debug!(agent = %agent.0, messages = messages.len(), "reader: a session's history read again");
        self.step(Command::ChatReplace { agent, messages }, now);
        Ok(serde_json::json!({"rev": self.store.snapshot().rev}))
    }

    /// A config taken in, whichever of the three ways it came: the
    /// watcher's `CoreMsg::Config`, `config.reload` or `config.set`. It is
    /// set ([`Core::set_config`]), the owner's pauses are worked out again
    /// (a `[sources]` switch may have moved) and the Usage face is redrawn
    /// (its `[usage] providers` may have). Taking the config the core
    /// already holds changes nothing, which is what the watcher's reload of
    /// a `config.set`'s own write comes to. On a store home a cloud voice
    /// key found in the file is sent to the store on the keychain thread;
    /// until it has moved, the file's key is the one in force.
    fn take_config(&mut self, config: Arc<Config>, now: Instant) {
        self.move_voice_key(&config);
        self.set_config(config);
        self.apply_pauses(now);
        self.push_usage(now);
    }

    /// The Usage face from the feeders' latest windows (`crate::usage`),
    /// pushed as the provider that reported last, and only when the
    /// rendered line moved: the store re-sends an unchanged line itself
    /// to end the deck's "updating…", so a duplicate here would be a wire
    /// line for nothing.
    fn push_usage(&mut self, now: Instant) {
        let Some(provider) = self.usage.latest() else {
            return;
        };
        let config = Arc::clone(&self.config);
        let Some(line) = self.usage.render(
            &config.usage,
            &config.device.usage_period,
            crate::usage::unix_now(),
        ) else {
            return;
        };
        if self.usage.already_pushed(&line) {
            return;
        }
        let req = Request::DeviceUsageSet {
            period: line.0.clone(),
            items: line.1.clone(),
        };
        match self.api_call(
            Principal::Adapter(provider.adapter()),
            SessionId(0),
            false,
            req,
            now,
        ) {
            Ok(_) => self.usage.note_pushed(line, now),
            Err(e) => tracing::warn!(code = e.code, why = %e.message, "usage: device.usage.set refused"),
        }
    }

    /// `CoreMsg::LogicalSession` with the caps of §3.3 applied: a bounded
    /// `client.name`, at most `LOGICAL_PER_TOKEN_MAX` logical sessions per
    /// bearer token (the token's least-recently-used one is closed to make
    /// room) and `LOGICAL_TOTAL_MAX` across tokens (over it a *new* one is
    /// refused, so no token can evict another's). An existing
    /// `(token, client)` is only refreshed and never hits a cap.
    fn logical_session(
        &mut self,
        token: String,
        client: String,
        principal: Principal,
        scopes: Vec<Scope>,
        now: Instant,
    ) -> Result<SessionId, ApiError> {
        check_client_name(&client)?;
        if !self.sessions.has_logical(&token, &client) {
            while self.sessions.logical_of_token(&token) >= sessions::LOGICAL_PER_TOKEN_MAX {
                let Some(lru) = self.sessions.logical_lru_of_token(&token) else {
                    break;
                };
                tracing::info!(
                    session = %lru,
                    max = sessions::LOGICAL_PER_TOKEN_MAX,
                    "logical sessions per token: evicting the least recently used"
                );
                self.close_session(lru, now);
            }
            if self.sessions.logical_len() >= sessions::LOGICAL_TOTAL_MAX {
                tracing::warn!(
                    max = sessions::LOGICAL_TOTAL_MAX,
                    "logical sessions: the daemon is full"
                );
                return Err(ApiError::rate_limited(LOGICAL_FULL_RETRY_MS));
            }
        }
        Ok(self.sessions.logical(token, client, principal, scopes, now))
    }

    fn close_session(&mut self, id: SessionId, now: Instant) {
        // The agents it leaves behind keep its source, and with it a pause.
        self.sources.remember(&self.sessions, id);
        if let Some(mut entry) = self.sessions.close(id) {
            // Whoever was parked on it hears `timeout`, not a dropped channel.
            for (agent, waiter) in entry.waiters.drain() {
                self.waits.remove(&(waiter.deadline, waiter.seq));
                waiter.reply.resolve(WaitResult::Timeout);
                tracing::debug!(session = %id, agent = %agent.0, "waiter released: session closed");
            }
            // §12: the answers it never picked up outlive it for
            // `mailbox_ttl_s`, so the session that claims its agents back
            // still gets them (`agent.claim` + `agent.wait`, below).
            let kept = self.sessions.orphan(entry.mailbox.drain(..));
            if kept > 0 {
                tracing::debug!(session = %id, mail = kept, "mail orphaned: session closed");
            }
            tracing::debug!(session = %id, sessions = self.sessions.len(), "session closed");
            // A companion that was transcribing a press is not going to
            // answer: the local final is used at once, not at the budget.
            self.transcribe.fail_session(id);
            self.step(Command::SessionClosed(id), now);
            self.apply_pauses(now);
        }
    }

    /// Mail left behind by a closed session moves to the session that owns
    /// the agent now (§12): on `agent.claim`, and on the new owner's first
    /// `agent.wait`. Returns how much moved.
    fn adopt_orphans(&mut self, session: SessionId, agent: &AgentId) -> usize {
        if !self.sessions.contains(session) {
            return 0;
        }
        let mail = self.sessions.take_orphans(agent);
        if mail.is_empty() {
            return 0;
        }
        let moved = mail.len();
        if let Some(entry) = self.sessions.get_mut(session) {
            for m in mail {
                entry.push_mail(m);
            }
        }
        tracing::debug!(session = %session, agent = %agent.0, mail = moved, "mail adopted");
        moved
    }

    /// Logical sessions past `mailbox_ttl_s` are closed (checked once a second).
    fn expire_logical(&mut self, now: Instant) {
        if self
            .expired_at
            .is_some_and(|at| now.duration_since(at) < EXPIRY_EVERY)
        {
            return;
        }
        self.expired_at = Some(now);
        let ttl = Duration::from_secs(u64::from(self.config.api.mailbox_ttl_s));
        for id in self.sessions.expired_logical(now, ttl) {
            tracing::debug!(session = %id, "logical session expired");
            self.close_session(id, now);
        }
        // The rate-limit buckets are keyed by agent id and by principal:
        // over a long uptime the map would keep an entry per agent that
        // ever existed and per token name anyone ever presented.
        self.guard.limits.prune(now);
    }

    // ── Waiters, mailboxes and holds (contract §3.2, design §4.8) ──────

    /// `agent.wait` arriving as a `CoreMsg::Call`: its params, then `wait`.
    fn wait_call(
        &mut self,
        ctx: CallCtx,
        params: serde_json::Value,
        reply: WaitReply,
        now: Instant,
    ) {
        let agent = params
            .get("agent")
            .and_then(|v| serde_json::from_value::<AgentId>(v.clone()).ok());
        let Some(agent) = agent else {
            reply.refuse(ApiError::invalid_params("agent is required"));
            return;
        };
        let secs = params
            .get("timeout_s")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(WAIT_DEFAULT_S)
            .min(u64::from(deck_model::MAX_CLIENT_SECS));
        self.wait(ctx.session, agent, Duration::from_secs(secs), reply, now);
    }

    /// One `agent.wait` from the owning session: the mailbox first, else a
    /// waiter parked until the deadline. One waiter per `(session, agent)`:
    /// a second replaces the first, which resolves `timeout`.
    pub fn wait(
        &mut self,
        session: SessionId,
        agent: AgentId,
        timeout: Duration,
        reply: WaitReply,
        now: Instant,
    ) {
        if !self.sessions.contains(session) {
            reply.refuse(ApiError::not_found("session"));
            return;
        }
        self.sessions.touch(session, now);
        match self.store.snapshot().agents.get(&agent) {
            None => {
                reply.refuse(ApiError::not_found("agent"));
                return;
            }
            Some(a) if a.owner.session != session => {
                let owner = a.owner.clone();
                reply.refuse(ApiError::conflict(&owner));
                return;
            }
            // Waiting on an agent is a call on it: the lease is refreshed.
            Some(_) => self.refresh_lease(session, &agent, now),
        }
        // A reconnected owner picks up what the old session never took.
        self.adopt_orphans(session, &agent);
        if let Some(mail) = self
            .sessions
            .get_mut(session)
            .and_then(|e| e.pop_mail(&agent))
        {
            tracing::debug!(session = %session, agent = %agent.0, "wait: mailbox");
            reply.resolve(mail.into_result());
            return;
        }
        self.wait_seq += 1;
        let seq = self.wait_seq;
        let deadline = now + timeout;
        let replaced = self.sessions.get_mut(session).and_then(|e| {
            e.park(
                agent.clone(),
                Waiter {
                    reply,
                    deadline,
                    seq,
                },
            )
        });
        if let Some(old) = replaced {
            self.waits.remove(&(old.deadline, old.seq));
            old.reply.resolve(WaitResult::Timeout);
        }
        self.waits.insert((deadline, seq), (session, agent));
    }

    /// Any owner call refreshes the lease; `agent.wait` is one (§4.6).
    fn refresh_lease(&mut self, session: SessionId, agent: &AgentId, now: Instant) {
        let Some(principal) = self.sessions.get(session).map(|e| e.principal.clone()) else {
            return;
        };
        let (_, events, effects) = self.store.call(
            principal,
            session,
            Request::AgentHeartbeat {
                agents: vec![agent.clone()],
            },
            now,
        );
        self.execute_all(effects, &events, now);
    }

    /// Parked waits whose deadline has passed resolve `timeout` (a stale
    /// timer whose waiter was replaced or taken is a no-op).
    fn expire_waits(&mut self, now: Instant) {
        let due: Vec<(Instant, u64)> = self
            .waits
            .range(..=(now, u64::MAX))
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            let Some((session, agent)) = self.waits.remove(&key) else {
                continue;
            };
            let waiter = self.sessions.get_mut(session).and_then(|e| {
                if e.waiters.get(&agent).is_some_and(|w| w.seq == key.1) {
                    e.take_waiter(&agent)
                } else {
                    None
                }
            });
            if let Some(waiter) = waiter {
                tracing::debug!(session = %session, agent = %agent.0, "wait: timeout");
                waiter.reply.resolve(WaitResult::Timeout);
            }
        }
    }

    /// The session's parked, still-listening waiter for `agent`, taken out
    /// of the wait table.
    fn take_live_waiter(&mut self, session: SessionId, agent: &AgentId) -> Option<Waiter> {
        let entry = self.sessions.get_mut(session)?;
        let waiter = entry.take_waiter(agent).filter(|w| !w.reply.is_closed())?;
        self.waits.remove(&(waiter.deadline, waiter.seq));
        Some(waiter)
    }

    /// Resolve the session's waiter for the agent with `mail`, or queue it
    /// in the mailbox. False when the session is gone.
    fn hand_to_waiter(&mut self, session: SessionId, mail: Mail) -> bool {
        let agent = mail.agent.clone();
        let Some(entry) = self.sessions.get_mut(session) else {
            return false;
        };
        // A waiter whose door dropped the request is no waiter at all.
        match entry.take_waiter(&agent).filter(|w| !w.reply.is_closed()) {
            Some(waiter) => {
                self.waits.remove(&(waiter.deadline, waiter.seq));
                waiter.reply.resolve(mail.into_result());
            }
            None => entry.push_mail(mail),
        }
        true
    }

    /// `CoreMsg::SetCardHeld`: the store allocates the card id and puts the
    /// card on the device, the core parks the hook's response under
    /// `(agent, card)`.
    ///
    /// B4: `Store::hold` hands back the id and the refusal, so a hook
    /// helper sees `-32003 not_found{agent}` when its session ended and
    /// `-32009 invalid{text, empty}` when the adapter built a bad card —
    /// two facts that used to arrive as one `invalid{card, refused}`.
    ///
    /// B7: `[api] max_held_hooks` is checked **before** the store step, or
    /// the refusal would leave a card on the deck with no hold behind it.
    fn set_card_held(
        &mut self,
        principal: Principal,
        session: SessionId,
        cmd: deck_model::CardSet,
        now: Instant,
    ) -> Result<(CardId, holds::HoldToken, HoldRx), ApiError> {
        // A same-id re-push of a card that is already held replaces its
        // hold rather than adding one (`Holds::park`), so it is not what
        // the cap is about and is let through at the ceiling.
        let replacing = cmd
            .id
            .is_some_and(|card| self.holds.contains(&cmd.agent, card));
        let cap = self.config.api.max_held_hooks as usize;
        if !replacing && self.holds.len() >= cap {
            tracing::warn!(
                holds = self.holds.len(),
                cap,
                "the held-hook cap is full: this hook fails open"
            );
            return Err(ApiError::rate_limited(crate::core::handle::RETRY_MS));
        }
        let agent = cmd.agent.clone();
        // A held question is a hook event like any other, but it comes this
        // way and not through `api_call`.
        self.sources.heard(&self.sessions, session, self.store.ts(now));
        self.hold_seq += 1;
        let (result, mut events, effects) =
            self.store
                .hold(&principal, session, cmd, HoldId(self.hold_seq), now);
        self.execute_all(effects, &events, now);
        self.drain_followups(&mut events, now);
        let card = result?;
        let (token, rx) = self.holds.park(agent.clone(), card);
        tracing::debug!(agent = %agent.0, card = card.0, holds = self.holds.len(), "card held");
        Ok((card, token, rx))
    }

    /// The owning session of an agent.
    pub fn owner_session(&self, agent: &AgentId) -> Option<SessionId> {
        self.store
            .snapshot()
            .agents
            .get(agent)
            .map(|a| a.owner.session)
    }

    /// The kind of `(agent, card)` while the store still holds it.
    fn card_kind(&self, agent: &AgentId, card: CardId) -> CardKind {
        self.store
            .snapshot()
            .agents
            .get(agent)
            .and_then(|a| a.card.as_ref())
            .filter(|c| c.id == card)
            .map_or(CardKind::Q, |c| c.kind)
    }

    /// The delivery executor (§3.2, §4.8): `hold` resolves the parked hook,
    /// `push` writes to the owner's connection, `poll` resolves its waiter
    /// or waits in its mailbox, `focus`/`none` are done on arrival. `None`
    /// means somebody else reports `Delivered`.
    fn deliver(
        &mut self,
        agent: AgentId,
        card: CardId,
        answer: Answer,
        deliver: Deliver,
        now: Instant,
    ) {
        let outcome = match deliver {
            Deliver::None | Deliver::Focus => Some(true),
            Deliver::Hold => {
                if self.holds.answer(&agent, card, answer) {
                    // The **door** confirms it, once the hook response is
                    // written: the handler hands its `Delivery` back on
                    // `Reply::after_write` and the door reports
                    // `Delivered{ok}` after the flush (§4.9). A write that
                    // failed therefore fails the card `undeliverable`
                    // rather than clearing it as delivered.
                    None
                } else {
                    tracing::warn!(agent = %agent.0, card = card.0, "hold: nobody parked");
                    Some(false)
                }
            }
            Deliver::Push => self.deliver_push(&agent, card, answer, now),
            Deliver::Poll => self.deliver_poll(&agent, card, answer, now),
        };
        if let Some(ok) = outcome {
            self.followups
                .push_back(Command::Delivered { agent, card, ok });
        }
    }

    /// `push`: the owning session's connection gets the notification and
    /// the door reports the flush; the demo is its own owner; anything else
    /// (no owner, an observer, a logical session, a full or closed channel)
    /// cannot deliver.
    fn deliver_push(
        &mut self,
        agent: &AgentId,
        card: CardId,
        answer: Answer,
        now: Instant,
    ) -> Option<bool> {
        let session = self.owner_session(agent)?;
        let line = self.decision_line(agent, card, &answer, self.card_kind(agent, card), now);
        let delivery = Delivery {
            agent: agent.clone(),
            card,
        };
        match self.sessions.get(session).map(|e| &e.kind) {
            Some(SessionKind::Internal) => Some(true),
            Some(SessionKind::Live { tx }) => {
                match tx.try_send(Outbound::Deliver { line, delivery }) {
                    Ok(()) => None,
                    Err(e) => {
                        tracing::warn!(session = %session, agent = %agent.0, error = %e, "push: no room");
                        Some(false)
                    }
                }
            }
            _ => Some(false),
        }
    }

    /// `poll`: the owner's parked `agent.wait`, else its mailbox — and then
    /// nothing, so the store's 4 s budget fails the card until a later
    /// `agent.wait` picks the answer up (§4.8 "late pickup").
    fn deliver_poll(
        &mut self,
        agent: &AgentId,
        card: CardId,
        answer: Answer,
        now: Instant,
    ) -> Option<bool> {
        let session = self.owner_session(agent)?;
        let kind = self.decision_mail(card, answer, self.card_kind(agent, card));
        let mail = Mail {
            agent: agent.clone(),
            at: now,
            kind,
        };
        if matches!(
            self.sessions.get(session).map(|e| &e.kind),
            Some(SessionKind::Internal)
        ) {
            // An internal session polls only when something in the daemon
            // is parked in `agent.wait` for it — the Codex app-server client
            // (`crate::codexd`) drives its agents exactly like a third
            // party's script would. With a waiter the mail is handed over
            // and the waiter reports the delivery; without one there is no
            // door to report it and nobody who will ever read a mailbox,
            // so it counts as delivered on arrival, as before.
            return match self.take_live_waiter(session, agent) {
                Some(waiter) => {
                    waiter.reply.resolve(mail.into_result());
                    None
                }
                None => Some(true),
            };
        }
        if self.hand_to_waiter(session, mail) {
            None
        } else {
            Some(false)
        }
    }

    /// A decision for the owner: a Ready step is an order carrying the
    /// label the operator saw, anything else an answer (§4.15).
    fn decision_mail(&self, card: CardId, answer: Answer, kind: CardKind) -> MailKind {
        if kind == CardKind::R {
            MailKind::Order {
                text: answer
                    .label
                    .clone()
                    .or_else(|| answer.text.clone())
                    .unwrap_or_default(),
                ctx: "step".into(),
                choice: answer.choice,
                card: Some(card),
                // A step is the card's answer, which fails by the card's own
                // revert; it is no order of its own.
                order: None,
            }
        } else {
            MailKind::Answer { card, answer }
        }
    }

    /// The `agent.answer`/`agent.order` notification of a push delivery
    /// (§4.13), already serialized as one JSON-RPC line.
    fn decision_line(
        &self,
        agent: &AgentId,
        card: CardId,
        answer: &Answer,
        card_kind: CardKind,
        now: Instant,
    ) -> String {
        let ts = self.store.ts(now);
        let (kind, data) = if card_kind == CardKind::R {
            (
                EventKind::AgentOrder,
                serde_json::json!({
                    "agent": agent, "card": card, "ctx": "step",
                    "text": answer.label.clone().or_else(|| answer.text.clone()),
                    "choice": answer.choice, "source": answer.source,
                    "principal": answer.principal, "session": answer.session, "at": ts,
                    "utterance": answer.utterance,
                }),
            )
        } else {
            (
                EventKind::AgentAnswer,
                serde_json::json!({
                    "agent": agent, "card": card, "choice": answer.choice,
                    "label": answer.label, "text": answer.text, "source": answer.source,
                    "principal": answer.principal, "session": answer.session, "at": ts,
                    "utterance": answer.utterance,
                }),
            )
        };
        crate::api::rpc::event_line(&Event {
            rev: self.store.snapshot().rev,
            ts,
            kind,
            principal: answer.principal.clone(),
            data,
        })
    }

    /// What a step's events mean for the sessions: the owner-only
    /// `agent.order`, a snooze for a push/poll owner, and the mailbox
    /// pruning of §4.8 (the card cleared, replaced, expired, agent gone).
    fn route_events(&mut self, events: &[Event], now: Instant) {
        for event in events {
            match event.kind {
                EventKind::AgentOrder => self.route_order(event, now),
                EventKind::CardSnoozed => self.route_snooze(event, now),
                EventKind::CardCleared | EventKind::CardExpired => {
                    if let (Some(agent), Some(card)) = (event_agent(event), event_card(event)) {
                        self.prune(|m| sessions::mailbox::prune_card(m, &agent, card));
                    }
                }
                EventKind::CardSet => {
                    // The new card's own mail (a re-push) stays.
                    if let (Some(agent), Some(card)) = (event_agent(event), event_card_id(event)) {
                        self.prune(|m| sessions::mailbox::prune_other_cards(m, &agent, card));
                    }
                }
                EventKind::AgentRemoved => {
                    // An owner that releases its agent with orders unread
                    // leaves them undelivered: the deck says so.
                    if let Some(agent) = event_agent(event) {
                        let unread = self.take_mail(|m| sessions::mailbox::take_agent(m, &agent));
                        self.fail_dropped_orders(unread, "agent_gone");
                    }
                }
                _ => {}
            }
        }
    }

    /// Run a pruning over every session's mailbox and over the orphan
    /// mailbox (an agent has one owner, so at most one of them holds
    /// anything — but a closed owner's mail lives on in the orphans).
    fn prune(&mut self, mut f: impl FnMut(&mut VecDeque<sessions::Mail>) -> usize) {
        let mut dropped = 0;
        for (_, entry) in self.sessions.iter_mut() {
            dropped += f(&mut entry.mailbox);
        }
        dropped += f(self.sessions.orphans_mut());
        if dropped > 0 {
            tracing::debug!(dropped, "mail dropped");
        }
    }

    /// Take mail out of every session's mailbox and the orphan mailbox
    /// with `f`, and return what came from a connected app or a closed
    /// session. The daemon's own sessions (the demo, the care agent, the
    /// Codex client) read no mailbox anybody waits on, so their mail goes
    /// with no word, as it always has.
    fn take_mail(&mut self, mut f: impl FnMut(&mut VecDeque<Mail>) -> Vec<Mail>) -> Vec<Mail> {
        let mut taken = Vec::new();
        let mut dropped = 0;
        for (_, entry) in self.sessions.iter_mut() {
            let mail = f(&mut entry.mailbox);
            if matches!(entry.kind, SessionKind::Internal) {
                dropped += mail.len();
            } else {
                taken.extend(mail);
            }
        }
        taken.extend(f(self.sessions.orphans_mut()));
        let total = dropped + taken.len();
        if total > 0 {
            tracing::debug!(mail = total, "mail dropped");
        }
        taken
    }

    /// Every order among `mail` failed for `reason`: `order.failed` and the
    /// deck, as the next step's commands (`Command::OrderResult`). A step
    /// is its card's answer and fails with it; answers and snoozes go with
    /// no word, as before.
    fn fail_dropped_orders(&mut self, mail: Vec<Mail>, reason: &str) {
        for mail in mail {
            let MailKind::Order {
                text,
                card: None,
                order,
                ..
            } = mail.kind
            else {
                continue;
            };
            // The store publishes it for whoever dictated the order, while
            // it can still be reported on; this is only the fallback.
            let principal = self
                .store
                .snapshot()
                .agents
                .get(&mail.agent)
                .map_or(Principal::Uid(0), |a| a.owner.principal.clone());
            tracing::info!(agent = %mail.agent.0, reason, "an order was never read");
            self.followups.push_back(Command::OrderResult {
                principal,
                agent: mail.agent,
                text,
                ok: false,
                reason: Some(reason.to_string()),
                order,
            });
        }
    }

    /// `agent.order` never goes on the bus: it belongs to the owner alone.
    ///
    /// Words the owner cannot be handed fail on the deck rather than
    /// vanish: a connection that is closed or full, a session that is gone
    /// (`Command::OrderResult`, applied after this step).
    fn route_order(&mut self, event: &Event, now: Instant) {
        use tokio::sync::mpsc::error::TrySendError;
        let Some(agent) = event_agent(event) else {
            return;
        };
        let Some(session) = self.owner_session(&agent) else {
            return;
        };
        let text = event.data["text"].as_str().unwrap_or_default().to_string();
        let order = event.data["order"].as_str().map(String::from);
        let failed = if event.data["via"].as_str() == Some("push") {
            match self.sessions.get(session).map(|e| &e.kind) {
                Some(SessionKind::Live { tx }) => {
                    match tx.try_send(Outbound::Line(crate::api::rpc::event_line(event))) {
                        Ok(()) => None,
                        Err(TrySendError::Full(_)) => Some("channel_full"),
                        Err(TrySendError::Closed(_)) => Some("owner_gone"),
                    }
                }
                // The demo is its own owner, and pushes to nobody.
                Some(SessionKind::Internal) => None,
                _ => Some("owner_gone"),
            }
        } else {
            let mail = Mail {
                agent: agent.clone(),
                at: now,
                kind: MailKind::Order {
                    text: text.clone(),
                    ctx: event.data["ctx"].as_str().unwrap_or("talk").to_string(),
                    choice: None,
                    card: None,
                    order: order.clone(),
                },
            };
            (!self.hand_to_waiter(session, mail)).then_some("owner_gone")
        };
        if let Some(reason) = failed {
            tracing::warn!(session = %session, agent = %agent.0, reason, "an order could not be handed to its owner");
            self.followups.push_back(Command::OrderResult {
                principal: event.principal.clone(),
                agent,
                text,
                ok: false,
                reason: Some(reason.to_string()),
                order,
            });
        }
    }

    /// A snooze on a push/poll card reaches its owner as well (§3.2): a
    /// notification for a connection, `{kind:"snooze"}` for a waiter.
    fn route_snooze(&mut self, event: &Event, now: Instant) {
        let Some(agent) = event_agent(event) else {
            return;
        };
        let Some(card) = event_card(event) else {
            return;
        };
        let deliver = self
            .store
            .snapshot()
            .agents
            .get(&agent)
            .and_then(|a| a.card.as_ref())
            .filter(|c| c.id == card)
            .map(|c| c.deliver);
        let Some(session) = self.owner_session(&agent) else {
            return;
        };
        match deliver {
            Some(Deliver::Push) => self.notify(session, crate::api::rpc::event_line(event)),
            Some(Deliver::Poll) => {
                let mail = Mail {
                    agent,
                    at: now,
                    kind: MailKind::Snooze { card },
                };
                self.hand_to_waiter(session, mail);
            }
            // `hold` was released and degraded; `focus`/`none` never snooze.
            _ => {}
        }
    }

    /// One already serialized notification for a connection (dropped when
    /// the session is not one, or its channel is full).
    fn notify(&self, session: SessionId, line: String) {
        if let Some(SessionKind::Live { tx }) = self.sessions.get(session).map(|e| &e.kind) {
            if let Err(e) = tx.try_send(Outbound::Line(line)) {
                tracing::warn!(session = %session, error = %e, "notification dropped");
            }
        }
    }

    /// The implicit heartbeat (§3.2): every 10 s the lease of every agent
    /// owned by a connected (or internal) session is refreshed, so only a
    /// logical HTTP session must call `agent.heartbeat` itself. Mail past
    /// `mailbox_ttl_s` is dropped on the same beat.
    fn beat(&mut self, now: Instant) {
        if self
            .beat_at
            .is_some_and(|at| now.duration_since(at) < HEARTBEAT_EVERY)
        {
            return;
        }
        self.beat_at = Some(now);
        for session in self.sessions.heartbeat_sessions() {
            let agents: Vec<AgentId> = self.owned_by(session).into_iter().collect();
            if agents.is_empty() {
                continue;
            }
            let Some(principal) = self.sessions.get(session).map(|e| e.principal.clone()) else {
                continue;
            };
            let (result, mut events, effects) =
                self.store
                    .call(principal, session, Request::AgentHeartbeat { agents }, now);
            self.execute_all(effects, &events, now);
            self.drain_followups(&mut events, now);
            if let Err(e) = result {
                tracing::debug!(session = %session, error = %e, "implicit heartbeat refused");
            }
        }
        let ttl = Duration::from_secs(u64::from(self.config.api.mailbox_ttl_s));
        // An order nobody picked up in time was not delivered: the deck says
        // so, late but true.
        let expired = self.take_mail(|m| sessions::mailbox::take_older(m, now, ttl));
        self.fail_dropped_orders(expired, "expired");
        self.drain_followups(&mut Vec::new(), now);
    }

    /// The orderly end: the closing metric, `Command::Shutdown` (holds
    /// released by the store), the bus, the delivery record (the cards
    /// still up end as `shutdown`), `state.json`, `status.json`.
    pub fn shutdown(&mut self) {
        let now = Instant::now();
        self.refresh_metric(now);
        self.step(Command::Shutdown, now);
        self.publish();
        self.take_asks();
        self.deliveries.shutdown(self.store.ts(now));
        self.state.deliveries = self.deliveries.persisted(now);
        self.flush_state();
        self.write_status(now);
    }

    /// `--demo`: the mockup roster registered through the API by the demo
    /// client, so the store owns it like any virtual agent's.
    pub fn enable_demo(&mut self, now: Instant) {
        let mut demo = Demo::new();
        let ts = self.store.ts(now);
        // The demo is an internal session: it owns its agents, is
        // heartbeated like a connection and never expires.
        self.sessions
            .open_internal(demo::SESSION, demo::principal(), "demo", now);
        for script in demo::scripts() {
            let Some(id) = self
                .demo_call(Request::AgentRegister(script.register()), now)
                .and_then(|r| serde_json::from_value::<AgentId>(r["agent"].clone()).ok())
            else {
                continue;
            };
            self.demo_call(Request::AgentUpdate(script.update(&id, ts)), now);
            if let Some(card) = script.card(&id) {
                self.demo_call(Request::CardSet(card), now);
            }
            if let Some(reply) = script.reply(&id) {
                self.demo_call(Request::Reply(reply), now);
            }
            demo.add(id, script.slot);
        }
        for req in demo::device_requests(0) {
            self.demo_call(req, now);
        }
        tracing::info!(agents = demo.agents(), "--demo: mockup roster registered");
        self.demo = Some(demo);
        self.enable_permission_demo(now);
    }

    /// The design's §4.13 example agent (contract §4): `cohdl-api` on slot
    /// 6 with a **held** permission card the demo itself parks, so the
    /// trust refusal and the hold path can be tried on real hardware.
    fn enable_permission_demo(&mut self, now: Instant) {
        let claude = demo::permission_principal();
        let registered = self
            .demo_call_as(
                claude.clone(),
                Request::AgentRegister(demo::permission_register()),
                now,
            )
            .and_then(|r| serde_json::from_value::<AgentId>(r["agent"].clone()).ok());
        let Some(id) = registered else {
            tracing::warn!("--demo: the example agent was refused");
            return;
        };
        self.demo_call_as(
            claude,
            Request::AgentUpdate(demo::permission_update(&id)),
            now,
        );
        if let Some(demo) = self.demo.as_mut() {
            demo.add(id.clone(), demo::PERMISSION_SLOT);
            demo.set_permission(id.clone());
        }
        self.push_permission_card(&id, now);
    }

    /// Set (or re-set) the demo's held permission card and keep its `HoldRx`.
    fn push_permission_card(&mut self, agent: &AgentId, now: Instant) {
        match self.set_card_held(
            demo::permission_principal(),
            demo::SESSION,
            demo::permission_card(agent),
            now,
        ) {
            Ok((card, _token, hold)) => {
                tracing::info!(agent = %agent.0, card = card.0, "demo: permission card held");
                if let Some(demo) = self.demo.as_mut() {
                    demo.park(card, hold);
                }
            }
            Err(e) => {
                tracing::warn!(agent = %agent.0, error = %e, "demo: the permission card was refused")
            }
        }
    }

    /// The demo's side of the hold: a resolution is logged and treated as
    /// written, and the card is set again `REPUSH_AFTER` later (§4).
    fn demo_tick(&mut self, now: Instant) {
        let Some(demo) = self.demo.as_mut() else {
            return;
        };
        let outcome = demo.poll_hold(now);
        let due = demo.repush_due(now);
        let agent = demo.permission_agent().cloned();
        let Some(agent) = agent else {
            return;
        };
        if let Some(outcome) = outcome {
            tracing::info!(
                agent = %agent.0,
                card = outcome.card().0,
                "demo: hold resolved {}",
                outcome.verdict()
            );
            if matches!(outcome, demo::HoldOutcome::Answer { .. }) {
                self.step(
                    Command::HoldWritten {
                        agent: agent.clone(),
                        card: outcome.card(),
                    },
                    now,
                );
            }
        }
        if due {
            self.push_permission_card(&agent, now);
        }
    }

    pub fn is_demo(&self) -> bool {
        self.demo.is_some()
    }

    /// Apply one command, execute its effects and any follow-up commands;
    /// returns the events.
    pub fn step(&mut self, cmd: Command, now: Instant) -> Vec<Event> {
        let mut events = self.apply(cmd, now);
        self.drain_followups(&mut events, now);
        events
    }

    /// Show on the deck that something the operator asked for did not
    /// happen, the way a failed order is shown: a card on `agent` when
    /// nothing of it waits on the deck, else the deck's own toast when
    /// nothing was sent, else nothing (`deck_model::Store::fail_on_deck`
    /// has the rule). `reason` is one word for the record; `line` is what
    /// the deck shows, in each of its languages. Returns what was shown.
    pub fn fail_on_deck(
        &mut self,
        agent: Option<&AgentId>,
        reason: &str,
        line: FailureLine,
        now: Instant,
    ) -> Shown {
        let (shown, mut events, effects) = self.store.fail_on_deck(agent, reason, line, now);
        self.execute_all(effects, &events, now);
        self.drain_followups(&mut events, now);
        tracing::info!(agent = ?agent.map(|a| &a.0), reason, shown = shown.as_str(), "a failure on the deck");
        shown
    }

    fn drain_followups(&mut self, events: &mut Vec<Event>, now: Instant) {
        while let Some(next) = self.followups.pop_front() {
            events.extend(self.apply(next, now));
        }
    }

    fn apply(&mut self, cmd: Command, now: Instant) -> Vec<Event> {
        let (events, effects) = self.store.apply(cmd, now);
        self.execute_all(effects, &events, now);
        events
    }

    fn execute_all(&mut self, effects: Vec<Effect>, events: &[Event], now: Instant) {
        for effect in effects {
            match effect {
                // The one effect that needs the events of its own `apply`:
                // `order.failed` is published for whoever `order
                // .dispatched` named, and the effect itself carries no
                // principal (`DesktopCmd::Inject` is `{agent, text, order}`).
                Effect::Desktop(cmd) => self.send_desktop(cmd, events),
                other => self.execute(other, now),
            }
        }
        for event in events {
            tracing::debug!(rev = event.rev, kind = event.kind.as_str(), "event");
        }
        self.route_events(events, now);
        self.note_answer_outcomes(events);
        self.note_deliveries(events, now);
        self.settle_tests(events, now);
        self.forget_inboxes(events);
        self.pending.extend(events.iter().cloned());
    }

    /// One API request from the demo client, like a dispatcher would run it.
    fn demo_call(&mut self, req: Request, now: Instant) -> Option<serde_json::Value> {
        self.demo_call_as(demo::principal(), req, now)
    }

    /// The same as another principal: the example agent is the Claude
    /// adapter's (a token never owns a permission card, §4.6).
    fn demo_call_as(
        &mut self,
        principal: Principal,
        req: Request,
        now: Instant,
    ) -> Option<serde_json::Value> {
        let (result, mut events, effects) = self.store.call(principal, demo::SESSION, req, now);
        self.execute_all(effects, &events, now);
        self.drain_followups(&mut events, now);
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                tracing::warn!(error = %e, "demo: request refused");
                None
            }
        }
    }

    /// A device line: the store first, then the demo client's reaction.
    pub fn on_device_line(&mut self, line: DeviceLine, now: Instant) -> Vec<Event> {
        // The delivery record reads every line first: an image request is
        // answered here and never reaches the store.
        self.deliveries.device_line(&line, now);
        if let DeviceLine::Image { slot, rev, index, offset } = &line {
            self.on_device_image(*slot, *rev, *index, *offset, now);
            return Vec::new();
        }
        let events = self.step(Command::Device(line), now);
        // The deck said which channel or card it shows: that session is
        // read from now, not from the next tick.
        self.reader_check(now);
        let acts = self
            .demo
            .as_mut()
            .map(|d| d.react(&events))
            .unwrap_or_default();
        for act in acts {
            match act {
                Act::Api(req) => {
                    self.demo_call(*req, now);
                }
                Act::Op(op) => self.send_op(op),
                Act::Transcript { agent, text } => {
                    if let Some(slot) = self
                        .store
                        .snapshot()
                        .agents
                        .get(&agent)
                        .and_then(|a| a.slot)
                    {
                        self.send_op(DeviceOp::Control(Control::Transcript {
                            slot,
                            text,
                            last: true, press: None, failed: false, channel_tip: false, rec: false,
                        }));
                    }
                }
            }
        }
        events
    }

    pub fn on_link(&mut self, link: LinkEvent, now: Instant) {
        tracing::info!(?link, "link");
        self.step(Command::Link(link), now);
        // A deck that links wakes the collectors now, not at the next tick.
        self.gate_check(now);
        self.write_status(now);
    }

    /// The adapters' per-daemon state, the collectors' gate on it: what a
    /// handle built around this core shares with it.
    pub fn adapters(&self) -> &Arc<crate::adapters::rt::Adapters> {
        &self.adapters
    }

    /// Start the collectors' dormancy (`crate::dormancy`), or end it with
    /// `Dormancy::off()`: `service.dormancy` and the gate say so at once,
    /// before any collector has looked.
    pub fn set_dormancy(&mut self, dormancy: crate::dormancy::Dormancy, now: Instant) {
        self.dormancy = dormancy;
        self.step(Command::Dormancy(self.dormancy.state()), now);
        self.gate_check(now);
    }

    /// The collectors' gate, from the link, the deck's strip and the clock
    /// (`crate::dormancy`), published to every collector; on every tick and
    /// every link change. While the service is quiet the agents the Claude
    /// and Codex adapters own are released, on every check, so one a hook
    /// registered just as the quiet began goes too.
    pub fn gate_check(&mut self, now: Instant) {
        let linked = self.store.snapshot().device.host_online;
        let wall = self.store.ts(now);
        if self.dormancy.observe(linked, wall, now) {
            if self.dormancy.collecting() {
                tracing::info!("collectors on: the deck linked");
            } else {
                tracing::info!("collectors off: no deck linked for ten minutes");
            }
            self.step(Command::Dormancy(self.dormancy.state()), now);
            self.write_status(now);
        }
        let gate = crate::dormancy::Gate::of(&self.dormancy, self.store.snapshot());
        self.adapters.set_gate(gate);
        if !gate.collect {
            self.release_collected(now);
        }
        self.reader_check(now);
    }

    /// The sessions whose history is collected (`crate::reader`), published
    /// beside the gate: on every tick, link event and line from the deck.
    /// The feed is lazy exactly while dormancy is on. A Claude or Codex
    /// session whose linger ran out has its workflow runs dropped
    /// (`Command::ReaderLeft`): its transcript watcher kept them current. A
    /// connected app publishes its own and keeps them.
    pub fn reader_check(&mut self, now: Instant) {
        if !self.dormancy.enabled() {
            self.adapters.set_reader(crate::reader::Reader::all());
            return;
        }
        let reading = self.store.reader();
        let agents = &self.store.snapshot().agents;
        let (fed, stopped) = self
            .readers
            .observe(reading, now, |agent| agents.contains_key(agent));
        for agent in stopped {
            let collected = self
                .store
                .snapshot()
                .agents
                .get(&agent)
                .is_some_and(|a| self.collected(a));
            if collected {
                self.step(Command::ReaderLeft { agent }, now);
            }
        }
        let sessions = fed.len();
        if self.adapters.set_reader(crate::reader::Reader::only(fed)) {
            tracing::debug!(sessions, "reader: the sessions the deck reads");
        }
    }

    /// Whether the service collects this agent from its source itself: it
    /// is the Claude or the Codex adapter's, told by the session that owns
    /// it, never by its principal (the demo's example agent speaks as the
    /// Claude adapter from a session of its own). These go quiet with
    /// dormancy and are read lazily; a connected app's, the demo's and the
    /// test agent are fed by whoever owns them.
    fn collected(&self, agent: &deck_model::Agent) -> bool {
        use crate::care::sources::Source;
        self.sessions
            .get(agent.owner.session)
            .and_then(Source::of)
            .is_some_and(|source| matches!(source, Source::Claude | Source::Codex))
    }

    /// The Claude and Codex adapters' agents ([`Core::collected`]), released
    /// while the service is quiet (`agent.removed{reason:"dormant"}`). They
    /// come back the way they come at start-up when a deck links: the
    /// roster seed, Codex's loaded threads, the next hook. Agents of
    /// connected apps, the demo and the test agent are kept.
    ///
    /// Released as the daemon's own act, the way the implicit heartbeat
    /// runs: the source did nothing, so it stamps no `last_event_at`.
    fn release_collected(&mut self, now: Instant) {
        let collected: Vec<(Principal, SessionId, AgentId)> = self
            .store
            .snapshot()
            .agents
            .values()
            .filter(|agent| self.collected(agent))
            .map(|agent| {
                (
                    agent.owner.principal.clone(),
                    agent.owner.session,
                    agent.id.clone(),
                )
            })
            .collect();
        for (principal, session, agent) in collected {
            let req = Request::AgentRelease {
                agent,
                reason: Some(crate::dormancy::DORMANT.into()),
            };
            let (result, mut events, effects) = self.store.call(principal, session, req, now);
            self.execute_all(effects, &events, now);
            self.drain_followups(&mut events, now);
            if let Err(e) = result {
                tracing::debug!(code = e.code, "a quiet release was refused");
            }
        }
    }

    /// What the deck said about itself, or why its link is ending: the
    /// store keeps it, and `status.json` follows whatever changed.
    pub fn on_device_report(&mut self, report: DeviceReport, now: Instant) {
        tracing::debug!(?report, "device report");
        if !self.step(Command::DeviceReport(report), now).is_empty() {
            self.write_status(now);
        }
    }

    pub fn on_metric(&mut self, metric: Metric, now: Instant) {
        tracing::info!("metrics: {}", metric.summary());
        self.metric = metric;
        self.metric_at = Some(now);
        self.write_status(now);
    }

    fn execute(&mut self, effect: Effect, now: Instant) {
        match effect {
            Effect::Device(op) => self.send_op(op),
            Effect::Persist(patch) => {
                // Card ids are written synchronously, before the card line goes out.
                let sync = matches!(patch, deck_model::StatePatch::CardId { .. });
                if self.state.apply(&patch) {
                    if sync {
                        self.flush_state();
                    } else {
                        self.state_dirty.get_or_insert(now);
                    }
                }
            }
            Effect::Timer { key, at } => {
                self.timer_seq += 1;
                self.timers.insert((at, self.timer_seq), key);
            }
            Effect::Deliver {
                agent,
                card,
                answer,
                deliver,
            } => self.deliver(agent, card, answer, deliver, now),
            Effect::ReleaseHold {
                agent,
                card,
                reason,
            } => {
                let parked = self.holds.release(&agent, card, reason);
                tracing::debug!(agent = %agent.0, card = card.0, ?reason, parked, "release hold");
            }
            Effect::Audio(cmd) => self.send_audio(cmd),
            // Handled in `execute_all`, which has the events this needs.
            Effect::Desktop(cmd) => self.send_desktop(cmd, &[]),
        }
    }

    /// Install a real audio engine's handle (B1). Before this, and in every
    /// build without one, `Effect::Audio` is logged and forgotten and the
    /// §7.4 machine runs exactly as it did.
    pub fn set_audio(&mut self, audio: AudioHandle) {
        self.audio = audio;
    }

    /// `try_send` only, exactly like [`Core::send_op`] — the core must never
    /// block behind an SCO bring-up wedged on a Bluetooth timeout (V1
    /// measured bring-up at ~1 s median and a wedged one is unbounded).
    ///
    /// Unlike a device op there is **no re-projection** to cover a drop, so
    /// what a refusal means is per command
    /// ([`deck_model::AudioCmd::aborts_when_refused`],
    /// [`deck_model::AudioCmd::retry_when_refused`]) and this is where that
    /// policy is applied.
    fn send_audio(&mut self, cmd: deck_model::AudioCmd) {
        if let deck_model::AudioCmd::Listen { agent, ctx, from_deck, .. } = &cmd {
            self.device_channel_tip_target = if *from_deck && *ctx == deck_model::PttCtx::Talk {
                agent.as_ref().and_then(|id| {
                    let a = self.store.snapshot().agents.get(id)?;
                    let press = self.store.snapshot().voice.ptt.as_ref()?.device_press?;
                    (a.kind == deck_model::Kind::Claude).then(|| (press, id.clone()))
                })
            } else { None };
            if let Some((press, id)) = &self.device_channel_tip_target {
                if let Some(slot) = self.store.snapshot().agents.get(id).and_then(|a| a.slot) {
                    self.send_op(DeviceOp::Control(Control::Transcript {
                        slot, text: String::new(), last: false, press: Some(*press),
                        failed: false, channel_tip: false, rec: false,
                    }));
                }
            }
        }
        if !self.audio.engine() {
            tracing::debug!(?cmd, "audio: no engine in this build");
            return;
        }
        if let Some(cmd) = self.audio.send_or_return(cmd) {
            self.refuse_audio(cmd);
        }
    }

    /// One `AudioCmd` the queue would not take.
    fn refuse_audio(&mut self, cmd: deck_model::AudioCmd) {
        if cmd.aborts_when_refused() {
            // A `Listen` that was never queued opened nothing, and the
            // operator is holding a button for a capture that does not
            // exist. Tell them now, through the same seam every other voice
            // event comes in by.
            tracing::warn!("core→audio full; the PTT session is aborted");
            self.followups.push_back(Command::Voice(VoiceIn::Error {
                code: "audio_busy".into(),
                message: None,
            }));
            return;
        }
        if cmd.retry_when_refused() {
            if self.audio_retry.len() < crate::audio::RETRY_CAPACITY {
                self.audio_retry.push_back(cmd);
            } else {
                // Even the retry queue has a bound; past it the capture is
                // stranded and the counter is the only honest record.
                self.audio_dropped += 1;
            }
            return;
        }
        tracing::debug!(?cmd, "core→audio full; dropped");
        self.audio_dropped += 1;
    }

    /// One `DesktopCmd` from the reducer (§5.5): a hand-off, an order to
    /// deliver, or a Macropad keystroke.
    fn send_desktop(&mut self, cmd: deck_model::DesktopCmd, events: &[Event]) {
        use deck_model::DesktopCmd;
        match cmd {
            // No agent and no guard: straight to the desktop thread.
            DesktopCmd::Keystroke { keys } => {
                let job = DesktopJob::Keystroke { keys };
                if let Some(DesktopJob::Keystroke { keys }) = self.desktop.send_or_return(job) {
                    tracing::info!(%keys, engine = self.desktop.engine(), "macro keystroke dropped: no desktop, or its queue is full");
                }
            }
            DesktopCmd::Focus { agent } => self.hand_off(agent),
            DesktopCmd::Inject { agent, text, order } => {
                self.deliver_order(agent, text, order, events);
            }
        }
    }

    /// The hand-off (`deck_model::handoff`): ENTER on a session's `Open app`
    /// card, or `op.focus`, brings forward the app the session runs in.
    ///
    /// Only a hook-registered session's app is raised: what a connected app
    /// declares about itself is informational (§4.6), and such an app
    /// brings itself forward on `card.focused` / `focus.changed`. With no
    /// desktop in this build (`--no-device`, Linux) nothing is raised and
    /// nothing is claimed. A hand-off that cannot happen — no app known for
    /// the session, the app not running, macOS refusing — comes back as a
    /// [`FromDesktop::Raised`] and is shown on the deck from there
    /// ([`Core::on_from_desktop`]), outside the step that asked.
    fn hand_off(&mut self, agent: AgentId) {
        let Some(found) = self.store.snapshot().agents.get(&agent) else {
            return;
        };
        if found.source != deck_model::Source::Hook {
            tracing::debug!(agent = %agent.0, "hand-off: a connected app's agent, which the app brings forward itself");
            return;
        }
        if !self.desktop.engine() {
            tracing::debug!(agent = %agent.0, "hand-off: no desktop in this build");
            return;
        }
        let Some(app) = handoff_app(found) else {
            tracing::info!(agent = %agent.0, "hand-off: no app is known for this session");
            let _ = self.desktop.outcomes().send(FromDesktop::Raised {
                agent,
                app: None,
                outcome: crate::desktop::Outcome::NoApp,
            });
            return;
        };
        if self
            .desktop
            .send_or_return(DesktopJob::Raise { agent, app })
            .is_some()
        {
            tracing::warn!("core->desktop full; a hand-off is dropped");
        }
    }

    /// An order to a session the daemon delivers itself (`caps.order:
    /// "inject"`): by Claude Channels once the session has opted in, else
    /// by the session's own inbox, else not at all — `order.failed
    /// {reason:"no_route"}` after the `order.dispatched` that already went
    /// out, which the deck shows like any failed order. Nothing is ever
    /// typed into a terminal.
    fn deliver_order(&mut self, agent: AgentId, text: String, order: String, events: &[Event]) {
        // The principal `order.failed` is published for: whoever the
        // `order.dispatched` of this same step named, else the agent's
        // owner, so the event stays attributable.
        let principal = events
            .iter()
            .find(|e| {
                e.kind == EventKind::OrderDispatched && event_agent(e).as_ref() == Some(&agent)
            })
            .map(|e| e.principal.clone())
            .or_else(|| {
                self.store
                    .snapshot()
                    .agents
                    .get(&agent)
                    .map(|a| a.owner.principal.clone())
            })
            .unwrap_or(Principal::Uid(0));
        // Guard 2, read fresh: a post into an open dialog would cancel it,
        // so both routes refuse with `dialog_open` and send nothing.
        let dialog_open = self
            .store
            .snapshot()
            .agents
            .get(&agent)
            .and_then(|a| a.card.as_ref())
            .is_some_and(deck_model::Card::is_open_dialog);
        // Channels are preferred only after a notification round trip proves
        // this session opted in.
        if let Some(target) = self.store.snapshot().agents.get(&agent) {
            if self.adapters.claude_channels.post(
                target,
                &text,
                Some(order.clone()),
                self.adapters.inbox(&agent),
                dialog_open,
                self.desktop.outcomes(),
            ) {
                return;
            }
        }
        // The inbox (`crate::inbox`): a Claude session whose message socket
        // a hook reported is posted to — no focus, no TCC, and the only
        // route a Desktop-app session has. It needs no desktop engine, so a
        // `--no-device` daemon still delivers a spoken order this way.
        if let Some(inbox) = self.adapters.inbox(&agent) {
            crate::inbox::post_async(
                self.desktop.outcomes(),
                agent,
                text,
                Some(order),
                inbox,
                dialog_open,
            );
            return;
        }
        tracing::info!(agent = %agent.0, "an order with no route: no Claude Channels connection and no inbox");
        self.followups.push_back(Command::OrderResult {
            principal,
            agent,
            text,
            ok: false,
            reason: Some("no_route".into()),
            order: Some(order),
        });
    }

    /// One outcome from `deckd-desktop`, or from a worker that reports
    /// through its channel (the inbox, Claude Channels).
    pub fn on_from_desktop(&mut self, msg: FromDesktop, now: Instant) {
        match msg {
            FromDesktop::Order {
                agent,
                text,
                order,
                result,
            } => {
                let principal = self
                    .store
                    .snapshot()
                    .agents
                    .get(&agent)
                    .map(|a| a.owner.principal.clone())
                    .unwrap_or(Principal::Uid(0));
                let reason = result.as_ref().err().map(|e| e.reason().to_string());
                self.step(
                    Command::OrderResult {
                        principal,
                        agent,
                        text,
                        ok: result.is_ok(),
                        reason,
                        order,
                    },
                    now,
                );
            }
            // A hand-off that happened is silent: the app is in front of the
            // operator. One that did not is shown on the deck the way a
            // failed order is, in the deck's language.
            FromDesktop::Raised {
                agent,
                app,
                outcome,
            } => {
                // Which app a session runs in is content (`terminal.app`):
                // the log at its default level says what happened, not where.
                tracing::info!(agent = %agent.0, outcome = %outcome.word(), "desktop: hand-off");
                tracing::debug!(agent = %agent.0, app = app.as_deref().unwrap_or("-"), "desktop: hand-off app");
                if let Some(failure) = outcome.failure() {
                    self.fail_on_deck(Some(&agent), failure.reason(), failure.line(), now);
                }
            }
            FromDesktop::Accessibility { granted } => {
                if granted {
                    tracing::info!("desktop: Accessibility is granted; Macropad keystrokes go through");
                } else {
                    tracing::warn!(
                        "desktop: Accessibility is not granted; Macropad keystrokes are refused until it is \
                         (System Settings > Privacy & Security > Accessibility)"
                    );
                }
                self.accessibility_granted = Some(granted);
            }
        }
    }

    /// A removed agent's inbox token dies with it (`crate::inbox`): the
    /// table is keyed by the `ag_…` id an `agent.removed` names, and every
    /// way an agent can go ends in that event.
    fn forget_inboxes(&mut self, events: &[Event]) {
        for event in events.iter().filter(|e| e.kind == EventKind::AgentRemoved) {
            if let Some(agent) = event_agent(event) {
                self.adapters.forget_inbox(&agent);
            }
        }
    }

    /// Install a real desktop worker's handle. Before this, and in every
    /// build without one, a hand-off or a keystroke is logged and
    /// forgotten.
    pub fn set_desktop(&mut self, desktop: DesktopHandle) {
        self.desktop = desktop;
    }

    /// `Say`/`Chime`/`Gain` a full audio queue refused (`metrics.get`).
    #[must_use]
    pub fn audio_dropped(&self) -> u64 {
        self.audio_dropped
    }

    /// `Stop`/`Finish` waiting for the next tick (`metrics.get`).
    #[must_use]
    pub fn audio_retry_len(&self) -> usize {
        self.audio_retry.len()
    }

    /// Retry the `Stop`/`Finish` commands a full queue refused, in order.
    fn replay_audio(&mut self) {
        while let Some(cmd) = self.audio_retry.pop_front() {
            if let Some(cmd) = self.audio.send_or_return(cmd) {
                self.audio_retry.push_front(cmd);
                return;
            }
        }
    }

    /// One report from the audio side, the way [`Core::on_from_hid`] takes
    /// one from the hid thread.
    pub fn on_from_audio(&mut self, msg: FromAudio, now: Instant) {
        match msg {
            FromAudio::Voice(v) => {
                self.step(Command::Voice(v), now);
            }
            // Not a `Command`: it changes nothing observable and moves no
            // `rev` — it is the answer to a question `voice.engines` asks,
            // cached here so that answering it never touches the run loop
            // (M5). Reported once; a second one simply replaces it.
            FromAudio::Engines { stt, tts } => {
                tracing::info!(stt = %stt.name, tts = %tts.name, "audio: engines up");
                self.engines = Some((stt, tts));
            }
            FromAudio::Transcribe { id } => self.offer_transcription(id),
        }
    }

    /// `FromAudio::Transcribe`: hand a released press's recording to the
    /// connection that owns its agent, as an `agent.transcribe` event sent
    /// to that connection alone (like `agent.order`, and never on the bus:
    /// it is about one agent's own audio). Nobody to hand it to fails the
    /// offer, and the engine uses its local final straight away.
    pub fn offer_transcription(&mut self, id: u64) {
        use crate::audio::companion::{offer_id, PAGE, WAV_RATE};
        let Some(info) = self.transcribe.info(id) else { return };
        let snapshot = self.store.snapshot();
        let owner = snapshot.agents.get(&info.agent).and_then(|agent| {
            (agent.caps.transcribe == deck_model::TranscribeCap::Push).then_some(agent.owner.session)
        });
        let live = owner.filter(|session| {
            matches!(self.sessions.get(*session).map(|e| &e.kind), Some(SessionKind::Live { .. }))
        });
        let Some(session) = live else {
            tracing::debug!(agent = %info.agent.0, "companion transcription: no live owner");
            self.transcribe.fail(id, "no live owner");
            return;
        };
        if !self.transcribe.bind(id, session) {
            return;
        }
        let line = crate::api::rpc::notification(
            "event",
            serde_json::json!({
                "type": "agent.transcribe",
                "rev": snapshot.rev,
                "ts": wall_now(),
                "data": {
                    "agent": info.agent,
                    "transcription": offer_id(id),
                    "mime": "audio/wav",
                    "rate": WAV_RATE,
                    "ms": info.ms,
                    "bytes": info.bytes,
                    "page": PAGE,
                    "locale": snapshot.voice.stt.locale,
                },
            }),
        );
        tracing::info!(agent = %info.agent.0, ms = info.ms, bytes = info.bytes, "companion transcription: offered");
        self.notify(session, line);
    }

    /// The `(stt, tts)` the audio side last reported (`voice.engines`).
    #[must_use]
    pub fn engines(
        &self,
    ) -> Option<&(
        crate::audio::traits::EngineInfo,
        crate::audio::traits::EngineInfo,
    )> {
        self.engines.as_ref()
    }

    /// Install the cell `service.restart`/`service.stop` write their exit
    /// code into (`core::run` holds the other end). Without it the two
    /// methods still answer and the daemon keeps running, which is the
    /// right behaviour for every rig that is not a supervised `deckd run`.
    pub fn set_exit(&mut self, exit: Arc<crate::service::Exit>) {
        self.exit = exit;
    }

    /// `try_send` only (§2.3): on overflow drop, remember, re-project later.
    pub fn send_op(&mut self, mut op: DeviceOp) {
        if let DeviceOp::Control(Control::Transcript { slot, press, failed, channel_tip, .. }) = &mut op {
            *channel_tip = !*failed && self.device_channel_tip_target.as_ref()
                .filter(|(p, _)| Some(*p) == *press)
                .and_then(|(_, id)| self.store.snapshot().agents.get(id))
                .is_some_and(|agent| agent.slot == Some(*slot) && !self.adapters.claude_channels.ready(agent));
        }
        if self.needs_resync {
            self.drop_op(op);
            return;
        }
        if let Err(TrySendError::Full(op) | TrySendError::Disconnected(op)) =
            self.hid.ops.try_send(op)
        {
            tracing::warn!("core→hid channel full; re-projecting after it drains");
            self.needs_resync = true;
            self.overflows += 1;
            self.drop_op(op);
        }
    }

    /// A refused op: a `Put` is covered by the re-projection; a `Remove` or
    /// a control (`gone`, card clear, focus ack, transcript) is not, so it
    /// waits in the side queue.
    fn drop_op(&mut self, op: DeviceOp) {
        self.ops_dropped += 1;
        if matches!(op, DeviceOp::Put(..) | DeviceOp::Sync { .. }) {
            return;
        }
        if self.dropped.len() < DROPPED_CAPACITY {
            self.dropped.push_back(op);
        } else {
            self.dropped_overflow = true;
        }
    }

    /// Replay the side queue once the channel has room; false when it is
    /// full again (the replay resumes on a later tick, order kept).
    fn replay_dropped(&mut self) -> bool {
        if std::mem::take(&mut self.dropped_overflow) {
            // Too many removals lost to know which: the hid side forgets
            // every slot and the focus, and the re-projection rebuilds them
            // (`gone` + full record + card per slot, one barrier).
            self.dropped.clear();
            for slot in 1..=self.store.snapshot().device.capacity() {
                self.dropped
                    .push_back(DeviceOp::Remove(Key::AgentState(Slot(slot))));
            }
            self.dropped.push_back(DeviceOp::Remove(Key::Focus));
        }
        while let Some(op) = self.dropped.pop_front() {
            match self.hid.ops.try_send(op) {
                Ok(()) => {}
                Err(TrySendError::Full(op)) => {
                    self.dropped.push_front(op);
                    self.needs_resync = true;
                    return false;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.dropped.clear();
                    return false;
                }
            }
        }
        true
    }

    /// Housekeeping on every tick: timers, the overflow re-projection, the
    /// debounced `state.json` write.
    pub fn tick(&mut self, now: Instant) {
        // Cancellation before the SetCardHeld reply is received has no
        // HoldGuard to report HoldGone. Reap its closed receiver as well.
        for (agent, card) in self.holds.take_abandoned() {
            self.step(Command::HoldGone { agent, card }, now);
        }
        if self.needs_resync && self.hid.ops.len() < crate::hid::OPS_CAPACITY / 2 {
            self.needs_resync = false;
            if self.replay_dropped() {
                self.step(Command::Link(LinkEvent::Overflow), now);
            }
        }
        // Timers are stateless: one tick settles every due deadline (§3).
        if self.timers.range(..=(now, u64::MAX)).next().is_some() {
            self.timers = self.timers.split_off(&(now, u64::MAX));
            self.step(Command::Tick(now), now);
        }
        // The delivery record: the settle window's counts, and the reader's
        // into `state.json` at most once a minute (a card that ended is
        // copied at once).
        self.take_asks();
        if self.deliveries.due(now) {
            self.persist_deliveries(now);
        }
        if self
            .state_dirty
            .is_some_and(|since| now.duration_since(since) >= STATE_DEBOUNCE)
        {
            self.flush_state();
        }
        // B1: the `Stop`/`Finish` a full audio queue refused. First, and
        // before the timers, because one of them may be what closes a
        // capture the 60 s cap would otherwise have to end.
        self.replay_audio();
        self.expire_waits(now);
        self.expire_answers(now);
        self.expire_logical(now);
        self.beat(now);
        self.demo_tick(now);
        // An install the deck confirmed while someone was talking.
        self.start_confirmed_install(now);
        // The countdowns under the usage bars are written to the minute.
        if self.usage.due(now) {
            self.push_usage(now);
        }
        // The window running out, and a strip that gained or lost the
        // Usage or System face.
        self.gate_check(now);
        // After the gate, so a look waits on the deck as it is now.
        self.check_for_updates(now);
    }

    pub fn flush_state(&mut self) {
        self.state_dirty = None;
        if let Err(e) = self.state.save(&self.paths.state) {
            tracing::error!(error = %e, "cannot write state.json");
        }
    }

    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    /// The live status for `status.json`.
    pub fn status(&self, now: Instant) -> Status {
        let snap = self.store.snapshot();
        Status {
            pid: std::process::id(),
            version: snap.service.version.clone(),
            started: self.started_wall,
            updated: wall_now(),
            uptime_s: now.duration_since(self.started).as_secs(),
            sid: snap.device.sid,
            demo: self.demo.is_some(),
            link: snap.device.link,
            session: snap.device.session,
            fw: snap.device.fw.clone(),
            boot: snap.device.boot,
            face: snap.device.face,
            faces: snap.device.faces.len(),
            agents: snap.by_slot().len(),
            rev: snap.rev,
            err_counts: snap.device.err_counts,
            overflows: self.overflows,
            ops_dropped: self.ops_dropped,
            metric: self.metric,
            transport: snap.device.transport,
            battery: snap.device.battery,
            charging: snap.device.charging,
            board: snap.device.board.clone(),
            deliveries: Some(self.deliveries.brief()),
            collecting: self.dormancy.state().map(|state| state.collecting),
            dormant_since: self.dormancy.state().and_then(|state| state.dormant_since),
        }
    }

    pub fn write_status(&mut self, now: Instant) {
        if let Err(e) = self.status(now).write(&self.paths.status) {
            tracing::warn!(error = %e, "cannot write status.json");
        }
    }

    /// Ask the hid thread for a metric and do **not** wait: the next report
    /// lands through `on_from_hid` like any other. `service.info` uses this
    /// so an `observe` call can never park the core thread (§9).
    pub fn nudge_metric(&self) {
        let _ = self.hid.debug.send(HidDebug::Metric);
    }

    /// Ask the hid thread for a metric now and wait briefly for it,
    /// handling whatever else arrives meanwhile.
    pub fn refresh_metric(&mut self, now: Instant) {
        if self.hid.debug.send(HidDebug::Metric).is_err() {
            return;
        }
        let until = Instant::now() + METRIC_WAIT;
        while let Ok(msg) = self
            .hid
            .lines
            .recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            let is_metric = matches!(msg, FromHid::Metric(_));
            self.on_from_hid(msg, now);
            if is_metric {
                break;
            }
        }
    }

    pub fn on_from_hid(&mut self, msg: FromHid, now: Instant) {
        match msg {
            FromHid::Line(line) => {
                self.on_device_line(line, now);
            }
            FromHid::Link(link) => self.on_link(link, now),
            FromHid::Report(report) => self.on_device_report(report, now),
            FromHid::Identity(serial) => self.on_identity(serial.as_deref(), now),
            FromHid::Metric(m) => self.on_metric(m, now),
        }
    }

    /// The deck the hid thread opened lists `serial`: when it names a deck,
    /// that deck becomes the current one in `state.json`, the store takes
    /// its own strip, settings and keys (and the hid thread's view with
    /// them, on the same ordered channel), and then `ProfileReady` lets the
    /// link's first hello go, so the sync it starts is this deck's. A deck
    /// that names none changes nothing, and the hid thread does not wait
    /// for it. Logs carry the deck's label, never its serial number.
    pub fn on_identity(&mut self, serial: Option<&str>, now: Instant) {
        let was = self.state.deck.clone();
        let selected = self.state.select(serial);
        if selected == crate::state::Selected::Unidentified {
            tracing::debug!("the deck lists no serial number: the last deck identified stays");
            return;
        }
        // The same deck is no news, unless the cable just named for good a
        // record a Bluetooth spelling had made.
        if selected != crate::state::Selected::Same || self.state.deck != was {
            self.state_dirty.get_or_insert(now);
        }
        let deck = self.state.deck_label();
        tracing::info!(deck = deck.as_deref().unwrap_or_default(), ?selected, "deck identified");
        let profile = self.state.profile();
        self.step(Command::DeckProfile { deck, profile }, now);
        self.send_op(DeviceOp::Control(Control::ProfileReady));
    }
}

/// The `(agent, card)` of an `op.answer`, whose reply waits for its
/// delivery (§4.9); `None` for every other request.
fn answer_target(req: &Request) -> Option<(AgentId, CardId)> {
    match req {
        Request::OpAnswer(a) => Some((a.agent.clone(), a.card)),
        _ => None,
    }
}

/// `data.reason` of an event, when it carries one.
fn reason_of(data: &serde_json::Value) -> Option<String> {
    data.get("reason")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

/// What `deckd run` needs.
pub struct RunOptions {
    pub config: Config,
    pub paths: Paths,
    pub demo: bool,
    pub foreground: bool,
    /// `--no-device`: never enumerate or open the deck (CI, the API gate).
    pub no_device: bool,
}

/// `deckd run`: lock, state, store, hid thread, the
/// core thread, the API doors on a 2-worker tokio runtime, until
/// SIGTERM/SIGINT; then the doors stop, the core shuts down (holds
/// released, status written) and the hid thread is joined. Returns the
/// exit code.
pub fn run(mut opts: RunOptions) -> Result<i32, CoreError> {
    let _lock = match crate::service::lock(&opts.paths.lock) {
        Ok(lock) => lock,
        Err(crate::service::ServiceError::AlreadyRunning) => {
            tracing::info!("deckd is already running");
            return Ok(crate::service::EXIT_ALREADY_RUNNING);
        }
        Err(e) => return Err(e.into()),
    };
    if !opts.foreground {
        match crate::service::capture_std_streams(&opts.paths.logs) {
            Ok(path) => tracing::info!(path = %path.display(), "stdout/stderr → launchd log"),
            Err(e) => tracing::warn!(error = %e, "cannot open the launchd log"),
        }
    }
    crate::service::install_panic_hook(opts.config.privacy.log_content);
    // A line the service no longer reads (`config::RETIRED_KEYS`) loaded
    // as nothing; the log names it, as every reload will.
    if let Ok(text) = std::fs::read_to_string(&opts.paths.config) {
        crate::config::warn_retired(&text);
    }
    let (state, loaded) = State::load(&opts.paths.state);
    if loaded == crate::state::Loaded::Corrupt {
        tracing::warn!("state.json was corrupt; moved aside and started empty");
    }
    // The cloud voice's key (`crate::secrets`): its store read here, on the
    // main thread before the core exists, because a Keychain call can wait
    // on a dialog, and a key still in config.toml moved into the store. The
    // config in force carries the stored key; the file, the watcher and the
    // doors do not.
    let mut voice_key =
        crate::secrets::VoiceKey::open(crate::secrets::home_for(&opts.paths, opts.no_device));
    opts.config = voice_key.adopt(&opts.paths, opts.config);
    let in_force = voice_key.overlay(Arc::new(opts.config.clone()));
    // The TCP door, decided once: `[api] tcp` when the file names it, else
    // off — unless Claude's settings still carry the pre-plugin hooks that
    // post to it, which are looked for only then (`config::Api::tcp_door`).
    let door = opts
        .config
        .api
        .tcp_door(|| crate::integrations::legacy_claude_hooks(&opts.paths, &opts.config));
    let legacy_hooks = door.rule == crate::config::TcpRule::LegacyHooks;
    tracing::info!(on = door.on, "tcp door: {}", door.reason());
    let sid = new_sid();
    let started = Instant::now();
    let mut cfg = StoreConfig::new(started, wall_now(), sid);
    // With the door off no port will be held, so the snapshot names none
    // from the start (`api::serve` records the same).
    cfg.port = if door.on { opts.config.api.port } else { 0 };
    cfg.grace_s = opts.config.agents.grace_s;
    cfg.ready_ttl_s = opts.config.cards.ready_ttl_s;
    cfg.hold_s = opts.config.hooks.claude.hold_s;
    cfg.pins = opts.config.pins();
    cfg.announce_questions = opts.config.voice.announce_questions;
    cfg.voice_pool = in_force.voice_pool();
    cfg.announce_max_chars = opts.config.voice.tts.announce_max_chars;
    cfg.companion_stt = opts.config.voice.stt.companion;
    cfg.persisted = state.persisted();
    let store = Store::new(cfg);
    // The deck's voice link (§7): one handle, two ports. Its Bluetooth port
    // attaches by itself (CoreBluetooth, to a deck the system holds
    // connected); its cable port is the hid thread's, driven whenever the
    // instance it opened is the USB one. Only with the BLE transport, and
    // never for `--no-device`: a door test may not touch the radio.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let voice_link = (!opts.no_device
        && opts.config.voice.transport == crate::config::VoiceTransport::Ble)
        .then(crate::ble::BleAudio::spawn);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let voice_link: Option<crate::ble::BleAudio> = None;
    // `--no-device`: the hid thread runs without hardware (never enumerates),
    // so the link's metrics and debug controls keep answering.
    let hid = crate::hid::spawn(crate::hid::HidConfig {
        pace: opts.config.device.pace(),
        hello_every: Duration::from_secs(u64::from(opts.config.device.hello_s)),
        sid,
        clock_local: opts.config.device.clock == "local",
        hardware: !opts.no_device,
        audio: voice_link.as_ref().map(crate::ble::BleAudio::usb_port),
        status_every: opts.config.device.status_every(),
        identify_wait: Some(crate::hid::IDENTIFY_WAIT),
    });
    let mut core = Core::new(store, state, hid, opts.paths.clone());
    core.set_voice_key(voice_key);
    // `--no-device` never opens the deck, and a firmware check is the one
    // request that would: it probes USB from a thread of its own, which
    // `hardware: false` above does not reach, and then fetches the release
    // manifest. The app sends one when the owner asks, so here it answers
    // with the idle status the updater has (B2); and the service's own
    // looks never start (`Core::check_for_updates`).
    if opts.no_device {
        core.firmware = crate::firmware::Updater::offline();
    }
    // …and `care.checks` does not even enumerate HID for its `device` row.
    core.no_device = opts.no_device;
    core.legacy_hooks = legacy_hooks;
    // The voice engine (§7). `--no-device` leaves `AudioHandle::none()` in
    // place, which is exactly the daemon the seam pass shipped: every
    // `Effect::Audio` is logged and forgotten and the §7.4 machine still
    // runs. The `api` gate step runs with it, and must: a door test may not
    // open a microphone.
    #[cfg(target_os = "macos")]
    if !opts.no_device {
        core.tts_settings
            .store(Arc::new(crate::audio::cloud::Settings::from_config(&in_force)));
        core.stt_settings
            .store(Arc::new(crate::stt::Settings::from_config(&opts.config)));
        // The local recogniser over Apple's: a pass-through until the
        // operator opts in and its model is on disk.
        let whisper = Arc::new(crate::audio::whisper::WhisperStt::new(
            core.stt_settings.clone(),
            opts.paths.stt.clone(),
            Arc::new(crate::audio::apple::AppleStt::new()),
        ));
        whisper.ensure_loaded();
        core.whisper = Some(whisper.clone());
        // The local voice over Apple's, likewise.
        core.kokoro_settings
            .store(Arc::new(crate::kokoro::Settings::from_config(&opts.config)));
        let kokoro = Arc::new(crate::audio::kokoro::KokoroTts::new(
            core.kokoro_settings.clone(),
            opts.paths.kokoro.clone(),
            Arc::new(crate::audio::apple::AppleTts::new()),
        ));
        kokoro.ensure_loaded();
        core.kokoro = Some(kokoro.clone());
        core.set_audio(crate::audio::spawn(&crate::audio::AudioConfig {
            transport: opts.config.voice.transport,
            device_uid: opts.config.voice.device_uid.clone(),
            fallback_output: opts.config.voice.fallback_output.clone(),
            link: voice_link,
            tts_engine: opts.config.voice.tts.engine.clone(),
            sidecar: Some(crate::audio::sidecar_config(&opts.config, &opts.paths)),
            cloud: Some(core.tts_settings.clone()),
            stt: Some(whisper),
            local_tts: Some(kokoro),
            companion: Some(core.transcribe.clone()),
        }));
    }
    // Linux: the deck's BLE audio service through BlueZ and the local Whisper
    // model. No Apple recogniser or voice to fall through to (`audio::linux`).
    #[cfg(target_os = "linux")]
    if !opts.no_device {
        core.stt_settings
            .store(Arc::new(crate::stt::Settings::from_config(&opts.config)));
        let whisper = Arc::new(crate::audio::whisper::WhisperStt::new(
            core.stt_settings.clone(),
            opts.paths.stt.clone(),
            Arc::new(crate::audio::linux::NoStt),
        ));
        whisper.ensure_loaded();
        core.whisper = Some(whisper.clone());
        core.set_audio(crate::audio::linux::spawn(&crate::audio::AudioConfig {
            transport: opts.config.voice.transport,
            device_uid: opts.config.voice.device_uid.clone(),
            fallback_output: opts.config.voice.fallback_output.clone(),
            link: voice_link,
            tts_engine: opts.config.voice.tts.engine.clone(),
            sidecar: None,
            cloud: None,
            stt: Some(whisper),
            local_tts: Some(Arc::new(crate::audio::command::CommandTts::new(
                &opts.config.voice.tts.command,
                &opts.config.voice.tts.voices,
            ))),
            companion: Some(core.transcribe.clone()),
        }));
    }
    // The desktop side (§5.5). `--no-device` leaves `DesktopHandle::none()`
    // in place, which is what the `api` gate step runs with: a door test may
    // not raise an app or post a keystroke.
    #[cfg(target_os = "macos")]
    if !opts.no_device {
        core.set_desktop(crate::desktop::spawn(
            crate::desktop::DesktopConfig::from_config(&opts.config),
        ));
    }
    // The doors and the core share one verifier: the core re-reads its
    // `[trust]` requirements on every config change (§3.7), the UDS door
    // asks it about each peer.
    let verifier: Arc<dyn crate::trust::PeerVerifier> = Arc::new(crate::trust::MacVerifier::new(
        opts.config.trust.gui_requirement.clone(),
        opts.config.trust.dev_requirement.clone(),
    ));
    core.set_verifier(verifier.clone());
    core.set_config(Arc::new(opts.config.clone()));
    core.set_bus(Arc::new(Bus::new(opts.config.privacy.event_ring)));
    // Seed the hid thread's view with the whole projection.
    for op in core.store.project() {
        core.send_op(op);
    }
    if opts.demo {
        core.enable_demo(Instant::now());
    }
    // Quiet without a deck (`crate::dormancy`): a daemon that can have one
    // collects only while one was linked in the last ten minutes, counted
    // from the last link `state.json` kept. `--no-device` has no deck to
    // wait for and collects as it always did. Set before the core thread and
    // every collector start, so the first look any of them takes is right.
    let now = Instant::now();
    let dormancy = if opts.no_device {
        crate::dormancy::Dormancy::off()
    } else {
        let last = core
            .store
            .snapshot()
            .device
            .last
            .as_ref()
            .map(|last| last.at);
        crate::dormancy::Dormancy::start(crate::dormancy::WINDOW, last, wall_now(), now)
    };
    core.set_dormancy(dormancy, now);
    core.write_status(Instant::now());
    let stop = install_signal_handlers();
    // `service.restart`/`service.stop`: the core writes the code here and
    // this loop is what reads it (§4.11). See `service::Exit` for why it is
    // a deadline and not a flag — the reply has to be written first.
    let exit = Arc::new(crate::service::Exit::default());
    core.set_exit(exit.clone());
    let bus = core.bus.clone();
    let (handle, core_thread) = core.spawn();
    let watch = crate::config::watch(
        &opts.paths,
        opts.config.clone(),
        handle.clone(),
        bus.clone(),
    );
    let api = Arc::new(crate::api::Api {
        core: handle.clone(),
        bus,
        config: watch.config(),
        // The watcher's own cell: `tokens.toml` is loaded there and
        // reloaded on every change, so a `deckd token create` while the
        // daemon runs is accepted without a restart (§3.4, §3.7).
        tokens: watch.tokens(),
        verifier,
        // §5: one set of adapters for the whole daemon, reading the same
        // hot-reloaded config as everything else.
        adapters: Arc::new(crate::adapters::Adapters::new(watch.config())),
        paths: opts.paths.clone(),
        legacy_hooks,
    });
    tracing::info!(
        sid,
        version = env!("CARGO_PKG_VERSION"),
        demo = opts.demo,
        no_device = opts.no_device,
        "deckd running"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("deckd-api")
        .enable_all()
        .build()
        .map_err(CoreError::Runtime)?;
    // The doors and the liveness pollers go up on the runtime's own
    // threads; `main` is then handed to a CFRunLoop (B10).
    let (served, stop_liveness, liveness) = runtime.block_on(async {
        let served = crate::api::serve(api.clone()).await;
        tracing::info!(port = ?served.port, uds = served.uds, "api serving");
        // The liveness pollers (§5.1): `claude agents --json` every 5 s and
        // `kill(pid, 0)` every 2 s for resolved CLI pids only. They run
        // here rather than inside `api::serve` because they are the
        // daemon's own housekeeping, not a door: a door test spins up an
        // `Api` a dozen times and must never spawn a CLI to do it.
        let (stop_liveness, liveness_rx) = tokio::sync::watch::channel(false);
        let codexd_rx = stop_liveness.subscribe();
        let liveness = tokio::spawn(crate::liveness::run(api.clone(), liveness_rx));
        // The Codex app-server daemon client (`crate::codexd`): the roster,
        // approvals and orders of every Codex thread on the shared daemon,
        // sessions that started before this daemon included.
        let codexd = tokio::spawn(crate::codexd::run(api.clone(), codexd_rx));
        // The Claude usage probe (`crate::usage::claude`): the account's
        // rate-limit windows for the Usage face, polled; Codex's come
        // through the app-server client above.
        let usage_rx = stop_liveness.subscribe();
        // The Claude Channels door, open while the owner has the bridge on:
        // the switch in the live config, or the plugin's own helper while
        // the file leaves it unset, read the way `config.get` reads it.
        let channels_on = {
            let (config, data) = (api.config.clone(), api.paths.data.clone());
            move || {
                crate::integrations::claude::channels_on(
                    config.load().sources.claude_channels,
                    &data,
                )
            }
        };
        let channels = tokio::spawn(crate::claude_channel::serve(
            api.paths.data.clone(),
            api.core.adapters().claude_channels.clone(),
            channels_on,
            stop_liveness.subscribe(),
        ));
        let system = tokio::spawn(crate::system_status::run(api.core.clone(), stop_liveness.subscribe()));
        // The doors' half of the quiet: the transcript watchers and the
        // Claude adapter's memo go when the collectors stop.
        let follower = tokio::spawn(crate::dormancy::follow(
            api.clone(),
            stop_liveness.subscribe(),
        ));
        // The lazy reader feed's Claude half: a transcript watcher for each
        // Claude session the deck opens (`crate::reader`).
        let reader = tokio::spawn(crate::reader::follow(
            api.clone(),
            stop_liveness.subscribe(),
        ));
        let usage = tokio::spawn(crate::usage::claude::run(api, usage_rx));
        (
            served,
            stop_liveness,
            (liveness, codexd, usage, channels, system, follower, reader),
        )
    });
    // **B10.** §2.3 requires a CFRunLoop on the main thread, and V13 makes
    // it load-bearing rather than decorative: AVSpeech's `writeUtterance`
    // buffers and `SFSpeechRecognizer`'s result handlers are delivered on
    // main **only** (measured: a worker with no run loop got 0 buffers, a
    // worker driving its own run loop got 0, main got 300/300), and §7.8's
    // TCC prompts need it too. Until now `main` sat in
    // `tokio::time::sleep`, so the main queue existed and never drained —
    // a daemon that could not have spoken however correct the rest was.
    //
    // It is a turning loop rather than `CFRunLoopRun()` because `main` is
    // also the daemon's supervisor: it watches the stop flag the signal
    // handler sets and the core thread's liveness. A `CFRunLoopStop` from
    // inside a signal handler is not async-signal-safe, so the flag stays
    // a flag and the loop keeps looking at it.
    while !stop.load(Ordering::Relaxed)
        && !core_thread.is_finished()
        && exit.requested(Instant::now()).is_none()
    {
        #[cfg(target_os = "macos")]
        crate::audio::mainloop::pump(STOP_POLL);
        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(STOP_POLL);
    }
    runtime.block_on(async {
        let _ = stop_liveness.send(true);
        let (liveness, codexd, usage, channels, system, follower, reader) = liveness;
        let _ = tokio::time::timeout(Duration::from_secs(2), liveness).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), codexd).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), usage).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), channels).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), system).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), follower).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        served.shutdown().await;
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(watch);
    handle.send_blocking(CoreMsg::Shutdown);
    match core_thread.join() {
        Ok(core) => {
            // Dropping the job sender ends `deckd-desktop` once the job it
            // is running (an `open -b`, a keystroke) is done. Joined before
            // the hid thread because it is the one that still touches
            // something outside us.
            core.desktop.join();
            core.hid.join();
        }
        Err(_) => tracing::error!("the core thread panicked"),
    }
    let _ = std::fs::remove_file(&opts.paths.socket);
    let _ = std::fs::write(&opts.paths.port, "");
    // A signal is still a 0: SIGTERM is launchd or the operator ending the
    // process, and only the two API methods name a code of their own.
    let code = exit.code().unwrap_or(0);
    tracing::info!(code, "deckd stopped");
    Ok(code)
}

/// The `deckd-core` thread: device lines first, then whichever of the
/// hid channel, the doors' messages and the ticker is ready; the bus after
/// every step. Returns the core after an orderly shutdown.
pub fn run_core(mut core: Core, msgs: Receiver<CoreMsg>) -> Core {
    let ticker = tick(TICK);
    loop {
        // Device priority: whatever the hid thread queued goes first.
        while let Ok(msg) = core.hid.lines.try_recv() {
            core.on_from_hid(msg, Instant::now());
        }
        core.publish();
        select! {
            recv(core.hid.lines) -> msg => match msg {
                Ok(msg) => core.on_from_hid(msg, Instant::now()),
                Err(_) => {
                    tracing::warn!("hid thread gone; stopping the core");
                    break;
                }
            },
            recv(msgs) -> msg => match msg {
                Ok(CoreMsg::Shutdown) | Err(_) => break,
                Ok(msg) => core.on_msg(msg, Instant::now()),
            },
            // A disconnect is unreachable while the handle lives (it keeps
            // a sender of its own precisely so this arm is never
            // permanently ready), and a `break` here would stop a daemon
            // that has everything except audio to do.
            recv(core.audio.events) -> msg => if let Ok(msg) = msg {
                core.on_from_audio(msg, Instant::now());
            },
            // Same reasoning as the audio arm: the handle keeps a sender
            // of its own so this can never be permanently ready, and a
            // daemon with no desktop has everything else to do.
            recv(core.desktop.events) -> msg => if let Ok(msg) = msg {
                core.on_from_desktop(msg, Instant::now());
            },
            // The keychain thread's answers (`crate::secrets::Keeper`). Its
            // handle keeps a sender of its own too.
            recv(core.keeper.events) -> msg => if let Ok(done) = msg {
                core.on_keeper(done, Instant::now());
            },
            recv(ticker) -> _ => core.tick(Instant::now()),
        }
        core.publish();
    }
    core.shutdown();
    core
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() -> &'static AtomicBool {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores an atomic flag (async-signal-safe).
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    &STOP
}

/// Shared fixtures for the crate's tests: a core with detached hid
/// channels, and one running on its own thread.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::hid::{HidDebug, HidHandle};
    use deck_model::{
        AgentRegister, AnswerCap, Caps, CardId, CardKind, CardSet, Choice, Deliver, Kind, OrderCap,
        Origin, Scope,
    };

    pub fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("deckd-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What `f` logs at `WARN` and above on this thread, as the service's
    /// log would print it (without colour). Only this thread's events are
    /// seen, so tests running beside it cannot add to the text.
    pub fn warnings_of(f: impl FnOnce()) -> String {
        logged_at(tracing::Level::WARN, f)
    }

    /// [`warnings_of`] at `INFO` and above: what the service's log keeps at
    /// its default level.
    pub fn infos_of(f: impl FnOnce()) -> String {
        logged_at(tracing::Level::INFO, f)
    }

    fn logged_at(level: tracing::Level, f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Text(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Text {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let text = Text::default();
        let writer = text.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(level)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = text.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    /// A core whose hid side is a pair of channels the test holds.
    pub fn detached(
        dir: &std::path::Path,
    ) -> (
        Core,
        Receiver<DeviceOp>,
        crossbeam_channel::Sender<FromHid>,
        Receiver<HidDebug>,
    ) {
        let paths = Paths::from_data_dir(dir.to_path_buf(), dir.join("logs"), dir.join("caches"));
        let store = Store::new(StoreConfig::new(Instant::now(), Ts(1_789_760_523_000), 7));
        let (hid, ops, lines, debug) = HidHandle::detached();
        (
            Core::new(store, State::default(), hid, paths),
            ops,
            lines,
            debug,
        )
    }

    /// `detached` on a fresh temp dir, which is returned for the cleanup.
    pub fn detached_core(
        name: &str,
    ) -> (
        Core,
        Receiver<DeviceOp>,
        crossbeam_channel::Sender<FromHid>,
        Receiver<HidDebug>,
        std::path::PathBuf,
    ) {
        let dir = temp(name);
        let (core, ops, lines, debug) = detached(&dir);
        (core, ops, lines, debug, dir)
    }

    pub fn ctx(session: u64, scopes: &[Scope]) -> CallCtx {
        CallCtx {
            principal: Principal::Uid(501),
            session: SessionId(session),
            scopes: scopes.to_vec(),
            trusted: false,
            own: BTreeSet::new(),
        }
    }

    /// A poll/poll virtual agent.
    pub fn register(name: &str, key: Option<&str>) -> AgentRegister {
        AgentRegister {
            kind: Kind::Other,
            name: name.into(),
            key: key.map(String::from),
            cwd: None,
            pid: None,
            pid_kind: None,
            terminal: None,
            lease_ttl_s: 30,
            caps: Caps {
                answer: AnswerCap::Poll,
                order: OrderCap::Poll,
                focus: false,
                transcribe: Default::default(),
            },
            slot_hint: None,
            voice: None,
            meta: None,
        }
    }

    /// A two-choice custom poll question.
    pub fn card(agent: &AgentId, text: &str) -> CardSet {
        CardSet {
            agent: agent.clone(),
            id: None,
            kind: CardKind::Q,
            text: text.into(),
            choices: vec![
                Choice {
                    label: "Yes".into(),
                    detail: None,
                },
                Choice {
                    label: "No".into(),
                    detail: None,
                },
            ],
            rec: None,
            free_text: false,
            deliver: Deliver::Poll,
            timeout_s: None,
            origin: Origin::custom(),
        }
    }

    /// A core on its own `deckd-core` thread with detached hid channels;
    /// stopped and joined on drop, its temp dir removed.
    pub struct LiveCore {
        pub handle: CoreHandle,
        pub ops: Receiver<DeviceOp>,
        pub lines: crossbeam_channel::Sender<FromHid>,
        pub debug: Receiver<HidDebug>,
        pub dir: std::path::PathBuf,
        join: Option<std::thread::JoinHandle<Core>>,
    }

    impl LiveCore {
        pub fn start(name: &str) -> LiveCore {
            LiveCore::start_with(name, |_| {})
        }

        /// `start`, with the core set up first (a key home, say).
        pub fn start_with(name: &str, setup: impl FnOnce(&mut Core)) -> LiveCore {
            let (mut core, ops, lines, debug, dir) = detached_core(name);
            setup(&mut core);
            let (handle, join) = core.spawn();
            LiveCore {
                handle,
                ops,
                lines,
                debug,
                dir,
                join: Some(join),
            }
        }

        pub fn paths(&self) -> Paths {
            Paths::from_data_dir(
                self.dir.clone(),
                self.dir.join("logs"),
                self.dir.join("caches"),
            )
        }
    }

    impl Drop for LiveCore {
        fn drop(&mut self) {
            self.handle.send_blocking(CoreMsg::Shutdown);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[allow(dead_code)]
    pub fn card_id(v: &serde_json::Value) -> CardId {
        serde_json::from_value(v["card"]["id"].clone()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{detached, temp};
    use super::*;
    use crate::hid::HidDebug;
    use deck_model::{
        AgentState, CardId, EventKind, HostLine, Key, PttCtx, SessionState, SetKey, Slot,
    };

    fn ops_of(rx: &Receiver<DeviceOp>) -> Vec<DeviceOp> {
        rx.try_iter().collect()
    }

    #[test]
    fn device_transcription_tip_tracks_claude_channel_readiness_and_press() {
        use deck_model::{AdapterKind, Kind, OrderCap};
        for (name, kind, ready) in [("fallback", Kind::Claude, false),
            ("channel", Kind::Claude, true), ("codex", Kind::Codex, false)] {
            let dir = temp(&format!("ptt-tip-{name}"));
            let (mut c, ops, _lines, _debug) = detached(&dir);
            let now = Instant::now();
            let principal = Principal::Adapter(AdapterKind::Claude);
            let sid = SessionId(9);
            c.sessions.open_internal(sid, principal.clone(), "claude-adapter", now);
            let mut reg = testkit::register(name, Some("claude:tip-target"));
            reg.kind = kind;
            reg.pid = Some(42);
            reg.pid_kind = Some(deck_model::PidKind::Cli);
            reg.slot_hint = Some(Slot(1));
            reg.caps.order = OrderCap::Inject;
            c.api_call(principal, sid, false, Request::AgentRegister(reg), now).unwrap();
            if ready { c.adapters.claude_channels.test_channel(42, "tip-target"); }
            ops.try_iter().for_each(drop);
            c.step(Command::Device(DeviceLine::Ptt { act: 1, slot: 1, ctx: PttCtx::Talk,
                card: None, abort: false, press: Some(5) }), now);
            let start: Vec<_> = ops.try_iter().filter_map(|op| match op {
                DeviceOp::Control(Control::Transcript { channel_tip, press: Some(5), .. }) => Some(channel_tip),
                _ => None,
            }).collect();
            assert_eq!(start, if kind == Kind::Claude { vec![!ready] } else { vec![] }, "{name}");
            // The target is retained while ASR finishes after the PTT release.
            c.step(Command::Device(DeviceLine::Ptt { act: 0, slot: 1, ctx: PttCtx::Talk,
                card: None, abort: false, press: Some(5) }), now);
            ops.try_iter().for_each(drop);
            for (press, failed) in [(5, false), (4, false), (5, true)] {
                c.send_op(DeviceOp::Control(Control::Transcript { slot: Slot(1), text: "hello".into(),
                    last: false, press: Some(press), failed, channel_tip: false , rec: false}));
                assert!(matches!(ops.recv().unwrap(), DeviceOp::Control(Control::Transcript { channel_tip, .. })
                    if channel_tip == (kind == Kind::Claude && !ready && press == 5 && !failed)));
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn claude_inbox_orders_do_not_require_a_terminal_or_desktop_engine() {
        use crate::inbox::Inbox;
        use deck_model::{AdapterKind, Kind, OpOrder, OrderCap, OrderSource, Terminal};
        use std::io::Read;
        use std::os::unix::net::UnixListener;

        for (label, program, channel) in [
            ("desktop", None, false),
            ("desktop-channel", None, true),
            ("iterm", Some("iTerm.app"), false),
            ("iterm-channel", Some("iTerm.app"), true),
            ("wezterm", Some("WezTerm"), false),
            ("wezterm-channel", Some("WezTerm"), true),
        ] {
            let dir = temp(&format!("route-{label}"));
            let socket = dir.join("peer.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let (mut c, _ops, _lines, _debug) = detached(&dir);
            assert!(!c.desktop.engine());
            let now = Instant::now();
            let adapter = Principal::Adapter(AdapterKind::Claude);
            let session = SessionId(9);
            c.sessions
                .open_internal(session, adapter.clone(), "claude-adapter", now);
            let mut reg = testkit::register(label, Some("claude:voice-target"));
            reg.kind = Kind::Claude;
            reg.pid = Some(42);
            reg.pid_kind = Some(deck_model::PidKind::Cli);
            reg.caps.order = OrderCap::Inject;
            reg.terminal = program.map(|program| Terminal {
                program: Some(program.into()),
                ..Terminal::default()
            });
            let registered = c
                .api_call(adapter, session, false, Request::AgentRegister(reg), now)
                .unwrap();
            let agent = AgentId(registered["agent"].as_str().unwrap().into());
            c.adapters.set_inbox(
                agent.clone(),
                Inbox {
                    socket,
                    token: Some("test-token".into()),
                },
            );
            let received = channel.then(|| c.adapters.claude_channels.test_channel(42, "voice-target"));
            let text = "Please continue. 请继续。";
            c.api_call(
                Principal::Device,
                session,
                true,
                Request::OpOrder(OpOrder {
                    agent: agent.clone(),
                    text: text.into(),
                    ctx: "talk".into(),
                    source: OrderSource::Ptt,
                    utterance: None,
                }),
                now,
            )
            .unwrap();
            let result = c
                .desktop
                .events
                .recv_timeout(Duration::from_secs(2))
                .expect("inbox delivery without terminal automation");
            // Both routes carry the order's id back, so a failure would name it.
            assert!(
                matches!(&result, FromDesktop::Order { agent: to, text: sent, order: Some(order), result: Ok(()) }
                    if to == &agent && sent == text && order == "or_1"),
                "{result:?}"
            );
            if let Some(received) = received {
                assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), text);
                assert!(
                    listener.accept().is_err(),
                    "channel delivery must not also reach inbox"
                );
            } else {
                let (mut stream, _) = listener.accept().unwrap();
                // The completion arrives after the sender closed its stream.
                stream.set_nonblocking(true).unwrap();
                let mut payload = String::new();
                stream.read_to_string(&mut payload).unwrap();
                let frames: Vec<serde_json::Value> = payload
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert_eq!(frames.len(), 2);
                assert_eq!(frames[1]["message"]["content"], text);
            }
            c.on_from_desktop(result, now);
            c.publish();
            assert!(!c
                .bus
                .replay(0, 100, None)
                .0
                .iter()
                .any(|e| e.kind == EventKind::OrderFailed));
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// B1: the core→audio channel is bounded and `try_send` only, and what
    /// a refusal *means* is per command (contract §3's back-pressure
    /// decision).
    ///
    /// The core must never block here. V1 measured SCO bring-up at ~1 s
    /// median and a bring-up wedged behind a Bluetooth timeout has no upper
    /// bound at all — a `send` would stop every agent, every hook and every
    /// device line for as long as a headset felt like taking. And unlike a
    /// device op there is no re-projection to paper over a drop, so
    /// "dropped" cannot be the answer for all of them: losing a `Finish`
    /// strands a live capture with the mic open until the 60 s cap.
    #[test]
    fn a_full_audio_queue_aborts_a_listen_retries_a_finish_and_drops_a_chime() {
        use crate::audio::{AudioHandle, FromAudio, CMDS_CAPACITY};
        use deck_model::{AudioCmd, VoiceIn};
        let dir = temp("audio-backpressure");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let (audio, cmds, events) = AudioHandle::detached();
        c.set_audio(audio);
        let t0 = Instant::now();

        // A press with a working engine really reaches it.
        c.step(
            Command::Device(DeviceLine::Ptt {
                act: 1,
                slot: 1,
                ctx: PttCtx::Talk,
                card: None,
                abort: false, press: None,
            }),
            t0,
        );
        let sent: Vec<AudioCmd> = cmds.try_iter().collect();
        assert!(
            sent.iter().any(|c| matches!(c, AudioCmd::Listen { .. })),
            "{sent:?}"
        );
        c.step(
            Command::Device(DeviceLine::Ptt {
                act: 0,
                slot: 1,
                ctx: PttCtx::Talk,
                card: None,
                abort: true, press: None,
            }),
            t0,
        );
        let _ = cmds.try_iter().count();

        // Now wedge it: fill the queue and never drain.
        for _ in 0..CMDS_CAPACITY {
            c.send_audio(AudioCmd::Chime {
                which: "question".into(),
            });
        }
        assert_eq!(c.audio.queued(), CMDS_CAPACITY);
        assert_eq!(c.audio_dropped(), 0);

        // `Say`/`Chime`/`Gain`: dropped, with a counter. A queue of stale
        // announcements is worse than a missing sentence.
        c.send_audio(AudioCmd::Chime {
            which: "ready".into(),
        });
        c.send_audio(AudioCmd::Gain {
            key: SetKey::Speaker,
            value: 3,
        });
        assert_eq!(c.audio_dropped(), 2);
        assert_eq!(c.audio_retry_len(), 0);

        // `Stop`/`Finish`: kept for the next tick.
        c.send_audio(AudioCmd::Finish { abort: false });
        c.send_audio(AudioCmd::Stop {
            utterance: None,
            lane: None,
            all: true,
        });
        assert_eq!(c.audio_retry_len(), 2);
        assert_eq!(c.audio_dropped(), 2, "not dropped, held");

        // `Listen`: the session is aborted at once with `audio_busy`, so
        // the operator learns now instead of holding a dead button.
        let events_before = c.bus.replay(0, 1000, None).0.len();
        c.step(
            Command::Device(DeviceLine::Ptt {
                act: 1,
                slot: 1,
                ctx: PttCtx::Talk,
                card: None,
                abort: false, press: None,
            }),
            t0,
        );
        c.publish();
        let fresh = c.bus.replay(0, 1000, None).0;
        let aborted = fresh[events_before..]
            .iter()
            .find(|e| e.kind == EventKind::VoiceState && e.data["error"] == "audio_busy")
            .expect("voice.state{error:\"audio_busy\"}");
        assert_eq!(aborted.data["state"], "aborted");
        assert!(
            c.store.snapshot().voice.ptt.is_none(),
            "no capture was opened, so no session is left holding one"
        );

        // Drain the engine; the tick replays what was held, in order, and
        // nothing that was dropped comes back.
        let drained = cmds.try_iter().count();
        assert_eq!(drained, CMDS_CAPACITY);
        c.tick(t0 + TICK);
        let replayed: Vec<AudioCmd> = cmds.try_iter().collect();
        assert!(
            replayed
                .iter()
                .any(|c| matches!(c, AudioCmd::Finish { abort: false })),
            "{replayed:?}"
        );
        assert!(
            replayed.iter().any(|c| matches!(c, AudioCmd::Stop { .. })),
            "{replayed:?}"
        );
        assert!(
            !replayed
                .iter()
                .any(|c| matches!(c, AudioCmd::Chime { which } if which == "ready")),
            "a dropped chime is dropped: {replayed:?}"
        );
        assert_eq!(c.audio_retry_len(), 0);

        // And the seam works the other way: a voice event from the engine
        // is applied by the reducer like any other command.
        events
            .send(FromAudio::Voice(VoiceIn::Route {
                route: deck_model::Route::A2dp,
                output_uid: Some("C2-2D:output".into()),
                input_uid: None,
                stuck: false,
                slow_sco_ms: None,
            }))
            .unwrap();
        let msg = c.audio.events.recv().unwrap();
        c.on_from_audio(msg, t0);
        assert_eq!(c.store.snapshot().voice.route, deck_model::Route::A2dp);

        // A build with no engine behaves exactly as it did before week 4:
        // the command is logged and forgotten, and nothing is counted.
        c.set_audio(AudioHandle::none());
        c.send_audio(AudioCmd::Chime {
            which: "question".into(),
        });
        assert_eq!(c.audio_dropped(), 2, "no engine is not a refusal");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn effects_reach_the_hid_channel_the_state_file_and_the_timer_wheel() {
        let dir = temp("effects");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let events = c.step(
            Command::Device(DeviceLine::Set {
                key: SetKey::Mic,
                value: 3,
            }),
            t0,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(c.state.settings.mic, 3);
        assert!(!dir.join("state.json").exists(), "debounced");
        c.tick(t0 + STATE_DEBOUNCE);
        assert!(dir.join("state.json").exists());
        c.step(
            Command::Device(DeviceLine::Refresh {
                what: "usage".into(),
            }),
            t0,
        );
        assert_eq!(c.pending_timers(), 1);
        c.tick(t0 + Duration::from_secs(1));
        assert_eq!(c.pending_timers(), 1, "not due yet");
        c.tick(t0 + Duration::from_secs(2));
        assert_eq!(c.pending_timers(), 0);
        assert!(ops_of(&ops).is_empty(), "no cached usage: nothing re-sent");
        c.step(Command::Device(DeviceLine::Channel { slot: 1, seq: 9 }), t0);
        assert_eq!(
            ops_of(&ops),
            vec![
                DeviceOp::Control(Control::Focus {
                    slot: Slot(1),
                    ack: Some(9)
                }),
                DeviceOp::Remove(Key::Focus),
            ],
            "the ack, and a device-sourced focus never re-pushed by a barrier"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn ptt(act: u8, slot: u8, ctx: PttCtx) -> DeviceLine {
        DeviceLine::Ptt {
            act,
            slot,
            ctx,
            card: None,
            abort: false, press: None,
        }
    }

    #[test]
    fn the_demo_registers_the_roster_answers_cards_and_refreshes() {
        let dir = temp("demo");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        assert!(!c.is_demo());
        c.enable_demo(t0);
        assert!(c.is_demo());
        let snap = c.store.snapshot();
        assert_eq!(
            snap.by_slot().len(),
            6,
            "five mockup agents plus the §4.13 example on their slots"
        );
        let a1 = snap.agent_at(Slot(1)).unwrap();
        assert_eq!(a1.state, AgentState::Question);
        assert_eq!(a1.card.as_ref().map(|card| card.id), Some(CardId(1)));
        assert_eq!(snap.agent_at(Slot(2)).unwrap().state, AgentState::Ready);
        let seeded = ops_of(&ops);
        let keys: Vec<Key> = seeded
            .iter()
            .filter_map(|op| match op {
                DeviceOp::Put(k, _) => Some(*k),
                _ => None,
            })
            .collect();
        for key in [
            Key::AgentState(Slot(1)),
            Key::AgentTitle(Slot(1)),
            Key::Card(Slot(1), 1),
            Key::Reply(Slot(1)),
            Key::Card(Slot(2), 1),
            Key::AgentState(Slot(5)),
            Key::Reply(Slot(5)),
            Key::Ws,
            Key::Macro(deck_model::FaceId(3)),
            Key::Usage,
        ] {
            assert!(keys.contains(&key), "{key:?} missing from {keys:?}");
        }
        assert!(
            seeded.iter().any(|op| matches!(
                op,
                DeviceOp::Put(
                    Key::AgentState(Slot(1)),
                    HostLine::Agent {
                        since_s: Some(120),
                        ..
                    }
                )
            )),
            "the mockup's age"
        );
        assert!(
            seeded.iter().any(|op| matches!(
                op,
                DeviceOp::Put(Key::Card(Slot(1), 1), HostLine::Card { text, rec: Some(0), .. })
                    if text.starts_with("Tests fail")
            )),
            "no » prefix on a question card"
        );
        // An answer from the deck: delivered to the demo, cleared, agent at work.
        let events = c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card: 1,
                choice: 0,
            },
            t0,
        );
        let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::CardAnswered), "{kinds:?}");
        assert!(kinds.contains(&EventKind::CardDelivered), "{kinds:?}");
        assert!(kinds.contains(&EventKind::CardCleared), "{kinds:?}");
        let after = ops_of(&ops);
        assert!(after.contains(&DeviceOp::Control(Control::CardClear(Slot(1), 1))));
        assert!(after.contains(&DeviceOp::Remove(Key::Card(Slot(1), 1))));
        assert!(after.iter().any(|op| matches!(
            op,
            DeviceOp::Put(Key::AgentTitle(Slot(1)), HostLine::Agent { title: Some(t), .. })
                if t == "Working on choice 0"
        )));
        let a1 = c.store.snapshot().agent_at(Slot(1)).unwrap();
        assert_eq!(a1.state, AgentState::Working);
        assert!(a1.card.is_none());
        // Answered twice: refused as an error event, nothing on the wire.
        let events = c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card: 1,
                choice: 0,
            },
            t0,
        );
        assert!(events.iter().any(|e| e.kind == EventKind::Error));
        assert!(ops_of(&ops).is_empty());
        // `refresh`: the demo feeds nudged bars; the 1.5 s re-send is then moot.
        c.on_device_line(
            DeviceLine::Refresh {
                what: "usage".into(),
            },
            t0,
        );
        assert_eq!(
            ops_of(&ops),
            vec![DeviceOp::Put(Key::Usage, crate::demo::usage(1))]
        );
        c.tick(t0 + Duration::from_secs(2));
        assert!(ops_of(&ops).is_empty(), "a feeder answered: no re-send");
        // A PTT release on slot 3 types a transcript, replies and works.
        c.on_device_line(ptt(1, 3, PttCtx::Talk), t0);
        assert!(ops_of(&ops).is_empty());
        c.on_device_line(ptt(0, 3, PttCtx::Talk), t0);
        let release = ops_of(&ops);
        assert!(matches!(
            &release[0],
            DeviceOp::Control(Control::Transcript {
                slot: Slot(3),
                last: true,
                ..
            })
        ));
        assert!(release
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Reply(Slot(3)), _))));
        assert!(release.iter().any(|op| matches!(
            op,
            DeviceOp::Put(Key::AgentTitle(Slot(3)), HostLine::Agent { title: Some(t), .. })
                if t == "Refactoring auth"
        )));
        c.on_device_line(ptt(1, 0, PttCtx::Name), t0);
        c.on_device_line(ptt(0, 0, PttCtx::Name), t0);
        assert_eq!(
            ops_of(&ops),
            vec![DeviceOp::Control(Control::Name("Deploy".into()))]
        );
        let status = c.status(t0 + Duration::from_secs(3));
        assert!(status.demo);
        assert_eq!(status.agents, 6, "the roster plus the §4.13 example");
        assert_eq!(status.sid, 7);
        // The §4.13 example agent sits on slot 6 with its held permission card.
        let example = c
            .store
            .snapshot()
            .agent_at(Slot(crate::demo::PERMISSION_SLOT))
            .unwrap()
            .clone();
        assert_eq!(example.name, "cohdl-api");
        assert_eq!(example.kind, deck_model::Kind::Claude);
        assert_eq!(
            example.owner.principal,
            Principal::Adapter(deck_model::AdapterKind::Claude)
        );
        let card = example.card.as_ref().unwrap();
        assert_eq!(card.deliver, Deliver::Hold);
        assert_eq!(card.origin.kind, deck_model::OriginKind::Permission);
        assert_eq!(card.text.as_deref(), Some(crate::demo::PERMISSION_TEXT));
        assert!(c.holds.contains(&example.id, card.id), "the demo parked it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn link_events_and_metrics_write_the_status_file() {
        let dir = temp("status");
        let (mut c, _ops, _lines, debug) = detached(&dir);
        let t0 = Instant::now();
        c.on_link(LinkEvent::Linked, t0);
        let s = Status::read(&dir.join("status.json")).unwrap();
        assert_eq!(s.session, SessionState::AwaitHello);
        assert_eq!(s.pid, std::process::id());
        c.on_link(LinkEvent::Syncing { sid: 0, boot: true }, t0);
        c.on_link(LinkEvent::Synced { sid: 7 }, t0);
        c.on_metric(
            Metric {
                reports_sent: 42,
                ..Metric::default()
            },
            t0,
        );
        let s = Status::read(&dir.join("status.json")).unwrap();
        assert_eq!(s.session, SessionState::Live);
        assert_eq!(s.boot, Some(true));
        assert_eq!(s.metric.reports_sent, 42);
        // The debug controls as calls: flood and sync reach the hid thread.
        let admin = testkit::ctx(2, &[deck_model::Scope::Admin]);
        let r = c.call(
            admin.clone(),
            "device.flood",
            serde_json::json!({"n": 5}),
            t0,
        );
        assert_eq!(r.result.unwrap()["queued"], 5);
        assert_eq!(debug.try_recv(), Ok(HidDebug::Flood(5)));
        let r = c.call(
            admin.clone(),
            "device.flood",
            serde_json::json!({"n": 0}),
            t0,
        );
        assert!(r.result.is_err());
        c.call(admin, "device.sync", serde_json::Value::Null, t0);
        assert_eq!(debug.try_recv(), Ok(HidDebug::Sync));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What the hid thread reports about the deck reaches the snapshot and
    /// `status.json` (for `deckd status`); a report about a deck that is not
    /// linked changes nothing, and the end of the link clears it again.
    #[test]
    fn a_device_report_reaches_the_snapshot_and_the_status_file() {
        use deck_model::Transport;
        let dir = temp("device-report");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let report = |report| FromHid::Report(report);
        c.on_from_hid(report(DeviceReport::Board(Some("Rev B".into()))), t0);
        assert_eq!(c.store.snapshot().device.board, None, "no deck is linked");
        assert!(
            Status::read(&dir.join("status.json")).is_none(),
            "nothing to write"
        );
        c.on_from_hid(FromHid::Link(LinkEvent::Linked), t0);
        c.on_from_hid(report(DeviceReport::Transport(Some(Transport::Usb))), t0);
        let power = DeviceReport::Power {
            battery: Some(85),
            charging: Some(true),
        };
        c.on_from_hid(report(power), t0);
        c.on_from_hid(report(DeviceReport::Board(Some("Rev B".into()))), t0);
        let device = &c.store.snapshot().device;
        assert_eq!(device.transport, Some(Transport::Usb));
        assert_eq!((device.battery, device.charging), (Some(85), Some(true)));
        assert_eq!(device.board.as_deref(), Some("Rev B"));
        let s = Status::read(&dir.join("status.json")).unwrap();
        assert_eq!(
            (s.transport, s.battery, s.charging, s.board.as_deref()),
            (Some(Transport::Usb), Some(85), Some(true), Some("Rev B"))
        );
        // The link ends: why, then the `Down` that records it and clears
        // the deck.
        let removed = DeviceReport::Dropped {
            why: "removed".into(),
        };
        c.on_from_hid(report(removed), t0);
        c.on_from_hid(FromHid::Link(LinkEvent::Down), t0);
        let device = &c.store.snapshot().device;
        assert_eq!(
            (device.transport, device.battery, device.board.as_deref()),
            (None, None, None)
        );
        assert_eq!(device.drops.len(), 1);
        assert_eq!(device.drops[0].why, "removed");
        let s = Status::read(&dir.join("status.json")).unwrap();
        assert_eq!(
            (s.transport, s.battery, s.charging, s.board),
            (None, None, None, None)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The core's ticker from `from` up to `to` on a simulated clock,
    /// compressed to one tick every 10 s: the implicit heartbeat still
    /// keeps every agent's lease, as it would.
    fn tick_through(c: &mut Core, from: Instant, to: Instant) {
        let mut at = from;
        while at < to {
            c.tick(at);
            at += Duration::from_secs(10);
        }
    }

    /// R5, the core's half of the phase-3 gate, on a simulated clock: ten
    /// minutes after the deck's link drops the gate shuts, the agents of
    /// the Claude and Codex adapters are released as `dormant` and a
    /// connected app's are kept; `service.dormancy`, the snapshot and
    /// `status.json` say so once. Nine minutes fifty-nine still collects. A
    /// deck that links again opens the gate on the spot.
    #[test]
    fn ten_minutes_without_a_deck_shut_the_gate_and_release_the_collected_agents() {
        use crate::dormancy::{Dormancy, Gate, WINDOW};
        use deck_model::{AdapterKind, DormancyState};
        let dir = temp("dormancy");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let wall = c.store.ts(t0);
        c.set_dormancy(Dormancy::start(WINDOW, None, wall, t0), t0);
        assert_eq!(
            c.adapters.gate(),
            Gate::shut(),
            "no deck was ever linked here"
        );
        assert_eq!(
            c.store.snapshot().service.dormancy,
            Some(DormancyState {
                window_s: 600,
                collecting: false,
                dormant_since: None
            })
        );
        c.on_from_hid(FromHid::Link(LinkEvent::Linked), t0);
        assert!(
            !c.adapters.collecting(),
            "a link still saying hello is no deck yet"
        );
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        assert!(c.adapters.collecting(), "a deck linked: at once");
        // The two adapters' internal sessions, as their doors open them, and
        // a connected app on a socket of its own.
        let (claude_session, codex_session) = (SessionId(91), SessionId(92));
        let claude_principal = Principal::Adapter(AdapterKind::Claude);
        c.sessions
            .open_internal(claude_session, claude_principal, "claude-adapter", t0);
        c.sessions.open_internal(
            codex_session,
            Principal::Adapter(AdapterKind::Codex),
            "codex-adapter",
            t0,
        );
        let (app, _app_rx) = live(&mut c, t0);
        let claude = register_on(&mut c, claude_session, "claude", t0);
        let codex = register_on(&mut c, codex_session, "codex", t0);
        let conol = register_on(&mut c, app, "conol", t0);
        c.publish();
        let mut events = c.bus.subscribe();

        let t1 = t0 + Duration::from_secs(90);
        tick_through(&mut c, t0, t1);
        c.on_from_hid(FromHid::Link(LinkEvent::Down), t1);
        let gone = c.store.snapshot().device.last.as_ref().unwrap().at;
        let almost = WINDOW - Duration::from_secs(1);
        tick_through(&mut c, t1, t1 + almost);
        c.tick(t1 + almost);
        assert!(c.adapters.collecting(), "9:59 after the drop");
        assert_eq!(c.store.snapshot().agents.len(), 3);
        c.tick(t1 + WINDOW);
        assert_eq!(c.adapters.gate(), Gate::shut());
        let since = gone.plus(WINDOW);
        assert_eq!(
            c.store.snapshot().service.dormancy,
            Some(DormancyState {
                window_s: 600,
                collecting: false,
                dormant_since: Some(since)
            })
        );
        let agents = &c.store.snapshot().agents;
        assert!(!agents.contains_key(&claude) && !agents.contains_key(&codex));
        assert!(agents.contains_key(&conol), "a connected app's agent stays");
        let status = Status::read(&dir.join("status.json")).unwrap();
        assert_eq!(
            (status.collecting, status.dormant_since),
            (Some(false), Some(since))
        );
        c.publish();
        let mut quiet = Vec::new();
        let mut removed = Vec::new();
        while let Ok(published) = events.try_recv() {
            match published.event.kind {
                EventKind::ServiceDormancy => quiet.push(published.event.data.clone()),
                EventKind::AgentRemoved => removed.push(published.event.data.clone()),
                _ => {}
            }
        }
        assert_eq!(quiet.len(), 1, "{quiet:?}");
        assert_eq!(quiet[0]["collecting"], false);
        let reasons: BTreeMap<String, String> = removed
            .iter()
            .map(|data| {
                (
                    data["agent"].as_str().unwrap().into(),
                    data["reason"].as_str().unwrap().into(),
                )
            })
            .collect();
        let dormant = |agent: &AgentId| (agent.0.clone(), "dormant".to_string());
        assert_eq!(reasons, BTreeMap::from([dormant(&claude), dormant(&codex)]));
        // A register that lands while it is quiet is released at the next look.
        let late = register_on(&mut c, claude_session, "late", t1 + WINDOW);
        c.tick(t1 + WINDOW + Duration::from_millis(250));
        assert!(!c.store.snapshot().agents.contains_key(&late));
        // The deck is back: collecting at once, and said so once more.
        let t2 = t1 + WINDOW + Duration::from_secs(60);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t2);
        assert_eq!(
            c.adapters.gate(),
            Gate {
                collect: true,
                usage: false,
                system: false
            }
        );
        assert_eq!(
            c.store.snapshot().service.dormancy,
            Some(DormancyState {
                window_s: 600,
                collecting: true,
                dormant_since: None
            })
        );
        assert_eq!(
            Status::read(&dir.join("status.json")).unwrap().collecting,
            Some(true)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The gate's other two switches follow the deck's strip while it
    /// collects: the account's usage only while the Usage face is on it,
    /// the Mac's load only while the System face is. And a link that is up
    /// but degraded (no hello answered) counts as no deck.
    #[test]
    fn usage_and_the_system_sampler_follow_the_faces_on_the_strip() {
        use crate::dormancy::{Dormancy, Gate, WINDOW};
        use deck_model::{FaceEntry, Template};
        let dir = temp("dormancy-strip");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let wall = c.store.ts(t0);
        c.set_dormancy(Dormancy::start(WINDOW, None, wall, t0), t0);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        let face = |f, t| FaceEntry {
            f,
            t,
            n: String::new(),
        };
        let strip = |c: &mut Core, list: Vec<FaceEntry>| {
            c.on_from_hid(FromHid::Line(DeviceLine::Faces { list }), t0);
            c.tick(t0);
            c.adapters.gate()
        };
        let agents = face(1, Template::Agents);
        assert_eq!(
            strip(&mut c, vec![agents.clone()]),
            Gate {
                collect: true,
                usage: false,
                system: false
            }
        );
        assert_eq!(
            strip(&mut c, vec![agents.clone(), face(4, Template::Usage)]),
            Gate {
                collect: true,
                usage: true,
                system: false
            }
        );
        assert_eq!(
            strip(
                &mut c,
                vec![
                    agents.clone(),
                    face(4, Template::Usage),
                    face(7, Template::System)
                ]
            ),
            Gate::always()
        );
        assert_eq!(
            strip(&mut c, vec![agents, face(7, Template::System)]),
            Gate {
                collect: true,
                usage: false,
                system: true
            }
        );
        // Degraded: the window starts, as for a drop.
        let t1 = t0 + Duration::from_secs(5);
        c.on_from_hid(FromHid::Link(LinkEvent::Degraded), t1);
        c.tick(t1 + WINDOW - Duration::from_secs(1));
        assert!(c.adapters.collecting());
        c.tick(t1 + WINDOW);
        assert_eq!(c.adapters.gate(), Gate::shut());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A core no `run` started, and `--no-device`, have no dormancy: the
    /// gate stays open whatever the link does, and nothing about it is in
    /// the snapshot or `status.json`.
    #[test]
    fn without_dormancy_the_gate_is_always_open() {
        use crate::dormancy::{Dormancy, Gate, WINDOW};
        let dir = temp("dormancy-off");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        c.set_dormancy(Dormancy::off(), t0);
        c.on_from_hid(FromHid::Link(LinkEvent::Down), t0);
        c.tick(t0 + WINDOW * 3);
        assert_eq!(c.adapters.gate(), Gate::always());
        assert_eq!(c.store.snapshot().service.dormancy, None);
        let status = c.status(t0);
        assert_eq!((status.collecting, status.dormant_since), (None, None));
        assert_eq!(
            c.adapters.reader(),
            crate::reader::Reader::all(),
            "no lazy feed either"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R6, the core's half of the lazy reader feed: with dormancy on, the
    /// reader names the sessions the deck reads. A `channel` line moves it
    /// at once, a card on another agent at the next look. A history read
    /// again is taken only from the session that owns the agent. A session
    /// the deck stops reading is still read for a minute; then a Claude
    /// session loses the workflow runs its watcher kept, not its history,
    /// and a connected app keeps the runs it published itself.
    #[test]
    fn the_reader_follows_the_deck_and_a_history_is_replaced_only_by_its_owner() {
        use crate::dormancy::{Dormancy, WINDOW};
        use crate::reader::{Reader, LINGER};
        use deck_model::{AdapterKind, CardClear, CardId, ChatRole, MessageMeta, ReplySet};
        let dir = temp("reader");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let wall = c.store.ts(t0);
        c.set_dormancy(Dormancy::start(WINDOW, None, wall, t0), t0);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        let read = |c: &Core, agents: &[&AgentId]| {
            c.adapters.reader() == Reader::only(agents.iter().map(|a| (*a).clone()).collect())
        };
        assert!(read(&c, &[]), "a deck, and nothing open on it");
        // A Claude session under the adapter's session, as the hook door
        // opens it, and a connected app on a socket of its own.
        let claude = SessionId(91);
        let adapter = Principal::Adapter(AdapterKind::Claude);
        c.sessions.open_internal(claude, adapter, "claude-adapter", t0);
        let (app, _app_rx) = live(&mut c, t0);
        let a = register_on(&mut c, claude, "a", t0);
        let b = register_on(&mut c, app, "b", t0);
        let other = register_on(&mut c, app, "other", t0);
        let slot_a = c.store.snapshot().agents[&a].slot.unwrap();
        c.on_from_hid(FromHid::Line(DeviceLine::Channel { slot: slot_a.0, seq: 1 }), t0);
        assert!(read(&c, &[&a]), "a channel line: at once");
        let principal = Principal::Uid(501);
        let card = c
            .api_call(principal.clone(), app, false, Request::CardSet(testkit::card(&b, "Ship?")), t0)
            .unwrap();
        c.tick(t0);
        assert!(read(&c, &[&a, &b]), "a card on the deck: at the next look");

        // A history read again, with a workflow run in it: from its owner.
        let mut runs = MessageMeta {
            summary: None,
            images: Vec::new(),
            activity: None,
            runs: None,
            plan: None,
            id: "workflow:runs".into(),
            role: ChatRole::Tool,
            streaming: false,
        };
        runs.runs = Some(vec![deck_model::workflow::Run {
            id: "wf_1".into(),
            name: "Checks".into(),
            status: deck_model::workflow::ActivityStatus::Running,
            completed: 0,
            total: 2,
            elapsed_ms: 0,
            tokens: 0,
        }]);
        let history = vec![(runs.clone(), "Checks · running".to_string())];
        let refused = c
            .chat_replace(app, a.clone(), history.clone(), t0)
            .unwrap_err();
        assert_eq!(refused.code, ApiError::CONFLICT, "not the owner's");
        assert!(c.store.snapshot().agents[&a].messages.is_empty());
        let missing = c.chat_replace(claude, AgentId("ag_gone".into()), history.clone(), t0);
        assert_eq!(missing.unwrap_err().code, ApiError::NOT_FOUND);
        c.chat_replace(claude, a.clone(), history, t0).unwrap();
        assert_eq!(c.store.snapshot().agents[&a].active_work(), 1);
        let own_runs = ReplySet {
            agent: b.clone(),
            text: "Checks · running".into(),
            message: Some(runs),
            speak: false,
            announce: None,
            lane: None,
        };
        c.api_call(principal.clone(), app, false, Request::Reply(own_runs), t0)
            .unwrap();
        assert_eq!(c.store.snapshot().agents[&b].active_work(), 1);

        // The deck turns to a third session and B's card goes: both linger a
        // minute, then A loses its runs and keeps its history, and B keeps
        // the runs it published.
        let slot_other = c.store.snapshot().agents[&other].slot.unwrap();
        c.on_from_hid(FromHid::Line(DeviceLine::Channel { slot: slot_other.0, seq: 2 }), t0);
        let clear = CardClear {
            agent: b.clone(),
            card: CardId(u16::try_from(card["card"]["id"].as_u64().unwrap()).unwrap()),
            delivered: false,
            reason: None,
        };
        c.api_call(principal, app, false, Request::CardClear(clear), t0)
            .unwrap();
        let almost = t0 + LINGER - Duration::from_secs(1);
        tick_through(&mut c, t0, almost);
        c.tick(almost);
        assert!(read(&c, &[&a, &b, &other]), "59 s after they left");
        c.tick(t0 + LINGER);
        assert!(read(&c, &[&other]));
        let agents = &c.store.snapshot().agents;
        assert_eq!((agents[&a].active_work(), agents[&a].messages.len()), (0, 1));
        assert_eq!(agents[&b].active_work(), 1, "an app's own runs stay");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two decks on one Mac: each keeps its own strip, settings and keys.
    /// The deck that links is identified before its first hello, the store
    /// takes its profile, the view gets the lines that make it (then
    /// `ProfileReady`, last, which lets the hello go), and `state.json`
    /// keeps both.
    #[test]
    fn two_decks_each_keep_their_own_strip_settings_and_keys() {
        use deck_model::{
            Binding, DeviceSet, FaceEntry, FaceId, MacroKey, Template, Transport,
        };
        const A: &str = "AAAAAAAAAAAA";
        const B: &str = "BBBBBBBBBBBB";
        let dir = temp("two-decks");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t = Instant::now();
        let face = |f: u8, t: Template| FaceEntry {
            f,
            t,
            n: format!("{t:?}"),
        };
        let strip_a = vec![
            face(1, Template::Agents),
            face(3, Template::Macropad),
            face(5, Template::Settings),
        ];
        let strip_b = vec![
            face(1, Template::Agents),
            face(4, Template::Usage),
            face(5, Template::Settings),
        ];
        let hello = |boot: bool, faces: Vec<FaceEntry>| {
            FromHid::Line(DeviceLine::Hello {
                v: 1,
                fw: "0.13.0-openwalkietalkie".into(),
                sid: 0,
                boot,
                rx: deck_model::LINE_MAX_U16,
                agents: None,
                lang: None,
                lang_rev: None,
                faces,
            })
        };
        let link = |c: &mut Core, serial: &str| {
            c.on_from_hid(FromHid::Link(LinkEvent::Linked), t);
            c.on_from_hid(
                FromHid::Report(DeviceReport::Transport(Some(Transport::Usb))),
                t,
            );
            c.on_from_hid(FromHid::Identity(Some(serial.into())), t);
        };
        let lose = |c: &mut Core| {
            let why = "removed".to_string();
            c.on_from_hid(FromHid::Report(DeviceReport::Dropped { why }), t);
            c.on_from_hid(FromHid::Link(LinkEvent::Down), t);
        };
        let ready = DeviceOp::Control(Control::ProfileReady);

        // Deck A, the first deck identified here, with its own strip; then
        // its speaker and its Macropad's keys from the app.
        link(&mut c, A);
        assert_eq!(ops_of(&ops).last(), Some(&ready));
        let label_a = c.store.snapshot().device.deck.clone().expect("A is named");
        c.on_from_hid(hello(false, strip_a.clone()), t);
        let principal = Principal::Uid(501);
        let speaker = |v| Request::DeviceSet(DeviceSet { key: SetKey::Speaker, value: v });
        c.api_call(principal.clone(), SessionId(2), false, speaker(3), t)
            .expect("device.set");
        let build = MacroKey {
            label: "build".into(),
            binding: Some(Binding::Left),
            keys: Some("cmd+b".into()),
        };
        let keys = Request::DeviceMacroSet {
            f: 3,
            layout: "Build".into(),
            src: None,
            keys: vec![build],
        };
        c.api_call(principal, SessionId(2), false, keys, t)
            .expect("device.macro.set");
        lose(&mut c);
        ops_of(&ops);

        // Deck B, new here: the firmware's defaults and no strip, so the
        // `boot:1` sync leaves it its own, which is then remembered.
        link(&mut c, B);
        let sent = ops_of(&ops);
        assert!(sent.contains(&DeviceOp::Remove(Key::Faces)), "{sent:?}");
        assert!(sent.contains(&DeviceOp::Put(
            Key::Set(SetKey::Speaker),
            HostLine::Set {
                key: SetKey::Speaker,
                value: 6
            }
        )));
        assert!(sent.contains(&DeviceOp::Remove(Key::Macro(FaceId(3)))));
        assert_eq!(sent.last(), Some(&ready), "the hello may go only now");
        let snap = c.store.snapshot();
        assert_ne!(snap.device.deck.as_ref(), Some(&label_a));
        assert!(snap.faces.is_empty() && snap.macros.is_empty());
        assert_eq!(snap.settings.speaker, 6);
        c.on_from_hid(hello(true, strip_b.clone()), t);
        assert_eq!(c.store.snapshot().faces, strip_b);
        lose(&mut c);
        ops_of(&ops);

        // Deck A again: its own strip, speaker and keys come back.
        link(&mut c, A);
        let sent = ops_of(&ops);
        assert!(sent.contains(&DeviceOp::Put(
            Key::Faces,
            HostLine::Faces {
                list: strip_a.clone()
            }
        )));
        assert!(sent.contains(&DeviceOp::Put(
            Key::Set(SetKey::Speaker),
            HostLine::Set {
                key: SetKey::Speaker,
                value: 3
            }
        )));
        assert!(sent
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Macro(FaceId(3)), _))));
        assert_eq!(sent.last(), Some(&ready));
        let snap = c.store.snapshot();
        assert_eq!(snap.device.deck.as_ref(), Some(&label_a));
        assert_eq!((snap.faces.clone(), snap.settings.speaker), (strip_a.clone(), 3));
        assert_eq!(snap.macros[&3].keys[0].label, "build");
        // The same deck again is no news: only the word for the hello.
        lose(&mut c);
        ops_of(&ops);
        link(&mut c, A);
        assert_eq!(ops_of(&ops), vec![ready.clone()]);

        c.flush_state();
        let (saved, _) = State::load(&dir.join("state.json"));
        assert_eq!(saved.deck.as_deref(), Some(A));
        assert_eq!(saved.decks.len(), 2);
        assert_eq!(saved.decks[A].faces, strip_a);
        assert_eq!(saved.decks[A].settings.speaker, 3);
        assert_eq!(saved.decks[A].macropad_bindings[&3], "Build");
        assert_eq!(saved.decks[B].faces, strip_b);
        assert_eq!(saved.decks[B].settings.speaker, 6);
        assert!(saved.decks[B].macros.is_empty());
        // The per-deck settings name a deck by its label only (§6.9); the
        // voice link's cable port and the firmware updater are not in this core.
        let snapshot = serde_json::to_string(c.store.snapshot()).unwrap();
        assert!(!snapshot.contains(A) && !snapshot.contains(B));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Bluetooth stack that spelled the deck's address the other way
    /// round made its record under the wrong id; the cable names it for
    /// good, and `state.json` is written again even though it is the same
    /// deck.
    #[test]
    fn the_cable_names_for_good_a_deck_a_bluetooth_spelling_made() {
        let dir = temp("rekey");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t = Instant::now();
        c.on_identity(Some("C0:FF:EE:15:60:0D"), t);
        assert_eq!(c.state.deck.as_deref(), Some("0D6015EEFFC0"));
        c.flush_state();
        assert!(c.state_dirty.is_none());
        ops_of(&ops);
        c.on_identity(Some("C0FFEE15600D"), t);
        assert_eq!(c.state.deck.as_deref(), Some("C0FFEE15600D"));
        assert!(c.state_dirty.is_some(), "the new id is written");
        assert_eq!(
            c.store.snapshot().device.deck,
            Some(crate::device::identity::label("C0FFEE15600D"))
        );
        assert_eq!(
            ops_of(&ops),
            vec![DeviceOp::Control(Control::ProfileReady)],
            "the same strip, settings and keys: nothing to send"
        );
        c.tick(t + STATE_DEBOUNCE);
        let (saved, _) = State::load(&dir.join("state.json"));
        assert_eq!(saved.deck.as_deref(), Some("C0FFEE15600D"));
        assert_eq!(saved.decks.keys().collect::<Vec<_>>(), vec!["C0FFEE15600D"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A deck that lists no serial number is linked exactly as before: the
    /// store hears nothing, the view gets nothing, and the hid thread was
    /// never told to wait.
    #[test]
    fn a_deck_with_no_serial_changes_nothing() {
        let dir = temp("no-serial");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t = Instant::now();
        c.on_from_hid(FromHid::Link(LinkEvent::Linked), t);
        ops_of(&ops);
        let before = c.store.snapshot().clone();
        for serial in [None, Some("000000000000".to_string())] {
            c.on_from_hid(FromHid::Identity(serial), t);
            assert!(ops_of(&ops).is_empty());
            assert_eq!(c.store.snapshot(), &before);
            assert_eq!((c.state.deck.as_ref(), c.state.decks.len()), (None, 0));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn metrics_get_waits_for_a_fresh_report() {
        let dir = temp("metric");
        let (mut c, _ops, lines, debug) = detached(&dir);
        let t0 = Instant::now();
        let feeder = std::thread::spawn(move || {
            assert_eq!(debug.recv().unwrap(), HidDebug::Metric);
            lines
                .send(FromHid::Line(DeviceLine::Face { f: 3 }))
                .unwrap();
            lines
                .send(FromHid::Metric(Metric {
                    lines_sent: 9,
                    ..Metric::default()
                }))
                .unwrap();
        });
        let admin = testkit::ctx(2, &[deck_model::Scope::Admin]);
        let r = c.call(admin, "metrics.get", serde_json::Value::Null, t0);
        assert_eq!(r.result.unwrap()["lines_sent"], 9);
        assert_eq!(
            c.store.snapshot().device.face,
            Some(3),
            "other messages handled meanwhile"
        );
        feeder.join().unwrap();
        assert_eq!(c.status(t0).face, Some(3));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn the_core_thread_round_trips_an_api_call_and_publishes_to_the_bus() {
        let live = testkit::LiveCore::start("thread-api");
        let handle = live.handle.clone();
        let mut sub = handle.bus().subscribe();
        let rev0 = handle.bus().rev();
        let result = rt()
            .block_on(handle.api(
                Principal::Uid(501),
                SessionId(2),
                false,
                Request::AgentRegister(testkit::register("bot", Some("k:bot"))),
            ))
            .unwrap();
        let id: AgentId = serde_json::from_value(result["agent"].clone()).unwrap();
        assert_eq!(result["owner"]["session"], "sess_2");
        assert_eq!(result["slot"], 1);
        // The events reached the bus with the owner, and the cell moved.
        let added = rt()
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), sub.recv()).await })
            .unwrap()
            .unwrap();
        assert_eq!(added.event.kind, EventKind::AgentAdded);
        assert_eq!(added.owner, Some(SessionId(2)));
        assert!(handle.bus().rev() > rev0);
        assert!(handle.bus().snapshot().agents.contains_key(&id));
        let (ring, gap) = handle.bus().replay(rev0, 10, None);
        assert!(!gap);
        assert!(ring.iter().any(|e| e.kind == EventKind::AgentAdded));
        // The op reached the hid channel too.
        let ops: Vec<DeviceOp> = live.ops.try_iter().collect();
        assert!(ops
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::AgentState(Slot(1)), _))));
        // A refused request is an error, not a panic.
        let err = rt()
            .block_on(handle.api(
                Principal::Uid(501),
                SessionId(2),
                false,
                Request::AgentClaim { key: "nope".into() },
            ))
            .unwrap_err();
        assert_eq!(err.code, deck_model::ApiError::NOT_FOUND);
    }

    #[test]
    fn two_open_sessions_get_two_ids_and_the_same_key_conflicts_between_them() {
        let live = testkit::LiveCore::start("thread-sessions");
        let handle = live.handle.clone();
        let rt = rt();
        let open = |name: &str| OpenSession {
            kind: SessionKind::Observer,
            principal: Principal::Uid(501),
            scopes: vec![deck_model::Scope::Agent],
            trusted: false,
            client: ClientInfo {
                name: name.into(),
                version: "1".into(),
            },
        };
        let a = rt.block_on(handle.open_session(open("a"))).unwrap();
        let b = rt.block_on(handle.open_session(open("b"))).unwrap();
        assert_eq!((a, b), (SessionId(2), SessionId(3)));
        let register = |s: SessionId| {
            rt.block_on(handle.api(
                Principal::Uid(501),
                s,
                false,
                Request::AgentRegister(testkit::register("bot", Some("k:bot"))),
            ))
        };
        let first = register(a).unwrap();
        let again = register(a).unwrap();
        assert_eq!(
            first["agent"], again["agent"],
            "same key + same session: idempotent"
        );
        let err = register(b).unwrap_err();
        assert_eq!(err.code, deck_model::ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["session"], "sess_2");
        assert_eq!(err.data["owner"]["principal"], "uid:501");
        // A logical session is another id; closing `a` orphans its agent.
        let l = rt
            .block_on(handle.logical_session(
                "hooks".into(),
                "bot".into(),
                Principal::Token("hooks".into()),
                vec![deck_model::Scope::Agent],
            ))
            .unwrap();
        assert_eq!(l, SessionId(4));
        handle.close_session(a);
        let deadline = Instant::now() + Duration::from_secs(2);
        let id = AgentId(first["agent"].as_str().unwrap().into());
        loop {
            let snap = handle.bus().snapshot();
            let orphaned = snap
                .agents
                .get(&id)
                .is_some_and(|ag| ag.state == AgentState::Idle);
            if orphaned || Instant::now() > deadline {
                assert!(orphaned, "{:?}", snap.agents.get(&id).map(|a| a.state));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let ping =
            rt.block_on(handle.call(testkit::ctx(3, &[]), "session.ping", serde_json::json!({})));
        assert!(ping.result.unwrap()["ts"].is_string());
    }

    fn deliver(agent: &str, deliver: Deliver) -> Effect {
        Effect::Deliver {
            agent: AgentId(agent.into()),
            card: CardId(1),
            answer: deck_model::Answer {
                choice: Some(0),
                label: None,
                text: None,
                source: deck_model::AnswerSource::Deck,
                principal: deck_model::Principal::Device,
                session: None,
                utterance: None,
            },
            deliver,
        }
    }

    #[test]
    fn delivery_effects_resolve_and_the_overflow_reprojects_the_store() {
        let dir = temp("deliver");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        c.enable_demo(t0);
        let demo_agent = c.store.snapshot().agent_at(Slot(2)).unwrap().id.0.clone();
        let delivered = |c: &mut Core, effect: Effect| -> bool {
            c.execute(effect, t0);
            let ok = matches!(
                c.followups.front(),
                Some(Command::Delivered { ok: true, .. })
            );
            c.followups.clear();
            ok
        };
        assert!(delivered(&mut c, deliver("ag_x", Deliver::None)));
        assert!(delivered(&mut c, deliver("ag_x", Deliver::Focus)));
        assert!(!delivered(&mut c, deliver("ag_x", Deliver::Hold)));
        assert!(
            !delivered(&mut c, deliver("ag_x", Deliver::Poll)),
            "no mailbox for a foreign agent yet"
        );
        assert!(
            delivered(&mut c, deliver(&demo_agent, Deliver::Poll)),
            "the demo client picks up its own"
        );
        assert!(delivered(&mut c, deliver(&demo_agent, Deliver::Push)));
        assert!(!delivered(&mut c, deliver(&demo_agent, Deliver::Hold)));
        c.execute(deliver("ag_x", Deliver::Hold), t0);
        c.step(Command::Tick(t0), t0);
        assert!(c.followups.is_empty(), "follow-ups drained by step");
        c.execute(
            Effect::ReleaseHold {
                agent: AgentId("ag_x".into()),
                card: deck_model::CardId(1),
                reason: deck_model::Release::Snooze,
            },
            t0,
        );
        c.execute(
            Effect::Desktop(deck_model::DesktopCmd::Focus {
                agent: deck_model::AgentId("ag_1".into()),
            }),
            t0,
        );
        c.execute(
            Effect::Audio(deck_model::AudioCmd::Chime {
                which: "question".into(),
            }),
            t0,
        );
        // Overflow: the channel is full, the core drops and re-projects once it drains.
        ops_of(&ops);
        for _ in 0..crate::hid::OPS_CAPACITY {
            let _ = c.hid.ops.try_send(DeviceOp::Control(Control::Hello));
        }
        c.send_op(DeviceOp::Control(Control::Hello));
        assert!(c.needs_resync);
        c.tick(t0);
        assert!(c.needs_resync, "still full");
        ops_of(&ops);
        c.tick(t0);
        assert!(!c.needs_resync);
        let reprojected = ops_of(&ops);
        assert!(matches!(
            reprojected.last(),
            Some(DeviceOp::Sync {
                reason: deck_model::SyncReason::Overflow,
                ..
            })
        ));
        // The whole roster is in the store's projection, ws/macro included.
        for key in [
            Key::AgentState(Slot(1)),
            Key::AgentState(Slot(5)),
            Key::Card(Slot(1), 1),
            Key::Reply(Slot(3)),
            Key::Macro(deck_model::FaceId(3)),
            Key::Usage,
            Key::Ws,
        ] {
            assert!(
                reprojected
                    .iter()
                    .any(|op| matches!(op, DeviceOp::Put(k, _) if *k == key)),
                "{key:?} missing"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A card answered while the core→hid channel is full: the `CardClear`
    /// and `Remove(Card)` are not `Put`s, so the re-projection alone would
    /// leave the ghost card on the device (§2.3 promises one barrier at most).
    #[test]
    fn an_overflow_replays_the_removals_it_refused_before_re_projecting() {
        use crate::hid::tests::{cfg, feed, written, HELLO_BOOT};
        let dir = temp("overflow");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let mut t = t0;
        c.enable_demo(t0);
        let mut link = crate::hid::Link::new(&cfg());
        for op in ops_of(&ops) {
            link.apply(op, t);
        }
        link.link_up(t);
        written(&mut link, &mut t);
        feed(&mut link, HELLO_BOOT, t);
        let barrier = written(&mut link, &mut t);
        assert!(barrier
            .iter()
            .any(|l| l.starts_with("{\"m\":\"card\",\"agent\":1,\"id\":1,")));
        assert_eq!(barrier.last().unwrap(), "{\"m\":\"synced\"}");
        // A filler the link ignores (`Key::Hello` is never a view key).
        let filler = || DeviceOp::Put(Key::Hello, HostLine::Hello { sid: 0, hm: None });
        let no_hello = |lines: Vec<String>| -> Vec<String> {
            lines
                .into_iter()
                .filter(|l| !l.starts_with("{\"m\":\"hello\""))
                .collect()
        };
        // The hid thread stalls: the channel fills, the core drops.
        for _ in 0..crate::hid::OPS_CAPACITY {
            let _ = c.hid.ops.try_send(filler());
        }
        c.send_op(filler());
        assert!(c.needs_resync);
        let events = c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card: 1,
                choice: 0,
            },
            t,
        );
        assert!(events.iter().any(|e| e.kind == EventKind::CardCleared));
        assert!(c.store.snapshot().agent_at(Slot(1)).unwrap().card.is_none());
        assert!(c.ops_dropped >= 2);
        // The stall ends: everything the hid thread gets, in order.
        for op in ops_of(&ops) {
            link.apply(op, t);
        }
        c.tick(t);
        assert!(!c.needs_resync);
        let after = ops_of(&ops);
        let clear_at = after
            .iter()
            .position(|op| *op == DeviceOp::Control(Control::CardClear(Slot(1), 1)))
            .expect("the refused clear is replayed");
        let remove_at = after
            .iter()
            .position(|op| *op == DeviceOp::Remove(Key::Card(Slot(1), 1)))
            .expect("the refused remove is replayed");
        let first_put = after
            .iter()
            .position(|op| matches!(op, DeviceOp::Put(..)))
            .unwrap();
        assert!(clear_at < remove_at && remove_at < first_put, "{after:?}");
        assert!(matches!(
            after.last(),
            Some(DeviceOp::Sync {
                reason: deck_model::SyncReason::Overflow,
                ..
            })
        ));
        for op in after {
            link.apply(op, t);
        }
        let lines = no_hello(written(&mut link, &mut t));
        assert_eq!(
            lines[0],
            "{\"m\":\"card\",\"agent\":1,\"id\":1,\"clear\":1}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("{\"m\":\"card\",\"agent\":1,\"id\":1,\"type\"")),
            "the answered card is not re-sent: {lines:?}"
        );
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
        assert!(link.view().get(&Key::Card(Slot(1), 1)).is_none());
        let status = c.status(t);
        assert_eq!(status.overflows, 1);
        assert!(status.ops_dropped >= 2);
        // The side queue itself overflowing: every slot is rebuilt instead.
        for _ in 0..crate::hid::OPS_CAPACITY {
            let _ = c.hid.ops.try_send(filler());
        }
        c.send_op(filler());
        for _ in 0..=DROPPED_CAPACITY {
            c.send_op(DeviceOp::Remove(Key::Reply(Slot(3))));
        }
        assert!(c.dropped_overflow);
        ops_of(&ops);
        c.tick(t);
        let after = ops_of(&ops);
        let removes: Vec<&DeviceOp> = after
            .iter()
            .take_while(|op| matches!(op, DeviceOp::Remove(_)))
            .collect();
        assert_eq!(removes.len(), usize::from(Slot::DEFAULT) + 1, "{removes:?}");
        assert_eq!(removes[0], &DeviceOp::Remove(Key::AgentState(Slot(1))));
        assert_eq!(removes[8], &DeviceOp::Remove(Key::Focus));
        assert!(c.dropped.is_empty() && !c.dropped_overflow);
        for op in after {
            link.apply(op, t);
        }
        let lines = no_hello(written(&mut link, &mut t));
        assert_eq!(lines[0], "{\"m\":\"agent\",\"id\":1,\"gone\":1}");
        assert!(lines
            .iter()
            .any(|l| l.starts_with("{\"m\":\"agent\",\"id\":1,\"kind\"")));
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A live connection whose `Outbound`s the test reads.
    fn live(c: &mut Core, now: Instant) -> (SessionId, tokio::sync::mpsc::Receiver<Outbound>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let id = c.sessions.open(
            OpenSession {
                kind: SessionKind::Live { tx },
                principal: Principal::Uid(501),
                scopes: vec![deck_model::Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            },
            now,
        );
        (id, rx)
    }

    /// Register an agent on `session` and put it to work, so that losing
    /// its lease is visible as the orphan's `idle` (`is_orphaned` is
    /// `pub(crate)` inside `deck-model`).
    fn register_on(c: &mut Core, session: SessionId, name: &str, now: Instant) -> AgentId {
        let principal = c.sessions.get(session).unwrap().principal.clone();
        let r = c
            .api_call(
                principal.clone(),
                session,
                false,
                Request::AgentRegister(testkit::register(name, Some(&format!("k:{name}")))),
                now,
            )
            .unwrap();
        let id: AgentId = serde_json::from_value(r["agent"].clone()).unwrap();
        c.api_call(
            principal,
            session,
            false,
            Request::AgentUpdate(deck_model::AgentUpdate {
                agent: id.clone(),
                state: Some(AgentState::Working),
                title: Some(name.into()),
                title_source: None,
                since: None,
                name: None,
                meta: None, task: None, subagents: None,
            }),
            now,
        )
        .unwrap();
        id
    }

    /// The orphan marker: the lease lapsed, so the agent went `idle`.
    fn orphaned(c: &Core, id: &AgentId) -> bool {
        c.store.snapshot().agents[id].state == AgentState::Idle
    }

    /// §3.2: every 10 s the core refreshes the lease of every agent owned by
    /// a connected (or internal) session; a logical HTTP session must call
    /// `agent.heartbeat` itself, so its agent is orphaned at the 30 s lease.
    #[test]
    fn the_implicit_heartbeat_keeps_connected_leases_alive_but_not_logical_ones() {
        let dir = temp("heartbeat");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (conn, _rx) = live(&mut c, t0);
        let logical = c.sessions.logical(
            "tok".into(),
            "bot".into(),
            Principal::Token("tok".into()),
            vec![deck_model::Scope::Agent],
            t0,
        );
        let kept = register_on(&mut c, conn, "kept", t0);
        let lost = register_on(&mut c, logical, "lost", t0);
        assert_eq!(c.sessions.heartbeat_sessions(), vec![conn]);
        let mut t = t0;
        for _ in 0..40 {
            t += Duration::from_secs(1);
            c.tick(t);
        }
        assert!(!orphaned(&c, &kept), "beaten every 10 s");
        assert!(orphaned(&c, &lost), "a logical session beats itself");
        // An explicit heartbeat from the logical session renews its lease.
        let renewed = c
            .api_call(
                Principal::Token("tok".into()),
                logical,
                false,
                Request::AgentHeartbeat {
                    agents: vec![lost.clone()],
                },
                t,
            )
            .unwrap();
        assert_eq!(
            renewed["expires"],
            serde_json::json!(c.store.ts(t).plus(Duration::from_secs(30)))
        );
        // The beat is not more often than every 10 s.
        let before = c.store.snapshot().agents[&kept].owner.lease_expires;
        c.tick(t + Duration::from_millis(250));
        assert_eq!(
            c.store.snapshot().agents[&kept].owner.lease_expires,
            before,
            "no beat within the interval"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// §3.7: a reload hot-applies the store's own timings and pins too,
    /// not only the core's `[trust]` key — and never the store's identity.
    #[test]
    fn a_reload_hot_applies_the_store_timings_and_the_pins() {
        let dir = temp("hot-store-keys");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        assert_eq!(c.store.config().grace_s, 300, "the §9 default");
        let mut cfg = crate::config::Config::default();
        cfg.agents.grace_s = 42;
        cfg.cards.ready_ttl_s = 7;
        cfg.hooks.claude.hold_s = 9;
        cfg.agents.pins.insert(
            "claude:cohdl-api".into(),
            crate::config::Pin {
                slot: Some(4),
                ..Default::default()
            },
        );
        cfg.trust.require_trusted_for_allow = false;
        c.set_config(Arc::new(cfg));
        assert_eq!(c.store.config().grace_s, 42);
        assert_eq!(c.store.config().ready_ttl_s, 7);
        assert_eq!(c.store.config().hold_s, 9);
        assert_eq!(
            c.store.config().pins["claude:cohdl-api"].slot,
            Some(deck_model::Slot(4))
        );
        assert!(!c.guard.require_trusted);
        // The identity the store was built with survives a reload.
        assert_eq!(c.store.config().sid, 7);
        assert_eq!(c.store.config().wall, Ts(1_789_760_523_000));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A logical session closes `mailbox_ttl_s` after its last call, which
    /// is a `Command::SessionClosed`: its agents are orphaned and its mail
    /// goes with it (§3.2).
    #[test]
    fn a_logical_session_expires_after_the_mailbox_ttl_and_closes() {
        let dir = temp("logical-expiry");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        assert_eq!(c.config().api.mailbox_ttl_s, 600, "the §9 default");
        // A 2 s ttl for the test, hot-applied exactly like a reload.
        let mut cfg = crate::config::Config::default();
        cfg.api.mailbox_ttl_s = 2;
        c.set_config(Arc::new(cfg));
        let ttl = Duration::from_secs(2);
        let logical = c.sessions.logical(
            "tok".into(),
            "bot".into(),
            Principal::Token("tok".into()),
            vec![deck_model::Scope::Agent],
            t0,
        );
        let id = register_on(&mut c, logical, "bot", t0);
        c.tick(t0);
        assert!(c.sessions.contains(logical));
        // A call keeps it alive; silence past the ttl closes it.
        let call_at = t0 + Duration::from_secs(1);
        c.api_call(
            Principal::Token("tok".into()),
            logical,
            false,
            Request::AgentHeartbeat {
                agents: vec![id.clone()],
            },
            call_at,
        )
        .unwrap();
        c.tick(t0 + ttl + Duration::from_millis(500));
        assert!(c.sessions.contains(logical), "refreshed by the call");
        c.tick(call_at + ttl + Duration::from_millis(500));
        assert!(!c.sessions.contains(logical));
        assert!(orphaned(&c, &id), "SessionClosed orphaned it");
        // A live connection never expires, however long it stays silent.
        let (conn, _rx) = live(&mut c, t0);
        c.tick(call_at + Duration::from_secs(60));
        assert!(c.sessions.contains(conn));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A `push` card's answer is written to the owner's connection as an
    /// `agent.answer` notification, never to the bus, and the `card clear`
    /// waits for the door's `Delivered` (§3.2, §4.8).
    #[test]
    fn a_push_answer_goes_to_the_owner_connection_and_clears_on_the_doors_confirmation() {
        let dir = temp("push");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (conn, mut outbound) = live(&mut c, t0);
        let mut reg = testkit::register("pusher", Some("k:pusher"));
        reg.caps.answer = deck_model::AnswerCap::Push;
        let id: AgentId = serde_json::from_value(
            c.api_call(
                Principal::Uid(501),
                conn,
                false,
                Request::AgentRegister(reg),
                t0,
            )
            .unwrap()["agent"]
                .clone(),
        )
        .unwrap();
        let mut set = testkit::card(&id, "ship it?");
        set.deliver = Deliver::Push;
        let card = testkit::card_id(
            &c.api_call(Principal::Uid(501), conn, false, Request::CardSet(set), t0)
                .unwrap(),
        );
        let _ = ops.try_iter().count();
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            Request::OpAnswer(deck_model::OpAnswer {
                agent: id.clone(),
                card,
                choice: Some(0),
                text: None,
            }),
            t0,
        )
        .unwrap();
        let out = outbound.try_recv().expect("the owner was written to");
        let Outbound::Deliver { line, delivery } = out else {
            panic!("{out:?}");
        };
        assert_eq!(delivery.agent, id);
        assert_eq!(delivery.card, card);
        let line: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(line["method"], "event");
        assert_eq!(line["params"]["type"], "agent.answer");
        assert_eq!(line["params"]["data"]["choice"], 0);
        assert_eq!(line["params"]["data"]["label"], "Yes");
        // Nothing cleared yet, and `agent.answer` is owner-only.
        c.publish();
        let (ring, _) = c.bus.replay(0, 500, None);
        assert!(!ring.iter().any(|e| e.kind == EventKind::AgentAnswer));
        assert!(!ops
            .try_iter()
            .any(|op| matches!(op, DeviceOp::Control(Control::CardClear(..)))));
        // The door flushed: the card is delivered, and a `push` owner sends
        // its own `agent.card.clear` within the 4 s budget (§4.8).
        c.step(
            Command::Delivered {
                agent: id.clone(),
                card,
                ok: true,
            },
            t0,
        );
        assert_eq!(
            c.store.snapshot().agents[&id]
                .card
                .as_ref()
                .map(|c| &c.status),
            Some(&deck_model::CardStatus::Delivered)
        );
        c.api_call(
            Principal::Uid(501),
            conn,
            false,
            Request::CardClear(deck_model::CardClear {
                agent: id.clone(),
                card,
                delivered: true,
                reason: None,
            }),
            t0,
        )
        .unwrap();
        assert!(ops
            .try_iter()
            .any(|op| matches!(op, DeviceOp::Control(Control::CardClear(..)))));
        assert!(c.store.snapshot().agents[&id].card.is_none());
        // A push owner that is gone cannot take a second answer.
        let mut again = testkit::card(&id, "again?");
        again.deliver = Deliver::Push;
        let card = testkit::card_id(
            &c.api_call(
                Principal::Uid(501),
                conn,
                false,
                Request::CardSet(again),
                t0,
            )
            .unwrap(),
        );
        drop(outbound);
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            Request::OpAnswer(deck_model::OpAnswer {
                agent: id.clone(),
                card,
                choice: Some(1),
                text: None,
            }),
            t0,
        )
        .unwrap();
        assert!(matches!(
            c.store.snapshot().agents[&id]
                .card
                .as_ref()
                .map(|c| &c.status),
            Some(deck_model::CardStatus::Failed { .. })
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Design §4.9: `op.answer` returns `{delivered, via, reason?}` **after
    /// delivery or 4 s**. `deck-model` answers before the executor has run,
    /// so the core parks the reply: it is answered inside the call when the
    /// delivery already failed, on the door's `Delivered` for a route that
    /// has one, and at the store's budget when nothing ever confirms.
    #[test]
    fn op_answer_answers_after_the_delivery_or_the_budget() {
        let dir = temp("answer-wait");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (conn, mut outbound) = live(&mut c, t0);
        let id = register_on(&mut c, conn, "waiter", t0);
        let answer_msg = |card: CardId, reply| CoreMsg::Api {
            principal: Principal::Uid(501),
            session: SessionId(9),
            trusted: false,
            req: Box::new(Request::OpAnswer(deck_model::OpAnswer {
                agent: id.clone(),
                card,
                choice: Some(0),
                text: None,
            })),
            reply,
        };
        let set = |c: &mut Core, deliver| {
            let mut set = testkit::card(&id, "ship it?");
            set.deliver = deliver;
            testkit::card_id(
                &c.api_call(Principal::Uid(501), conn, false, Request::CardSet(set), t0)
                    .unwrap(),
            )
        };

        // 1. `push` to a live owner: parked until the door reports the flush.
        let card = set(&mut c, Deliver::Push);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.on_msg(answer_msg(card, tx), t0);
        assert_eq!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        assert_eq!(c.answers.len(), 1, "parked until the delivery is known");
        assert!(matches!(outbound.try_recv(), Ok(Outbound::Deliver { .. })));
        c.on_msg(
            CoreMsg::Delivered {
                agent: id.clone(),
                card,
                ok: true,
            },
            t0,
        );
        let v = rx.try_recv().expect("answered on the flush").unwrap();
        assert_eq!(v["delivered"], true, "{v}");
        assert_eq!(v["via"], "push");
        assert!(v["reason"].is_null());
        assert!(c.answers.is_empty());
        c.api_call(
            Principal::Uid(501),
            conn,
            false,
            Request::CardClear(deck_model::CardClear {
                agent: id.clone(),
                card,
                delivered: true,
                reason: None,
            }),
            t0,
        )
        .unwrap();

        // 2. `poll` with nobody waiting: the answer sits in the owner's
        //    mailbox and the store's 4 s budget is what answers the caller
        //    (§4.8, "no waiter within 4 s → failed").
        let card = set(&mut c, Deliver::Poll);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.on_msg(answer_msg(card, tx), t0);
        assert_eq!(c.answers.len(), 1);
        c.tick(t0 + Duration::from_secs(1));
        assert_eq!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        c.tick(t0 + deck_model::DELIVERY_BUDGET);
        let v = rx.try_recv().expect("answered at the budget").unwrap();
        assert_eq!(v["delivered"], false, "{v}");
        assert_eq!(v["via"], "poll");
        assert_eq!(v["reason"], "timeout");
        assert!(c.answers.is_empty());

        // 3. `push` whose owner's connection is gone: the executor fails the
        //    delivery inside the call, so the reply is written at once.
        drop(outbound);
        let card = set(&mut c, Deliver::Push);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.on_msg(answer_msg(card, tx), t0);
        let v = rx.try_recv().expect("answered inside the call").unwrap();
        assert_eq!(v["delivered"], false, "{v}");
        assert_eq!(v["via"], "push");
        assert_eq!(v["reason"], "undeliverable");
        assert!(c.answers.is_empty());

        // 4. A refusal is the whole answer: nothing is ever parked for it.
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.on_msg(answer_msg(CardId(999), tx), t0);
        assert!(rx.try_recv().expect("answered at once").is_err());
        assert!(c.answers.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The guard inside the core (§4.2): an untrusted allow on the demo's
    /// permission card is `-32002 {need:"trusted"}` plus `op.confirm_requested`
    /// on the bus; a deny goes through and resolves the hold.
    #[test]
    fn an_untrusted_allow_on_a_permission_card_is_refused_with_a_confirmation_request() {
        let dir = temp("guarded");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        c.enable_demo(t0);
        let agent = c
            .store
            .snapshot()
            .agent_at(Slot(crate::demo::PERMISSION_SLOT))
            .unwrap()
            .clone();
        let card = agent.card.as_ref().unwrap().id;
        assert!(c.holds.contains(&agent.id, card));
        c.publish();
        let before = c.bus.ring_len();
        let answer = |choice: u8| {
            Request::OpAnswer(deck_model::OpAnswer {
                agent: agent.id.clone(),
                card,
                choice: Some(choice),
                text: None,
            })
        };
        let err = c
            .api_call(Principal::Uid(501), SessionId(9), false, answer(0), t0)
            .unwrap_err();
        assert_eq!(err.code, deck_model::ApiError::FORBIDDEN);
        assert_eq!(err.data["need"], "trusted");
        assert_eq!(err.data["how"], "gui_confirm");
        c.publish();
        let (ring, _) = c.bus.replay(0, 500, None);
        let confirm = ring
            .iter()
            .find(|e| e.kind == EventKind::OpConfirmRequested)
            .expect("op.confirm_requested is on the bus");
        assert_eq!(confirm.data["agent"], agent.id.0);
        assert_eq!(confirm.data["need"], "trusted");
        assert!(c.bus.ring_len() > before);
        assert!(c.holds.contains(&agent.id, card), "the hold is untouched");
        // A token is refused the same way; a trusted principal is not.
        assert_eq!(
            c.api_call(
                Principal::Token("mcp".into()),
                SessionId(9),
                false,
                answer(1),
                t0
            )
            .unwrap_err()
            .code,
            deck_model::ApiError::FORBIDDEN
        );
        // Deny (choice 2) is never guarded: it resolves the parked hold.
        c.api_call(Principal::Uid(501), SessionId(9), false, answer(2), t0)
            .unwrap();
        assert!(!c.holds.contains(&agent.id, card), "the hold was answered");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Contract §4: the demo owns the `HoldRx` of its permission card,
    /// treats a resolution as written (so the deck's card clears) and sets
    /// the card again `REPUSH_AFTER` later, under a new id and parked again.
    #[test]
    fn the_demo_re_sets_its_permission_card_three_seconds_after_a_resolution() {
        let dir = temp("demo-hold");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        c.enable_demo(t0);
        let agent = c
            .store
            .snapshot()
            .agent_at(Slot(crate::demo::PERMISSION_SLOT))
            .unwrap()
            .id
            .clone();
        let first = c.store.snapshot().agents[&agent].card.as_ref().unwrap().id;
        assert!(c.holds.contains(&agent, first));
        // Deny from the deck resolves the parked hold at once …
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            Request::OpAnswer(deck_model::OpAnswer {
                agent: agent.clone(),
                card: first,
                choice: Some(2),
                text: None,
            }),
            t0,
        )
        .unwrap();
        assert!(!c.holds.contains(&agent, first));
        assert!(c.store.snapshot().agents[&agent].card.is_some(), "not yet");
        // … and the demo, on its next tick, reports it written, which clears
        // the card well inside the 4 s budget.
        let resolved = t0 + TICK;
        c.tick(resolved);
        assert!(
            c.store.snapshot().agents[&agent].card.is_none(),
            "hold_written cleared it"
        );
        c.tick(resolved + crate::demo::REPUSH_AFTER - Duration::from_millis(1));
        assert!(
            c.store.snapshot().agents[&agent].card.is_none(),
            "not before the re-push is due"
        );
        c.tick(resolved + crate::demo::REPUSH_AFTER);
        let again = c.store.snapshot().agents[&agent]
            .card
            .as_ref()
            .expect("the card is back")
            .clone();
        assert_ne!(again.id, first, "a new card id, so the deck chimes again");
        assert_eq!(again.deliver, Deliver::Hold);
        assert_eq!(again.origin.kind, deck_model::OriginKind::Permission);
        assert!(c.holds.contains(&agent, again.id), "parked again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// §3.2: `agent.order` and a snooze belong to the owning session alone —
    /// a notification on its connection when it pushes, a mail its
    /// `agent.wait` picks up when it polls — and never reach the bus, where
    /// only `order.dispatched`/`card.snoozed` are visible.
    #[test]
    fn a_transcription_offer_reaches_its_owner_alone_and_only_it_may_answer() {
        use base64::Engine;
        let dir = temp("transcribe-offer");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (owner, mut outbound) = live(&mut c, t0);
        let (other, mut quiet) = live(&mut c, t0);
        let mut reg = testkit::register("conol chat", Some("conol:chat"));
        reg.caps.order = deck_model::OrderCap::Push;
        reg.caps.transcribe = deck_model::TranscribeCap::Push;
        let agent: AgentId = serde_json::from_value(
            c.api_call(Principal::Uid(501), owner, false, Request::AgentRegister(reg), t0).unwrap()["agent"].clone(),
        )
        .unwrap();
        let call = |c: &mut Core, session: SessionId, method: &str, params: serde_json::Value| {
            let ctx = CallCtx {
                principal: Principal::Uid(501),
                session,
                scopes: vec![deck_model::Scope::Agent],
                trusted: false,
                own: std::collections::BTreeSet::new(),
            };
            c.call(ctx, method, params, t0).result
        };

        let wav = crate::audio::companion::wav16(&vec![0.1; 120_000], 16_000);
        let id = c.transcribe.offer(agent.clone(), wav.clone(), 7_500);
        c.on_from_audio(FromAudio::Transcribe { id }, t0);
        let Outbound::Line(line) = outbound.try_recv().expect("the owner was offered the press") else {
            panic!("an offer is a notification");
        };
        assert!(quiet.try_recv().is_err(), "nobody else hears of it");
        let line: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(line["params"]["type"], "agent.transcribe");
        let data = &line["params"]["data"];
        assert_eq!((data["agent"].as_str(), data["mime"].as_str(), data["ms"].as_u64()), (Some(agent.0.as_str()), Some("audio/wav"), Some(7_500)));
        let tr = data["transcription"].clone();

        // Paged out to the owner, refused to anyone else.
        assert!(call(&mut c, other, "agent.transcribe.audio", serde_json::json!({"transcription": tr})).is_err());
        let mut got = Vec::new();
        while got.len() < wav.len() {
            let page = call(&mut c, owner, "agent.transcribe.audio", serde_json::json!({"transcription": tr, "offset": got.len()})).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < 256 * 1024, "a page fits the API's line");
            got.extend(base64::engine::general_purpose::STANDARD.decode(page["data"].as_str().unwrap()).unwrap());
        }
        assert_eq!(got, wav);
        assert!(call(&mut c, other, "agent.transcribe.result", serde_json::json!({"transcription": tr, "text": "hijack"})).is_err());
        call(&mut c, owner, "agent.transcribe.result", serde_json::json!({"transcription": tr, "text": "ship it"})).unwrap();
        assert!(
            call(&mut c, owner, "agent.transcribe.result", serde_json::json!({"transcription": tr, "text": "again"})).is_err(),
            "the first answer stands"
        );

        // A disconnected owner fails its offer at once.
        let second = c.transcribe.offer(agent.clone(), wav, 7_500);
        c.on_from_audio(FromAudio::Transcribe { id: second }, t0);
        c.close_session(owner, t0);
        let tr2 = crate::audio::companion::offer_id(second);
        assert!(call(&mut c, owner, "agent.transcribe.result", serde_json::json!({"transcription": tr2, "text": "late"})).is_err());

        // No live owner with the cap: nothing is sent, the offer fails.
        let third = c.transcribe.offer(AgentId("ag_missing".into()), vec![0; 64], 1_000);
        c.on_from_audio(FromAudio::Transcribe { id: third }, t0);
        assert!(quiet.try_recv().is_err());
    }

    #[test]
    fn an_order_and_a_snooze_reach_the_owner_alone_by_push_or_poll() {
        let dir = temp("order-snooze");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (pushing, mut outbound) = live(&mut c, t0);
        let (polling, _quiet) = live(&mut c, t0);
        let mut push_reg = testkit::register("pusher", Some("k:pusher"));
        push_reg.caps.order = deck_model::OrderCap::Push;
        let push_agent: AgentId = serde_json::from_value(
            c.api_call(
                Principal::Uid(501),
                pushing,
                false,
                Request::AgentRegister(push_reg),
                t0,
            )
            .unwrap()["agent"]
                .clone(),
        )
        .unwrap();
        let poll_agent = register_on(&mut c, polling, "poller", t0);
        let order = |agent: &AgentId, text: &str| {
            Request::OpOrder(deck_model::OpOrder {
                agent: agent.clone(),
                text: text.into(),
                ctx: "talk".into(),
                source: deck_model::OrderSource::Ptt,
                utterance: None,
            })
        };

        // A push owner is written to; a poll owner's mail waits for its wait.
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            order(&push_agent, "also run the smoke tests"),
            t0,
        )
        .unwrap();
        let out = outbound.try_recv().expect("the owner was written to");
        let Outbound::Line(line) = out else {
            panic!("an order is a notification, not a delivery: {out:?}");
        };
        let line: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(line["params"]["type"], "agent.order");
        assert_eq!(line["params"]["data"]["text"], "also run the smoke tests");
        assert_eq!(line["params"]["data"]["via"], "push");
        // The id an owner reports a failure under (`agent.order.result`).
        assert_eq!(line["params"]["data"]["order"], "or_1");

        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            order(&poll_agent, "and then deploy"),
            t0,
        )
        .unwrap();
        assert_eq!(c.sessions.get(polling).unwrap().mailbox.len(), 1);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.wait(
            polling,
            poll_agent.clone(),
            Duration::from_secs(60),
            WaitReply::Result(tx),
            t0,
        );
        let WaitResult::Order {
            text,
            ctx,
            card,
            order,
            ..
        } = rx.try_recv().unwrap()
        else {
            panic!("the poll owner did not get its order");
        };
        assert_eq!(
            (text.as_str(), ctx.as_str(), card, order.as_deref()),
            ("and then deploy", "talk", None, Some("or_2"))
        );

        // A snooze on a poll card is a mail; on a push card a notification.
        let card = testkit::card_id(
            &c.api_call(
                Principal::Uid(501),
                polling,
                false,
                Request::CardSet(testkit::card(&poll_agent, "deploy?")),
                t0,
            )
            .unwrap(),
        );
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            Request::OpSnooze {
                agent: poll_agent.clone(),
                card,
            },
            t0,
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        c.wait(
            polling,
            poll_agent.clone(),
            Duration::from_secs(60),
            WaitReply::Result(tx),
            t0,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            WaitResult::Snooze { card: c } if c == card
        ));

        let mut pushed = testkit::card(&push_agent, "ship it?");
        pushed.deliver = Deliver::Push;
        let pushed_card = testkit::card_id(
            &c.api_call(
                Principal::Uid(501),
                pushing,
                false,
                Request::CardSet(pushed),
                t0,
            )
            .unwrap(),
        );
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            false,
            Request::OpSnooze {
                agent: push_agent.clone(),
                card: pushed_card,
            },
            t0,
        )
        .unwrap();
        let Outbound::Line(line) = outbound.try_recv().expect("the owner heard the snooze") else {
            panic!("a snooze is a notification");
        };
        let line: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(line["params"]["type"], "card.snoozed");

        // The bus saw the operator's side of both, and neither `agent.order`.
        c.publish();
        let (ring, _) = c.bus.replay(0, 500, None);
        let kinds: Vec<EventKind> = ring.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::OrderDispatched));
        assert!(kinds.contains(&EventKind::CardSnoozed));
        assert!(
            !kinds.contains(&EventKind::AgentOrder),
            "an order is the owner's alone"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A deck that said hello and went live, in English.
    fn link(c: &mut Core, now: Instant) {
        c.on_link(LinkEvent::Linked, now);
        c.on_device_line(
            DeviceLine::Hello {
                v: 1,
                fw: "0.13.0".into(),
                sid: 7,
                boot: false,
                rx: deck_model::LINE_MAX_U16,
                agents: None,
                lang: Some(0),
                lang_rev: Some(1),
                faces: Vec::new(),
            },
            now,
        );
        c.on_link(LinkEvent::Live { sid: 7 }, now);
    }

    /// An order the operator dictated, sent the way a door sends it.
    fn order_from_deck(c: &mut Core, agent: &AgentId, text: &str, now: Instant) -> ApiResult {
        let order = Request::OpOrder(deck_model::OpOrder {
            agent: agent.clone(),
            text: text.into(),
            ctx: "talk".into(),
            source: deck_model::OrderSource::Ptt,
            utterance: None,
        });
        c.api_call(Principal::Uid(501), SessionId(9), false, order, now)
    }

    /// A push agent owned by `session`.
    fn pusher(c: &mut Core, session: SessionId, now: Instant) -> AgentId {
        let mut reg = testkit::register("pusher", Some("k:pusher"));
        reg.caps.order = deck_model::OrderCap::Push;
        let r = c
            .api_call(
                Principal::Uid(501),
                session,
                false,
                Request::AgentRegister(reg),
                now,
            )
            .unwrap();
        serde_json::from_value(r["agent"].clone()).unwrap()
    }

    /// The `order.failed` events on the bus so far.
    fn failures(c: &mut Core) -> Vec<Event> {
        c.publish();
        c.bus
            .replay(0, 500, None)
            .0
            .iter()
            .filter(|e| e.kind == EventKind::OrderFailed)
            .map(|e| (**e).clone())
            .collect()
    }

    /// The failure card, when the core put one on the deck.
    fn failure_card_put(ops: &Receiver<DeviceOp>) -> bool {
        ops.try_iter().any(|op| {
            matches!(op, DeviceOp::Put(Key::Card(..), HostLine::Card { ref text, .. })
                if text == deck_model::NOT_DELIVERED.text[0])
        })
    }

    #[test]
    fn a_push_order_to_a_full_channel_fails_on_the_deck() {
        let dir = temp("order-full");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let session = c.sessions.open(
            OpenSession {
                kind: SessionKind::Live { tx: tx.clone() },
                principal: Principal::Uid(501),
                scopes: vec![deck_model::Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            },
            t0,
        );
        let agent = pusher(&mut c, session, t0);
        tx.try_send(Outbound::Line("{}".into())).unwrap();
        ops_of(&ops);
        let res = order_from_deck(&mut c, &agent, "deploy", t0).unwrap();
        let failed = failures(&mut c);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["reason"], "channel_full");
        assert_eq!(failed[0].data["order"], res["order"]);
        assert_eq!(failed[0].data["text"], "deploy");
        assert_eq!(failed[0].data["shown"], "card");
        assert!(failure_card_put(&ops), "the deck was told");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_push_order_to_a_closed_connection_fails() {
        let dir = temp("order-closed");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let (session, rx) = live(&mut c, t0);
        let agent = pusher(&mut c, session, t0);
        drop(rx);
        order_from_deck(&mut c, &agent, "deploy", t0).unwrap();
        let failed = failures(&mut c);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["reason"], "owner_gone");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_poll_order_left_in_the_mailbox_past_the_ttl_fails_as_expired() {
        let dir = temp("order-expired");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let mut cfg = crate::config::Config::default();
        cfg.api.mailbox_ttl_s = 2;
        c.set_config(Arc::new(cfg));
        let t0 = Instant::now();
        let (polling, _rx) = live(&mut c, t0);
        let agent = register_on(&mut c, polling, "poller", t0);
        c.tick(t0);
        let res = order_from_deck(&mut c, &agent, "deploy", t0).unwrap();
        assert_eq!(c.sessions.get(polling).unwrap().mailbox.len(), 1);
        assert!(failures(&mut c).is_empty(), "waiting for its owner");
        ops_of(&ops);
        c.tick(t0 + HEARTBEAT_EVERY + Duration::from_secs(1));
        assert!(c.sessions.get(polling).unwrap().mailbox.is_empty());
        let failed = failures(&mut c);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["reason"], "expired");
        assert_eq!(failed[0].data["order"], res["order"]);
        assert_eq!(failed[0].data["text"], "deploy");
        assert!(failure_card_put(&ops));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_agent_released_with_unread_orders_reports_agent_gone() {
        let dir = temp("order-released");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (polling, _rx) = live(&mut c, t0);
        let agent = register_on(&mut c, polling, "poller", t0);
        let res = order_from_deck(&mut c, &agent, "deploy", t0).unwrap();
        ops_of(&ops);
        c.api_call(
            Principal::Uid(501),
            polling,
            false,
            Request::AgentRelease {
                agent: agent.clone(),
                reason: None,
            },
            t0,
        )
        .unwrap();
        let failed = failures(&mut c);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["reason"], "agent_gone");
        assert_eq!(failed[0].data["order"], res["order"]);
        // The agent is gone, so the deck's own toast says it.
        assert_eq!(failed[0].data["shown"], "toast");
        assert!(ops.try_iter().any(|op| matches!(
            op,
            DeviceOp::Control(Control::Transcript {
                failed: true,
                press: None,
                ..
            })
        )));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orders_for_internal_sessions_never_fail_on_expiry_or_removal() {
        let dir = temp("order-internal");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let mut cfg = crate::config::Config::default();
        cfg.api.mailbox_ttl_s = 2;
        c.set_config(Arc::new(cfg));
        let t0 = Instant::now();
        // The Codex client's session: it waits for its own mail, and what
        // it never read is nobody's to report.
        let codex = Principal::Adapter(deck_model::AdapterKind::Codex);
        let session = SessionId(50);
        c.sessions
            .open_internal(session, codex.clone(), "codex", t0);
        let r = c
            .api_call(
                codex.clone(),
                session,
                false,
                Request::AgentRegister(testkit::register("thread", Some("codex:t1"))),
                t0,
            )
            .unwrap();
        let agent: AgentId = serde_json::from_value(r["agent"].clone()).unwrap();
        c.tick(t0);
        order_from_deck(&mut c, &agent, "one", t0).unwrap();
        c.tick(t0 + HEARTBEAT_EVERY + Duration::from_secs(1));
        assert!(c.sessions.get(session).unwrap().mailbox.is_empty());
        order_from_deck(&mut c, &agent, "two", t0).unwrap();
        c.api_call(
            codex,
            session,
            false,
            Request::AgentRelease {
                agent,
                reason: None,
            },
            t0,
        )
        .unwrap();
        assert!(failures(&mut c).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The door R3's hand-off and any other failure the core finds use: the
    /// same card as a failed order, from one call.
    #[test]
    fn the_core_shows_a_failure_of_its_own_through_one_door() {
        let dir = temp("fail-on-deck");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (session, _rx) = live(&mut c, t0);
        let agent = register_on(&mut c, session, "bot", t0);
        ops_of(&ops);
        let shown = c.fail_on_deck(Some(&agent), "no_route", deck_model::NOT_DELIVERED, t0);
        assert_eq!(shown, Shown::Card);
        assert!(failure_card_put(&ops));
        c.publish();
        let (ring, _) = c.bus.replay(0, 500, None);
        let set = ring
            .iter()
            .rev()
            .find(|e| e.kind == EventKind::CardSet)
            .expect("card.set");
        assert_eq!(set.data["host"], deck_model::HOST_FAILURE);
        assert_eq!(set.data["reason"], "no_route");
        // No agent to put a card on: the deck's toast.
        let shown = c.fail_on_deck(None, "no_route", deck_model::NOT_DELIVERED, t0);
        assert_eq!(shown, Shown::Toast);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Claude session on the deck, registered by the Claude adapter the
    /// way its hook does, with the app its helper found; and a detached
    /// desktop, whose far end the test holds.
    fn handoff_rig(
        name: &str,
        app: Option<&str>,
    ) -> (
        Core,
        Receiver<DeviceOp>,
        Receiver<DesktopJob>,
        AgentId,
        SessionId,
        std::path::PathBuf,
        Instant,
    ) {
        use deck_model::{AdapterKind, Kind, OrderCap, Terminal};
        let dir = temp(name);
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let (desktop, jobs, _answers) = DesktopHandle::detached();
        c.set_desktop(desktop);
        let t0 = Instant::now();
        link(&mut c, t0);
        let adapter = Principal::Adapter(AdapterKind::Claude);
        let session = SessionId(90);
        c.sessions
            .open_internal(session, adapter.clone(), "claude-adapter", t0);
        let mut reg = testkit::register(name, Some(&format!("claude:{name}")));
        reg.kind = Kind::Claude;
        reg.caps.order = OrderCap::Inject;
        reg.caps.answer = deck_model::AnswerCap::Hold;
        reg.slot_hint = Some(Slot(1));
        reg.terminal = app.map(|app| Terminal {
            app: Some(app.into()),
            ..Terminal::default()
        });
        let r = c
            .api_call(adapter, session, false, Request::AgentRegister(reg), t0)
            .unwrap();
        let agent: AgentId = serde_json::from_value(r["agent"].clone()).unwrap();
        ops_of(&ops);
        (c, ops, jobs, agent, session, dir, t0)
    }

    /// An informational `Open app` card on `agent`, as Claude's
    /// `Notification` hook makes one; its id.
    fn focus_card(c: &mut Core, agent: &AgentId, session: SessionId, now: Instant) -> u16 {
        let mut card = testkit::card(agent, "Claude needs your permission to use Bash");
        card.deliver = Deliver::Focus;
        card.choices = vec![deck_model::Choice {
            label: deck_model::OPEN_APP[0].into(),
            detail: None,
        }];
        card.rec = Some(0);
        card.origin.kind = deck_model::OriginKind::Permission;
        let principal = Principal::Adapter(deck_model::AdapterKind::Claude);
        testkit::card_id(
            &c.api_call(principal, session, false, Request::CardSet(card), now)
                .unwrap(),
        )
        .0
    }

    fn press(c: &mut Core, card: u16, now: Instant) {
        c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card,
                choice: 0,
            },
            now,
        );
    }

    /// The card the core put on the deck for a failure, by its text.
    fn put_card_text(ops: &Receiver<DeviceOp>) -> Option<String> {
        ops.try_iter().find_map(|op| match op {
            DeviceOp::Put(Key::Card(..), HostLine::Card { text, .. }) => Some(text),
            _ => None,
        })
    }

    /// ENTER on a hook session's `Open app` card asks the desktop for that
    /// session's own app, by the bundle identifier its helper reported.
    #[test]
    fn a_focus_card_press_raises_the_hook_sessions_app() {
        let (mut c, _ops, jobs, agent, session, dir, t0) =
            handoff_rig("handoff-press", Some("com.mitchellh.ghostty"));
        let card = focus_card(&mut c, &agent, session, t0);
        press(&mut c, card, t0);
        assert_eq!(
            jobs.try_recv(),
            Ok(DesktopJob::Raise {
                agent: agent.clone(),
                app: "com.mitchellh.ghostty".into(),
            })
        );
        assert!(jobs.try_recv().is_err(), "once");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A channel change and a PTT press move the focus and raise nothing:
    /// dictating to a session in the background must not pull its app
    /// forward.
    #[test]
    fn a_channel_change_and_a_ptt_press_raise_nothing() {
        let (mut c, _ops, jobs, _agent, _session, dir, t0) =
            handoff_rig("handoff-channel", Some("dev.zed.Zed"));
        c.on_device_line(DeviceLine::Channel { slot: 1, seq: 4 }, t0);
        let ptt = |act| DeviceLine::Ptt {
            act,
            slot: 1,
            ctx: deck_model::PttCtx::Talk,
            card: None,
            abort: false,
            press: None,
        };
        c.on_device_line(ptt(1), t0);
        c.on_device_line(DeviceLine::Channel { slot: 1, seq: 5 }, t0);
        c.on_device_line(ptt(0), t0);
        assert!(jobs.try_recv().is_err(), "nothing was raised");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `op.focus` is a deliberate act too: a hook agent's app is raised.
    #[test]
    fn op_focus_raises_a_hook_agents_app() {
        let (mut c, _ops, jobs, agent, _session, dir, t0) =
            handoff_rig("handoff-op", Some("dev.zed.Zed"));
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            true,
            Request::OpFocus {
                agent: agent.clone(),
            },
            t0,
        )
        .unwrap();
        assert_eq!(
            jobs.try_recv(),
            Ok(DesktopJob::Raise {
                agent,
                app: "dev.zed.Zed".into(),
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What a connected app says about its own terminal is informational
    /// (§4.6): it is never raised, and nothing is claimed on the deck — the
    /// app brings itself forward on `card.focused`.
    #[test]
    fn a_connected_apps_self_declared_app_is_never_raised() {
        let dir = temp("handoff-app");
        let (mut c, ops, _lines, _debug) = detached(&dir);
        let (desktop, jobs, _answers) = DesktopHandle::detached();
        c.set_desktop(desktop);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (session, _rx) = live(&mut c, t0);
        let mut reg = testkit::register("conol", Some("conol:chat"));
        reg.terminal = Some(deck_model::Terminal {
            app: Some("com.apple.Terminal".into()),
            ..deck_model::Terminal::default()
        });
        let r = c
            .api_call(Principal::Uid(501), session, false, Request::AgentRegister(reg), t0)
            .unwrap();
        let agent: AgentId = serde_json::from_value(r["agent"].clone()).unwrap();
        assert_eq!(
            c.store.snapshot().agents[&agent]
                .session
                .terminal
                .as_ref()
                .and_then(|t| t.app.as_deref()),
            Some("com.apple.Terminal"),
            "kept, as information"
        );
        assert_eq!(handoff_app(&c.store.snapshot().agents[&agent]), None);
        ops_of(&ops);
        c.api_call(
            Principal::Uid(501),
            SessionId(9),
            true,
            Request::OpFocus {
                agent: agent.clone(),
            },
            t0,
        )
        .unwrap();
        assert!(jobs.try_recv().is_err());
        assert!(c.desktop.events.try_recv().is_err(), "no failure either");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A hook session whose helper found no app: nothing is asked of
    /// macOS, and the press is answered on the deck, in its language.
    #[test]
    fn a_session_with_no_app_is_not_raised_and_the_deck_says_so() {
        let (mut c, ops, jobs, agent, session, dir, t0) = handoff_rig("handoff-none", None);
        let card = focus_card(&mut c, &agent, session, t0);
        press(&mut c, card, t0);
        assert!(jobs.try_recv().is_err(), "no job");
        let outcome = c.desktop.events.try_recv().expect("the core's own answer");
        assert_eq!(
            outcome,
            FromDesktop::Raised {
                agent: agent.clone(),
                app: None,
                outcome: crate::desktop::Outcome::NoApp,
            }
        );
        ops_of(&ops);
        c.on_from_desktop(outcome, t0);
        assert_eq!(
            put_card_text(&ops).as_deref(),
            Some(deck_model::HandoffFailure::NoApp.line().text[0])
        );
        c.publish();
        let (ring, _) = c.bus.replay(0, 500, None);
        let set = ring
            .iter()
            .rev()
            .find(|e| e.kind == EventKind::CardSet)
            .expect("card.set");
        assert_eq!(set.data["host"], deck_model::HOST_FAILURE);
        assert_eq!(set.data["reason"], "no_app");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An app that is not running, or one macOS will not bring forward:
    /// the desktop's answer is shown on the deck the same way. One that
    /// came forward shows nothing.
    #[test]
    fn a_hand_off_that_did_not_happen_is_shown_on_the_deck() {
        for (outcome, failure) in [
            (
                crate::desktop::Outcome::NotRunning,
                Some(deck_model::HandoffFailure::NotRunning),
            ),
            (
                crate::desktop::Outcome::Refused("refused".into()),
                Some(deck_model::HandoffFailure::Refused),
            ),
            (crate::desktop::Outcome::Raised, None),
        ] {
            let (mut c, ops, _jobs, agent, session, dir, t0) =
                handoff_rig(&format!("handoff-{}", outcome.word()), Some("dev.zed.Zed"));
            let card = focus_card(&mut c, &agent, session, t0);
            press(&mut c, card, t0);
            ops_of(&ops);
            c.on_from_desktop(
                FromDesktop::Raised {
                    agent: agent.clone(),
                    app: Some("dev.zed.Zed".into()),
                    outcome: outcome.clone(),
                },
                t0,
            );
            assert_eq!(
                put_card_text(&ops).as_deref(),
                failure.map(|f| f.line().text[0]),
                "{outcome:?}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// Which app a session runs in is the session's own business (§5.5 keeps
    /// `terminal.app` as content), so the hand-off's line in the log at its
    /// default level names the agent and the outcome only; the app is a
    /// `debug` line.
    #[test]
    fn the_hand_off_logs_its_outcome_and_never_the_app_at_info() {
        use crate::core::testkit::infos_of;
        let (mut c, _ops, _jobs, agent, _session, dir, t0) =
            handoff_rig("handoff-log", Some("com.example.Private"));
        let logged = infos_of(|| {
            c.on_from_desktop(
                FromDesktop::Raised {
                    agent: agent.clone(),
                    app: Some("com.example.Private".into()),
                    outcome: crate::desktop::Outcome::Raised,
                },
                t0,
            );
        });
        assert!(logged.contains("desktop: hand-off"), "{logged}");
        assert!(logged.contains("outcome=raised"), "{logged}");
        assert!(!logged.contains("com.example.Private"), "{logged}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An order to a session the daemon delivers itself, with neither a
    /// Claude Channels connection nor an inbox: dispatched, then failed
    /// `no_route` with the words kept, and nothing reaches the desktop —
    /// with an engine or without one. Nothing is ever typed.
    #[test]
    fn an_inject_order_with_no_channel_and_no_inbox_fails_no_route() {
        for engine in [false, true] {
            let (mut c, _ops, jobs, agent, _session, dir, t0) =
                handoff_rig(&format!("no-route-{engine}"), Some("dev.zed.Zed"));
            if !engine {
                c.set_desktop(DesktopHandle::none());
            }
            // Dictated on the deck: the device's own principal, which an
            // `inject` order needs (a uid caller must be trusted).
            let order = Request::OpOrder(deck_model::OpOrder {
                agent: agent.clone(),
                text: "deploy the api".into(),
                ctx: "talk".into(),
                source: deck_model::OrderSource::Ptt,
                utterance: None,
            });
            let r = c
                .api_call(Principal::Device, SessionId(9), true, order, t0)
                .unwrap();
            assert_eq!(r["via"], "inject");
            c.publish();
            let (ring, _) = c.bus.replay(0, 500, None);
            let kinds: Vec<EventKind> = ring.iter().map(|e| e.kind).collect();
            let dispatched = kinds
                .iter()
                .position(|k| *k == EventKind::OrderDispatched)
                .expect("order.dispatched");
            let failed = ring
                .iter()
                .position(|e| e.kind == EventKind::OrderFailed)
                .expect("order.failed");
            assert!(dispatched < failed, "{kinds:?}");
            assert_eq!(ring[failed].data["reason"], "no_route");
            assert_eq!(ring[failed].data["text_kept"], true);
            assert_eq!(ring[failed].data["text"], "deploy the api");
            assert!(jobs.try_recv().is_err(), "nothing for the desktop");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// The Codex client's session and a poll agent on it. Nothing waits
    /// in `agent.wait` for it here, so an answer counts as handed over the
    /// moment it arrives (`Core::deliver_poll`).
    fn codex_agent(c: &mut Core, now: Instant) -> (AgentId, Principal) {
        let codex = Principal::Adapter(deck_model::AdapterKind::Codex);
        c.sessions
            .open_internal(SessionId(50), codex.clone(), "codex", now);
        let r = c
            .api_call(
                codex.clone(),
                SessionId(50),
                false,
                Request::AgentRegister(testkit::register("thread", Some("codex:t1"))),
                now,
            )
            .unwrap();
        (serde_json::from_value(r["agent"].clone()).unwrap(), codex)
    }

    /// `metrics.get`'s `deliveries`, as `deckd ctl --scopes admin` reads it.
    fn deliveries_of(c: &mut Core, now: Instant) -> serde_json::Value {
        let admin = testkit::ctx(2, &[deck_model::Scope::Admin]);
        let mut metrics = c
            .call(admin, "metrics.get", serde_json::Value::Null, now)
            .result
            .unwrap();
        metrics["deliveries"].take()
    }

    /// A card answered on the deck, read back from `metrics.get`: its
    /// source by its word, where it was answered, that it reached the deck
    /// and that the deck was used; never its words.
    #[test]
    fn metrics_get_serves_the_delivery_record() {
        let dir = temp("deliveries-metrics");
        // No hid thread to wait for: `metrics.get` answers at once.
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (agent, codex) = codex_agent(&mut c, t0);
        let set = c
            .api_call(
                codex,
                SessionId(50),
                false,
                Request::CardSet(testkit::card(&agent, "Run the migration?")),
                t0,
            )
            .unwrap();
        let card = testkit::card_id(&set).0;
        let slot = c.store.snapshot().agents[&agent]
            .slot
            .expect("on the deck")
            .0;
        c.on_device_line(
            DeviceLine::Answer {
                slot,
                card,
                choice: 0,
            },
            t0,
        );
        let record = deliveries_of(&mut c, t0);
        let cards = &record["cards"];
        assert_eq!(cards["outcomes"]["answered"], 1, "{record}");
        assert_eq!(cards["answered_on"]["deck"], 1);
        assert_eq!(cards["reached"], 1);
        assert_eq!(cards["deck_used"], 1);
        assert_eq!(record["by_from"]["codex"]["total"], 1);
        assert_eq!(record["recent"][0]["from"], "codex");
        assert_eq!(record["recent"][0]["card"], card);
        assert_eq!(record["open"], 0);
        assert!(!record.to_string().contains("migration"), "{record}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every deck line reaches the record before anything else handles it,
    /// an image request among them, which the core answers itself and
    /// never hands to the store; `status.json` carries the counts.
    #[test]
    fn reader_lines_reach_the_counters_including_images() {
        let dir = temp("deliveries-reader");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        for line in [
            DeviceLine::Channel { slot: 1, seq: 1 },
            DeviceLine::DocLines {
                slot: 1,
                doc: deck_model::DocKind::Reply,
                id: 1,
                first: 0,
            },
            DeviceLine::Tool {
                slot: 1,
                id: 1,
                index: 0,
            },
            DeviceLine::Image {
                slot: 1,
                rev: 1,
                index: 0,
                offset: 0,
            },
        ] {
            c.on_device_line(line, t0);
        }
        let reader = deliveries_of(&mut c, t0)["reader"].take();
        for (counter, n) in [
            ("channels", 1),
            ("history", 1),
            ("tool_rows", 1),
            ("images", 1),
            ("lines", 3),
        ] {
            assert_eq!(reader[counter], n, "{counter}: {reader}");
        }
        let brief = c.status(t0).deliveries.expect("the record in brief");
        assert_eq!((brief.reader.images, brief.reader.channels), (1, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The record is in `state.json` when the daemon stops, the card still
    /// up ending as `shutdown`, and the next daemon goes on from it.
    #[test]
    fn the_record_survives_a_restart() {
        let dir = temp("deliveries-restart");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (agent, codex) = codex_agent(&mut c, t0);
        let ask = |c: &mut Core, text: &str| {
            let set = c
                .api_call(
                    codex.clone(),
                    SessionId(50),
                    false,
                    Request::CardSet(testkit::card(&agent, text)),
                    t0,
                )
                .unwrap();
            testkit::card_id(&set).0
        };
        let first = ask(&mut c, "first");
        c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card: first,
                choice: 0,
            },
            t0,
        );
        ask(&mut c, "still up");
        c.on_device_line(DeviceLine::Channel { slot: 1, seq: 1 }, t0);
        let before = deliveries_of(&mut c, t0);
        assert_eq!(before["open"], 1);
        c.shutdown();

        let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        let (state, loaded) = State::load(&paths.state);
        assert_eq!(loaded, crate::state::Loaded::Read);
        let store = Store::new(StoreConfig::new(Instant::now(), Ts(1_789_760_600_000), 8));
        let (hid, _ops, _lines, _) = crate::hid::HidHandle::detached();
        let mut next = Core::new(store, state, hid, paths);
        let after = deliveries_of(&mut next, Instant::now());
        assert_eq!(after["since"], before["since"]);
        assert_eq!(after["version"], before["version"]);
        let outcomes = &after["cards"]["outcomes"];
        assert_eq!(outcomes["answered"], 1);
        assert_eq!(outcomes["shutdown"], 1, "the card still up when it stopped");
        assert_eq!(after["reader"]["channels"], 1);
        assert_eq!(after["recent"].as_array().map(Vec::len), Some(2));
        assert_eq!(after["open"], 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R2's card for an order that did not reach its agent is a failure
    /// card in the record, kept apart from the cards the sources asked,
    /// and the order is counted by route and by why it failed.
    #[test]
    fn an_order_failure_card_is_a_failure_card_in_the_record() {
        let dir = temp("deliveries-order-failed");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        link(&mut c, t0);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let session = c.sessions.open(
            OpenSession {
                kind: SessionKind::Live { tx: tx.clone() },
                principal: Principal::Uid(501),
                scopes: vec![deck_model::Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            },
            t0,
        );
        let agent = pusher(&mut c, session, t0);
        tx.try_send(Outbound::Line("{}".into())).unwrap();
        order_from_deck(&mut c, &agent, "SECRET-ORDER", t0).unwrap();
        while rx.try_recv().is_ok() {}
        // It goes like any Ready card: at the agent's next state change.
        c.api_call(
            Principal::Uid(501),
            session,
            false,
            Request::AgentUpdate(deck_model::AgentUpdate {
                agent: agent.clone(),
                state: Some(AgentState::Working),
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None,
                task: None,
                subagents: None,
            }),
            t0,
        )
        .unwrap();
        let record = deliveries_of(&mut c, t0);
        assert_eq!(record["failure_cards"]["total"], 1, "{record}");
        assert_eq!(record["failure_cards"]["outcomes"]["expired"], 1);
        assert_eq!(record["recent"][0]["failure"], "channel_full");
        assert_eq!(record["recent"][0]["from"], "app");
        assert_eq!(record["cards"]["total"], 0, "no source asked anything");
        let orders = &record["orders"];
        assert_eq!(orders["dispatched"]["push"], 1);
        assert_eq!(orders["failed"]["channel_full"], 1);
        assert_eq!(orders["shown"]["card"], 1);
        assert!(!record.to_string().contains("SECRET"), "{record}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R4 and R5 together: a card whose agent the quiet service released
    /// (`crate::dormancy`) ends as `agent_gone` with the daemon's own reason
    /// word, `dormant`, and not as an app's own text (`other`).
    #[test]
    fn a_card_released_with_its_agent_into_the_quiet_is_recorded_as_dormant() {
        use crate::dormancy::{Dormancy, WINDOW};
        let dir = temp("deliveries-dormant");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        let wall = c.store.ts(t0);
        c.set_dormancy(Dormancy::start(WINDOW, None, wall, t0), t0);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        assert!(c.adapters.collecting());
        let (agent, codex) = codex_agent(&mut c, t0);
        c.api_call(
            codex,
            SessionId(50),
            false,
            Request::CardSet(testkit::card(&agent, "SECRET-ASK")),
            t0,
        )
        .unwrap();
        c.on_from_hid(FromHid::Link(LinkEvent::Down), t0);
        tick_through(&mut c, t0, t0 + WINDOW);
        c.tick(t0 + WINDOW);
        assert!(!c.adapters.collecting(), "ten minutes without the deck");
        assert!(!c.store.snapshot().agents.contains_key(&agent));
        let record = deliveries_of(&mut c, t0 + WINDOW);
        let last = &record["recent"][0];
        assert_eq!(
            (last["outcome"].as_str(), last["reason"].as_str()),
            (Some("agent_gone"), Some("dormant")),
            "{record}"
        );
        assert_eq!(record["cards"]["outcomes"]["agent_gone"], 1);
        assert!(!record.to_string().contains("SECRET"), "{record}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R10, R7 and R4 together: forgetting a deck that is not in use
    /// (`care.decks.forget`) drops that deck's record and nothing else. The
    /// deck in use keeps its own strip, at the top level and in its record,
    /// and the delivery record is kept whole, in the core and in
    /// `state.json`.
    #[test]
    fn forgetting_a_deck_keeps_the_delivery_record_and_the_deck_in_use() {
        use deck_model::{FaceEntry, Template};
        let dir = temp("forget-keeps-deliveries");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        let (a, b) = ("A1B2C3D4E5F6", "C0FFEE15600D");
        c.on_identity(Some(a), t0);
        c.on_identity(Some(b), t0);
        // Deck B, the one in use, links with a strip of its own.
        let face = |f, t, n: &str| FaceEntry { f, t, n: n.into() };
        let strip = vec![
            face(1, Template::Agents, "Agents"),
            face(4, Template::Usage, "Usage"),
            face(5, Template::Settings, "Settings"),
        ];
        c.on_link(LinkEvent::Linked, t0);
        c.on_device_line(
            DeviceLine::Hello {
                v: 1,
                fw: "0.13.0".into(),
                sid: 7,
                boot: false,
                rx: deck_model::LINE_MAX_U16,
                agents: None,
                lang: Some(0),
                lang_rev: Some(1),
                faces: strip.clone(),
            },
            t0,
        );
        c.on_link(LinkEvent::Live { sid: 7 }, t0);
        assert_eq!(c.store.snapshot().faces, strip);
        // A card answered on the deck: the record holds it.
        let (agent, codex) = codex_agent(&mut c, t0);
        let set = c
            .api_call(
                codex,
                SessionId(50),
                false,
                Request::CardSet(testkit::card(&agent, "ship it?")),
                t0,
            )
            .unwrap();
        let card = testkit::card_id(&set).0;
        c.on_device_line(
            DeviceLine::Answer {
                slot: 1,
                card,
                choice: 0,
            },
            t0,
        );
        let before = deliveries_of(&mut c, t0);
        assert_eq!(before["cards"]["outcomes"]["answered"], 1, "{before}");

        let admin = testkit::ctx(2, &[deck_model::Scope::Admin]);
        let label_a = crate::device::identity::label(a);
        let forgotten = c
            .call(
                admin,
                "care.decks.forget",
                serde_json::json!({ "deck": label_a }),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(forgotten["forgotten"], true);
        assert_eq!(deliveries_of(&mut c, t0), before, "the record is the Mac's");
        assert_eq!(c.store.snapshot().faces, strip, "the deck in use keeps its strip");
        c.tick(t0 + STATE_DEBOUNCE);
        let (saved, _) = State::load(&dir.join("state.json"));
        assert_eq!(saved.decks.keys().collect::<Vec<_>>(), vec![b]);
        assert_eq!(saved.deck.as_deref(), Some(b));
        assert_eq!((&saved.faces, &saved.decks[b].faces), (&strip, &strip));
        let outcome = |record: &crate::deliveries::Record| record.outcome.clone();
        assert_eq!(
            saved.deliveries.recent.iter().map(outcome).collect::<Vec<_>>(),
            vec!["answered".to_string()]
        );
        assert_eq!(saved.deliveries.cards.total, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What the settle window did with Claude's asks, counted on the hook
    /// door's side, reaches the record on the core's next tick.
    #[test]
    fn the_settle_windows_counts_reach_the_record_on_a_tick() {
        use crate::adapters::settle::Outcome;
        let dir = temp("deliveries-asks");
        let (mut c, _ops, _lines, _) = detached(&dir);
        let t0 = Instant::now();
        for outcome in [Some(Outcome::Settled), Some(Outcome::Settled), None] {
            c.adapters.asks.waited();
            c.adapters.asks.ended(outcome);
        }
        c.adapters.asks.waited();
        c.tick(t0);
        assert_eq!(
            c.deliveries.json()["asks"],
            serde_json::json!({"waited": 4, "settled": 2, "prompted": 0, "passed": 1})
        );
        c.adapters.asks.ended(Some(Outcome::Ask));
        let asks = deliveries_of(&mut c, t0)["asks"].take();
        assert_eq!(asks["prompted"], 1, "metrics.get takes them too: {asks}");
        assert_eq!(asks["waited"], 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sids_are_nonzero_and_vary() {
        let a = new_sid();
        let b = new_sid();
        assert_ne!(a, 0);
        assert!(a != b || new_sid() != a);
        assert!(wall_now().millis() > 1_700_000_000_000);
    }
}
