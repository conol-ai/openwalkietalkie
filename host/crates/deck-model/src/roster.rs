//! Agents (§4.6, §4.15): register/update/release, leases with the orphan
//! grace, `claim`, session closes, the slot policy (pins → memory by key →
//! memory by `(kind, cwd)` → hint → lowest free → evict the longest-idle
//! `idle` agent without a pending card; never an agent of a muted session),
//! focus and replies.
use crate::api::{ApiError, ApiResult};
use crate::store::AgentAux;
use crate::{
    Agent, AgentId, AgentRegister, AgentState, AgentUpdate, AnswerCap, AudioCmd, Caps, Deliver,
    DesktopCmd, DeviceOp, Effect, EventKind, FocusReport, FocusSource, HostLine, Key, OrderCap,
    Owner, Principal, Reply, ReplySet, SessionId, SessionInfo, Slot, Source, StatePatch, Store,
    TimerKey, TitleSource,
};
use std::collections::BTreeSet;
use std::time::Duration;

/// The title an orphaned agent shows on the deck.
pub const DISCONNECTED: &str = "(disconnected)";

impl Store {
    pub(crate) fn agent_by_key(&self, key: &str) -> Option<AgentId> {
        self.snapshot
            .agents
            .values()
            .find(|a| a.session.key.as_deref() == Some(key))
            .map(|a| a.id.clone())
    }

    fn agent_json(agent: &Agent) -> serde_json::Value {
        serde_json::to_value(agent).unwrap_or(serde_json::Value::Null)
    }

    /// `[agents]` pin by `"<kind>:<name>"`, else by cwd.
    fn pin_for(
        &self,
        kind: crate::Kind,
        name: &str,
        cwd: Option<&str>,
    ) -> Option<crate::PinConfig> {
        let kind = kind_name(kind);
        self.cfg
            .pins
            .get(&format!("{kind}:{name}"))
            .or_else(|| cwd.and_then(|c| self.cfg.pins.get(c)))
            .cloned()
    }

    /// §7.7's voice for one agent, in the order §7.7 names them:
    /// the voice the caller asked for, the `[agents."<key>"].voice` pin,
    /// the one this key was given last time, then the round-robin over
    /// `[voice.voices] default_pool`.
    ///
    /// **Nothing assigned one before**: `StatePatch::Voice` had no
    /// producer, so `state.json.voices` was written by nobody and every
    /// agent announced in the same default voice. Four agents in one voice
    /// is four announcements the operator has to parse by content; four
    /// voices is four they can tell apart before the second word — which
    /// is the point of §7.7 and the reason the pool is four *distinct*
    /// voices rather than a preference.
    ///
    /// The rotation skips voices a live agent already has, so the pool is
    /// spent on telling agents apart rather than on counting: with four
    /// voices and four agents nobody ever shares, however the roster
    /// churned to get there. Only the round-robin is persisted — a pin and
    /// a caller's choice are already written down somewhere the operator
    /// controls, and remembering them would make a changed pin stop taking
    /// effect.
    fn voice_for(
        &mut self,
        key: &str,
        asked: Option<String>,
        pinned: Option<String>,
    ) -> Option<String> {
        if let Some(voice) = asked.or(pinned) {
            return Some(voice);
        }
        if let Some(voice) = self.voices.get(key) {
            return Some(voice.clone());
        }
        let pool = &self.cfg.voice_pool;
        if pool.is_empty() {
            // No pool configured: the engine's own default, and nothing
            // persisted to undo later.
            return None;
        }
        let taken: Vec<&String> = self
            .snapshot
            .agents
            .values()
            .filter_map(|a| a.voice.as_ref())
            .collect();
        let start = self.voice_seq % pool.len();
        let voice = (0..pool.len())
            .map(|i| &pool[(start + i) % pool.len()])
            .find(|v| !taken.contains(v))
            .unwrap_or(&pool[start])
            .clone();
        self.voice_seq += 1;
        self.voices.insert(key.to_string(), voice.clone());
        self.effects.push(Effect::Persist(StatePatch::Voice {
            key: key.to_string(),
            voice: voice.clone(),
        }));
        Some(voice)
    }

    pub(crate) fn register(
        &mut self,
        principal: Principal,
        session: SessionId,
        r: AgentRegister,
    ) -> ApiResult {
        let adapter = matches!(principal, Principal::Adapter(_));
        if !adapter && r.caps.answer == AnswerCap::Hold {
            return Err(ApiError::invalid("caps.answer", "adapter_only"));
        }
        if !adapter && r.caps.order == OrderCap::Inject {
            return Err(ApiError::invalid("caps.order", "adapter_only"));
        }
        if r.name.trim().is_empty() {
            return Err(ApiError::invalid("name", "empty"));
        }
        if r.slot_hint.is_some_and(|s| !s.is_valid()) {
            return Err(ApiError::invalid("slot_hint", "1..=255"));
        }
        if let Some(id) = r.key.as_deref().and_then(|k| self.agent_by_key(k)) {
            // Idempotent by key: same session → same agent (§4.6).
            let owner = &self.snapshot.agents[&id].owner;
            if owner.session != session {
                return Err(ApiError::conflict(owner));
            }
            self.touch(&id);
            self.apply_registration_changes(&id, &r);
            return Ok(self.registration_result(&id));
        }
        self.agent_seq += 1;
        let id = AgentId::from_seed((u64::from(self.cfg.sid) << 32) | self.agent_seq);
        let ts = self.ts_now();
        let pin = self.pin_for(r.kind, &r.name, r.cwd.as_deref());
        // An adapter is an internal session that never closes: its agents
        // live by hook events (`SessionEnd`), not by a lease.
        let lease_ttl = if adapter {
            Duration::ZERO
        } else {
            Duration::from_secs(u64::from(if r.lease_ttl_s == 0 {
                self.cfg.lease_ttl_s
            } else {
                r.lease_ttl_s.min(crate::MAX_CLIENT_SECS)
            }))
        };
        let lease_expires = (!adapter).then(|| ts.plus(lease_ttl));
        let (title, title_source) = match r.cwd.as_deref().and_then(project_label) {
            Some(t) => (Some(t), TitleSource::Cwd),
            None => (None, TitleSource::SessionName),
        };
        let voice_key = r
            .key
            .clone()
            .unwrap_or_else(|| format!("{}:{}", kind_name(r.kind), r.name));
        let voice = self.voice_for(
            &voice_key,
            r.voice.clone(),
            pin.as_ref().and_then(|p| p.voice.clone()),
        );
        let agent = Agent {
            id: id.clone(),
            slot: None,
            kind: r.kind,
            name: pin
                .as_ref()
                .and_then(|p| p.name.clone())
                .unwrap_or_else(|| r.name.clone()),
            state: AgentState::Idle,
            title,
            title_source,
            since: ts,
            source: if adapter { Source::Hook } else { Source::Api },
            owner: Owner {
                principal,
                session,
                lease_expires,
            },
            session: SessionInfo {
                key: r.key.clone(),
                cwd: r.cwd.clone(),
                pid: r.pid,
                pid_kind: r.pid_kind,
                terminal: r.terminal.clone(),
            },
            caps: r.caps,
            companion: None,
            card: None,
            reply: None,
            messages: Vec::new(),
            workflow_runs: Vec::new(), task: None, task_source: None, subagents: 0, workflow: None,
            chat_rev: 0,
            reply_rev: 0,
            voice,
            pinned: pin.as_ref().is_some_and(|p| p.slot.is_some()),
            meta: r.meta.clone().unwrap_or(serde_json::Value::Null),
            redacted: false,
        };
        let data = serde_json::json!({"agent": Self::agent_json(&agent)});
        self.snapshot.agents.insert(id.clone(), agent);
        self.aux.insert(
            id.clone(),
            AgentAux {
                lease_ttl,
                lease_at: (!adapter).then(|| self.now + lease_ttl),
                last_active: Some(self.now),
                ..AgentAux::default()
            },
        );
        if !adapter {
            self.timer(TimerKey::Lease(id.clone()), self.now + lease_ttl);
        }
        self.emit(EventKind::AgentAdded, self.principal_of(&id), data);
        self.assign_slot(&id, pin.and_then(|p| p.slot), r.slot_hint);
        Ok(self.registration_result(&id))
    }

    fn registration_result(&self, id: &AgentId) -> serde_json::Value {
        let agent = &self.snapshot.agents[id];
        serde_json::json!({
            "rev": self.snapshot.rev,
            "agent": agent.id,
            "slot": agent.slot,
            "lease": {"expires": agent.owner.lease_expires},
            "owner": {"principal": agent.owner.principal, "session": agent.owner.session},
        })
    }

    pub(crate) fn principal_of(&self, id: &AgentId) -> Principal {
        self.snapshot
            .agents
            .get(id)
            .map(|a| a.owner.principal.clone())
            .unwrap_or(Principal::Uid(0))
    }

    /// A re-registration under the same key: apply what differs, if anything.
    fn apply_registration_changes(&mut self, id: &AgentId, r: &AgentRegister) {
        let mut fields = serde_json::Map::new();
        let agent = match self.snapshot.agents.get_mut(id) {
            Some(a) => a,
            None => return,
        };
        if agent.name != r.name && !r.name.trim().is_empty() {
            agent.name = r.name.clone();
            fields.insert("name".into(), serde_json::json!(agent.name));
        }
        if r.cwd.is_some() && agent.session.cwd != r.cwd {
            agent.session.cwd = r.cwd.clone();
            fields.insert("cwd".into(), serde_json::json!(agent.session.cwd));
        }
        if r.pid.is_some() && agent.session.pid != r.pid {
            agent.session.pid = r.pid;
            fields.insert("pid".into(), serde_json::json!(agent.session.pid));
        }
        // A re-register is how a `SessionStart` that resolved the CLI pid
        // corrects an earlier `unknown`: without this the liveness poll of
        // §5 could never be turned on for a session already on the roster.
        if r.pid_kind.is_some() && agent.session.pid_kind != r.pid_kind {
            agent.session.pid_kind = r.pid_kind;
            fields.insert("pid_kind".into(), serde_json::json!(agent.session.pid_kind));
        }
        if r.terminal.is_some() && agent.session.terminal != r.terminal {
            agent.session.terminal = r.terminal.clone();
            fields.insert("terminal".into(), serde_json::json!(agent.session.terminal));
        }
        if agent.caps != r.caps {
            agent.caps = r.caps;
            fields.insert("caps".into(), serde_json::json!(agent.caps));
        }
        if r.voice.is_some() && agent.voice != r.voice {
            agent.voice = r.voice.clone();
            fields.insert("voice".into(), serde_json::json!(agent.voice));
        }
        if let Some(meta) = &r.meta {
            if agent.meta != *meta {
                agent.meta = meta.clone();
                fields.insert("meta".into(), meta.clone());
            }
        }
        if fields.is_empty() {
            return;
        }
        let name_changed = fields.contains_key("name");
        self.emit(
            EventKind::AgentChanged,
            self.principal_of(id),
            serde_json::json!({"agent": id, "fields": fields}),
        );
        if name_changed {
            self.put_agent_state(id);
        }
    }

    /// Any owner activity: the lease is refreshed (no event — bookkeeping,
    /// not observable state) and an orphaned agent comes back.
    pub(crate) fn touch(&mut self, id: &AgentId) {
        let now = self.now;
        let ts = self.ts_now();
        let Some(aux) = self.aux.get_mut(id) else {
            return;
        };
        let ttl = aux.lease_ttl;
        let leased = !ttl.is_zero();
        aux.lease_at = leased.then(|| now + ttl);
        aux.last_active = Some(now);
        let revived = aux.orphaned_at.take().is_some();
        let saved = aux.saved_title.take();
        if let Some(agent) = self.snapshot.agents.get_mut(id) {
            agent.owner.lease_expires = leased.then(|| ts.plus(ttl));
        }
        if leased {
            self.timer(TimerKey::Lease(id.clone()), now + ttl);
        }
        if revived {
            if let (Some(agent), Some((title, source))) = (self.snapshot.agents.get_mut(id), saved)
            {
                agent.title = title;
                agent.title_source = source;
            }
            self.emit(
                EventKind::AgentChanged,
                self.principal_of(id),
                serde_json::json!({"agent": id, "fields": {"orphaned": false}}),
            );
            self.put_agent_state(id);
            self.put_agent_title(id);
        }
    }

    pub(crate) fn heartbeat(&mut self, session: SessionId, agents: &[AgentId]) -> ApiResult {
        let mut expires = None;
        for id in agents {
            self.owned(id, session)?;
            expires = self
                .snapshot
                .agents
                .get(id)
                .and_then(|a| a.owner.lease_expires);
        }
        Ok(serde_json::json!({"rev": self.snapshot.rev, "expires": expires}))
    }

