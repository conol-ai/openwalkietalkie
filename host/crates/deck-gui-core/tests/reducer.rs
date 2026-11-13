//! The hermetic reducer suite (contract §4, §5): `HostState::detached()` and
//! scripted `ApiEvent`s. No socket, no daemon, no window, no clock — the
//! reducer reads none of them, so every test here is a pure function of the
//! event script it writes.
//!
//! Three of the rows in §5 are proved here and named for it:
//! `killing_the_daemon_and_reconnecting_makes_the_mirror_whole_again`,
//! `a_rev_gap_asks_for_state_get_instead_of_folding_onto_a_stale_mirror`, and
//! `no_ui_action_can_write_to_the_mirror`.
//!
//! So is the rule the companion lives by, never a session:
//! `every_snapshot_arrives_without_its_sessions` and
//! `no_agent_card_reply_order_focus_or_transcript_event_reaches_the_mirror`.
//! The rev rule is sampled with the deck's own events, which are what this
//! window folds.
use deck_gui_core::{ApiEvent, AppEvent, HostEffect, HostState, MenuCommand, SheetKind, UiAction};
use deck_model::{
    AdapterKind, Agent, AgentId, AgentState, AnswerCap, Binding, Caps, Card, CardId, CardKind,
    CardStatus, Choice, Deliver, DormancyState, Event, EventKind, FaceEntry, Focus, FocusSource,
    Kind, LastLinked, Link, LinkDrop, MacroKey, MacroLayout, OrderCap, Origin, OriginKind, Owner,
    Page, Permission, Principal, Route, SessionId, SessionInfo, SessionState, SetKey, Snapshot,
    Source, Template, TitleSource, Transport, Ts, Usage, UsageItem, VoiceTransport,
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
            transcribe: Default::default(),
        },
        companion: None,
        card: None,
        reply: None,
        messages: Vec::new(),
        workflow_runs: Vec::new(), task: None, task_source: None, subagents: 0, workflow: None,
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

/// A snapshot at `rev` the way a hello answers: a linked deck, and one agent
/// the service is serving, which this window never keeps.
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

/// `snapshot` as the mirror holds it: the same, without its sessions.
fn mirrored(mut snapshot: Snapshot) -> Value {
    snapshot.agents.clear();
    snapshot.focus = Focus::default();
    serde_json::to_value(snapshot).unwrap()
}

/// The mirror as JSON with `rev` left out, for "nothing changed but the rev".
fn without_rev(state: &HostState) -> Value {
    let mut mirror = serde_json::to_value(state.mirror()).unwrap();
    mirror["rev"] = Value::Null;
    mirror
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

/// A state already connected at `rev` to a linked deck.
fn live(rev: u64) -> HostState {
    let mut state = HostState::detached();
    assert!(state.handle_event(connected(rev)).is_empty());
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
    assert!(state.level().is_none());
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
    assert_eq!(state.mirror().device.link, Link::Connected);
    assert_eq!(state.mirror().device.fw.as_deref(), Some("0.1.0"));
    assert!(state.mirror().agents.is_empty(), "never a session");
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
            .handle_event(event(rev, EventKind::DeviceFace, json!({"f": 6})))
            .is_empty());
    }
    assert_eq!(state.mirror().rev, 10);
    assert_eq!(state.folded(), 0);
    assert_eq!(state.mirror().device.face, None, "the replay changed nothing");
}

#[test]
fn a_rev_gap_asks_for_state_get_instead_of_folding_onto_a_stale_mirror() {
    let mut state = live(10);
    // rev 11 never arrived: 12 is a hole, not the next event.
    let effects = state.handle_event(event(
        12,
        EventKind::DeviceLink,
        json!({"link": "offline", "session": "absent", "fw": null, "host_online": false}),
    ));
    assert_eq!(effects, vec![HostEffect::StateGet]);
    assert_eq!(state.gaps(), 1);
    assert!(state.resyncing());
    assert_eq!(state.mirror().rev, 10, "the mirror did not move");
    assert_eq!(
        (state.mirror().device.link, state.mirror().device.fw.as_deref()),
        (Link::Connected, Some("0.1.0")),
        "the event was refused, not applied half-way"
    );
    assert_eq!(state.folded(), 0);
}

#[test]
fn only_one_state_get_is_asked_for_while_a_resync_is_outstanding() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(event(20, EventKind::DeviceFace, json!({"f": 3}))),
        vec![HostEffect::StateGet]
    );
    for rev in 21..30 {
        assert!(
            state
                .handle_event(event(rev, EventKind::DeviceFace, json!({"f": 3})))
                .is_empty(),
            "rev {rev} asked for a second snapshot"
        );
    }
    assert!(state.resyncing());
    assert_eq!(state.folded(), 0, "nothing was folded onto a stale mirror");
    assert_eq!(state.mirror().device.face, None);
}

#[test]
fn the_snapshot_that_answers_a_gap_closes_the_resync() {
    let mut state = live(10);
    state.handle_event(event(99, EventKind::DeviceFace, json!({"f": 3})));
    assert!(state.resyncing());
    state.handle_event(connected(120));
    assert!(!state.resyncing());
    assert_eq!(state.mirror().rev, 120);
    // And the mirror folds again from there.
    state.handle_event(event(121, EventKind::DeviceFace, json!({"f": 2})));
    assert_eq!(state.mirror().device.face, Some(2));
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
/// the owner the deck's state for no reason, so a contiguous event is folded
/// even with a `state.get` outstanding.
#[test]
fn a_contiguous_event_still_folds_while_a_lag_resync_is_outstanding() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(AppEvent::Api(ApiEvent::Resync)),
        vec![HostEffect::StateGet]
    );
    assert!(state
        .handle_event(event(11, EventKind::DeviceFace, json!({"f": 4})))
        .is_empty());
    assert_eq!(state.mirror().device.face, Some(4));
    assert_eq!(state.mirror().rev, 11);
    assert!(state.resyncing(), "the state.get is still outstanding");
    assert_eq!(state.gaps(), 0, "a lag is not a hole");
}

