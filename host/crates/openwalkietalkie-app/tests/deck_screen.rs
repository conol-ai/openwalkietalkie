//! What the **Deck** screen shows, driven through the real reducer (§8.2).
//!
//! These are the view's tests as far as a test can go without a window: a
//! `HostState::detached()` fed scripted `ApiEvent`s, and then every line of
//! §8.2's Deck row read back out of the mirror through
//! `openwalkietalkie_app::present` — the same functions the layout calls. What
//! they cannot check is pixels; what they do check is that the daemon's own
//! events end up saying the right thing on screen, which is where a mirror
//! and a view most easily drift apart.
use deck_gui_core::{ApiEvent, AppEvent, HostEffect, HostState, SheetKind};
use deck_model::{
    AdapterKind, Agent, AgentId, AgentState, AnswerCap, Caps, Card, CardId, CardKind, CardStatus,
    Choice, Deliver, Event, EventKind, Kind, OrderCap, Origin, OriginKind, Owner, Page, PidKind,
    Principal, SessionId, SessionInfo, Slot, Snapshot, Source, Terminal, TitleSource, Ts,
};
use openwalkietalkie_app::confirm::{self, Decision};
use openwalkietalkie_app::present;
use serde_json::{json, Value};

/// A fixed stamp: nothing under test reads a clock.
const TS: Ts = Ts::from_millis(1_789_760_523_120);

fn agent(id: &str, slot: Option<u8>) -> Agent {
    Agent {
        id: AgentId(id.into()),
        slot: slot.map(Slot),
        kind: Kind::Claude,
        name: "cohdl-api".into(),
        state: AgentState::Question,
        title: Some("Bash: psql".into()),
        title_source: TitleSource::Tool,
        since: TS,
        source: Source::Hook,
        owner: Owner {
            principal: Principal::Adapter(AdapterKind::Claude),
            session: SessionId(7),
            lease_expires: None,
        },
        session: SessionInfo {
            key: Some("claude:abc".into()),
            cwd: Some("/Users/t/code/api".into()),
            pid: Some(4242),
            pid_kind: Some(PidKind::Cli),
            terminal: Some(Terminal {
                program: Some("Ghostty".into()),
                tty: Some("/dev/ttys004".into()),
                ..Terminal::default()
            }),
        },
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

fn card() -> Card {
    Card {
        id: CardId(17),
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
                label: "Allow always".into(),
                detail: None,
            },
            Choice {
                label: "Deny".into(),
                detail: None,
            },
        ],
        choices_n: 3,
        choice_map: Vec::new(),
        rec: Some(0),
        free_text: true,
        page: Page { index: 0, count: 2 },
        created: TS,
        expires: Some(TS.plus(std::time::Duration::from_secs(45))),
        redacted: false,
    }
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

/// A mirror holding one slotted agent with a held permission card, and one
/// overflow agent the slot policy could not fit.
fn live() -> HostState {
    let mut snapshot = Snapshot {
        rev: 10,
        ..Snapshot::default()
    };
    let mut one = agent("ag_1", Some(1));
    one.card = Some(card());
    let mut two = agent("ag_2", None);
    two.name = "docs".into();
    two.state = AgentState::Working;
    two.since = TS.plus(std::time::Duration::from_secs(120));
    snapshot.agents.insert(one.id.clone(), one);
    snapshot.agents.insert(two.id.clone(), two);
    snapshot.focus.agent = Some(AgentId("ag_1".into()));
    let mut state = HostState::detached();
    assert!(state
        .handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }))
        .is_empty());
    state
}

fn mirrored(state: &HostState, id: &str) -> Agent {
    state
        .mirror()
        .agents
        .get(&AgentId(id.to_string()))
        .cloned()
        .unwrap_or_else(|| panic!("{id} is not in the mirror"))
}