    /// Lease expired or session closed: kept for `grace_s`, drawn idle
    /// "(disconnected)"; held cards released, push cards downgraded.
    pub(crate) fn orphan(&mut self, id: &AgentId, reason: &str) {
        let now = self.now;
        let ts = self.ts_now();
        let grace = Duration::from_secs(u64::from(self.cfg.grace_s));
        let Some(aux) = self.aux.get_mut(id) else {
            return;
        };
        if aux.orphaned_at.is_some() {
            return;
        }
        aux.orphaned_at = Some(now);
        aux.lease_at = None;
        let Some(agent) = self.snapshot.agents.get_mut(id) else {
            return;
        };
        aux.saved_title = Some((agent.title.take(), agent.title_source));
        agent.title = Some(DISCONNECTED.into());
        agent.title_source = TitleSource::SessionName;
        if agent.state != AgentState::Idle {
            agent.state = AgentState::Idle;
            agent.since = ts;
        }
        let card_action = agent
            .card
            .as_ref()
            .map(|c| (c.id, c.deliver, c.status.hold_parked()));
        self.timer(TimerKey::Grace(id.clone()), now + grace);
        self.emit(
            EventKind::AgentChanged,
            self.principal_of(id),
            serde_json::json!({"agent": id, "fields": {"state": "idle", "title": DISCONNECTED, "orphaned": true, "reason": reason}}),
        );
        self.put_agent_state(id);
        self.put_agent_title(id);
        match card_action {
            Some((card, Deliver::Hold, true)) => {
                self.effects.push(Effect::ReleaseHold {
                    agent: id.clone(),
                    card,
                    reason: crate::Release::AgentGone,
                });
                self.degrade_card(id, "agent_gone");
            }
            Some((_, Deliver::Push, _)) => {
                // §12: push → focus if a terminal is known, else informational.
                let has_terminal = self
                    .snapshot
                    .agents
                    .get(id)
                    .is_some_and(|a| a.session.terminal.is_some());
                if has_terminal {
                    self.degrade_card(id, "owner_gone");
                } else {
                    self.neutralise_card(id, "owner_gone");
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_session_closed(&mut self, session: SessionId) {
        // A capture this session started dies with it. `voice.cancel` is
        // owner-only now, so without this the microphone would stay open —
        // and the deck would stay silent, since a live capture holds the
        // link in SCO — until the 60 s cap, with nobody left who is allowed
        // to stop it. A deck press has `session: None` and is never this.
        if self
            .snapshot
            .voice
            .ptt
            .as_ref()
            .is_some_and(|p| p.session == Some(session))
        {
            self.abort_live("session_closed");
        }
        let ids: Vec<AgentId> = self
            .snapshot
            .agents
            .values()
            .filter(|a| a.owner.session == session)
            .map(|a| a.id.clone())
            .collect();
        for id in ids {
            self.orphan(&id, "session_closed");
        }
    }

    pub(crate) fn is_orphaned(&self, id: &AgentId) -> bool {
        self.aux.get(id).is_some_and(|a| a.orphaned_at.is_some())
    }

    /// `agent.claim{key}`: the only ownership transfer (§4.6).
    pub(crate) fn claim(
        &mut self,
        principal: Principal,
        session: SessionId,
        key: &str,
    ) -> ApiResult {
        let id = self
            .agent_by_key(key)
            .ok_or_else(|| ApiError::not_found("agent"))?;
        let lease_over = self
            .aux
            .get(&id)
            .is_some_and(|a| a.lease_at.is_some_and(|at| at <= self.now));
        if !self.is_orphaned(&id) && !lease_over {
            return Err(ApiError::conflict(&self.snapshot.agents[&id].owner));
        }
        let was_muted = self.is_muted(&self.snapshot.agents[&id]);
        if let Some(agent) = self.snapshot.agents.get_mut(&id) {
            agent.owner.principal = principal;
            agent.owner.session = session;
        }
        self.touch(&id);
        let owner = self.snapshot.agents[&id].owner.clone();
        self.emit(
            EventKind::AgentChanged,
            owner.principal.clone(),
            serde_json::json!({"agent": id, "fields": {"owner": {"principal": owner.principal, "session": owner.session}}}),
        );
        // The new owner's session can be muted where the old one was not,
        // or the other way round: a paused app that reconnects claims the
        // agents its last connection left, slots and all.
        self.follow_mute(&id, was_muted);
        Ok(self.registration_result(&id))
    }

    pub(crate) fn update_agent(&mut self, session: SessionId, u: AgentUpdate) -> ApiResult {
        self.owned(&u.agent, session)?;
        let id = u.agent.clone();
        let ts = self.ts_now();
        let mut fields = serde_json::Map::new();
        let Some(agent) = self.snapshot.agents.get_mut(&id) else {
            return Err(ApiError::not_found("agent"));
        };
        let mut state_changed = false;
        if let Some(state) = u.state {
            if agent.state != state {
                agent.state = state;
                agent.since = u.since.unwrap_or(ts);
                state_changed = true;
                fields.insert("state".into(), serde_json::json!(state));
                fields.insert("since".into(), serde_json::json!(agent.since));
            } else if let Some(since) = u.since.filter(|s| *s != agent.since) {
                agent.since = since;
                fields.insert("since".into(), serde_json::json!(since));
            }
        }
        if let Some(title) = u.title {
            let source = u.title_source.unwrap_or(TitleSource::Api);
            let title = if title.trim().is_empty() {
                None
            } else {
                Some(title)
            };
            if agent.title != title || agent.title_source != source {
                agent.title = title.clone();
                agent.title_source = source;
                fields.insert("title".into(), serde_json::json!(title));
                fields.insert("title_source".into(), serde_json::json!(source));
            }
            // The major task: the CLI's own session title always, else the
            // first prompt of the session (later prompts are steps of it).
            let task = match (source, &agent.task) {
                (TitleSource::SessionName, _) => title,
                (TitleSource::Prompt, None) => title,
                _ => None,
            };
            if let Some(task) = task.filter(|t| agent.task.as_ref() != Some(t)) {
                agent.task = Some(task.clone());
                agent.task_source = Some(source);
                fields.insert("task".into(), serde_json::json!(task));
                fields.insert("task_source".into(), serde_json::json!(source));
            }
        }
        if let Some(task) = u.task {
            let task = crate::sanitize::cut(&crate::sanitize::collapse(&task), 160);
            let task = (!task.trim().is_empty()).then_some(task);
            let source = task.as_ref().map(|_| TitleSource::Api);
            if agent.task != task {
                agent.task = task.clone();
                agent.task_source = source;
                fields.insert("task".into(), serde_json::json!(task));
                fields.insert("task_source".into(), serde_json::json!(source));
            }
        }
        if let Some(subagents) = u.subagents {
            if agent.subagents != subagents {
                agent.subagents = subagents;
                fields.insert("subagents".into(), serde_json::json!(subagents));
            }
        }
        if let Some(name) = u.name.filter(|n| !n.trim().is_empty()) {
            if agent.name != name {
                agent.name = name.clone();
                fields.insert("name".into(), serde_json::json!(name));
            }
        }
        if let Some(meta) = u.meta {
            if agent.meta != meta {
                agent.meta = meta.clone();
                fields.insert("meta".into(), meta);
            }
        }
        if fields.is_empty() {
            return Ok(self.rev());
        }
        let new_state = agent.state;
        let title_changed = fields.contains_key("title");
        let name_changed = fields.contains_key("name")
            || fields.contains_key("task")
            || fields.contains_key("subagents");
        self.emit(
            EventKind::AgentChanged,
            self.principal_of(&id),
            serde_json::json!({"agent": id, "fields": fields}),
        );
        if state_changed || name_changed {
            self.put_agent_state(&id);
        }
        if title_changed {
            self.put_agent_title(&id);
        }
        if state_changed {
            self.after_state_change(&id, new_state);
        }
        Ok(self.rev())
    }

    /// What a state change drags along: Ready-card expiry, the auto-question
    /// bookkeeping, a slot for an agent that woke up, announcements.
    fn after_state_change(&mut self, id: &AgentId, state: AgentState) {
        if state != AgentState::Question {
            if let Some(aux) = self.aux.get_mut(id) {
                aux.prev_state = None;
            }
        }
        let ready_card = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.card.as_ref())
            .is_some_and(|c| c.kind == crate::CardKind::R && c.status.on_device());
        if ready_card {
            self.expire_card(id, "state_change");
        }
        let unslotted = self
            .snapshot
            .agents
            .get(id)
            .is_some_and(|a| a.slot.is_none());
        if unslotted && state != AgentState::Idle {
            self.assign_slot(id, None, None);
        }
        // A question asked or answered, a session started or stopped: the
        // deck's eight slots follow the ranking.
        self.rebalance_slots();
        if self.snapshot.settings.announce == 1 {
            let max_chars = self.cfg.announce_max_chars;
            if let Some(agent) = self.snapshot.agents.get(id) {
                let text = match state {
                    AgentState::Ready => agent.reply.as_ref().and_then(|r| {
                        // §7.6's sanitiser, not a bare first sentence: what
                        // a model writes is markdown with fences in it, and
                        // the synthesiser would read the markup out.
                        r.announce
                            .clone()
                            .or_else(|| {
                                r.text
                                    .as_deref()
                                    .map(|t| crate::sanitize::speech(t, max_chars))
                            })
                            .map(|t| format!("{}: {t}", agent.name))
                    }),
                    AgentState::Error => Some(format!("{} failed", agent.name)),
                    _ => None,
                };
                if let Some(text) = text {
                    self.say(text, Some(id.clone()), "ready");
                }
            }
        }
    }

    /// The daemon's own announcement (§7.7): a new utterance id, the
    /// agent's voice, no interrupt, no deadline.
    ///
    /// The `[voice.tts] announce_max_chars` budget is applied **here**, on
    /// the whole composed line, so no path can compose its way past it: an
    /// agent with a long name and a long question would otherwise add up
    /// to an announcement nobody sits through. `voice.say` from an API
    /// client does not come through here (§7.6) — a caller that asked for
    /// two sentences and silently got one is worse off than one refused.
    pub(crate) fn say(&mut self, text: String, agent: Option<AgentId>, lane: &str) {
        // Nothing of a muted agent is spoken. Its replies, questions and
        // state changes still reach the snapshot and the bus; the speaker is
        // the one place they would still reach the owner who paused it.
        let muted = agent
            .as_ref()
            .and_then(|id| self.snapshot.agents.get(id))
            .is_some_and(|a| self.is_muted(a));
        if muted {
            return;
        }
        let text = crate::sanitize::clip_chars(
            &crate::sanitize::clean(&text),
            self.cfg.announce_max_chars,
        );
        if text.is_empty() {
            return;
        }
        self.utterance_seq += 1;
        let utterance = format!("ut_{}", self.utterance_seq);
        let voice = agent
            .as_ref()
            .and_then(|id| self.snapshot.agents.get(id))
            .and_then(|a| a.voice.clone());
        self.queue_say(AudioCmd::Say {
            utterance,
            text,
            agent,
            lane: lane.to_string(),
            voice,
            interrupt: false,
            ttl_s: None,
        });
    }

    pub(crate) fn reply(&mut self, r: ReplySet) -> ApiResult {
        let id = r.agent.clone();
        if r.text.trim().is_empty() && r.message.as_ref().is_none_or(|m| m.images.is_empty()) {
            return Err(ApiError::invalid("text", "empty"));
        }
        if r.message.as_ref().is_some_and(|m| m.id.is_empty() || m.id.len() > 256) {
            return Err(ApiError::invalid("message.id", "must contain 1 to 256 bytes"));
        }
        let ts = self.ts_now();
        let Some(agent) = self.snapshot.agents.get_mut(&id) else {
            return Err(ApiError::not_found("agent"));
        };
        let mut meta = r.message.clone().unwrap_or_else(|| crate::MessageMeta { summary: None, images: Vec::new(),
            activity: None,
            runs: None, plan: None,
            id: format!("reply-{}-{}", ts.0, agent.reply_rev),
            role: crate::ChatRole::Assistant,
            streaming: false,
        });
        meta.images.truncate(4);
        for image in &mut meta.images { image.id = crate::sanitize::cut(&image.id, 64); image.label = crate::sanitize::cut(&image.label, 96); }
        meta.plan = meta.plan.map(crate::workflow::Plan::bounded);
        meta.summary = meta.summary.filter(|_| meta.role == crate::ChatRole::Tool)
            .map(crate::workflow::ActivitySummary::bounded);
        meta.runs = meta.runs.map(|runs| runs.into_iter().take(crate::workflow::MAX_RUNS).map(crate::workflow::Run::bounded).collect());
        let work_before = agent.active_work();
        crate::workflow::observe(agent, &meta);
        let work_changed = agent.active_work() != work_before;
        let duplicate = r.message.is_none() && agent.messages.last().is_some_and(|m|
            m.message.role == crate::ChatRole::Assistant && m.text == r.text);
        if duplicate {
            if let Some(last) = agent.messages.last_mut() { last.message.streaming = false; }
        }
        if !duplicate {
            let text = crate::sanitize::cut(&r.text, 64 * 1024);
            if r.message.is_some() && agent.messages.last().is_some_and(|m| m.message.id.starts_with("reply-") && m.message.role == meta.role && m.text == text) {
                if let Some(old) = agent.messages.last_mut() { old.message = meta.clone(); }
            } else if let Some(old) = agent.messages.iter_mut().find(|m| m.message.id == meta.id) {
                if old.message.images != meta.images { agent.chat_rev = agent.chat_rev.wrapping_add(1).max(1); }
                old.text = text;
                old.message = meta.clone();
            } else {
                agent.messages.push(crate::ChatMessage { message: meta.clone(), text, at: ts });
            }
            agent.chat_rev = agent.chat_rev.max(1);
            while agent.messages.len() > 64 || agent.messages.iter().map(|m| m.text.len()).sum::<usize>() > 512 * 1024 {
                agent.messages.remove(0);
                agent.chat_rev = agent.chat_rev.wrapping_add(1).max(1);
            }
        }
        if meta.role == crate::ChatRole::Assistant && !meta.streaming {
            agent.reply = Some(Reply {
                text: Some(r.text.clone()), at: ts, announce: r.announce.clone(), redacted: false,
            });
            agent.reply_rev = agent.reply_rev.wrapping_add(1).max(1);
        }
        self.emit(
            EventKind::ReplySet,
            self.principal_of(&id),
            serde_json::json!({"agent": id, "text": r.text, "message": meta}),
        );
        if self.aux.get(&id).is_some_and(|a| a.workflow_view) {
            let agent = self.snapshot.agents.get_mut(&id).unwrap();
            agent.chat_rev = agent.chat_rev.wrapping_add(1).max(1);
        }
        self.put_reply(&id);
        if work_changed {
            // The roster's count of running work.
            self.put_agent_state(&id);
        }
        if r.speak && meta.role == crate::ChatRole::Assistant && !meta.streaming && !r.text.trim().is_empty() {
            let lane = r.lane.as_deref().unwrap_or("chatter").to_string();
            self.say(r.text, Some(id), &lane);
        }
        Ok(self.rev())
    }

    /// Opening a native session can replace an unpinned sibling of the same
    /// owning connection, application and account/workspace. Launchers stay.
    fn promote_companion_chat(&mut self, id: &AgentId) {
        let agent = &self.snapshot.agents[id];
        // A muted chat is never promoted. Its siblings share its session, so
        // none of them holds a slot to give it, but this path places it
        // without asking `assign_slot` and says so itself.
        if agent.companion.is_none() || agent.slot.is_some() || self.is_muted(agent) { return; }
        self.assign_slot(id, None, None);
        let agent = &self.snapshot.agents[id];
        if agent.slot.is_some() { return; }
        let profile = agent.companion.as_ref().unwrap();
        let victim = self.snapshot.agents.values().filter(|other| {
            other.slot.is_some() && !other.pinned && other.owner.session == agent.owner.session
                && other.companion.as_ref().is_some_and(|p| p.app_id == profile.app_id
                    && p.group_id == profile.group_id && p.role != crate::companion::CompanionRole::Launcher)
        }).min_by_key(|other| (
            other.card.as_ref().is_some_and(|card| card.status.on_device()),
            other.state == AgentState::Working, other.since, other.id.clone(),
        )).map(|other| other.id.clone());
        if let Some(victim) = victim {
            if let Some(slot) = self.unslot(&victim) { self.place(id, slot); }
        }
    }

    /// `agent.focus` / `op.focus`: the desktop, then `focus` to the device
    /// without `ack` (§4.7).
    pub(crate) fn focus_agent(
        &mut self,
        id: &AgentId,
        source: FocusSource,
        principal: Principal,
    ) -> ApiResult {
        self.exists(id)?;
        self.promote_companion_chat(id);
        let slot = self.snapshot.agents[id].slot;
        self.snapshot.focus.agent = Some(id.clone());
        self.snapshot.focus.source = Some(source);
        // Native clients receive focus.changed and bring their own window forward.
        if !self.snapshot.agents[id].native_companion() {
            self.effects.push(Effect::Desktop(DesktopCmd::Focus { agent: id.clone() }));
        }
        self.put_focus(slot);
        self.emit(
            EventKind::FocusChanged,
            principal,
            serde_json::json!({"agent": id, "slot": slot, "source": source}),
        );
        Ok(self.rev())
    }

    fn put_focus(&mut self, slot: Option<Slot>) {
        match slot {
            Some(slot) => self.device_op(DeviceOp::Put(
                Key::Focus,
                HostLine::Focus {
                    slot: slot.0,
                    ack: None,
                },
            )),
            None => self.device_op(DeviceOp::Remove(Key::Focus)),
        }
    }

    /// A PC-side tracker reports the focused agent (§4.7).
    pub(crate) fn focus_report(&mut self, f: FocusReport, principal: Principal) -> ApiResult {
        if let Some(id) = &f.agent {
            self.exists(id)?;
        }
        if f.source == FocusSource::Device {
            return Err(ApiError::invalid("source", "device is not a PC source"));
        }
        let focus = &self.snapshot.focus;
        if focus.agent == f.agent && focus.source == Some(f.source) {
            return Ok(self.rev());
        }
        let slot = f
            .agent
            .as_ref()
            .and_then(|id| self.snapshot.agents.get(id))
            .and_then(|a| a.slot);
        self.snapshot.focus.agent = f.agent.clone();
        self.snapshot.focus.source = Some(f.source);
        self.put_focus(slot);
        self.emit(
            EventKind::FocusChanged,
            principal,
            serde_json::json!({"agent": f.agent, "slot": slot, "source": f.source}),
        );
        Ok(self.rev())
    }

    // ----- slots (§4.15) -----------------------------------------------------

    fn occupant(&self, slot: Slot) -> Option<AgentId> {
        self.snapshot.agent_at(slot).map(|a| a.id.clone())
    }

    /// A deck reported how many sessions it holds: the sessions in slots
    /// above that leave the deck, and a larger deck fills its new slots.
    pub(crate) fn set_capacity(&mut self, sessions: u8) {
        if self.snapshot.device.sessions == Some(sessions) {
            return;
        }
        self.snapshot.device.sessions = Some(sessions);
        let over: Vec<AgentId> = self
            .snapshot
            .agents
            .values()
            .filter(|a| a.slot.is_some_and(|s| s.0 > sessions))
            .map(|a| a.id.clone())
            .collect();
        for id in over {
            self.unslot(&id);
        }
        self.fill_free_slots();
        self.rebalance_slots();
    }

    fn lowest_free(&self) -> Option<Slot> {
        (1..=self.snapshot.device.capacity())
            .map(Slot)
            .find(|s| self.occupant(*s).is_none())
    }

    /// How much a session deserves one of the deck's eight slots, highest
    /// first: one asking a question; a running CLI session; any other CLI
    /// session; a running companion chat or a companion's launcher; any other
    /// companion chat. A companion (Conol) keeps every chat in its own app
    /// and gets a chat's slot back when it is opened there, so its chats
    /// yield to the terminal sessions the deck is the only view of.
    pub(crate) fn slot_rank(agent: &Agent) -> u8 {
        let asking = agent.state == AgentState::Question
            || agent.card.as_ref().is_some_and(|c| c.kind == crate::CardKind::Q);
        let companion = agent.native_companion();
        let launcher = agent
            .companion
            .as_ref()
            .is_some_and(|p| p.role == crate::companion::CompanionRole::Launcher);
        match (asking, companion, agent.state == AgentState::Working) {
            (true, _, _) => 5,
            (false, false, true) => 4,
            (false, false, false) => 3,
            (false, true, true) => 2,
            (false, true, false) if launcher => 2,
            (false, true, false) => 1,
        }
    }

    /// The slotted session `id` may take the slot of when the deck is full:
    /// the lowest-ranked one ([`Self::slot_rank`]) that ranks below `id`,
    /// longest idle first. Unless `strict`, an idle session also yields to an
    /// equal one, so a newcomer still replaces the longest-idle session of
    /// its own kind; a rebalance is strict, or two equals would trade places
    /// forever. Never a pinned one, one whose card is on the deck, or the
    /// focus.
    fn eviction_candidate(&self, id: &AgentId, strict: bool) -> Option<AgentId> {
        let incoming = self.snapshot.agents.get(id)?;
        let key = |a: &Agent| (Self::slot_rank(a), a.state != AgentState::Idle);
        let wanted = key(incoming);
        let focus = self.snapshot.focus.agent.as_ref();
        self.snapshot
            .agents
            .values()
            .filter(|a| {
                a.slot.is_some()
                    && &a.id != id
                    && !a.pinned
                    && Some(&a.id) != focus
                    && !a.card.as_ref().is_some_and(|c| c.status.on_device())
                    && (key(a) < wanted
                        || (!strict && a.state == AgentState::Idle && Self::slot_rank(a) <= wanted.0))
            })
            .min_by_key(|a| (key(a), a.since, a.id.clone()))
            .map(|a| a.id.clone())
    }

    /// Give `id` a slot by the policy; evicts when the deck is full. Never
    /// to an agent of a muted session, whoever asks: its registration, its
    /// question, its state change or a freed slot.
    pub(crate) fn assign_slot(&mut self, id: &AgentId, pin: Option<Slot>, hint: Option<Slot>) {
        let Some(agent) = self.snapshot.agents.get(id) else {
            return;
        };
        if agent.slot.is_some() || self.is_muted(agent) {
            return;
        }
        let key = agent.session.key.clone();
        let kind = agent.kind;
        let cwd = agent.session.cwd.clone();
        let fits = |s: &Slot| s.is_valid() && s.0 <= self.snapshot.device.capacity();
        if let Some(p) = pin.filter(fits) {
            match self.occupant(p) {
                None => return self.place(id, p),
                Some(other) if !self.snapshot.agents[&other].pinned => {
                    // The pin wins; the occupant moves to another slot if one is free.
                    self.unslot(&other);
                    self.place(id, p);
                    self.fill_free_slots();
                    return;
                }
                Some(_) => {}
            }
        }
        let remembered = key
            .as_deref()
            .and_then(|k| self.slots.get(k))
            .map(|m| m.slot)
            .or_else(|| {
                cwd.as_deref().and_then(|c| {
                    self.slots
                        .values()
                        .find(|m| m.kind == kind && m.cwd.as_deref() == Some(c))
                        .map(|m| m.slot)
                })
            });
        if let Some(s) = remembered.filter(|s| fits(s) && self.occupant(*s).is_none()) {
            return self.place(id, s);
        }
        if let Some(s) = hint.filter(|s| fits(s) && self.occupant(*s).is_none()) {
            return self.place(id, s);
        }
        if let Some(s) = self.lowest_free() {
            return self.place(id, s);
        }
        if let Some(victim) = self.eviction_candidate(id, false) {
            if let Some(s) = self.unslot(&victim) {
                self.place(id, s);
            }
        }
    }

    fn place(&mut self, id: &AgentId, slot: Slot) {
        let ts = self.ts_now();
        let now = self.now;
        let Some(agent) = self.snapshot.agents.get_mut(id) else {
            return;
        };
        agent.slot = Some(slot);
        let memory = agent.session.key.clone().map(|key| {
            (
                key,
                crate::SlotMemory {
                    slot,
                    last_seen: ts,
                    kind: agent.kind,
                    cwd: agent.session.cwd.clone(),
                },
            )
        });
        let live_card = agent.card.as_ref().is_some_and(|c| c.status.on_device());
        if let Some(aux) = self.aux.get_mut(id) {
            aux.last_active = Some(now);
        }
        if let Some((key, memory)) = memory {
            self.effects.push(Effect::Persist(StatePatch::Slot {
                key: key.clone(),
                slot,
                kind: memory.kind,
                cwd: memory.cwd.clone(),
                last_seen: ts,
            }));
            self.slots.insert(key, memory);
        }
        if live_card {
            // Off-deck ids are re-allocated from the slot's counter (§6.5).
            let new_id = self.next_card_id(slot);
            if let Some(card) = self
                .snapshot
                .agents
                .get_mut(id)
                .and_then(|a| a.card.as_mut())
            {
                card.id = new_id;
            }
            let card = Self::card_json(&self.snapshot.agents[id]);
            self.emit(
                EventKind::CardSet,
                self.principal_of(id),
                serde_json::json!({"agent": id, "card": card}),
            );
        }
        self.emit(
            EventKind::AgentSlot,
            self.principal_of(id),
            serde_json::json!({"agent": id, "slot": slot}),
        );
        self.put_agent_all(id);
    }

    /// Take the agent off the deck; the device forgets the slot (`agent gone`).
    pub(crate) fn unslot(&mut self, id: &AgentId) -> Option<Slot> {
        let slot = self.snapshot.agents.get_mut(id)?.slot.take()?;
        self.device_op(DeviceOp::Put(
            Key::AgentGone(slot),
            HostLine::AgentGone { slot: slot.0 },
        ));
        self.device_op(DeviceOp::Remove(Key::AgentState(slot)));
        self.reset_slot(slot);
        if self.snapshot.focus.agent.as_ref() == Some(id) {
            self.snapshot.focus.agent = None;
            self.device_op(DeviceOp::Remove(Key::Focus));
        }
        self.emit(
            EventKind::AgentSlot,
            self.principal_of(id),
            serde_json::json!({"agent": id, "slot": null}),
        );
        Some(slot)
    }

    /// Rankings changed (a state, a question, a companion profile): while a
    /// session off the deck strictly outranks one on it, it takes that slot.
    /// A muted agent is never one of the waiting, however it ranks: the one
    /// picked here is placed directly, past `assign_slot`.
    pub(crate) fn rebalance_slots(&mut self) {
        for _ in 0..Slot::MAX {
            let waiting = self
                .snapshot
                .agents
                .values()
                .filter(|a| a.slot.is_none() && !self.is_muted(a))
                .map(|a| (Self::slot_rank(a), a.state != AgentState::Idle, self.aux.get(&a.id).and_then(|x| x.last_active), a.id.clone()))
                .max_by_key(|(rank, busy, active, id)| (*rank, *busy, *active, std::cmp::Reverse(id.clone())))
                .map(|(_, _, _, id)| id);
            let Some(waiting) = waiting else { return };
            let Some(victim) = self.eviction_candidate(&waiting, true) else { return };
            let Some(slot) = self.unslot(&victim) else { return };
            self.place(&waiting, slot);
        }
    }

    /// A slot freed: the highest-ranked unslotted agent takes it, the most
    /// recently active among equals. Never a muted one: `assign_slot` would
    /// refuse it, and the loop below would stop with the slot still free.
    pub(crate) fn fill_free_slots(&mut self) {
        while self.lowest_free().is_some() {
            let Some(next) = self
                .snapshot
                .agents
                .values()
                .filter(|a| a.slot.is_none() && !self.is_muted(a))
                .map(|a| {
                    (
                        Self::slot_rank(a),
                        self.aux.get(&a.id).and_then(|x| x.last_active),
                        a.id.clone(),
                    )
                })
                // The highest-ranked waiting session first, then the most
                // recently active.
                .max_by_key(|(rank, active, id)| (*rank, *active, std::cmp::Reverse(id.clone())))
                .map(|(_, _, id)| id)
            else {
                return;
            };
            self.assign_slot(&next, None, None);
            if self.snapshot.agents[&next].slot.is_none() {
                return;
            }
        }
    }

    // ----- muted sessions (paused sources) -----------------------------------

    /// Whether `agent`'s session is muted (`Command::Mute`): the agent is
    /// kept, served and published, but it is not on the deck and is never
    /// heard.
    pub(crate) fn is_muted(&self, agent: &Agent) -> bool {
        self.muted.contains(&agent.owner.session)
    }

    /// `Command::Mute`: the core's new set of muted sessions, the sources
    /// the owner paused. An agent whose session joins the set gives its
    /// parked question back to its tool and leaves the deck, and the slots
    /// it frees go to whoever is waiting. One whose session leaves the set
    /// is given a slot by the policy, the way a newcomer is. Nothing else
    /// about either changes: both stay registered, their owners' calls keep
    /// working, and the snapshot and the bus carry them.
    pub(crate) fn on_mute(&mut self, sessions: BTreeSet<SessionId>) {
        if sessions == self.muted {
            return;
        }
        let before = std::mem::replace(&mut self.muted, sessions);
        let hiding: Vec<AgentId> = self
            .snapshot
            .agents
            .values()
            .filter(|a| self.is_muted(a) && !before.contains(&a.owner.session))
            .map(|a| a.id.clone())
            .collect();
        let mut showing: Vec<&Agent> = self
            .snapshot
            .agents
            .values()
            .filter(|a| !self.is_muted(a) && before.contains(&a.owner.session))
            .collect();
        // Seated as `rebalance_slots` picks the waiting: the highest-ranked
        // first, then the busy, then the most recently active. So when the
        // deck cannot take them all, a question or a busy session is never
        // displaced by an idle one seated after it (`eviction_candidate`).
        showing.sort_by_key(|a| {
            std::cmp::Reverse((
                Self::slot_rank(a),
                a.state != AgentState::Idle,
                self.aux.get(&a.id).and_then(|x| x.last_active),
            ))
        });
        let showing: Vec<AgentId> = showing.into_iter().map(|a| a.id.clone()).collect();
        self.reseat(&hiding, &showing);
    }

    /// One agent changed owner (`claim`), and maybe with it whether its
    /// session is muted: it leaves the deck or comes back, as it would have
    /// if its whole session had joined or left the set.
    fn follow_mute(&mut self, id: &AgentId, was_muted: bool) {
        let Some(muted) = self.snapshot.agents.get(id).map(|a| self.is_muted(a)) else {
            return;
        };
        match (was_muted, muted) {
            (false, true) => self.reseat(std::slice::from_ref(id), &[]),
            (true, false) => self.reseat(&[], std::slice::from_ref(id)),
            _ => {}
        }
    }

    /// `hiding` agents' sessions joined the muted set and `showing` agents'
    /// sessions left it. A hiding agent's parked question goes back to its
    /// tool and the agent leaves the deck; the slots freed go to whoever is
    /// waiting; then each showing agent is given a slot by the policy, the
    /// way a newcomer is (its pin, the slot it had, a free one, else one it
    /// outranks).
    fn reseat(&mut self, hiding: &[AgentId], showing: &[AgentId]) {
        // A question still parked for a showing agent (one that reached it
        // while it was muted, which `card_set` leaves to the next tick) goes
        // back first, before the fill below or `assign_slot` can seat that
        // agent: a slot gives its card a new id (§6.5), and the hold parked
        // under the old one could then be neither answered nor released.
        for id in showing {
            self.release_to_tool(id);
        }
        for id in hiding {
            self.release_to_tool(id);
            self.unslot(id);
        }
        if !hiding.is_empty() {
            self.fill_free_slots();
        }
        for id in showing {
            let pin = self
                .snapshot
                .agents
                .get(id)
                .and_then(|a| self.pin_for(a.kind, &a.name, a.session.cwd.as_deref()))
                .and_then(|p| p.slot);
            self.assign_slot(id, pin, None);
        }
    }

    /// Remove the agent for good (`agent.release`, grace expiry).
    pub(crate) fn remove_agent(&mut self, id: &AgentId, reason: &str) {
        let Some(agent) = self.snapshot.agents.get(id) else {
            return;
        };
        if let Some(card) = agent
            .card
            .as_ref()
            .filter(|c| c.deliver == Deliver::Hold && c.status.hold_parked())
        {
            // The agent and its card go together: nothing is degraded, so
            // this is `Cleared` and not the `AgentGone` of the *orphaned*
            // path above, which does leave a `⌨ Focus terminal` behind.
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: card.id,
                reason: crate::Release::Cleared,
            });
        }
        self.unslot(id);
        let principal = self.principal_of(id);
        self.snapshot.agents.remove(id);
        self.aux.remove(id);
        if self.snapshot.focus.agent.as_ref() == Some(id) {
            self.snapshot.focus.agent = None;
        }
        if self
            .snapshot
            .voice
            .ptt
            .as_ref()
            .is_some_and(|p| p.agent.as_ref() == Some(id))
        {
            self.abort_ptt("agent_gone");
        }
        self.emit(
            EventKind::AgentRemoved,
            principal,
            serde_json::json!({"agent": id, "reason": reason}),
        );
        self.fill_free_slots();
    }

    pub(crate) fn caps_of(&self, id: &AgentId) -> Caps {
        self.snapshot
            .agents
            .get(id)
            .map(|a| a.caps)
            .unwrap_or_default()
    }
}

/// The project a working directory names: its last path component. It is
/// the state-class default title, the `proj` of the device's `agent` line
/// (the roster's pages) and the label of the GUI's roster groups, so every
/// surface spells a project the same way.
pub fn project_label(cwd: &str) -> Option<String> {
    let name = cwd.trim_end_matches('/').rsplit('/').next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The wire spelling of a `Kind`, which is what an `[agents]` pin and a
/// `state.json` voice key are written with (`"claude:cohdl-api"`).
fn kind_name(kind: crate::Kind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{claude_adapter, register, Sim, UID, UID2};
    use crate::{CardKind, Command, LinkEvent, PinConfig, Request, Ts};

    #[test]
    fn the_task_is_the_first_prompt_or_the_session_title_and_the_count_adds_running_runs() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "web-ui", None);
        let titled = |text: &str, source: TitleSource| Request::AgentUpdate(AgentUpdate {
            agent: id.clone(), state: None, title: Some(text.into()), title_source: Some(source),
            since: None, name: None, meta: None, task: None, subagents: None,
        });
        let state_line = |sim: &Sim| sim.effects.iter().rev().find_map(|e| match e {
            Effect::Device(DeviceOp::Put(Key::AgentState(_), HostLine::Agent { task, subs, .. })) => Some((task.clone(), *subs)),
            _ => None,
        });
        sim.api(UID, SessionId(1), titled("Refactor the auth module", TitleSource::Prompt)).unwrap();
        assert_eq!(state_line(&sim), Some((Some("Refactor the auth module".into()), Some(0))));
        sim.api(UID, SessionId(1), titled("also update the changelog", TitleSource::Prompt)).unwrap();
        let agent = &sim.store.snapshot().agents[&id];
        assert_eq!(agent.task.as_deref(), Some("Refactor the auth module"), "later prompts are steps of the task");
        assert_eq!(agent.title.as_deref(), Some("also update the changelog"));
        sim.api(UID, SessionId(1), titled("Auth refactor", TitleSource::SessionName)).unwrap();
        assert_eq!(sim.store.snapshot().agents[&id].task.as_deref(), Some("Auth refactor"), "the CLI's own title wins");

        // Subagents reported by the owner, plus workflow runs still running.
        sim.api(UID, SessionId(1), Request::AgentUpdate(AgentUpdate {
            agent: id.clone(), state: None, title: None, title_source: None, since: None, name: None,
            meta: None, task: None, subagents: Some(2),
        })).unwrap();
        assert_eq!(state_line(&sim).unwrap().1, Some(2));
        let run = |rid: &str, status| crate::workflow::Run {
            id: rid.into(), name: "review".into(), status, completed: 0, total: 3, elapsed_ms: 0, tokens: 0,
        };
        sim.api(UID, SessionId(1), Request::Reply(crate::ReplySet {
            agent: id.clone(), text: "Running a workflow".into(), speak: false, announce: None, lane: None,
            message: Some(crate::MessageMeta {
                summary: None, images: vec![], id: "wf".into(), role: crate::ChatRole::Tool, streaming: true,
                plan: None, activity: None,
                runs: Some(vec![run("a", crate::workflow::ActivityStatus::Running), run("b", crate::workflow::ActivityStatus::Completed)]),
            }),
        })).unwrap();
        assert_eq!(sim.store.snapshot().agents[&id].active_work(), 3);
        assert_eq!(state_line(&sim).unwrap().1, Some(3), "a new running run re-sends the state line");

        // `""` clears the task; an observer never sees a prompt-derived one.
        sim.api(UID, SessionId(1), Request::AgentUpdate(AgentUpdate {
            agent: id.clone(), state: None, title: None, title_source: None, since: None, name: None,
            meta: None, task: Some(String::new()), subagents: None,
        })).unwrap();
        assert_eq!(sim.store.snapshot().agents[&id].task, None);
        sim.api(UID, SessionId(1), titled("Ship the release", TitleSource::Prompt)).unwrap();
        let mut agent = sim.store.snapshot().agents[&id].clone();
        crate::redact::redact_agent(&mut agent);
        assert_eq!(agent.task, None, "a prompt is content");
    }