/// The other half of the loop a `StateGet` opens: the `state.get` reply is
/// the event that installs the snapshot it answered with.
#[test]
fn a_state_get_reply_closes_the_resync_without_claiming_a_reconnection() {
    let mut state = live(10);
    state.handle_event(event(40, EventKind::DeviceFace, json!({"f": 3})));
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
    state.handle_event(event(40, EventKind::DeviceFace, json!({"f": 3})));
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
        "the deck as it was two seconds ago beats an empty window"
    );
    assert_eq!(state.mirror().device.fw.as_deref(), Some("0.1.0"));
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
/// daemon that came back makes the mirror whole — including the deck the
/// GUI had folded a change onto before the lights went out.
#[test]
fn killing_the_daemon_and_reconnecting_makes_the_mirror_whole_again() {
    let mut state = live(10);
    state.handle_event(event(11, EventKind::DeviceFace, json!({"f": 1})));
    assert_eq!(state.mirror().device.face, Some(1));

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
        assert_eq!(state.mirror().device.face, Some(1), "the mirror is kept, greyed");
    }
    assert_eq!(waits, vec![250, 500, 1000, 2000, 4000, 5000]);

    // The daemon is back with a snapshot of its own: rev 2000, a deck on
    // newer firmware, and two agents of its own.
    let mut fresh = snapshot(2000);
    fresh.device.fw = Some("0.2.0".into());
    let two = agent("ag_2");
    fresh.agents.insert(two.id.clone(), two);
    let effects = state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: fresh.clone(),
    }));
    assert_eq!(
        effects,
        vec![HostEffect::Toast("Service reconnected".into())]
    );
    assert!(state.connected());
    assert_eq!(state.mirror().rev, 2000);
    assert_eq!(state.backoff(), Duration::from_millis(250));
    // The mirror is the daemon's again, not a merge of what we had: the
    // face we folded before the kill is gone because the daemon says so,
    // and its sessions are not this window's.
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), mirrored(fresh));
    assert_eq!(state.mirror().device.face, None);
    // And it folds forward from the new rev without a gap.
    assert!(state
        .handle_event(event(2001, EventKind::UsageRefreshRequested, json!({})))
        .is_empty());
    assert_eq!(state.mirror().rev, 2001);
    assert_eq!(state.gaps(), 0);
}

/// §5: "the mirror is not mutated by a UI action path". `send` takes `&self`,
/// so this is a compile-time fact as much as a test — but the test is what
/// says so out loud, for every control the Deck page has: each sets one of
/// the deck's settings, and only the `device.set` that comes back moves the
/// mirror.
#[test]
fn no_ui_action_can_write_to_the_mirror() {
    let state = live(10);
    let before = serde_json::to_value(state.mirror()).unwrap();
    let mut actions: Vec<UiAction> = [
        (SetKey::Speaker, 6),
        (SetKey::Mic, 7),
        (SetKey::Announce, 0),
        (SetKey::Chime, 1),
        (SetKey::Autojump, 1),
        (SetKey::Brightness, 4),
        (SetKey::Idlehome, 120),
        (SetKey::Textsize, 1),
        (SetKey::Language, 1),
    ]
    .into_iter()
    .map(|(key, value)| UiAction::DeviceSet { key, value })
    .collect();
    actions.push(UiAction::StateGet);
    let mut methods = Vec::new();
    for action in &actions {
        let (method, params) = action.request();
        methods.push(method);
        assert!(params.is_object(), "{method} has no params object");
        // Detached: there is no client, so nothing is sent — and the point
        // of the test is that nothing was written either way.
        assert!(state.send(action).is_none());
    }
    let mut expected = vec!["device.set"; 9];
    expected.push("state.get");
    assert_eq!(methods, expected);
    assert_eq!(
        serde_json::to_value(state.mirror()).unwrap(),
        before,
        "a UI action wrote to the mirror"
    );
}

/// Never a session: whatever the service's snapshot holds of its sessions —
/// the agents with their cards, replies and chat, and the focus on one of
/// them — is dropped on the way into the mirror, however the snapshot comes:
/// the hello, a `state.get` reply, a reconnection. The rest of it is
/// installed as the service sent it.
#[test]
fn every_snapshot_arrives_without_its_sessions() {
    let busy = |rev: u64| {
        let mut snapshot = snapshot(rev);
        let mut asking = agent("ag_2");
        asking.state = AgentState::Question;
        asking.card = Some(card(17));
        snapshot.agents.insert(asking.id.clone(), asking);
        snapshot.focus = Focus {
            agent: Some(AgentId("ag_2".into())),
            seq: Some(41),
            source: Some(FocusSource::Device),
        };
        snapshot
    };
    let mut state = HostState::detached();
    // The hello.
    state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: busy(10),
    }));
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), mirrored(busy(10)));
    assert!(state.mirror().agents.is_empty());
    assert_eq!(state.mirror().focus, Focus::default());
    // A `state.get` reply, closing a hole.
    state.handle_event(event(12, EventKind::DeviceFace, json!({"f": 3})));
    assert!(state.resyncing());
    let reply = json!({"rev": 30, "snapshot": busy(30)});
    let installed = ApiEvent::snapshot_reply(&reply).expect("a snapshot reply");
    assert!(state.handle_event(AppEvent::Api(installed)).is_empty());
    assert!(!state.resyncing());
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), mirrored(busy(30)));
    // A reconnection.
    state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: busy(40),
    }));
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), mirrored(busy(40)));
    // Nothing the window holds names a session or says what it was doing.
    let held = serde_json::to_string(state.mirror()).unwrap();
    for word in ["ag_1", "ag_2", "cohdl-api", "Compiling", "psql"] {
        assert!(!held.contains(word), "{word} is in the mirror: {held}");
    }
}