#[test]
fn a_roster_row_reads_the_dot_the_kind_the_slot_the_title_and_the_age() {
    let state = live();
    let groups = present::projects(state.mirror());
    let rows: Vec<&Agent> = groups.iter().flat_map(|g| g.agents.iter().copied()).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().filter(|a| a.slot.is_none()).count(),
        1,
        "§8.2's overflow row sits in its project, marked not on deck"
    );
    let row = rows[0];
    assert_eq!(
        present::state_tone(row.state, present::stale(row)),
        deck_model::Tone::Red,
        "a question is red on the deck and red here"
    );
    assert_eq!(
        present::kind_icon(row.kind),
        present::kind_icon(Kind::Claude)
    );
    assert_eq!(present::slot_badge(row.slot), "Slot 1");
    assert_eq!(present::live_title(row), "Bash: psql");
    assert_eq!(
        present::state_age(row, TS.plus(std::time::Duration::from_secs(150))).as_deref(),
        Some("2m")
    );
    assert_eq!(present::owner_source(row), "hook");
    // The active-channel bar follows the daemon's focus, not the selection.
    assert_eq!(state.mirror().focus.agent, Some(AgentId("ag_1".into())));
    // The overflow row says it is not on the deck rather than showing a slot,
    // and both rows share their directory's group, named after it.
    assert_eq!(present::slot_badge(rows[1].slot), present::NOT_ON_DECK);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        (groups[0].label.as_str(), groups[0].cwd.as_deref()),
        ("api", Some("/Users/t/code/api"))
    );
}

#[test]
fn the_detail_pane_reads_the_whole_card_every_choice_the_route_and_the_countdown() {
    let state = live();
    let agent = mirrored(&state, "ag_1");
    let card = agent.card.as_ref().expect("the fixture holds a card");
    assert_eq!(present::card_ref(card), "#17.1/2", "§8.2's `#17.1/2`");
    assert_eq!(
        present::card_text(card),
        "Bash: psql -f migrate.sql staging"
    );
    assert_eq!(present::route(card.deliver), "hold");
    assert_eq!(
        present::hold_countdown(card, TS.plus(std::time::Duration::from_secs(20))).as_deref(),
        Some("25s left")
    );
    // Every choice, flat, with the recommendation marked.
    let labels: Vec<String> = (0..card.choices.len())
        .map(|n| present::choice_label(card, n as u8))
        .collect();
    assert_eq!(
        labels,
        vec![
            "1. Allow \u{b7} recommended".to_string(),
            "2. Allow always".to_string(),
            "3. Deny".to_string(),
        ]
    );
    assert_eq!(present::origin_line(&card.origin), "permission \u{b7} Bash");
    assert_eq!(present::custom_badge(&card.origin), None);
    assert_eq!(
        present::source_line(&agent.session),
        "/Users/t/code/api \u{b7} pid 4242 (cli) \u{b7} Ghostty /dev/ttys004"
    );
    assert_eq!(
        present::caps_line(&agent.caps),
        "answer hold \u{b7} order inject \u{b7} focus"
    );
}

#[test]
fn a_custom_card_from_a_token_is_badged_and_attributed_to_a_script() {
    let mut state = live();
    let mut card = card();
    card.origin = Origin::custom();
    card.id = CardId(18);
    state.handle_event(event(
        11,
        EventKind::CardSet,
        json!({"agent": "ag_1", "card": card}),
    ));
    state.handle_event(event(
        12,
        EventKind::AgentChanged,
        json!({"agent": "ag_1", "fields": {"source": "api", "owner": {
            "principal": "token:mcp", "session": SessionId(9), "lease_expires": null}}}),
    ));
    let agent = mirrored(&state, "ag_1");
    let card = agent.card.as_ref().expect("the card was folded");
    assert_eq!(present::custom_badge(&card.origin), Some("custom"));
    assert_eq!(present::origin_line(&card.origin), "custom");
    assert_eq!(present::owner_source(&agent), "script");
}

#[test]
fn a_failed_answer_offers_release_and_only_offers_retry_off_a_hold() {
    let mut state = live();
    state.handle_event(event(
        11,
        EventKind::CardAnswered,
        json!({"agent": "ag_1", "card": 17, "choice": 0}),
    ));
    let agent = mirrored(&state, "ag_1");
    assert_eq!(
        present::answering(agent.card.as_ref().unwrap()).as_deref(),
        Some("Allow")
    );
    state.handle_event(event(
        12,
        EventKind::CardFailed,
        json!({"agent": "ag_1", "card": 17, "reason": "hold_gone"}),
    ));
    let agent = mirrored(&state, "ag_1");
    let card = agent.card.as_ref().unwrap();
    assert_eq!(present::failed_reason(card), Some("hold_gone"));
    assert!(present::release_offered(card), "§4.8 always offers Release");
    assert!(
        !present::retry_offered(card),
        "§4.8: a hold card's parked response is gone, so Retry is refused"
    );
    // A `poll` card can go back to `answering`, so that one does offer Retry.
    let mut poll = card.clone();
    poll.deliver = Deliver::Poll;
    assert!(present::retry_offered(&poll));
}

