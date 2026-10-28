//! The hermetic reducer suite (contract §4, §5): `HostState::detached()` and
//! scripted `ApiEvent`s. No socket, no daemon, no window, no clock — the
//! reducer reads none of them, so every test here is a pure function of the
//! event script it writes.
//!
//! Three of the rows in §5 are proved here and named for it:
//! `killing_the_daemon_and_reconnecting_makes_the_mirror_whole_again`,
//! `a_rev_gap_asks_for_state_get_instead_of_folding_onto_a_stale_mirror`, and
//! `no_ui_action_can_write_to_the_mirror`.
use deck_gui_core::host_state::{
    BLUETOOTH_SETTINGS, MICROPHONE_SETTINGS, ORDER_HISTORY, SPEECH_SETTINGS,
};
use deck_gui_core::{ApiEvent, AppEvent, HostEffect, HostState, SheetKind, UiAction};
use deck_model::{
    AdapterKind, Agent, AgentId, AgentState, AnswerCap, Caps, Card, CardId, CardKind, CardStatus,
    Choice, Deliver, Event, EventKind, FaceEntry, FocusSource, Kind, Link, OrderCap, Origin,
    OriginKind, Owner, Page, Permission, Principal, Route, SessionId, SessionInfo, SessionState,
    SetKey, Slot, Snapshot, Source, Template, TitleSource, Ts, Usage, UsageItem, VoiceTransport,
};
use serde_json::{json, Value};
use std::time::Duration;

/// A fixed stamp: nothing in the reducer reads a clock.
const TS: Ts = Ts(1_789_760_523_120);

fn agent(id: &str) -> Agent {
    Agent {
        id: AgentId(id.into()),
        slot: None,
        kind: Kind::Claude,
        name: "cohdl-api".into(),
        state: AgentState::Working,
        title: Some("Compiling openmicro".into()),
        title_source: TitleSource::Prompt,
        since: TS,
        source: Source::Hook,
        owner: Owner {
            principal: Principal::Adapter(AdapterKind::Claude),
            session: SessionId(7),
            lease_expires: None,
        },
        session: SessionInfo::default(),
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: true,
        },
        companion: None,
        card: None,
        reply: None,
        messages: Vec::new(),
        workflow_runs: Vec::new(), workflow: None,
        chat_rev: 0,
        reply_rev: 0,
        voice: None,
        pinned: false,
        meta: Value::Null,
        redacted: false,
    }
}

fn card(id: u16) -> Card {
    Card {
        id: CardId(id),
        kind: CardKind::Q,
        status: CardStatus::Pending,
        deliver: Deliver::Hold,
        origin: Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: Some("toolu_9".into()),
            group: None,
        },
        text: Some("Bash: psql -f migrate.sql staging".into()),
        choices: vec![
            Choice {
                label: "Allow".into(),
                detail: None,
            },
            Choice {
                label: "Deny".into(),
                detail: None,
            },
        ],
        choices_n: 2,
        choice_map: Vec::new(),
        rec: Some(0),
        free_text: true,
        page: Page { index: 0, count: 1 },
        created: TS,
        expires: None,
        redacted: false,
    }
}

/// A snapshot at `rev` holding one agent, the way a hello answers.
fn snapshot(rev: u64) -> Snapshot {
    let mut snapshot = Snapshot {
        rev,
        ..Snapshot::default()
    };
    let one = agent("ag_1");
    snapshot.agents.insert(one.id.clone(), one);
    snapshot.device.link = Link::Connected;
    snapshot.device.session = SessionState::Live;
    snapshot.device.fw = Some("0.1.0".into());
    snapshot
}

/// The mirror's agent, by id. A `BTreeMap<AgentId, Agent>` does not index by
/// `&str`, and every test here wants one agent out of the roster.
fn mirrored<'a>(state: &'a HostState, id: &str) -> &'a Agent {
    state
        .mirror()
        .agents
        .get(&AgentId(id.to_string()))
        .unwrap_or_else(|| panic!("{id} is not in the mirror"))
}

fn connected(rev: u64) -> AppEvent {
    AppEvent::Api(ApiEvent::Connected {
        snapshot: snapshot(rev),
    })
}

fn event(rev: u64, kind: EventKind, data: Value) -> AppEvent {
    AppEvent::Api(ApiEvent::Event(Event {
        rev,
        ts: TS,
        kind,
        principal: Principal::Uid(501),
        data,
    }))
}

/// A state already connected at `rev` with one agent in it.
fn live(rev: u64) -> HostState {
    let mut state = HostState::detached();
    assert!(state.handle_event(connected(rev)).is_empty());
    state
}

/// A state with `ag_1` holding card 17.
fn live_with_card() -> HostState {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::CardSet,
        json!({"agent": "ag_1", "card": card(17)}),
    ));
    state
}

#[test]
fn a_detached_state_starts_empty_disconnected_and_untrusted() {
    let state = HostState::detached();
    assert_eq!(state.mirror().rev, 0);
    assert!(state.mirror().agents.is_empty());
    assert!(!state.connected());
    assert!(!state.resyncing());
    assert!(!state.trusted(), "no client, so nothing was verified");
    assert!(state.client().is_none());
    assert_eq!(state.backoff(), Duration::from_millis(250));
    assert_eq!(state.orders(&AgentId("ag_1".into())), &[]);
    assert!(state.transcript().is_none());
}

#[test]
fn a_connect_installs_the_snapshot_as_the_mirror_and_says_nothing() {
    let mut state = HostState::detached();
    let effects = state.handle_event(connected(1202));
    assert!(
        effects.is_empty(),
        "the first connect is not news: {effects:?}"
    );
    assert!(state.connected());
    assert_eq!(state.mirror().rev, 1202);
    assert_eq!(state.mirror().agents.len(), 1);
    assert_eq!(state.mirror().device.fw.as_deref(), Some("0.1.0"));
}

#[test]
fn a_contiguous_event_advances_the_mirror_rev() {
    let mut state = live(10);
    assert!(state
        .handle_event(event(
            11,
            EventKind::ConfigChanged,
            json!({"keys": ["api.port"]})
        ))
        .is_empty());
    assert_eq!(state.mirror().rev, 11);
    assert_eq!(state.folded(), 1);
    assert_eq!(state.gaps(), 0);
}