    fn update(id: &AgentId, state: AgentState) -> Request {
        Request::AgentUpdate(AgentUpdate {
            agent: id.clone(),
            state: Some(state),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None, task: None, subagents: None,
        })
    }

    #[test]
    fn register_adds_the_agent_to_the_lowest_free_slot_with_its_lease() {
        let mut sim = Sim::new();
        let mut r = register("cohdl-api", Some("myapp:1"));
        r.cwd = Some("/Users/tony/projects/cohdl/".into());
        r.lease_ttl_s = 30;
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["slot"], 1);
        let id = AgentId(res["agent"].as_str().unwrap().to_string());
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.state, AgentState::Idle);
        assert_eq!(a.title.as_deref(), Some("cohdl"), "cwd-derived title");
        assert_eq!(a.title_source, TitleSource::Cwd);
        assert_eq!(a.source, Source::Api);
        assert_eq!(a.owner.session, SessionId(1));
        assert_eq!(
            a.owner.lease_expires,
            Some(sim.store.ts(sim.t).plus(Duration::from_secs(30)))
        );
        assert_eq!(
            sim.kinds(),
            vec![EventKind::AgentAdded, EventKind::AgentSlot]
        );
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Timer {
                key: TimerKey::Lease(a),
                ..
            } if *a == id
        )));
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Persist(StatePatch::Slot { key, slot: Slot(1), .. }) if key == "myapp:1"
        )));
        assert!(sim.puts().contains(&Key::AgentState(Slot(1))));
        assert!(sim.puts().contains(&Key::AgentTitle(Slot(1))));
        assert_eq!(sim.store.slot_memory()["myapp:1"].slot, Slot(1));
    }

    #[test]
    fn register_is_idempotent_by_key_within_a_session() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        let rev = sim.store.snapshot().rev;
        let res = sim
            .api(
                UID,
                SessionId(1),
                Request::AgentRegister(register("bot", Some("k:1"))),
            )
            .unwrap();
        assert_eq!(res["agent"], id.0);
        assert_eq!(sim.store.snapshot().rev, rev, "nothing changed, no rev");
        assert!(sim.events.is_empty());
        let mut r = register("bot-renamed", Some("k:1"));
        r.meta = Some(serde_json::json!({"x": 1}));
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["agent"], id.0);
        assert_eq!(sim.kinds(), vec![EventKind::AgentChanged]);
        assert_eq!(sim.events[0].data["fields"]["name"], "bot-renamed");
        assert_eq!(sim.store.snapshot().agents.len(), 1);
    }

    #[test]
    fn the_same_key_from_another_live_session_conflicts_whoever_runs_it() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", Some("k:1"));
        for (principal, session) in [(UID, SessionId(2)), (UID2, SessionId(3))] {
            let err = sim
                .api(
                    principal,
                    session,
                    Request::AgentRegister(register("bot", Some("k:1"))),
                )
                .unwrap_err();
            assert_eq!(err.code, ApiError::CONFLICT);
            assert_eq!(err.data["owner"]["session"], "sess_1");
        }
        let err = sim
            .api(
                UID2,
                SessionId(3),
                Request::AgentClaim { key: "k:1".into() },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT, "claim only after expiry");
    }

    /// B1: `pid_kind` decides whether §5's liveness poll may `kill(pid, 0)`
    /// an agent at all, so `agent.register` must be able to set it — and a
    /// later hook that resolved the CLI must be able to correct it. Before
    /// this the field was hard-coded `None` and no agent was ever pollable.
    #[test]
    fn pid_kind_is_set_by_the_registration_and_corrected_by_a_re_register() {
        use crate::PidKind;
        let wire: AgentRegister = serde_json::from_value(serde_json::json!({
            "kind": "claude", "name": "c", "caps": {"answer": "hold", "order": "inject"},
            "pid": 7, "pid_kind": "cli",
        }))
        .expect("pid_kind on the wire");
        assert_eq!(wire.pid_kind, Some(PidKind::Cli));
        assert_eq!(register("bot", None).pid_kind, None, "optional");

        let mut sim = Sim::new();
        let mut r = register("bot", Some("k:1"));
        r.pid = Some(4242);
        r.pid_kind = Some(PidKind::Unknown);
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        let id = AgentId(res["agent"].as_str().unwrap().to_string());
        let pid_kind = |sim: &Sim| sim.store.snapshot().agents[&id].session.pid_kind;
        assert_eq!(pid_kind(&sim), Some(PidKind::Unknown));

        let mut again = register("bot", Some("k:1"));
        again.pid_kind = Some(PidKind::Cli);
        sim.api(UID, SessionId(1), Request::AgentRegister(again))
            .unwrap();
        assert_eq!(pid_kind(&sim), Some(PidKind::Cli));
        let changed = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::AgentChanged)
            .expect("the change is an event");
        assert_eq!(changed.data["fields"]["pid_kind"], "cli");

        // An omitted `pid_kind` never clears a resolved one.
        sim.api(
            UID,
            SessionId(1),
            Request::AgentRegister(register("bot", Some("k:1"))),
        )
        .unwrap();
        assert_eq!(pid_kind(&sim), Some(PidKind::Cli));
    }

    #[test]
    fn adapter_only_caps_are_refused_from_the_api() {
        let mut sim = Sim::new();
        let mut r = register("bot", None);
        r.caps.answer = AnswerCap::Hold;
        let err = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        assert_eq!(err.data["field"], "caps.answer");
        let mut r = register("bot", None);
        r.caps.order = OrderCap::Inject;
        let err = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap_err();
        assert_eq!(err.data["field"], "caps.order");
        let mut r = register("bot", None);
        r.caps.answer = AnswerCap::Hold;
        r.caps.order = OrderCap::Inject;
        assert!(sim
            .api(claude_adapter(), SessionId(1), Request::AgentRegister(r))
            .is_ok());
        let mut r = register("  ", None);
        r.slot_hint = Some(Slot(0));
        let err = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap_err();
        assert_eq!(err.data["field"], "name");
        let mut r = register("x", None);
        r.slot_hint = Some(Slot(0));
        let err = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap_err();
        assert_eq!(err.data["field"], "slot_hint");
        assert!(sim.store.snapshot().agents.len() == 1);
    }

    #[test]
    fn a_lease_expires_into_the_orphan_grace_and_then_removal() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        sim.api(UID, SessionId(1), update(&id, AgentState::Working))
            .unwrap();
        sim.advance(Duration::from_secs(29));
        assert!(!sim.store.is_orphaned(&id));
        sim.advance(Duration::from_secs(2));
        assert!(sim.store.is_orphaned(&id));
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.state, AgentState::Idle);
        assert_eq!(a.title.as_deref(), Some(DISCONNECTED));
        assert_eq!(a.slot, Some(Slot(1)), "still on deck during the grace");
        assert_eq!(sim.events[0].kind, EventKind::AgentChanged);
        assert_eq!(sim.events[0].data["fields"]["orphaned"], true);
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Timer {
                key: TimerKey::Grace(_),
                ..
            }
        )));
        assert!(sim.puts().contains(&Key::AgentState(Slot(1))));
        sim.advance(Duration::from_secs(299));
        assert!(sim.store.snapshot().agents.contains_key(&id));
        sim.advance(Duration::from_secs(2));
        assert!(!sim.store.snapshot().agents.contains_key(&id));
        assert_eq!(sim.events[0].kind, EventKind::AgentSlot);
        assert_eq!(sim.events[1].kind, EventKind::AgentRemoved);
        assert_eq!(sim.events[1].data["reason"], "grace");
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::AgentGone(Slot(1)),
            HostLine::AgentGone { slot: 1 }
        ))));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Remove(Key::AgentState(Slot(1))))));
    }

    #[test]
    fn heartbeats_and_owner_calls_refresh_the_lease() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        sim.advance(Duration::from_secs(20));
        let res = sim
            .api(
                UID,
                SessionId(1),
                Request::AgentHeartbeat {
                    agents: vec![id.clone()],
                },
            )
            .unwrap();
        assert_eq!(
            res["expires"],
            serde_json::json!(sim.store.ts(sim.t).plus(Duration::from_secs(30)))
        );
        assert!(sim.events.is_empty(), "a refresh is not an event");
        sim.advance(Duration::from_secs(20));
        assert!(!sim.store.is_orphaned(&id));
        sim.api(UID, SessionId(1), update(&id, AgentState::Working))
            .unwrap();
        sim.advance(Duration::from_secs(25));
        assert!(!sim.store.is_orphaned(&id));
        sim.advance(Duration::from_secs(10));
        assert!(sim.store.is_orphaned(&id));
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::AgentHeartbeat {
                    agents: vec![id.clone()],
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
    }

    #[test]
    fn the_owning_session_revives_its_orphan_and_a_new_session_claims_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        sim.api(
            UID,
            SessionId(1),
            Request::AgentUpdate(AgentUpdate {
                agent: id.clone(),
                state: Some(AgentState::Working),
                title: Some("Compiling".into()),
                title_source: None,
                since: None,
                name: None,
                meta: None, task: None, subagents: None,
            }),
        )
        .unwrap();
        sim.advance(Duration::from_secs(31));
        assert!(sim.store.is_orphaned(&id));
        // Same session re-registers: revived, title restored.
        sim.api(
            UID,
            SessionId(1),
            Request::AgentRegister(register("bot", Some("k:1"))),
        )
        .unwrap();
        assert!(!sim.store.is_orphaned(&id));
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.title.as_deref(), Some("Compiling"));
        assert_eq!(a.state, AgentState::Idle, "the owner reports its state");
        sim.advance(Duration::from_secs(31));
        assert!(sim.store.is_orphaned(&id));
        let res = sim
            .api(
                UID2,
                SessionId(5),
                Request::AgentClaim { key: "k:1".into() },
            )
            .unwrap();
        assert_eq!(res["agent"], id.0);
        assert_eq!(res["owner"]["session"], "sess_5");
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.owner.principal, UID2);
        assert!(sim.events.iter().any(|e| e.kind == EventKind::AgentChanged
            && e.data["fields"]["owner"]["session"] == "sess_5"));
        // The old session lost it.
        let err = sim
            .api(UID, SessionId(1), update(&id, AgentState::Working))
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["session"], "sess_5");
        let err = sim
            .api(
                UID2,
                SessionId(5),
                Request::AgentClaim { key: "nope".into() },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::NOT_FOUND);
    }

    #[test]
    fn a_closed_session_orphans_every_agent_it_owned() {
        let mut sim = Sim::new();
        let a = sim.register(UID, SessionId(1), "a", Some("k:a"));
        let b = sim.register(UID, SessionId(1), "b", Some("k:b"));
        let c = sim.register(UID, SessionId(2), "c", Some("k:c"));
        sim.apply(Command::SessionClosed(SessionId(1)));
        assert!(sim.store.is_orphaned(&a) && sim.store.is_orphaned(&b));
        assert!(!sim.store.is_orphaned(&c));
        assert_eq!(sim.events[0].data["fields"]["reason"], "session_closed");
        sim.apply(Command::SessionClosed(SessionId(1)));
        assert!(sim.events.is_empty(), "already orphaned");
    }

    #[test]
    fn update_changes_only_what_differs_and_bumps_rev_once() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let rev = sim.store.snapshot().rev;
        let res = sim
            .api(UID, SessionId(1), update(&id, AgentState::Idle))
            .unwrap();
        assert_eq!(res["rev"], rev);
        assert!(sim.events.is_empty() && sim.puts().is_empty());
        sim.api(
            UID,
            SessionId(1),
            Request::AgentUpdate(AgentUpdate {
                agent: id.clone(),
                state: Some(AgentState::Working),
                title: Some("Compiling openmicro".into()),
                title_source: Some(TitleSource::Prompt),
                since: Some(Ts(5)),
                name: Some("renamed".into()),
                meta: Some(serde_json::json!({"k": 1})), task: None, subagents: None,
            }),
        )
        .unwrap();
        assert_eq!(sim.store.snapshot().rev, rev + 1);
        let fields = &sim.events[0].data["fields"];
        assert_eq!(fields["state"], "working");
        assert_eq!(fields["since"], serde_json::json!(Ts(5)));
        assert_eq!(fields["title"], "Compiling openmicro");
        assert_eq!(fields["title_source"], "prompt");
        assert_eq!(fields["name"], "renamed");
        assert_eq!(fields["meta"]["k"], 1);
        assert_eq!(
            sim.puts(),
            vec![Key::AgentState(Slot(1)), Key::AgentTitle(Slot(1))]
        );
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.since, Ts(5));
        assert_eq!(a.title_source, TitleSource::Prompt);
        // A title only: the state line is untouched.
        sim.api(
            UID,
            SessionId(1),
            Request::AgentUpdate(AgentUpdate {
                agent: id.clone(),
                state: None,
                title: Some("Testing".into()),
                title_source: None,
                since: None,
                name: None,
                meta: None, task: None, subagents: None,
            }),
        )
        .unwrap();
        assert_eq!(sim.puts(), vec![Key::AgentTitle(Slot(1))]);
        assert_eq!(
            sim.store.snapshot().agents[&id].title_source,
            TitleSource::Api
        );
    }

    #[test]
    fn pins_win_over_memory_and_move_an_unpinned_occupant() {
        let mut sim = Sim::with(|cfg| {
            cfg.pins.insert(
                "other:pinned".into(),
                PinConfig {
                    slot: Some(Slot(1)),
                    voice: Some("Samantha".into()),
                    name: Some("Pinned".into()),
                },
            );
            cfg.pins.insert(
                "/repo".into(),
                PinConfig {
                    slot: Some(Slot(3)),
                    voice: None,
                    name: None,
                },
            );
        });
        let first = sim.register(UID, SessionId(1), "first", Some("k:first"));
        assert_eq!(sim.slot_of(&first), Some(Slot(1)));
        let pinned = sim.register(UID, SessionId(1), "pinned", Some("k:pinned"));
        assert_eq!(sim.slot_of(&pinned), Some(Slot(1)));
        assert_eq!(sim.slot_of(&first), Some(Slot(2)), "moved, not dropped");
        let p = &sim.store.snapshot().agents[&pinned];
        assert!(p.pinned);
        assert_eq!(p.name, "Pinned");
        assert_eq!(p.voice.as_deref(), Some("Samantha"));
        let mut r = register("repo", Some("k:repo"));
        r.cwd = Some("/repo".into());
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["slot"], 3, "cwd pin");
        // A pinned occupant is never displaced by another pin.
        let mut r = register("pinned", Some("k:pinned2"));
        r.kind = crate::Kind::Other;
        let res = sim
            .api(UID, SessionId(2), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["slot"], 4);
    }

    #[test]
    fn slot_memory_by_key_then_by_kind_and_cwd_keeps_the_channel_number() {
        let mut sim = Sim::with(|cfg| {
            cfg.persisted.slots.insert(
                "claude:old".into(),
                crate::SlotMemory {
                    slot: Slot(4),
                    last_seen: Ts(1),
                    kind: crate::Kind::Claude,
                    cwd: Some("/Users/tony/projects/openmicro".into()),
                },
            );
        });
        let mut r = register("openmicro", Some("claude:new"));
        r.kind = crate::Kind::Claude;
        r.cwd = Some("/Users/tony/projects/openmicro".into());
        let res = sim
            .api(claude_adapter(), SessionId(1), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["slot"], 4, "re-opened repo keeps its channel");
        let res = sim
            .api(
                claude_adapter(),
                SessionId(1),
                Request::AgentRegister(register("old", Some("claude:old"))),
            )
            .unwrap();
        assert_eq!(res["slot"], 1, "remembered slot taken → lowest free");
        let mut r = register("hinted", None);
        r.slot_hint = Some(Slot(7));
        let res = sim
            .api(UID, SessionId(2), Request::AgentRegister(r))
            .unwrap();
        assert_eq!(res["slot"], 7);
    }

    /// The card clause of `eviction_candidate`, which the neighbouring test
    /// does **not** reach: it gives its card-holder a `Q`, and a question card
    /// flips the agent to `Question`, so the `state == Idle` arm answers first
    /// and the `on_device()` check never runs. Delete that check and the whole
    /// suite still passes.
    ///
    /// A **`ready`** card is the case that matters: it leaves the agent `Idle`,
    /// so without the clause the longest-idle-wins rule would unslot an agent
    /// whose card is on the deck in front of the operator — §12's "never one
    /// with a card".
    #[test]
    fn an_idle_agent_whose_ready_card_is_on_the_deck_is_never_the_one_evicted() {
        let mut sim = Sim::new();
        let ids: Vec<AgentId> = (0..8)
            .map(|i| {
                sim.advance(Duration::from_secs(1));
                sim.register(UID, SessionId(1), &format!("a{i}"), Some(&format!("k:{i}")))
            })
            .collect();
        // a0 is the longest idle and holds a *ready* card…
        sim.set_card(&ids[0], CardKind::R, Deliver::Poll, vec!["x"]);
        assert_eq!(
            sim.store.snapshot().agents[&ids[0]].state,
            AgentState::Idle,
            "a ready card must not flip the state, or this test proves nothing"
        );
        assert!(
            sim.store.snapshot().agents[&ids[0]]
                .card
                .as_ref()
                .is_some_and(|c| c.status.on_device()),
            "and the card must be on the device"
        );

        // …so the ninth agent evicts the next-longest-idle instead.
        let ninth = sim.register(UID, SessionId(1), "a9", Some("k:9"));
        assert!(sim.slot_of(&ninth).is_some(), "the ninth got a slot");
        assert!(
            sim.slot_of(&ids[0]).is_some(),
            "the agent with a card on the deck kept its slot"
        );
        assert_eq!(
            sim.slot_of(&ids[1]),
            None,
            "the next-longest-idle went instead"
        );
    }

    #[test]
    fn the_decks_capacity_bounds_its_slots_both_ways() {
        let mut sim = Sim::new();
        assert_eq!(sim.store.snapshot().device.capacity(), Slot::DEFAULT, "an old deck holds eight");
        let ids: Vec<AgentId> = (0..20)
            .map(|i| sim.register(UID, SessionId(1), &format!("s{i}"), Some(&format!("claude:{i}"))))
            .collect();
        assert_eq!(ids.iter().filter(|c| sim.slot_of(c).is_some()).count(), 8);
        // A deck that holds 255 seats every waiting session.
        sim.store.set_capacity(Slot::MAX);
        assert!(ids.iter().all(|c| sim.slot_of(c).is_some()));
        // A smaller deck unseats the sessions above its last slot.
        sim.store.set_capacity(4);
        assert_eq!(ids.iter().filter(|c| sim.slot_of(c).is_some()).count(), 4);
        assert!(ids.iter().filter_map(|c| sim.slot_of(c)).all(|s| s.0 <= 4));
    }

    #[test]
    fn terminal_sessions_and_questions_outrank_a_companions_chats_for_the_eight_slots() {
        let mut sim = Sim::new();
        // A companion fills the deck: its launcher and seven ready chats.
        let mut chats = Vec::new();
        for i in 0..8 {
            sim.advance(Duration::from_secs(1));
            let mut r = register(&format!("Chat {i}"), Some(&format!("conol:{i}")));
            r.kind = crate::Kind::Conol;
            let result = sim.api(UID, SessionId(1), Request::AgentRegister(r)).unwrap();
            let id: AgentId = serde_json::from_value(result["agent"].clone()).unwrap();
            let profile = serde_json::from_value(serde_json::json!({"method":"agent.companion.set", "params": {
                "agent": id, "profile": {"app_id": "conol", "group_id": "me", "group_label": "Conol",
                    "role": if i == 0 { "launcher" } else { "session" }}}})).unwrap();
            sim.api(UID, SessionId(1), profile).unwrap();
            sim.api(UID, SessionId(1), update(&id, AgentState::Ready)).unwrap();
            chats.push(id);
        }
        assert!(chats.iter().all(|c| sim.slot_of(c).is_some()));
        // Terminal sessions arrive, idle: each takes a ready chat's slot,
        // never the launcher's.
        let cli: Vec<AgentId> = (0..3)
            .map(|i| sim.register(UID, SessionId(2), &format!("claude {i}"), Some(&format!("claude:{i}"))))
            .collect();
        assert!(cli.iter().all(|c| sim.slot_of(c).is_some()), "the terminal sessions are on the deck");
        assert!(sim.slot_of(&chats[0]).is_some(), "the launcher stays");
        assert_eq!(chats.iter().filter(|c| sim.slot_of(c).is_none()).count(), 3, "three chats made room");
        // Five more terminal sessions take the rest; the ninth waits.
        let more: Vec<AgentId> = (3..9)
            .map(|i| sim.register(UID, SessionId(2), &format!("claude {i}"), Some(&format!("claude:{i}"))))
            .collect();
        let off = more.iter().chain(&cli).filter(|c| sim.slot_of(c).is_none()).count();
        assert_eq!(off, 1, "eight slots, nine terminal sessions");
        assert!(sim.slot_of(&chats[0]).is_none(), "and the launcher yields to them too");
        // The waiting one asks a question: it takes an idle session's slot.
        let waiting = more.iter().chain(&cli).find(|c| sim.slot_of(c).is_none()).unwrap().clone();
        sim.api(UID, SessionId(2), update(&waiting, AgentState::Question)).unwrap();
        assert!(sim.slot_of(&waiting).is_some(), "a question is always on the deck");
        // Busy sessions are not shuffled among themselves.
        for c in more.iter().chain(&cli) {
            if sim.slot_of(c).is_some() {
                sim.api(UID, SessionId(2), update(c, AgentState::Working)).unwrap();
            }
        }
        let before: Vec<_> = more.iter().chain(&cli).map(|c| sim.slot_of(c)).collect();
        let late = sim.register(UID, SessionId(2), "claude late", Some("claude:late"));
        sim.api(UID, SessionId(2), update(&late, AgentState::Working)).unwrap();
        assert_eq!(sim.slot_of(&late), None, "a working newcomer does not bump a working equal");
        assert_eq!(more.iter().chain(&cli).map(|c| sim.slot_of(c)).collect::<Vec<_>>(), before);
    }

    #[test]
    fn a_full_deck_evicts_the_longest_idle_agent_never_one_with_a_card() {
        let mut sim = Sim::new();
        let ids: Vec<AgentId> = (0..8)
            .map(|i| {
                sim.advance(Duration::from_secs(1));
                sim.register(UID, SessionId(1), &format!("a{i}"), Some(&format!("k:{i}")))
            })
            .collect();
        // a0 is the longest idle but holds a card; a1 works; a2 is next.
        sim.set_card(&ids[0], CardKind::Q, Deliver::Poll, vec!["x"]);
        sim.api(UID, SessionId(1), update(&ids[1], AgentState::Working))
            .unwrap();
        let ninth = sim.register(UID, SessionId(1), "a9", Some("k:9"));
        assert_eq!(sim.slot_of(&ninth), Some(Slot(3)));
        assert_eq!(sim.slot_of(&ids[2]), None, "not on deck");
        assert!(sim.store.snapshot().agents.contains_key(&ids[2]));
        assert!(sim.events.iter().any(|e| e.kind == EventKind::AgentSlot
            && e.data["agent"] == ids[2].0
            && e.data["slot"].is_null()));
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::AgentGone(Slot(3)),
            HostLine::AgentGone { slot: 3 }
        ))));
        // Everyone busy: the tenth stays off the deck …
        for id in ids.iter().chain([&ninth]) {
            if sim.slot_of(id).is_some() {
                sim.api(UID, SessionId(1), update(id, AgentState::Working))
                    .unwrap();
            }
        }
        let tenth = sim.register(UID, SessionId(1), "a10", Some("k:10"));
        assert_eq!(sim.slot_of(&tenth), None);
        // … until a slot frees: the most recently active unslotted agent takes it.
        sim.api(
            UID,
            SessionId(1),
            Request::AgentRelease {
                agent: ids[7].clone(),
                reason: None,
            },
        )
        .unwrap();
        assert_eq!(sim.slot_of(&tenth), Some(Slot(8)));
        assert_eq!(sim.slot_of(&ids[2]), None);
    }

    #[test]
    fn an_unslotted_agent_that_wakes_up_takes_a_slot_from_an_idle_one() {
        let mut sim = Sim::new();
        let ids: Vec<AgentId> = (0..8)
            .map(|i| sim.register(UID, SessionId(1), &format!("a{i}"), None))
            .collect();
        let extra = sim.register(UID, SessionId(1), "extra", None);
        assert_eq!(sim.slot_of(&extra), Some(Slot(1)), "evicts the oldest idle");
        assert_eq!(sim.slot_of(&ids[0]), None);
        sim.api(UID, SessionId(1), update(&ids[0], AgentState::Question))
            .unwrap();
        assert_eq!(sim.slot_of(&ids[0]), Some(Slot(2)));
        assert_eq!(sim.slot_of(&ids[1]), None);
    }

    #[test]
    fn release_frees_the_slot_and_the_device_forgets_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.api(
            UID,
            SessionId(1),
            Request::AgentRelease {
                agent: id.clone(),
                reason: Some("done".into()),
            },
        )
        .unwrap();
        assert_eq!(
            sim.kinds(),
            vec![EventKind::AgentSlot, EventKind::AgentRemoved]
        );
        assert_eq!(sim.events[1].data["reason"], "done");
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::AgentGone(Slot(1)),
            HostLine::AgentGone { slot: 1 }
        ))));
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::CardId {
            slot: Slot(1),
            last: crate::CardId(0)
        })));
        assert!(sim.store.snapshot().agents.is_empty());
        let next = sim.register(UID, SessionId(1), "next", None);
        let cid = sim.set_card(&next, CardKind::Q, Deliver::Poll, vec!["a"]);
        assert_eq!(cid, crate::CardId(1), "counter reset with the slot");
    }

    #[test]
    fn focus_methods_push_focus_without_ack_and_mark_the_source() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.api(UID, SessionId(2), Request::OpFocus { agent: id.clone() })
            .unwrap();
        assert_eq!(sim.store.snapshot().focus.source, Some(FocusSource::Api));
        assert!(sim
            .effects
            .contains(&Effect::Desktop(DesktopCmd::Focus { agent: id.clone() })));
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::Focus,
            HostLine::Focus { slot: 1, ack: None }
        ))));
        assert_eq!(sim.events[0].kind, EventKind::FocusChanged);
        assert!(sim
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Focus, _))));
        // agent.focus needs ownership.
        let err = sim
            .api(UID, SessionId(2), Request::AgentFocus { agent: id.clone() })
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        assert!(sim
            .api(UID, SessionId(1), Request::AgentFocus { agent: id.clone() })
            .is_ok());
        // A device channel afterwards takes focus out of the barrier.
        sim.apply(Command::Device(crate::DeviceLine::Channel {
            slot: 1,
            seq: 3,
        }));
        assert!(!sim
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Focus, _))));
        // focus.report from a tracker.
        sim.api(
            UID,
            SessionId(9),
            Request::FocusReport(FocusReport {
                agent: Some(id.clone()),
                source: FocusSource::Desktop,
            }),
        )
        .unwrap();
        assert_eq!(
            sim.store.snapshot().focus.source,
            Some(FocusSource::Desktop)
        );
        assert!(sim.puts().contains(&Key::Focus));
        let rev = sim.store.snapshot().rev;
        sim.api(
            UID,
            SessionId(9),
            Request::FocusReport(FocusReport {
                agent: Some(id.clone()),
                source: FocusSource::Desktop,
            }),
        )
        .unwrap();
        assert_eq!(sim.store.snapshot().rev, rev, "same report, no change");
        sim.api(
            UID,
            SessionId(9),
            Request::FocusReport(FocusReport {
                agent: None,
                source: FocusSource::Desktop,
            }),
        )
        .unwrap();
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Remove(Key::Focus))));
        let err = sim
            .api(
                UID,
                SessionId(9),
                Request::FocusReport(FocusReport {
                    agent: None,
                    source: FocusSource::Device,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
    }

    #[test]
    fn replies_reach_the_device_and_optionally_the_speaker() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.api(
            UID,
            SessionId(1),
            Request::Reply(ReplySet {
                message: None,
                agent: id.clone(),
                text: "Tests pass. Deploy?".into(),
                speak: true,
                announce: Some("tests pass".into()),
                lane: None,
            }),
        )
        .unwrap();
        assert_eq!(sim.events[0].kind, EventKind::ReplySet);
        assert_eq!(sim.events[0].data["text"], "Tests pass. Deploy?");
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::Reply(Slot(1)),
            HostLine::Reply {
                slot: 1,
                text: "Tests pass. Deploy?".into(),
                rev: 1,
            }
        ))));
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Audio(AudioCmd::Say { utterance, lane, text, .. })
                if utterance == "ut_1" && lane == "chatter" && text == "Tests pass. Deploy?"
        )));
        // state:ready with announce on → the announcement in the agent's voice.
        sim.api(UID, SessionId(1), update(&id, AgentState::Ready))
            .unwrap();
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Audio(AudioCmd::Say { lane, text, .. })
                if lane == "ready" && text == "bot: tests pass"
        )));
        sim.api(UID, SessionId(1), update(&id, AgentState::Error))
            .unwrap();
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Audio(AudioCmd::Say { text, .. }) if text == "bot failed"
        )));
        let err = sim
            .api(
                UID,
                SessionId(1),
                Request::Reply(ReplySet {
                    message: None,
                    agent: id.clone(),
                    text: " ".into(),
                    speak: false,
                    announce: None,
                    lane: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        // §7.6's sentence rule is one implementation now
        // (`sanitize::first_sentence`), so the announcement and the spoken
        // report cannot drift apart.
        assert_eq!(
            crate::sanitize::first_sentence("Done here! Next?"),
            "Done here!"
        );
        assert_eq!(
            crate::sanitize::first_sentence("no punctuation"),
            "no punctuation"
        );
    }

    /// §7.7's voice assignment across a restart, which the integration row
    /// cannot reach: `state.json.voices` is what an agent is given back.
    ///
    /// An agent whose voice changed every time the daemon restarted would
    /// be worse than one voice for everybody — the operator learns "that
    /// one is Daniel" and the learning is what the feature is for.
    #[test]
    fn a_remembered_voice_is_given_back_and_the_rotation_carries_on() {
        let remembered = |key: &str, voice: &str| {
            let mut voices = std::collections::BTreeMap::new();
            voices.insert(key.to_string(), voice.to_string());
            voices
        };
        let mut sim = Sim::with(|cfg| {
            cfg.voice_pool = ["A", "B", "C"].map(String::from).to_vec();
            cfg.persisted.voices = remembered("other:bot", "C");
        });
        // The key this agent had last time gets its voice back, whatever
        // the rotation would have said…
        let id = sim.register(UID, SessionId(1), "bot", Some("other:bot"));
        assert_eq!(sim.store.snapshot().agents[&id].voice.as_deref(), Some("C"));
        assert!(
            !sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Persist(StatePatch::Voice { .. }))),
            "nothing to write: it was already on file"
        );
        // …and the rotation starts *past* what was on file, so the next
        // agent is not handed the first voice all over again.
        let next = sim.register(UID, SessionId(2), "two", Some("other:two"));
        assert_eq!(
            sim.store.snapshot().agents[&next].voice.as_deref(),
            Some("B"),
            "one voice was already remembered, so the cursor starts at 1"
        );
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::Voice {
            key: "other:two".into(),
            voice: "B".into(),
        })));
        // The third takes the one voice nobody holds, rather than the one
        // the cursor happens to point at.
        let third = sim.register(UID, SessionId(3), "three", Some("other:three"));
        assert_eq!(
            sim.store.snapshot().agents[&third].voice.as_deref(),
            Some("A"),
            "the pool is spent telling agents apart, not counting"
        );

        // A fourth agent against a pool of three has to share with
        // somebody, and the cursor decides who rather than the map's
        // iteration order. It is also keyed by "<kind>:<name>", because it
        // registered with no session key — the same spelling an `[agents]`
        // pin uses, so the two can never disagree about who is who.
        let keyless = sim.register(UID, SessionId(4), "four", None);
        assert_eq!(
            sim.store.snapshot().agents[&keyless].voice.as_deref(),
            Some("A")
        );
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::Voice {
            key: "other:four".into(),
            voice: "A".into(),
        })));

        // An empty pool assigns nothing and persists nothing: the engine's
        // own default, with no stale row to undo when a pool appears.
        let mut bare = Sim::with(|cfg| cfg.voice_pool = Vec::new());
        let id = bare.register(UID, SessionId(1), "bot", Some("other:bot"));
        assert_eq!(bare.store.snapshot().agents[&id].voice, None);
        assert!(!bare
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Persist(StatePatch::Voice { .. }))));
    }

    #[test]
    fn link_drop_and_agent_removal_abort_nothing_when_idle() {
        let mut sim = Sim::new();
        sim.apply(Command::Link(LinkEvent::Down));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Audio(_))));
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.store.remove_agent(&id, "test");
        sim.store.remove_agent(&id, "twice");
        assert!(sim.store.snapshot().agents.is_empty());
        assert_eq!(project_label("/a/b/"), Some("b".into()));
        assert_eq!(project_label("/"), None);
    }
}

