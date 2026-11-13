//! Contract §5 row 12: `CoreHandle::set_card_held` hands back the parked
//! hook response together with a `HoldGuard`, and dropping that guard
//! without disarming it is the hook client going away (Claude's own
//! timeout, Ctrl-C, a reset connection): the hold is released with no
//! decision and the **same card id** comes back at once as the degraded
//! `⌨ … Open app` card (§4.8, §12) — not after `hold_s`.
mod core_support;

use core_support::{
    agent_of, answer, card_put, claude, ctx, hold_register, live_session, observer_session,
    permission_card, uid, Running,
};
use deck_model::{Control, DeviceOp, EventKind, Request, Scope};
use deckd::core::HoldResult;

/// Cancellation can happen before the async caller receives its HoldGuard.
/// Drop the reply on either side of the core's response, without ever making
/// a guard, so this race is covered independently of task scheduling.
#[tokio::test]
async fn abandoned_hold_replies_degrade_without_a_guard() {
    for before_reply in [true, false] {
        let live = Running::start("abandoned-hold", 4096);
        let handle = live.handle.clone();
        let (open, _outbound) = live_session(&[Scope::Agent]);
        let session = handle.open_session(open).await.unwrap();
        let agent = agent_of(
            &handle
                .api(
                    claude(),
                    session,
                    false,
                    Request::AgentRegister(hold_register("abandoned-hook")),
                )
                .await,
        );
        live.parts.drain();
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let receiver = if before_reply {
            drop(receiver);
            None
        } else {
            Some(receiver)
        };
        handle
            .send(deckd::core::CoreMsg::SetCardHeld {
                principal: claude(),
                session,
                cmd: permission_card(&agent),
                reply,
            })
            .await
            .unwrap();
        if let Some(receiver) = receiver {
            let (_, _, hold) = receiver.await.unwrap().unwrap();
            drop(hold);
        }
        let first = live.op(|op| card_put(op).is_some()).await;
        let (id, _, _) = card_put(&first).unwrap();
        let degraded = live
            .op(|op| card_put(op).is_some_and(|(_, text, _)| text.starts_with('⌨')))
            .await;
        let (again, _, choices) = card_put(&degraded).unwrap();
        assert_eq!(again, id, "cancellation keeps the original card identity");
        assert_eq!(choices, vec!["Open app"]);
        let json = agent_json(&handle, session, &agent).await;
        assert_eq!(json["card"]["status"], "degraded");
        assert_eq!(json["card"]["deliver"], "focus");
    }
}

/// The agent object as `agent.get` answers it for an `admin` caller.
async fn agent_json(
    handle: &deckd::core::CoreHandle,
    session: deck_model::SessionId,
    agent: &deck_model::AgentId,
) -> serde_json::Value {
    handle
        .call(
            ctx(session, &[Scope::Admin]),
            "agent.get",
            serde_json::json!({"agent": agent}),
        )
        .await
        .result
        .expect("agent.get")
}

#[tokio::test]
async fn hold_guard_drop_sends_hold_gone() {
    let live = Running::start("hold-guard", 4096);
    let handle = live.handle.clone();
    let (open, _outbound) = live_session(&[Scope::Agent]);
    let session = handle.open_session(open).await.unwrap();
    // A held card is adapter-only (§4.6): the agent is the Claude adapter's.
    let agent = agent_of(
        &handle
            .api(
                claude(),
                session,
                false,
                Request::AgentRegister(hold_register("cohdl-api")),
            )
            .await,
    );
    live.parts.drain();

    let (card, mut hold, guard) = handle
        .set_card_held(claude(), session, permission_card(&agent))
        .await
        .expect("the adapter's permission card");
    let put = live.op(|op| card_put(op).is_some()).await;
    let (id, text, choices) = card_put(&put).unwrap();
    assert_eq!(id, card.0);
    assert_eq!(text, "Bash: psql -f migrate.sql staging");
    assert_eq!(choices, vec!["Allow", "Allow always", "Deny"]);
    assert!(hold.try_recv().is_err(), "nothing resolved the hold yet");

    // The handler goes away: the armed guard reports it.
    assert!(guard.is_armed());
    drop(guard);

    // The same id is re-pushed, degraded, with one choice that still works.
    let put = live
        .op(|op| card_put(op).is_some_and(|(_, text, _)| text.starts_with('⌨')))
        .await;
    let (again, text, choices) = card_put(&put).unwrap();
    assert_eq!(again, card.0, "a same-id re-push: no new chime");
    assert_eq!(text, "⌨ Bash: psql -f migrate.sql staging");
    assert_eq!(choices, vec!["Open app"]);
    assert!(
        !live
            .parts
            .taken()
            .iter()
            .any(|op| matches!(op, DeviceOp::Control(Control::CardClear(..)))),
        "a released hold is degraded, never cleared"
    );

    // The store says the same, and the release is on the bus.
    let json = agent_json(&handle, session, &agent).await;
    assert_eq!(json["card"]["id"], card.0);
    assert_eq!(json["card"]["status"], "degraded");
    assert_eq!(json["card"]["deliver"], "focus");
    let (ring, _) = handle.bus().replay(0, 500, None);
    let degraded = ring
        .iter()
        .find(|e| e.kind == EventKind::CardDegraded)
        .expect("card.degraded is on the bus");
    assert_eq!(degraded.data["reason"], "hold_gone");

    // A disarmed guard is the answered case: it says nothing on drop, so a
    // second held card survives its handler returning.
    live.parts.drain();
    let (second, parked, mut quiet) = handle
        .set_card_held(claude(), session, permission_card(&agent))
        .await
        .expect("the card comes back");
    quiet.disarm();
    drop(quiet);
    let json = agent_json(&handle, session, &agent).await;
    assert_eq!(json["card"]["id"], second.0);
    assert_eq!(json["card"]["status"], "pending");
    assert_eq!(json["card"]["deliver"], "hold");

    // And an answer still reaches whoever is parked on that second hold.
    let operator = handle
        .open_session(observer_session(&[Scope::Operate]))
        .await
        .unwrap();
    handle
        .api(uid(), operator, false, answer(&agent, second, 2))
        .await
        .expect("Deny is never a guarded operation");
    let resolved = parked.await.expect("the parked hook was resolved");
    let HoldResult::Answer(decision) = resolved else {
        panic!("the hold got {resolved:?}");
    };
    assert_eq!(decision.label.as_deref(), Some("Deny"));
}