#[test]
fn an_event_the_mirror_already_holds_is_ignored_as_a_replay() {
    let mut state = live(10);
    // A reconnect with `since: rev` replays from the ring, and the ring's
    // idea of "after" can overlap by one.
    for rev in [1, 9, 10] {
        assert!(state
            .handle_event(event(
                rev,
                EventKind::AgentRemoved,
                json!({"agent": "ag_1"})
            ))
            .is_empty());
    }
    assert_eq!(state.mirror().rev, 10);
    assert_eq!(state.folded(), 0);
    assert_eq!(state.mirror().agents.len(), 1, "the replay changed nothing");
}

#[test]
fn a_rev_gap_asks_for_state_get_instead_of_folding_onto_a_stale_mirror() {
    let mut state = live(10);
    // rev 11 never arrived: 12 is a hole, not the next event.
    let effects = state.handle_event(event(
        12,
        EventKind::AgentRemoved,
        json!({"agent": "ag_1", "reason": "released"}),
    ));
    assert_eq!(effects, vec![HostEffect::StateGet]);
    assert_eq!(state.gaps(), 1);
    assert!(state.resyncing());
    assert_eq!(state.mirror().rev, 10, "the mirror did not move");
    assert_eq!(
        state.mirror().agents.len(),
        1,
        "the event was refused, not applied half-way"
    );
    assert_eq!(state.folded(), 0);
}

#[test]
fn only_one_state_get_is_asked_for_while_a_resync_is_outstanding() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(event(20, EventKind::CardCleared, json!({"agent": "ag_1"}))),
        vec![HostEffect::StateGet]
    );
    for rev in 21..30 {
        assert!(
            state
                .handle_event(event(rev, EventKind::CardCleared, json!({"agent": "ag_1"})))
                .is_empty(),
            "rev {rev} asked for a second snapshot"
        );
    }
    assert!(state.resyncing());
    assert_eq!(state.folded(), 0, "nothing was folded onto a stale mirror");
}

#[test]
fn the_snapshot_that_answers_a_gap_closes_the_resync() {
    let mut state = live(10);
    state.handle_event(event(99, EventKind::CardCleared, json!({"agent": "ag_1"})));
    assert!(state.resyncing());
    state.handle_event(connected(120));
    assert!(!state.resyncing());
    assert_eq!(state.mirror().rev, 120);
    // And the mirror folds again from there.
    state.handle_event(event(
        121,
        EventKind::AgentSlot,
        json!({"agent": "ag_1", "slot": 2}),
    ));
    assert_eq!(mirrored(&state, "ag_1").slot, Some(Slot(2)));
}

#[test]
fn a_resync_notification_asks_for_state_get() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(AppEvent::Api(ApiEvent::Resync)),
        vec![HostEffect::StateGet]
    );
    assert!(state.resyncing());
    assert!(
        state
            .handle_event(AppEvent::Api(ApiEvent::Resync))
            .is_empty(),
        "one outstanding state.get is enough"
    );
}

/// A `resync` from a lag, not a hole: the daemon is still sending and the
/// next event is still the next event. Freezing the mirror there would cost
/// the operator a roster for no reason, so a contiguous event is folded even
/// with a `state.get` outstanding.
#[test]
fn a_contiguous_event_still_folds_while_a_lag_resync_is_outstanding() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(AppEvent::Api(ApiEvent::Resync)),
        vec![HostEffect::StateGet]
    );
    assert!(state
        .handle_event(event(
            11,
            EventKind::AgentSlot,
            json!({"agent": "ag_1", "slot": 4}),
        ))
        .is_empty());
    assert_eq!(mirrored(&state, "ag_1").slot, Some(Slot(4)));
    assert_eq!(state.mirror().rev, 11);
    assert!(state.resyncing(), "the state.get is still outstanding");
    assert_eq!(state.gaps(), 0, "a lag is not a hole");
}

/// The other half of the loop a `StateGet` opens: the `state.get` reply is
/// the event that installs the snapshot it answered with.
#[test]
fn a_state_get_reply_closes_the_resync_without_claiming_a_reconnection() {
    let mut state = live(10);
    state.handle_event(event(40, EventKind::CardCleared, json!({"agent": "ag_1"})));
    assert!(state.resyncing());
    let reply = json!({"rev": 40, "snapshot": snapshot(40)});
    let installed = ApiEvent::snapshot_reply(&reply).expect("a snapshot reply");
    assert!(
        state.handle_event(AppEvent::Api(installed)).is_empty(),
        "the socket never went down, so nothing reconnected"
    );
    assert!(!state.resyncing());
    assert!(state.connected());
    assert_eq!(state.mirror().rev, 40);
}

/// The other half of that: the reply was in flight when the socket dropped.
/// Installing a snapshot is not a connection, so it must not un-grey the
/// window, re-enable its buttons, reset the countdown or toast a
/// reconnection that never happened.
#[test]
fn a_state_get_reply_that_lands_after_the_drop_does_not_claim_the_socket_is_up() {
    let mut state = live(10);
    state.handle_event(event(40, EventKind::CardCleared, json!({"agent": "ag_1"})));
    assert!(state.resyncing());
    // The socket goes away while the `state.get` is outstanding.
    state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    assert!(!state.connected());
    let stepped = state.backoff();
    // …and the reply lands anyway.
    let reply = json!({"rev": 40, "snapshot": snapshot(40)});
    let installed = ApiEvent::snapshot_reply(&reply).expect("a snapshot reply");
    assert!(
        state.handle_event(AppEvent::Api(installed)).is_empty(),
        "nothing reconnected: there is no connection"
    );
    assert_eq!(state.mirror().rev, 40, "the mirror is still installed");
    assert!(!state.resyncing(), "and the hole is still closed");
    assert!(!state.connected(), "but the service is still down");
    assert_eq!(state.backoff(), stepped, "and the back-off is untouched");
}

