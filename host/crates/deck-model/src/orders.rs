//! Orders that did not reach their agent, and what the deck shows for them
//! (the companion design's "An order that could not be delivered shows on
//! the deck").
//!
//! Every order `op.order` dispatches gets an id, `or_<n>`, unique within
//! one run of the daemon: `order.dispatched`, the owner's `agent.order` and
//! `agent.wait`'s `{kind:"order"}` carry it. An order can fail at once (no
//! route, an open dialog, a gone owner), later on a route that reports back
//! (the inbox, Claude Channels; `no_route` when there is neither), in the
//! core (a connection gone or full, mail dropped unread), or at its owner,
//! which says so with `agent.order.result` ([`Store::order_outcome`]). Each
//! of them ends in [`Store::order_failed`]: `order.failed` on the bus, then
//! the deck.
//!
//! What the deck is given ([`Store::show_failure`]), on firmware 0.13.0:
//!
//! - **a card**, when the agent is on the deck and nothing on it waits for
//!   the operator: no card, only an informational one (`deliver: none`),
//!   or a `focus` card the operator already pressed, which has left the
//!   deck. It is an ordinary Ready card the daemon made in the deck's
//!   language, so it goes the way every Ready card does — at the agent's
//!   next state change or after `ready_ttl_s` — and its `card.set` says
//!   `host:"failure"` with the `reason`. A card that waits for an answer
//!   (a question, a hold) is never replaced: that would retire it and
//!   release its hold. A replaced card that stood for a dialog still open
//!   in the session lends the failure card its origin, so guard 2 still
//!   refuses an order into that dialog.
//! - **the deck's own two-second toast** otherwise, when nothing was sent,
//!   the link is live and no press is open or finishing. Its words are the
//!   firmware's: "Transcription failed / Nothing was sent. Try again." A
//!   press-less failed `transcript` line ends a held press and clears a
//!   pending one on the deck, so it waits for neither, and an order that
//!   may have arrived never says that nothing was sent.
//! - **nothing** else; the bus still has the failure.
//!
//! The same rule serves any failure the core has to put in front of the
//! operator: [`Store::fail_on_deck`] takes the agent, a reason word and the
//! line in the deck's four languages ([`FailureLine`]). A hand-off that
//! could not happen is one (`crate::handoff::HandoffFailure`).
use crate::api::{ApiError, ApiResult};
use crate::{
    AgentId, CardKind, CardSet, Control, Deliver, DeviceOp, Effect, Event, EventKind, OrderOutcome,
    Origin, OriginKind, Principal, SessionId, SessionState, Slot, Snapshot, Store,
};
use std::time::{Duration, Instant};

/// How long `agent.order.result` may report on an order.
pub const ORDER_RESULT_WINDOW: Duration = Duration::from_secs(600);
/// How many orders wait for a report at once; past it the oldest goes.
pub const PENDING_ORDERS_MAX: usize = 128;
/// The `host` word on a `card.set` the daemon made to show a failure.
pub const HOST_FAILURE: &str = "failure";
/// The one route that cannot tell "not delivered" from "delivered but not
/// confirmed" (`claude_channel`'s `Delivery::Uncertain`).
const UNCONFIRMED_REASON: &str = "channel_delivery_uncertain";
/// A reason no route named.
const UNDELIVERABLE: &str = "undeliverable";

/// What the deck says about something the operator asked for that did not
/// happen, in each of the deck's languages.
///
/// The firmware shows host text as it comes, so the daemon picks the
/// language ([`Store::deck_language`]), as it does for the care test card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FailureLine {
    /// English, 简体中文, 繁體中文 and 日本語: the order of the deck's
    /// `language` setting.
    pub text: [&'static str; 4],
    /// True when nothing reached the agent. Only then may firmware 0.13.0's
    /// fixed toast, "Nothing was sent. Try again.", stand in for the card.
    pub nothing_sent: bool,
}

/// An order that never reached its agent. Every word is the firmware's own
/// (`Not delivered`, `Nothing was sent. Try again.`); Japanese keeps only the
/// second sentence, which already begins 未送信.
pub const NOT_DELIVERED: FailureLine = FailureLine {
    text: [
        "Not delivered. Nothing was sent. Try again.",
        "未送达。未发送任何内容，请重试。",
        "未送達。未傳送任何內容，請重試。",
        "未送信です。再試行してください。",
    ],
    nothing_sent: true,
};

/// An order Claude Channels took and could not confirm: it may have arrived,
/// so trying again could send it twice.
pub const UNCONFIRMED: FailureLine = FailureLine {
    text: [
        "Delivery not confirmed. Check the session before you try again.",
        "未能确认送达。请先查看会话，再决定是否重试。",
        "未能確認送達。請先查看工作階段，再決定是否重試。",
        "送信を確認できませんでした。再試行する前にセッションを確認してください。",
    ],
    nothing_sent: false,
};

/// What the deck was given for a failure: the `shown` of `order.failed` and
/// of `agent.order.result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A host-made Ready card on the agent.
    Card,
    /// The deck's two-second toast.
    Toast,
    /// Nothing: the bus has the failure.
    Nothing,
}

impl Shown {
    /// The word `order.failed` and `agent.order.result` carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Shown::Card => "card",
            Shown::Toast => "toast",
            Shown::Nothing => "none",
        }
    }
}