#[test]
fn a_degraded_hold_says_when_the_operator_was_taken_to_the_terminal() {
    let mut state = live();
    state.handle_event(event(
        11,
        EventKind::CardDegraded,
        json!({"agent": "ag_1", "card": 17, "deliver": "focus"}),
    ));
    let agent = mirrored(&state, "ag_1");
    let card = agent.card.as_ref().unwrap();
    assert_eq!(present::degraded_at(card), Some(None));
    assert_eq!(present::route(card.deliver), "focus");
    state.handle_event(event(
        12,
        EventKind::CardFocused,
        json!({"agent": "ag_1", "card": 17}),
    ));
    let agent = mirrored(&state, "ag_1");
    assert_eq!(
        present::degraded_at(agent.card.as_ref().unwrap()),
        Some(Some("19:42:03Z".to_string())),
        "§8.2's `degraded` with \"focused at\""
    );
}

#[test]
fn the_order_history_keeps_the_exact_string_and_copy_can_only_take_it_from_a_failure() {
    let mut state = live();
    let id = AgentId("ag_1".into());
    assert!(state.orders(&id).is_empty());
    // §4.7: `order.dispatched{text}` is written *before* the text is typed.
    state.handle_event(event(
        11,
        EventKind::OrderDispatched,
        json!({"agent": "ag_1", "text": "psql -f migrate.sql staging", "via": "inject"}),
    ));
    let orders = state.orders(&id).to_vec();
    assert_eq!(orders.len(), 1);
    assert_eq!(
        present::order_line(&orders[0]),
        "\u{201c}psql -f migrate.sql staging\u{201d} \u{b7} inject"
    );
    assert_eq!(
        present::copyable(&orders[0]),
        None,
        "nothing has failed yet, so there is nothing to rescue"
    );
    // §4.12: `order.failed` carries no text, so the history is the only copy.
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
    let orders = state.orders(&id).to_vec();
    assert_eq!(
        orders.len(),
        1,
        "the failure marked the dispatch it belongs to"
    );
    assert_eq!(
        present::order_line(&orders[0]),
        "\u{201c}psql -f migrate.sql staging\u{201d} \u{b7} inject \u{b7} failed: not_foreground"
    );
    assert_eq!(
        present::copyable(&orders[0]),
        Some("psql -f migrate.sql staging")
    );
}

#[test]
fn a_confirmation_request_is_decided_against_the_card_the_mirror_holds_now() {
    let mut state = live();
    let effects = state.handle_event(event(
        11,
        EventKind::OpConfirmRequested,
        json!({"agent": "ag_1", "card": 17, "principal": "token:mcp", "need": "trusted"}),
    ));
    let request = match effects.as_slice() {
        [HostEffect::ShowWindow, HostEffect::RaisePermissionSheet(SheetKind::Confirm(r))] => {
            r.clone()
        }
        other => panic!("a guarded allow raises the sheet, got {other:?}"),
    };
    let card = mirrored(&state, "ag_1").card;
    assert!(confirm::allow_offered(&request, card.as_ref(), true));
    assert!(
        !confirm::allow_offered(&request, card.as_ref(), false),
        "§4.2: an unverified GUI has no allow to re-issue"
    );
    let allow = confirm::action(&request, card.as_ref(), Decision::Allow);
    let deny = confirm::action(&request, card.as_ref(), Decision::Deny);
    assert_eq!(allow.as_ref().map(|a| a.request().0), Some("op.answer"));
    assert_eq!(
        allow.as_ref().map(|a| a.request().1["choice"].clone()),
        Some(json!(0))
    );
    assert_eq!(
        deny.as_ref().map(|a| a.request().1["choice"].clone()),
        Some(json!(2))
    );
    assert_ne!(allow, deny, "dismissing must never do what allowing does");
    // The daemon answers the card while the sheet is up: neither button can
    // re-issue anything, because there is nothing left to answer.
    state.handle_event(event(
        12,
        EventKind::CardCleared,
        json!({"agent": "ag_1", "card": 17}),
    ));
    let card = mirrored(&state, "ag_1").card;
    assert_eq!(card, None);
    assert_eq!(confirm::action(&request, None, Decision::Allow), None);
    assert_eq!(confirm::action(&request, None, Decision::Deny), None);
    assert!(!confirm::allow_offered(&request, None, true));
}