#[test]
fn a_disconnect_keeps_the_last_mirror_and_asks_for_a_reconnect() {
    let mut state = live(10);
    let effects = state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    assert_eq!(
        effects,
        vec![HostEffect::Reconnect(Duration::from_millis(250))]
    );
    assert!(!state.connected());
    assert_eq!(
        state.mirror().rev,
        10,
        "a roster from two seconds ago beats an empty window"
    );
    assert_eq!(state.mirror().agents.len(), 1);
}

#[test]
fn the_reconnect_backoff_doubles_from_250_ms_and_caps_at_5_s() {
    let mut state = live(10);
    let mut waits = Vec::new();
    for _ in 0..8 {
        for effect in state.handle_event(AppEvent::Api(ApiEvent::Disconnected)) {
            if let HostEffect::Reconnect(after) = effect {
                waits.push(after.as_millis() as u64);
            }
        }
    }
    assert_eq!(waits, vec![250, 500, 1000, 2000, 4000, 5000, 5000, 5000]);
}

#[test]
fn a_connect_resets_the_reconnect_backoff() {
    let mut state = live(10);
    for _ in 0..4 {
        state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    }
    // Four attempts made: 250, 500, 1000, 2000 asked for, 4000 next.
    assert_eq!(state.backoff(), Duration::from_secs(4));
    state.handle_event(connected(11));
    assert_eq!(state.backoff(), Duration::from_millis(250));
    assert_eq!(
        state.handle_event(AppEvent::Api(ApiEvent::Disconnected)),
        vec![HostEffect::Reconnect(Duration::from_millis(250))]
    );
}

/// §5: "kill the daemon and the GUI recovers". The daemon dies, every
/// attempt fails on the 250 ms → 5 s schedule, and the snapshot of the
/// daemon that came back makes the mirror whole — including the agent the
/// GUI had folded a change onto before the lights went out.
#[test]
fn killing_the_daemon_and_reconnecting_makes_the_mirror_whole_again() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::AgentSlot,
        json!({"agent": "ag_1", "slot": 1}),
    ));
    assert_eq!(mirrored(&state, "ag_1").slot, Some(Slot(1)));

    // The daemon is killed. Six failed attempts, one `Disconnected` each.
    let mut waits = Vec::new();
    for _ in 0..6 {
        let effects = state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
        assert_eq!(effects.len(), 1);
        let HostEffect::Reconnect(after) = effects[0] else {
            panic!("a disconnect asks for a reconnect and nothing else: {effects:?}");
        };
        waits.push(after.as_millis() as u64);
        assert!(!state.connected());
        assert_eq!(state.mirror().agents.len(), 1, "the mirror is kept, greyed");
    }
    assert_eq!(waits, vec![250, 500, 1000, 2000, 4000, 5000]);

    // The daemon is back with a snapshot of its own: rev 2000, two agents.
    let mut fresh = snapshot(2000);
    let two = agent("ag_2");
    fresh.agents.insert(two.id.clone(), two);
    let effects = state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot: fresh }));
    assert_eq!(
        effects,
        vec![HostEffect::Toast("Service reconnected".into())]
    );
    assert!(state.connected());
    assert_eq!(state.mirror().rev, 2000);
    assert_eq!(state.mirror().agents.len(), 2);
    assert_eq!(state.backoff(), Duration::from_millis(250));
    // The mirror is the daemon's again, not a merge of what we had: the
    // slot we folded before the kill is gone because the daemon says so.
    assert_eq!(mirrored(&state, "ag_1").slot, None);
    // And it folds forward from the new rev without a gap.
    assert!(state
        .handle_event(event(2001, EventKind::UsageRefreshRequested, json!({})))
        .is_empty());
    assert_eq!(state.mirror().rev, 2001);
    assert_eq!(state.gaps(), 0);
}

/// §5: "the mirror is not mutated by a UI action path". `send` takes `&self`,
/// so this is a compile-time fact as much as a test — but the test is what
/// says so out loud, for every action the Deck screen has.
#[test]
fn no_ui_action_can_write_to_the_mirror() {
    let state = live_with_card();
    let before = serde_json::to_value(state.mirror()).unwrap();
    let agent = AgentId("ag_1".into());
    let actions = [
        UiAction::Answer {
            agent: agent.clone(),
            card: CardId(17),
            choice: Some(0),
            text: None,
        },
        UiAction::Snooze {
            agent: agent.clone(),
            card: CardId(17),
        },
        UiAction::Ack {
            agent: agent.clone(),
            card: CardId(17),
        },
        UiAction::Focus {
            agent: agent.clone(),
        },
        UiAction::Order {
            agent,
            text: "also run the smoke tests".into(),
        },
        UiAction::StateGet,
    ];
    let mut methods = Vec::new();
    for action in &actions {
        let (method, params) = action.request();
        methods.push(method);
        assert!(params.is_object(), "{method} has no params object");
        // Detached: there is no client, so nothing is sent — and the point
        // of the test is that nothing was written either way.
        assert!(state.send(action).is_none());
    }
    assert_eq!(
        methods,
        vec![
            "op.answer",
            "op.snooze",
            "op.ack",
            "op.focus",
            "op.order",
            "state.get"
        ]
    );
    assert_eq!(
        serde_json::to_value(state.mirror()).unwrap(),
        before,
        "a UI action wrote to the mirror"
    );
}