#[cfg(test)]
mod companion_tests {
    use super::*;
    use crate::testkit::{Sim, UID, register};
    use crate::Request;

    #[test]
    fn native_conol_groups_without_a_cwd_and_focus_never_injects_a_terminal() {
        let mut sim = Sim::new();
        let mut registration = register("Chat", Some("conol:account:chat"));
        registration.kind = crate::Kind::Conol;
        registration.cwd = None;
        let result = sim.api(UID, SessionId(1), crate::Request::AgentRegister(registration)).unwrap();
        let id: AgentId = serde_json::from_value(result["agent"].clone()).unwrap();
        assert_eq!(sim.store.snapshot().agents[&id].project().as_deref(), Some("Conol"));
        sim.api(UID, SessionId(1), crate::Request::AgentFocus { agent: id.clone() }).unwrap();
        assert!(sim.events.iter().any(|e| e.kind == EventKind::FocusChanged));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
        sim.apply(crate::Command::Device(crate::DeviceLine::Channel { slot: 1, seq: 1 }));
        assert!(sim.events.iter().any(|e| e.kind == EventKind::FocusChanged));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
    }
    #[test]
    fn opening_an_overflow_companion_chat_preserves_launcher_other_apps_and_accounts() {
        let mut sim = Sim::new();
        let mut ids = Vec::new();
        for i in 0..9 {
            let mut r = register(&format!("Chat {i}"), Some(&format!("conol:{i}")));
            r.kind = crate::Kind::Other;
            let result = sim.api(UID, SessionId(1), Request::AgentRegister(r)).unwrap();
            let id: AgentId = serde_json::from_value(result["agent"].clone()).unwrap();
            let configure = serde_json::from_value(serde_json::json!({"method":"agent.companion.set", "params": {
                "agent": id, "profile": {"app_id": if i == 2 { "other-app" } else { "example" },
                    "group_id": if i == 1 { "other-account" } else { "mine" }, "group_label": "Example",
                    "role": if i == 0 { "launcher" } else { "session" }}
            }})).unwrap();
            sim.api(UID, SessionId(1), configure).unwrap();
            let update = serde_json::from_value(serde_json::json!({"method":"agent.update", "params":{"agent":id,"state":"working"}})).unwrap();
            sim.api(UID, SessionId(1), update).unwrap();
            ids.push(id);
        }
        assert!(sim.slot_of(&ids[8]).is_none());
        let launcher = sim.slot_of(&ids[0]);
        let other = sim.slot_of(&ids[1]);
        let other_app = sim.slot_of(&ids[2]);
        sim.api(UID, SessionId(1), Request::AgentFocus { agent: ids[8].clone() }).unwrap();
        assert!(sim.slot_of(&ids[8]).is_some());
        assert_eq!(sim.slot_of(&ids[0]), launcher);
        assert_eq!(sim.slot_of(&ids[1]), other);
        assert_eq!(sim.slot_of(&ids[2]), other_app);
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
    }