/// B4: every store refusal used to collapse into one `-32009
/// invalid{card, refused}`, because the core recovered the card id by
/// scanning the emitted events and had nothing else to report. A hook
/// helper must be able to tell "this session is gone" (stop, the CLI
/// quit) from "I built a bad card" (a bug of ours) from "that is not my
/// agent" (B3), and the store already says which.
#[tokio::test]
async fn a_refused_hold_carries_the_stores_own_error() {
    let live = Running::start("hold-refusal", 4096);
    let handle = live.handle.clone();
    let (open, _outbound) = live_session(&[Scope::Agent]);
    let session = handle.open_session(open).await.unwrap();
    let agent = agent_of(
        &handle
            .api(
                claude(),
                session,
                false,
                Request::AgentRegister(hold_register("cohdl-api")),
            )
            .await,
    );

    let mut gone = permission_card(&agent);
    gone.agent = deck_model::AgentId("ag_nope".into());
    let err = handle
        .set_card_held(claude(), session, gone)
        .await
        .expect_err("no such agent");
    assert_eq!(err.code, deck_model::ApiError::NOT_FOUND);
    assert_eq!(err.data["what"], "agent");

    let mut blank = permission_card(&agent);
    blank.text = "   ".into();
    let err = handle
        .set_card_held(claude(), session, blank)
        .await
        .expect_err("a card with no text");
    assert_eq!(err.code, deck_model::ApiError::INVALID);
    assert_eq!(err.data["field"], "text");
    assert_eq!(err.data["why"], "empty");

    let mut over = permission_card(&agent);
    over.rec = Some(9);
    let err = handle
        .set_card_held(claude(), session, over)
        .await
        .expect_err("a rec past the choices");
    assert_eq!(err.data["field"], "rec");
    assert_eq!(err.data["why"], "out_of_range");

    // Another session parking on our agent (B3) is a conflict, not
    // "refused": the caller learns who owns it.
    let (other, _other_rx) = live_session(&[Scope::Agent]);
    let other = handle.open_session(other).await.unwrap();
    let err = handle
        .set_card_held(claude(), other, permission_card(&agent))
        .await
        .expect_err("not this session's agent");
    assert_eq!(err.code, deck_model::ApiError::CONFLICT);
    assert_eq!(err.data["owner"]["principal"], "adapter:claude");

    // Nothing was written by any of them, and the real hold still works.
    let json = agent_json(&handle, session, &agent).await;
    assert!(json["card"].is_null(), "{json}");
    let (card, _hold, mut guard) = handle
        .set_card_held(claude(), session, permission_card(&agent))
        .await
        .expect("the owner's own card");
    guard.disarm();
    assert_eq!(card.0, 1);
}

