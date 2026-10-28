//! The reducer: `Store::apply(Command, now) -> (Vec<Event>, Vec<Effect>)`,
//! the device projection and the per-scope redaction. This file holds the
//! store itself, the link/device-line handlers, the projection and the
//! redaction; the API methods, roster, cards, voice, device methods and the
//! timers live in their own `impl Store` modules (`api`, `roster`, `cards`,
//! `voice`, `devapi`, `tick`).
use crate::{
    Agent, AgentId, AgentState, CardId, CardStatus, Command, Control, DeviceLine, DeviceOp,
    DocKind, Effect, Event, EventKind, FaceEntry, FocusSource, HostLine, Key, Kind, Link,
    LinkEvent, OriginKind, Principal, Scope, SessionState, Settings, Slot, Snapshot, StatePatch,
    TitleSource, Ts, Usage,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// `[agents]` pins from `config.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinConfig {
    pub slot: Option<Slot>,
    pub voice: Option<String>,
    pub name: Option<String>,
}

/// `state.json.slots[key]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotMemory {
    pub slot: Slot,
    pub last_seen: Ts,
    pub kind: Kind,
    pub cwd: Option<String>,
}

/// `state.json.macros[f]`: a Macropad face's keys as last set, by the API or
/// the deck's own editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroLayout {
    pub src: Option<String>,
    pub keys: Vec<crate::MacroKey>,
}

/// What `state.json` contributes at start.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Persisted {
    pub faces: Vec<FaceEntry>,
    pub settings: Settings,
    pub slots: BTreeMap<String, SlotMemory>,
    /// slot → last card id.
    pub card_ids: BTreeMap<u8, u16>,
    /// `state.json.voices`: agent key → voice id (§9, §7.7). Restored so
    /// an agent keeps the voice it was given, rather than being handed a
    /// new one every morning.
    pub voices: BTreeMap<String, String>,
    pub usage: Option<Usage>,
    /// face id → its keys, so a Macropad comes back as it was left.
    #[serde(default)]
    pub macros: BTreeMap<u8, MacroLayout>,
}

#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// The `Instant` at which `wall` was observed; every `ts` derives from it.
    pub started: Instant,
    pub wall: Ts,
    pub version: String,
    pub port: u16,
    /// This daemon's session id (nonzero, chosen once per start).
    pub sid: u32,
    pub grace_s: u32,
    pub ready_ttl_s: u32,
    pub hold_s: u32,
    pub lease_ttl_s: u32,
    /// `[voice.stt] locale` (B3): the recogniser is built per locale, so
    /// `AudioCmd::Listen` has to carry it and the reducer has to know it.
    /// Hot (B14), because a `config.reload` that changed the language and
    /// did not reach the next press would be a silent lie.
    pub locale: String,
    /// `[voice] announce_questions` (§7.7): speak a new Question card even
    /// when the operator is already looking at the Agents face. Hot (B14),
    /// because it is read where it is used — the next card.
    pub announce_questions: bool,
    /// `[voice.voices] default_pool` (§7.7): the voices an agent that
    /// named none is assigned from, round-robin. Hot (B14): read at the
    /// next registration.
    pub voice_pool: Vec<String>,
    /// `[voice.tts] announce_max_chars` (§7.6): the budget, in
    /// **characters**, for every announcement the daemon composes. Hot
    /// (B14): read at the next announcement.
    pub announce_max_chars: usize,
    pub pins: BTreeMap<String, PinConfig>,
    pub persisted: Persisted,
}

impl StoreConfig {
    /// Design §9 defaults with an empty `state.json`.
    pub fn new(started: Instant, wall: Ts, sid: u32) -> Self {
        StoreConfig {
            started,
            wall,
            version: env!("CARGO_PKG_VERSION").to_string(),
            port: 47201,
            sid,
            grace_s: 300,
            ready_ttl_s: 900,
            hold_s: 120,
            lease_ttl_s: 30,
            locale: "en-US".into(),
            announce_questions: false,
            voice_pool: ["Samantha", "Daniel", "Karen", "Rishi"]
                .map(String::from)
                .to_vec(),
            announce_max_chars: 120,
            pins: BTreeMap::new(),
            persisted: Persisted::default(),
        }
    }
}

/// The answer → `card clear` budget (§4.8, §6.1: the device reverts at 5 s).
pub const DELIVERY_BUDGET: Duration = Duration::from_secs(4);
/// The PTT capture cap (§7.4; Apple's one-minute server limit).
pub const PTT_CAP: Duration = Duration::from_secs(60);
/// §7.4's `Arming` liveness deadline: the press → **first non-zero sample**
/// budget, after which the microphone is reported unavailable (V2).
///
/// §7.4 wrote 1.5 s, against a bring-up nobody had measured. The
/// measurement (V1, 2026-09-21, n=16) puts bring-up at 1031 ms median and
/// **1246 ms at worst**, so 1.5 s leaves 254 ms of margin over the slowest
/// press anybody has seen — and the failure it buys is the expensive
/// direction: a working microphone declared dead, a capture aborted, and
/// the operator's sentence thrown away. Reporting a genuinely dead mic a
/// second later costs nothing by comparison, so the deadline is twice the
/// measured worst case.
///
/// It must also stay **above** `[voice] slow_sco_ms` (1500 ms): a session
/// that died before its own "this was slow" report could be filed would
/// make that report unreachable.
pub const ARMING_LIVENESS: Duration = Duration::from_millis(2500);
/// Last-resort watchdog, longer than the audio engine's final-decode budget.
/// Expiry fails the utterance; a partial transcript must never be dispatched.
pub const FINALIZE_BUDGET: Duration = Duration::from_secs(120);
/// `refresh{what:"usage"}` with no feeder answer → re-send the cached usage.
pub const USAGE_REFRESH_GRACE: Duration = Duration::from_millis(1500);

/// Per-agent bookkeeping the snapshot does not show: deadlines as `Instant`s
/// (timers are stateless — a `Tick` looks here, never at the timer key).
#[derive(Debug, Default)]
pub(crate) struct AgentAux {
    pub(crate) controls: Option<crate::session_controls::SessionControls>,
    pub(crate) settings_pending: Option<Instant>,
    pub(crate) settings_request_rev: Option<u16>,
    /// Last requested reading window; updates must not pull the reader away.
    pub(crate) reply_first: Option<u16>,
    /// Opt-in: older firmware continues to receive only chat documents.
    pub(crate) workflow_view: bool,
    /// Tool message ids explicitly expanded on the deck (bounded by history).
    pub(crate) expanded_tools: std::collections::BTreeSet<String>,
    pub(crate) lease_ttl: Duration,
    pub(crate) lease_at: Option<Instant>,
    pub(crate) orphaned_at: Option<Instant>,
    /// The title replaced by "(disconnected)" while orphaned.
    pub(crate) saved_title: Option<(Option<String>, TitleSource)>,
    /// The state before a question card flipped the agent to `question`.
    pub(crate) prev_state: Option<AgentState>,
    pub(crate) last_active: Option<Instant>,
    /// The 4 s answer → clear budget.
    pub(crate) deliver_at: Option<Instant>,
    /// `hold_s`, shared by every card of one question group.
    pub(crate) hold_at: Option<Instant>,
    /// Ready TTL or `timeout_s`.
    pub(crate) expire_at: Option<Instant>,
    /// Card ids of an agent that is not on the deck (re-allocated per slot
    /// when it gets one).
    pub(crate) offdeck_last: CardId,
}

#[derive(Debug)]
pub struct Store {
    pub(crate) session_panel: Option<crate::session_controls::Panel>,
    pub(crate) session_menu_rev: u16,
    pub(crate) cfg: StoreConfig,
    pub(crate) snapshot: Snapshot,
    /// Last `now` seen by `apply`, for `project()` and event stamps.
    pub(crate) now: Instant,
    pub(crate) card_ids: BTreeMap<Slot, CardId>,
    pub(crate) slots: BTreeMap<String, SlotMemory>,
    /// Events and effects of the apply in progress.
    pub(crate) events: Vec<Event>,
    pub(crate) effects: Vec<Effect>,
    pub(crate) aux: BTreeMap<AgentId, AgentAux>,
    pub(crate) agent_seq: u64,
    pub(crate) utterance_seq: u64,
    /// Whether a Question card was pending the last time anything looked,
    /// so `AudioCmd::Warm` is an **edge** and not a command per apply.
    pub(crate) warm_pending: bool,
    /// `state.json.voices` as the reducer holds it (§7.7): agent key →
    /// voice id, both remembered and newly assigned.
    pub(crate) voices: BTreeMap<String, String>,
    /// Where the round-robin is up to. It starts past whatever
    /// `state.json` already remembers, so a restart with three agents on
    /// file does not hand the fourth the first voice again.
    pub(crate) voice_seq: usize,
    /// The last **final** transcript and the slot it belonged to, kept only so
    /// a `doclines` request for it can be answered. Partials are not kept: they
    /// are replaced every few hundred milliseconds and are never laid out.
    pub(crate) transcript_doc: Option<(Slot, String)>,
    /// Bumped with each final transcript; `DocText.id` for `Tr`.
    pub(crate) transcript_rev: u16,
    /// Desired `macro` line per Macropad face (device-facing only).
    pub(crate) macros: BTreeMap<u8, HostLine>,
    /// Desired `ws` line (device-facing only).
    pub(crate) ws: Option<HostLine>,
    pub(crate) usage_refresh_at: Option<Instant>,
    pub(crate) ptt_cap_at: Option<Instant>,
    /// §7.4's `Arming` deadline, cleared by the first non-zero sample.
    pub(crate) arming_at: Option<Instant>,
    /// §7.4's `Finalizing` deadline, cleared by the final transcript.
    pub(crate) finalizing_at: Option<Instant>,
    /// The released sessions waiting for their final transcripts (§7.4's
    /// `Finalizing`), oldest first.
    ///
    /// `snapshot.voice.ptt` means "a capture is open", so the release has
    /// to clear it — but the dispatch happens when `VoiceIn::Final` lands,
    /// once the recogniser finishes, and it needs the agent, the ctx, the card, the
    /// principal and the session the press had. Keeping it here rather than
    /// in the snapshot is what lets the two facts differ: the operator has
    /// let go, and the words are still on their way.
    ///
    /// It is a **queue** because the operator can press again inside that
    /// wait — a double tap is two sessions released before either has its
    /// words — and because the engine answers one `Finish{abort:false}`
    /// with exactly one `VoiceIn::Final`, in order. The front of this queue
    /// is therefore always the session the next final belongs to.
    pub(crate) finalizing: VecDeque<Finalizing>,
    /// A `channel` line that arrived while a capture was open, held until
    /// the release (§7.4, M6). Last one wins: the operator can turn the
    /// knob twice, and only where it ends up is a focus.
    pub(crate) pending_channel: Option<(Slot, u16)>,
}