/// Never a session, the other half: the service goes on sending every event
/// about its sessions, because the subscription stays whole for the rev
/// rule, and each one advances the mirror's `rev` and nothing else. No toast
/// for a failed order, no window raised and no sheet for a confirmation.
#[test]
fn no_agent_card_reply_order_focus_or_transcript_event_reaches_the_mirror() {
    let mut state = live(10);
    let before = without_rev(&state);
    let mut asking = agent("ag_2");
    asking.state = AgentState::Question;
    let script = [
        (EventKind::AgentAdded, json!({"agent": asking})),
        (
            EventKind::AgentChanged,
            json!({"agent": "ag_2", "fields": {"state": "question",
                   "title": "Run migrations on staging?"}}),
        ),
        (EventKind::AgentSlot, json!({"agent": "ag_2", "slot": 1})),
        (EventKind::CardSet, json!({"agent": "ag_2", "card": card(17)})),
        (
            EventKind::FocusChanged,
            json!({"agent": "ag_2", "slot": 1, "seq": 41, "source": "device"}),
        ),
        (
            EventKind::TranscriptPartial,
            json!({"agent": "ag_2", "ctx": "answer", "text": "allow", "utterance": "ut_7"}),
        ),
        (
            EventKind::TranscriptFinal,
            json!({"agent": "ag_2", "ctx": "answer", "text": "allow always",
                   "utterance": "ut_7", "intent": "allow"}),
        ),
        (
            EventKind::CardAnswered,
            json!({"agent": "ag_2", "card": 17, "choice": 0, "label": "Allow",
                   "text": null, "via": "hold", "source": "deck", "principal": "device"}),
        ),
        (
            EventKind::CardDelivered,
            json!({"agent": "ag_2", "card": 17, "via": "hold"}),
        ),
        (
            EventKind::CardFailed,
            json!({"agent": "ag_2", "card": 17, "reason": "link"}),
        ),
        (EventKind::CardSnoozed, json!({"agent": "ag_2", "card": 17})),
        (
            EventKind::CardDegraded,
            json!({"agent": "ag_2", "card": 17, "reason": "snooze"}),
        ),
        (EventKind::CardFocused, json!({"agent": "ag_2", "card": 17})),
        (EventKind::CardAcked, json!({"agent": "ag_2", "card": 17})),
        (EventKind::CardExpired, json!({"agent": "ag_2", "card": 17})),
        (
            EventKind::CardCleared,
            json!({"agent": "ag_2", "card": 17, "delivered": true, "reason": "ack"}),
        ),
        (
            EventKind::ReplySet,
            json!({"agent": "ag_2", "text": "Migrations applied.", "announce": "applied"}),
        ),
        (
            EventKind::OrderDispatched,
            json!({"agent": "ag_2", "via": "inject", "text": "also run the smoke tests"}),
        ),
        (
            EventKind::OrderFailed,
            json!({"agent": "ag_2", "reason": "not_foreground", "text_kept": true}),
        ),
        (
            EventKind::OpConfirmRequested,
            json!({"agent": "ag_2", "card": 17, "order": null,
                   "principal": "token:mcp", "need": "trusted"}),
        ),
        (
            EventKind::AgentRemoved,
            json!({"agent": "ag_1", "reason": "released"}),
        ),
    ];
    let count = script.len() as u64;
    for (rev, (kind, data)) in (11..).zip(script) {
        let effects = state.handle_event(event(rev, kind, data));
        assert!(effects.is_empty(), "{kind:?} raised {effects:?}");
    }
    assert_eq!(state.mirror().rev, 10 + count);
    assert_eq!(
        (state.folded(), state.gaps()),
        (count, 0),
        "each advanced the rev and none was a hole"
    );
    assert!(!state.resyncing());
    assert_eq!(without_rev(&state), before, "a session reached the mirror");
}

/// The same property for week 6's actions, which matter for a second reason:
/// `token.create` is the one call in the whole API whose **reply** is a
/// secret (§4.11), and the mirror is the one place a secret must never
/// reach. Nothing folds these replies at all — they are calls, not events —
/// so the mirror is untouched by construction, and this is what says so.
#[test]
fn no_integrations_or_service_action_can_write_to_the_mirror_either() {
    let state = live(10);
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

    // The capture's level is the meter's; the words heard are not this
    // window's to hold (never a session), and still advance the rev.
    state.handle_event(event(13, EventKind::VoiceLevel, json!({"rms": 0.4})));
    assert_eq!(state.level().map(|level| level.at), Some(TS));
    state.handle_event(event(
        14,
        EventKind::TranscriptPartial,
        json!({"agent": "ag_1", "ctx": "talk", "text": "hello", "utterance": "ut_9"}),
    ));
    assert_eq!(state.mirror().rev, 14);
    assert!(state.mirror().voice.ptt.as_ref().unwrap().live);
    state.handle_event(event(
        15,
        EventKind::VoiceState,
        json!({"state": "ended", "agent": "ag_1", "ctx": "talk", "empty": false,
               "principal": "device", "utterance": "ut_9"}),
    ));
    assert!(state.mirror().voice.ptt.is_none());
    assert!(state.level().is_none(), "the session is over");
}

