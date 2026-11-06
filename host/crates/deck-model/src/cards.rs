//! Cards (§4.6, §4.8, §5.6): set / same-id re-push / replace, the status
//! machine `pending → answering → delivered → cleared`, `failed`, `snoozed →
//! degraded`, `expired`, the delivery modes with the 4 s budget, holds
//! (`HoldWritten`/`HoldGone`/expiry), question groups and orders.
//!
//! Routing the reducer leaves to the core: `Effect::Deliver` carries an
//! answer (or a Ready step as `text`) to the owner by `deliver`; a snooze on
//! a push/poll card is the `card.snoozed` event the core turns into the
//! owner's `{kind:"snooze"}`; an `op.order` by push/poll is the
//! `agent.order` event, which the fan-out hands to the owning session only.
use crate::api::{ApiError, ApiResult};
use crate::store::DELIVERY_BUDGET;
use crate::{
    Agent, AgentId, AgentState, Answer, AnswerSource, AudioCmd, Card, CardClear, CardId, CardKind,
    CardSet, CardStatus, Choice, Control, Deliver, DesktopCmd, DeviceOp, DocKind, Effect,
    EventKind, HoldId, Key, OpOrder, OrderCap, OriginKind, Page, Principal, Release, SessionId,
    Slot, Store, TimerKey,
};
use std::time::Duration;

/// Who answered what (a device `answer`, `op.answer`, later a PTT transcript).
#[derive(Clone, Debug)]
pub struct AnswerInput {
    pub choice: Option<u8>,
    pub text: Option<String>,
    pub source: AnswerSource,
    pub principal: Principal,
    pub session: Option<SessionId>,
    /// The PTT session this answer was dictated from (§7.4), `None` for a
    /// typed or chosen one.
    ///
    /// The seam HOST-CONTRACT-W4 §8 left open. B12 threaded `utterance`
    /// through `OpOrder`, which covers everything that leaves as an order;
    /// a spoken answer to a **question** goes through [`Store::answer`]
    /// instead, and had no field to carry it. That is the
    /// highest-consequence thing this daemon does — a permission card
    /// answered by voice — and the owner was handed `{text: "allow"}` with
    /// no way back to the recording: not the `transcript.final`, not the
    /// partials the operator watched, not the `voice.state` naming the
    /// session. Matching on the text does not work, because §5.6's grammar
    /// rewrites it and two presses a minute apart produce the same string.
    pub utterance: Option<String>,
}

/// The device shows at most four choices (v1 truncates, §5.6).
pub const MAX_CHOICES: usize = 4;

/// §7.7's budget for the spoken form of a question card's text, in
/// characters. Long enough to carry the question, short enough that the
/// operator is not listening to a paragraph while holding a button.
pub(crate) const ANNOUNCE_CARD_CHARS: usize = 80;

/// The ≤`MAX_CHOICES` the deck shows, the recommended index among them,
/// and the shown→original map (§5.6, B10).
///
/// The recommended option is always shown, so one past the cut is swapped
/// into slot 0 — which makes the mapping non-obvious, and the answer
/// carries the index of the **shown** list. Without the map an adapter
/// with more than four options has only `Answer.label` to match on, and
/// two long options can cut to the same label. The map is empty when
/// nothing was cut: an adapter reads `card.choice_map.get(n)` and falls
/// back to `n` itself.
pub(crate) fn shown_choices(
    mut choices: Vec<Choice>,
    mut rec: Option<u8>,
) -> (Vec<Choice>, Option<u8>, Vec<u8>) {
    if choices.len() <= MAX_CHOICES {
        return (choices, rec, Vec::new());
    }
    let mut map: Vec<u8> = (0..MAX_CHOICES)
        .map(|i| u8::try_from(i).unwrap_or(u8::MAX))
        .collect();
    if let Some(r) = rec.filter(|r| usize::from(*r) >= MAX_CHOICES) {
        choices.swap(0, usize::from(r));
        map[0] = r;
        rec = Some(0);
    }
    choices.truncate(MAX_CHOICES);
    (choices, rec, map)
}

const fn deliver_name(d: Deliver) -> &'static str {
    match d {
        Deliver::Hold => "hold",
        Deliver::Push => "push",
        Deliver::Poll => "poll",
        Deliver::Focus => "focus",
        Deliver::None => "none",
    }
}

impl Store {
    pub(crate) fn card_json(agent: &Agent) -> serde_json::Value {
        serde_json::to_value(&agent.card).unwrap_or(serde_json::Value::Null)
    }

    fn current_card(&self, id: &AgentId) -> Option<&Card> {
        self.snapshot.agents.get(id).and_then(|a| a.card.as_ref())
    }

    fn current_card_mut(&mut self, id: &AgentId) -> Option<&mut Card> {
        self.snapshot
            .agents
            .get_mut(id)
            .and_then(|a| a.card.as_mut())
    }

    /// The agent's current card if it is `card_id`, else the stale-card error.
    fn matching_card(&self, id: &AgentId, card_id: CardId) -> Result<&Card, ApiError> {
        self.exists(id)?;
        match self.current_card(id) {
            Some(card) if card.id == card_id => Ok(card),
            other => Err(ApiError::stale_card(other.map(|c| c.id))),
        }
    }

    /// A held hook (`Command::SetCardHeld`): the adapter's card,
    /// `deliver:hold`.
    ///
    /// This is the hook door's authorization seam (B3). Until week 3 it
    /// checked only that the agent existed and then borrowed the *agent's*
    /// owner principal for the adapter-only test, so any in-process holder
    /// of a `CoreHandle` — the Codex adapter, say — could park a hold on
    /// another adapter's agent. Like every other write it now goes through
    /// `owned()`: an adapter may hold only its own agents' cards, and the
    /// caller's own principal decides the adapter-only fields.
    pub(crate) fn set_card_held(
        &mut self,
        principal: &Principal,
        session: SessionId,
        cmd: CardSet,
        _hold: HoldId,
    ) -> Result<CardId, ApiError> {
        self.owned(&cmd.agent, session)?;
        self.card_set(principal, cmd, true)
    }

    /// `agent.card.set` (ownership already checked by the caller).
    pub(crate) fn card_set(
        &mut self,
        principal: &Principal,
        c: CardSet,
        held: bool,
    ) -> Result<CardId, ApiError> {
        let id = c.agent.clone();
        self.exists(&id)?;
        let adapter = matches!(principal, Principal::Adapter(_));
        let deliver = if held { Deliver::Hold } else { c.deliver };
        if deliver == Deliver::Hold && !adapter {
            return Err(ApiError::invalid("deliver", "adapter_only"));
        }
        if matches!(
            c.origin.kind,
            OriginKind::Permission | OriginKind::Question | OriginKind::Elicitation
        ) && !adapter
        {
            return Err(ApiError::invalid("origin.kind", "adapter_only"));
        }
        if c.text.trim().is_empty() {
            return Err(ApiError::invalid("text", "empty"));
        }
        if deliver == Deliver::None && !c.choices.is_empty() {
            return Err(ApiError::invalid(
                "choices",
                "informational cards take no choices",
            ));
        }
        if c.rec.is_some_and(|r| usize::from(r) >= c.choices.len()) {
            return Err(ApiError::invalid("rec", "out_of_range"));
        }
        if c.choices.iter().any(|ch| ch.label.trim().is_empty()) {
            return Err(ApiError::invalid("choices", "empty label"));
        }
        let total = c.choices.len();
        let (choices, rec, choice_map) = shown_choices(c.choices, c.rec);
        if let Some(cid) = c.id {
            return self.repush_card(
                &id,
                cid,
                c.text,
                choices,
                total,
                choice_map,
                rec,
                c.free_text,
            );
        }
        let inherited_hold = self.aux.get(&id).and_then(|a| a.hold_at);
        self.retire_current_card(&id, "replaced");
        let unslotted = self.snapshot.agents[&id].slot.is_none();
        if c.kind == CardKind::Q && unslotted {
            // A question must reach the deck.
            self.assign_slot(&id, None, None);
        }
        let slot = self.snapshot.agents[&id].slot;
        let new_id = match slot {
            Some(s) => self.next_card_id(s),
            None => {
                let aux = self.aux.entry(id.clone()).or_default();
                aux.offdeck_last = aux.offdeck_last.next();
                aux.offdeck_last
            }
        };
        let now = self.now;
        let ts = self.ts_now();
        let timeout = c
            .timeout_s
            .map(|t| Duration::from_secs(u64::from(t.min(crate::MAX_CLIENT_SECS))));
        let hold_s = Duration::from_secs(u64::from(self.cfg.hold_s));
        let ready_ttl = Duration::from_secs(u64::from(self.cfg.ready_ttl_s));
        let in_group = c.origin.group.is_some_and(|g| g.index > 0);
        let (hold_at, expire_at) = match (deliver, c.kind) {
            (Deliver::Hold, _) => {
                // One `hold_s` bounds a whole question group (§4.6).
                let at = inherited_hold
                    .filter(|_| in_group)
                    .unwrap_or(now + timeout.unwrap_or(hold_s));
                (Some(at), None)
            }
            (_, CardKind::R) => (None, Some(now + timeout.unwrap_or(ready_ttl))),
            (_, CardKind::Q) => (None, timeout.map(|t| now + t)),
        };
        let expires = hold_at
            .or(expire_at)
            .map(|at| ts.plus(at.saturating_duration_since(now)));
        let card = Card {
            id: new_id,
            kind: c.kind,
            status: CardStatus::Pending,
            deliver,
            origin: c.origin,
            text: Some(c.text),
            choices,
            choices_n: u8::try_from(total).unwrap_or(u8::MAX),
            choice_map,
            rec,
            free_text: c.free_text,
            page: Page { index: 0, count: 1 },
            created: ts,
            expires,
            redacted: false,
        };
        let mut state_changed = false;
        if let Some(agent) = self.snapshot.agents.get_mut(&id) {
            agent.card = Some(card);
            if c.kind == CardKind::Q && agent.state != AgentState::Question {
                let prev = agent.state;
                agent.state = AgentState::Question;
                agent.since = ts;
                state_changed = true;
                self.aux.entry(id.clone()).or_default().prev_state = Some(prev);
            }
        }
        {
            let aux = self.aux.entry(id.clone()).or_default();
            aux.hold_at = hold_at;
            aux.expire_at = expire_at;
            aux.deliver_at = None;
            aux.last_active = Some(now);
        }
        if let Some(at) = hold_at {
            self.timer(TimerKey::HoldExpiry(new_id), at);
        }
        if let Some(at) = expire_at {
            self.timer(TimerKey::ReadyTtl(new_id), at);
        }
        let card = Self::card_json(&self.snapshot.agents[&id]);
        let principal = self.principal_of(&id);
        self.emit(
            EventKind::CardSet,
            principal.clone(),
            serde_json::json!({"agent": id, "card": card}),
        );
        if total > MAX_CHOICES {
            self.emit(
                EventKind::CardTruncated,
                principal.clone(),
                serde_json::json!({"agent": id, "card": new_id, "shown": MAX_CHOICES, "total": total}),
            );
        }
        if state_changed {
            self.emit(
                EventKind::AgentChanged,
                principal,
                serde_json::json!({"agent": id, "fields": {"state": "question", "since": ts}}),
            );
            self.put_agent_state(&id);
        }
        self.put_card(&id);
        if c.kind == CardKind::Q {
            if self.snapshot.settings.chime == 1 {
                self.effects.push(Effect::Audio(AudioCmd::Chime {
                    which: "question".into(),
                }));
            }
            self.announce_question(&id);
        }
        Ok(new_id)
    }

    /// §7.7's `"<name> needs a decision: <card text ≤80 chars>"` on the
    /// `Question` lane.
    ///
    /// The chime already says *something* happened. This says **which
    /// agent** and **about what**, which is the difference between a beep
    /// the operator has to go and investigate and a question they can
    /// answer with the button already under their thumb.
    ///
    /// Two gates, deliberately independent (§7.7): `set announce` is the
    /// operator's "stop talking to me", and the face check is "I am already
    /// looking at it" — a card that arrives while the Agents face is on the
    /// deck is on screen in front of them, and reading it out is noise.
    /// `[voice] announce_questions` is the override for an operator who
    /// wants it anyway. `set chime` gates the chime **only** and is checked
    /// by the caller, so turning the beeps off does not also take the words
    /// away.
    ///
    /// An unknown face counts as "not the Agents face": before the device
    /// reports its strip there is no evidence the operator can see
    /// anything, and the failure that costs is a sentence they did not
    /// need — against a question they never heard about.
    fn announce_question(&mut self, id: &AgentId) {
        if self.snapshot.settings.announce != 1 {
            return;
        }
        if self.on_agents_face() && !self.cfg.announce_questions {
            return;
        }
        let Some(agent) = self.snapshot.agents.get(id) else {
            return;
        };
        let Some(text) = agent.card.as_ref().and_then(|c| c.text.as_deref()) else {
            return;
        };
        let text = crate::sanitize::clip_chars(&crate::sanitize::clean(text), ANNOUNCE_CARD_CHARS);
        if text.is_empty() {
            return;
        }
        let line = format!("{} needs a decision: {text}", agent.name);
        self.say(line, Some(id.clone()), "question");
    }

    /// Is the deck showing a face built from the `agents` template?
    ///
    /// `device.face` is the id the device last reported and `device.faces`
    /// is the strip it reported with it, so this is what the operator is
    /// actually looking at — not what the host would like them to be.
    fn on_agents_face(&self) -> bool {
        let device = &self.snapshot.device;
        device
            .face
            .and_then(|f| device.faces.iter().find(|e| e.f == f))
            .is_some_and(|e| e.t == crate::Template::Agents)
    }