/// One released session waiting for its final transcript.
#[derive(Debug)]
pub(crate) struct Finalizing {
    /// None after cancellation or watchdog failure. Keep this tombstone
    /// until the engine answers so late results cannot reach a later press.
    pub(crate) ptt: Option<crate::PttInfo>,
    /// This session's own `FINALIZE_BUDGET` deadline.
    pub(crate) deadline: Instant,
}

impl Store {
    /// A released PTT still has a final recognition result pending.
    pub fn is_finalizing(&self) -> bool { !self.finalizing.is_empty() }

    pub fn new(cfg: StoreConfig) -> Self {
        let mut snapshot = Snapshot {
            settings: cfg.persisted.settings,
            faces: cfg.persisted.faces.clone(),
            usage: cfg.persisted.usage.clone(),
            ..Snapshot::default()
        };
        snapshot.device.sid = cfg.sid;
        snapshot.device.settings = cfg.persisted.settings;
        snapshot.device.pace_ms = 8;
        snapshot.voice.stt.locale.clone_from(&cfg.locale);
        snapshot.service.version = cfg.version.clone();
        snapshot.service.port = cfg.port;
        let card_ids = cfg
            .persisted
            .card_ids
            .iter()
            .map(|(slot, id)| (Slot(*slot), CardId(*id)))
            .collect();
        let voices = cfg.persisted.voices.clone();
        let macros = cfg
            .persisted
            .macros
            .iter()
            .map(|(f, layout)| {
                (
                    *f,
                    HostLine::Macro {
                        face: *f,
                        src: layout.src.clone(),
                        keys: layout.keys.clone(),
                    },
                )
            })
            .collect();
        Store {
            now: cfg.started,
            slots: cfg.persisted.slots.clone(),
            card_ids,
            cfg,
            snapshot,
            events: Vec::new(),
            effects: Vec::new(),
            aux: BTreeMap::new(),
            agent_seq: 0,
            utterance_seq: 0,
            warm_pending: false,
            voice_seq: voices.len(),
            voices,
            transcript_doc: None,
            transcript_rev: 0,
            macros,
            ws: None,
            usage_refresh_at: None,
            ptt_cap_at: None,
            arming_at: None,
            finalizing_at: None,
            finalizing: VecDeque::new(),
            pending_channel: None,
            session_panel: None,
            session_menu_rev: 0,
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }

    /// Apply a reloaded `config.toml`'s hot keys (host contract §3.7):
    /// `agents.grace_s`, `cards.ready_ttl_s`, `hooks.*.hold_s` and the slot
    /// pins, all of which are read where they are used (lease and grace
    /// timers, the Ready TTL, the hold budget, slot resolution), so a new
    /// value takes effect from the next timer or registration on.
    /// Identity and persisted state (`started`, `wall`, `sid`, `version`,
    /// `persisted`) and `port` (owned by `Command::ServiceBound`) are never
    /// touched, so the caller may pass a freshly built `StoreConfig`.
    pub fn reconfigure(&mut self, cfg: &StoreConfig) {
        self.cfg.grace_s = cfg.grace_s;
        self.cfg.ready_ttl_s = cfg.ready_ttl_s;
        self.cfg.hold_s = cfg.hold_s;
        // B14: `[voice]` is hot too. The locale is read where it is used —
        // the next `AudioCmd::Listen` — so a reload takes effect from the
        // next press on, and the snapshot's `voice.stt` says so at once.
        self.cfg.locale.clone_from(&cfg.locale);
        if self.snapshot.voice.stt.locale != cfg.locale {
            self.snapshot.voice.stt.locale.clone_from(&cfg.locale);
        }
        self.cfg.announce_questions = cfg.announce_questions;
        self.cfg.voice_pool.clone_from(&cfg.voice_pool);
        self.cfg.announce_max_chars = cfg.announce_max_chars;
        self.cfg.pins.clone_from(&cfg.pins);
    }

    /// The wall clock at `now`.
    pub fn ts(&self, now: Instant) -> Ts {
        Ts::at(self.cfg.wall, self.cfg.started, now)
    }

    /// The wall clock of the apply in progress.
    pub(crate) fn ts_now(&self) -> Ts {
        self.ts(self.now)
    }

    /// The next card id for `slot`: `last + 1`, wrap skipping 0, persisted
    /// (as an effect) before the caller emits the card line.
    pub fn next_card_id(&mut self, slot: Slot) -> CardId {
        let next = self
            .card_ids
            .get(&slot)
            .copied()
            .unwrap_or(CardId(0))
            .next();
        self.card_ids.insert(slot, next);
        self.effects
            .push(Effect::Persist(StatePatch::CardId { slot, last: next }));
        next
    }

    /// Forget a slot's card counter (agent gone → no collision on reassignment).
    pub fn reset_slot(&mut self, slot: Slot) {
        if self.card_ids.remove(&slot).is_some() {
            self.effects.push(Effect::Persist(StatePatch::CardId {
                slot,
                last: CardId(0),
            }));
        }
    }

    pub fn slot_memory(&self) -> &BTreeMap<String, SlotMemory> {
        &self.slots
    }

    pub fn apply(&mut self, cmd: Command, now: Instant) -> (Vec<Event>, Vec<Effect>) {
        self.now = now;
        match cmd {
            Command::SystemStatus(status) => { if status.valid() { self.device_op(DeviceOp::Put(Key::System, HostLine::System(status))); } },
            Command::Link(link) => self.on_link(link),
            Command::Device(line) => self.on_device(line),
            Command::SessionControls { agent, session, settled, controls } => self.on_session_controls(agent, session, settled, controls),
            Command::Voice(v) => self.on_voice(v),
            Command::Tick(_) => self.on_tick(),
            Command::Shutdown => self.on_shutdown(),
            Command::Api {
                principal,
                session,
                req,
            } => {
                // The result goes to the caller of `call`; a refused request
                // changes nothing and is not an event.
                let _ = self.on_api(principal, session, *req);
            }
            Command::HoldWritten { agent, card } => self.on_hold_written(&agent, card),
            Command::HoldGone { agent, card } => self.on_hold_gone(&agent, card),
            Command::Delivered { agent, card, ok } => self.on_delivered(&agent, card, ok),
            Command::OrderResult {
                principal,
                agent,
                text,
                ok,
                reason,
            } => self.on_order_result(principal, &agent, &text, ok, reason.as_deref()),
            Command::ConfirmRequested {
                principal,
                agent,
                card,
                order,
            } => self.emit(
                EventKind::OpConfirmRequested,
                principal.clone(),
                serde_json::json!({
                    "agent": agent, "card": card, "order": order,
                    "principal": principal, "need": "trusted",
                }),
            ),
            Command::SessionClosed(session) => self.on_session_closed(session),
            Command::ServiceBound {
                port,
                degraded,
                listener,
            } => self.on_service_bound(port, degraded, listener),
        }
        self.take()
    }

    /// The TCP door's bind result: the bound port is a fact of the snapshot;
    /// a change of `degraded` (or of who holds the port) is the
    /// `service.degraded{reason, port, listener}` event of §4.1/§4.12.
    fn on_service_bound(
        &mut self,
        port: Option<u16>,
        degraded: Option<String>,
        listener: Option<crate::Listener>,
    ) {
        if let Some(port) = port {
            self.snapshot.service.port = port;
        }
        if self.snapshot.service.degraded != degraded || self.snapshot.service.listener != listener
        {
            self.snapshot.service.degraded = degraded.clone();
            self.snapshot.service.listener = listener.clone();
            let port = self.snapshot.service.port;
            self.emit(
                EventKind::ServiceDegraded,
                Principal::Uid(0),
                serde_json::json!({"reason": degraded, "port": port, "listener": listener}),
            );
        }
    }

    pub(crate) fn take(&mut self) -> (Vec<Event>, Vec<Effect>) {
        self.sync_warm();
        (
            std::mem::take(&mut self.events),
            std::mem::take(&mut self.effects),
        )
    }

    /// §7.1's `[voice] mic_warm = "card_pending"`, as a fact the audio side
    /// can act on.
    ///
    /// Every exit from the reducer runs through [`Store::take`], so the
    /// edge is computed here rather than threaded through the dozen places
    /// a card can appear, be answered, expire, be snoozed or be cleared.
    /// The audio side owns the *policy* (`"never"` ignores this,
    /// `"always"` needs no signal); this owns only the fact.
    fn sync_warm(&mut self) {
        let pending = self.snapshot.agents.values().any(|a| {
            a.card.as_ref().is_some_and(|c| {
                c.kind == crate::CardKind::Q && c.status == crate::CardStatus::Pending
            })
        });
        if pending != self.warm_pending {
            self.warm_pending = pending;
            self.effects
                .push(Effect::Audio(crate::AudioCmd::Warm { pending }));
        }
    }

    /// The device projection of the current snapshot (seeds the hid thread,
    /// re-sent after an overflow), in barrier order (§6.5), with the
    /// device-only `macro`/`ws` lines the snapshot does not carry.
    pub fn project(&self) -> Vec<DeviceOp> {
        let mut ops = project_with(&self.snapshot, self.ts(self.now), Some(&self.aux));
        ops.extend(
            self.macros
                .iter()
                .map(|(f, line)| DeviceOp::Put(Key::Macro(crate::FaceId(*f)), line.clone())),
        );
        if let Some(ws) = &self.ws {
            ops.push(DeviceOp::Put(Key::Ws, ws.clone()));
        }
        ops.sort_by_key(|op| match op {
            DeviceOp::Put(key, _) | DeviceOp::Remove(key) => Some(*key),
            _ => None,
        });
        ops
    }

    /// The snapshot as a principal with `scopes` sees it (`crate::redact`).
    pub fn redact(&self, scopes: &[Scope]) -> Snapshot {
        crate::redact::redact(&self.snapshot, scopes)
    }

    pub(crate) fn emit(&mut self, kind: EventKind, principal: Principal, data: serde_json::Value) {
        self.snapshot.rev += 1;
        self.events.push(Event {
            rev: self.snapshot.rev,
            ts: self.ts(self.now),
            kind,
            principal,
            data,
        });
    }

    pub(crate) fn device_op(&mut self, op: DeviceOp) {
        self.effects.push(Effect::Device(op));
    }

    pub(crate) fn timer(&mut self, key: crate::TimerKey, at: Instant) {
        self.effects.push(Effect::Timer { key, at });
    }

    /// The deck's channel knob: focus the agent on that slot (§6.5, §4.7).
    fn apply_channel(&mut self, slot: Slot, seq: u16) {
        let agent = self.snapshot.agent_at(slot).map(|a| a.id.clone());
        self.snapshot.focus = crate::Focus {
            agent: agent.clone(),
            seq: Some(seq),
            source: Some(FocusSource::Device),
        };
        if let Some(agent) = &agent {
            if !self.snapshot.agents[agent].native_companion() {
                self.effects.push(Effect::Desktop(crate::DesktopCmd::Focus {
                    agent: agent.clone(),
                }));
            }
            self.aux.entry(agent.clone()).or_default().reply_first = None;
            self.put_reply(agent);
        }
        self.emit(
            EventKind::FocusChanged,
            Principal::Device,
            serde_json::json!({"agent": agent, "slot": slot, "seq": seq, "source": "device"}),
        );
    }

    /// §7.4's "applied after release": the channel the operator turned to
    /// while they were dictating. Called wherever a live capture closes.
    pub(crate) fn apply_pending_channel(&mut self) {
        if let Some((slot, seq)) = self.pending_channel.take() {
            self.apply_channel(slot, seq);
        }
    }

    /// `Put` the agent's state line (kind/name/state/since) for its slot.
    pub(crate) fn put_agent_state(&mut self, id: &AgentId) {
        let now = self.ts_now();
        if let Some((slot, agent)) = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.slot.zip(Some(a)))
        {
            let line = agent_state_line(slot, agent, now);
            self.device_op(DeviceOp::Put(Key::AgentState(slot), line));
        }
        if self.aux.get(id).is_some_and(|a| a.workflow_view) {
            if let Some(agent) = self.snapshot.agents.get_mut(id) {
                agent.chat_rev = agent.chat_rev.wrapping_add(1).max(1);
            }
            self.put_reply(id);
        }
    }