/// B6 end to end: the §5 degraded re-push writes the **same card id**, so
/// a re-push while the card is still held parks a second hold on one
/// `(agent, card)`. `Holds::park` was `HashMap::insert`: the first sender
/// was dropped, handler A saw a bare `RecvError` (the core going away, as
/// far as it could tell), answered `{}` with its guard still armed, and
/// the `HoldGone` that guard sent took handler **B's** hold out of the
/// map and degraded the card under it — two hooks unanswered, no card.
#[tokio::test]
async fn a_same_id_re_push_replaces_the_hold_without_killing_the_new_one() {
    let live = Running::start("hold-repush", 4096);
    let handle = live.handle.clone();
    let (open, _outbound) = live_session(&[Scope::Agent]);
    let session = handle.open_session(open).await.unwrap();
    let agent = agent_of(
        &handle
            .api(
                claude(),
                session,
                false,
                Request::AgentRegister(hold_register("cohdl-api")),
            )
            .await,
    );

    let (card, first, first_guard) = handle
        .set_card_held(claude(), session, permission_card(&agent))
        .await
        .expect("the first hold");
    let mut again = permission_card(&agent);
    again.id = Some(card);
    again.text = "⌨ Bash: psql -f migrate.sql staging".into();
    let (same, second, mut second_guard) = handle
        .set_card_held(claude(), session, again)
        .await
        .expect("the same-id re-push");
    assert_eq!(same, card, "a re-push keeps the id");

    // The displaced handler is released, not abandoned: it writes `{}`.
    assert!(matches!(
        first.await.expect("the first handler was told"),
        HoldResult::Released {
            reason: deck_model::Release::HoldGone
        }
    ));
    // Its guard is still armed — the handler had no reason to disarm — and
    // dropping it must not disturb the hold that replaced it.
    assert!(first_guard.is_armed());
    drop(first_guard);

    let json = agent_json(&handle, session, &agent).await;
    assert_eq!(json["card"]["id"], card.0);
    assert_eq!(json["card"]["status"], "pending", "not degraded: {json}");
    assert_eq!(json["card"]["deliver"], "hold");

    // And the answer reaches the second hook.
    let operator = handle
        .open_session(observer_session(&[Scope::Operate]))
        .await
        .unwrap();
    handle
        .api(uid(), operator, false, answer(&agent, card, 2))
        .await
        .expect("Deny is never a guarded operation");
    let HoldResult::Answer(decision) = second.await.expect("the second hook was resolved") else {
        panic!("the second hold was not answered");
    };
    assert_eq!(decision.label.as_deref(), Some("Deny"));
    second_guard.disarm();
}

/// B7: §4.1 caps the parked hooks at `[api] max_held_hooks`, and nothing
/// enforced it. Past the cap the hold is refused `-32008` and that hook
/// fails open to the CLI's own dialog — the right answer when the deck
/// already holds more than anyone can work through. Without a cap the
/// first thing a flood meets is the core channel's 1024 slots, by which
/// time every agent on the box is waiting on it.
#[tokio::test]
async fn the_held_hook_cap_refuses_and_leaves_no_card_behind() {
    let live = Running::start("hold-cap", 4096);
    let handle = live.handle.clone();
    let mut config = deckd::config::Config::default();
    config.api.max_held_hooks = 2;
    handle.config(std::sync::Arc::new(config)).await;
    let (open, _outbound) = live_session(&[Scope::Agent]);
    let session = handle.open_session(open).await.unwrap();

    let mut parked = Vec::new();
    for name in ["one", "two"] {
        let agent = agent_of(
            &handle
                .api(
                    claude(),
                    session,
                    false,
                    Request::AgentRegister(hold_register(name)),
                )
                .await,
        );
        let (card, hold, mut guard) = handle
            .set_card_held(claude(), session, permission_card(&agent))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        guard.disarm();
        parked.push((agent, card, hold, guard));
    }

    let third = agent_of(
        &handle
            .api(
                claude(),
                session,
                false,
                Request::AgentRegister(hold_register("three")),
            )
            .await,
    );
    let err = handle
        .set_card_held(claude(), session, permission_card(&third))
        .await
        .expect_err("the cap");
    assert_eq!(err.code, deck_model::ApiError::RATE_LIMITED);
    assert_eq!(err.data["retry_ms"].as_u64(), Some(100));
    let json = agent_json(&handle, session, &third).await;
    assert!(json["card"].is_null(), "no card for a refused hook: {json}");

    // A same-id re-push of a card that is already held replaces its hold
    // rather than adding one, so the ceiling does not block the degraded
    // re-push of §5 — which is the one thing that must always work.
    let (agent, card, _hold, _guard) = &parked[0];
    let mut degraded = permission_card(agent);
    degraded.id = Some(*card);
    degraded.text = "⌨ Bash: psql -f migrate.sql staging".into();
    let (same, _hold, mut guard) = handle
        .set_card_held(claude(), session, degraded)
        .await
        .expect("a re-push at the ceiling");
    assert_eq!(same, *card);
    guard.disarm();

    let metrics = handle
        .call(
            ctx(session, &[Scope::Admin]),
            "metrics.get",
            serde_json::json!({}),
        )
        .await
        .result
        .expect("metrics.get");
    assert_eq!(metrics["holds"], 2, "{metrics}");
}