    #[test]
    fn companion_focus_cannot_displace_another_owners_busy_agents() {
        let mut sim = Sim::new();
        for i in 0..8 {
            let id = sim.register(UID, SessionId(2), &format!("Other {i}"), None);
            let update = serde_json::from_value(serde_json::json!({"method":"agent.update", "params":{"agent":id,"state":"working"}})).unwrap();
            sim.api(UID, SessionId(2), update).unwrap();
        }
        let mut r = register("Example chat", None);
        r.kind = crate::Kind::Other;
        let result = sim.api(UID, SessionId(1), Request::AgentRegister(r)).unwrap();
        let id: AgentId = serde_json::from_value(result["agent"].clone()).unwrap();
        sim.api(UID, SessionId(1), Request::CompanionSet(crate::companion::CompanionSet {
            agent: id.clone(), profile: Some(crate::companion::CompanionProfile {
                app_id: "example".into(), group_id: "personal".into(), group_label: "Example".into(),
                role: crate::companion::CompanionRole::Session,
            }),
        })).unwrap();
        sim.api(UID, SessionId(1), Request::AgentFocus { agent: id.clone() }).unwrap();
        assert!(sim.slot_of(&id).is_none());
        assert_eq!(sim.store.snapshot().agents.values().filter(|a| a.slot.is_some()).count(), 8);
    }

}