    pub(crate) fn put_agent_title(&mut self, id: &AgentId) {
        if let Some((slot, title)) = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.slot.zip(a.title.clone()))
        {
            self.device_op(DeviceOp::Put(
                Key::AgentTitle(slot),
                HostLine::Agent {
                    slot: slot.0,
                    kind: None,
                    name: None,
                    project: None,
                    state: None,
                    title: Some(title),
                    since_s: None,
                },
            ));
        }
    }

    /// Re-wrap and re-send every document after a change that moves line
    /// breaks — today only the `text` size setting.
    fn relayout_documents(&mut self) {
        let ids: Vec<AgentId> = self
            .snapshot
            .agents
            .iter()
            .filter(|(_, a)| a.slot.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.put_card(&id);
            self.put_reply(&id);
        }
    }

    /// The deck's `text` size setting, as the host last saw it. Everything the
    /// host lays out is wrapped with the face this picks, because the deck
    /// draws with it.
    pub(crate) fn text_size(&self) -> u8 {
        self.snapshot.settings.textsize.min(2) as u8
    }

    /// `Put` the agent's card line if the device should show it.
    pub(crate) fn put_card(&mut self, id: &AgentId) {
        if let Some((slot, card)) = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.slot.zip(a.card.as_ref()))
        {
            if card.status.on_device() {
                let cid = card.id.0;
                let size = self.snapshot.settings.textsize.min(2) as u8;
                let line = card_line(slot, card);
                let window = doc_window(slot, DocKind::Card, cid, &card_body(card), 0, size);
                self.device_op(DeviceOp::Put(Key::Card(slot, cid), line));
                if let Some(window) = window {
                    self.device_op(DeviceOp::Put(
                        Key::DocText(slot, DocKind::Card, cid),
                        window,
                    ));
                }
            }
        }
    }

    pub(crate) fn put_reply(&mut self, id: &AgentId) {
        let doc = if self.aux.get(id).is_some_and(|a| a.workflow_view) { DocKind::Workflow } else { DocKind::Reply };
        let Some((slot, text, rev)) = self.snapshot.agents.get(id).and_then(|a| {
            a.slot.zip(doc_body(a, doc))
                .map(|(slot, (text, rev))| (slot, text, rev))
        }) else {
            return;
        };
        let size = self.text_size();
        if let Some(aux) = self.aux.get_mut(id) {
            aux.expanded_tools.retain(|key| key.starts_with("workflow:") || self.snapshot.agents[id].messages.iter().any(|m| &m.message.id == key));
        }
        let first = self.aux.get(id).and_then(|aux| aux.reply_first).unwrap_or(if doc == DocKind::Workflow { 0 } else { u16::MAX });
        let window = agent_doc_window(&self.snapshot.agents[id], doc, first, size, self.aux.get(id).map(|a| &a.expanded_tools));
        self.device_op(DeviceOp::Put(
            Key::Reply(slot),
            HostLine::Reply {
                slot: slot.0,
                text,
                rev,
            },
        ));
        if let Some(window) = window {
            self.device_op(DeviceOp::Put(
                Key::DocText(slot, doc, rev),
                window,
            ));
        }
    }

    /// Everything the device shows for the agent, `agent` before `card`.
    pub(crate) fn put_agent_all(&mut self, id: &AgentId) {
        self.put_agent_state(id);
        self.put_agent_title(id);
        self.put_card(id);
        self.put_reply(id);
    }

    fn on_link(&mut self, link: LinkEvent) {
        let dev = &mut self.snapshot.device;
        let (link_state, session) = match link {
            LinkEvent::Linked => (Link::Connected, SessionState::AwaitHello),
            LinkEvent::Down => (Link::Offline, SessionState::Absent),
            LinkEvent::Degraded => (Link::Degraded, SessionState::Degraded),
            LinkEvent::Live { .. } => (Link::Connected, SessionState::Live),
            LinkEvent::Syncing { boot, .. } => {
                dev.boot = Some(boot);
                (Link::Connected, SessionState::Syncing)
            }
            LinkEvent::Synced { .. } => (Link::Connected, SessionState::Live),
            LinkEvent::Overflow => {
                // Re-seed the view and ask for a barrier (§2.3).
                let mut ops = self.project();
                ops.push(DeviceOp::Sync {
                    boot: false,
                    reason: crate::SyncReason::Overflow,
                });
                self.effects.extend(ops.into_iter().map(Effect::Device));
                return;
            }
            LinkEvent::PaceBackoff { pace_ms } => {
                dev.pace_ms = pace_ms;
                self.emit(
                    EventKind::Error,
                    Principal::Device,
                    serde_json::json!({"context": "pace_backoff", "pace_ms": pace_ms}),
                );
                return;
            }
        };
        let changed = dev.link != link_state || dev.session != session;
        dev.link = link_state;
        dev.session = session;
        dev.host_online = matches!(session, SessionState::Live | SessionState::Syncing);
        if changed {
            let data = serde_json::json!({
                "link": link_state, "sid": self.snapshot.device.sid, "fw": self.snapshot.device.fw,
                "boot": self.snapshot.device.boot, "host_online": self.snapshot.device.host_online,
                "session": session,
            });
            self.emit(EventKind::DeviceLink, Principal::Device, data);
        }
        if matches!(link, LinkEvent::Down) {
            self.snapshot.device.fw = None;
            self.snapshot.device.face = None;
            // §6.3 Absent: answers in flight cannot be confirmed, the utterance is lost.
            self.fail_answering_cards("link");
            self.abort_all_ptt("link");
        }
    }

    fn on_device(&mut self, line: DeviceLine) {
        match line {
            DeviceLine::Hello {
                fw,
                boot,
                faces,
                rx,
                ..
            } => {
                let dev = &mut self.snapshot.device;
                dev.fw = Some(fw);
                dev.boot = Some(boot);
                dev.faces = faces.clone();
                if rx != crate::LINE_MAX_U16 {
                    self.emit(
                        EventKind::Error,
                        Principal::Device,
                        serde_json::json!({"context": "hello.rx", "rx": rx}),
                    );
                }
                if !boot && self.snapshot.faces != faces {
                    // boot:0 → adopt the device's strip.
                    self.snapshot.faces = faces.clone();
                    self.effects
                        .push(Effect::Persist(StatePatch::Faces(faces.clone())));
                    self.emit(
                        EventKind::DeviceFaces,
                        Principal::Device,
                        serde_json::json!({"list": faces}),
                    );
                }
                self.emit(
                    EventKind::DeviceSession,
                    Principal::Device,
                    serde_json::json!({"boot": boot, "fw": self.snapshot.device.fw}),
                );
            }
            DeviceLine::Channel { slot, seq } => {
                let slot = Slot(slot);
                // The ack is about ordering, not the PC's success: immediate (P:118).
                self.effects
                    .push(Effect::Device(DeviceOp::Control(Control::Focus {
                        slot,
                        ack: Some(seq),
                    })));
                // A device-sourced focus must not be re-pushed by a barrier (§6.5).
                self.device_op(DeviceOp::Remove(Key::Focus));
                if self.snapshot.voice.ptt.is_some() {
                    // §7.4 (G:222-223): a `channel` that arrives mid-session
                    // is applied **after the release**. Raising another
                    // terminal under the operator's hands while they are
                    // dictating is the thing that rule exists to stop; the
                    // deck has already been told the knob turned.
                    self.pending_channel = Some((slot, seq));
                } else {
                    self.apply_channel(slot, seq);
                }
            }
            DeviceLine::Set { key, value } => {
                let dev_changed = self.snapshot.device.settings.set(key, value);
                let changed = self.snapshot.settings.set(key, value);
                if (changed || dev_changed) && key == crate::SetKey::Textsize {
                    // Every window the deck is holding was wrapped for the old
                    // face. Its line breaks are wrong for the new one, so the
                    // documents are laid out again and re-sent. Until they
                    // land the deck keeps drawing in the face its window was
                    // measured with, so the two never disagree mid-flight.
                    self.relayout_documents();
                }
                if changed || dev_changed {
                    self.effects
                        .push(Effect::Persist(StatePatch::Setting { key, value }));
                    // B4: the deck's own volume buttons are the same knob
                    // as `device.set`, so they reach the mixer by the same
                    // route.
                    self.push_gain(key, value);
                    self.emit(
                        EventKind::DeviceSet,
                        Principal::Device,
                        serde_json::json!({"k": key, "v": value}),
                    );
                }
            }
            DeviceLine::Faces { list } => {
                self.snapshot.device.faces = list.clone();
                if self.snapshot.faces != list {
                    self.snapshot.faces = list.clone();
                    self.effects
                        .push(Effect::Persist(StatePatch::Faces(list.clone())));
                    self.emit(
                        EventKind::DeviceFaces,
                        Principal::Device,
                        serde_json::json!({"list": list}),
                    );
                }
            }
            DeviceLine::Face { f } => {
                if self.snapshot.device.face != Some(f) {
                    self.snapshot.device.face = Some(f);
                    self.emit(
                        EventKind::DeviceFace,
                        Principal::Device,
                        serde_json::json!({"f": f}),
                    );
                }
            }
            DeviceLine::Err { why } => {
                let counts = &mut self.snapshot.device.err_counts;
                match why {
                    crate::ErrWhy::Range => counts.range += 1,
                    crate::ErrWhy::Parse => counts.parse += 1,
                    crate::ErrWhy::Size => counts.size += 1,
                    crate::ErrWhy::Unknown => counts.unknown += 1,
                }
                self.emit(
                    EventKind::Error,
                    Principal::Device,
                    serde_json::json!({"context": "device.err", "why": why}),
                );
            }
            DeviceLine::Refresh { what } => {
                self.emit(
                    EventKind::UsageRefreshRequested,
                    Principal::Device,
                    serde_json::json!({"what": what}),
                );
                let at = self.now + USAGE_REFRESH_GRACE;
                self.usage_refresh_at = Some(at);
                self.timer(crate::TimerKey::UsageRefresh, at);
            }
            DeviceLine::Key { k, act, shift, f } => {
                self.emit(
                    EventKind::DeviceKey,
                    Principal::Device,
                    serde_json::json!({"k": k, "act": act, "shift": shift, "f": f}),
                );
            }
            DeviceLine::Macro { f, i, act } => {
                self.emit(
                    EventKind::DeviceMacro,
                    Principal::Device,
                    serde_json::json!({"f": f, "i": i, "act": act}),
                );
                // A press or a wheel step posts the key's keystroke; a
                // release does nothing (the stroke is a down-up pair).
                if act != 0 {
                    let keys = match self.macros.get(&f) {
                        Some(HostLine::Macro { keys, .. }) => keys.get(usize::from(i)).and_then(|key| key.keys.clone()),
                        _ => None,
                    };
                    if let Some(keys) = keys {
                        self.effects
                            .push(Effect::Desktop(crate::DesktopCmd::Keystroke { keys }));
                    }
                }
            }
            DeviceLine::MacroKey {
                f,
                i,
                label,
                binding,
                keys: stroke,
            } => {
                // The slot lands in the face's line (an empty label keeps
                // later keys in place; trailing empties are dropped), which
                // is persisted and sent back whole.
                let mut line = self.macros.remove(&f).unwrap_or(HostLine::Macro {
                    face: f,
                    src: None,
                    keys: Vec::new(),
                });
                let empty = crate::MacroKey {
                    label: String::new(),
                    binding: None,
                    keys: None,
                };
                if let HostLine::Macro { keys, .. } = &mut line {
                    let at = usize::from(i.min(8));
                    if keys.len() <= at {
                        keys.resize(at + 1, empty.clone());
                    }
                    keys[at] = crate::MacroKey {
                        label: label.clone().unwrap_or_default(),
                        binding,
                        keys: stroke.clone(),
                    };
                    while keys.last().is_some_and(|key| key.label.is_empty()) {
                        keys.pop();
                    }
                }
                let (src, keys) = match &line {
                    HostLine::Macro { src, keys, .. } => (src.clone(), keys.clone()),
                    _ => (None, Vec::new()),
                };
                self.macros.insert(f, line.clone());
                self.effects
                    .push(Effect::Persist(StatePatch::Macro { f, src, keys }));
                self.device_op(DeviceOp::Put(Key::Macro(crate::FaceId(f)), line));
                self.emit(
                    EventKind::DeviceMacroEdit,
                    Principal::Device,
                    serde_json::json!({"f": f, "i": i, "l": label, "b": binding, "k": stroke}),
                );
            }
            DeviceLine::WsGo(go) => {
                self.emit(
                    EventKind::DeviceWs,
                    Principal::Device,
                    serde_json::json!({"go": go}),
                );
            }
            DeviceLine::WsWin(win) => {
                self.emit(
                    EventKind::DeviceWs,
                    Principal::Device,
                    serde_json::json!({"win": win}),
                );
            }
            // `up` (P:132): release synthesised keys (none in v1), cancel PTT.
            DeviceLine::Up => self.abort_ptt("up"),
            DeviceLine::Ptt {
                act,
                slot,
                ctx,
                card,
                abort,
                press,
            } => self.on_ptt(act, Slot(slot), ctx, card.map(CardId), abort, press),
            DeviceLine::Answer { slot, card, choice } => {
                self.on_device_answer(Slot(slot), CardId(card), choice)
            }
            DeviceLine::Snooze { slot, card } => self.on_device_snooze(Slot(slot), CardId(card)),
            DeviceLine::DocLines {
                slot,
                doc,
                id,
                first,
            } => self.on_device_doc_lines(Slot(slot), doc, id, first),
            DeviceLine::Image { .. } => {},
            DeviceLine::Session { slot, field, page, rev, choice } => self.on_device_session(slot, field, page, rev, choice),
            DeviceLine::Tool { slot, id, index } => self.on_device_tool(Slot(slot), id, index),
        }
    }

    fn on_shutdown(&mut self) {
        self.emit(
            EventKind::ServiceShutdown,
            Principal::Uid(0),
            serde_json::json!({}),
        );
        // Every parked hook resolves with `{}` so the CLIs show their own dialogs.
        let held: Vec<(AgentId, CardId)> = self
            .snapshot
            .agents
            .values()
            .filter_map(|a| a.card.as_ref().map(|c| (a, c)))
            .filter(|(_, c)| c.deliver == crate::Deliver::Hold && c.status.hold_parked())
            .map(|(a, c)| (a.id.clone(), c.id))
            .collect();
        for (agent, card) in held {
            self.effects.push(Effect::ReleaseHold {
                agent,
                card,
                reason: crate::Release::Shutdown,
            });
        }
        self.abort_all_ptt("shutdown");
        let dev = &mut self.snapshot.device;
        if dev.link != Link::Offline || dev.session != SessionState::Absent {
            dev.link = Link::Offline;
            dev.session = SessionState::Absent;
            dev.host_online = false;
            self.emit(
                EventKind::DeviceLink,
                Principal::Uid(0),
                serde_json::json!({"link": "offline", "session": "absent", "host_online": false}),
            );
        }
    }
}