/// The same property for week 6's actions, which matter for a second reason:
/// `token.create` is the one call in the whole API whose **reply** is a
/// secret (§4.11), and the mirror is the one place a secret must never
/// reach. Nothing folds these replies at all — they are calls, not events —
/// so the mirror is untouched by construction, and this is what says so.
#[test]
fn no_integrations_or_service_action_can_write_to_the_mirror_either() {
    let state = live_with_card();
    let before = serde_json::to_value(state.mirror()).unwrap();
    let actions = [
        UiAction::IntegrationsStatus { detect: true },
        UiAction::Integrations {
            verb: deck_gui_core::Verb::Install,
            target: "claude".into(),
            trust: false,
            dry_run: true,
        },
        UiAction::TokenList,
        UiAction::TokenCreate {
            name: "script".into(),
            scopes: vec!["observe".into()],
            expires: None,
        },
        UiAction::TokenRevoke {
            name: "script".into(),
        },
        UiAction::TokenRotate {
            name: "hooks".into(),
        },
        UiAction::PermStatus,
        UiAction::ServiceInfo,
        UiAction::ConfigGet,
        UiAction::ConfigSet {
            key: "api.port".into(),
            value: deck_gui_core::ConfigValue::Int(47_202),
        },
        UiAction::ServiceStop,
        UiAction::ServiceRestart,
        UiAction::LogTail {
            lines: 200,
            level: None,
        },
    ];
    let mut methods = Vec::new();
    for action in &actions {
        let (method, params) = action.request();
        methods.push(method);
        assert!(params.is_object(), "{method} has no params object");
        assert!(state.send(action).is_none(), "{method} on a detached state");
    }
    assert_eq!(
        methods,
        vec![
            "integrations.status",
            "integrations.install",
            "token.list",
            "token.create",
            "token.revoke",
            "token.rotate",
            "perm.status",
            "service.info",
            "config.get",
            "config.set",
            "service.stop",
            "service.restart",
            "log.tail",
        ]
    );
    assert_eq!(
        serde_json::to_value(state.mirror()).unwrap(),
        before,
        "an admin action wrote to the mirror"
    );
}

#[test]
fn agent_added_changed_and_removed_fold_into_the_roster() {
    let mut state = live(10);
    let two = agent("ag_2");
    state.handle_event(event(11, EventKind::AgentAdded, json!({"agent": two})));
    assert_eq!(state.mirror().agents.len(), 2);
    assert_eq!(mirrored(&state, "ag_2").state, AgentState::Working);

    // `changed` carries only the changed fields, plus keys that are not
    // `Agent` fields at all (`orphaned`, `reason`) — ignored, not fatal.
    state.handle_event(event(
        12,
        EventKind::AgentChanged,
        json!({"agent": "ag_2", "fields": {
            "state": "question", "title": "Run migrations on staging?",
            "orphaned": false, "reason": null,
        }}),
    ));
    let changed = &mirrored(&state, "ag_2");
    assert_eq!(changed.state, AgentState::Question);
    assert_eq!(changed.title.as_deref(), Some("Run migrations on staging?"));
    assert_eq!(
        changed.name, "cohdl-api",
        "untouched fields survive a merge"
    );
    assert_eq!(changed.caps.order, OrderCap::Inject);

    state.handle_event(event(
        13,
        EventKind::AgentRemoved,
        json!({"agent": "ag_2", "reason": "released"}),
    ));
    assert_eq!(state.mirror().agents.len(), 1);
    assert!(!state.mirror().agents.contains_key(&AgentId("ag_2".into())));
}

#[test]
fn agent_slot_assigns_a_slot_and_a_null_slot_evicts_it() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::AgentSlot,
        json!({"agent": "ag_1", "slot": 3}),
    ));
    assert_eq!(mirrored(&state, "ag_1").slot, Some(Slot(3)));
    assert_eq!(state.mirror().by_slot().len(), 1);
    state.handle_event(event(
        12,
        EventKind::AgentSlot,
        json!({"agent": "ag_1", "slot": null}),
    ));
    assert_eq!(mirrored(&state, "ag_1").slot, None, "not on deck");
}

#[test]
fn card_set_folds_the_whole_card_and_card_cleared_takes_it_away() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::CardSet,
        json!({"agent": "ag_1", "card": card(17)}),
    ));
    let held = mirrored(&state, "ag_1").card.as_ref().unwrap();
    assert_eq!(held.id, CardId(17));
    assert_eq!(held.origin.kind, OriginKind::Permission);
    assert_eq!(held.choices.len(), 2);
    assert_eq!(
        held.text.as_deref(),
        Some("Bash: psql -f migrate.sql staging")
    );

    state.handle_event(event(
        12,
        EventKind::CardCleared,
        json!({"agent": "ag_1", "card": 17, "delivered": true, "reason": "ack"}),
    ));
    assert!(mirrored(&state, "ag_1").card.is_none());
}

#[test]
fn a_card_lifecycle_event_for_a_card_that_was_replaced_is_ignored() {
    let mut state = live_with_card();
    // Card 17 is gone and 18 is up; a late `card.delivered` for 17 belongs
    // to nothing and must not mark 18 delivered.
    state.handle_event(event(
        12,
        EventKind::CardSet,
        json!({"agent": "ag_1", "card": card(18)}),
    ));
    state.handle_event(event(
        13,
        EventKind::CardDelivered,
        json!({"agent": "ag_1", "card": 17, "via": "hold"}),
    ));
    let held = mirrored(&state, "ag_1").card.as_ref().unwrap();
    assert_eq!(held.id, CardId(18));
    assert_eq!(held.status, CardStatus::Pending);
    // …and for an agent that is not there at all.
    state.handle_event(event(
        14,
        EventKind::CardDelivered,
        json!({"agent": "ag_absent", "card": 18, "via": "hold"}),
    ));
    assert_eq!(state.mirror().rev, 14);
}

#[test]
fn the_card_status_machine_folds_answering_delivered_failed_and_snoozed() {
    let mut state = live_with_card();
    state.handle_event(event(
        12,
        EventKind::CardAnswered,
        json!({"agent": "ag_1", "card": 17, "choice": 0, "label": "Allow",
               "text": null, "via": "hold", "source": "deck", "principal": "device"}),
    ));
    assert_eq!(
        mirrored(&state, "ag_1").card.as_ref().unwrap().status,
        CardStatus::Answering {
            choice: Some(0),
            text: None
        }
    );
    state.handle_event(event(
        13,
        EventKind::CardDelivered,
        json!({"agent": "ag_1", "card": 17, "via": "hold"}),
    ));
    assert_eq!(
        mirrored(&state, "ag_1").card.as_ref().unwrap().status,
        CardStatus::Delivered
    );
    state.handle_event(event(
        14,
        EventKind::CardFailed,
        json!({"agent": "ag_1", "card": 17, "reason": "link"}),
    ));
    assert_eq!(
        mirrored(&state, "ag_1").card.as_ref().unwrap().status,
        CardStatus::Failed {
            reason: "link".into()
        }
    );
    state.handle_event(event(
        15,
        EventKind::CardSnoozed,
        json!({"agent": "ag_1", "card": 17}),
    ));
    assert_eq!(
        mirrored(&state, "ag_1").card.as_ref().unwrap().status,
        CardStatus::Snoozed
    );
}