#[test]
fn a_press_and_a_spoken_reply_are_kept_without_whom_they_are_for() {
    // Never a session: the press's own facts are kept (its context, who
    // pressed, the utterance, whether it is live), and the agent, the card
    // it answers and the caller's session are not.
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::VoiceState,
        json!({"state": "listening", "agent": "ag_1", "ctx": "answer", "card": 17,
               "principal": "uid:501", "session": 7, "utterance": "ut_9"}),
    ));
    let ptt = state.mirror().voice.ptt.as_ref().unwrap();
    assert_eq!((&ptt.agent, &ptt.card, &ptt.session), (&None, &None, &None));
    assert_eq!(ptt.principal, Principal::Uid(501));
    assert_eq!(ptt.utterance, "ut_9");
    assert!(ptt.live);

    // A snapshot that names them, as the hello's does, is kept the same way.
    let mut named = snapshot(20);
    named.voice.ptt = Some(deck_model::PttInfo {
        device_press: None,
        agent: Some(AgentId("ag_1".into())),
        ctx: deck_model::PttCtx::Answer,
        card: Some(CardId(17)),
        principal: Principal::Device,
        session: Some(SessionId(7)),
        utterance: "ut_10".into(),
        live: true,
        since: TS,
        companion: false,
    });
    named.voice.speaking = Some(deck_model::Speaking {
        utterance: "ut_11".into(),
        lane: "ready".into(),
        agent: Some(AgentId("ag_1".into())),
    });
    state.handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot: named }));
    let voice = &state.mirror().voice;
    let ptt = voice.ptt.as_ref().unwrap();
    assert_eq!((&ptt.agent, &ptt.card, &ptt.session), (&None, &None, &None));
    assert_eq!((ptt.utterance.as_str(), ptt.live), ("ut_10", true));
    let speaking = voice.speaking.as_ref().unwrap();
    assert_eq!((speaking.utterance.as_str(), &speaking.agent), ("ut_11", &None));
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
fn only_speech_not_yet_asked_raises_a_sheet_and_nothing_opens_settings_unasked() {
    let mut state = live(10);
    // The Mac's microphone is no voice path of this app any more (owner
    // decision 11): not asked yet, it is folded and raises nothing.
    let effects = state.handle_event(event(
        11,
        EventKind::PermChanged,
        json!({"microphone": "not_determined", "speech": "granted"}),
    ));
    assert_eq!(effects, vec![]);
    assert_eq!(
        state.mirror().voice.permissions.microphone,
        Permission::NotDetermined
    );
    assert_eq!(state.mirror().voice.permissions.speech, Permission::Granted);

    // Speech recognition that has not been asked for is the one sheet.
    let effects = state.handle_event(event(
        12,
        EventKind::PermChanged,
        json!({"microphone": "not_determined", "speech": "not_determined"}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::RaisePermissionSheet(SheetKind::Speech)]
    );

    // A denial is lifted in System Settings, which the Voice page opens
    // when the owner presses Open Settings, never by itself.
    let effects = state.handle_event(event(
        13,
        EventKind::PermChanged,
        json!({"microphone": "denied", "speech": "denied"}),
    ));
    assert_eq!(effects, vec![]);
    assert_eq!(state.mirror().voice.permissions.microphone, Permission::Denied);
    assert_eq!(state.mirror().voice.permissions.speech, Permission::Denied);
    // Re-emitting the same permissions says nothing new either.
    assert!(state
        .handle_event(event(
            14,
            EventKind::PermChanged,
            json!({"microphone": "denied", "speech": "denied"}),
        ))
        .is_empty());

    // Bluetooth (the BLE voice transport): macOS asks by itself, so a
    // not-determined grant raises no sheet, and a denial opens nothing.
    assert!(state
        .handle_event(event(
            15,
            EventKind::PermChanged,
            json!({"microphone": "denied", "speech": "denied", "bluetooth": "not_determined"}),
        ))
        .is_empty());
    assert_eq!(
        state.mirror().voice.permissions.bluetooth,
        Permission::NotDetermined
    );
    assert!(state
        .handle_event(event(
            16,
            EventKind::PermChanged,
            json!({"microphone": "denied", "speech": "denied", "bluetooth": "denied"}),
        ))
        .is_empty());
    assert_eq!(state.mirror().voice.permissions.bluetooth, Permission::Denied);
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

/// A linked deck's `device.status`, as the store writes it.
fn status_linked() -> Value {
    json!({
        "transport": "bluetooth", "battery": 85, "charging": false, "board": "Rev B",
        "last": {"at": "2026-11-11T09:41:00.000Z", "fw": "0.13.0-openwalkietalkie",
                 "board": "Rev B", "transport": "bluetooth"},
        "drops": [{"at": "2026-11-11T09:36:00.000Z", "why": "write_stall"}]
    })
}

/// 2026-11-11T09:41:00.000Z and 09:36:00.000Z.
const LIVE_AT: Ts = Ts(1_794_390_060_000);
const DROP_AT: Ts = Ts(1_794_389_760_000);

#[test]
fn device_status_replaces_all_six_fields_at_once() {
    let mut state = live(10);
    assert!(state
        .handle_event(event(11, EventKind::DeviceStatus, status_linked()))
        .is_empty());
    let last = LastLinked {
        at: LIVE_AT,
        fw: Some("0.13.0-openwalkietalkie".into()),
        board: Some("Rev B".into()),
        transport: Some(Transport::Bluetooth),
    };
    let device = &state.mirror().device;
    assert_eq!(device.transport, Some(Transport::Bluetooth));
    assert_eq!((device.battery, device.charging), (Some(85), Some(false)));
    assert_eq!(device.board.as_deref(), Some("Rev B"));
    assert_eq!(device.last, Some(last.clone()));
    assert_eq!(
        device.drops,
        vec![LinkDrop {
            at: DROP_AT,
            why: "write_stall".into()
        }]
    );
    // The next one replaces all six: what it calls null is unknown now,
    // not the old value kept.
    state.handle_event(event(
        12,
        EventKind::DeviceStatus,
        json!({"transport": "usb", "battery": null, "charging": true, "board": null,
               "last": last, "drops": []}),
    ));
    let device = &state.mirror().device;
    assert_eq!(device.transport, Some(Transport::Usb));
    assert_eq!((device.battery, device.charging), (None, Some(true)));
    assert_eq!(device.board, None);
    assert_eq!(device.last, Some(last));
    assert!(device.drops.is_empty());
    // What the status does not carry is the link's, and stays.
    assert_eq!(device.link, Link::Connected);
    assert_eq!(device.fw.as_deref(), Some("0.1.0"));
}

#[test]
fn device_macros_replaces_one_faces_keys_whole_and_leaves_the_others() {
    let mut state = live(10);
    let build = MacroKey {
        label: "build".into(),
        binding: Some(Binding::Left),
        keys: Some("cmd+b".into()),
    };
    let empty = MacroKey {
        label: String::new(),
        binding: None,
        keys: None,
    };
    let shot = MacroKey {
        label: "shot".into(),
        binding: None,
        keys: Some("cmd+shift+4".into()),
    };
    state.handle_event(event(
        11,
        EventKind::DeviceMacros,
        json!({"f": 3, "src": null, "keys": [build, empty, shot]}),
    ));
    assert_eq!(
        state.mirror().macros[&3],
        MacroLayout {
            src: None,
            keys: vec![build.clone(), empty.clone(), shot.clone()],
        }
    );
    let mute = MacroKey {
        label: "mute".into(),
        binding: Some(Binding::Cw),
        keys: None,
    };
    state.handle_event(event(
        12,
        EventKind::DeviceMacros,
        json!({"f": 6, "src": "deck.toml", "keys": [mute]}),
    ));
    assert_eq!(state.mirror().macros.len(), 2);
    assert_eq!(state.mirror().macros[&6].src.as_deref(), Some("deck.toml"));
    assert_eq!(state.mirror().macros[&3].keys.len(), 3, "face 3 untouched");
    // Replaced, never merged: the slots the new layout does not have are
    // gone, and its tag is the new one.
    state.handle_event(event(
        13,
        EventKind::DeviceMacros,
        json!({"f": 3, "src": "app", "keys": [build]}),
    ));
    assert_eq!(
        state.mirror().macros[&3],
        MacroLayout {
            src: Some("app".into()),
            keys: vec![build],
        }
    );
    // A face the deck emptied keeps its (empty) layout, as the store does.
    state.handle_event(event(
        14,
        EventKind::DeviceMacros,
        json!({"f": 6, "src": "deck.toml", "keys": []}),
    ));
    assert!(state.mirror().macros[&6].keys.is_empty());
    assert_eq!(state.mirror().macros.len(), 2);
}

/// Another deck links: its strip, settings and keys replace the mirror's,
/// whole, and the mirror names it. The language is the Mac's: neither copy
/// of it moves, whatever the event carries. A profile that does not read
/// whole changes nothing.
#[test]
fn a_deck_profile_replaces_the_strip_settings_and_keys_but_not_the_language() {
    let mut state = live(10);
    state.handle_event(event(
        11,
        EventKind::DeviceSet,
        json!({"k": "language", "v": 3, "rev": 4}),
    ));
    state.handle_event(event(
        12,
        EventKind::DeviceMacros,
        json!({"f": 3, "src": null, "keys": [{"label": "build", "binding": "left", "keys": "cmd+b"}]}),
    ));
    assert_eq!(state.mirror().settings.language, 3);
    let strip = vec![
        FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        },
        FaceEntry {
            f: 4,
            t: Template::Usage,
            n: "Usage".into(),
        },
    ];
    let deploy = MacroKey {
        label: "deploy".into(),
        binding: None,
        keys: Some("cmd+d".into()),
    };
    let settings = deck_model::Settings {
        speaker: 2,
        textsize: 2,
        ..deck_model::Settings::default()
    };
    let profile = json!({
        "deck": "5d41402a",
        "faces": strip,
        // The service sends the Mac's language here; a stale one must not
        // win either.
        "settings": serde_json::to_value(settings).unwrap(),
        "macros": {"6": {"src": "app", "keys": [deploy]}},
    });
    assert!(state
        .handle_event(event(13, EventKind::DeviceProfile, profile))
        .is_empty());
    let mirror = state.mirror();
    assert_eq!(mirror.rev, 13);
    assert_eq!(mirror.device.deck.as_deref(), Some("5d41402a"));
    assert_eq!(mirror.faces, strip);
    assert_eq!((mirror.settings.speaker, mirror.settings.textsize), (2, 2));
    assert_eq!((mirror.device.settings.speaker, mirror.device.settings.textsize), (2, 2));
    assert_eq!(
        (mirror.settings.language, mirror.settings.language_revision),
        (3, 4)
    );
    assert_eq!(
        (mirror.device.settings.language, mirror.device.settings.language_revision),
        (3, 4)
    );
    assert_eq!(mirror.macros.keys().copied().collect::<Vec<_>>(), vec![6]);
    assert_eq!(mirror.macros[&6].keys, vec![deploy]);
    // A deck the service keeps nothing for yet: no strip, no keys.
    let empty = json!({
        "deck": "7a1f0c22", "faces": [], "macros": {},
        "settings": serde_json::to_value(deck_model::Settings::default()).unwrap(),
    });
    state.handle_event(event(14, EventKind::DeviceProfile, empty));
    let mirror = state.mirror();
    assert!(mirror.faces.is_empty() && mirror.macros.is_empty());
    assert_eq!(mirror.device.deck.as_deref(), Some("7a1f0c22"));
    // Half a profile is none.
    let before = without_rev(&state);
    for (rev, broken) in (15..).zip([
        json!({}),
        json!({"deck": "5d41402a", "faces": [], "settings": {}}),
        json!({"deck": 7, "faces": [], "settings": {}, "macros": {}}),
        json!({"deck": null, "faces": "agents", "settings": {}, "macros": {}}),
        json!({"deck": null, "faces": [], "settings": {}, "macros": {"x": {"src": null, "keys": []}}}),
    ]) {
        assert!(state
            .handle_event(event(rev, EventKind::DeviceProfile, broken))
            .is_empty());
    }
    assert_eq!(without_rev(&state), before);
}

#[test]
fn a_malformed_status_or_key_layout_advances_the_rev_and_changes_nothing() {
    let mut state = live(10);
    state.handle_event(event(11, EventKind::DeviceStatus, status_linked()));
    state.handle_event(event(
        12,
        EventKind::DeviceMacros,
        json!({"f": 3, "src": null,
               "keys": [{"label": "build", "binding": "left", "keys": "cmd+b"}]}),
    ));
    let before = serde_json::to_value(state.mirror()).unwrap();
    // One field wrong spoils the status: it is never applied in half.
    let spoiled = |key: &str, value: Value| {
        let mut status = status_linked();
        status[key] = value;
        status
    };
    let mut no_drops = status_linked();
    no_drops.as_object_mut().unwrap().remove("drops");
    let samples = [
        (EventKind::DeviceStatus, json!({})),
        (EventKind::DeviceStatus, json!("usb")),
        (EventKind::DeviceStatus, no_drops),
        (EventKind::DeviceStatus, spoiled("transport", json!("carrier pigeon"))),
        (EventKind::DeviceStatus, spoiled("battery", json!("full"))),
        (EventKind::DeviceStatus, spoiled("battery", json!(300))),
        (EventKind::DeviceStatus, spoiled("charging", json!(1))),
        (EventKind::DeviceStatus, spoiled("last", json!({"at": "yesterday"}))),
        (EventKind::DeviceStatus, spoiled("drops", json!([{"why": "read"}]))),
        (EventKind::DeviceMacros, json!({"src": null, "keys": []})),
        (EventKind::DeviceMacros, json!({"f": 300, "src": null, "keys": []})),
        (EventKind::DeviceMacros, json!({"f": 3, "src": null, "keys": "cmd+b"})),
        (EventKind::DeviceMacros, json!({"f": 3, "src": 7, "keys": []})),
        (EventKind::DeviceMacros, json!({"f": 3, "src": null, "keys": [{"binding": "left"}]})),
        (EventKind::DeviceMacros, json!({"f": 3, "src": null})),
    ];
    let count = samples.len() as u64;
    for (rev, (kind, data)) in (13..).zip(samples) {
        assert!(
            state.handle_event(event(rev, kind, data)).is_empty(),
            "{kind:?} produced an effect"
        );
    }
    assert_eq!(state.mirror().rev, 12 + count);
    assert_eq!(state.folded(), 2 + count);
    let mut after = serde_json::to_value(state.mirror()).unwrap();
    after["rev"] = before["rev"].clone();
    assert_eq!(after, before);
}

#[test]
fn a_link_that_goes_offline_clears_what_described_the_deck_on_it() {
    let mut linked = snapshot(10);
    linked.device.fw = Some("0.13.0-openwalkietalkie".into());
    linked.device.face = Some(1);
    linked.device.language_supported = true;
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot: linked }));
    state.handle_event(event(11, EventKind::DeviceStatus, status_linked()));
    // A silent link is still the same deck: nothing is cleared.
    state.handle_event(event(
        12,
        EventKind::DeviceLink,
        json!({"link": "degraded", "session": "degraded", "host_online": false}),
    ));
    let device = &state.mirror().device;
    assert_eq!(device.fw.as_deref(), Some("0.13.0-openwalkietalkie"));
    assert_eq!((device.face, device.transport), (Some(1), Some(Transport::Bluetooth)));
    // The store says the link went down with the firmware it still held,
    // then clears it: the mirror ends where the store does.
    state.handle_event(event(
        13,
        EventKind::DeviceLink,
        json!({"link": "offline", "sid": 314159, "fw": "0.13.0-openwalkietalkie",
               "boot": false, "host_online": false, "session": "absent"}),
    ));
    let device = &state.mirror().device;
    assert_eq!(device.link, Link::Offline);
    assert_eq!(device.fw, None);
    assert_eq!(device.face, None);
    assert!(!device.language_supported);
    assert_eq!(
        (device.transport, device.battery, device.charging, device.board.as_deref()),
        (None, None, None, None)
    );
    // The remembered deck and the drops are the status's to change, and
    // the status that follows the drop says what they are now.
    assert_eq!(device.last.as_ref().map(|last| last.at), Some(LIVE_AT));
    assert_eq!(device.drops.len(), 1);
    // A link word this build cannot read clears nothing.
    let mut state = live(10);
    state.handle_event(event(11, EventKind::DeviceLink, json!({"link": "gone"})));
    assert_eq!(state.mirror().device.link, Link::Connected);
    assert_eq!(state.mirror().device.fw.as_deref(), Some("0.1.0"));
}