impl CardStatus {
    /// A hold is still parked behind the card (released on snooze/expiry/shutdown).
    pub(crate) const fn hold_parked(&self) -> bool {
        matches!(
            self,
            CardStatus::Pending | CardStatus::Snoozed | CardStatus::Answering { .. }
        )
    }
}

/// The whole desired device state of `snapshot` as `Put` ops (§6.5), in
/// barrier order. `now` gives every agent its `since` age.
pub fn project(snapshot: &Snapshot, now: Ts) -> Vec<DeviceOp> {
    project_with(snapshot, now, None)
}

fn project_with(snapshot: &Snapshot, now: Ts, aux: Option<&BTreeMap<AgentId, AgentAux>>) -> Vec<DeviceOp> {
    let mut ops = Vec::new();
    // Every document in the barrier is wrapped with the face the deck's `text`
    // setting picks, the same as the lanes do.
    let size = snapshot.settings.textsize.min(2) as u8;
    for (slot, agent) in snapshot.by_slot() {
        ops.push(DeviceOp::Put(
            Key::AgentState(slot),
            agent_state_line(slot, agent, now),
        ));
        if let Some(title) = &agent.title {
            ops.push(DeviceOp::Put(
                Key::AgentTitle(slot),
                HostLine::Agent {
                    slot: slot.0,
                    kind: None,
                    name: None,
                    project: None,
                    state: None,
                    title: Some(title.clone()),
                    since_s: None,
                },
            ));
        }
        if let Some(card) = agent.card.as_ref().filter(|c| c.status.on_device()) {
            ops.push(DeviceOp::Put(
                Key::Card(slot, card.id.0),
                card_line(slot, card),
            ));
            if let Some(window) =
                doc_window(slot, DocKind::Card, card.id.0, &card_body(card), 0, size)
            {
                ops.push(DeviceOp::Put(
                    Key::DocText(slot, DocKind::Card, card.id.0),
                    window,
                ));
            }
        }
        let view = aux.and_then(|a| a.get(&agent.id));
        let doc = if view.is_some_and(|a| a.workflow_view) { DocKind::Workflow } else { DocKind::Reply };
        if let Some((text, rev)) = doc_body(agent, doc) {
            let window = agent_doc_window(agent, doc, view.and_then(|a| a.reply_first).unwrap_or(if doc == DocKind::Workflow { 0 } else { u16::MAX }), size, view.map(|a| &a.expanded_tools));
            ops.push(DeviceOp::Put(
                Key::Reply(slot),
                HostLine::Reply {
                    slot: slot.0,
                    text,
                    rev,
                },
            ));
            if let Some(window) = window {
                ops.push(DeviceOp::Put(
                    Key::DocText(slot, doc, rev),
                    window,
                ));
            }
        }
    }
    if let (Some(agent), Some(FocusSource::Desktop | FocusSource::Api)) =
        (&snapshot.focus.agent, snapshot.focus.source)
    {
        if let Some(slot) = snapshot.agents.get(agent).and_then(|a| a.slot) {
            ops.push(DeviceOp::Put(
                Key::Focus,
                HostLine::Focus {
                    slot: slot.0,
                    ack: None,
                },
            ));
        }
    }
    if let Some(usage) = &snapshot.usage {
        ops.push(DeviceOp::Put(
            Key::Usage,
            HostLine::Usage {
                period: usage.period.clone(),
                items: usage.items.clone(),
            },
        ));
    }
    if !snapshot.faces.is_empty() {
        ops.push(DeviceOp::Put(
            Key::Faces,
            HostLine::Faces {
                list: snapshot.faces.clone(),
            },
        ));
    }
    for (key, value) in snapshot.settings.iter() {
        // Older firmware cannot parse this key. The power-on Arcade entry
        // advertises support even when we restore a customized legacy strip.
        if key == crate::SetKey::Arcadesound
            && !snapshot.device.faces.iter().any(|f| f.t == crate::Template::Arcade)
        {
            continue;
        }
        ops.push(DeviceOp::Put(Key::Set(key), HostLine::Set { key, value }));
    }
    ops
}