#[test]
fn a_degraded_card_becomes_a_focus_card_and_card_focused_stamps_it() {
    let mut state = live_with_card();
    state.handle_event(event(
        12,
        EventKind::CardDegraded,
        json!({"agent": "ag_1", "card": 17, "reason": "snooze"}),
    ));
    let held = mirrored(&state, "ag_1").card.as_ref().unwrap();
    assert_eq!(held.status, CardStatus::Degraded { focused_at: None });
    assert_eq!(held.deliver, Deliver::Focus, "⌨ Focus terminal");
    assert!(held.expires.is_none());

    state.handle_event(event(
        13,
        EventKind::CardFocused,
        json!({"agent": "ag_1", "card": 17}),
    ));
    assert_eq!(
        mirrored(&state, "ag_1").card.as_ref().unwrap().status,
        CardStatus::Degraded {
            focused_at: Some(TS)
        },
        "kept for the GUI until the hook flow ends"
    );
}

#[test]
fn a_neutralised_card_loses_its_choices_and_its_route() {
    let mut state = live_with_card();
    state.handle_event(event(
        12,
        EventKind::CardDegraded,
        json!({"agent": "ag_1", "card": 17, "reason": "owner_gone", "deliver": "none"}),
    ));
    let held = mirrored(&state, "ag_1").card.as_ref().unwrap();
    assert_eq!(held.deliver, Deliver::None, "informational");
    assert!(held.choices.is_empty());
    assert!(held.rec.is_none());
    assert_eq!(
        held.status,
        CardStatus::Pending,
        "the status is not touched"
    );
}

#[test]
fn reply_set_folds_the_reply_text_and_its_stamp() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::ReplySet,
        json!({"agent": "ag_1", "text": "Tests fail on main.", "announce": "tests fail"}),
    ));
    let reply = mirrored(&state, "ag_1").reply.as_ref().unwrap();
    assert_eq!(reply.text.as_deref(), Some("Tests fail on main."));
    assert_eq!(reply.announce.as_deref(), Some("tests fail"));
    assert_eq!(reply.at, TS);
    assert!(!reply.redacted);
}

#[test]
fn an_order_is_kept_in_the_history_with_the_exact_string_and_its_route() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::OrderDispatched,
        json!({"agent": "ag_1", "via": "inject",
               "text": "also run the smoke tests after the migration",
               "card": null, "utterance": "ut_31"}),
    ));
    let history = state.orders(&AgentId("ag_1".into()));
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].text.as_deref(),
        Some("also run the smoke tests after the migration"),
        "§8.2: shown before an inject executes"
    );
    assert_eq!(history[0].via.as_deref(), Some("inject"));
    assert!(history[0].reason.is_none());
    assert_eq!(history[0].at, TS);
}

#[test]
fn order_failed_marks_the_last_order_in_the_history_and_toasts() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::OrderDispatched,
        json!({"agent": "ag_1", "via": "inject", "text": "run the migration"}),
    ));
    let effects = state.handle_event(event(
        12,
        EventKind::OrderFailed,
        json!({"agent": "ag_1", "reason": "not_foreground", "text_kept": true}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast(
            "Order not delivered: not_foreground".into()
        )]
    );
    let history = state.orders(&AgentId("ag_1".into()));
    assert_eq!(
        history.len(),
        1,
        "the failure landed on the order it failed"
    );
    assert_eq!(history[0].reason.as_deref(), Some("not_foreground"));
    assert_eq!(
        history[0].text.as_deref(),
        Some("run the migration"),
        "§4.12: order.failed carries no text, so Copy needs the history"
    );
}

