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
    DeviceOp, Effect, Event, EventKind, HoldId, Key, LinkEvent, Principal, Request, Scope,
    SessionId, Slot, Store, StoreConfig, TimerKey, Ts, VoiceIn,
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
    /// `[voice.stt]` as the local recogniser reads it at every press
    /// (`audio::whisper`), refreshed the same way.
    pub stt_settings: Arc<arc_swap::ArcSwap<crate::stt::Settings>>,
    /// The one model download at a time (`stt.install`, `stt.status`).
    pub stt_install: crate::download::Installer,
    pub firmware: crate::firmware::Updater,
    /// The local recogniser, when this build has one, for `stt.status`
    /// and for loading a model the moment it is on disk.
    #[cfg(target_os = "macos")]
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
    /// The desktop seam (§5.5, week 5). `DesktopHandle::none()` until
    /// something installs a worker, which is what `--no-device` and every
    /// unit test leave in place: `Effect::Desktop` is logged and forgotten
    /// and `op.order` still resolves to `via:"inject"` and writes the order
    /// history.
    pub desktop: DesktopHandle,
    /// Orders a full desktop queue refused, for `metrics.get`. There is no
    /// re-projection to cover a drop here, and retrying an order the
    /// operator dictated seconds ago is worse than telling them it was
    /// lost — so a refusal is `order.failed{reason:"undeliverable"}` and a
    /// counter, never a queue.
    desktop_dropped: u64,
    /// What the injector last learned about Automation, for `perm.status`.
    /// `None` until it has tried: an un-probed permission is `unknown`, which
    /// is not the same claim as `not_determined` (§4.11).
    automation_denied: Option<bool>,
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
            stt_settings: Arc::new(arc_swap::ArcSwap::from_pointee(crate::stt::Settings::default())),
            stt_install: crate::download::Installer::default(),
            firmware: crate::firmware::Updater::default(),
            executable: std::env::current_exe().ok(),
            #[cfg(target_os = "macos")]
            whisper: None,
            kokoro_settings: Arc::new(arc_swap::ArcSwap::from_pointee(crate::kokoro::Settings::default())),
            tts_install: crate::download::Installer::default(),
            #[cfg(target_os = "macos")]
            kokoro: None,
            transcribe: crate::audio::companion::CompanionHub::new(),
            desktop: DesktopHandle::none(),
            desktop_dropped: 0,
            automation_denied: None,
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
        }
    }

    /// The effective config (`service.info`, the logical-session TTL) and
    /// every hot key of §3.7 the core owns: `[trust]
    /// require_trusted_for_allow` here, and the store's own timings
    /// (`agents.grace_s`, `cards.ready_ttl_s`, `hooks.claude.hold_s`, the
    /// pins) through `Store::reconfigure`, which leaves the store's
    /// identity and `port` alone.
    pub fn set_config(&mut self, config: Arc<Config>) {
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
        // operator is listening to.
        let devices = deck_model::AudioCmd::Devices {
            device_uid: config.voice.device_uid.clone(),
            fallback_output: config.voice.fallback_output.clone(),
            fallback_input: config.voice.fallback_input.clone(),
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
                let _ = reply.send(opened);
            }
            CoreMsg::LogicalSession {
                token,
                client,
                principal,
                scopes,
                reply,
            } => {
                let id = self.logical_session(token, client, principal, scopes, now);
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
                self.set_config(config);
                // `[usage] providers` may have changed what is drawn.
                self.push_usage(now);
            }
            CoreMsg::Shutdown => {}
        }
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
                    if let Some(agent) = event_agent(event) {
                        self.prune(|m| sessions::mailbox::prune_agent(m, &agent));
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

    /// `agent.order` never goes on the bus: it belongs to the owner alone.
    fn route_order(&mut self, event: &Event, now: Instant) {
        let Some(agent) = event_agent(event) else {
            return;
        };
        let Some(session) = self.owner_session(&agent) else {
            return;
        };
        if event.data["via"].as_str() == Some("push") {
            self.notify(session, crate::api::rpc::event_line(event));
            return;
        }
        let mail = Mail {
            agent,
            at: now,
            kind: MailKind::Order {
                text: event.data["text"].as_str().unwrap_or_default().to_string(),
                ctx: event.data["ctx"].as_str().unwrap_or("talk").to_string(),
                choice: None,
                card: None,
            },
        };
        self.hand_to_waiter(session, mail);
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
        self.prune(|m| sessions::mailbox::prune_older(m, now, ttl));
    }

    /// The orderly end: the closing metric, `Command::Shutdown` (holds
    /// released by the store), the bus, `state.json`, `status.json`.
    pub fn shutdown(&mut self) {
        let now = Instant::now();
        self.refresh_metric(now);
        self.step(Command::Shutdown, now);
        self.publish();
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
                // principal (`DesktopCmd::Inject` is `{agent, text}`).
                Effect::Desktop(cmd) => self.send_desktop(cmd, events),
                other => self.execute(other, now),
            }
        }
        for event in events {
            tracing::debug!(rev = event.rev, kind = event.kind.as_str(), "event");
        }
        self.route_events(events, now);
        self.note_answer_outcomes(events);
        self.unpin_removed(events);
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
        if let DeviceLine::Image { slot, rev, index, offset } = &line {
            self.on_device_image(*slot, *rev, *index, *offset, now);
            return Vec::new();
        }
        let events = self.step(Command::Device(line), now);
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
        self.write_status(now);
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

    /// One `DesktopCmd` from the reducer, turned into a [`DesktopJob`] the
    /// worker can run (§5.5, week 5).
    ///
    /// **This is where guard 1 is applied** on the core side:
    /// [`crate::desktop::inject::target_of`] is the only way to build a
    /// `Target`, and it refuses every agent §5.5 says may not be an
    /// injection target — a card choice's `push`/`poll` route, an agent
    /// whose `source` is not `Hook` (a virtual agent's self-supplied
    /// `terminal`/`pid` is informational, §4.6), a pid the hook helper
    /// resolved as `shell`/`unknown`, a session with no tty. The *trust*
    /// half of guard 1 ran earlier still, in `core::guard`, before the
    /// reducer routed anything.
    fn send_desktop(&mut self, cmd: deck_model::DesktopCmd, events: &[Event]) {
        use deck_model::DesktopCmd;
        let agent = match &cmd {
            DesktopCmd::Focus { agent } | DesktopCmd::Inject { agent, .. } => agent.clone(),
            // No agent, no terminal, no guard: straight to the desktop thread.
            DesktopCmd::Keystroke { keys } => {
                let job = crate::desktop::DesktopJob::Keystroke { keys: keys.clone() };
                if let Some(crate::desktop::DesktopJob::Keystroke { keys }) = self.desktop.send_or_return(job) {
                    tracing::info!(%keys, engine = self.desktop.engine(), "macro keystroke dropped: no desktop, or its queue is full");
                }
                return;
            }
        };
        // The principal `order.failed` is published for: whoever the
        // `order.dispatched` of this same step named. Falling back to the
        // agent's owner keeps the event attributable when the dispatch is
        // not in this batch (a `Focus`, or a future caller).
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
        // Channels are preferred only after a notification round trip proves
        // this session opted in. The old inbox remains the safe fallback.
        if let DesktopCmd::Inject { text, .. } = &cmd {
            if let Some(target) = self.store.snapshot().agents.get(&agent) {
                let dialog_open = target.card.as_ref().is_some_and(deck_model::Card::is_open_dialog);
                if self.adapters.claude_channels.post(
                    target, text, self.adapters.inbox(&agent), dialog_open, self.desktop.outcomes(),
                ) {
                    return;
                }
            }
        }
        // The inbox fallback (§5.5, `crate::inbox`): a Claude session
        // whose message socket a hook reported is **posted to**, never typed
        // at — no focus, no AppleScript, no TCC, and the only route a
        // Desktop-app session has. It needs no desktop engine, so it runs
        // before the `engine()` check below; a `--no-device` daemon can
        // still deliver a spoken order this way. Guard 2 is read fresh here
        // and re-checked by the poster, exactly as the injector does.
        if let DesktopCmd::Inject { text, .. } = &cmd {
            if let Some(inbox) = self.adapters.inbox(&agent) {
                let dialog_open = self
                    .store
                    .snapshot()
                    .agents
                    .get(&agent)
                    .and_then(|a| a.card.as_ref())
                    .is_some_and(deck_model::Card::is_open_dialog);
                crate::inbox::post_async(
                    self.desktop.outcomes(),
                    agent,
                    text.clone(),
                    inbox,
                    dialog_open,
                );
                return;
            }
        }
        let target = match self
            .store
            .snapshot()
            .agents
            .get(&agent)
            .ok_or(crate::desktop::DesktopError::NoTerminal)
            .and_then(crate::desktop::inject::target_of)
        {
            Ok(target) => target,
            Err(refusal) => {
                tracing::info!(
                    agent = %agent.0, reason = refusal.reason(),
                    "desktop: this agent is not an injection target"
                );
                if let DesktopCmd::Inject { text, .. } = cmd {
                    self.followups.push_back(Command::OrderResult {
                        principal,
                        agent,
                        text,
                        ok: false,
                        reason: Some(refusal.reason().to_string()),
                    });
                }
                return;
            }
        };
        let job = match cmd {
            // Sent to the desktop thread above, before any agent was resolved.
            DesktopCmd::Keystroke { .. } => return,
            DesktopCmd::Focus { agent } => DesktopJob::Focus { agent, target },
            DesktopCmd::Inject { agent, text } => {
                // The card half of guard 2, **read again here** rather than
                // inherited as a constant. The reducer refuses
                // `dialog_open` before `order.dispatched`, so in the
                // ordinary case this is `false` — but a hardcoded `false`
                // gave the worker's own re-check no producer at all, which
                // is a guard that cannot fail and therefore is not one.
                // `Card::is_open_dialog` is the reducer's own predicate, so
                // the two sides cannot drift.
                let dialog_open = self
                    .store
                    .snapshot()
                    .agents
                    .get(&agent)
                    .and_then(|a| a.card.as_ref())
                    .is_some_and(deck_model::Card::is_open_dialog);
                DesktopJob::Inject {
                    agent,
                    target,
                    text,
                    dialog_open,
                }
            }
        };
        if !self.desktop.engine() {
            tracing::debug!(?job, "desktop: no injector in this build");
            if let DesktopJob::Inject { agent, text, .. } = job {
                self.followups.push_back(Command::OrderResult {
                    principal,
                    agent,
                    text,
                    ok: false,
                    reason: Some("no_injector".into()),
                });
            }
            return;
        }
        if let Some(DesktopJob::Inject { agent, text, .. }) = self.desktop.send_or_return(job) {
            // A full queue means Ghostty is wedged; there is no
            // re-projection to cover the drop and re-queueing an order the
            // operator dictated is worse than saying it was lost.
            tracing::warn!("core→desktop full; the order is dropped");
            self.desktop_dropped += 1;
            self.followups.push_back(Command::OrderResult {
                principal,
                agent,
                text,
                ok: false,
                reason: Some("undeliverable".into()),
            });
        }
    }

    /// One outcome from `deckd-desktop`.
    pub fn on_from_desktop(&mut self, msg: FromDesktop, now: Instant) {
        match msg {
            FromDesktop::Order {
                agent,
                text,
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
                    },
                    now,
                );
            }
            // Silent on the device — the `focus{ack}` already went out
            // (§6.4) — and a log line the GUI's diagnostics bundle carries.
            FromDesktop::Focus {
                agent,
                selected,
                reason,
            } => tracing::info!(agent = %agent.0, selected, ?reason, "desktop: focus"),
            FromDesktop::Automation { denied } => {
                tracing::warn!(denied, "desktop: automation permission changed");
                self.automation_denied = Some(denied);
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
            FromDesktop::Pins(map) => {
                if self.state.terminal_map != map {
                    self.state.terminal_map = map;
                    self.state_dirty.get_or_insert(now);
                }
            }
        }
    }

    /// An agent that has gone gets its tab unpinned (§5.5 step 7, M7).
    ///
    /// Driven off `agent.removed` rather than off a `SessionEnd` hook,
    /// because every way an agent can go — the hook, `agent.release`, a
    /// lease expiry, the liveness poller's `gone`, shutdown — ends in that
    /// one event, and a tab left reading `deck:cd34` after the session
    /// behind it died is a title the operator cannot get rid of without
    /// knowing about us. The job names the `ag_…` id: by the time the event
    /// exists the agent is out of the snapshot, so its key cannot be read
    /// any more, and the resolver remembers both.
    fn unpin_removed(&mut self, events: &[Event]) {
        if !self.desktop.engine() {
            return;
        }
        for event in events.iter().filter(|e| e.kind == EventKind::AgentRemoved) {
            if let Some(agent) = event_agent(event) {
                // A refused unpin is not worth a retry queue: shutdown
                // unpins everything that is left anyway.
                let _ = self.desktop.send_or_return(DesktopJob::Release {
                    key: agent.0.clone(),
                });
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

    /// Install a real desktop worker's handle (week 5). Before this, and in
    /// every build without one, `Effect::Desktop` fails the order with
    /// `no_injector` rather than pretending it was typed.
    pub fn set_desktop(&mut self, desktop: DesktopHandle) {
        self.desktop = desktop;
    }

    /// Orders a full desktop queue refused (`metrics.get`).
    #[must_use]
    pub fn desktop_dropped(&self) -> u64 {
        self.desktop_dropped
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
            for slot in 1..=Slot::MAX {
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
        // The countdowns under the usage bars are written to the minute.
        if self.usage.due(now) {
            self.push_usage(now);
        }
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
            FromHid::Metric(m) => self.on_metric(m, now),
        }
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
pub fn run(opts: RunOptions) -> Result<i32, CoreError> {
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
    let (state, loaded) = State::load(&opts.paths.state);
    if loaded == crate::state::Loaded::Corrupt {
        tracing::warn!("state.json was corrupt; moved aside and started empty");
    }
    let sid = new_sid();
    let started = Instant::now();
    let mut cfg = StoreConfig::new(started, wall_now(), sid);
    cfg.port = opts.config.api.port;
    cfg.grace_s = opts.config.agents.grace_s;
    cfg.ready_ttl_s = opts.config.cards.ready_ttl_s;
    cfg.hold_s = opts.config.hooks.claude.hold_s;
    cfg.pins = opts.config.pins();
    cfg.announce_questions = opts.config.voice.announce_questions;
    cfg.voice_pool = opts.config.voice_pool();
    cfg.announce_max_chars = opts.config.voice.tts.announce_max_chars;
    cfg.companion_stt = opts.config.voice.stt.companion;
    cfg.persisted = state.persisted();
    let store = Store::new(cfg);
    // The deck's voice link (§7): one handle, two ports. Its Bluetooth port
    // attaches by itself (CoreBluetooth, to a deck the system holds
    // connected); its cable port is the hid thread's, driven whenever the
    // instance it opened is the USB one. Only with the BLE transport, and
    // never for `--no-device`: a door test may not touch the radio.
    #[cfg(target_os = "macos")]
    let voice_link = (!opts.no_device
        && opts.config.voice.transport == crate::config::VoiceTransport::Ble)
        .then(crate::ble::BleAudio::spawn);
    #[cfg(not(target_os = "macos"))]
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
    });
    let mut core = Core::new(store, state, hid, opts.paths.clone());
    // The voice engine (§7). `--no-device` leaves `AudioHandle::none()` in
    // place, which is exactly the daemon the seam pass shipped: every
    // `Effect::Audio` is logged and forgotten and the §7.4 machine still
    // runs. The `api` gate step runs with it, and must: a door test may not
    // open a microphone.
    #[cfg(target_os = "macos")]
    if !opts.no_device {
        core.tts_settings
            .store(Arc::new(crate::audio::cloud::Settings::from_config(&opts.config)));
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
            fallback_input: opts.config.voice.fallback_input.clone(),
            link: voice_link,
            tts_engine: opts.config.voice.tts.engine.clone(),
            sidecar: Some(crate::audio::sidecar_config(&opts.config, &opts.paths)),
            cloud: Some(core.tts_settings.clone()),
            stt: Some(whisper),
            local_tts: Some(kokoro),
            companion: Some(core.transcribe.clone()),
        }));
    }
    // The desktop side (§5.5). `--no-device` leaves `DesktopHandle::none()`
    // in place, which is what the `api` gate step runs with: a door test may
    // not pin a tab or type into a terminal.
    #[cfg(target_os = "macos")]
    if !opts.no_device {
        let stored = core.state.terminal_map.clone();
        core.set_desktop(crate::desktop::spawn(
            crate::desktop::DesktopConfig::from_config(&opts.config),
            &stored,
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
        let channels = tokio::spawn(crate::claude_channel::serve(
            api.paths.data.clone(),
            api.core.adapters().claude_channels.clone(),
            stop_liveness.subscribe(),
        ));
        let system = tokio::spawn(crate::system_status::run(api.core.clone(), stop_liveness.subscribe()));
        let usage = tokio::spawn(crate::usage::claude::run(api, usage_rx));
        (served, stop_liveness, (liveness, codexd, usage, channels, system))
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
        let (liveness, codexd, usage, channels, system) = liveness;
        let _ = tokio::time::timeout(Duration::from_secs(2), liveness).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), codexd).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), usage).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), channels).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), system).await;
        served.shutdown().await;
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(watch);
    handle.send_blocking(CoreMsg::Shutdown);
    match core_thread.join() {
        Ok(core) => {
            // Dropping the job sender ends `deckd-desktop`, whose last act
            // is to unpin every tab it pinned (§6: leave the operator's
            // Ghostty as we found it). Joined before the hid thread because
            // it is the one that still touches something outside us.
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
            // daemon with no injector has everything else to do.
            recv(core.desktop.events) -> msg => if let Ok(msg) = msg {
                core.on_from_desktop(msg, Instant::now());
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
            let (core, ops, lines, debug, dir) = detached_core(name);
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
            assert!(
                matches!(&result, FromDesktop::Order { agent: to, text: sent, result: Ok(()) } if to == &agent && sent == text),
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
        assert_eq!(removes.len(), usize::from(Slot::MAX) + 1, "{removes:?}");
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
                meta: None,
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
            text, ctx, card, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("the poll owner did not get its order");
        };
        assert_eq!(
            (text.as_str(), ctx.as_str(), card),
            ("and then deploy", "talk", None)
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

    #[test]
    fn sids_are_nonzero_and_vary() {
        let a = new_sid();
        let b = new_sid();
        assert_ne!(a, 0);
        assert!(a != b || new_sid() != a);
        assert!(wall_now().millis() > 1_700_000_000_000);
    }
}