pub(crate) fn agent_state_line(slot: Slot, agent: &Agent, now: Ts) -> HostLine {
    HostLine::Agent {
        slot: slot.0,
        kind: Some(agent.device_kind()),
        name: Some(agent.name.clone()),
        // Always sent, empty for "no project": the state line is the whole
        // record, so a session that lost its cwd on a re-register leaves
        // the page it was on.
        project: Some(agent.project().unwrap_or_default()),
        state: Some(agent.state),
        title: None,
        since_s: match agent.state {
            AgentState::Working | AgentState::Question => {
                Some((now.millis().saturating_sub(agent.since.millis()) / 1000) as u32)
            }
            _ => None,
        },
    }
}

/// The body the card shows, prefix and all.
///
/// `card_line` sends as much of it as one wire line holds; `card_window` wraps
/// the whole of it. Both read it from here, because a window has to be a window
/// of the *same* text the card carries — a prefix on one side only would shift
/// every line break.
pub(crate) fn card_body(card: &crate::Card) -> String {
    let text = card.text.clone().unwrap_or_default();
    match card.status {
        // A degraded hold: ENTER focuses the terminal (§4.8).
        CardStatus::Degraded { .. } => format!("⌨ {text}"),
        _ if card.origin.kind == OriginKind::Custom => format!("» {text}"),
        _ => text,
    }
}

pub(crate) fn card_line(slot: Slot, card: &crate::Card) -> HostLine {
    let (choices, rec) = match card.status {
        CardStatus::Degraded { .. } => (vec!["Focus terminal".to_string()], Some(0)),
        _ => (
            card.choices
                .iter()
                .take(4)
                .map(|c| c.label.clone())
                .collect(),
            card.rec,
        ),
    };
    HostLine::Card {
        slot: slot.0,
        id: card.id.0,
        kind: card.kind,
        text: card_body(card),
        choices,
        rec,
    }
}
/// The body of one of a slot's documents, with the id that identifies it: the
/// card's question, or the agent's last reply.
///
/// One function, because the deck does the same thing with both -- draws a
/// window of a layout it did not compute and asks for the next one. The live
/// transcript is not a document the store keeps, so it has no entry here.
pub(crate) fn doc_body(agent: &crate::Agent, doc: DocKind) -> Option<(String, u16)> {
    match doc {
        DocKind::Workflow => Some(("Workflow".into(), if agent.messages.is_empty() { agent.reply_rev } else { agent.chat_rev })),
        DocKind::Card => agent
            .card
            .as_ref()
            .filter(|c| c.status.on_device())
            .map(|c| (card_body(c), c.id.0)),
        DocKind::Reply if !agent.messages.is_empty() => agent.messages.last().map(|m| {
            let text = if m.message.role == crate::ChatRole::Tool {
                deck_wrap::tool_header(&m.text, 0, 0, false, m.message.streaming)[4..].to_string()
            } else { m.text.clone() };
            (text, agent.chat_rev)
        }),
        DocKind::Reply => agent
            .reply
            .as_ref()
            .and_then(|r| r.text.clone())
            .map(|text| (text, agent.reply_rev)),
        DocKind::Tr => None,
    }
}

/// The window of the card's wrapped body that holds line `first`, or `None`
/// when the body needs no window at all.
///
/// The host wraps with the **deck's own** `font::wrap` (`deck-wrap`), so the
/// two sides break lines in the same places; the deck never re-wraps a body it
/// was sent a window for.
///
/// `None` means "the deck can do this itself": the body fits the lane *and*
/// fits one wire line, so the deck wraps its own `text` exactly as it did
/// before any of this existed and no second line is spent. The `card_text`
/// half of the test matters — a body of three short lines can still be longer
/// than the 128 bytes the card line carries, and that is the ellipsis this
/// feature exists to remove.
pub(crate) fn doc_window(
    slot: Slot,
    doc: DocKind,
    id: u16,
    body: &str,
    first: u16,
    // `size` is the deck's `text` setting: the host has to wrap with the face
    // the deck will draw with, or the two break lines in different places.
    size: u8,
) -> Option<HostLine> {
    let caps = deck_proto::caps();
    let (font, lines) = match doc {
        DocKind::Card => deck_wrap::wrap_question(body),
        DocKind::Reply | DocKind::Workflow => deck_wrap::wrap_rich_reply(body, size),
        DocKind::Tr => deck_wrap::wrap_reply(body, size),
    };
    // The cap on the flat line this document already rides on: below it the
    // deck can lay the body out itself and no window is spent.
    let flat_cap = match doc {
        DocKind::Card => caps.card_text,
        DocKind::Reply | DocKind::Workflow => caps.reply,
        DocKind::Tr => caps.transcript,
    };
    if doc != DocKind::Reply && lines.len() <= deck_wrap::visible(doc, font) && body.len() <= flat_cap {
        return None;
    }
    lines_window(slot, doc, id, first, font, &lines)
}

pub(crate) fn agent_doc_window(agent: &crate::Agent, doc: DocKind, first: u16, size: u8, expanded: Option<&std::collections::BTreeSet<String>>) -> Option<HostLine> {
    let slot = agent.slot?;
    if doc == DocKind::Workflow {
        let (font, lines) = crate::workflow::device_lines(agent, size, expanded);
        return lines_window(slot, doc, doc_body(agent, doc)?.1, first, font, &lines);
    }
    if doc != DocKind::Reply || agent.messages.is_empty() {
        let (body, id) = doc_body(agent, doc)?;
        return doc_window(slot, doc, id, &body, first, size);
    }
    let (font, lines) = chat_lines(agent, size, expanded);
    lines_window(slot, doc, agent.chat_rev, first, font, &lines)
}