#[test]
fn an_order_refused_before_it_was_dispatched_is_its_own_entry_with_no_text() {
    let mut state = live(10);
    // §5.5 guard 2: a permission dialog is open, so nothing was typed and
    // there is no `order.dispatched` before this.
    let effects = state.handle_event(event(
        11,
        EventKind::OrderFailed,
        json!({"agent": "ag_1", "reason": "dialog_open", "text_kept": true}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast("Order not delivered: dialog_open".into())]
    );
    let history = state.orders(&AgentId("ag_1".into()));
    assert_eq!(history.len(), 1);
    assert!(history[0].text.is_none());
    assert!(history[0].via.is_none());
    assert_eq!(history[0].reason.as_deref(), Some("dialog_open"));
}

#[test]
fn the_order_history_is_capped_and_keeps_the_newest() {
    let mut state = live(0);
    for n in 1..=(ORDER_HISTORY as u64 + 5) {
        state.handle_event(event(
            n,
            EventKind::OrderDispatched,
            json!({"agent": "ag_1", "via": "push", "text": format!("order {n}")}),
        ));
    }
    let history = state.orders(&AgentId("ag_1".into()));
    assert_eq!(history.len(), ORDER_HISTORY);
    assert_eq!(history[0].text.as_deref(), Some("order 6"));
    assert_eq!(
        history[ORDER_HISTORY - 1].text.as_deref(),
        Some(&*format!("order {}", ORDER_HISTORY + 5))
    );
}

#[test]
fn op_confirm_requested_shows_the_window_and_raises_the_confirmation_sheet() {
    let mut state = live(10);
    let effects = state.handle_event(event(
        11,
        EventKind::OpConfirmRequested,
        json!({"agent": "ag_1", "card": 17, "order": null,
               "principal": "token:mcp", "need": "trusted"}),
    ));
    assert_eq!(effects.len(), 2);
    assert_eq!(effects[0], HostEffect::ShowWindow);
    let HostEffect::RaisePermissionSheet(SheetKind::Confirm(ref ask)) = effects[1] else {
        panic!("a guarded operation raises the confirmation sheet: {effects:?}");
    };
    assert_eq!(ask.agent, Some(AgentId("ag_1".into())));
    assert_eq!(ask.card, Some(CardId(17)));
    assert!(ask.order.is_none());
    assert_eq!(ask.principal, Some(Principal::Token("mcp".into())));

    // The `inject` case carries the exact order text instead of a card.
    let effects = state.handle_event(event(
        12,
        EventKind::OpConfirmRequested,
        json!({"agent": "ag_1", "card": null, "order": "rm -rf build",
               "principal": "uid:501", "need": "trusted"}),
    ));
    let HostEffect::RaisePermissionSheet(SheetKind::Confirm(ref ask)) = effects[1] else {
        panic!("{effects:?}");
    };
    assert_eq!(ask.order.as_deref(), Some("rm -rf build"));
    assert!(ask.card.is_none());
}

#[test]
fn transcripts_fold_into_the_live_dictation_and_not_into_the_snapshot() {
    let mut state = live(10);
    let before = serde_json::to_value(state.mirror()).unwrap();
    state.handle_event(event(
        11,
        EventKind::TranscriptPartial,
        json!({"agent": "ag_1", "ctx": "answer", "text": "allow", "utterance": "ut_7"}),
    ));
    let live = state.transcript().unwrap();
    assert_eq!(live.text, "allow");
    assert_eq!(live.utterance, "ut_7");
    assert!(!live.final_);
    state.handle_event(event(
        12,
        EventKind::TranscriptFinal,
        json!({"agent": "ag_1", "ctx": "answer", "text": "allow always",
               "utterance": "ut_7", "intent": "allow"}),
    ));
    let live = state.transcript().unwrap();
    assert_eq!(live.text, "allow always");
    assert!(live.final_);
    assert_eq!(live.intent.as_deref(), Some("allow"));
    let mut after = serde_json::to_value(state.mirror()).unwrap();
    after["rev"] = before["rev"].clone();
    assert_eq!(after, before, "§4.3 keeps no transcript in the snapshot");
}

#[test]
fn the_ptt_session_opens_on_arming_goes_live_on_listening_and_closes_on_ended() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::VoiceState,
        json!({"state": "arming", "agent": "ag_1", "ctx": "talk", "card": null,
               "principal": "device", "session": null, "utterance": "ut_9"}),
    ));
    let ptt = state.mirror().voice.ptt.as_ref().unwrap();
    assert!(
        !ptt.live,
        "§7.4: SCO bring-up is a second of digital zeroes"
    );
    assert_eq!(ptt.utterance, "ut_9");
    assert_eq!(ptt.principal, Principal::Device);
    assert_eq!(ptt.since, TS);

    state.handle_event(event(
        12,
        EventKind::VoiceState,
        json!({"state": "listening", "agent": "ag_1", "ctx": "talk", "card": null,
               "principal": "device", "session": null, "utterance": "ut_9"}),
    ));
    assert!(state.mirror().voice.ptt.as_ref().unwrap().live);

    state.handle_event(event(
        13,
        EventKind::TranscriptPartial,
        json!({"agent": "ag_1", "ctx": "talk", "text": "hello", "utterance": "ut_9"}),
    ));
    state.handle_event(event(
        14,
        EventKind::VoiceState,
        json!({"state": "ended", "agent": "ag_1", "ctx": "talk", "empty": false,
               "principal": "device", "utterance": "ut_9"}),
    ));
    assert!(state.mirror().voice.ptt.is_none());
    assert!(state.transcript().is_none(), "the session is over");
}

#[test]
fn voice_route_and_voice_utterance_fold_the_voice_mirror() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::VoiceRoute,
        json!({"route": "sco_ours", "output_uid": null, "input_uid": "C2-2D:input"}),
    ));
    assert_eq!(state.mirror().voice.route, Route::ScoOurs);
    assert_eq!(
        state.mirror().voice.input_uid.as_deref(),
        Some("C2-2D:input")
    );
    assert!(state.mirror().voice.output_uid.is_none());

    state.handle_event(event(
        12,
        EventKind::VoiceUtterance,
        json!({"utterance": "ut_77", "lane": "ready", "phase": "started"}),
    ));
    assert_eq!(
        state.mirror().voice.speaking.as_ref().unwrap().utterance,
        "ut_77"
    );
    state.handle_event(event(
        13,
        EventKind::VoiceUtterance,
        json!({"utterance": "ut_77", "lane": "ready", "phase": "ended"}),
    ));
    assert!(state.mirror().voice.speaking.is_none());
}