    /// A same-id re-push updates content only: no new chime, drawer state kept.
    #[allow(clippy::too_many_arguments)]
    fn repush_card(
        &mut self,
        id: &AgentId,
        cid: CardId,
        text: String,
        choices: Vec<Choice>,
        total: usize,
        choice_map: Vec<u8>,
        rec: Option<u8>,
        free_text: bool,
    ) -> Result<CardId, ApiError> {
        let card = self.matching_card(id, cid)?;
        if !card.status.on_device() {
            return Err(ApiError::bad_state("cleared", None));
        }
        if let Some(card) = self.current_card_mut(id) {
            card.text = Some(text);
            card.choices = choices;
            card.choices_n = u8::try_from(total).unwrap_or(u8::MAX);
            card.choice_map = choice_map;
            card.rec = rec;
            card.free_text = free_text;
        }
        let card = Self::card_json(&self.snapshot.agents[id]);
        self.emit(
            EventKind::CardSet,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": card, "repush": true}),
        );
        self.put_card(id);
        Ok(cid)
    }

    /// Drop the agent's current card (replaced by a new one).
    fn retire_current_card(&mut self, id: &AgentId, reason: &str) {
        let Some(agent) = self.snapshot.agents.get_mut(id) else {
            return;
        };
        let Some(old) = agent.card.take() else {
            return;
        };
        let slot = agent.slot;
        if old.deliver == Deliver::Hold && old.status.hold_parked() {
            // `Cleared`, not `Expiry`: this card is being replaced, so
            // nothing degraded is left behind for the adapter to remember.
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: old.id,
                reason: Release::Cleared,
            });
        }
        if let Some(slot) = slot.filter(|_| old.status.on_device()) {
            self.device_op(DeviceOp::Remove(Key::Card(slot, old.id.0)));
            self.device_op(DeviceOp::Remove(Key::DocText(
                slot,
                DocKind::Card,
                old.id.0,
            )));
        }
        self.clear_deadlines(id, false);
        self.emit(
            EventKind::CardCleared,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": old.id, "delivered": matches!(old.status, CardStatus::Delivered), "reason": reason}),
        );
    }

    fn clear_deadlines(&mut self, id: &AgentId, keep_hold: bool) {
        if let Some(aux) = self.aux.get_mut(id) {
            aux.deliver_at = None;
            aux.expire_at = None;
            if !keep_hold {
                aux.hold_at = None;
            }
        }
    }

    /// Take the card off both sides: `card clear` on the wire (when the
    /// device had it), the view forgets it, the store drops it.
    fn clear_card(
        &mut self,
        id: &AgentId,
        kind: EventKind,
        delivered: bool,
        reason: Option<&str>,
        keep_hold: bool,
    ) {
        let Some(agent) = self.snapshot.agents.get_mut(id) else {
            return;
        };
        let Some(card) = agent.card.take() else {
            return;
        };
        let slot = agent.slot;
        if let Some(slot) = slot.filter(|_| card.status.on_device()) {
            self.device_op(DeviceOp::Control(Control::CardClear(slot, card.id.0)));
            self.device_op(DeviceOp::Remove(Key::Card(slot, card.id.0)));
            self.device_op(DeviceOp::Remove(Key::DocText(
                slot,
                DocKind::Card,
                card.id.0,
            )));
        }
        self.clear_deadlines(id, keep_hold);
        self.emit(
            kind,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": card.id, "delivered": delivered, "reason": reason}),
        );
        self.restore_state_after_card(id);
    }

    /// A question card flipped the agent to `question`; its end flips it back.
    fn restore_state_after_card(&mut self, id: &AgentId) {
        let prev = self.aux.get_mut(id).and_then(|a| a.prev_state.take());
        let ts = self.ts_now();
        if let (Some(prev), Some(agent)) = (prev, self.snapshot.agents.get_mut(id)) {
            if agent.state == AgentState::Question {
                agent.state = prev;
                agent.since = ts;
                self.emit(
                    EventKind::AgentChanged,
                    self.principal_of(id),
                    serde_json::json!({"agent": id, "fields": {"state": prev, "since": ts}}),
                );
                self.put_agent_state(id);
            }
        }
    }

    fn mark_delivered(&mut self, id: &AgentId) {
        let Some(card) = self.current_card_mut(id) else {
            return;
        };
        card.status = CardStatus::Delivered;
        let (cid, via) = (card.id, deliver_name(card.deliver));
        self.emit(
            EventKind::CardDelivered,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": cid, "via": via}),
        );
    }

    pub(crate) fn fail_card(&mut self, id: &AgentId, reason: &str) {
        let Some(card) = self.current_card_mut(id) else {
            return;
        };
        card.status = CardStatus::Failed {
            reason: reason.to_string(),
        };
        let cid = card.id;
        if let Some(aux) = self.aux.get_mut(id) {
            aux.deliver_at = None;
        }
        self.emit(
            EventKind::CardFailed,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": cid, "reason": reason}),
        );
    }

    /// Link down: an answer in flight can no longer be confirmed.
    pub(crate) fn fail_answering_cards(&mut self, reason: &str) {
        let ids: Vec<AgentId> = self
            .snapshot
            .agents
            .values()
            .filter(|a| {
                a.card
                    .as_ref()
                    .is_some_and(|c| matches!(c.status, CardStatus::Answering { .. }))
            })
            .map(|a| a.id.clone())
            .collect();
        for id in ids {
            self.fail_card(&id, reason);
        }
    }

    /// The same id re-pushed as a `⌨ Focus terminal` card (§4.8).
    pub(crate) fn degrade_card(&mut self, id: &AgentId, reason: &str) {
        let Some(card) = self.current_card_mut(id) else {
            return;
        };
        if !card.status.on_device() {
            return;
        }
        card.status = CardStatus::Degraded { focused_at: None };
        card.deliver = Deliver::Focus;
        card.expires = None;
        let cid = card.id;
        self.clear_deadlines(id, false);
        self.emit(
            EventKind::CardDegraded,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": cid, "reason": reason}),
        );
        self.put_card(id);
    }

    /// A push card whose owner is gone and who has no terminal: informational.
    pub(crate) fn neutralise_card(&mut self, id: &AgentId, reason: &str) {
        let Some(card) = self.current_card_mut(id) else {
            return;
        };
        if !card.status.on_device() {
            return;
        }
        card.deliver = Deliver::None;
        card.choices.clear();
        card.rec = None;
        let cid = card.id;
        self.clear_deadlines(id, false);
        self.emit(
            EventKind::CardDegraded,
            self.principal_of(id),
            serde_json::json!({"agent": id, "card": cid, "reason": reason, "deliver": "none"}),
        );
        self.put_card(id);
    }

    pub(crate) fn expire_card(&mut self, id: &AgentId, reason: &str) {
        if let Some(card) = self
            .current_card(id)
            .filter(|c| c.deliver == Deliver::Hold && c.status.hold_parked())
        {
            // The card is cleared here, never degraded, so the release is
            // `Cleared` — `Expiry` is the *hold budget*, which leaves the
            // `⌨ Focus terminal` card on the deck (`tick.rs`).
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: card.id,
                reason: Release::Cleared,
            });
        }
        self.clear_card(id, EventKind::CardExpired, false, Some(reason), false);
    }

    /// `agent.card.clear` (ownership already checked).
    pub(crate) fn card_clear_api(&mut self, c: CardClear) -> ApiResult {
        let id = c.agent.clone();
        match self.current_card(&id) {
            None => return Ok(self.rev()),
            Some(card) if card.id != c.card => return Err(ApiError::stale_card(Some(card.id))),
            Some(card) => {
                if card.deliver == Deliver::Hold && card.status.hold_parked() {
                    // The owner moved on: the parked hook resolves with `{}`
                    // and the card goes off the deck with it.
                    self.effects.push(Effect::ReleaseHold {
                        agent: id.clone(),
                        card: card.id,
                        reason: Release::Cleared,
                    });
                }
            }
        }
        self.clear_card(
            &id,
            EventKind::CardCleared,
            c.delivered,
            c.reason.as_deref(),
            false,
        );
        Ok(self.rev())
    }

    /// A decision on a card: device `answer`, `op.answer`, later PTT.
    pub(crate) fn answer(
        &mut self,
        id: &AgentId,
        card_id: CardId,
        input: AnswerInput,
    ) -> ApiResult {
        let card = self.matching_card(id, card_id)?;
        let orphaned = self.is_orphaned(id);
        match &card.status {
            CardStatus::Pending | CardStatus::Snoozed => {}
            CardStatus::Degraded { focused_at: None } => {
                return self.focus_card(id, input.principal)
            }
            CardStatus::Failed { .. } => match card.deliver {
                Deliver::Hold => return Err(ApiError::bad_state("failed", Some("hold_gone"))),
                Deliver::Push | Deliver::Poll if orphaned => {
                    return Err(ApiError::bad_state("failed", Some("owner_gone")))
                }
                _ => {}
            },
            CardStatus::Answering { .. } => return Err(ApiError::bad_state("answering", None)),
            CardStatus::Delivered => return Err(ApiError::bad_state("delivered", None)),
            CardStatus::Cleared => return Err(ApiError::bad_state("cleared", None)),
            CardStatus::Degraded {
                focused_at: Some(_),
            } => return Err(ApiError::bad_state("degraded", Some("focused"))),
        }
        match card.deliver {
            Deliver::Focus => return self.focus_card(id, input.principal),
            Deliver::None => return Err(ApiError::undeliverable("informational")),
            _ => {}
        }
        let label = match input.choice {
            Some(n) => Some(
                card.choices
                    .get(usize::from(n))
                    .ok_or_else(|| ApiError::invalid("choice", "out_of_range"))?
                    .label
                    .clone(),
            ),
            None => None,
        };
        if label.is_none() && input.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            return Err(ApiError::invalid("choice", "missing"));
        }
        let (kind, deliver, group) = (card.kind, card.deliver, card.origin.group);
        let via = if kind == CardKind::R {
            // A Ready step is an order to the owner, by push/poll only (§5.6).
            match self.caps_of(id).order {
                OrderCap::Push => Deliver::Push,
                OrderCap::Poll => Deliver::Poll,
                OrderCap::Inject | OrderCap::None => {
                    self.emit(
                        EventKind::OrderFailed,
                        input.principal,
                        serde_json::json!({"agent": id, "reason": "no_route", "text_kept": true}),
                    );
                    return Err(ApiError::undeliverable("no_route"));
                }
            }
        } else {
            deliver
        };
        let text = if kind == CardKind::R {
            label.clone().or_else(|| input.text.clone())
        } else {
            input.text.clone()
        };
        let answer = Answer {
            choice: input.choice,
            label: label.clone(),
            text: text.clone(),
            source: input.source,
            principal: input.principal.clone(),
            session: input.session,
            utterance: input.utterance.clone(),
        };
        if let Some(card) = self.current_card_mut(id) {
            card.status = CardStatus::Answering {
                choice: input.choice,
                text: text.clone(),
            };
        }
        let intermediate = kind == CardKind::Q
            && deliver == Deliver::Hold
            && group.is_some_and(|g| g.index + 1 < g.count);
        let via_name = deliver_name(via);
        self.emit(
            EventKind::CardAnswered,
            input.principal.clone(),
            serde_json::json!({
                "agent": id, "card": card_id, "choice": input.choice, "label": label, "text": text,
                "via": via_name, "source": input.source, "principal": input.principal,
                "utterance": input.utterance,
            }),
        );
        if kind == CardKind::R {
            self.emit(
                EventKind::OrderDispatched,
                input.principal.clone(),
                serde_json::json!({
                    "agent": id, "via": via_name, "text": text, "card": card_id,
                    "utterance": input.utterance,
                }),
            );
        }
        self.effects.push(Effect::Deliver {
            agent: id.clone(),
            card: card_id,
            answer,
            deliver: via,
        });
        if intermediate {
            // Cleared at once: the accumulator keeps the answer, card k+1 follows (§4.6).
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: card_id,
                reason: Release::GroupStep,
            });
            self.mark_delivered(id);
            self.clear_card(id, EventKind::CardCleared, true, Some("group_step"), true);
        } else {
            let at = self.now + DELIVERY_BUDGET;
            if let Some(aux) = self.aux.get_mut(id) {
                aux.deliver_at = Some(at);
            }
            self.timer(TimerKey::DeliveryTimeout(card_id), at);
        }
        Ok(
            serde_json::json!({"rev": self.snapshot.rev, "delivered": intermediate, "via": via_name}),
        )
    }

    /// ENTER on a `focus` card: activate the terminal, clear honestly, retire
    /// the card on the device (§4.8).
    fn focus_card(&mut self, id: &AgentId, principal: Principal) -> ApiResult {
        let ts = self.ts_now();
        let Some(agent) = self.snapshot.agents.get_mut(id) else {
            return Err(ApiError::not_found("agent"));
        };
        let Some(card) = agent.card.as_mut() else {
            return Err(ApiError::not_found("card"));
        };
        let cid = card.id;
        let slot = agent.slot;
        let degraded = matches!(card.status, CardStatus::Degraded { .. });
        self.emit(
            EventKind::CardFocused,
            principal,
            serde_json::json!({"agent": id, "card": cid}),
        );
        self.effects
            .push(Effect::Desktop(DesktopCmd::Focus { agent: id.clone() }));
        if let Some(slot) = slot {
            self.device_op(DeviceOp::Control(Control::CardClear(slot, cid.0)));
            self.device_op(DeviceOp::Remove(Key::Card(slot, cid.0)));
            self.device_op(DeviceOp::Remove(Key::DocText(slot, DocKind::Card, cid.0)));
        }
        self.clear_deadlines(id, false);
        if degraded {
            // Kept for the GUI until the hook flow ends (PostToolUse/Stop clears).
            if let Some(card) = self.current_card_mut(id) {
                card.status = CardStatus::Degraded {
                    focused_at: Some(ts),
                };
            }
        } else if let Some(agent) = self.snapshot.agents.get_mut(id) {
            agent.card = None;
            self.emit(
                EventKind::CardCleared,
                self.principal_of(id),
                serde_json::json!({"agent": id, "card": cid, "delivered": true, "reason": "focus"}),
            );
            self.restore_state_after_card(id);
        }
        Ok(serde_json::json!({"rev": self.snapshot.rev, "delivered": true, "via": "focus"}))
    }

    pub(crate) fn on_device_answer(&mut self, slot: Slot, card: CardId, choice: u8) {
        let Some(id) = self.snapshot.agent_at(slot).map(|a| a.id.clone()) else {
            self.device_refusal("device.answer", slot, card, ApiError::not_found("agent"));
            return;
        };
        let input = AnswerInput {
            choice: Some(choice),
            text: None,
            source: AnswerSource::Deck,
            principal: Principal::Device,
            session: None,
            // A chosen option, not a dictated one: the operator pressed a
            // key, and there is no recording to point at.
            utterance: None,
        };
        if let Err(e) = self.answer(&id, card, input) {
            self.device_refusal("device.answer", slot, card, e);
        }
    }

    pub(crate) fn on_device_snooze(&mut self, slot: Slot, card: CardId) {
        let Some(id) = self.snapshot.agent_at(slot).map(|a| a.id.clone()) else {
            self.device_refusal("device.snooze", slot, card, ApiError::not_found("agent"));
            return;
        };
        if let Err(e) = self.snooze(&id, card, Principal::Device) {
            self.device_refusal("device.snooze", slot, card, e);
        }
    }

    /// The deck scrolled into a part of a body it does not hold and is asking
    /// for the window around line `first`.
    ///
    /// A request naming a body that is no longer the one on that slot is
    /// **dropped silently**: the deck asked about text that has since been
    /// replaced, so there is nothing to answer and nothing went wrong. A
    /// missing agent is the same — a stale request that raced a removal is not
    /// an error worth waking the operator for, unlike an answer, which is an
    /// action that has been lost.
    pub(crate) fn on_device_doc_lines(&mut self, slot: Slot, doc: DocKind, id: u16, first: u16) {
        let (body, current) = if doc == DocKind::Tr {
            match &self.transcript_doc {
                Some((at, text)) if *at == slot => (text.clone(), self.transcript_rev),
                _ => return,
            }
        } else {
            let Some(agent) = self.snapshot.agent_at(slot) else {
                return;
            };
            match crate::store::doc_body(agent, doc) {
                Some(pair) => pair,
                None if doc == DocKind::Reply => (String::new(), agent.reply_rev),
                None => return,
            }
        };
        if current != id {
            return;
        }
        if matches!(doc, DocKind::Reply | DocKind::Workflow) {
            if let Some(agent) = self.snapshot.agent_at(slot) {
                let aux = self.aux.entry(agent.id.clone()).or_default();
                aux.workflow_view = doc == DocKind::Workflow;
                aux.reply_first = Some(first);
            }
        }
        let window = if doc == DocKind::Reply && body.is_empty() {
            crate::store::doc_window(slot, doc, id, "No messages yet", first, self.text_size())
        } else if doc == DocKind::Tr {
            crate::store::doc_window(slot, doc, id, &body, first, self.text_size())
        } else {
            self.snapshot.agent_at(slot).and_then(|agent| crate::store::agent_doc_window(agent, doc, first, self.text_size(), self.aux.get(&agent.id).map(|a| &a.expanded_tools)))
        };
        let Some(window) = window else {
            return;
        };
        self.device_op(DeviceOp::Put(Key::DocText(slot, doc, id), window));
    }

    pub(crate) fn on_device_tool(&mut self, slot: Slot, id: u16, index: u8) {
        let Some(agent) = self.snapshot.agent_at(slot).filter(|a| a.chat_rev == id) else { return; };
        let workflow = self.aux.get(&agent.id).is_some_and(|a| a.workflow_view);
        let key = match (workflow, index) {
            (true, 64) => "workflow:plan".to_owned(),
            (true, 65) => "workflow:tools".to_owned(),
            _ => {
                let Some(entry) = agent.messages.get(index as usize).filter(|m| m.message.role == crate::ChatRole::Tool) else { return; };
                entry.message.id.clone()
            }
        };
        let agent_id = agent.id.clone();
        let expanded = self.aux.get(&agent_id).map(|a| &a.expanded_tools);
        let (_, lines) = if workflow { crate::workflow::device_lines(agent, self.text_size(), expanded) }
            else { crate::store::chat_lines(agent, self.text_size(), expanded) };
        let Some(first) = lines.iter().position(|line| deck_wrap::tool_entry(line).is_some_and(|(i, _)| i == index)) else { return; };
        let aux = self.aux.entry(agent_id.clone()).or_default();
        if !aux.expanded_tools.remove(&key) { aux.expanded_tools.insert(key); }
        // Keep the touched header in view. A new revision discards cached
        // lines whose offsets changed, and rejects duplicate/stale taps.
        aux.reply_first = Some(first.min(u16::MAX as usize) as u16);
        let agent = self.snapshot.agents.get_mut(&agent_id).unwrap();
        agent.chat_rev = agent.chat_rev.wrapping_add(1).max(1);
        self.put_reply(&agent_id);
    }

    /// A device edge the store could not honour is an `error` event, never a panic.
    fn device_refusal(&mut self, context: &str, slot: Slot, card: CardId, e: ApiError) {
        self.emit(
            EventKind::Error,
            Principal::Device,
            serde_json::json!({"context": context, "slot": slot, "card": card, "code": e.code, "message": e.message, "data": e.data}),
        );
    }

    /// Device CANCEL / `op.snooze`: back to the queue; a held hook is released
    /// and the same id re-pushed degraded (§4.8).
    pub(crate) fn snooze(
        &mut self,
        id: &AgentId,
        card_id: CardId,
        principal: Principal,
    ) -> ApiResult {
        let (status, deliver) = {
            let card = self.matching_card(id, card_id)?;
            (card.status.clone(), card.deliver)
        };
        match status {
            CardStatus::Pending => {}
            CardStatus::Snoozed | CardStatus::Degraded { focused_at: None } => {
                return Ok(self.rev());
            }
            CardStatus::Answering { .. } => return Err(ApiError::bad_state("answering", None)),
            CardStatus::Delivered => return Err(ApiError::bad_state("delivered", None)),
            CardStatus::Cleared => return Err(ApiError::bad_state("cleared", None)),
            // §4.8's `failed → degraded`, which is the GUI's **Release to
            // terminal** and the only documented way out of `failed` for a
            // `hold` card (Retry is refused there because the parked response
            // is gone — that is what failing means). So there is no hold left
            // to release: the `⌨` card is re-pushed under the same id, and a
            // later ENTER focuses the terminal instead of failing again.
            // Until this arm existed the button could only ever answer
            // `bad_state`, and `failed → degraded` had no producer at all.
            CardStatus::Failed { .. } => {
                self.degrade_card(id, "release");
                return Ok(self.rev());
            }
            CardStatus::Degraded {
                focused_at: Some(_),
            } => return Err(ApiError::bad_state("degraded", Some("focused"))),
        }
        if let Some(card) = self.current_card_mut(id) {
            card.status = CardStatus::Snoozed;
        }
        self.emit(
            EventKind::CardSnoozed,
            principal,
            serde_json::json!({"agent": id, "card": card_id}),
        );
        if deliver == Deliver::Hold {
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: card_id,
                reason: Release::Snooze,
            });
            self.degrade_card(id, "snooze");
        }
        Ok(self.rev())
    }

    /// `op.ack` (GUI-only): a Ready card acknowledged.
    ///
    /// §4.7 scopes `op.ack` to Ready cards: it is the GUI saying "seen", not
    /// a decision. A type `q` card carries the operator's answer — for a
    /// `hold` card that answer *is* the parked hook response — so acking one
    /// would take the card off the deck while the hook stayed parked until
    /// `hold_s`. The withdrawal paths for a question card are `op.answer`,
    /// `op.snooze` and expiry (§4.8), each of which releases the hold, so a
    /// non-Ready card is refused here rather than cleared.
    pub(crate) fn ack(&mut self, id: &AgentId, card_id: CardId, principal: Principal) -> ApiResult {
        let card = self.matching_card(id, card_id)?;
        if card.kind != CardKind::R {
            return Err(ApiError::bad_state(card.status.name(), Some("not_ready")));
        }
        if !card.status.on_device() {
            return Err(ApiError::bad_state("cleared", None));
        }
        // A Ready card may still hold a parked hook (`deck_done` over a held
        // adapter route): the ack is the owner moving on, so release it like
        // `agent.card.clear` does rather than strand it.
        if card.deliver == Deliver::Hold && card.status.hold_parked() {
            self.effects.push(Effect::ReleaseHold {
                agent: id.clone(),
                card: card_id,
                reason: Release::Cleared,
            });
        }
        self.emit(
            EventKind::CardAcked,
            principal,
            serde_json::json!({"agent": id, "card": card_id}),
        );
        self.clear_card(id, EventKind::CardCleared, true, Some("ack"), false);
        Ok(self.rev())
    }

    /// True when `id`'s current card is the held card `card` and matches
    /// `pred`. Holds are keyed by (agent, card): ids are per slot (§6.5),
    /// so two agents routinely hold the same id.
    fn is_held_card(&self, id: &AgentId, card: CardId, pred: impl Fn(&Card) -> bool) -> bool {
        self.current_card(id)
            .is_some_and(|c| c.id == card && c.deliver == Deliver::Hold && pred(c))
    }

    pub(crate) fn on_hold_written(&mut self, id: &AgentId, card: CardId) {
        if !self.is_held_card(id, card, |c| {
            matches!(c.status, CardStatus::Answering { .. })
        }) {
            return;
        }
        self.mark_delivered(id);
        self.clear_card(
            id,
            EventKind::CardCleared,
            true,
            Some("hold_written"),
            false,
        );
    }

    /// The hook client went away: released at once, degraded at once (§12).
    pub(crate) fn on_hold_gone(&mut self, id: &AgentId, card: CardId) {
        if !self.is_held_card(id, card, |c| c.status.hold_parked()) {
            return;
        }
        if matches!(
            self.current_card(id).map(|c| &c.status),
            Some(CardStatus::Answering { .. })
        ) {
            self.fail_card(id, "hold_gone");
        } else {
            self.degrade_card(id, "hold_gone");
        }
    }

    /// Result of an `Effect::Deliver`.
    pub(crate) fn on_delivered(&mut self, id: &AgentId, card_id: CardId, ok: bool) {
        let Some(card) = self.current_card(id).filter(|c| c.id == card_id) else {
            return;
        };
        // Who sends the clear. §4.8 leaves it to the owner in one case only:
        // a `push` *answer*, which the owner acknowledges with
        // `agent.card.clear` inside the 4 s budget. A Ready step is not an
        // answer — it leaves as `op.order` by the owner's **caps** route,
        // which need not be the card's own `deliver`, and §4.7 has the
        // daemon send `card clear` after the dispatch (§4.15 step 4: the
        // owner "answers with nothing"). Reading `card.deliver` here left a
        // Ready card set `deliver:"push"` whose owner polls uncleared until
        // the budget failed a step that had in fact been delivered.
        let owner_clears = card.kind == CardKind::Q && card.deliver == Deliver::Push;
        // A hold's door reports its write here rather than through
        // `HoldWritten`, because only the door knows whether the response
        // reached the CLI (§4.9). The clear keeps the reason the hook path
        // has always published, so the bus vocabulary is unchanged.
        let why = (card.deliver == Deliver::Hold).then_some("hold_written");
        match (&card.status, ok) {
            (CardStatus::Answering { .. }, true) => {
                self.mark_delivered(id);
                if !owner_clears {
                    // hold/poll/ready: the response is written, the card is done.
                    self.clear_card(id, EventKind::CardCleared, true, why, false);
                }
            }
            (CardStatus::Answering { .. }, false) => self.fail_card(id, "undeliverable"),
            // A late `poll` pickup: store and device agree again (§4.8).
            (CardStatus::Failed { .. }, true) => {
                self.mark_delivered(id);
                self.clear_card(id, EventKind::CardCleared, true, Some("late_pickup"), false);
            }
            _ => {}
        }
    }

    /// `op.order`: owner push/poll, or the injector for trusted operator text (§5.5).
    pub(crate) fn op_order(
        &mut self,
        principal: Principal,
        session: SessionId,
        o: OpOrder,
    ) -> ApiResult {
        let id = o.agent.clone();
        self.exists(&id)?;
        if o.text.trim().is_empty() {
            return Err(ApiError::invalid("text", "empty"));
        }
        let via = match self.caps_of(&id).order {
            OrderCap::Push => "push",
            OrderCap::Poll => "poll",
            OrderCap::Inject => {
                if o.text.chars().any(|c| c.is_control() && c != '\t') {
                    return Err(ApiError::invalid("text", "control_chars"));
                }
                if self.dialog_open(&id) {
                    self.emit(
                        EventKind::OrderFailed,
                        principal,
                        serde_json::json!({"agent": id, "reason": "dialog_open", "text_kept": true}),
                    );
                    return Err(ApiError::undeliverable("dialog_open"));
                }
                "inject"
            }
            OrderCap::None => {
                self.emit(
                    EventKind::OrderFailed,
                    principal,
                    serde_json::json!({"agent": id, "reason": "no_route", "text_kept": true}),
                );
                return Err(ApiError::undeliverable("no_route"));
            }
        };
        // Written before anything is typed (§5.5). `utterance` (B12) is the
        // PTT session the words came from, so a client can line the order up
        // against the `transcript.final` that produced it instead of
        // matching on a text the §5.6 grammar may have rewritten.
        self.emit(
            EventKind::OrderDispatched,
            principal.clone(),
            serde_json::json!({
                "agent": id, "via": via, "text": o.text, "utterance": o.utterance,
            }),
        );
        if via == "inject" {
            self.effects.push(Effect::Desktop(DesktopCmd::Inject {
                agent: id,
                text: o.text,
            }));
        } else {
            self.emit(
                EventKind::AgentOrder,
                principal.clone(),
                serde_json::json!({
                    "agent": id, "text": o.text, "ctx": o.ctx, "source": o.source,
                    "principal": principal, "session": session, "via": via,
                    "utterance": o.utterance,
                }),
            );
        }
        Ok(serde_json::json!({"rev": self.snapshot.rev, "via": via}))
    }

    /// The injector chain's outcome (§5.5, week 5).
    ///
    /// `order.dispatched{text}` is written **before** the text is typed, so
    /// a chain that then refuses has to publish its own event — and this is
    /// the only place that can, because only the reducer owns `rev`. A
    /// success adds nothing: `order.dispatched` already said the exact
    /// string, and a second event per order would double the bus traffic of
    /// the one method an operator uses by voice.
    ///
    /// `text_kept: true` is the GUI's cue that the order is still in the
    /// history with Copy and Retry beside it (§4.8, §8.2).
    pub(crate) fn on_order_result(
        &mut self,
        principal: Principal,
        id: &AgentId,
        text: &str,
        ok: bool,
        reason: Option<&str>,
    ) {
        if ok {
            return;
        }
        self.emit(
            EventKind::OrderFailed,
            principal,
            serde_json::json!({
                "agent": id, "reason": reason.unwrap_or("undeliverable"),
                "text": text, "text_kept": true,
            }),
        );
    }

    /// A permission/question dialog is (or was) open in the terminal
    /// (§5.5). The predicate itself is [`Card::is_open_dialog`], shared
    /// with the core so the two sides of guard 2 cannot drift apart.
    fn dialog_open(&self, id: &AgentId) -> bool {
        self.current_card(id).is_some_and(Card::is_open_dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{card_set, claude_adapter, register, Sim, UID};
    use crate::{
        Command, DeviceLine, EventKind, Group, HostLine, LinkEvent, Origin, Request, TitleSource,
    };

    fn status(sim: &Sim, id: &AgentId) -> Option<CardStatus> {
        sim.store.snapshot().agents[id]
            .card
            .as_ref()
            .map(|c| c.status.clone())
    }

    /// A claude adapter agent with hold/inject caps in slot 1.
    fn adapter_agent(sim: &mut Sim) -> AgentId {
        let mut r = register("cohdl-api", Some("claude:1"));
        r.kind = crate::Kind::Claude;
        r.caps.answer = crate::AnswerCap::Hold;
        r.caps.order = OrderCap::Inject;
        r.terminal = Some(crate::Terminal::default());
        let res = sim
            .api(claude_adapter(), SessionId(100), Request::AgentRegister(r))
            .unwrap();
        let id = AgentId(res["agent"].as_str().unwrap().into());
        // The adapter reports the state itself (PermissionRequest → question).
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::AgentUpdate(crate::AgentUpdate {
                agent: id.clone(),
                state: Some(AgentState::Question),
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None, task: None, subagents: None,
            }),
        )
        .unwrap();
        id
    }

    fn permission(agent: &AgentId, group: Option<Group>) -> CardSet {
        let mut c = card_set(agent, CardKind::Q, Deliver::Hold, &["Allow", "Deny"]);
        c.text = "Bash: psql".into();
        c.origin = Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: None,
            group,
        };
        c
    }

    #[test]
    fn a_card_is_persisted_before_its_line_and_flips_the_agent_to_question() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["Yes", "No"]);
        assert_eq!(cid, CardId(1));
        let persist = sim
            .effects
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Persist(crate::StatePatch::CardId {
                        slot: Slot(1),
                        last: CardId(1)
                    })
                )
            })
            .unwrap();
        let put = sim
            .effects
            .iter()
            .position(|e| matches!(e, Effect::Device(DeviceOp::Put(Key::Card(Slot(1), 1), _))))
            .unwrap();
        assert!(persist < put, "persist-before-emit");
        let state_put = sim
            .effects
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Device(DeviceOp::Put(Key::AgentState(Slot(1)), _))
                )
            })
            .unwrap();
        assert!(state_put < put, "agent before card");
        assert_eq!(
            sim.kinds(),
            vec![
                EventKind::CardSet,
                EventKind::AgentChanged,
                // §7.7's spoken announcement is queued with the chime: the
                // deck has reported no face here, so there is no evidence
                // the operator can see the card.
                EventKind::VoiceUtterance,
            ]
        );
        let a = &sim.store.snapshot().agents[&id];
        assert_eq!(a.state, AgentState::Question);
        let card = a.card.as_ref().unwrap();
        assert_eq!(card.status, CardStatus::Pending);
        assert_eq!(card.choices_n, 2);
        assert_eq!(card.expires, None);
        assert!(sim.effects.contains(&Effect::Audio(AudioCmd::Chime {
            which: "question".into()
        })));
        match sim.line(Key::Card(Slot(1), 1)) {
            Some(HostLine::Card { text, choices, .. }) => {
                assert_eq!(text, "» question?", "custom cards are marked");
                assert_eq!(choices, vec!["Yes", "No"]);
            }
            other => panic!("{other:?}"),
        }
        // Ids climb per slot.
        let cid = sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        assert_eq!(cid, CardId(2));
        assert!(sim.events.iter().any(|e| e.kind == EventKind::CardCleared
            && e.data["card"] == 1
            && e.data["reason"] == "replaced"));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Remove(Key::Card(Slot(1), 1)))));
    }

    #[test]
    fn card_validation_refuses_what_the_device_would() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cases: Vec<(CardSet, &str)> = vec![
            (card_set(&id, CardKind::Q, Deliver::Hold, &["a"]), "deliver"),
            (
                {
                    let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &["a"]);
                    c.origin.kind = OriginKind::Permission;
                    c
                },
                "origin.kind",
            ),
            (card_set(&id, CardKind::Q, Deliver::Poll, &[]), "text"),
            (card_set(&id, CardKind::Q, Deliver::None, &["a"]), "choices"),
            (
                {
                    let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &["a"]);
                    c.rec = Some(1);
                    c
                },
                "rec",
            ),
            (card_set(&id, CardKind::Q, Deliver::Poll, &[" "]), "choices"),
        ];
        for (mut c, field) in cases {
            if field == "text" {
                c.text = "  ".into();
            }
            let err = sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID, "{field}");
            assert_eq!(err.data["field"], field);
        }
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::CardSet(card_set(&id, CardKind::Q, Deliver::Poll, &["a"])),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT, "not the owner");
    }

    #[test]
    fn more_than_four_choices_are_truncated_with_the_recommended_first() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let mut c = card_set(
            &id,
            CardKind::Q,
            Deliver::Poll,
            &["a", "b", "c", "d", "e", "f"],
        );
        c.rec = Some(4);
        sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(
            card.choices
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>(),
            vec!["e", "b", "c", "d"]
        );
        assert_eq!(card.rec, Some(0));
        assert_eq!(card.choices_n, 6);
        assert!(sim.events.iter().any(|e| e.kind == EventKind::CardTruncated
            && e.data["shown"] == 4
            && e.data["total"] == 6));
        // B10: the swap-and-cut is recorded, so the answer can be mapped
        // back to the option the owner sent.
        assert_eq!(card.choice_map, vec![4, 1, 2, 3]);
    }

    /// B10: `choices.swap(0, rec)` then `truncate(4)` left nothing saying
    /// which shown row was which option, while `Answer.choice` indexes the
    /// **shown** list — so an adapter with more than four options (a Codex
    /// app-server list, a virtual agent; Claude cannot exceed four, C7) had
    /// only `Answer.label` to match on, and two long options can cut to the
    /// same label.
    #[test]
    fn a_truncated_card_records_which_option_each_shown_row_was() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let labels = ["a", "b", "c", "d", "e", "f", "g"];
        let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &labels);
        c.rec = Some(6);
        sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(card.choice_map, vec![6, 1, 2, 3]);
        // Every shown row maps to the option whose label it carries.
        for (shown, choice) in card.choices.iter().enumerate() {
            let original = usize::from(card.choice_map[shown]);
            assert_eq!(choice.label, labels[original], "shown {shown}");
        }
        // The operator picks row 0 and the owner learns it was option 6.
        sim.api(
            UID,
            SessionId(2),
            Request::OpAnswer(crate::OpAnswer {
                agent: id.clone(),
                card: card.id,
                choice: Some(0),
                text: None,
            }),
        )
        .unwrap();
        let delivered = sim
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Deliver { answer, .. } => Some(answer.clone()),
                _ => None,
            })
            .expect("the answer is delivered to the owner");
        assert_eq!(delivered.choice, Some(0), "the index of the SHOWN list");
        assert_eq!(delivered.label.as_deref(), Some("g"));
        assert_eq!(labels[usize::from(card.choice_map[0])], "g");

        // A recommendation that is already shown leaves the order alone…
        let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &labels);
        c.rec = Some(2);
        sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(card.choice_map, vec![0, 1, 2, 3]);
        assert_eq!(card.rec, Some(2));

        // …and four or fewer options are not a mapping at all.
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["x", "y"]);
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert!(card.choice_map.is_empty(), "nothing was cut");
        let json = serde_json::to_value(&card).unwrap();
        assert!(
            json.get("choice_map").is_none(),
            "an empty map is not on the wire: {json}"
        );
    }

    #[test]
    fn a_same_id_repush_updates_content_only() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 1, card: 1 }));
        assert_eq!(status(&sim, &id), Some(CardStatus::Snoozed));
        let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &["a", "b"]);
        c.id = Some(cid);
        c.text = "updated".into();
        let res = sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        assert_eq!(res["card"]["id"], 1);
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Snoozed),
            "drawer state kept"
        );
        assert_eq!(sim.kinds(), vec![EventKind::CardSet]);
        assert_eq!(sim.events[0].data["repush"], true);
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Persist(_))));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Audio(_))));
        match sim.line(Key::Card(Slot(1), 1)) {
            Some(HostLine::Card { text, choices, .. }) => {
                assert_eq!(text, "» updated");
                assert_eq!(choices.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &["a"]);
        c.id = Some(CardId(9));
        let err = sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        assert_eq!(err.data["card"], 1);
    }

    #[test]
    fn a_poll_answer_from_the_deck_is_delivered_then_cleared() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["Yes", "No"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 1,
        }));
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Answering {
                choice: Some(1),
                text: None
            })
        );
        assert_eq!(sim.kinds(), vec![EventKind::CardAnswered]);
        let e = &sim.events[0];
        assert_eq!(e.principal, Principal::Device);
        assert_eq!(e.data["label"], "No");
        assert_eq!(e.data["via"], "poll");
        assert_eq!(e.data["source"], "deck");
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Deliver { agent, card: CardId(1), answer, deliver: Deliver::Poll }
                if *agent == id && answer.label.as_deref() == Some("No") && answer.principal == Principal::Device
        )));
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::DeliveryTimeout(CardId(1)),
            at: sim.t + DELIVERY_BUDGET
        }));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(1),
            ok: true,
        });
        assert_eq!(
            sim.kinds(),
            vec![
                EventKind::CardDelivered,
                EventKind::CardCleared,
                EventKind::AgentChanged
            ]
        );
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Remove(Key::Card(Slot(1), 1)))));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        assert_eq!(
            sim.store.snapshot().agents[&id].state,
            AgentState::Idle,
            "back to the state before the question"
        );
        assert!(sim.puts().contains(&Key::AgentState(Slot(1))));
    }

    #[test]
    fn no_delivery_within_four_seconds_fails_the_card_without_a_clear() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.advance(Duration::from_millis(3999));
        assert!(matches!(
            status(&sim, &id),
            Some(CardStatus::Answering { .. })
        ));
        sim.advance(Duration::from_millis(2));
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Failed {
                reason: "timeout".into()
            })
        );
        assert_eq!(sim.kinds(), vec![EventKind::CardFailed]);
        assert!(
            sim.effects.is_empty(),
            "no clear: the device reverts by itself"
        );
        // The operator answers again (retry from the device).
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        assert!(matches!(
            status(&sim, &id),
            Some(CardStatus::Answering { .. })
        ));
        // A late poll pickup after a second failure clears exactly once.
        sim.advance(Duration::from_secs(5));
        assert!(matches!(status(&sim, &id), Some(CardStatus::Failed { .. })));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(1),
            ok: true,
        });
        assert_eq!(
            sim.effects
                .iter()
                .filter(|e| matches!(e, Effect::Device(DeviceOp::Control(Control::CardClear(..)))))
                .count(),
            1
        );
        assert!(sim
            .events
            .iter()
            .any(|e| e.kind == EventKind::CardCleared && e.data["reason"] == "late_pickup"));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(1),
            ok: true,
        });
        assert!(sim.events.is_empty(), "nothing left to deliver");
    }

    #[test]
    fn a_failed_delivery_result_fails_the_card() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(1),
            ok: false,
        });
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Failed {
                reason: "undeliverable".into()
            })
        );
        assert!(
            sim.store
                .project()
                .iter()
                .any(|op| matches!(op, DeviceOp::Put(Key::Card(Slot(1), 1), _))),
            "a failed card is still on the device, in the barrier"
        );
    }

    #[test]
    fn a_push_card_waits_for_the_owner_clear_within_the_budget() {
        let mut sim = Sim::new();
        let mut r = register("bot", None);
        r.caps.answer = crate::AnswerCap::Push;
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        let id = AgentId(res["agent"].as_str().unwrap().into());
        sim.set_card(&id, CardKind::Q, Deliver::Push, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(1),
            ok: true,
        });
        assert_eq!(status(&sim, &id), Some(CardStatus::Delivered));
        assert!(sim.effects.is_empty(), "the owner clears");
        sim.advance(Duration::from_secs(2));
        sim.api(
            UID,
            SessionId(1),
            Request::CardClear(CardClear {
                agent: id.clone(),
                card: CardId(1),
                delivered: true,
                reason: None,
            }),
        )
        .unwrap();
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        assert_eq!(sim.events[0].kind, EventKind::CardCleared);
        assert_eq!(sim.events[0].data["delivered"], true);
        // Clearing again is idempotent; a stale id conflicts.
        let rev = sim.store.snapshot().rev;
        let res = sim
            .api(
                UID,
                SessionId(1),
                Request::CardClear(CardClear {
                    agent: id.clone(),
                    card: CardId(1),
                    delivered: true,
                    reason: None,
                }),
            )
            .unwrap();
        assert_eq!(res["rev"], rev);
        sim.set_card(&id, CardKind::Q, Deliver::Push, vec!["a"]);
        let err = sim
            .api(
                UID,
                SessionId(1),
                Request::CardClear(CardClear {
                    agent: id.clone(),
                    card: CardId(1),
                    delivered: true,
                    reason: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        // No clear within 4 s of the answer → failed.
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 2,
            choice: 0,
        }));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: CardId(2),
            ok: true,
        });
        sim.advance(Duration::from_secs(5));
        assert!(matches!(status(&sim, &id), Some(CardStatus::Failed { .. })));
    }

    #[test]
    fn answer_refusals_follow_the_status_machine() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let err = sim
            .api(
                UID,
                SessionId(1),
                Request::OpAnswer(crate::OpAnswer {
                    agent: id.clone(),
                    card: CardId(1),
                    choice: Some(0),
                    text: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT, "no card");
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        let answer = |choice: Option<u8>, text: Option<&str>| {
            Request::OpAnswer(crate::OpAnswer {
                agent: id.clone(),
                card: CardId(1),
                choice,
                text: text.map(String::from),
            })
        };
        let err = sim
            .api(UID, SessionId(2), answer(Some(3), None))
            .unwrap_err();
        assert_eq!(err.data["field"], "choice");
        let err = sim.api(UID, SessionId(2), answer(None, None)).unwrap_err();
        assert_eq!(err.data["why"], "missing");
        let res = sim
            .api(UID, SessionId(2), answer(None, Some("free text")))
            .unwrap();
        assert_eq!(res["via"], "poll");
        assert_eq!(res["delivered"], false);
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Answering {
                choice: None,
                text: Some("free text".into())
            })
        );
        assert_eq!(sim.events[0].principal, UID);
        let err = sim
            .api(UID, SessionId(2), answer(Some(0), None))
            .unwrap_err();
        assert_eq!(err.code, ApiError::BAD_STATE);
        assert_eq!(err.data["status"], "answering");
        // The device answering an unknown slot or a stale card is an error event.
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 7,
            card: 1,
            choice: 0,
        }));
        assert_eq!(sim.events[0].kind, EventKind::Error);
        assert_eq!(sim.events[0].data["context"], "device.answer");
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 9,
            choice: 0,
        }));
        assert_eq!(sim.events[0].data["code"], ApiError::CONFLICT);
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 6, card: 1 }));
        assert_eq!(sim.events[0].data["context"], "device.snooze");
    }

    #[test]
    fn a_hold_is_written_then_cleared_and_hold_gone_degrades_at_once() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        let cid = sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        assert_eq!(cid, CardId(1));
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(card.deliver, Deliver::Hold);
        assert_eq!(
            card.expires,
            Some(sim.store.ts(sim.t).plus(Duration::from_secs(120)))
        );
        match sim.line(Key::Card(Slot(1), 1)) {
            Some(HostLine::Card { text, .. }) => {
                assert_eq!(text, "Bash: psql", "no » for adapters")
            }
            other => panic!("{other:?}"),
        }
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Deliver {
                deliver: Deliver::Hold,
                ..
            }
        )));
        sim.apply(Command::HoldWritten {
            agent: id.clone(),
            card: CardId(1),
        });
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardDelivered, EventKind::CardCleared]
        );
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        assert_eq!(
            sim.store.snapshot().agents[&id].state,
            AgentState::Question,
            "the adapter reports the state"
        );
        // A second hold whose client goes away is degraded immediately.
        sim.hold(permission(&id, None), HoldId(2)).unwrap();
        sim.drain();
        sim.apply(Command::HoldGone {
            agent: id.clone(),
            card: CardId(2),
        });
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Degraded { focused_at: None })
        );
        assert_eq!(sim.kinds(), vec![EventKind::CardDegraded]);
        assert_eq!(sim.events[0].data["reason"], "hold_gone");
        assert!(
            !sim.effects
                .iter()
                .any(|e| matches!(e, Effect::ReleaseHold { .. })),
            "nothing left to release"
        );
        match sim.line(Key::Card(Slot(1), 2)) {
            Some(HostLine::Card { text, choices, .. }) => {
                assert_eq!(text, "⌨ Bash: psql");
                assert_eq!(choices, vec!["Focus terminal"]);
            }
            other => panic!("{other:?}"),
        }
        // ENTER on the degraded card focuses the terminal and clears honestly.
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 2,
            choice: 0,
        }));
        assert_eq!(sim.kinds(), vec![EventKind::CardFocused]);
        assert!(sim
            .effects
            .contains(&Effect::Desktop(DesktopCmd::Focus { agent: id.clone() })));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                2
            )))));
        assert!(matches!(
            status(&sim, &id),
            Some(CardStatus::Degraded {
                focused_at: Some(_)
            })
        ));
        assert!(
            !sim.store
                .project()
                .iter()
                .any(|op| matches!(op, DeviceOp::Put(Key::Card(..), _))),
            "retired on the device, kept for the GUI"
        );
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAnswer(crate::OpAnswer {
                    agent: id.clone(),
                    card: CardId(2),
                    choice: Some(0),
                    text: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.data["reason"], "focused");
        // PostToolUse: the adapter clears it.
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::CardClear(CardClear {
                agent: id.clone(),
                card: CardId(2),
                delivered: false,
                reason: Some("post_tool_use".into()),
            }),
        )
        .unwrap();
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        assert!(sim.effects.is_empty(), "already retired on the device");
    }

    #[test]
    fn snooze_releases_the_hold_and_repushes_the_same_id_degraded() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 1, card: 1 }));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardSnoozed, EventKind::CardDegraded]
        );
        assert_eq!(sim.events[1].data["reason"], "snooze");
        assert!(sim.effects.contains(&Effect::ReleaseHold {
            agent: id.clone(),
            card: CardId(1),
            reason: Release::Snooze
        }));
        assert_eq!(
            sim.puts(),
            vec![Key::Card(Slot(1), 1)],
            "same id, content only"
        );
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Device(DeviceOp::Control(Control::CardClear(..))))));
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(card.deliver, Deliver::Focus);
        assert_eq!(card.expires, None);
        // A second CANCEL is a no-op; the hold timer no longer fires.
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 1, card: 1 }));
        assert!(sim.events.is_empty());
        sim.advance(Duration::from_secs(130));
        assert!(sim.events.is_empty() && sim.effects.is_empty());
    }

    /// B3: the hold path is the hook door's authorization seam, and until
    /// week 3 it was empty — it checked only that the agent existed and then
    /// borrowed the *agent's* owner principal for the adapter-only test, so
    /// the Codex adapter could park a card on a Claude agent and the store
    /// would sign it as Claude's.
    #[test]
    fn a_hold_may_only_be_parked_by_the_session_that_owns_the_agent() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        let codex = Principal::Adapter(crate::AdapterKind::Codex);
        let err = sim
            .store
            .set_card_held(&codex, SessionId(101), permission(&id, None), HoldId(1))
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["principal"], "adapter:claude");
        assert!(
            sim.store.snapshot().agents[&id].card.is_none(),
            "a refused hold writes nothing"
        );

        // The owning session, but a principal that is not an adapter: the
        // adapter-only fields are judged on the caller, not on the owner.
        let err = sim
            .store
            .set_card_held(&UID, SessionId(100), permission(&id, None), HoldId(1))
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        assert_eq!(err.data["field"], "deliver");
        assert_eq!(err.data["why"], "adapter_only");

        let mut gone = permission(&id, None);
        gone.agent = AgentId("ag_nope".into());
        let err = sim
            .store
            .set_card_held(&claude_adapter(), SessionId(100), gone, HoldId(1))
            .unwrap_err();
        assert_eq!(err.code, ApiError::NOT_FOUND);

        // The owner itself still holds.
        assert_eq!(
            sim.hold(permission(&id, None), HoldId(1)).unwrap(),
            CardId(1)
        );
    }

    /// B2: only `[hooks.claude] hold_s` ever reached `StoreConfig`, so a
    /// Codex hold silently ran on Claude's budget. The per-CLI budget is
    /// the **adapter's** input and rides on the card as `timeout_s`, which
    /// the store honours over its own default — and one budget still bounds
    /// a whole question group, whatever the later cards ask for.
    #[test]
    fn a_held_cards_own_timeout_s_is_the_budget_and_the_group_shares_it() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.store.config().hold_s,
            120,
            "the daemon-wide default a card may override"
        );
        let id = adapter_agent(&mut sim);
        let mut c = permission(&id, None);
        c.timeout_s = Some(30);
        sim.hold(c, HoldId(1)).unwrap();
        sim.drain();
        sim.advance(Duration::from_secs(29));
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending));
        sim.advance(Duration::from_secs(2));
        assert!(
            sim.effects.contains(&Effect::ReleaseHold {
                agent: id.clone(),
                card: CardId(1),
                reason: Release::Expiry,
            }),
            "31 s in, well short of the store's 120 s"
        );

        // A group: card 0 sets the budget, card 1 inherits it whatever it
        // asks for (§4.6 — the group is one held request).
        let mut first = permission(&id, Some(Group { index: 0, count: 2 }));
        first.timeout_s = Some(30);
        sim.hold(first, HoldId(2)).unwrap();
        sim.drain();
        let mut second = permission(&id, Some(Group { index: 1, count: 2 }));
        second.timeout_s = Some(90);
        let card = sim.hold(second, HoldId(3)).unwrap();
        sim.drain();
        sim.advance(Duration::from_secs(29));
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending));
        sim.advance(Duration::from_secs(2));
        assert!(
            sim.effects.contains(&Effect::ReleaseHold {
                agent: id.clone(),
                card,
                reason: Release::Expiry,
            }),
            "the group's 30 s, not the second card's 90 s"
        );
    }

    /// §7.1's `[voice] mic_warm = "card_pending"`, as the one fact the
    /// audio side cannot work out for itself.
    ///
    /// V1 measured SCO bring-up at 1031 ms median (815–1246, n=16) from
    /// start-IO to the **first non-zero sample**, and that second is the
    /// operator's first syllables. A pending Question is the moment they
    /// are about to press, so it is the moment worth paying it in advance —
    /// but the reducer holds the cards and the engine holds the microphone,
    /// so the fact has to cross the seam. The policy itself stays on the
    /// audio side: this says only what is true.
    #[test]
    fn a_pending_question_asks_the_audio_side_to_warm_the_microphone() {
        let warm = |pending: bool| Effect::Audio(crate::AudioCmd::Warm { pending });
        let mut sim = Sim::new();
        let bot = sim.register(UID, SessionId(1), "bot", None);
        assert!(
            !sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Audio(crate::AudioCmd::Warm { .. }))),
            "a registration is not a question"
        );

        sim.set_card(&bot, CardKind::Q, Deliver::Poll, vec!["a"]);
        assert!(
            sim.effects.contains(&warm(true)),
            "{:?} carries no warm-up",
            sim.effects
        );

        // …and answering it takes the microphone away again.
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        assert!(
            sim.effects.contains(&warm(false)),
            "{:?} never gives the link back",
            sim.effects
        );

        // An **edge**, not a command per apply: nothing has changed since.
        sim.advance(Duration::from_secs(1));
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Audio(crate::AudioCmd::Warm { .. }))));
    }

    #[test]
    fn hold_expiry_degrades_the_card_and_a_poll_snooze_keeps_it_answerable() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        sim.advance(Duration::from_secs(119));
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending));
        sim.advance(Duration::from_secs(2));
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Degraded { focused_at: None })
        );
        assert!(sim.effects.contains(&Effect::ReleaseHold {
            agent: id.clone(),
            card: CardId(1),
            reason: Release::Expiry
        }));
        assert_eq!(sim.events[0].data["reason"], "expiry");
        // A poll card: snooze keeps the choices, a later answer works.
        let bot = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&bot, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.api(
            UID,
            SessionId(2),
            Request::OpSnooze {
                agent: bot.clone(),
                card: CardId(1),
            },
        )
        .unwrap();
        assert_eq!(sim.kinds(), vec![EventKind::CardSnoozed]);
        // A snooze takes the card out of `Pending`, which is §7.1's
        // `mic_warm = "card_pending"` edge and the only effect it has.
        assert_eq!(
            sim.effects,
            vec![Effect::Audio(crate::AudioCmd::Warm { pending: false })]
        );
        assert_eq!(status(&sim, &bot), Some(CardStatus::Snoozed));
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 2,
            card: 1,
            choice: 0,
        }));
        assert!(matches!(
            status(&sim, &bot),
            Some(CardStatus::Answering { .. })
        ));
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpSnooze {
                    agent: bot.clone(),
                    card: CardId(1),
                },
            )
            .unwrap_err();
        assert_eq!(err.data["status"], "answering");
    }

    /// §4.8 names exactly one way out of `failed` for a `hold` card — the
    /// GUI's **Release to terminal** — and the button could only ever answer
    /// `bad_state` until `snooze` grew its `Failed` arm. The neighbouring test
    /// is named "…can_be_released" but releases it through the *adapter's*
    /// `CardClear`, so the operator's own path was never exercised.
    #[test]
    fn the_gui_releases_a_failed_hold_card_to_the_terminal() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.apply(Command::HoldGone {
            agent: id.clone(),
            card: CardId(1),
        });
        assert!(matches!(status(&sim, &id), Some(CardStatus::Failed { .. })));

        sim.api(
            UID,
            SessionId(2),
            Request::OpSnooze {
                agent: id.clone(),
                card: CardId(1),
            },
        )
        .expect("§4.8: failed -> degraded is the GUI's Release to terminal");

        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Degraded { focused_at: None }),
            "the card is re-pushed degraded under the same id"
        );
        let card = sim
            .store
            .snapshot()
            .agents
            .get(&id)
            .and_then(|a| a.card.clone())
            .expect("the card is still there");
        assert_eq!(card.id, CardId(1), "the same id, not a new card");
        assert_eq!(card.deliver, Deliver::Focus, "a keyboard-focus card now");
        // The parked response is already gone, so nothing may try to release it.
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ReleaseHold { .. })));
    }

    #[test]
    fn a_failed_hold_card_refuses_retry_and_a_timeout_card_can_be_released() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.apply(Command::HoldGone {
            agent: id.clone(),
            card: CardId(1),
        });
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Failed {
                reason: "hold_gone".into()
            })
        );
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAnswer(crate::OpAnswer {
                    agent: id.clone(),
                    card: CardId(1),
                    choice: Some(0),
                    text: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.data["reason"], "hold_gone");
        // The adapter's clear ends it without a second release.
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::CardClear(CardClear {
                agent: id.clone(),
                card: CardId(1),
                delivered: false,
                reason: None,
            }),
        )
        .unwrap();
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ReleaseHold { .. })));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        // A pending hold cleared by the owner releases it with `{}`.
        sim.hold(permission(&id, None), HoldId(2)).unwrap();
        sim.drain();
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::CardClear(CardClear {
                agent: id.clone(),
                card: CardId(2),
                delivered: false,
                reason: Some("stop".into()),
            }),
        )
        .unwrap();
        assert!(
            sim.effects.contains(&Effect::ReleaseHold {
                agent: id.clone(),
                card: CardId(2),
                reason: Release::Cleared
            }),
            "the card is gone, so the adapter is told `cleared`, not `expiry`"
        );
    }

    #[test]
    fn question_groups_clear_intermediate_answers_at_once_under_one_budget() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        let group = |index: u8| Some(Group { index, count: 3 });
        sim.hold(permission(&id, group(0)), HoldId(1)).unwrap();
        sim.drain();
        let deadline = sim.t + Duration::from_secs(120);
        sim.advance(Duration::from_secs(40));
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        assert_eq!(
            sim.kinds(),
            vec![
                EventKind::CardAnswered,
                EventKind::CardDelivered,
                EventKind::CardCleared
            ]
        );
        assert!(sim.effects.contains(&Effect::ReleaseHold {
            agent: id.clone(),
            card: CardId(1),
            reason: Release::GroupStep
        }));
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        assert!(
            !sim.effects.iter().any(|e| matches!(
                e,
                Effect::Timer {
                    key: TimerKey::DeliveryTimeout(_),
                    ..
                }
            )),
            "nothing to wait for"
        );
        // Card 2 inherits the group deadline.
        sim.hold(permission(&id, group(1)), HoldId(1)).unwrap();
        sim.drain();
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::HoldExpiry(CardId(2)),
            at: deadline
        }));
        assert_eq!(
            sim.store.snapshot().agents[&id]
                .card
                .as_ref()
                .unwrap()
                .expires,
            Some(sim.store.ts(deadline))
        );
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 2,
            choice: 1,
        }));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        // The last card waits for the hook response like any hold.
        sim.hold(permission(&id, group(2)), HoldId(1)).unwrap();
        sim.drain();
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 3,
            choice: 0,
        }));
        assert!(matches!(
            status(&sim, &id),
            Some(CardStatus::Answering { .. })
        ));
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Timer {
                key: TimerKey::DeliveryTimeout(CardId(3)),
                ..
            }
        )));
        sim.apply(Command::HoldWritten {
            agent: id.clone(),
            card: CardId(3),
        });
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        // Budget exhaustion mid-group degrades the current card.
        sim.hold(permission(&id, group(0)), HoldId(2)).unwrap();
        sim.drain();
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 4,
            choice: 0,
        }));
        sim.advance(Duration::from_secs(100));
        sim.hold(permission(&id, group(1)), HoldId(2)).unwrap();
        sim.drain();
        sim.advance(Duration::from_secs(21));
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Degraded { focused_at: None })
        );
    }

    #[test]
    fn a_ready_step_is_an_order_by_push_or_poll_then_a_clear() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = sim.set_card(&id, CardKind::R, Deliver::Poll, vec!["Deploy", "Test"]);
        let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
        assert_eq!(
            card.expires,
            Some(sim.store.ts(sim.t).plus(Duration::from_secs(900)))
        );
        assert_eq!(
            sim.store.snapshot().agents[&id].state,
            AgentState::Idle,
            "a ready card does not touch the state"
        );
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: cid.0,
            choice: 1,
        }));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardAnswered, EventKind::OrderDispatched]
        );
        assert_eq!(sim.events[1].data["text"], "Test");
        assert_eq!(sim.events[1].data["via"], "poll");
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Deliver { deliver: Deliver::Poll, answer, .. }
                if answer.text.as_deref() == Some("Test") && answer.choice == Some(1)
        )));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: cid,
            ok: true,
        });
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        // An owner that cannot take orders: no route, the card stays.
        let mut r = register("mute", None);
        r.caps.order = OrderCap::None;
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        let mute = AgentId(res["agent"].as_str().unwrap().into());
        sim.set_card(&mute, CardKind::R, Deliver::Poll, vec!["Deploy"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 2,
            card: 1,
            choice: 0,
        }));
        assert_eq!(sim.events[0].kind, EventKind::OrderFailed);
        assert_eq!(sim.events[1].kind, EventKind::Error);
        assert_eq!(status(&sim, &mute), Some(CardStatus::Pending));
    }

    #[test]
    fn ready_cards_expire_on_ttl_state_change_or_ack() {
        // A short TTL keeps every wait under the 30 s lease.
        let mut sim = Sim::with(|cfg| cfg.ready_ttl_s = 20);
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::R, Deliver::Poll, vec!["Deploy"]);
        sim.advance(Duration::from_secs(21));
        assert_eq!(sim.kinds(), vec![EventKind::CardExpired]);
        assert_eq!(sim.events[0].data["reason"], "ttl");
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                1
            )))));
        let mut c = card_set(&id, CardKind::R, Deliver::Poll, &["Deploy"]);
        c.timeout_s = Some(10);
        sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        sim.advance(Duration::from_secs(5));
        sim.api(
            UID,
            SessionId(1),
            Request::AgentUpdate(crate::AgentUpdate {
                agent: id.clone(),
                state: Some(AgentState::Working),
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None, task: None, subagents: None,
            }),
        )
        .unwrap();
        assert!(sim
            .events
            .iter()
            .any(|e| e.kind == EventKind::CardExpired && e.data["reason"] == "state_change"));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        sim.advance(Duration::from_secs(10));
        assert!(sim.events.is_empty(), "no stale expiry");
        sim.api(
            UID,
            SessionId(1),
            Request::AgentHeartbeat {
                agents: vec![id.clone()],
            },
        )
        .unwrap();
        sim.set_card(&id, CardKind::R, Deliver::Poll, vec![]);
        sim.api(
            UID,
            SessionId(2),
            Request::OpAck {
                agent: id.clone(),
                card: CardId(3),
            },
        )
        .unwrap();
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardAcked, EventKind::CardCleared]
        );
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAck {
                    agent: id.clone(),
                    card: CardId(3),
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        // A question card with a timeout expires too, and the state comes back.
        let mut c = card_set(&id, CardKind::Q, Deliver::Poll, &["a"]);
        c.timeout_s = Some(15);
        sim.api(UID, SessionId(1), Request::CardSet(c)).unwrap();
        assert_eq!(sim.store.snapshot().agents[&id].state, AgentState::Question);
        sim.advance(Duration::from_secs(16));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        assert_eq!(sim.store.snapshot().agents[&id].state, AgentState::Working);
    }

    #[test]
    fn a_ready_step_is_cleared_on_the_caps_route_even_when_the_cards_deliver_differs() {
        // §4.7: on a type `r` card the choice leaves as `op.order` to the
        // owner by push/poll *only*, then `card clear`. The route is the
        // owner's caps route, which need not be the card's own `deliver`.
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None); // caps.order = poll
        let cid = sim.set_card(&id, CardKind::R, Deliver::Push, vec!["Deploy"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: cid.0,
            choice: 0,
        }));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardAnswered, EventKind::OrderDispatched]
        );
        assert_eq!(
            sim.events[1].data["via"], "poll",
            "the caps route, not the card's deliver"
        );
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Deliver {
                deliver: Deliver::Poll,
                ..
            }
        )));
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: cid,
            ok: true,
        });
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardDelivered, EventKind::CardCleared]
        );
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(1),
                cid.0
            )))));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        // The 4 s budget can no longer fail a step that was delivered.
        sim.advance(DELIVERY_BUDGET + Duration::from_secs(1));
        assert!(!sim.events.iter().any(|e| e.kind == EventKind::CardFailed));
    }

    #[test]
    fn op_ack_refuses_a_card_that_is_not_ready_and_never_strands_a_hold() {
        // §4.7: `op.ack` marks a *Ready* card acknowledged and sends the
        // clear. A held permission card is a type `q` card whose answer is
        // the parked hook response, so acking it would take the card off the
        // deck and leave Claude blocked until `hold_s`.
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        sim.hold(permission(&id, None), HoldId(1)).unwrap();
        sim.drain();
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAck {
                    agent: id.clone(),
                    card: CardId(1),
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::BAD_STATE);
        assert_eq!(err.data["status"], "pending");
        assert_eq!(err.data["reason"], "not_ready");
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending), "still parked");
        assert!(sim.events.is_empty(), "no card.acked for a question card");
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ReleaseHold { .. })));
        // A Ready card is acked, and a parked hold on one is released with it.
        let mut held_ready = permission(&id, None);
        held_ready.kind = CardKind::R;
        held_ready.origin = Origin {
            kind: OriginKind::Ready,
            tool: None,
            request_id: None,
            group: None,
        };
        let cid = sim.hold(held_ready, HoldId(2)).unwrap();
        sim.drain();
        sim.api(
            UID,
            SessionId(2),
            Request::OpAck {
                agent: id.clone(),
                card: cid,
            },
        )
        .unwrap();
        assert_eq!(
            sim.kinds(),
            vec![EventKind::CardAcked, EventKind::CardCleared]
        );
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::ReleaseHold { card, reason: Release::Cleared, .. } if *card == cid
        )));
        assert!(sim.store.snapshot().agents[&id].card.is_none());
    }

    #[test]
    fn a_focus_and_an_informational_card_behave_per_their_mode() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Focus, vec!["Focus terminal"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        assert_eq!(
            sim.kinds(),
            vec![
                EventKind::CardFocused,
                EventKind::CardCleared,
                EventKind::AgentChanged
            ]
        );
        assert!(sim.store.snapshot().agents[&id].card.is_none());
        sim.set_card(&id, CardKind::Q, Deliver::None, vec![]);
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAnswer(crate::OpAnswer {
                    agent: id.clone(),
                    card: CardId(2),
                    choice: None,
                    text: Some("x".into()),
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::UNDELIVERABLE);
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending));
    }

    #[test]
    fn link_down_fails_answers_in_flight() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        sim.apply(Command::Link(LinkEvent::Down));
        assert_eq!(
            status(&sim, &id),
            Some(CardStatus::Failed {
                reason: "link".into()
            })
        );
        assert!(sim.events.iter().any(|e| e.kind == EventKind::CardFailed));
    }

    #[test]
    fn an_orphaned_owner_downgrades_its_cards() {
        let mut sim = Sim::new();
        // hold → released and degraded
        let adapter = adapter_agent(&mut sim);
        sim.hold(permission(&adapter, None), HoldId(1)).unwrap();
        sim.drain();
        sim.apply(Command::SessionClosed(SessionId(100)));
        assert!(sim.effects.contains(&Effect::ReleaseHold {
            agent: adapter.clone(),
            card: CardId(1),
            reason: Release::AgentGone
        }));
        assert_eq!(
            status(&sim, &adapter),
            Some(CardStatus::Degraded { focused_at: None })
        );
        // push with a terminal → focus; without → informational
        for (terminal, expect) in [(true, Deliver::Focus), (false, Deliver::None)] {
            let mut r = register("p", None);
            r.caps.answer = crate::AnswerCap::Push;
            r.terminal = terminal.then(crate::Terminal::default);
            let res = sim
                .api(
                    UID,
                    SessionId(if terminal { 7 } else { 8 }),
                    Request::AgentRegister(r),
                )
                .unwrap();
            let id = AgentId(res["agent"].as_str().unwrap().into());
            let session = if terminal { SessionId(7) } else { SessionId(8) };
            sim.api(
                UID,
                session,
                Request::CardSet(card_set(&id, CardKind::Q, Deliver::Push, &["a", "b"])),
            )
            .unwrap();
            sim.apply(Command::SessionClosed(session));
            let card = sim.store.snapshot().agents[&id].card.clone().unwrap();
            assert_eq!(card.deliver, expect);
            if expect == Deliver::None {
                assert!(card.choices.is_empty());
                assert!(
                    matches!(sim.line(Key::Card(Slot(3), 1)), Some(HostLine::Card { choices, .. }) if choices.is_empty())
                );
            }
        }
        // The poll card of an orphan cannot be retried after a failure.
        let bot = sim.register(UID, SessionId(9), "poll", None);
        sim.set_card(&bot, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 4,
            card: 1,
            choice: 0,
        }));
        sim.advance(Duration::from_secs(5));
        sim.apply(Command::SessionClosed(SessionId(9)));
        let err = sim
            .api(
                UID,
                SessionId(2),
                Request::OpAnswer(crate::OpAnswer {
                    agent: bot.clone(),
                    card: CardId(1),
                    choice: Some(0),
                    text: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.data["reason"], "owner_gone");
    }

    /// B12: an order dictated over PTT names the utterance it came from, on
    /// `order.dispatched` and on `agent.order` (§4.12's `utterance?`).
    ///
    /// The field existed on [`crate::Order`] from week 1 and nothing could
    /// fill it, because `op.order` had nowhere to carry it. Matching the two
    /// up by text is not a substitute: §5.6's grammar is allowed to rewrite
    /// what was said before it becomes an order.
    #[test]
    fn a_dictated_order_names_the_utterance_it_came_from() {
        let mut sim = Sim::new();
        let bot = sim.register(UID, SessionId(1), "bot", None);
        let order = |utterance: Option<&str>| {
            Request::OpOrder(OpOrder {
                agent: bot.clone(),
                text: "run the tests".into(),
                ctx: "talk".into(),
                source: crate::OrderSource::Ptt,
                utterance: utterance.map(String::from),
            })
        };
        sim.api(UID, SessionId(2), order(Some("ut_7"))).unwrap();
        assert_eq!(
            sim.kinds(),
            vec![EventKind::OrderDispatched, EventKind::AgentOrder]
        );
        for event in &sim.events {
            assert_eq!(event.data["utterance"], "ut_7", "{:?}", event.kind);
        }
        // Typed from the GUI or an API client: the key is present and null,
        // never missing, so a reader never has to tell "no utterance" from
        // "an older daemon".
        sim.apply(crate::Command::Tick(sim.t));
        sim.api(UID, SessionId(2), order(None)).unwrap();
        for event in &sim.events {
            assert_eq!(
                event.data["utterance"],
                serde_json::Value::Null,
                "{:?}",
                event.kind
            );
            assert!(event.data.get("utterance").is_some(), "{:?}", event.kind);
        }
        // …and a caller that never heard of the field is still accepted
        // (`#[serde(default)]`): every existing `op.order` keeps working.
        let req: Request = serde_json::from_value(serde_json::json!({
            "method": "op.order",
            "params": {"agent": bot, "text": "ship it", "ctx": "talk", "source": "api"},
        }))
        .expect("op.order without an utterance");
        assert!(matches!(
            req,
            Request::OpOrder(OpOrder {
                utterance: None,
                ..
            })
        ));
    }

    #[test]
    fn orders_route_by_the_owner_caps_with_the_dialog_guard() {
        let mut sim = Sim::new();
        let bot = sim.register(UID, SessionId(1), "bot", None);
        let order = |agent: &AgentId, text: &str| {
            Request::OpOrder(OpOrder {
                agent: agent.clone(),
                text: text.into(),
                ctx: "talk".into(),
                source: crate::OrderSource::Gui,
                utterance: None,
            })
        };
        let res = sim
            .api(UID, SessionId(2), order(&bot, "run tests"))
            .unwrap();
        assert_eq!(res["via"], "poll");
        assert_eq!(
            sim.kinds(),
            vec![EventKind::OrderDispatched, EventKind::AgentOrder]
        );
        assert_eq!(sim.events[1].data["session"], "sess_2");
        assert!(sim.effects.is_empty());
        let err = sim.api(UID, SessionId(2), order(&bot, " ")).unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        let adapter = adapter_agent(&mut sim);
        let res = sim
            .api(UID, SessionId(2), order(&adapter, "fix it"))
            .unwrap();
        assert_eq!(res["via"], "inject");
        assert!(sim.effects.contains(&Effect::Desktop(DesktopCmd::Inject {
            agent: adapter.clone(),
            text: "fix it".into()
        })));
        let err = sim
            .api(UID, SessionId(2), order(&adapter, "rm\n-rf"))
            .unwrap_err();
        assert_eq!(err.data["why"], "control_chars");
        sim.hold(permission(&adapter, None), HoldId(1)).unwrap();
        sim.drain();
        let err = sim
            .api(UID, SessionId(2), order(&adapter, "yes"))
            .unwrap_err();
        assert_eq!(err.code, ApiError::UNDELIVERABLE);
        assert_eq!(err.data["reason"], "dialog_open");
        assert_eq!(sim.events[0].kind, EventKind::OrderFailed);
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 2, card: 1 }));
        let err = sim
            .api(UID, SessionId(2), order(&adapter, "yes"))
            .unwrap_err();
        assert_eq!(err.data["reason"], "dialog_open", "degraded still counts");
        let mut r = register("mute", None);
        r.caps.order = OrderCap::None;
        let res = sim
            .api(UID, SessionId(1), Request::AgentRegister(r))
            .unwrap();
        let mute = AgentId(res["agent"].as_str().unwrap().into());
        let err = sim.api(UID, SessionId(2), order(&mute, "x")).unwrap_err();
        assert_eq!(err.data["reason"], "no_route");
    }

    #[test]
    fn an_unslotted_agent_asking_a_question_gets_a_slot_and_a_slot_id() {
        let mut sim = Sim::new();
        let ids: Vec<AgentId> = (0..8)
            .map(|i| sim.register(UID, SessionId(1), &format!("a{i}"), None))
            .collect();
        for id in &ids {
            sim.api(
                UID,
                SessionId(1),
                Request::AgentUpdate(crate::AgentUpdate {
                    agent: id.clone(),
                    state: Some(AgentState::Working),
                    title: None,
                    title_source: None,
                    since: None,
                    name: None,
                    meta: None, task: None, subagents: None,
                }),
            )
            .unwrap();
        }
        let extra = sim.register(UID, SessionId(1), "extra", None);
        assert_eq!(sim.slot_of(&extra), None);
        // A ready card off the deck gets an off-deck id and no device line.
        let cid = sim.set_card(&extra, CardKind::R, Deliver::Poll, vec!["Deploy"]);
        assert_eq!(cid, CardId(1));
        assert!(sim.puts().is_empty());
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Persist(_))));
        // Everyone idle again: the question finds a slot and the card is re-id'd.
        for id in &ids {
            sim.api(
                UID,
                SessionId(1),
                Request::AgentUpdate(crate::AgentUpdate {
                    agent: id.clone(),
                    state: Some(AgentState::Idle),
                    title: None,
                    title_source: None,
                    since: None,
                    name: None,
                    meta: None, task: None, subagents: None,
                }),
            )
            .unwrap();
        }
        sim.set_card(&ids[0], CardKind::Q, Deliver::Poll, vec!["x"]);
        let cid = sim.set_card(&extra, CardKind::Q, Deliver::Poll, vec!["Yes"]);
        let slot = sim.slot_of(&extra).unwrap();
        assert_ne!(slot, Slot(1), "never the one with a pending card");
        assert_eq!(cid, CardId(1), "first id of the freed slot");
        assert!(sim.puts().contains(&Key::Card(slot, 1)));
        let a = &sim.store.snapshot().agents[&extra];
        assert_eq!(a.title_source, TitleSource::SessionName);
    }

    #[test]
    fn a_snooze_repush_keeps_the_card_id_and_a_focus_card_is_never_resent() {
        let mut sim = Sim::new();
        let id = adapter_agent(&mut sim);
        for n in 1..=3u16 {
            sim.hold(permission(&id, None), HoldId(u64::from(n)))
                .unwrap();
            sim.drain();
            sim.apply(Command::Device(DeviceLine::Snooze { slot: 1, card: n }));
            let puts = sim.puts();
            assert_eq!(puts, vec![Key::Card(Slot(1), n)]);
            sim.apply(Command::Device(DeviceLine::Answer {
                slot: 1,
                card: n,
                choice: 0,
            }));
            for _ in 0..3 {
                sim.advance(Duration::from_secs(60));
                assert!(!sim.puts().contains(&Key::Card(Slot(1), n)));
            }
            assert!(!sim
                .store
                .project()
                .iter()
                .any(|op| matches!(op, DeviceOp::Put(Key::Card(..), _))));
        }
    }

    #[test]
    fn hold_written_and_gone_ignore_cards_that_are_not_holds() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
        sim.apply(Command::HoldWritten {
            agent: id.clone(),
            card: CardId(1),
        });
        sim.apply(Command::HoldGone {
            agent: id.clone(),
            card: CardId(1),
        });
        assert!(sim.events.is_empty());
        assert_eq!(status(&sim, &id), Some(CardStatus::Pending));
        assert_eq!(deliver_name(Deliver::Hold), "hold");
        // An unknown agent is ignored, never a panic.
        sim.apply(Command::HoldGone {
            agent: AgentId("ag_nobody".into()),
            card: CardId(1),
        });
        assert!(sim.events.is_empty());
    }

    /// Card ids are per slot, so two held cards share id 1: a hook dropping
    /// on one session must degrade that session's card, not the other's.
    #[test]
    fn holds_are_resolved_by_agent_and_card_not_by_card_id_alone() {
        let mut sim = Sim::new();
        let a = adapter_agent(&mut sim);
        let mut r = register("cohdl-web", Some("claude:2"));
        r.kind = crate::Kind::Claude;
        r.caps.answer = crate::AnswerCap::Hold;
        let res = sim
            .api(claude_adapter(), SessionId(100), Request::AgentRegister(r))
            .unwrap();
        let b = AgentId(res["agent"].as_str().unwrap().into());
        let ca = sim.hold(permission(&a, None), HoldId(1)).unwrap();
        let cb = sim.hold(permission(&b, None), HoldId(2)).unwrap();
        assert_eq!(
            (ca, cb),
            (CardId(1), CardId(1)),
            "the same id on both slots"
        );
        sim.drain();
        // B's hook client dies: B degrades, A stays pending.
        sim.apply(Command::HoldGone {
            agent: b.clone(),
            card: CardId(1),
        });
        assert_eq!(status(&sim, &a), Some(CardStatus::Pending));
        assert_eq!(
            status(&sim, &b),
            Some(CardStatus::Degraded { focused_at: None })
        );
        assert_eq!(sim.puts(), vec![Key::Card(Slot(2), 1)], "B's re-push only");
        // Both answering: B's response written clears B, A keeps waiting.
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::CardClear(CardClear {
                agent: b.clone(),
                card: CardId(1),
                delivered: false,
                reason: None,
            }),
        )
        .unwrap();
        sim.hold(permission(&b, None), HoldId(3)).unwrap();
        sim.drain();
        for (slot, card) in [(1, 1), (2, 2)] {
            sim.apply(Command::Device(DeviceLine::Answer {
                slot,
                card,
                choice: 0,
            }));
        }
        assert!(matches!(
            status(&sim, &a),
            Some(CardStatus::Answering { .. })
        ));
        sim.apply(Command::HoldWritten {
            agent: b.clone(),
            card: CardId(2),
        });
        assert_eq!(status(&sim, &b), None, "B delivered and cleared");
        assert!(
            matches!(status(&sim, &a), Some(CardStatus::Answering { .. })),
            "A untouched"
        );
        assert!(sim
            .effects
            .contains(&Effect::Device(DeviceOp::Control(Control::CardClear(
                Slot(2),
                2
            )))));
        // A release names its agent.
        sim.apply(Command::Device(DeviceLine::Snooze { slot: 1, card: 1 }));
        sim.apply(Command::HoldGone {
            agent: a.clone(),
            card: CardId(1),
        });
        assert!(sim.effects.iter().all(|e| !matches!(
            e,
            Effect::ReleaseHold { agent, .. } if *agent == b
        )));
    }

    // ---- host-side layout: the question body is wrapped here and sent as a
    // window the deck scrolls (design "host does the layout").

    /// A card with a body of `text`. `card_set`'s origin is `custom`, so the
    /// body the deck shows is prefixed `» ` — the window has to be a window of
    /// *that*, which `card_body` is what guarantees.
    fn card_with_text(sim: &mut Sim, id: &AgentId, text: &str) -> CardId {
        let owner = sim.store.snapshot().agents[id].owner.clone();
        let mut c = card_set(id, CardKind::Q, Deliver::Poll, &["Yes", "No"]);
        c.text = text.into();
        let res = sim
            .api(owner.principal, owner.session, Request::CardSet(c))
            .expect("card.set");
        CardId(res["card"]["id"].as_u64().expect("card id") as u16)
    }

    fn doc(sim: &Sim, slot: Slot, kind: DocKind, id: u16) -> Option<(u16, u16, Vec<String>)> {
        match sim.line(Key::DocText(slot, kind, id)) {
            Some(HostLine::DocText {
                first,
                total,
                lines,
                ..
            }) => Some((first, total, lines)),
            _ => None,
        }
    }

    fn window(sim: &Sim, slot: Slot, card: CardId) -> Option<(u16, u16, Vec<String>)> {
        match sim.line(Key::DocText(slot, DocKind::Card, card.0)) {
            Some(HostLine::DocText {
                first,
                total,
                lines,
                ..
            }) => Some((first, total, lines)),
            _ => None,
        }
    }

    /// A body the deck can lay out itself costs nothing extra: no window is
    /// sent and the deck wraps its own `text`, exactly as before this existed.
    /// This is the compatibility requirement the design calls the most
    /// important one.
    #[test]
    fn a_short_question_is_sent_without_a_window() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = card_with_text(&mut sim, &id, "Ship it?");
        assert!(sim.puts().contains(&Key::Card(Slot(1), cid.0)));
        assert_eq!(window(&sim, Slot(1), cid), None);
    }

    /// The common case must not cost a round trip: the first window rides along
    /// with the card.
    #[test]
    fn a_long_question_arrives_with_its_first_window() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let body = "Claude wants to run a database migration against the staging \
                    cluster. This drops the sessions table and recreates it from \
                    the new schema, so every signed-in user is logged out and any \
                    row written since the last snapshot is lost. The migration \
                    cannot be rolled back once it starts. Allow it?";
        let cid = card_with_text(&mut sim, &id, body);
        let (first, total, lines) = window(&sim, Slot(1), cid).expect("a window");
        assert_eq!(first, 0, "the first window starts at the top");
        assert!(total > 3, "this body needs scrolling, total={total}");
        assert!(!lines.is_empty());
        assert!(
            usize::from(first) + lines.len() <= usize::from(total),
            "a window must not run past the body"
        );
        // And the card line itself is unchanged: an old deck still works.
        assert!(matches!(
            sim.line(Key::Card(Slot(1), cid.0)),
            Some(HostLine::Card { .. })
        ));
    }

    /// The window is a window of the body, not a paraphrase of it: every word
    /// comes back, in order, with no ellipsis. The old path cut the text twice
    /// — once at `card_text` on the wire and again at two lines on the screen —
    /// and this is the assertion that both cuts are gone.
    #[test]
    fn the_window_carries_the_bodys_own_words_in_order() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let body = "alpha bravo charlie delta echo foxtrot golf hotel india \
                    juliett kilo lima mike november oscar papa quebec romeo";
        let cid = card_with_text(&mut sim, &id, body);
        let (_, total, _) = window(&sim, Slot(1), cid).expect("a window");

        // Walk the whole body by asking for each window in turn.
        let mut seen: Vec<String> = Vec::new();
        let mut at = 0u16;
        while at < total {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Card,
                id: cid.0,
                first: at,
            }));
            let (first, _, lines) = window(&sim, Slot(1), cid).expect("a window");
            for (n, line) in lines.iter().enumerate() {
                let index = usize::from(first) + n;
                if index == seen.len() {
                    seen.push(line.clone());
                }
            }
            let next = u16::try_from(seen.len()).unwrap();
            assert!(next > at, "asking for line {at} must make progress");
            at = next;
        }
        assert_eq!(seen.len(), usize::from(total));

        let got: Vec<&str> = seen.iter().flat_map(|l| l.split_whitespace()).collect();
        let mut want: Vec<&str> = vec!["»"];
        want.extend(body.split_whitespace());
        assert_eq!(got, want, "no word lost, none added, order kept");
        assert!(!seen.concat().contains('\u{2026}'), "and no ellipsis");
    }

    /// The host serves the chunk **starting where the deck asked**.
    ///
    /// It used to centre the chunk on the requested line. That was overscan
    /// the deck did not need once it buffered a run of the body itself: it
    /// asks for the lines it is *missing*, so half a centred chunk would be
    /// lines it already had, and the buffer crept forward instead of filling.
    #[test]
    fn a_chunk_starts_where_the_deck_asked_for_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = card_with_text(&mut sim, &id, &"word ".repeat(200));
        let (_, total, _) = window(&sim, Slot(1), cid).expect("a window");
        let span = deck_proto::caps().doc_lines;

        for want in [0u16, 5, 20, total / 2] {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Card,
                id: cid.0,
                first: want,
            }));
            let (first, _, lines) = window(&sim, Slot(1), cid).expect("a window");
            assert_eq!(first, want, "asked for {want}, chunk started at {first}");
            assert!(!lines.is_empty());
            assert!(usize::from(first) + lines.len() <= usize::from(total));
        }

        // A request past the end still returns the tail rather than nothing,
        // so the deck can always reach the last line.
        sim.apply(Command::Device(DeviceLine::DocLines {
            slot: 1,
            doc: DocKind::Card,
            id: cid.0,
            first: total,
        }));
        let (first, _, lines) = window(&sim, Slot(1), cid).expect("a window");
        assert_eq!(
            usize::from(first) + lines.len(),
            usize::from(total),
            "the last chunk ends at the end of the body"
        );
        assert_eq!(
            usize::from(first),
            usize::from(total) - span.min(usize::from(total))
        );
    }

    /// A request near the end clamps instead of running past `total` — the deck
    /// would otherwise scroll to a line that does not exist, which is the one
    /// thing the wire refuses outright.
    #[test]
    fn a_window_request_near_the_end_clamps_inside_the_body() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = card_with_text(&mut sim, &id, &"word ".repeat(200));
        let (_, total, _) = window(&sim, Slot(1), cid).expect("a window");
        for first in [total - 1, total, u16::MAX] {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Card,
                id: cid.0,
                first,
            }));
            let (got, t, lines) = window(&sim, Slot(1), cid).expect("a window");
            assert_eq!(t, total);
            assert!(
                usize::from(got) + lines.len() <= usize::from(total),
                "asked {first}: window {got}+{} runs past {total}",
                lines.len()
            );
            assert!(!lines.is_empty(), "asked {first}: the lane must not blank");
        }
    }

    /// The body under the window changed, so the request is about a text that
    /// no longer exists. Dropped, and not as an error: it is an ordinary race
    /// with a card being replaced, not a lost action.
    #[test]
    fn a_window_request_for_a_stale_card_is_dropped() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = card_with_text(&mut sim, &id, &"word ".repeat(200));
        sim.apply(Command::Device(DeviceLine::DocLines {
            slot: 1,
            doc: DocKind::Card,
            id: cid.0 + 7,
            first: 4,
        }));
        assert!(
            window(&sim, Slot(1), cid).is_none(),
            "no window for a stale id"
        );
        assert!(
            !sim.kinds().contains(&EventKind::Error),
            "and no error event: this is a race, not a fault"
        );
    }

    /// Every window the store emits has to survive the wire, or the outbox
    /// drops it and the operator scrolls into a hole.
    #[test]
    fn every_window_fits_one_line_of_the_wire() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        // A body of quoted shell, which is what a permission card actually
        // carries and the densest thing to escape.
        let body = (0..30)
            .map(|i| format!(r#"run "cmd{i}" --path "/a/b \"c\"" &&"#))
            .collect::<Vec<_>>()
            .join(" ");
        let cid = card_with_text(&mut sim, &id, &body);
        let (_, total, _) = window(&sim, Slot(1), cid).expect("a window");
        for first in 0..total {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Card,
                id: cid.0,
                first,
            }));
            let line = sim
                .line(Key::DocText(Slot(1), DocKind::Card, cid.0))
                .expect("a window");
            let encoded = deck_proto::encode(&line).expect("the window encodes");
            assert!(
                encoded.len() <= deck_proto::caps().line,
                "window at {first} is {} B",
                encoded.len()
            );
            // Trimming for length must never cost the deck the line it asked
            // for: it does not ask twice for the same `first`, so a window that
            // ends before the request leaves it stalled at the edge.
            let (got, _, lines) = window(&sim, Slot(1), cid).expect("a window");
            assert!(
                got <= first && usize::from(first) < usize::from(got) + lines.len(),
                "asked for line {first}, got {got}..{}",
                usize::from(got) + lines.len()
            );
        }
    }

    /// A card leaving the deck takes its window with it: a stale window is
    /// text from a question that has already been answered.
    #[test]
    fn clearing_a_card_takes_its_window_with_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let cid = card_with_text(&mut sim, &id, &"word ".repeat(200));
        assert!(window(&sim, Slot(1), cid).is_some());
        sim.api(
            UID,
            SessionId(1),
            Request::CardClear(crate::CardClear {
                agent: id.clone(),
                card: cid,
                delivered: true,
                reason: None,
            }),
        )
        .expect("card.clear");
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Device(DeviceOp::Remove(Key::DocText(Slot(1), DocKind::Card, _)))
        )));
    }

    // ---- the reply is a document too. `CAPS.reply` cut an agent's reply at
    // 200 bytes with an ellipsis; a real one measured 2347.

    fn set_reply(sim: &mut Sim, id: &AgentId, text: &str) {
        let owner = sim.store.snapshot().agents[id].owner.clone();
        sim.api(
            owner.principal,
            owner.session,
            Request::Reply(crate::ReplySet {
                message: None,
                agent: id.clone(),
                text: text.into(),
                speak: false,
                announce: None,
                lane: None,
            }),
        )
        .expect("agent.reply");
    }

    fn reply_rev(sim: &Sim, id: &AgentId) -> u16 {
        sim.store.snapshot().agents[id].chat_rev
    }

    /// A reply the deck can lay out itself costs nothing extra, exactly as
    /// before this existed.
    #[test]
    fn a_short_reply_gets_a_rich_window() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        set_reply(&mut sim, &id, "Done.");
        assert!(sim.puts().contains(&Key::Reply(Slot(1))));
        let (_, total, lines) = doc(&sim, Slot(1), DocKind::Reply, reply_rev(&sim, &id)).expect("rich window");
        assert_eq!(total, 3);
        assert_eq!(lines.last().unwrap(), "p0Done.");
    }

    /// The defect this replaces: a 2347-character reply reached the deck as
    /// 200 bytes and an ellipsis.
    #[test]
    fn a_long_reply_arrives_whole_as_a_window() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let body = "word ".repeat(470);
        set_reply(&mut sim, &id, &body);
        let rev = reply_rev(&sim, &id);
        let (first, total, lines) =
            doc(&sim, Slot(1), DocKind::Reply, rev).expect("a reply window");
        assert_eq!(usize::from(first) + lines.len(), usize::from(total), "opens at latest text");
        assert!(total > 20, "2347 chars is many screens, got {total} lines");
        assert!(!lines.is_empty());
        assert!(usize::from(first) + lines.len() <= usize::from(total));

        // The flat `reply` line is unchanged and still capped, so a deck that
        // never scrolls behaves as it always did.
        match sim.line(Key::Reply(Slot(1))) {
            Some(HostLine::Reply {
                text,
                rev: line_rev,
                ..
            }) => {
                assert_eq!(line_rev, rev, "the line carries the window's id");
                assert!(text.len() >= body.len().min(200));
            }
            other => panic!("no reply line: {other:?}"),
        }
    }

    /// Walking the whole reply by asking for each window in turn returns every
    /// word, in order, with no ellipsis anywhere.
    #[test]
    fn the_reply_windows_carry_every_word_in_order() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let body = (0..90)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        set_reply(&mut sim, &id, &body);
        let rev = reply_rev(&sim, &id);
        let (_, total, _) = doc(&sim, Slot(1), DocKind::Reply, rev).expect("a window");

        let mut seen: Vec<String> = Vec::new();
        let mut at = 0u16;
        while at < total {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Reply,
                id: rev,
                first: at,
            }));
            let (first, _, lines) = doc(&sim, Slot(1), DocKind::Reply, rev).expect("a window");
            for (n, line) in lines.iter().enumerate() {
                if usize::from(first) + n == seen.len() {
                    seen.push(line.clone());
                }
            }
            let next = u16::try_from(seen.len()).unwrap();
            assert!(next > at, "asking for line {at} must make progress");
            at = next;
        }
        let joined = seen.iter().filter_map(|s| s.strip_prefix("p0")).collect::<Vec<_>>().join(" ");
        assert_eq!(
            joined.split_whitespace().collect::<Vec<_>>(),
            body.split_whitespace().collect::<Vec<_>>()
        );
        assert!(!joined.contains('\u{2026}'));
    }

    /// A window for the reply before last is dropped: the deck would otherwise
    /// draw lines of a text it is no longer showing.
    #[test]
    fn history_retains_old_reply_windows_and_drops_wrong_epochs() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        set_reply(&mut sim, &id, &"word ".repeat(200));
        let old = reply_rev(&sim, &id);
        set_reply(&mut sim, &id, &"other ".repeat(200));
        let new = reply_rev(&sim, &id);
        assert_eq!(old, new, "appending preserves the reading epoch");

        sim.apply(Command::Device(DeviceLine::DocLines {
            slot: 1,
            doc: DocKind::Reply,
            id: old.wrapping_add(1),
            first: 4,
        }));
        assert_eq!(doc(&sim, Slot(1), DocKind::Reply, old), None);
        assert!(
            !sim.kinds().contains(&EventKind::Error),
            "a stale request is a race, not a fault"
        );
    }

    /// Whatever the deck asks for, the window that comes back has to survive
    /// the wire, and it has to contain the line that was asked for.
    #[test]
    fn every_reply_window_fits_the_wire_and_holds_what_was_asked_for() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        set_reply(&mut sim, &id, &"word ".repeat(300));
        let rev = reply_rev(&sim, &id);
        let (_, total, _) = doc(&sim, Slot(1), DocKind::Reply, rev).expect("a window");
        for first in 0..total {
            sim.apply(Command::Device(DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Reply,
                id: rev,
                first,
            }));
            let line = sim
                .line(Key::DocText(Slot(1), DocKind::Reply, rev))
                .expect("a window");
            let encoded = deck_proto::encode(&line).expect("the window encodes");
            assert!(
                encoded.len() <= deck_proto::caps().line,
                "{first}: {} B",
                encoded.len()
            );
            let (got, _, lines) = doc(&sim, Slot(1), DocKind::Reply, rev).expect("a window");
            assert!(
                got <= first && usize::from(first) < usize::from(got) + lines.len(),
                "asked for line {first}, got {got}..{}",
                usize::from(got) + lines.len()
            );
        }
    }

    /// The live transcript reads at the **end** of the body: what the operator
    /// wants to see is the newest speech, not the first thing they said. Only
    /// the final transcript is laid out — a partial is replaced every few
    /// hundred milliseconds, and on a link that measured 0.26 s per report a
    /// window per partial would always land stale.
    #[test]
    fn the_transcript_window_sits_at_the_end_of_what_was_said() {
        let body = (0..120)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let line = crate::store::doc_window(Slot(1), DocKind::Tr, 3, &body, u16::MAX, 1)
            .expect("a transcript window");
        let HostLine::DocText {
            doc,
            id,
            first,
            total,
            lines,
            ..
        } = line
        else {
            panic!("not a window")
        };
        assert_eq!((doc, id), (DocKind::Tr, 3));
        assert!(total > 1, "this body is meant to wrap");
        assert_eq!(
            usize::from(first) + lines.len(),
            usize::from(total),
            "the window ends where the speech does"
        );
    }

    /// The `text` size setting moves line breaks, so every window the deck is
    /// holding was measured for the wrong face the moment it changes. The host
    /// has to lay the documents out again and re-send them.
    #[test]
    fn changing_the_text_size_relays_out_every_document() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        set_reply(&mut sim, &id, &"word ".repeat(300));
        let rev = reply_rev(&sim, &id);
        let (_, small_total, small_lines) =
            doc(&sim, Slot(1), DocKind::Reply, rev).expect("a window");

        sim.apply(Command::Device(DeviceLine::Set {
            key: crate::SetKey::Textsize,
            value: 2,
        }));
        let (_, large_total, large_lines) = doc(&sim, Slot(1), DocKind::Reply, rev)
            .expect("the window is sent again at the new size");

        assert!(
            large_total > small_total,
            "bigger text is more lines: {small_total} -> {large_total}"
        );
        assert_ne!(
            small_lines, large_lines,
            "the same text broken for a different face must break differently"
        );
        match sim.line(Key::DocText(Slot(1), DocKind::Reply, rev)) {
            Some(HostLine::DocText { font, .. }) => {
                assert_eq!(
                    font,
                    deck_proto::DocFont::RichTitle,
                    "measured with the deck's rich face"
                )
            }
            other => panic!("no window: {other:?}"),
        }
    }

    /// A size the two sides disagreed about would break every line in a
    /// different place, so the host wraps with the face the **firmware's own**
    /// ramp names.
    #[test]
    fn the_host_wraps_with_the_face_the_deck_will_draw() {
        for (size, font) in [
            (0u8, crate::DocFont::Row),
            (1, crate::DocFont::Text),
            (2, crate::DocFont::Title),
        ] {
            assert_eq!(deck_wrap::face_for_size(size), font, "size {size}");
        }
    }
}