pub(crate) fn chat_lines(agent: &crate::Agent, size: u8, expanded: Option<&std::collections::BTreeSet<String>>) -> (deck_proto::DocFont, Vec<String>) {
    let mut lines = Vec::new();
    let font = match size { 0 => deck_proto::DocFont::RichRow, 1 => deck_proto::DocFont::RichText, _ => deck_proto::DocFont::RichTitle };
    let mut i = 0;
    let mut image_index = 0usize;
    while i < agent.messages.len() {
        let entry = &agent.messages[i];
        if entry.message.role == crate::ChatRole::Tool {
            let end = agent.messages[i..].iter().position(|m| m.message.role != crate::ChatRole::Tool).map_or(agent.messages.len(), |n| i + n);
            let group = &agent.messages[i..end];
            let open = expanded.is_some_and(|set| set.contains(&entry.message.id));
            let summaries: Vec<_> = group.iter().map(|m| crate::workflow::tool_summary(&m.text)).collect();
            let exploring = summaries.iter().all(|(verb, _)| matches!(verb.as_str(), "Read" | "Search" | "List"));
            let running = group.iter().any(|m| m.message.streaming);
            let failed = group.iter().any(|m| m.text.lines().next().is_some_and(|s| s.contains("failed") || s.contains("error")));
            let title = if failed { "Activity · failed" } else if exploring { if running { "Exploring…" } else { "Explored" } } else if running { "Working…" } else { "Activity" };
            if !lines.is_empty() { lines.push("b0".into()); }
            lines.extend(deck_wrap::wrap_rich_reply(&format!("## {title}"), size).1);
            let mut seen = std::collections::BTreeSet::new();
            let unique: Vec<_> = summaries.iter().filter(|item| seen.insert((*item).clone())).collect();
            for (verb, target) in unique.iter().take(3) { lines.push(deck_wrap::activity_row(verb, target, size)); }
            if unique.len() > 3 { lines.push(deck_wrap::activity_row("+", &format!("{} more", unique.len() - 3), size)); }
            lines.push(deck_wrap::tool_disclosure(size, i as u8, open, group.len()));
            if open {
                for item in group {
                    lines.push("b0".into());
                    lines.extend(deck_wrap::wrap_rich_reply(&item.text, size).1);
                }
            }
            for item in group { append_image_lines(&mut lines, &item.message.images, &mut image_index); }
            i = end;
            continue;
        }
        let role = match entry.message.role { crate::ChatRole::User => "You", crate::ChatRole::Assistant => "Assistant", crate::ChatRole::Tool => "Tool" };
        let status = if entry.message.streaming { "live" } else { "done" };
        if i != 0 { lines.extend(["b0".into(), "r0".into()]); }
        let (_, header) = deck_wrap::wrap_rich_reply(&format!("## {role} · {} · {status}", i + 1), size);
        lines.extend(header);
        lines.push("b0".into());
        // Parse each message independently: an unfinished code fence in a
        // streaming message must never consume the next message's heading.
        lines.extend(deck_wrap::wrap_rich_reply(&entry.text, size).1);
        append_image_lines(&mut lines, &entry.message.images, &mut image_index);
        i += 1;
    }
    (font, lines)
}


fn append_image_lines(lines: &mut Vec<String>, images: &[crate::ChatImage], index: &mut usize) {
    for image in images {
        if *index < 256 { lines.push(format!("i0{:02x}Image · {} × {} · tap to view", *index, image.width, image.height)); }
        *index += 1;
    }
}