#[test]
fn a_not_determined_permission_raises_a_sheet_and_a_denied_one_opens_settings() {
    let mut state = live(10);
    let effects = state.handle_event(event(
        11,
        EventKind::PermChanged,
        json!({"microphone": "not_determined", "speech": "granted"}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::RaisePermissionSheet(SheetKind::Microphone)]
    );
    assert_eq!(
        state.mirror().voice.permissions.microphone,
        Permission::NotDetermined
    );
    assert_eq!(state.mirror().voice.permissions.speech, Permission::Granted);

    // macOS will not prompt twice, so a denial is a trip to System Settings.
    let effects = state.handle_event(event(
        12,
        EventKind::PermChanged,
        json!({"microphone": "denied", "speech": "denied"}),
    ));
    assert_eq!(
        effects,
        vec![
            HostEffect::OpenUrl(MICROPHONE_SETTINGS.into()),
            HostEffect::OpenUrl(SPEECH_SETTINGS.into()),
        ]
    );
    // Re-emitting the same permissions says nothing new.
    assert!(state
        .handle_event(event(
            13,
            EventKind::PermChanged,
            json!({"microphone": "denied", "speech": "denied"}),
        ))
        .is_empty());

    // Bluetooth (the BLE voice transport): macOS asks by itself, so a
    // not-determined grant raises no sheet; a denial is System Settings.
    assert!(state
        .handle_event(event(
            14,
            EventKind::PermChanged,
            json!({"microphone": "denied", "speech": "denied", "bluetooth": "not_determined"}),
        ))
        .is_empty());
    assert_eq!(
        state.mirror().voice.permissions.bluetooth,
        Permission::NotDetermined
    );
    let effects = state.handle_event(event(
        15,
        EventKind::PermChanged,
        json!({"microphone": "denied", "speech": "denied", "bluetooth": "denied"}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::OpenUrl(BLUETOOTH_SETTINGS.into())]
    );
}

#[test]
fn voice_route_carries_the_transport() {
    let mut state = live(10);
    assert_eq!(state.mirror().voice.transport, None);
    state.handle_event(event(
        11,
        EventKind::VoiceRoute,
        json!({"route": "ble", "output_uid": "ble:FAKE", "input_uid": "ble:FAKE",
               "stuck": false, "transport": "ble"}),
    ));
    assert_eq!(state.mirror().voice.route, Route::Ble);
    assert_eq!(state.mirror().voice.transport, Some(VoiceTransport::Ble));
    // An older daemon sends no transport: what the mirror had stays.
    state.handle_event(event(
        12,
        EventKind::VoiceRoute,
        json!({"route": "ble_ours", "output_uid": null, "input_uid": null}),
    ));
    assert_eq!(state.mirror().voice.route, Route::BleOurs);
    assert_eq!(state.mirror().voice.transport, Some(VoiceTransport::Ble));
}

#[test]
fn device_link_folds_the_link_session_firmware_and_sid() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::DeviceLink,
        json!({"link": "degraded", "sid": 314159, "fw": "0.2.0", "boot": true,
               "host_online": false, "session": "syncing"}),
    ));
    let device = &state.mirror().device;
    assert_eq!(device.link, Link::Degraded);
    assert_eq!(device.session, SessionState::Syncing);
    assert_eq!(device.sid, 314_159);
    assert_eq!(device.fw.as_deref(), Some("0.2.0"));
    assert_eq!(device.boot, Some(true));
    assert!(!device.host_online);

    // A link that went down nulls the firmware it can no longer vouch for.
    state.handle_event(event(
        12,
        EventKind::DeviceLink,
        json!({"link": "offline", "session": "absent", "fw": null, "host_online": false}),
    ));
    assert_eq!(state.mirror().device.link, Link::Offline);
    assert!(state.mirror().device.fw.is_none());
}

#[test]
fn device_set_folds_one_key_into_both_the_device_and_the_host_settings() {
    let mut state = live(10);
    assert_eq!(state.mirror().settings.get(SetKey::Brightness), 5);
    state.handle_event(event(
        11,
        EventKind::DeviceSet,
        json!({"k": "brightness", "v": 2}),
    ));
    assert_eq!(state.mirror().settings.get(SetKey::Brightness), 2);
    assert_eq!(state.mirror().device.settings.get(SetKey::Brightness), 2);
    // Out of the device's range: refused by `Settings::set`, not folded.
    state.handle_event(event(
        12,
        EventKind::DeviceSet,
        json!({"k": "brightness", "v": 99}),
    ));
    assert_eq!(state.mirror().settings.get(SetKey::Brightness), 2);
}

#[test]
fn device_faces_replaces_the_strip_and_device_face_moves_the_cursor() {
    let mut state = live(10);
    let list = vec![
        FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        },
        FaceEntry {
            f: 6,
            t: Template::Settings,
            n: "Settings".into(),
        },
    ];
    state.handle_event(event(11, EventKind::DeviceFaces, json!({"list": list})));
    assert_eq!(state.mirror().device.faces, list);
    assert_eq!(state.mirror().faces, list, "the host adopted the strip");
    state.handle_event(event(12, EventKind::DeviceFace, json!({"f": 6})));
    assert_eq!(state.mirror().device.face, Some(6));
}

#[test]
fn focus_changed_folds_the_agent_the_sequence_and_the_source() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::FocusChanged,
        json!({"agent": "ag_1", "slot": 1, "seq": 41, "source": "device"}),
    ));
    let focus = &state.mirror().focus;
    assert_eq!(focus.agent, Some(AgentId("ag_1".into())));
    assert_eq!(focus.seq, Some(41));
    assert_eq!(focus.source, Some(FocusSource::Device));
    // The focused agent going away takes the focus with it.
    state.handle_event(event(
        12,
        EventKind::AgentRemoved,
        json!({"agent": "ag_1", "reason": "released"}),
    ));
    assert_eq!(state.mirror().focus.agent, None);
}

#[test]
fn usage_updated_replaces_the_usage_block() {
    let mut state = live(10);
    let usage = Usage {
        period: "5h / 7d".into(),
        items: vec![UsageItem {
            reset_tip: None,
            name: "Claude 5h".into(),
            account: None,
            percent: 42,
            reset: "resets 21:00".into(),
            tone: None,
        }],
        at: Some(TS),
    };
    state.handle_event(event(11, EventKind::UsageUpdated, json!({"usage": usage})));
    let held = state.mirror().usage.as_ref().unwrap();
    assert_eq!(held.period, "5h / 7d");
    assert_eq!(held.items.len(), 1);
    assert_eq!(held.items[0].percent, 42);
}

#[test]
fn a_hijacked_port_is_toasted_once_by_name_and_clears_when_it_recovers() {
    let mut state = live(10);
    let effects = state.handle_event(event(
        11,
        EventKind::ServiceDegraded,
        json!({"reason": "hooks_hijack_risk", "port": 47201,
               "listener": {"pid": 991, "command": "python3"}}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast(
            "Port 47201 is held by python3; Claude hooks may stall".into()
        )]
    );
    assert_eq!(
        state.mirror().service.degraded.as_deref(),
        Some("hooks_hijack_risk")
    );
    assert_eq!(state.mirror().service.listener.as_ref().unwrap().pid, 991);
    // The daemon re-probes every 30 s; a banner is not a notification.
    assert!(state
        .handle_event(event(
            12,
            EventKind::ServiceDegraded,
            json!({"reason": "hooks_hijack_risk", "port": 47201,
                   "listener": {"pid": 991, "command": "python3"}}),
        ))
        .is_empty());
    let effects = state.handle_event(event(
        13,
        EventKind::ServiceDegraded,
        json!({"reason": null, "port": 47201, "listener": null}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast("The service recovered".into())]
    );
    assert!(state.mirror().service.degraded.is_none());
    assert!(state.mirror().service.listener.is_none());
}

#[test]
fn a_service_shutdown_is_toasted_and_a_plain_degradation_names_its_reason() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(event(
            11,
            EventKind::ServiceDegraded,
            json!({"reason": "port_in_use", "port": 47201, "listener": null}),
        )),
        vec![HostEffect::Toast("Service degraded: port_in_use".into())]
    );
    assert_eq!(
        state.handle_event(event(12, EventKind::ServiceShutdown, json!({}))),
        vec![HostEffect::Toast("The service is shutting down".into())]
    );
}

