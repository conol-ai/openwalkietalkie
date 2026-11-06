//! Contract §5 row 16: the two ways a subscriber falls behind. Overflowing
//! the broadcast lags it, which is what a door turns into the `resync`
//! notification of §3.3; and once the ring has dropped what a caller asks
//! for, `state.events{since}` answers `gap:true` so the caller knows to call
//! `state.get` (design §2.2 read path).
mod core_support;

use core_support::{agent_of, ctx, live_session, poll_card, poll_register, uid, Running};
use deck_model::{AgentState, AgentUpdate, Event, EventKind, Principal, Request, Scope, Ts};
use deckd::bus::{Bus, Published, BROADCAST_CAPACITY};
use std::sync::Arc;
use tokio::sync::broadcast::error::TryRecvError;

fn published(rev: u64) -> Published {
    Published {
        event: Arc::new(Event {
            rev,
            ts: Ts(rev),
            kind: EventKind::CardSet,
            principal: Principal::Device,
            data: serde_json::json!({}),
        }),
        owner: None,
    }
}

#[tokio::test]
async fn lagged_subscriber_gets_resync_and_events_replay_with_gap() {
    // A subscriber that never reads falls off the 4096-deep broadcast.
    let bus = Bus::new(16);
    let mut slow = bus.subscribe();
    let mut quick = bus.subscribe();
    for rev in 1..=(BROADCAST_CAPACITY as u64 + 8) {
        bus.publish(published(rev));
        // The quick one keeps up, so only the slow one lags.
        while quick.try_recv().is_ok() {}
    }
    let lagged = slow.try_recv().expect_err("the slow subscriber lagged");
    let TryRecvError::Lagged(missed) = lagged else {
        panic!("the slow subscriber got {lagged:?}");
    };
    assert!(missed >= 8, "{missed} events were dropped for it");
    assert!(quick.try_recv().is_err(), "the quick one is current");

    // That is exactly what the door writes as a `resync` (§3.3), after
    // which the subscriber is current again.
    let line: serde_json::Value =
        serde_json::from_str(&deckd::api::rpc::resync_line(bus.rev())).unwrap();
    assert_eq!(line["jsonrpc"], "2.0");
    assert_eq!(line["method"], "resync");
    assert_eq!(line["params"]["rev"], bus.rev());
    assert!(line.get("id").is_none(), "a notification has no id");
    // After the resync the door resubscribes from where the channel now is
    // (the ring, not the broadcast, is what replays `since`), and the next
    // event reaches it like any other.
    while slow.try_recv().is_ok() {}
    let next = BROADCAST_CAPACITY as u64 + 9;
    bus.publish(published(next));
    assert_eq!(slow.try_recv().unwrap().event.rev, next);

    // The other half: a core whose ring keeps four events. Everything older
    // is a gap, and asking from the oldest event it still holds is not.
    let live = Running::start("bus-gap", 4);
    let handle = live.handle.clone();
    let (open, _outbound) = live_session(&[Scope::Agent]);
    let session = handle.open_session(open).await.unwrap();
    let agent = agent_of(
        &handle
            .api(
                uid(),
                session,
                false,
                Request::AgentRegister(poll_register("ringer")),
            )
            .await,
    );
    for n in 0..6u32 {
        handle
            .api(
                uid(),
                session,
                false,
                Request::AgentUpdate(AgentUpdate {
                    agent: agent.clone(),
                    state: Some(if n % 2 == 0 {
                        AgentState::Working
                    } else {
                        AgentState::Idle
                    }),
                    title: Some(format!("step {n}")),
                    title_source: None,
                    since: None,
                    name: None,
                    meta: None, task: None, subagents: None,
                }),
            )
            .await
            .unwrap();
    }
    handle
        .api(
            uid(),
            session,
            false,
            Request::CardSet(poll_card(&agent, "keep going?")),
        )
        .await
        .unwrap();

    let caller = ctx(session, &[Scope::Admin]);
    let from_zero = handle
        .call(
            caller.clone(),
            "state.events",
            serde_json::json!({"since": 0}),
        )
        .await
        .result
        .unwrap();
    assert_eq!(from_zero["gap"], true, "the ring dropped the beginning");
    let kept = from_zero["events"].as_array().expect("events");
    assert!(!kept.is_empty() && kept.len() <= 4, "a ring of four");
    let oldest = kept[0]["rev"].as_u64().expect("rev");
    let newest = kept[kept.len() - 1]["rev"].as_u64().expect("rev");
    assert!(oldest <= newest);

    let from_oldest = handle
        .call(
            caller.clone(),
            "state.events",
            serde_json::json!({"since": oldest}),
        )
        .await
        .result
        .unwrap();
    assert_eq!(from_oldest["gap"], false, "the ring still holds all of it");
    assert!(
        from_oldest["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["rev"].as_u64().unwrap() > oldest),
        "`since` is exclusive"
    );
    let from_newest = handle
        .call(caller, "state.events", serde_json::json!({"since": newest}))
        .await
        .result
        .unwrap();
    assert_eq!(from_newest["gap"], false);
    assert!(from_newest["events"].as_array().unwrap().is_empty());
}