#[test]
fn seen_is_false_until_the_service_first_answers_and_true_for_good_after() {
    let mut state = HostState::detached();
    assert!(!state.seen());
    state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    assert!(!state.seen(), "a failed dial is not an answer");
    state.handle_event(connected(10));
    assert!(state.seen() && state.connected());
    state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    assert!(state.seen(), "a service that went away was still there once");
    assert!(!state.connected());
}

#[test]
fn a_menu_click_folds_nothing_and_asks_for_nothing() {
    let mut state = live(10);
    let before = serde_json::to_value(state.mirror()).unwrap();
    for command in MenuCommand::ALL {
        assert!(
            state.handle_event(AppEvent::Menu(command)).is_empty(),
            "{command:?}"
        );
    }
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), before);
    assert_eq!(state.folded(), 0);
}

/// The same property for the companion's actions: none of them is an event,
/// so none can reach the mirror.
#[test]
fn no_companion_action_can_write_to_the_mirror() {
    let state = live(10);
    let before = serde_json::to_value(state.mirror()).unwrap();
    let actions = [
        UiAction::DeviceFacesSet {
            list: vec![FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "Agents".into(),
            }],
        },
        UiAction::DeviceMacroSet {
            f: 3,
            layout: "app".into(),
            src: None,
            keys: Vec::new(),
        },
        UiAction::ConfigPatch {
            entries: vec![(
                "sources.paused".into(),
                deck_gui_core::ConfigValue::Bool(true),
            )],
        },
        UiAction::SourcesList,
        UiAction::TestQuestion,
        UiAction::TestVoice,
        UiAction::TestSay,
        UiAction::TestStatus,
        UiAction::CareChecks,
        UiAction::CareFootprint,
        UiAction::CareSupportFile {
            to: "desktop".into(),
        },
        UiAction::CareCacheClear {
            what: "firmware".into(),
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
            "device.faces.set",
            "device.macro.set",
            "config.set",
            "sources.list",
            "care.test.question",
            "care.test.voice",
            "care.test.say",
            "care.test.status",
            "care.checks",
            "care.footprint",
            "care.support_file",
            "care.cache.clear",
        ]
    );
    assert_eq!(
        serde_json::to_value(state.mirror()).unwrap(),
        before,
        "a companion action wrote to the mirror"
    );
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
    // What the lost port takes away is the local API, and nothing of the
    // plugin's hooks, which reach the service over its socket.
    assert_eq!(
        effects,
        vec![HostEffect::Toast(
            "Port 47201 is held by python3, so the local API is closed until the port is free."
                .into()
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

/// `service.dormancy` (R5) is a fact for two pages to read, never a toast:
/// the state lands in the mirror whole, and `null`, from a service that has
/// no dormancy, clears it.
#[test]
fn the_services_dormancy_folds_into_the_mirror_and_null_clears_it() {
    let mut state = live(10);
    assert_eq!(state.mirror().service.dormancy, None);
    let effects = state.handle_event(event(
        11,
        EventKind::ServiceDormancy,
        json!({"window_s": 600, "collecting": false, "dormant_since": "2026-09-18T19:52:03.120Z"}),
    ));
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(
        state.mirror().service.dormancy,
        Some(DormancyState {
            window_s: 600,
            collecting: false,
            dormant_since: Some(Ts(1_789_761_123_120)),
        })
    );
    assert_eq!(state.mirror().rev, 11);
    assert!(state
        .handle_event(event(12, EventKind::ServiceDormancy, Value::Null))
        .is_empty());
    assert_eq!(state.mirror().service.dormancy, None);
}

#[test]
fn a_service_shutdown_is_toasted_and_a_plain_degradation_names_its_reason() {
    let mut state = live(10);
    assert_eq!(
        state.handle_event(event(
            11,
            EventKind::ServiceDegraded,
            json!({"reason": "disk_full", "port": 47201, "listener": null}),
        )),
        vec![HostEffect::Toast("Service degraded: disk_full".into())]
    );
    assert_eq!(
        state.handle_event(event(12, EventKind::ServiceShutdown, json!({}))),
        vec![HostEffect::Toast("The service is shutting down".into())]
    );
}

#[test]
fn a_port_another_program_holds_is_toasted_as_the_local_api_closed_and_never_as_the_hooks() {
    let mut state = live(10);
    // The bind lost, and the service could not tell who to: another process.
    let effects = state.handle_event(event(
        11,
        EventKind::ServiceDegraded,
        json!({"reason": "port_in_use", "port": 47201, "listener": null}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast(
            "Port 47201 is held by another process, so the local API is closed until the port is free."
                .into()
        )]
    );
    assert_eq!(state.mirror().service.degraded.as_deref(), Some("port_in_use"));
    // The same program starts answering on it: news, said by name.
    let effects = state.handle_event(event(
        12,
        EventKind::ServiceDegraded,
        json!({"reason": "hooks_hijack_risk", "port": 47201,
               "listener": {"pid": 4242, "command": "node"}}),
    ));
    assert_eq!(
        effects,
        vec![HostEffect::Toast(
            "Port 47201 is held by node, so the local API is closed until the port is free."
                .into()
        )]
    );
    // In every language, and never a word about Claude or its hooks.
    for language in [
        deck_gui_core::i18n::Language::SimplifiedChinese,
        deck_gui_core::i18n::Language::TraditionalChinese,
        deck_gui_core::i18n::Language::Japanese,
    ] {
        let mut state = live(10);
        let mut snapshot = state.mirror().clone();
        snapshot.rev = 11;
        snapshot.settings.language = language as u16;
        state.handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot }));
        let effects = state.handle_event(event(
            12,
            EventKind::ServiceDegraded,
            json!({"reason": "port_in_use", "port": 47201,
                   "listener": {"pid": 4242, "command": "node"}}),
        ));
        let [HostEffect::Toast(toast)] = effects.as_slice() else {
            panic!("{language:?}: {effects:?}");
        };
        assert!(toast.contains("47201") && toast.contains("node"), "{toast}");
        assert!(toast.contains("API"), "{language:?}: {toast}");
        assert!(!toast.contains("Claude"), "{language:?}: {toast}");
    }
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
    let before = without_rev(&state);
    // Every one of these is a well-formed envelope with `data` the reducer
    // cannot use. A client that panicked here would be a client the daemon
    // could crash by adding a field.
    for (rev, kind, data) in [
        (11, EventKind::DeviceSet, json!({"k": "nope", "v": 1})),
        (12, EventKind::DeviceSet, json!({"k": "brightness", "v": "dim"})),
        (13, EventKind::DeviceLink, json!({"link": 7})),
        (14, EventKind::DeviceFaces, json!({"list": "all of them"})),
        (15, EventKind::DeviceFace, json!({"f": "six"})),
        (16, EventKind::UsageUpdated, json!({"usage": "lots"})),
        (17, EventKind::VoiceState, json!({"state": "arming"})),
        (18, EventKind::VoiceRoute, json!({"route": "carrier pigeon"})),
        (19, EventKind::VoiceUtterance, json!({"phase": 3})),
        (20, EventKind::PermChanged, json!({"microphone": "maybe"})),
        (21, EventKind::ServiceDegraded, json!({"reason": 7, "port": "high"})),
    ] {
        assert!(
            state.handle_event(event(rev, kind, data)).is_empty(),
            "{kind:?} produced an effect"
        );
    }
    assert_eq!(state.mirror().rev, 21);
    assert_eq!(state.folded(), 11);
    assert_eq!(without_rev(&state), before, "nothing was applied half-way");
}

#[test]
fn the_whole_script_of_a_days_work_folds_to_the_daemons_own_snapshot() {
    // The reducer is only worth anything if a long script lands where the
    // daemon is: fold the events, then compare against the snapshot the
    // daemon would have answered `state.get` with — as this window keeps
    // it, without its sessions. The deck's day, with a session's share of
    // it in among it folding nothing.
    let mut state = live(10);
    let strip = vec![
        FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        },
        FaceEntry {
            f: 3,
            t: Template::Macropad,
            n: "Macropad".into(),
        },
        FaceEntry {
            f: 6,
            t: Template::Settings,
            n: "Settings".into(),
        },
    ];
    let build = MacroKey {
        label: "build".into(),
        binding: Some(Binding::Left),
        keys: Some("cmd+b".into()),
    };
    let script: Vec<(EventKind, Value)> = vec![
        (EventKind::DeviceStatus, status_linked()),
        (EventKind::DeviceFaces, json!({"list": strip})),
        (EventKind::AgentAdded, json!({"agent": agent("ag_2")})),
        (EventKind::DeviceFace, json!({"f": 3})),
        (EventKind::DeviceSet, json!({"k": "speaker", "v": 6})),
        (
            EventKind::DeviceMacros,
            json!({"f": 3, "src": null, "keys": [build]}),
        ),
        (
            EventKind::CardSet,
            json!({"agent": "ag_2", "card": card(17)}),
        ),
        (
            EventKind::VoiceRoute,
            json!({"route": "ble", "output_uid": "ble:FAKE", "input_uid": "ble:FAKE",
                   "transport": "ble"}),
        ),
        (
            EventKind::VoiceUtterance,
            json!({"utterance": "ut_77", "lane": "ready", "phase": "started"}),
        ),
        (
            EventKind::ReplySet,
            json!({"agent": "ag_2", "text": "Migrations applied.", "announce": null}),
        ),
        (
            EventKind::VoiceUtterance,
            json!({"utterance": "ut_77", "lane": "ready", "phase": "ended"}),
        ),
        (
            EventKind::DeviceLink,
            json!({"link": "degraded", "session": "degraded", "host_online": false}),
        ),
        (
            EventKind::DeviceLink,
            json!({"link": "connected", "session": "live", "host_online": true}),
        ),
    ];
    let count = script.len() as u64;
    for (rev, (kind, data)) in (11..).zip(script) {
        state.handle_event(event(rev, kind, data));
    }

    let mut expected = snapshot(10);
    expected.rev = 10 + count;
    let device = &mut expected.device;
    device.transport = Some(Transport::Bluetooth);
    device.battery = Some(85);
    device.charging = Some(false);
    device.board = Some("Rev B".into());
    device.last = Some(LastLinked {
        at: LIVE_AT,
        fw: Some("0.13.0-openwalkietalkie".into()),
        board: Some("Rev B".into()),
        transport: Some(Transport::Bluetooth),
    });
    device.drops = vec![LinkDrop {
        at: DROP_AT,
        why: "write_stall".into(),
    }];
    device.faces = strip.clone();
    device.face = Some(3);
    device.settings.speaker = 6;
    device.link = Link::Connected;
    device.session = SessionState::Live;
    device.host_online = true;
    expected.faces = strip;
    expected.settings.speaker = 6;
    expected.macros.insert(
        3,
        MacroLayout {
            src: None,
            keys: vec![build],
        },
    );
    let voice = &mut expected.voice;
    voice.route = Route::Ble;
    voice.output_uid = Some("ble:FAKE".into());
    voice.input_uid = Some("ble:FAKE".into());
    voice.transport = Some(VoiceTransport::Ble);
    assert_eq!(serde_json::to_value(state.mirror()).unwrap(), mirrored(expected));
    assert_eq!(state.gaps(), 0);
    assert_eq!(state.folded(), count);
}