/// An order handed to its owner by `push` or `poll`, which
/// `agent.order.result` may still report on.
#[derive(Debug)]
pub(crate) struct PendingOrder {
    id: String,
    agent: AgentId,
    /// Who dictated or typed it: `order.failed` is published for them.
    principal: Principal,
    at: Instant,
}

/// The deck's language, as an index into [`FailureLine::text`] and
/// [`crate::handoff::OPEN_APP`]: its setting, or English on a deck whose
/// hello said it has no language setting (it shows nothing else). The care
/// test card picks its words the same way (`deckd::care::test::Language::of`).
pub(crate) fn language_of(snapshot: &Snapshot) -> usize {
    let device = &snapshot.device;
    if device.fw.is_some() && !device.language_supported {
        return 0;
    }
    usize::from(snapshot.settings.language.min(3))
}

/// `reason` is a word, never a sentence or content: `^[a-z0-9_]{1,32}$`.
fn is_reason_word(reason: &str) -> bool {
    (1..=32).contains(&reason.len())
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Store {
    /// The next order's id. An order handed to its owner is remembered for
    /// `agent.order.result` (the window and the cap bound how many).
    pub(crate) fn next_order(
        &mut self,
        agent: &AgentId,
        principal: &Principal,
        to_owner: bool,
    ) -> String {
        self.order_seq += 1;
        let id = format!("or_{}", self.order_seq);
        if to_owner {
            self.prune_pending_orders();
            if self.pending_orders.len() >= PENDING_ORDERS_MAX {
                self.pending_orders.pop_front();
            }
            self.pending_orders.push_back(PendingOrder {
                id: id.clone(),
                agent: agent.clone(),
                principal: principal.clone(),
                at: self.now,
            });
        }
        id
    }

    fn prune_pending_orders(&mut self) {
        let now = self.now;
        self.pending_orders
            .retain(|o| now.saturating_duration_since(o.at) < ORDER_RESULT_WINDOW);
    }

    /// `agent`'s order `id`, taken out of the pending list while it may
    /// still be reported on.
    fn take_pending_order(&mut self, agent: &AgentId, id: &str) -> Option<PendingOrder> {
        self.prune_pending_orders();
        let at = self
            .pending_orders
            .iter()
            .position(|o| o.id == id && o.agent == *agent)?;
        self.pending_orders.remove(at)
    }

    /// An order that did not reach `agent` (`None`: no agent was named):
    /// `order.failed`, then the deck ([`Store::show_failure`]).
    ///
    /// `text` is the words, for a failure that came back after the
    /// dispatch already said them (§5.5); `order` is the id, when the route
    /// that failed carried it. The event is published for whoever
    /// dispatched the order while it is pending, else for `principal`.
    /// `by_owner` marks a failure the agent's owner reported.
    pub(crate) fn order_failed(
        &mut self,
        principal: Principal,
        agent: Option<&AgentId>,
        reason: &str,
        text: Option<&str>,
        order: Option<&str>,
        by_owner: bool,
    ) -> Shown {
        // Reported once: a failure the core or a route found is not the
        // owner's to report again.
        let pending = order.and_then(|id| {
            let at = self.pending_orders.iter().position(|o| o.id == id)?;
            self.pending_orders.remove(at)
        });
        let principal = pending.map_or(principal, |o| o.principal);
        let line = if reason == UNCONFIRMED_REASON {
            UNCONFIRMED
        } else {
            NOT_DELIVERED
        };
        let shown = self.failure_shown(agent, &line);
        let mut data = serde_json::json!({
            "agent": agent, "reason": reason, "text_kept": true, "shown": shown.as_str(),
        });
        if let Some(text) = text {
            data["text"] = text.into();
        }
        if let Some(order) = order {
            data["order"] = order.into();
        }
        if by_owner {
            data["by"] = "owner".into();
        }
        self.emit(EventKind::OrderFailed, principal, data);
        self.show(shown, agent, reason, &line);
        shown
    }

    /// Show on the deck that something the operator asked for did not
    /// happen: a host-made card on `agent` when nothing of it waits on the
    /// deck, else the deck's own toast when `line` may say that nothing was
    /// sent, else nothing (the module's rule). `reason` is one word for
    /// the record.
    pub(crate) fn show_failure(
        &mut self,
        agent: Option<&AgentId>,
        reason: &str,
        line: FailureLine,
    ) -> Shown {
        let shown = self.failure_shown(agent, &line);
        self.show(shown, agent, reason, &line);
        shown
    }

    /// The door for the core, like [`Store::call`]: [`Store::show_failure`]
    /// with the events and effects it produced. A failure the core finds
    /// itself (a hand-off that cannot happen, say) reaches the deck the way
    /// a failed order does.
    pub fn fail_on_deck(
        &mut self,
        agent: Option<&AgentId>,
        reason: &str,
        line: FailureLine,
        now: Instant,
    ) -> (Shown, Vec<Event>, Vec<Effect>) {
        self.now = now;
        let shown = self.show_failure(agent, reason, line);
        let (events, effects) = self.take();
        (shown, events, effects)
    }

    /// The rule, decided before anything is shown so `order.failed` can say
    /// what was.
    fn failure_shown(&self, agent: Option<&AgentId>, line: &FailureLine) -> Shown {
        if let Some(agent) = agent.and_then(|id| self.snapshot.agents.get(id)) {
            // A paused source is off the deck, and nothing of it shows.
            if self.is_muted(agent) {
                return Shown::Nothing;
            }
            // A card waits while the deck shows it and it takes an answer.
            // A pressed `focus` card has left the deck, the press being its
            // answer, so a failure may take its place there; the failure
            // card keeps the dialog it stood for (`failure_card`).
            let waits = agent
                .card
                .as_ref()
                .is_some_and(|c| c.deliver != Deliver::None && c.status.on_device());
            if agent.slot.is_some() && !waits {
                return Shown::Card;
            }
        }
        // The toast's line carries no press, and a press-less failed line
        // ends the press the deck holds and drops the one it is waiting on.
        let no_press =
            self.snapshot.voice.ptt.is_none() && self.finalizing.iter().all(|f| f.ptt.is_none());
        if line.nothing_sent && self.snapshot.device.session == SessionState::Live && no_press {
            Shown::Toast
        } else {
            Shown::Nothing
        }
    }

    fn show(&mut self, shown: Shown, agent: Option<&AgentId>, reason: &str, line: &FailureLine) {
        let text = line.text[self.deck_language()];
        match (shown, agent) {
            (Shown::Card, Some(id)) => self.failure_card(id, reason, text),
            (Shown::Toast, _) => {
                let slot = agent
                    .and_then(|id| self.snapshot.agents.get(id))
                    .and_then(|a| a.slot);
                self.device_op(DeviceOp::Control(Control::Transcript {
                    slot: slot.unwrap_or(Slot(0)),
                    text: text.to_string(),
                    last: true,
                    press: None,
                    failed: true,
                    channel_tip: false,
                    rec: false,
                }));
            }
            _ => {}
        }
    }

    /// The host-made card: Ready, informational, no steps, as Claude's own
    /// Stop card is, so ENTER on it is the deck's own and sends nothing.
    ///
    /// A card it replaces may stand for a dialog still open in the session:
    /// a degraded hold whose `Open app` press could not hand off, or an
    /// informational card about a dialog. Guard 2 reads that off the card
    /// (`Card::is_open_dialog`), and a post into an open dialog cancels it,
    /// so the failure card keeps that card's origin: an order is still
    /// refused until the session moves on and the Ready card goes with its
    /// next state change.
    fn failure_card(&mut self, id: &AgentId, reason: &str, text: &str) {
        let principal = self.principal_of(id);
        let origin = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.card.as_ref())
            .filter(|c| c.is_open_dialog())
            .map_or(
                Origin {
                    kind: OriginKind::Ready,
                    tool: None,
                    request_id: None,
                    group: None,
                },
                |c| Origin {
                    kind: c.origin.kind,
                    tool: c.origin.tool.clone(),
                    request_id: None,
                    group: None,
                },
            );
        let replaced = self
            .snapshot
            .agents
            .get(id)
            .and_then(|a| a.card.as_ref())
            .map(|c| c.id);
        let set = self.card_set(
            &principal,
            CardSet {
                agent: id.clone(),
                id: None,
                kind: CardKind::R,
                text: text.to_string(),
                choices: Vec::new(),
                rec: None,
                free_text: false,
                deliver: Deliver::None,
                timeout_s: None,
                origin,
            },
            false,
        );
        debug_assert!(
            set.is_ok(),
            "a failure card is always a valid card: {set:?}"
        );
        // The card it replaced (an informational one, nothing waiting on it)
        // is still its owner's to clear: that clear is answered as done.
        if set.is_ok() {
            self.aux.entry(id.clone()).or_default().failure_replaced = replaced;
        }
        // Whoever owns the agent sees a card it did not make: say whose.
        if let Some(event) = self
            .events
            .iter_mut()
            .rev()
            .find(|e| e.kind == EventKind::CardSet && e.data["agent"] == id.0)
        {
            event.data["host"] = HOST_FAILURE.into();
            event.data["reason"] = reason.into();
        }
    }

    /// The deck's language, as an index into [`FailureLine::text`]
    /// ([`language_of`]).
    pub(crate) fn deck_language(&self) -> usize {
        language_of(&self.snapshot)
    }

    /// `agent.order.result` from the agent's owner.
    pub(crate) fn order_outcome(&mut self, session: SessionId, o: OrderOutcome) -> ApiResult {
        self.owned(&o.agent, session)?;
        if o.reason.as_deref().is_some_and(|r| !is_reason_word(r)) {
            return Err(ApiError::invalid("reason", "not_a_word"));
        }
        if o.ok {
            // Delivered: nothing to show, and nothing left to report.
            self.take_pending_order(&o.agent, &o.order);
            return Ok(
                serde_json::json!({"rev": self.snapshot.rev, "shown": Shown::Nothing.as_str()}),
            );
        }
        let pending = self
            .take_pending_order(&o.agent, &o.order)
            .ok_or_else(|| ApiError::not_found("order"))?;
        let reason = o.reason.as_deref().unwrap_or(UNDELIVERABLE);
        let shown = self.order_failed(
            pending.principal,
            Some(&o.agent),
            reason,
            None,
            Some(&pending.id),
            true,
        );
        Ok(serde_json::json!({"rev": self.snapshot.rev, "shown": shown.as_str()}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{card_set, claude_adapter, register, Sim, UID, UID2};
    use crate::{
        AgentState, AudioCmd, Card, CardStatus, Command, DeviceLine, HoldId, LinkEvent, OpOrder,
        OrderCap, OrderSource, PttCtx, Request,
    };

    fn order(agent: &AgentId, text: &str) -> Request {
        Request::OpOrder(OpOrder {
            agent: agent.clone(),
            text: text.into(),
            ctx: "talk".into(),
            source: OrderSource::Ptt,
            utterance: None,
        })
    }

    /// A virtual agent on `session` whose orders go the way `cap` says.
    fn agent_with(sim: &mut Sim, session: u64, name: &str, cap: OrderCap) -> AgentId {
        let mut r = register(name, Some(&format!("app:{name}")));
        r.caps.order = cap;
        let res = sim
            .api(UID, SessionId(session), Request::AgentRegister(r))
            .unwrap();
        AgentId(res["agent"].as_str().unwrap().into())
    }

    /// A Claude adapter agent: inject orders, held questions.
    fn adapter_agent(sim: &mut Sim) -> AgentId {
        let mut r = register("cohdl-api", Some("claude:1"));
        r.kind = crate::Kind::Claude;
        r.caps.answer = crate::AnswerCap::Hold;
        r.caps.order = OrderCap::Inject;
        let res = sim
            .api(claude_adapter(), SessionId(100), Request::AgentRegister(r))
            .unwrap();
        AgentId(res["agent"].as_str().unwrap().into())
    }

    /// A held permission card on the adapter's agent.
    fn hold_permission(sim: &mut Sim, agent: &AgentId) {
        let mut c = card_set(agent, CardKind::Q, Deliver::Hold, &["Allow", "Deny"]);
        c.origin = Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: None,
            group: None,
        };
        sim.hold(c, HoldId(1)).unwrap();
        sim.drain();
    }

    fn card(sim: &Sim, agent: &AgentId) -> Option<Card> {
        sim.store.snapshot().agents[agent].card.clone()
    }

    /// The toast lines of the last step: `(slot, text, press)`.
    fn toasts(sim: &Sim) -> Vec<(Slot, String, Option<u16>)> {
        sim.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Device(DeviceOp::Control(Control::Transcript {
                    slot,
                    text,
                    press,
                    failed: true,
                    last: true,
                    ..
                })) => Some((*slot, text.clone(), *press)),
                _ => None,
            })
            .collect()
    }

    fn failed(sim: &Sim) -> serde_json::Value {
        sim.events
            .iter()
            .find(|e| e.kind == EventKind::OrderFailed)
            .map(|e| e.data.clone())
            .expect("order.failed")
    }

    #[test]
    fn a_refused_order_to_an_agent_without_a_card_gets_a_host_made_ready_card() {
        let mut sim = Sim::new();
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        let err = sim
            .api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert_eq!(err.code, ApiError::UNDELIVERABLE);
        assert_eq!(err.data["reason"], "no_route");
        assert_eq!(
            sim.kinds(),
            vec![EventKind::OrderFailed, EventKind::CardSet]
        );
        assert_eq!(sim.events[0].data["shown"], "card");
        assert_eq!(sim.events[1].data["host"], HOST_FAILURE);
        assert_eq!(sim.events[1].data["reason"], "no_route");
        let card = card(&sim, &bot).expect("the failure card");
        assert_eq!(card.kind, CardKind::R);
        assert_eq!(card.deliver, Deliver::None);
        assert_eq!(card.origin.kind, OriginKind::Ready);
        assert!(card.choices.is_empty());
        assert_eq!(card.text.as_deref(), Some(NOT_DELIVERED.text[0]));
        assert!(
            !sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Audio(AudioCmd::Chime { .. }))),
            "a failure is not a question: no chime"
        );
        assert!(
            sim.puts().iter().any(|k| matches!(k, crate::Key::Card(..))),
            "the card goes to the deck"
        );
        assert_eq!(
            sim.store.snapshot().agents[&bot].state,
            AgentState::Idle,
            "a Ready card does not touch the state"
        );
    }

    #[test]
    fn the_failure_card_goes_like_any_ready_card() {
        let mut sim = Sim::with(|cfg| cfg.ready_ttl_s = 20);
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert!(card(&sim, &bot).is_some());
        sim.advance(Duration::from_secs(21));
        assert_eq!(sim.kinds(), vec![EventKind::CardExpired]);
        assert!(card(&sim, &bot).is_none(), "ready_ttl_s");
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        sim.api(
            UID,
            SessionId(1),
            Request::AgentUpdate(crate::AgentUpdate {
                agent: bot.clone(),
                state: Some(AgentState::Working),
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None,
                task: None,
                subagents: None,
            }),
        )
        .unwrap();
        assert!(card(&sim, &bot).is_none(), "the next state change");
    }

    #[test]
    fn an_informational_card_gives_way_to_the_failure_card() {
        let mut sim = Sim::new();
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        let mut ready = card_set(&bot, CardKind::R, Deliver::None, &[]);
        ready.text = "Done: the build is green.".into();
        let old = sim.api(UID, SessionId(1), Request::CardSet(ready)).unwrap()["card"]["id"]
            .as_u64()
            .unwrap();
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert_eq!(
            sim.kinds(),
            vec![
                EventKind::OrderFailed,
                EventKind::CardCleared,
                EventKind::CardSet
            ]
        );
        assert_eq!(sim.events[1].data["card"], old);
        assert_eq!(sim.events[1].data["reason"], "replaced");
        assert_eq!(
            card(&sim, &bot).unwrap().text.as_deref(),
            Some(NOT_DELIVERED.text[0])
        );
        // Its owner clears the card it set, as it always could: done, as for
        // a card already gone, and the failure card stays on the deck.
        let cleared = sim.api(
            UID,
            SessionId(1),
            Request::CardClear(crate::CardClear {
                agent: bot.clone(),
                card: crate::CardId(u16::try_from(old).unwrap()),
                delivered: false,
                reason: None,
            }),
        );
        assert!(cleared.is_ok(), "{cleared:?}");
        assert_eq!(
            card(&sim, &bot).unwrap().text.as_deref(),
            Some(NOT_DELIVERED.text[0])
        );
        // A card the owner never had is still a conflict.
        let stale = sim.api(
            UID,
            SessionId(1),
            Request::CardClear(crate::CardClear {
                agent: bot.clone(),
                card: crate::CardId(u16::try_from(old).unwrap() + 100),
                delivered: false,
                reason: None,
            }),
        );
        assert_eq!(stale.unwrap_err().code, ApiError::CONFLICT);
    }

    #[test]
    fn a_failure_never_replaces_or_releases_a_pending_card() {
        let mut sim = Sim::new();
        sim.link_live(Some(0));
        let adapter = adapter_agent(&mut sim);
        hold_permission(&mut sim, &adapter);
        let before = card(&sim, &adapter).unwrap();
        let err = sim
            .api(UID, SessionId(2), order(&adapter, "yes"))
            .unwrap_err();
        assert_eq!(err.data["reason"], "dialog_open");
        assert_eq!(sim.kinds(), vec![EventKind::OrderFailed]);
        assert_eq!(failed(&sim)["shown"], "toast");
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ReleaseHold { .. })));
        let after = card(&sim, &adapter).unwrap();
        assert_eq!((after.id, after.status), (before.id, CardStatus::Pending));
        assert_eq!(
            toasts(&sim),
            vec![(
                sim.slot_of(&adapter).unwrap(),
                NOT_DELIVERED.text[0].into(),
                None
            )]
        );
    }

    #[test]
    fn the_toast_waits_out_a_live_or_finalizing_press() {
        let mut sim = Sim::new();
        sim.link_live(Some(0));
        let gone = AgentId("ag_gone".into());
        let result = |sim: &mut Sim| {
            sim.apply(Command::OrderResult {
                principal: crate::Principal::Device,
                agent: gone.clone(),
                text: "deploy".into(),
                ok: false,
                reason: Some("inbox_unreachable".into()),
                order: Some("or_9".into()),
            });
        };
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Poll);
        let slot = sim.slot_of(&bot).unwrap().0;
        // Held: the capture is open.
        sim.apply(Command::Device(DeviceLine::Ptt {
            act: 1,
            slot,
            ctx: PttCtx::Talk,
            card: None,
            abort: false,
            press: Some(4),
        }));
        result(&mut sim);
        assert_eq!(sim.kinds(), vec![EventKind::OrderFailed]);
        assert_eq!(failed(&sim)["shown"], "none");
        assert!(toasts(&sim).is_empty());
        // Released: the words are still on their way.
        sim.apply(Command::Device(DeviceLine::Ptt {
            act: 0,
            slot,
            ctx: PttCtx::Talk,
            card: None,
            abort: false,
            press: Some(4),
        }));
        assert!(sim.store.is_finalizing());
        result(&mut sim);
        assert!(toasts(&sim).is_empty());
        // The final lands: the deck is quiet again, and a failure may toast.
        sim.apply(Command::Voice(crate::VoiceIn::Final {
            text: String::new(),
        }));
        result(&mut sim);
        assert_eq!(toasts(&sim).len(), 1);
    }

    #[test]
    fn no_toast_without_a_live_deck() {
        let mut sim = Sim::new();
        let gone = AgentId("ag_gone".into());
        sim.apply(Command::OrderResult {
            principal: crate::Principal::Device,
            agent: gone.clone(),
            text: "deploy".into(),
            ok: false,
            reason: None,
            order: None,
        });
        assert_eq!(sim.kinds(), vec![EventKind::OrderFailed]);
        assert_eq!(failed(&sim)["reason"], UNDELIVERABLE);
        assert_eq!(failed(&sim)["shown"], "none");
        assert!(sim.effects.is_empty());
        // A deck that answers hello but has not gone live is not live.
        sim.apply(Command::Link(LinkEvent::Linked));
        sim.apply(Command::OrderResult {
            principal: crate::Principal::Device,
            agent: gone,
            text: "deploy".into(),
            ok: false,
            reason: None,
            order: None,
        });
        assert!(toasts(&sim).is_empty());
    }

    #[test]
    fn an_unconfirmed_channel_delivery_never_says_nothing_was_sent() {
        let mut sim = Sim::new();
        sim.link_live(Some(0));
        let adapter = adapter_agent(&mut sim);
        let unconfirmed = |sim: &mut Sim| {
            sim.apply(Command::OrderResult {
                principal: crate::Principal::Device,
                agent: adapter.clone(),
                text: "deploy".into(),
                ok: false,
                reason: Some(UNCONFIRMED_REASON.into()),
                order: Some("or_1".into()),
            });
        };
        hold_permission(&mut sim, &adapter);
        unconfirmed(&mut sim);
        assert_eq!(sim.kinds(), vec![EventKind::OrderFailed]);
        assert_eq!(failed(&sim)["shown"], "none");
        assert!(toasts(&sim).is_empty(), "the toast says nothing was sent");
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::CardClear(crate::CardClear {
                agent: adapter.clone(),
                card: card(&sim, &adapter).unwrap().id,
                delivered: false,
                reason: None,
            }),
        )
        .unwrap();
        unconfirmed(&mut sim);
        assert_eq!(failed(&sim)["shown"], "card");
        assert_eq!(
            card(&sim, &adapter).unwrap().text.as_deref(),
            Some(UNCONFIRMED.text[0])
        );
    }

    #[test]
    fn the_failure_card_speaks_the_decks_language() {
        for (language, want) in [(1, 1), (2, 2), (3, 3), (0, 0)] {
            let mut sim = Sim::new();
            sim.link_live(Some(language));
            let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
            sim.api(UID, SessionId(2), order(&bot, "deploy"))
                .unwrap_err();
            assert_eq!(
                card(&sim, &bot).unwrap().text.as_deref(),
                Some(NOT_DELIVERED.text[want]),
                "language {language}"
            );
        }
        // A deck whose hello names no language shows English, whatever the
        // Mac's setting.
        let mut sim = Sim::with(|cfg| cfg.persisted.settings.language = 3);
        sim.link_live(None);
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert_eq!(
            card(&sim, &bot).unwrap().text.as_deref(),
            Some(NOT_DELIVERED.text[0])
        );
        // No deck yet: the Mac's setting, which the next hello shares.
        let mut sim = Sim::with(|cfg| cfg.persisted.settings.language = 2);
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert_eq!(
            card(&sim, &bot).unwrap().text.as_deref(),
            Some(NOT_DELIVERED.text[2])
        );
    }

    #[test]
    fn every_failure_line_fits_the_deck() {
        let caps = deck_proto::caps();
        let handoff = [
            crate::HandoffFailure::NoApp,
            crate::HandoffFailure::NotRunning,
            crate::HandoffFailure::Refused,
        ]
        .map(crate::HandoffFailure::line);
        for line in [NOT_DELIVERED, UNCONFIRMED].into_iter().chain(handoff) {
            for text in line.text {
                assert!(text.len() <= caps.card_text, "{text}");
                if line.nothing_sent {
                    assert!(text.len() <= caps.transcript, "{text}");
                }
            }
        }
    }

    #[test]
    fn an_order_to_an_orphaned_owner_fails_at_once() {
        let mut sim = Sim::new();
        let pusher = agent_with(&mut sim, 1, "pusher", OrderCap::Push);
        let poller = agent_with(&mut sim, 1, "poller", OrderCap::Poll);
        sim.apply(Command::SessionClosed(SessionId(1)));
        for agent in [&pusher, &poller] {
            let err = sim
                .api(UID, SessionId(2), order(agent, "deploy"))
                .unwrap_err();
            assert_eq!(err.code, ApiError::UNDELIVERABLE);
            assert_eq!(err.data["reason"], "owner_gone");
            let kinds = sim.kinds();
            assert_eq!(kinds[0], EventKind::OrderFailed);
            assert!(!kinds.contains(&EventKind::OrderDispatched));
            assert!(!kinds.contains(&EventKind::AgentOrder));
            assert_eq!(failed(&sim)["reason"], "owner_gone");
        }
    }

    #[test]
    fn push_and_poll_orders_carry_an_order_id() {
        let mut sim = Sim::new();
        let pusher = agent_with(&mut sim, 1, "pusher", OrderCap::Push);
        let poller = agent_with(&mut sim, 1, "poller", OrderCap::Poll);
        for (agent, id) in [(&pusher, "or_1"), (&poller, "or_2")] {
            let res = sim.api(UID, SessionId(2), order(agent, "deploy")).unwrap();
            assert_eq!(res["order"], id);
            assert_eq!(
                sim.kinds(),
                vec![EventKind::OrderDispatched, EventKind::AgentOrder]
            );
            assert_eq!(sim.events[0].data["order"], id);
            assert_eq!(sim.events[1].data["order"], id);
        }
        assert_eq!(sim.store.pending_orders.len(), 2);
        // An inject order has an id too, on its effect, and waits for no
        // report: the route reports back itself.
        let adapter = adapter_agent(&mut sim);
        let res = sim
            .api(UID, SessionId(2), order(&adapter, "fix it"))
            .unwrap();
        assert_eq!(res["order"], "or_3");
        assert_eq!(sim.events[0].data["order"], "or_3");
        assert!(sim
            .effects
            .contains(&Effect::Desktop(crate::DesktopCmd::Inject {
                agent: adapter,
                text: "fix it".into(),
                order: "or_3".into(),
            })));
        assert_eq!(sim.store.pending_orders.len(), 2);
    }

    fn outcome(agent: &AgentId, order: &str, ok: bool, reason: Option<&str>) -> Request {
        Request::OrderOutcome(OrderOutcome {
            agent: agent.clone(),
            order: order.into(),
            ok,
            reason: reason.map(String::from),
        })
    }

    #[test]
    fn agent_order_result_reports_once_and_only_for_the_owner() {
        let mut sim = Sim::new();
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Push);
        let first = sim.api(UID2, SessionId(2), order(&bot, "deploy")).unwrap()["order"]
            .as_str()
            .unwrap()
            .to_string();

        // Another session may not report on it.
        let err = sim
            .api(UID, SessionId(3), outcome(&bot, &first, false, None))
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        // A reason is a word.
        let err = sim
            .api(
                UID,
                SessionId(1),
                outcome(&bot, &first, false, Some("Bad Word")),
            )
            .unwrap_err();
        assert_eq!(
            (err.code, err.data["field"].as_str()),
            (ApiError::INVALID, Some("reason"))
        );
        let long = "x".repeat(33);
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, &first, false, Some(&long)))
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        // An unknown order.
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, "or_99", false, None))
            .unwrap_err();
        assert_eq!(
            (err.code, err.data["what"].as_str()),
            (ApiError::NOT_FOUND, Some("order"))
        );

        // The owner reports it: the deck shows it, for whoever dictated it.
        let res = sim
            .api(
                UID,
                SessionId(1),
                outcome(&bot, &first, false, Some("backend_refused")),
            )
            .unwrap();
        assert_eq!(res["shown"], "card");
        assert_eq!(
            sim.kinds(),
            vec![EventKind::OrderFailed, EventKind::CardSet]
        );
        let event = &sim.events[0];
        assert_eq!(event.principal, UID2);
        assert_eq!(event.data["order"], first.as_str());
        assert_eq!(event.data["by"], "owner");
        assert_eq!(event.data["reason"], "backend_refused");
        assert_eq!(event.data["shown"], "card");
        // Once.
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, &first, false, None))
            .unwrap_err();
        assert_eq!(err.data["what"], "order");

        // `ok: true` changes nothing, known or not, and ends the order.
        let second = sim.api(UID, SessionId(2), order(&bot, "and test")).unwrap()["order"]
            .as_str()
            .unwrap()
            .to_string();
        let res = sim
            .api(UID, SessionId(1), outcome(&bot, &second, true, None))
            .unwrap();
        assert_eq!(res["shown"], "none");
        assert!(sim.events.is_empty());
        let res = sim
            .api(UID, SessionId(1), outcome(&bot, "or_99", true, None))
            .unwrap();
        assert_eq!(res["shown"], "none");
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, &second, false, None))
            .unwrap_err();
        assert_eq!(err.data["what"], "order");

        // Ten minutes on, an order can no longer be reported on.
        let third = sim.api(UID, SessionId(2), order(&bot, "ship")).unwrap()["order"]
            .as_str()
            .unwrap()
            .to_string();
        sim.advance(ORDER_RESULT_WINDOW);
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, &third, false, None))
            .unwrap_err();
        assert_eq!(err.data["what"], "order");
        // `undeliverable` when the owner names no reason.
        let fourth = sim.api(UID, SessionId(2), order(&bot, "ship")).unwrap()["order"]
            .as_str()
            .unwrap()
            .to_string();
        sim.api(UID, SessionId(1), outcome(&bot, &fourth, false, None))
            .unwrap();
        assert_eq!(failed(&sim)["reason"], UNDELIVERABLE);
    }

    #[test]
    fn a_failure_found_elsewhere_cannot_be_reported_again() {
        let mut sim = Sim::new();
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Poll);
        let id = sim.api(UID, SessionId(2), order(&bot, "deploy")).unwrap()["order"]
            .as_str()
            .unwrap()
            .to_string();
        sim.apply(Command::OrderResult {
            principal: crate::Principal::Uid(0),
            agent: bot.clone(),
            text: "deploy".into(),
            ok: false,
            reason: Some("expired".into()),
            order: Some(id.clone()),
        });
        let event = &sim.events[0];
        assert_eq!(event.kind, EventKind::OrderFailed);
        assert_eq!(event.principal, UID, "the dispatcher, not the core's guess");
        assert_eq!(event.data["order"], id.as_str());
        assert_eq!(event.data["text"], "deploy");
        assert!(event.data.get("by").is_none());
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, &id, false, None))
            .unwrap_err();
        assert_eq!(err.data["what"], "order");
    }

    #[test]
    fn the_pending_list_is_bounded() {
        let mut sim = Sim::new();
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Poll);
        for _ in 0..PENDING_ORDERS_MAX + 2 {
            sim.api(UID, SessionId(2), order(&bot, "again")).unwrap();
        }
        assert_eq!(sim.store.pending_orders.len(), PENDING_ORDERS_MAX);
        let err = sim
            .api(UID, SessionId(1), outcome(&bot, "or_1", false, None))
            .unwrap_err();
        assert_eq!(err.data["what"], "order", "the oldest went first");
    }

    #[test]
    fn the_core_shows_its_own_failures_through_the_same_door() {
        let mut sim = Sim::new();
        sim.link_live(Some(3));
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Push);
        let line = FailureLine {
            text: ["One.", "一。", "一。", "一つ。"],
            nothing_sent: true,
        };
        let (shown, events, _) = sim
            .store
            .fail_on_deck(Some(&bot), "not_running", line, sim.t);
        assert_eq!(shown, Shown::Card);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::CardSet);
        assert_eq!(events[0].data["reason"], "not_running");
        assert_eq!(card(&sim, &bot).unwrap().text.as_deref(), Some("一つ。"));
        // Nobody named, or a line that may not say "nothing was sent".
        let (shown, events, effects) = sim.store.fail_on_deck(None, "no_app", line, sim.t);
        assert_eq!(shown, Shown::Toast);
        assert!(events.is_empty());
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Device(DeviceOp::Control(Control::Transcript {
                slot: Slot(0),
                failed: true,
                ..
            }))
        )));
        let quiet = FailureLine {
            nothing_sent: false,
            ..line
        };
        let (shown, _, effects) = sim.store.fail_on_deck(None, "no_app", quiet, sim.t);
        assert_eq!((shown, effects.len()), (Shown::Nothing, 0));
    }

    /// A hand-off that could not happen from a degraded hold. The press took
    /// the card off the deck, so the failure card takes its place there, in
    /// the deck's language. The dialog it stood for is still open in the
    /// session, so the failure card keeps its origin: an order is still
    /// refused `dialog_open` until the session moves on, and the card goes
    /// with the next state change like any Ready card.
    #[test]
    fn a_failed_hand_off_from_a_degraded_hold_shows_and_keeps_guard_two() {
        let mut sim = Sim::new();
        sim.link_live(Some(1));
        let adapter = adapter_agent(&mut sim);
        hold_permission(&mut sim, &adapter);
        let held = card(&sim, &adapter).unwrap().id;
        sim.apply(Command::HoldGone {
            agent: adapter.clone(),
            card: held,
        });
        let slot = sim.slot_of(&adapter).unwrap();
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: slot.0,
            card: held.0,
            choice: 0,
        }));
        assert!(sim
            .effects
            .contains(&Effect::Desktop(crate::DesktopCmd::Focus {
                agent: adapter.clone()
            })));
        let failure = crate::HandoffFailure::NoApp;
        let (shown, events, _) =
            sim.store
                .fail_on_deck(Some(&adapter), failure.reason(), failure.line(), sim.t);
        assert_eq!(shown, Shown::Card);
        let set = events
            .iter()
            .find(|e| e.kind == EventKind::CardSet)
            .expect("a host-made card");
        assert_eq!(
            (&set.data["host"], &set.data["reason"]),
            (&serde_json::json!("failure"), &serde_json::json!("no_app"))
        );
        let shown_card = card(&sim, &adapter).unwrap();
        assert_ne!(shown_card.id, held);
        assert_eq!(shown_card.kind, CardKind::R);
        assert_eq!(shown_card.text.as_deref(), Some(failure.line().text[1]));
        assert_eq!(shown_card.origin.kind, OriginKind::Permission);
        assert!(shown_card.is_open_dialog());
        // Guard 2 still holds.
        let err = sim
            .api(UID, SessionId(2), order(&adapter, "yes"))
            .unwrap_err();
        assert_eq!(err.data["reason"], "dialog_open");
        // The session moves on: the card goes, and with it the guard.
        sim.api(
            claude_adapter(),
            SessionId(100),
            Request::AgentUpdate(crate::AgentUpdate {
                agent: adapter.clone(),
                state: Some(AgentState::Working),
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None,
                task: None,
                subagents: None,
            }),
        )
        .unwrap();
        assert!(card(&sim, &adapter).is_none(), "the next state change");
    }

    /// An informational card with nothing open behind it gives way to a
    /// plain Ready failure card, as before.
    #[test]
    fn a_failure_card_is_ready_unless_a_dialog_stands_behind_it() {
        let mut sim = Sim::new();
        sim.link_live(Some(0));
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::Push);
        let failure = crate::HandoffFailure::NotRunning;
        let (shown, _, _) =
            sim.store
                .fail_on_deck(Some(&bot), failure.reason(), failure.line(), sim.t);
        assert_eq!(shown, Shown::Card);
        let shown_card = card(&sim, &bot).unwrap();
        assert_eq!(shown_card.origin.kind, OriginKind::Ready);
        assert!(!shown_card.is_open_dialog());
        assert_eq!(shown_card.text.as_deref(), Some(failure.line().text[0]));
    }

    #[test]
    fn a_paused_source_shows_nothing() {
        let mut sim = Sim::new();
        sim.link_live(Some(0));
        let bot = agent_with(&mut sim, 1, "bot", OrderCap::None);
        sim.mute(&[SessionId(1)]);
        sim.api(UID, SessionId(2), order(&bot, "deploy"))
            .unwrap_err();
        assert_eq!(failed(&sim)["shown"], "none");
        assert!(card(&sim, &bot).is_none());
        assert!(toasts(&sim).is_empty());
    }
}