#[test]
fn an_event_the_gui_does_not_mirror_advances_the_rev_and_nothing_else() {
    let mut state = live(10);
    let before = serde_json::to_value(state.mirror()).unwrap();
    // Owner-only (§4.12), opt-in device edges, the raw hook stream, the
    // mic meter: none of them is state this window holds.
    for (rev, kind, data) in [
        (11, EventKind::AgentAnswer, json!({"agent": "ag_1"})),
        (12, EventKind::AgentOrder, json!({"agent": "ag_1"})),
        (13, EventKind::VoiceLevel, json!({"rms": 0.4})),
        (14, EventKind::DeviceKey, json!({"k": "enter"})),
        (15, EventKind::HookRaw, json!({"cli": "claude"})),
        (16, EventKind::Error, json!({"context": "pace_backoff"})),
        (
            17,
            EventKind::IntegrationChanged,
            json!({"party": "claude"}),
        ),
        (
            18,
            EventKind::CardTruncated,
            json!({"agent": "ag_1", "card": 17}),
        ),
    ] {
        assert!(
            state.handle_event(event(rev, kind, data)).is_empty(),
            "{kind:?} produced an effect"
        );
    }
    assert_eq!(state.mirror().rev, 18);
    assert_eq!(state.folded(), 8);
    let mut after = serde_json::to_value(state.mirror()).unwrap();
    after["rev"] = before["rev"].clone();
    assert_eq!(after, before);
}

#[test]
fn a_malformed_payload_is_ignored_and_still_advances_the_rev() {
    let mut state = live(10);
    // Every one of these is a well-formed envelope with `data` the reducer
    // cannot use. A client that panicked here would be a client the daemon
    // could crash by adding a field.
    for (rev, kind, data) in [
        (11, EventKind::AgentAdded, json!({"agent": {"id": "ag_9"}})),
        (12, EventKind::AgentAdded, json!({})),
        (
            13,
            EventKind::AgentChanged,
            json!({"agent": "ag_1", "fields": 7}),
        ),
        (
            14,
            EventKind::AgentChanged,
            json!({"agent": "ag_1", "fields": {"state": "not_a_state"}}),
        ),
        (15, EventKind::CardSet, json!({"agent": "ag_1", "card": 17})),
        (16, EventKind::DeviceSet, json!({"k": "nope", "v": 1})),
        (17, EventKind::FocusChanged, json!({"source": "nowhere"})),
        (18, EventKind::UsageUpdated, json!({"usage": "lots"})),
        (19, EventKind::VoiceState, json!({"state": "arming"})),
    ] {
        assert!(
            state.handle_event(event(rev, kind, data)).is_empty(),
            "{kind:?} produced an effect"
        );
    }
    assert_eq!(state.mirror().rev, 19);
    assert_eq!(state.mirror().agents.len(), 1, "no half-applied agent");
    assert_eq!(
        mirrored(&state, "ag_1").state,
        AgentState::Working,
        "a fields merge that would not parse left the agent alone"
    );
    assert!(mirrored(&state, "ag_1").card.is_none());
    assert!(state.mirror().voice.ptt.is_none());
    assert_eq!(state.mirror().focus.agent, None);
}

#[test]
fn the_whole_script_of_a_days_work_folds_to_the_daemons_own_snapshot() {
    // The reducer is only worth anything if a long script lands where the
    // daemon is: fold the events, then compare against the snapshot the
    // daemon would have answered `state.get` with.
    let mut state = live(10);
    let mut expected = snapshot(10);
    let script: Vec<(u64, EventKind, Value)> = vec![
        (
            11,
            EventKind::AgentSlot,
            json!({"agent": "ag_1", "slot": 1}),
        ),
        (
            12,
            EventKind::AgentChanged,
            json!({"agent": "ag_1", "fields": {"state": "question"}}),
        ),
        (
            13,
            EventKind::CardSet,
            json!({"agent": "ag_1", "card": card(17)}),
        ),
        (
            14,
            EventKind::FocusChanged,
            json!({"agent": "ag_1", "slot": 1, "seq": 41, "source": "device"}),
        ),
        (
            15,
            EventKind::CardAnswered,
            json!({"agent": "ag_1", "card": 17, "choice": 0, "label": "Allow",
                   "text": null, "via": "hold", "source": "deck", "principal": "device"}),
        ),
        (
            16,
            EventKind::CardDelivered,
            json!({"agent": "ag_1", "card": 17, "via": "hold"}),
        ),
        (
            17,
            EventKind::CardCleared,
            json!({"agent": "ag_1", "card": 17, "delivered": true, "reason": "answered"}),
        ),
        (
            18,
            EventKind::AgentChanged,
            json!({"agent": "ag_1", "fields": {"state": "working"}}),
        ),
        (
            19,
            EventKind::ReplySet,
            json!({"agent": "ag_1", "text": "Migrations applied.", "announce": null}),
        ),
    ];
    for (rev, kind, data) in script {
        state.handle_event(event(rev, kind, data));
    }

    expected.rev = 19;
    let one = expected.agents.get_mut(&AgentId("ag_1".into())).unwrap();
    one.slot = Some(Slot(1));
    one.reply = Some(deck_model::Reply {
        text: Some("Migrations applied.".into()),
        at: TS,
        announce: None,
        redacted: false,
    });
    expected.focus = deck_model::Focus {
        agent: Some(AgentId("ag_1".into())),
        seq: Some(41),
        source: Some(FocusSource::Device),
    };
    assert_eq!(
        serde_json::to_value(state.mirror()).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(state.gaps(), 0);
    assert_eq!(state.folded(), 9);
}