/// A muted session (`Command::Mute`, a source the owner paused) is kept and
/// served, but none of its agents holds a slot, a question parked for one
/// goes back to its tool, and nothing of it is heard.
#[cfg(test)]
mod mute_tests {
    use super::*;
    use crate::companion::{CompanionProfile, CompanionRole, CompanionSet};
    use crate::testkit::{card_set, claude_adapter, register, Sim, UID};
    use crate::{
        CardKind, CardSet, CardStatus, Command, Control, HoldId, Origin, OriginKind, PinConfig,
        Release, Request,
    };

    fn update(id: &AgentId, state: AgentState) -> Request {
        Request::AgentUpdate(AgentUpdate {
            agent: id.clone(),
            state: Some(state),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None,
            task: None,
            subagents: None,
        })
    }

    /// A reply its owner asks to have spoken.
    fn reply(id: &AgentId, text: &str) -> Request {
        Request::Reply(ReplySet {
            agent: id.clone(),
            text: text.into(),
            speak: true,
            announce: None,
            lane: None,
            message: None,
        })
    }

    /// A Claude hook agent of the adapter's own session 100: its questions
    /// are parked holds.
    fn hook_agent(sim: &mut Sim, name: &str) -> AgentId {
        let mut r = register(name, Some(&format!("claude:{name}")));
        r.kind = crate::Kind::Claude;
        r.caps.answer = AnswerCap::Hold;
        r.caps.order = OrderCap::Inject;
        let res = sim
            .api(claude_adapter(), SessionId(100), Request::AgentRegister(r))
            .unwrap();
        AgentId(res["agent"].as_str().unwrap().into())
    }