fn lines_window(slot: Slot, doc: DocKind, id: u16, first: u16, font: deck_proto::DocFont, lines: &[String]) -> Option<HostLine> {
    let caps = deck_proto::caps();
    // A body past 65535 lines cannot be addressed on the wire; it is also not
    // something anyone is going to read, so it is cut rather than mis-numbered.
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let lines = &lines[..usize::from(total)];

    // Cut by *encoded* length, not by count: escaping can double a line, and
    // the outbox drops an over-cap line rather than truncating it.
    let fit = |start: usize| {
        let window = &lines[start..(start + caps.doc_lines).min(lines.len())];
        deck_proto::fit_doc_window(slot.0, doc, id, start as u16, total, font, window)
    };

    // Serve the chunk the deck asked for, starting where it asked.
    //
    // The host used to centre the chunk on the requested line, which was
    // overscan the deck did not need: it holds a run of the body now
    // (`DOC_HELD`) and asks for the lines it is *missing*, so a centred chunk
    // would be half lines it already had. Clamped to the end of the body so a
    // request past it still returns the tail rather than nothing.
    let last = lines.len().saturating_sub(1);
    let mut start = if usize::from(first) > last {
        // Past the end of the body: the tail. That is what a request for "the
        // end" means — the live transcript asks that way — and it keeps a
        // request that raced a shortening body landing on real lines instead
        // of one stray one.
        lines.len().saturating_sub(caps.doc_lines)
    } else {
        usize::from(first)
    };
    let mut fitted = fit(start);
    if usize::from(first) > last {
        while start + fitted.len() < lines.len() && start < last {
            start += 1;
            fitted = fit(start);
        }
    }
    if fitted.is_empty() {
        return None;
    }
    Some(HostLine::DocText {
        slot: slot.0,
        doc,
        id,
        first: start as u16,
        total,
        font,
        lines: fitted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::fixture_agent as agent;
    use crate::{AgentId, Request, SessionId, SetKey, Template};

    fn store() -> Store {
        Store::new(StoreConfig::new(Instant::now(), Ts(1_000_000), 314_159))
    }

    #[test]
    fn system_samples_are_ephemeral_device_updates_without_snapshot_churn() {
        let mut s = store();
        let rev = s.snapshot().rev;
        let status = deck_proto::SystemStatus { cpu: Some(20), sample: 1, ..deck_proto::SystemStatus::EMPTY };
        let (events, effects) = s.apply(Command::SystemStatus(status), Instant::now());
        assert!(events.is_empty());
        assert_eq!(s.snapshot().rev, rev);
        assert!(matches!(&effects[..], [Effect::Device(DeviceOp::Put(Key::System, HostLine::System(got)))] if *got == status));
        let invalid = deck_proto::SystemStatus { gpu: Some(101), ..status };
        let (events, effects) = s.apply(Command::SystemStatus(invalid), Instant::now());
        assert!(events.is_empty() && effects.is_empty());
    }

    #[test]
    fn reconfigure_applies_the_hot_keys_and_keeps_the_identity() {
        let started = Instant::now();
        let mut s = Store::new(StoreConfig::new(started, Ts(7), 11));
        let mut next = StoreConfig::new(Instant::now(), Ts(9), 99);
        next.grace_s = 60;
        next.ready_ttl_s = 30;
        next.hold_s = 45;
        next.port = 1;
        next.version = "9.9.9".into();
        next.pins.insert(
            "claude:cohdl-api".into(),
            PinConfig {
                slot: Some(Slot(4)),
                ..PinConfig::default()
            },
        );
        s.reconfigure(&next);
        assert_eq!(s.config().grace_s, 60);
        assert_eq!(s.config().ready_ttl_s, 30);
        assert_eq!(s.config().hold_s, 45);
        assert_eq!(s.config().pins["claude:cohdl-api"].slot, Some(Slot(4)));
        // Identity, the wall clock and the port are the store's own.
        assert_eq!(s.config().sid, 11);
        assert_eq!(s.config().wall, Ts(7));
        assert_eq!(s.config().started, started);
        assert_eq!(s.config().port, 47201);
        assert_eq!(s.config().version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_new_store_carries_the_persisted_state_and_service_facts() {
        let mut cfg = StoreConfig::new(Instant::now(), Ts(5), 42);
        cfg.persisted.faces = vec![FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        }];
        cfg.persisted.card_ids.insert(3, 9);
        let mut s = Store::new(cfg);
        assert_eq!(s.snapshot().device.sid, 42);
        assert_eq!(s.snapshot().faces.len(), 1);
        assert_eq!(s.snapshot().service.port, 47201);
        assert_eq!(s.snapshot().rev, 0);
        assert_eq!(s.next_card_id(Slot(3)), CardId(10));
        assert_eq!(s.next_card_id(Slot(1)), CardId(1));
        assert_eq!(s.next_card_id(Slot(1)), CardId(2));
        let (_, effects) = s.apply(Command::Tick(Instant::now()), Instant::now());
        assert_eq!(effects.len(), 3);
        assert!(matches!(
            effects[0],
            Effect::Persist(StatePatch::CardId {
                slot: Slot(3),
                last: CardId(10)
            })
        ));
        s.reset_slot(Slot(1));
        let (_, effects) = s.apply(Command::Tick(Instant::now()), Instant::now());
        assert_eq!(
            effects,
            vec![Effect::Persist(StatePatch::CardId {
                slot: Slot(1),
                last: CardId(0)
            })]
        );
        assert_eq!(s.next_card_id(Slot(1)), CardId(1));
    }

    #[test]
    fn link_events_drive_the_device_state_and_rev() {
        let mut s = store();
        let t0 = Instant::now();
        let (events, _) = s.apply(Command::Link(LinkEvent::Linked), t0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::DeviceLink);
        assert_eq!(events[0].rev, 1);
        assert_eq!(events[0].principal, Principal::Device);
        assert_eq!(s.snapshot().device.link, Link::Connected);
        assert_eq!(s.snapshot().device.session, SessionState::AwaitHello);
        let (events, _) = s.apply(Command::Link(LinkEvent::Linked), t0);
        assert!(events.is_empty(), "no change, no rev");
        s.apply(Command::Link(LinkEvent::Syncing { sid: 0, boot: true }), t0);
        assert!(s.snapshot().device.host_online);
        assert_eq!(s.snapshot().device.boot, Some(true));
        s.apply(Command::Link(LinkEvent::Live { sid: 314_159 }), t0);
        assert_eq!(s.snapshot().device.session, SessionState::Live);
        s.apply(Command::Link(LinkEvent::Degraded), t0);
        assert_eq!(s.snapshot().device.link, Link::Degraded);
        s.apply(Command::Link(LinkEvent::Down), t0);
        assert_eq!(s.snapshot().device.link, Link::Offline);
        assert!(!s.snapshot().device.host_online);
        let (events, _) = s.apply(Command::Link(LinkEvent::PaceBackoff { pace_ms: 10 }), t0);
        assert_eq!(s.snapshot().device.pace_ms, 10);
        assert_eq!(events[0].kind, EventKind::Error);
    }

    #[test]
    fn event_timestamps_derive_from_the_wall_base() {
        let t0 = Instant::now();
        let mut s = Store::new(StoreConfig::new(t0, Ts(1_789_760_523_000), 1));
        let (events, _) = s.apply(
            Command::Link(LinkEvent::Linked),
            t0 + Duration::from_millis(120),
        );
        assert_eq!(events[0].ts.to_string(), "2026-09-18T19:42:03.120Z");
    }

    #[test]
    fn hello_adopts_faces_on_boot_zero_and_keeps_them_on_boot_one() {
        let mut s = store();
        let faces = vec![FaceEntry {
            f: 2,
            t: Template::Omarchy,
            n: "Omarchy".into(),
        }];
        let hello = |boot: bool| DeviceLine::Hello {
            v: 1,
            fw: "0.1.0".into(),
            sid: 0,
            boot,
            rx: 1024,
            faces: faces.clone(),
        };
        let (events, effects) = s.apply(Command::Device(hello(true)), Instant::now());
        assert!(s.snapshot().faces.is_empty(), "boot:1 pushes ours instead");
        assert!(effects.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(s.snapshot().device.fw.as_deref(), Some("0.1.0"));
        let (events, effects) = s.apply(Command::Device(hello(false)), Instant::now());
        assert_eq!(s.snapshot().faces, faces);
        assert!(matches!(&effects[..], [Effect::Persist(StatePatch::Faces(f))] if *f == faces));
        assert_eq!(events[0].kind, EventKind::DeviceFaces);
        let (_, effects) = s.apply(Command::Device(hello(false)), Instant::now());
        assert!(effects.is_empty(), "already adopted");
    }

    #[test]
    fn a_bad_rx_is_reported() {
        let mut s = store();
        let (events, _) = s.apply(
            Command::Device(DeviceLine::Hello {
                v: 1,
                fw: "x".into(),
                sid: 0,
                boot: true,
                rx: 512,
                faces: vec![],
            }),
            Instant::now(),
        );
        assert!(events
            .iter()
            .any(|e| e.kind == EventKind::Error && e.data["context"] == "hello.rx"));
    }

    #[test]
    fn channel_acks_immediately_and_moves_focus() {
        let mut s = store();
        s.snapshot.agents.insert(
            AgentId("ag_1".into()),
            agent("ag_1", 1, AgentState::Working),
        );
        let (events, effects) = s.apply(
            Command::Device(DeviceLine::Channel { slot: 1, seq: 41 }),
            Instant::now(),
        );
        assert_eq!(
            effects[0],
            Effect::Device(DeviceOp::Control(Control::Focus {
                slot: Slot(1),
                ack: Some(41)
            }))
        );
        assert_eq!(effects[1], Effect::Device(DeviceOp::Remove(Key::Focus)));
        assert!(
            matches!(&effects[2], Effect::Desktop(crate::DesktopCmd::Focus { agent }) if agent.0 == "ag_1")
        );
        assert_eq!(events[0].kind, EventKind::FocusChanged);
        assert_eq!(s.snapshot().focus.seq, Some(41));
        assert_eq!(s.snapshot().focus.source, Some(FocusSource::Device));
        assert_eq!(
            s.snapshot().focus.agent.as_ref().map(|a| a.0.as_str()),
            Some("ag_1")
        );
        // An empty slot still gets its ack (ordering), no desktop focus.
        let (_, effects) = s.apply(
            Command::Device(DeviceLine::Channel { slot: 5, seq: 42 }),
            Instant::now(),
        );
        assert_eq!(effects.len(), 2);
        assert_eq!(s.snapshot().focus.agent, None);
    }

    #[test]
    fn device_settings_persist_and_never_echo() {
        let mut s = store();
        let (events, effects) = s.apply(
            Command::Device(DeviceLine::Set {
                key: SetKey::Brightness,
                value: 3,
            }),
            Instant::now(),
        );
        assert_eq!(
            effects,
            vec![Effect::Persist(StatePatch::Setting {
                key: SetKey::Brightness,
                value: 3
            })]
        );
        assert_eq!(events[0].kind, EventKind::DeviceSet);
        assert_eq!(s.snapshot().settings.brightness, 3);
        assert_eq!(s.snapshot().device.settings.brightness, 3);
        let (events, effects) = s.apply(
            Command::Device(DeviceLine::Set {
                key: SetKey::Brightness,
                value: 3,
            }),
            Instant::now(),
        );
        assert!(events.is_empty() && effects.is_empty());
        let (events, _) = s.apply(
            Command::Device(DeviceLine::Set {
                key: SetKey::Speaker,
                value: 99,
            }),
            Instant::now(),
        );
        assert!(events.is_empty(), "out of range is ignored");
        assert_eq!(s.snapshot().settings.speaker, 6);
    }

    #[test]
    fn faces_face_err_and_refresh_are_mirrored() {
        let mut s = store();
        let list = vec![FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        }];
        let (events, effects) = s.apply(
            Command::Device(DeviceLine::Faces { list: list.clone() }),
            Instant::now(),
        );
        assert_eq!(events[0].kind, EventKind::DeviceFaces);
        assert_eq!(
            effects,
            vec![Effect::Persist(StatePatch::Faces(list.clone()))]
        );
        assert_eq!(s.snapshot().faces, list);
        let (events, _) = s.apply(Command::Device(DeviceLine::Face { f: 4 }), Instant::now());
        assert_eq!(events[0].kind, EventKind::DeviceFace);
        assert_eq!(s.snapshot().device.face, Some(4));
        let (events, _) = s.apply(Command::Device(DeviceLine::Face { f: 4 }), Instant::now());
        assert!(events.is_empty());
        s.apply(
            Command::Device(DeviceLine::Err {
                why: crate::ErrWhy::Range,
            }),
            Instant::now(),
        );
        s.apply(
            Command::Device(DeviceLine::Err {
                why: crate::ErrWhy::Size,
            }),
            Instant::now(),
        );
        assert_eq!(s.snapshot().device.err_counts.range, 1);
        assert_eq!(s.snapshot().device.err_counts.size, 1);
        let t = Instant::now();
        let (events, effects) = s.apply(
            Command::Device(DeviceLine::Refresh {
                what: "usage".into(),
            }),
            t,
        );
        assert_eq!(events[0].kind, EventKind::UsageRefreshRequested);
        assert_eq!(
            effects,
            vec![Effect::Timer {
                key: crate::TimerKey::UsageRefresh,
                at: t + Duration::from_millis(1500)
            }]
        );
        let (events, _) = s.apply(
            Command::Device(DeviceLine::Key {
                k: crate::KeyId::Cw,
                act: 2,
                shift: false,
                f: 2,
            }),
            t,
        );
        assert_eq!(events[0].kind, EventKind::DeviceKey);
        let (events, _) = s.apply(Command::Device(DeviceLine::WsGo(3)), t);
        assert_eq!(events[0].data["go"], 3);
    }

    #[test]
    fn overflow_reprojects_and_asks_for_a_barrier() {
        let mut s = store();
        s.snapshot
            .agents
            .insert(AgentId("ag_1".into()), agent("ag_1", 2, AgentState::Idle));
        let (events, effects) = s.apply(Command::Link(LinkEvent::Overflow), Instant::now());
        assert!(events.is_empty());
        // §6.5's "`synced` is always last" is a property of the **device**
        // stream; effects of other kinds carry no ordering against it.
        assert!(matches!(
            effects.iter().rfind(|e| matches!(e, Effect::Device(_))),
            Some(Effect::Device(DeviceOp::Sync {
                boot: false,
                reason: crate::SyncReason::Overflow
            }))
        ));
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Device(DeviceOp::Put(Key::AgentState(Slot(2)), _))
        )));
    }

    #[test]
    fn arcade_sound_is_projected_only_to_a_device_that_advertises_arcade() {
        let mut snapshot = Snapshot::default();
        snapshot.settings.arcadesound = 2;
        let has_sound = |ops: Vec<DeviceOp>| ops.into_iter().any(|op| matches!(op,
            DeviceOp::Put(Key::Set(crate::SetKey::Arcadesound), HostLine::Set { value: 2, .. })));
        assert!(!has_sound(project(&snapshot, Ts::from_millis(0))));
        snapshot.device.faces.push(FaceEntry { f: 6, t: Template::Arcade, n: "Arcade".into() });
        assert!(has_sound(project(&snapshot, Ts::from_millis(0))));
    }

    #[test]
    fn projection_dumps_the_view_in_barrier_order() {
        let mut s = store();
        s.snapshot.agents.insert(
            AgentId("ag_b".into()),
            agent("ag_b", 3, AgentState::Working),
        );
        let mut a = agent("ag_a", 1, AgentState::Idle);
        a.card = None;
        a.reply = None;
        a.title = None;
        s.snapshot.agents.insert(AgentId("ag_a".into()), a);
        s.snapshot.usage = Some(Usage {
            period: "SEP".into(),
            items: vec![],
            at: None,
        });
        s.snapshot.faces = vec![FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        }];
        let ops = s.project();
        let keys: Vec<Key> = ops
            .iter()
            .map(|op| match op {
                DeviceOp::Put(k, _) => *k,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            &keys[..8],
            &[
                Key::AgentState(Slot(1)),
                Key::AgentState(Slot(3)),
                Key::AgentTitle(Slot(3)),
                Key::Card(Slot(3), 17),
                Key::Reply(Slot(3)),
                Key::DocText(Slot(3), DocKind::Reply, 0),
                Key::Usage,
                Key::Faces,
            ]
        );
        assert_eq!(keys.len(), 8 + 9);
        assert!(keys[8..].iter().all(|k| matches!(k, Key::Set(_))));
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "the natural Key order is the barrier order");
        // The working agent carries its age; the card its choices.
        match &ops[1] {
            DeviceOp::Put(
                _,
                HostLine::Agent {
                    since_s: Some(_),
                    state: Some(AgentState::Working),
                    title: None,
                    ..
                },
            ) => {}
            other => panic!("{other:?}"),
        }
        match &ops[3] {
            DeviceOp::Put(
                _,
                HostLine::Card {
                    choices,
                    rec: Some(0),
                    text,
                    ..
                },
            ) => {
                assert_eq!(choices, &["Allow", "Deny"]);
                assert_eq!(text, "Bash: psql");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_store_projection_slots_macro_and_ws_lines_in_barrier_order() {
        let mut s = store();
        s.snapshot
            .agents
            .insert(AgentId("ag_1".into()), agent("ag_1", 1, AgentState::Idle));
        s.snapshot.usage = Some(Usage {
            period: "SEP".into(),
            items: vec![],
            at: None,
        });
        s.macros.insert(
            5,
            HostLine::Macro {
                face: 5,
                src: None,
                keys: vec![],
            },
        );
        s.ws = Some(HostLine::Ws {
            active: 1,
            count: 2,
            sel: 0,
            windows: vec![],
        });
        let keys: Vec<Key> = s
            .project()
            .into_iter()
            .map(|op| match op {
                DeviceOp::Put(k, _) => k,
                other => panic!("{other:?}"),
            })
            .collect();
        let pos = |k: Key| keys.iter().position(|x| *x == k).unwrap();
        assert!(pos(Key::Reply(Slot(1))) < pos(Key::Macro(crate::FaceId(5))));
        assert!(pos(Key::Macro(crate::FaceId(5))) < pos(Key::Usage));
        assert!(pos(Key::Usage) < pos(Key::Ws));
        assert!(pos(Key::Ws) < pos(Key::Set(SetKey::Speaker)));
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn custom_and_degraded_cards_are_marked_on_the_wire() {
        let mut a = agent("ag_1", 1, AgentState::Question);
        let card = a.card.as_mut().unwrap();
        card.origin.kind = OriginKind::Custom;
        let HostLine::Card { text, .. } = card_line(Slot(1), card) else {
            unreachable!()
        };
        assert_eq!(text, "» Bash: psql");
        card.status = CardStatus::Degraded { focused_at: None };
        let HostLine::Card {
            text, choices, rec, ..
        } = card_line(Slot(1), card)
        else {
            unreachable!()
        };
        assert_eq!(text, "⌨ Bash: psql");
        assert_eq!(choices, vec!["Focus terminal"]);
        assert_eq!(rec, Some(0));
        card.status = CardStatus::Degraded {
            focused_at: Some(Ts(1)),
        };
        let mut snap = Snapshot::default();
        snap.agents.insert(a.id.clone(), a);
        assert!(
            !project(&snap, Ts(0))
                .iter()
                .any(|op| matches!(op, DeviceOp::Put(Key::Card(..), _))),
            "retired"
        );
    }

    #[test]
    fn focus_is_projected_only_when_pc_sourced() {
        let mut snap = Snapshot::default();
        snap.agents
            .insert(AgentId("ag_1".into()), agent("ag_1", 1, AgentState::Idle));
        snap.focus = crate::Focus {
            agent: Some(AgentId("ag_1".into())),
            seq: Some(3),
            source: Some(FocusSource::Device),
        };
        assert!(!project(&snap, Ts(0))
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Focus, _))));
        snap.focus.source = Some(FocusSource::Desktop);
        assert!(project(&snap, Ts(0)).iter().any(|op| matches!(
            op,
            DeviceOp::Put(Key::Focus, HostLine::Focus { slot: 1, ack: None })
        )));
    }

    #[test]
    fn the_store_redacts_through_the_shared_function() {
        let mut s = store();
        s.snapshot.agents.insert(
            AgentId("ag_1".into()),
            agent("ag_1", 1, AgentState::Question),
        );
        let observer = s.redact(&[Scope::Observe]);
        assert!(observer.agents[&AgentId("ag_1".into())].redacted);
        assert_eq!(s.redact(&[Scope::Operate]), *s.snapshot());
    }

    #[test]
    fn confirm_requested_is_an_event_from_the_refused_principal() {
        let mut s = store();
        let (events, effects) = s.apply(
            Command::ConfirmRequested {
                principal: Principal::Token("mcp".into()),
                agent: AgentId("ag_1".into()),
                card: Some(CardId(17)),
                order: None,
            },
            Instant::now(),
        );
        assert!(effects.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::OpConfirmRequested);
        assert_eq!(events[0].rev, 1);
        assert_eq!(events[0].principal, Principal::Token("mcp".into()));
        assert_eq!(
            events[0].data,
            serde_json::json!({
                "agent": "ag_1", "card": 17, "order": null,
                "principal": "token:mcp", "need": "trusted",
            })
        );
        let (events, _) = s.apply(
            Command::ConfirmRequested {
                principal: Principal::Uid(501),
                agent: AgentId("ag_1".into()),
                card: None,
                order: Some("rm -rf build".into()),
            },
            Instant::now(),
        );
        assert_eq!(events[0].data["order"], "rm -rf build");
        assert_eq!(events[0].data["card"], serde_json::Value::Null);
        assert_eq!(s.snapshot().rev, 2);
    }

    #[test]
    fn the_bound_port_is_a_fact_and_a_degradation_change_is_an_event() {
        let mut s = store();
        let t = Instant::now();
        let squatter = || {
            Some(crate::Listener {
                pid: 4242,
                command: Some("python3".into()),
            })
        };
        // A plain bind on an ephemeral port: the port changes, nothing is degraded.
        let (events, _) = s.apply(
            Command::ServiceBound {
                port: Some(49_152),
                degraded: None,
                listener: None,
            },
            t,
        );
        assert!(events.is_empty());
        assert_eq!(s.snapshot().service.port, 49_152);
        assert_eq!(s.snapshot().rev, 0);
        // A failed bind: the configured port is kept, the reason and who
        // holds the port are the event (§4.1).
        let (events, _) = s.apply(
            Command::ServiceBound {
                port: None,
                degraded: Some("port_in_use".into()),
                listener: squatter(),
            },
            t,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::ServiceDegraded);
        assert_eq!(
            events[0].data,
            serde_json::json!({
                "reason": "port_in_use", "port": 49_152,
                "listener": {"pid": 4242, "command": "python3"},
            })
        );
        assert_eq!(
            s.snapshot().service.degraded.as_deref(),
            Some("port_in_use")
        );
        assert_eq!(s.snapshot().service.listener, squatter());
        // The same reason and the same listener again is not an event.
        let (events, _) = s.apply(
            Command::ServiceBound {
                port: None,
                degraded: Some("port_in_use".into()),
                listener: squatter(),
            },
            t,
        );
        assert!(events.is_empty());
        // A different process on the port is news even under the same reason.
        let (events, _) = s.apply(
            Command::ServiceBound {
                port: None,
                degraded: Some("port_in_use".into()),
                listener: Some(crate::Listener {
                    pid: 77,
                    command: None,
                }),
            },
            t,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["listener"]["pid"], 77);
        assert_eq!(
            events[0].data["listener"]["command"],
            serde_json::Value::Null
        );
        // Clearing it drops the listener with the reason.
        let (events, _) = s.apply(
            Command::ServiceBound {
                port: Some(49_152),
                degraded: None,
                listener: None,
            },
            t,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["reason"], serde_json::Value::Null);
        assert_eq!(events[0].data["listener"], serde_json::Value::Null);
        assert_eq!(s.snapshot().service.degraded, None);
        assert_eq!(s.snapshot().service.listener, None);
        assert_eq!(s.snapshot().rev, 3);
    }

    #[test]
    fn shutdown_and_unknown_api_targets_do_not_panic() {
        let mut s = store();
        let (events, _) = s.apply(Command::Shutdown, Instant::now());
        assert_eq!(events[0].kind, EventKind::ServiceShutdown);
        let (events, effects) = s.apply(
            Command::Api {
                principal: Principal::Uid(1),
                session: SessionId(1),
                req: Box::new(Request::OpFocus {
                    agent: AgentId("ag_1".into()),
                }),
            },
            Instant::now(),
        );
        assert!(events.is_empty() && effects.is_empty());
        for line in [
            DeviceLine::Up,
            DeviceLine::Answer {
                slot: 1,
                card: 1,
                choice: 0,
            },
            DeviceLine::Snooze { slot: 1, card: 1 },
            DeviceLine::Ptt {
                act: 1,
                slot: 1,
                ctx: crate::PttCtx::Talk,
                card: None,
                abort: false, press: None,
            },
        ] {
            s.apply(Command::Device(line), Instant::now());
        }
        for cmd in [
            Command::HoldWritten {
                agent: AgentId("ag_x".into()),
                card: CardId(1),
            },
            Command::HoldGone {
                agent: AgentId("ag_x".into()),
                card: CardId(1),
            },
            Command::Delivered {
                agent: AgentId("ag_x".into()),
                card: CardId(1),
                ok: true,
            },
            Command::SessionClosed(SessionId(9)),
        ] {
            let (events, effects) = s.apply(cmd, Instant::now());
            assert!(events.is_empty() && effects.is_empty());
        }
    }

    #[test]
    fn shutdown_releases_every_parked_hold_and_marks_the_device_offline() {
        let mut s = store();
        s.apply(Command::Link(LinkEvent::Live { sid: 1 }), Instant::now());
        s.snapshot.agents.insert(
            AgentId("ag_1".into()),
            agent("ag_1", 1, AgentState::Question),
        );
        let mut b = agent("ag_2", 2, AgentState::Question);
        b.card.as_mut().unwrap().status = CardStatus::Delivered;
        s.snapshot.agents.insert(AgentId("ag_2".into()), b);
        let (events, effects) = s.apply(Command::Shutdown, Instant::now());
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(
                    e,
                    Effect::ReleaseHold {
                        agent,
                        card: CardId(17),
                        reason: crate::Release::Shutdown
                    } if agent.0 == "ag_1"
                ))
                .count(),
            1,
            "only the parked one"
        );
        assert_eq!(s.snapshot().device.link, Link::Offline);
        assert!(!s.snapshot().device.host_online);
        assert!(events.iter().any(|e| e.kind == EventKind::DeviceLink));
    }
}