#[test]
fn a_disconnected_window_keeps_the_last_roster_and_the_greyed_dots_say_so() {
    let mut state = live();
    let rows = |state: &HostState| -> usize {
        present::projects(state.mirror())
            .iter()
            .map(|g| g.agents.len())
            .sum()
    };
    let before = rows(&state);
    let effects = state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    assert!(matches!(effects.as_slice(), [HostEffect::Reconnect(_)]));
    assert!(!state.connected());
    // §8.2: greyed, not blanked. The roster is still there to read.
    assert_eq!(rows(&state), before);
    let row = mirrored(&state, "ag_1");
    assert_eq!(
        present::state_tone(row.state, true),
        deck_model::Tone::Gray,
        "every dot is gray while the window cannot vouch for it"
    );
    // A detached state was never verified as the trusted principal, which is
    // the red badge and not an error (§4.2).
    assert!(!state.trusted());
}

#[test]
fn a_rev_gap_stops_the_mirror_and_asks_the_screen_to_re_read_it() {
    let mut state = live();
    let effects = state.handle_event(event(
        99,
        EventKind::CardAcked,
        json!({"agent": "ag_1", "card": 17}),
    ));
    assert_eq!(effects, vec![HostEffect::StateGet]);
    assert!(state.resyncing());
    assert_eq!(state.mirror().rev, 10, "the mirror froze where it was true");
    assert_eq!(state.gaps(), 1);
    // The reply comes back onto the bus as the snapshot that closes it,
    // which is what the screen's `StateGet` arm does with it.
    let reply = json!({"rev": 120, "snapshot": Snapshot { rev: 120, ..Snapshot::default() }});
    let event = ApiEvent::snapshot_reply(&reply).expect("a state.get reply is a snapshot");
    assert!(state.handle_event(AppEvent::Api(event)).is_empty());
    assert!(!state.resyncing());
    assert_eq!(state.mirror().rev, 120);
}

#[test]
fn workflow_mirror_updates_in_place_and_expansion_survives_streaming() {
    use deck_model::workflow::{activity, ActivityStatus, Plan};
    use openwalkietalkie_app::workflow::{visible_steps, ViewState};
    let mut state = live();
    let id = AgentId("ag_2".into());
    let mut view = ViewState::default();
    view.toggle_tool(&id, "t1");
    let plan = Plan::codex(&json!({"plan":(0..9).map(|i| json!({"step":format!("Step {i}"),
        "status":if i < 6 { "completed" } else if i == 6 { "inProgress" } else { "pending" }})).collect::<Vec<_>>()})).unwrap();
    state.handle_event(event(11, EventKind::ReplySet, json!({"agent":id, "text":plan.text(),
        "message":{"id":"p1","role":"tool","plan":plan}})));
    state.handle_event(event(12, EventKind::ReplySet, json!({"agent":id, "text":"**Bash**\n\n```\ncargo test\n```",
        "message":{"id":"t1","role":"tool","streaming":true,"activity":"running"}})));
    assert_eq!(activity(&mirrored(&state, "ag_2"))[0].status, ActivityStatus::Running);
    state.handle_event(event(13, EventKind::ReplySet, json!({"agent":id, "text":"**Bash · failed**\n\nTest failed",
        "message":{"id":"t1","role":"tool","activity":"failed"}})));
    let agent = mirrored(&state, "ag_2");
    assert_eq!(agent.messages.len(), 2);
    assert_eq!(activity(&agent)[0].status, ActivityStatus::Failed);
    assert!(view.tool_open(&id, "t1"));
    assert!(!view.tool_open(&AgentId("another".into()), "t1"));
    let plan = agent.workflow.as_ref().unwrap();
    assert_eq!(plan.completed(), 6);
    assert_eq!(visible_steps(plan, false, 5), 4..9);
    assert_eq!(visible_steps(plan, true, 5), 0..9);
    assert_eq!(visible_steps(plan, false, 3), 5..8);
    state.handle_event(event(14, EventKind::ReplySet, json!({"agent":id, "text":"Next request",
        "message":{"id":"u2","role":"user"}})));
    let agent = mirrored(&state, "ag_2");
    assert!(agent.workflow.is_none());
    assert!(activity(&agent).is_empty());
}