    fn permission(agent: &AgentId) -> CardSet {
        let mut c = card_set(agent, CardKind::Q, Deliver::Hold, &["Allow", "Deny"]);
        c.text = "Bash: psql".into();
        c.origin = Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: None,
            group: None,
        };
        c
    }

    fn audio(sim: &Sim) -> Vec<&Effect> {
        sim.effects
            .iter()
            .filter(|e| matches!(e, Effect::Audio(_)))
            .collect()
    }

    fn releases(sim: &Sim) -> Vec<&Effect> {
        sim.effects
            .iter()
            .filter(|e| matches!(e, Effect::ReleaseHold { .. }))
            .collect()
    }

    #[test]
    fn muting_a_session_takes_its_agents_off_the_deck_and_hands_their_slots_on() {
        let mut sim = Sim::new();
        // Seven busy agents of session 1 and one of session 2 fill the deck…
        let ones: Vec<AgentId> = (0..7)
            .map(|i| sim.register(UID, SessionId(1), &format!("a{i}"), Some(&format!("k:{i}"))))
            .collect();
        let paused = sim.register(UID, SessionId(2), "paused", Some("k:paused"));
        for id in &ones {
            sim.api(UID, SessionId(1), update(id, AgentState::Working)).unwrap();
        }
        sim.api(UID, SessionId(2), update(&paused, AgentState::Working)).unwrap();
        let slot = sim.slot_of(&paused).expect("on the deck");
        // …so a ninth, idle, waits for a slot.
        let waiting = sim.register(UID, SessionId(1), "waiting", Some("k:waiting"));
        assert_eq!(sim.slot_of(&waiting), None);

        sim.mute(&[SessionId(2)]);
        assert_eq!(sim.slot_of(&paused), None, "off the deck");
        assert!(sim.store.snapshot().agents.contains_key(&paused), "and still registered");
        assert_eq!(sim.slot_of(&waiting), Some(slot), "its slot went to the one waiting");
        assert!(sim.effects.contains(&Effect::Device(DeviceOp::Put(
            Key::AgentGone(slot),
            HostLine::AgentGone { slot: slot.0 }
        ))));
        let moves: Vec<(&str, &serde_json::Value)> = sim
            .events
            .iter()
            .filter(|e| e.kind == EventKind::AgentSlot)
            .map(|e| (e.data["agent"].as_str().unwrap_or_default(), &e.data["slot"]))
            .collect();
        assert_eq!(
            moves,
            vec![
                (paused.0.as_str(), &serde_json::Value::Null),
                (waiting.0.as_str(), &serde_json::json!(slot)),
            ]
        );

        // Its owner's calls keep working, and none of them brings it back.
        sim.api(UID, SessionId(2), update(&paused, AgentState::Question))
            .expect("a muted session is served, never refused");
        assert_eq!(sim.slot_of(&paused), None);
    }

    #[test]
    fn unmuting_gives_an_agent_back_the_slot_it_had() {
        let mut sim = Sim::new();
        let a = sim.register(UID, SessionId(1), "a", Some("k:a"));
        let b = sim.register(UID, SessionId(2), "b", Some("k:b"));
        let c = sim.register(UID, SessionId(1), "c", Some("k:c"));
        sim.mute(&[SessionId(2)]);
        assert_eq!(sim.slot_of(&b), None);
        assert_eq!(sim.store.snapshot().by_slot().len(), 2, "nobody was waiting for it");

        sim.mute(&[]);
        assert_eq!(sim.slot_of(&b), Some(Slot(2)), "the slot it had, from the slot memory");
        assert!(sim.events.iter().any(|e| e.kind == EventKind::AgentSlot
            && e.data["agent"] == b.0
            && e.data["slot"] == 2));
        assert_eq!(
            (sim.slot_of(&a), sim.slot_of(&c)),
            (Some(Slot(1)), Some(Slot(3))),
            "nobody else moved"
        );
    }

    /// The deck may fill up while a session is paused. Coming back, its
    /// agent is seated the way a newcomer is: here in the place of the
    /// longest-idle agent, the one that took its slot.
    #[test]
    fn an_agent_coming_back_to_a_full_deck_is_seated_as_a_newcomer_would_be() {
        let mut sim = Sim::new();
        let paused = sim.register(UID, SessionId(2), "paused", Some("k:paused"));
        sim.mute(&[SessionId(2)]);
        let ids: Vec<AgentId> = (0..8)
            .map(|i| {
                sim.advance(Duration::from_secs(1));
                sim.register(UID, SessionId(1), &format!("a{i}"), Some(&format!("k:{i}")))
            })
            .collect();
        assert_eq!(sim.slot_of(&ids[0]), Some(Slot(1)), "the first one took its slot");
        assert!(ids.iter().all(|id| sim.slot_of(id).is_some()), "a full deck");

        sim.mute(&[]);
        assert_eq!(sim.slot_of(&paused), Some(Slot(1)));
        assert_eq!(sim.slot_of(&ids[0]), None, "the longest idle made room");
        assert!(ids[1..].iter().all(|id| sim.slot_of(id).is_some()));
    }

    /// An `[agents]` pin is the first rule of the policy, for an agent coming
    /// back as for a newcomer: the pinned slot is taken back from whoever
    /// sat in it, and that one moves to a free slot.
    #[test]
    fn a_pinned_agent_coming_back_takes_its_slot_back_from_whoever_sat_in_it() {
        let mut sim = Sim::with(|cfg| {
            cfg.pins.insert(
                "other:pinned".into(),
                PinConfig {
                    slot: Some(Slot(3)),
                    voice: None,
                    name: None,
                },
            );
        });
        sim.register(UID, SessionId(1), "a", Some("k:a"));
        let pinned = sim.register(UID, SessionId(2), "pinned", Some("k:pinned"));
        assert_eq!(sim.slot_of(&pinned), Some(Slot(3)));
        sim.mute(&[SessionId(2)]);
        sim.register(UID, SessionId(1), "b", Some("k:b"));
        let sitter = sim.register(UID, SessionId(1), "c", Some("k:c"));
        assert_eq!(sim.slot_of(&sitter), Some(Slot(3)), "the pinned slot was free");

        sim.mute(&[]);
        assert_eq!(sim.slot_of(&pinned), Some(Slot(3)));
        assert_eq!(sim.slot_of(&sitter), Some(Slot(4)), "moved, not dropped");
    }

    #[test]
    fn a_question_parked_for_an_agent_goes_back_to_its_tool_when_it_is_muted() {
        let mut sim = Sim::new();
        let id = hook_agent(&mut sim, "web");
        let card = sim.hold(permission(&id), HoldId(1)).unwrap();
        sim.drain();
        assert_eq!(sim.slot_of(&id), Some(Slot(1)));

        sim.mute(&[SessionId(100)]);
        assert_eq!(
            releases(&sim),
            vec![&Effect::ReleaseHold {
                agent: id.clone(),
                card,
                reason: Release::Cleared,
            }],
            "the hook is answered, so the CLI asks in its own dialog"
        );
        let cleared = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::CardCleared)
            .expect("card.cleared");
        assert_eq!(cleared.data["reason"], "muted");
        assert_eq!(cleared.data["delivered"], false);
        assert!(
            sim.store.snapshot().agents[&id].card.is_none(),
            "no `⌨` copy is kept to outlive the question"
        );
        assert!(
            sim.effects.contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                card.0
            )))),
            "the question leaves the deck before its agent does"
        );
        assert_eq!(sim.slot_of(&id), None);
        // The hold budget has nothing left to release.
        sim.advance(Duration::from_secs(130));
        assert!(releases(&sim).is_empty());
    }

    #[test]
    fn a_muted_agents_replies_states_and_questions_make_no_sound() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(2), "bot", None);
        sim.mute(&[SessionId(2)]);
        sim.api(UID, SessionId(2), reply(&id, "Tests pass. Deploy?")).unwrap();
        assert_eq!(sim.kinds(), vec![EventKind::ReplySet], "the bus still carries it");
        assert!(audio(&sim).is_empty(), "{:?}", sim.effects);
        // Its announced states…
        for state in [AgentState::Ready, AgentState::Error] {
            sim.api(UID, SessionId(2), update(&id, state)).unwrap();
            assert!(audio(&sim).is_empty(), "{state:?}: {:?}", sim.effects);
        }
        // …and its question: no chime, no announcement, no microphone
        // warmed for an answer nobody can give on the deck.
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["Yes", "No"]);
        assert!(audio(&sim).is_empty(), "{:?}", sim.effects);
        assert!(!sim.kinds().contains(&EventKind::VoiceUtterance));

        // Back on the deck, its question warms the microphone and its
        // replies are heard again.
        sim.mute(&[]);
        assert!(sim
            .effects
            .contains(&Effect::Audio(AudioCmd::Warm { pending: true })));
        sim.api(UID, SessionId(2), reply(&id, "Deployed.")).unwrap();
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Audio(AudioCmd::Say { text, .. }) if text == "Deployed."
        )));
    }

    #[test]
    fn an_agent_of_another_session_is_untouched_and_the_same_set_again_is_no_change() {
        let mut sim = Sim::new();
        let kept = sim.register(UID, SessionId(1), "kept", Some("k:kept"));
        sim.set_card(&kept, CardKind::Q, Deliver::Poll, vec!["a", "b"]);
        let paused = sim.register(UID, SessionId(2), "paused", Some("k:paused"));
        let before = sim.store.snapshot().agents[&kept].clone();

        sim.mute(&[SessionId(2)]);
        assert_eq!(sim.slot_of(&paused), None);
        assert_eq!(sim.store.snapshot().agents[&kept], before, "not one field of it moved");
        assert!(
            sim.events.iter().all(|e| e.data["agent"] != kept.0),
            "{:?}",
            sim.kinds()
        );
        sim.api(UID, SessionId(1), reply(&kept, "Done.")).unwrap();
        assert!(
            sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Audio(AudioCmd::Say { .. }))),
            "and it is still heard"
        );

        let rev = sim.store.snapshot().rev;
        sim.mute(&[SessionId(2)]);
        assert!(sim.events.is_empty() && sim.effects.is_empty());
        assert_eq!(sim.store.snapshot().rev, rev);
    }

    #[test]
    fn a_question_arriving_for_a_muted_agent_does_not_slot_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(2), "bot", Some("k:bot"));
        sim.mute(&[SessionId(2)]);
        let card = sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["Yes", "No"]);
        assert_eq!(sim.slot_of(&id), None, "every slot is free, and still it is not seated");
        assert!(sim.store.snapshot().by_slot().is_empty());
        assert!(sim.puts().is_empty(), "nothing of it reaches the deck: {:?}", sim.puts());
        let agent = &sim.store.snapshot().agents[&id];
        assert_eq!(
            agent.card.as_ref().map(|c| (c.id, c.status.clone())),
            Some((card, CardStatus::Pending)),
            "a poll question is kept for its owner, who settles it"
        );
        assert_eq!(agent.state, AgentState::Question);
        assert!(releases(&sim).is_empty());
    }

    #[test]
    fn a_held_question_arriving_for_a_muted_agent_goes_back_to_its_tool_on_the_next_tick() {
        let mut sim = Sim::new();
        let id = hook_agent(&mut sim, "web");
        sim.mute(&[SessionId(100)]);
        let card = sim.hold(permission(&id), HoldId(1)).unwrap();
        sim.drain();
        assert_eq!(sim.slot_of(&id), None);
        // Not in the step that parks it: the core parks a hold only after
        // it has run that step's effects, so this release would find
        // nothing parked.
        assert!(releases(&sim).is_empty(), "{:?}", sim.effects);
        assert!(
            sim.effects.contains(&Effect::Timer {
                key: TimerKey::HoldExpiry(card),
                at: sim.t,
            }),
            "due at once: {:?}",
            sim.effects
        );
        assert!(audio(&sim).is_empty(), "{:?}", sim.effects);

        sim.advance(Duration::ZERO);
        assert_eq!(
            releases(&sim),
            vec![&Effect::ReleaseHold {
                agent: id.clone(),
                card,
                reason: Release::Cleared,
            }]
        );
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        assert_eq!(sim.slot_of(&id), None);
    }

    #[test]
    fn a_held_question_still_parked_when_its_agent_comes_back_is_handed_back_before_it_is_seated() {
        let mut sim = Sim::new();
        let id = hook_agent(&mut sim, "web");
        sim.mute(&[SessionId(100)]);
        let card = sim.hold(permission(&id), HoldId(1)).unwrap();
        sim.drain();
        // Unmuted before the core's next tick.
        sim.mute(&[]);
        let released = sim
            .effects
            .iter()
            .position(|e| {
                *e == Effect::ReleaseHold {
                    agent: id.clone(),
                    card,
                    reason: Release::Cleared,
                }
            })
            .expect("handed back");
        let seated = sim
            .effects
            .iter()
            .position(|e| matches!(e, Effect::Device(DeviceOp::Put(Key::AgentState(_), _))))
            .expect("seated");
        assert!(
            released < seated,
            "released under the id it was parked with, before a slot could renumber it"
        );
        assert_eq!(sim.slot_of(&id), Some(Slot(1)));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        sim.advance(Duration::ZERO);
        assert!(releases(&sim).is_empty(), "nothing is left for the tick");
    }

    /// One set can mute a session and unmute another. The slots the first
    /// frees are filled before the second's agents are seated by the policy,
    /// so a question still parked for one of those must be handed back
    /// before the fill, not after it.
    #[test]
    fn a_swap_of_muted_sessions_hands_a_parked_question_back_before_the_fill_seats_its_agent() {
        let mut sim = Sim::new();
        let hook = hook_agent(&mut sim, "web");
        sim.mute(&[SessionId(100)]);
        let card = sim.hold(permission(&hook), HoldId(1)).unwrap();
        sim.drain();
        let other = sim.register(UID, SessionId(2), "other", Some("k:other"));
        assert_eq!(sim.slot_of(&other), Some(Slot(1)), "it took the hook agent's slot");

        sim.mute(&[SessionId(2)]);
        let released = sim
            .effects
            .iter()
            .position(|e| {
                *e == Effect::ReleaseHold {
                    agent: hook.clone(),
                    card,
                    reason: Release::Cleared,
                }
            })
            .expect("handed back");
        let seated = sim
            .effects
            .iter()
            .position(|e| matches!(e, Effect::Persist(StatePatch::Slot { key, .. }) if key == "claude:web"))
            .expect("seated");
        assert!(released < seated, "{:?}", sim.effects);
        assert_eq!(sim.slot_of(&hook), Some(Slot(1)));
        assert_eq!(sim.slot_of(&other), None);
    }

    #[test]
    fn rebalance_never_slots_a_muted_agent() {
        let mut sim = Sim::new();
        let idle: Vec<AgentId> = (0..8)
            .map(|i| {
                sim.advance(Duration::from_secs(1));
                sim.register(UID, SessionId(1), &format!("a{i}"), Some(&format!("k:{i}")))
            })
            .collect();
        sim.mute(&[SessionId(2)]);
        let asking = sim.register(UID, SessionId(2), "asking", Some("k:asking"));
        assert_eq!(sim.slot_of(&asking), None, "its registration does not seat it");
        // A question outranks every idle session on the deck…
        sim.api(UID, SessionId(2), update(&asking, AgentState::Question)).unwrap();
        sim.set_card(&asking, CardKind::Q, Deliver::Poll, vec!["Yes"]);
        assert_eq!(sim.slot_of(&asking), None, "…and still it takes nobody's slot");
        // …nor does any other ranking change seat it.
        sim.api(UID, SessionId(1), update(&idle[0], AgentState::Working)).unwrap();
        assert_eq!(sim.slot_of(&asking), None, "a peer going busy");
        let profile = CompanionProfile {
            app_id: "example".into(),
            group_id: "me".into(),
            group_label: "Example".into(),
            role: CompanionRole::Session,
        };
        sim.api(
            UID,
            SessionId(1),
            Request::CompanionSet(CompanionSet {
                agent: idle[1].clone(),
                profile: Some(profile),
            }),
        )
        .unwrap();
        assert_eq!(sim.slot_of(&asking), None, "a companion profile");
        assert!(idle.iter().all(|id| sim.slot_of(id).is_some()));

        // Unmuted, the same question takes the lowest-ranked slot at once.
        sim.mute(&[]);
        assert_eq!(sim.slot_of(&asking), Some(Slot(2)));
        assert_eq!(sim.slot_of(&idle[1]), None, "the companion chat yields");
    }

    #[test]
    fn a_claim_carries_an_agent_into_and_out_of_a_muted_session() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", Some("k:1"));
        sim.mute(&[SessionId(2)]);
        // Its connection drops; a paused app reconnects as session 2 and
        // claims it.
        sim.apply(Command::SessionClosed(SessionId(1)));
        assert_eq!(sim.slot_of(&id), Some(Slot(1)), "an orphan keeps its slot through the grace");
        let res = sim
            .api(UID, SessionId(2), Request::AgentClaim { key: "k:1".into() })
            .unwrap();
        assert_eq!(sim.slot_of(&id), None, "claimed into a muted session, it leaves the deck");
        assert_eq!(res["slot"], serde_json::Value::Null, "and the claim says so");

        // Claimed again from a session that is not muted, it comes back.
        sim.apply(Command::SessionClosed(SessionId(2)));
        let res = sim
            .api(UID, SessionId(3), Request::AgentClaim { key: "k:1".into() })
            .unwrap();
        assert_eq!(res["slot"], 1);
        assert_eq!(sim.slot_of(&id), Some(Slot(1)));
    }
}
