//! Week-3 contract §6 rows 1–6 and the hook door's own rows, driven end to
//! end: a real daemon, a real `POST /v1/hooks/claude` with a bearer, and
//! the recorded payloads of `tests/fixtures/hooks/claude/*.json` replayed
//! through the adapter.
//!
//! Nothing here reads or writes the user's `~/.claude`: every payload is a
//! committed fixture, scrubbed of content and asserted so by
//! `every_fixture_is_scrubbed`, and the daemon runs on a temp data dir with
//! `[api] port = 0` like every other exit test.
//!
//! The four facts these rows exist to protect, each measured against the
//! installed `claude 2.1.276` and each a silent bug when it is dropped:
//!
//! * **C2** — a response whose `hookSpecificOutput.hookEventName` is not the
//!   event that fired is thrown away whole, decision and all. Every response
//!   asserted below is asserted by exact equality, name included.
//! * **C5** — `PermissionRequest` has no `tool_use_id`, so
//!   `AskUserQuestion`'s own follow-up is recognised by tool name and by the
//!   30 s window after a released group.
//! * **C7** — a multi-select answer is a **comma-joined string**; an array
//!   fails Claude's schema and loses the turn.
//! * **C8** — "held" is not uniform: the permission card is first-answer-wins
//!   and its dialog is already on screen, the question group owns its dialog
//!   for the whole hold.
mod common;

use common::{Daemon, Options, PATIENCE};
use deck_model::{Card, CardStatus, Deliver, Group, Kind, OriginKind, Scope};
use deckd::adapters::claude::{self, Claude};
use deckd::adapters::{Adapter, HookCtx, HookPayload, Ingest};
use deckd::trust::FakeVerifier;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The session id every fixture carries (they are all one recorded run).
const SESSION: &str = "abc123";
/// `[hooks.claude] hold_s` as an install leaves it — the daemon-side
/// budget, **not** the CLI-side hook `timeout`.
///
/// The installed pair is `timeout: 130` (C6: seconds; the binary multiplies
/// by 1000) and `hold_s: 120`, and the installer refuses any `hold_s` above
/// `timeout - 10` (`integrations::claude::HELD_TIMEOUT_S` and the check at
/// `integrations/claude.rs:135`). The ten seconds are not slack: a hook
/// Claude gives up on leaves its terminal dialog live (S1, facts-claude
/// §5), so the hold has to end first and the daemon has to be the one that
/// writes `{}`. `130` here named the wrong side of that pair and asked the
/// door for a budget an install can never produce.
const INSTALLED_HOLD_S: u32 = 120;

// ---------------------------------------------------------------------------
// Fixtures

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks/claude")
}

/// One recorded payload, by file stem.
fn fixture(name: &str) -> serde_json::Value {
    let path = fixtures_dir().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every fixture's file stem, sorted.
fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("the fixture directory")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            Some(path.file_stem()?.to_str()?.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Every string in a payload, at any depth (keys included: a recorded key
/// can carry a path just as a value can).
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                out.push(key.clone());
                strings(v, out);
            }
        }
        _ => {}
    }
}

/// A `PermissionRequest` for a command of our choosing: C5 dedupes on the
/// hash of `tool_input`, so a row that wants a second card asks about
/// something else rather than the same thing twice.
fn permission_payload(command: &str) -> serde_json::Value {
    let mut payload = fixture("permission_request_no_suggestions");
    payload["tool_input"]["command"] = command.into();
    payload["tool_input"]["description"] = "a fixture command".into();
    payload
}

// ---------------------------------------------------------------------------
// The door

/// `POST /v1/hooks/{cli}` on its own task, so the test can drive the deck
/// while the hook is parked. The handle answers `(status, body)`.
fn post_hook(
    port: u16,
    auth: String,
    cli: &str,
    payload: serde_json::Value,
) -> tokio::task::JoinHandle<(u16, serde_json::Value)> {
    let path = format!("/v1/hooks/{cli}");
    let body = payload.to_string();
    tokio::spawn(async move {
        common::http(port, "POST", &path, &[("Authorization", &auth)], &body).await
    })
}

/// The same POST, awaited: for the report-only events, which answer at once.
async fn hook(
    daemon: &Daemon,
    auth: &str,
    payload: &serde_json::Value,
) -> (u16, serde_json::Value) {
    daemon
        .http(
            "POST",
            "/v1/hooks/claude",
            &[("Authorization", auth)],
            &payload.to_string(),
        )
        .await
}

/// `[hooks.claude] hold_s`, as a `config.reload` would set it: the adapter
/// reads the live config per call (B2), so the next hook gets this budget.
fn set_hold_s(daemon: &Daemon, hold_s: u32) {
    let mut config = (**daemon.api.config.load()).clone();
    config.hooks.claude.hold_s = hold_s;
    daemon.api.config.store(Arc::new(config));
}

/// `[hooks.claude] settle_s`, live like `hold_s`.
fn set_settle_s(daemon: &Daemon, settle_s: u32) {
    let mut config = (**daemon.api.config.load()).clone();
    config.hooks.claude.settle_s = settle_s;
    daemon.api.config.store(Arc::new(config));
}

/// The first card on the deck that `f` accepts, with the agent holding it.
async fn card_where(
    daemon: &Daemon,
    what: &str,
    mut f: impl FnMut(&Card) -> bool,
) -> (String, Card) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for agent in daemon.api.bus.snapshot().agents.values() {
            if let Some(card) = agent.card.as_ref().filter(|c| f(c)) {
                return (agent.id.0.clone(), card.clone());
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The card a specific agent is showing right now.
fn current_card(daemon: &Daemon, agent: &str) -> Option<Card> {
    daemon
        .api
        .bus
        .snapshot()
        .agents
        .get(&deck_model::AgentId(agent.to_string()))
        .and_then(|a| a.card.clone())
}

/// A card that is pending and waiting on a hold.
fn parked(card: &Card, kind: OriginKind) -> bool {
    card.origin.kind == kind && card.deliver == Deliver::Hold && card.status == CardStatus::Pending
}

/// A UDS peer with these scopes.
async fn peer(daemon: &Daemon, name: &str, scopes: &[&str]) -> common::UdsClient {
    let mut client = daemon.connect().await;
    client
        .call(
            "session.hello",
            json!({"client": {"name": name, "version": "1"}, "scopes": scopes}),
        )
        .await;
    client
}

fn labels(card: &Card) -> Vec<&str> {
    card.choices.iter().map(|c| c.label.as_str()).collect()
}

/// The budget a card carries, in milliseconds.
fn budget_ms(card: &Card) -> u64 {
    card.expires.expect("a held card expires").0 - card.created.0
}

// ---------------------------------------------------------------------------
// §6 row 1

/// The permission path end to end: the fixture posts, the card lands on the
/// deck with the choices §5.1 defines, and the operator's answer comes back
/// as Claude's own `PermissionRequest` output — the exact object, event name
/// and all (C2).
///
/// `Allow always` is offered **only** when Claude handed us
/// `permission_suggestions`: it is the row that writes `updatedPermissions`,
/// and without a rule to persist it would be a lie.
#[tokio::test]
async fn a_permission_request_becomes_a_held_card_and_the_allow_is_written() {
    // An allow on a permission card is guarded (§4.2): only a trusted
    // principal may say yes, so the deck peer here is a verified one.
    let daemon = Daemon::with(Options {
        verifier: FakeVerifier {
            trusted: true,
            capped: false,
        },
        ..Options::default()
    })
    .await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let payload = fixture("permission_request");
    let suggestions = payload["permission_suggestions"].clone();

    let posted = post_hook(daemon.port, auth.clone(), "claude", payload);
    let (agent, card) = card_where(&daemon, "the permission card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    assert_eq!(card.kind, deck_model::CardKind::Q);
    assert_eq!(
        card.text.as_deref(),
        Some("Bash: rm -rf node_modules"),
        "the tool summary of §5.6, sanitised"
    );
    assert_eq!(labels(&card), vec!["Allow", "Allow always", "Deny"]);
    assert_eq!(card.rec, Some(0), "Allow is the recommended row");
    assert_eq!(card.origin.tool.as_deref(), Some("Bash"));
    assert_eq!(budget_ms(&card), 120_000, "[hooks.claude] hold_s, per card");

    let mut gui = peer(&daemon, "gui", &["observe", "operate"]).await;
    let answered = gui
        .call(
            "op.answer",
            json!({"agent": agent, "card": card.id, "choice": 1}),
        )
        .await;
    // §4.9: `delivered` is the truth, not a promise — it waits for the hook
    // response to be written.
    assert_eq!(answered["delivered"], true, "{answered}");
    assert_eq!(answered["via"], "hold");

    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow", "updatedPermissions": suggestions},
        }}),
        "the exact object Claude's schema takes"
    );

    // The same tool with nothing to persist: two choices, and a plain allow
    // that writes no `updatedPermissions`.
    let bare = fixture("permission_request_no_suggestions");
    let posted = post_hook(daemon.port, auth, "claude", bare);
    let (_, card) = card_where(&daemon, "the second permission card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    assert_eq!(labels(&card), vec!["Allow", "Deny"]);
    assert_eq!(card.text.as_deref(), Some("Bash: date"));
    let answered = gui
        .call(
            "op.answer",
            json!({"agent": agent, "card": card.id, "choice": 0}),
        )
        .await;
    assert_eq!(answered["delivered"], true, "{answered}");
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"},
        }}),
        "no suggestions, nothing persisted"
    );
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// §6 row 2

/// A three-question `AskUserQuestion` answered one card at a time, with one
/// budget across the group and one response at the end.
///
/// The store gives each group card its own hold and shares only the
/// deadline (facts-seams §2e), so the accumulator is the adapter's: cards
/// 1 and 2 are cleared as the group steps (`reason: "group_step"`) and the
/// single `updatedInput.answers` is written after card 3. The multi-select
/// answer is a comma-joined **string** (C7).
#[tokio::test]
async fn a_three_question_group_is_answered_card_by_card_under_one_budget() {
    // A question card is not a guarded allow (§4.2), so an ordinary deck
    // peer answers it.
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let payload = fixture("pre_tool_use_ask_user_question");
    let questions = payload["tool_input"]["questions"].clone();

    let mut watch = daemon.connect().await;
    watch
        .call(
            "session.hello",
            json!({"client": {"name": "gui"}, "scopes": ["observe"], "subscribe": ["card.*"]}),
        )
        .await;
    let mut deck = peer(&daemon, "deck", &["observe", "operate"]).await;
    let posted = post_hook(daemon.port, auth, "claude", payload);

    let (agent, first) = card_where(&daemon, "question 1", |c| {
        c.origin.group == Some(Group { index: 0, count: 3 }) && parked(c, OriginKind::Question)
    })
    .await;
    assert_eq!(
        first.text.as_deref(),
        Some("Colour: Which colour do you prefer?"),
        "the header prefixes the question (§5.6)"
    );
    assert_eq!(labels(&first), vec!["Red", "Green", "Blue"]);
    assert_eq!(first.origin.tool.as_deref(), Some("AskUserQuestion"));
    assert_eq!(first.origin.request_id.as_deref(), Some("toolu_01FIXTURE"));
    assert!(
        first.free_text,
        "the operator may answer in their own words"
    );
    let deadline = first.expires.expect("the group's deadline");
    assert_eq!(budget_ms(&first), 120_000);

    // Long enough that a card starting its own budget could not land on the
    // same deadline by accident.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stepped = deck
        .call(
            "op.answer",
            json!({"agent": agent, "card": first.id, "choice": 1}),
        )
        .await;
    assert_eq!(
        stepped["delivered"], true,
        "an intermediate answer is delivered at once: {stepped}"
    );

    let (_, second) = card_where(&daemon, "question 2", |c| {
        c.origin.group == Some(Group { index: 1, count: 3 }) && parked(c, OriginKind::Question)
    })
    .await;
    assert_ne!(second.id, first.id, "each question is its own card");
    assert_eq!(labels(&second), vec!["Small", "Large"]);
    // One `hold_s` bounds the whole group: card 2 inherits card 1's
    // deadline rather than starting a fresh budget.
    assert!(
        second.expires.unwrap().0.abs_diff(deadline.0) <= 5,
        "card 2 must share the group's deadline: {:?} vs {deadline:?}",
        second.expires
    );
    assert!(
        budget_ms(&second) + 100 < 120_000,
        "card 2 started a budget of its own: {} ms left",
        budget_ms(&second)
    );
    deck.call(
        "op.answer",
        json!({"agent": agent, "card": second.id, "choice": 0}),
    )
    .await;

    let (_, third) = card_where(&daemon, "question 3", |c| {
        c.origin.group == Some(Group { index: 2, count: 3 }) && parked(c, OriginKind::Question)
    })
    .await;
    assert_eq!(labels(&third), vec!["Docs", "Tests", "Bench"]);
    // Multi-select: the operator's own words, which are already the
    // comma-joined form Claude's schema takes.
    let last = deck
        .call(
            "op.answer",
            json!({"agent": agent, "card": third.id, "text": "Docs, Tests"}),
        )
        .await;
    assert_eq!(last["delivered"], true, "{last}");

    // Each intermediate card left the deck as the group stepped.
    for card in [first.id, second.id] {
        let line = watch
            .line_where("card.cleared", |v| {
                v["method"] == "event"
                    && v["params"]["type"] == "card.cleared"
                    && v["params"]["data"]["card"] == card.0
            })
            .await;
        assert_eq!(line["params"]["data"]["reason"], "group_step", "{line}");
        assert_eq!(line["params"]["data"]["delivered"], true, "{line}");
    }

    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    let out = &body["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse", "C2: {body}");
    assert_eq!(out["permissionDecision"], "allow");
    assert_eq!(
        out["updatedInput"]["questions"], questions,
        "updatedInput replaces the whole tool input, so questions is echoed verbatim"
    );
    assert_eq!(
        out["updatedInput"]["answers"],
        json!({
            "Which colour do you prefer?": "Green",
            "Which size fits?": "Small",
            "Which extras do you want?": "Docs, Tests",
        }),
        "answers are keyed by the question text, never the header"
    );
    assert!(
        out["updatedInput"]["answers"]["Which extras do you want?"].is_string(),
        "C7: a multi-select answer is a comma-joined string, never an array"
    );
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// §6 row 3

/// C5, both halves. `PermissionRequest` carries no `tool_use_id`, so the
/// question group's own follow-up — fired 3 ms after a released
/// `PreToolUse`, with the dialog 7 ms behind it (S2) — is recognised two
/// ways, and both answer `{}` with nothing on the deck. A second card here
/// would land on top of the group card the operator is already looking at.
#[tokio::test]
async fn askuserquestion_permission_requests_are_answered_empty() {
    // 1. Concurrent: `tool_name == "AskUserQuestion"`, whatever else is
    //    going on. Nothing is registered, so nothing reaches the roster.
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let (status, body) = hook(
        &daemon,
        &auth,
        &fixture("permission_request_askuserquestion"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({}), "the group owns this ask");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        daemon.api.bus.snapshot().agents.is_empty(),
        "an ignored payload never reaches the core"
    );
    daemon.shutdown().await;

    // 2. Released in the last 30 s: a real group, released at its budget,
    //    then an ordinary `Bash` permission from the same session.
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    set_hold_s(&daemon, 1);
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        fixture("pre_tool_use_ask_user_question"),
    );
    let (agent, question) = card_where(&daemon, "the question card", |c| {
        parked(c, OriginKind::Question)
    })
    .await;
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(body, json!({}), "a released group answers nothing");
    let (_, degraded) = card_where(&daemon, "the degraded group card", |c| {
        c.id == question.id && c.deliver == Deliver::Focus
    })
    .await;

    let (status, body) = hook(&daemon, &auth, &permission_payload("date")).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({}), "the follow-up of the released group");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let now = current_card(&daemon, &agent).expect("the agent still has its card");
    assert_eq!(
        now.id, degraded.id,
        "a permission card landed on top of the group's: {now:?}"
    );
    assert_eq!(now.deliver, Deliver::Focus);
    daemon.shutdown().await;

    // 3. Past the window it is a permission of its own again. 30 s is not a
    //    test, so the boundary is driven through the adapter's injected
    //    clock rather than by waiting.
    let adapter = Claude::default();
    let t0 = Instant::now();
    let ask = HookPayload(permission_payload("date"));
    let released: deck_model::CardSet =
        serde_json::from_value(json!({"agent": "ag_1", "type": "q", "text": "Colour?",
            "choices": [{"label": "Red"}, {"label": "Green"}], "deliver": "hold",
            "origin": {"kind": "question", "group": {"index": 0, "count": 1}}}))
        .expect("a question card");
    adapter.released(
        &ask,
        &released,
        deck_model::CardId(1),
        deck_model::Release::Snooze,
        t0,
    );
    let ctx = |at: Instant| HookCtx {
        now: Some(at),
        ..HookCtx::default()
    };
    assert!(matches!(
        adapter.ingest(&ask, &ctx(t0 + claude::RELEASED - Duration::from_millis(1))),
        Ingest::Nothing
    ));
    assert!(
        matches!(
            adapter.ingest(&ask, &ctx(t0 + claude::RELEASED + Duration::from_millis(1))),
            Ingest::Held { .. }
        ),
        "past 30 s the operator is being asked something new"
    );
}

// ---------------------------------------------------------------------------
// §6 row 4

/// Every way a hold ends without a decision: the operator snoozes it, the
/// budget runs out, or the handler goes away (`HoldGone`). All three answer
/// `{}` — the CLI's own dialog is on screen and a decision written now would
/// be one nobody asked for — and all three leave the **same card id** on the
/// deck as `⌨ Focus terminal`: `deliver: focus`, no new chime.
#[tokio::test]
async fn a_released_hold_degrades_to_focus_terminal_under_the_same_id() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let mut deck = peer(&daemon, "deck", &["observe", "operate"]).await;

    // 1. Snooze.
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        permission_payload("git status"),
    );
    let (agent, card) = card_where(&daemon, "the snoozed card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    deck.call("op.snooze", json!({"agent": agent, "card": card.id}))
        .await;
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!((status, body), (200, json!({})), "a snooze decides nothing");
    let (_, degraded) = card_where(&daemon, "the degraded snooze", |c| {
        c.id == card.id && c.deliver == Deliver::Focus
    })
    .await;
    assert_eq!(
        degraded.status,
        CardStatus::Degraded { focused_at: None },
        "same id, no new chime"
    );
    assert!(degraded.expires.is_none(), "a focus card does not expire");

    // 2. The hold budget.
    set_hold_s(&daemon, 1);
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        permission_payload("git diff"),
    );
    let (_, card) = card_where(&daemon, "the expiring card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    assert_eq!(budget_ms(&card), 1_000, "the live config reached this card");
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(
        (status, body),
        (200, json!({})),
        "an expiry decides nothing"
    );
    let (_, degraded) = card_where(&daemon, "the degraded expiry", |c| {
        c.id == card.id && c.deliver == Deliver::Focus
    })
    .await;
    assert_eq!(degraded.status, CardStatus::Degraded { focused_at: None });

    // 3. The handler goes away: the armed `HoldGuard` is the only thing
    //    that can still report the card, and its drop is what does it (B6).
    set_hold_s(&daemon, 120);
    let adapters = daemon.api.adapters.clone();
    let core = daemon.api.core.clone();
    let payload = permission_payload("uname -a");
    let dropped = tokio::spawn(async move {
        deckd::api::hooks::ingest(&adapters, &core, "claude", json!({"payload": payload})).await
    });
    let (_, card) = card_where(&daemon, "the orphaned card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    dropped.abort();
    let (agent, degraded) = card_where(&daemon, "the degraded hold_gone", |c| {
        c.id == card.id && c.deliver == Deliver::Focus
    })
    .await;
    assert_eq!(degraded.status, CardStatus::Degraded { focused_at: None });

    // The degraded card belongs to the turn now, and the turn's end takes
    // it off (§5.6) — it is a pointer at a dialog, and the dialog is gone.
    // The call it asked about finished: that call's `PostToolUse`.
    let mut ran = permission_payload("uname -a");
    ran["hook_event_name"] = "PostToolUse".into();
    let (status, body) = hook(&daemon, &auth, &ran).await;
    assert_eq!((status, body), (200, json!({})));
    let deadline = Instant::now() + PATIENCE;
    while current_card(&daemon, &agent).is_some_and(|c| c.id == degraded.id) {
        assert!(
            Instant::now() < deadline,
            "the degraded card outlived its turn"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// The card-pushing notifications, against a parked hold

/// A `Notification` that raises a focus card, with a card of this session
/// already on the deck.
fn notification(kind: &str) -> serde_json::Value {
    let mut payload = fixture("notification_agent_needs_input");
    payload["notification_type"] = kind.into();
    payload
}

/// The agent's state right now.
fn state_of(daemon: &Daemon, agent: &str) -> Option<deck_model::AgentState> {
    daemon
        .api
        .bus
        .snapshot()
        .agents
        .get(&deck_model::AgentId(agent.to_string()))
        .map(|a| a.state)
}

/// Wait until `f` accepts this agent's state.
async fn state_where(
    daemon: &Daemon,
    agent: &str,
    what: &str,
    f: impl Fn(deck_model::AgentState) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    while !state_of(daemon, agent).is_some_and(&f) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; state is {:?}",
            state_of(daemon, agent)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Post one notification and wait until its plan has landed.
///
/// Report-only plans are answered first and run behind the answer on a
/// task, one at a time per daemon (`Adapters::order`). So a
/// `UserPromptSubmit` posted after the notification lands after it too,
/// and its `state: working` is the signal that the notification is done —
/// including the notifications whose whole plan is "do nothing", which
/// have no signal of their own.
async fn notify_and_settle(daemon: &Daemon, auth: &str, agent: &str, kind: &str) {
    let (status, body) = hook(daemon, auth, &notification(kind)).await;
    assert_eq!((status, body), (200, json!({})), "{kind}");
    // The barrier is a report that lands behind the notification in the
    // same order and changes something visible: a parallel `Read` failing.
    // It is not the call the card asked about, so it must leave the card
    // alone (a new prompt would rightly take it off: its dialog is gone).
    let mut failed = notification(kind);
    failed["hook_event_name"] = "PostToolUseFailure".into();
    failed["tool_name"] = "Read".into();
    failed["tool_input"] = json!({"file_path": "/w/project/missing.txt"});
    let (status, body) = hook(daemon, auth, &failed).await;
    assert_eq!((status, body), (200, json!({})), "{kind}");
    state_where(daemon, agent, "the report behind the notification", |s| {
        s == deck_model::AgentState::Working
    })
    .await;
}

/// `agent.card.set` **retires** the agent's current card, and retiring a
/// parked hold emits `ReleaseHold{Expiry}` (`deck-model/src/cards.rs:314`).
/// So a `Notification` that raises a focus card while one of this adapter's
/// own holds is parked would answer the CLI's still-open hook with `{}` —
/// the permission dialog the operator is reading decides itself, and the
/// card goes off the deck under them.
///
/// Every notification type that raises a card asks first, this row is the
/// proof for all of them, and the two holds it protects are the two this
/// adapter parks: a `PermissionRequest` and a question group.
#[tokio::test]
async fn a_card_pushing_notification_never_releases_a_parked_hold() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let mut deck = peer(&daemon, "deck", &["observe", "operate"]).await;

    // 1. A parked `PermissionRequest`.
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        permission_payload("git push --force"),
    );
    let (agent, card) = card_where(&daemon, "the parked permission card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;

    for kind in [
        "agent_needs_input",
        "elicitation_dialog",
        "elicitation_url_dialog",
        "permission_prompt",
        "worker_permission_prompt",
    ] {
        notify_and_settle(&daemon, &auth, &agent, kind).await;
        assert!(
            !posted.is_finished(),
            "{kind} released the hook the operator is still answering"
        );
        let still = current_card(&daemon, &agent).expect("the card is still on the deck");
        assert_eq!(still.id, card.id, "{kind} replaced the parked card");
        assert_eq!(still.deliver, Deliver::Hold, "{kind}");
        assert_eq!(still.origin.kind, OriginKind::Permission, "{kind}");
        assert_eq!(labels(&still), vec!["Allow", "Deny"], "{kind}");
    }

    // The operator's own answer still reaches Claude: the hold was never
    // touched.
    deck.call(
        "op.answer",
        json!({"agent": agent, "card": card.id, "choice": 1}),
    )
    .await;
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": "Denied from the deck"},
        }})
    );

    // 2. A parked question group, which is the hold C8 says owns its dialog
    //    for the whole hold: releasing it loses the answers already given.
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        fixture("pre_tool_use_ask_user_question"),
    );
    let (agent, first) = card_where(&daemon, "the first question", |c| {
        parked(c, OriginKind::Question)
    })
    .await;
    let group = first.origin.group.expect("a group card");
    assert!(group.count >= 2, "a group of one proves nothing: {group:?}");

    for kind in ["agent_needs_input", "elicitation_dialog"] {
        notify_and_settle(&daemon, &auth, &agent, kind).await;
        assert!(!posted.is_finished(), "{kind} released the question group");
        let still = current_card(&daemon, &agent).expect("the question is still on the deck");
        assert_eq!(still.id, first.id, "{kind} replaced the question");
        assert_eq!(still.origin.kind, OriginKind::Question, "{kind}");
    }

    // The group still answers card by card, under the budget it started
    // with.
    let mut card = first;
    for index in 0..group.count {
        deck.call(
            "op.answer",
            json!({"agent": agent, "card": card.id, "choice": 0}),
        )
        .await;
        if index + 1 == group.count {
            break;
        }
        let (_, next) = card_where(&daemon, "the next question", |c| {
            parked(c, OriginKind::Question) && c.id != card.id
        })
        .await;
        card = next;
    }
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(
        body["hookSpecificOutput"]["hookEventName"], "PreToolUse",
        "{body}"
    );
    assert!(
        body["hookSpecificOutput"]["updatedInput"]["answers"].is_object(),
        "the group answered as one: {body}"
    );
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// §6 row 5

/// §4.11: a report-only event is answered **before** the work behind it
/// runs. They are synchronous http hooks with `timeout: 2`, so a daemon
/// that did the roster write first would put its own latency on every
/// prompt the operator types.
///
/// The proof holds the lock every deferred plan takes: with it held, the
/// core cannot have seen anything, and the door answers `{}` anyway.
#[tokio::test]
async fn report_only_events_answer_empty_before_the_core_sees_them() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);

    let gate = daemon.api.adapters.order().await;
    let (status, body) = hook(&daemon, &auth, &fixture("user_prompt_submit")).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({}));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        daemon.api.bus.snapshot().agents.is_empty(),
        "the answer went out before the roster changed"
    );
    drop(gate);

    daemon
        .until("the prompt reaches the roster", |s| {
            s.agents.values().any(|a| a.kind == Kind::Claude)
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    let agent = snapshot
        .agents
        .values()
        .find(|a| a.kind == Kind::Claude)
        .expect("the CLI session");
    assert_eq!(agent.name, "project", "the last path component of the cwd");
    assert_eq!(agent.state, deck_model::AgentState::Working);
    assert_eq!(agent.title.as_deref(), Some("SCRUBBED-PROMPT"));
    assert_eq!(agent.title_source, deck_model::TitleSource::Prompt);
    assert_eq!(agent.session.key.as_deref(), Some("claude:abc123"));

    // Every other report-only event answers the same way, and none of them
    // parks: the whole set is answered well inside one `PATIENCE`.
    for name in [
        "post_tool_use",
        "stop",
        "stop_failure",
        "notification_idle_prompt",
        "notification_permission_prompt",
        "notification_agent_needs_input",
        "notification_computer_use_enter",
        "pre_tool_use_bash",
        "session_end",
    ] {
        let (status, body) = hook(&daemon, &auth, &fixture(name)).await;
        assert_eq!((status, body), (200, json!({})), "{name}");
    }
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// §6 row 6

/// C25: the stamp is a one-shot beacon on `SessionStart` and a **complete**
/// OSC 2 sequence. Claude's allowlist takes OSC 0/1/2/9/99/777 terminated by
/// BEL or ST and silently drops the whole field for anything else, so a
/// missing terminator is a stamp that never reaches the terminal and a
/// resolver (C26) that never finds the tab.
#[tokio::test]
async fn the_terminal_sequence_is_a_complete_osc2_stamp() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let (status, body) = hook(&daemon, &auth, &fixture("session_start")).await;
    assert_eq!(status, 200);

    let stamp = claude::stamp(SESSION);
    assert_eq!(
        body,
        json!({
            "hookSpecificOutput": {"hookEventName": "SessionStart"},
            "terminalSequence": format!("\u{1b}]2;{stamp}\u{7}"),
        }),
        "ESC ] 2 ; deck:<4 hex> BEL, and the event that fired (C2)"
    );
    let sequence = body["terminalSequence"].as_str().expect("the sequence");
    assert_eq!(sequence.as_bytes()[0], 0x1b, "ESC");
    assert_eq!(&sequence[1..4], "]2;", "OSC 2 is the window title");
    assert_eq!(*sequence.as_bytes().last().unwrap(), 0x07, "BEL terminates");
    assert_eq!(stamp.len(), 9, "`deck:` plus four hex");
    assert!(stamp.starts_with("deck:"));

    // The register carries the same stamp: that is what the resolver looks
    // for in a Ghostty title before it pins the tab (C26).
    daemon
        .until("the session joins the roster", |s| {
            s.agents.values().any(|a| a.kind == Kind::Claude)
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    let agent = snapshot
        .agents
        .values()
        .find(|a| a.kind == Kind::Claude)
        .expect("the CLI session");
    assert_eq!(
        agent
            .session
            .terminal
            .as_ref()
            .and_then(|t| t.title_stamp.clone()),
        Some(stamp),
        "the stamp the beacon wrote is the stamp the roster carries"
    );

    // One shot: no later event re-emits it (§5.1's plan of re-stamping on
    // every turn is dropped — a Ghostty tab's name is wiped by the next
    // OSC 2 anyway, so deckd pins the tab instead).
    for name in ["user_prompt_submit", "stop", "post_tool_use"] {
        let (status, body) = hook(&daemon, &auth, &fixture(name)).await;
        assert_eq!((status, body), (200, json!({})), "{name} re-stamped");
    }
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// The door's own rows

/// The hook door is the `/v1/rpc` door's twin: no bearer is `401` with a
/// B8: `/v1/info.capabilities` names the adapters this daemon actually
/// answers for, and nothing else.
///
/// It is the one signal that separates the two silences an operator sees
/// when no card arrives: hooks installed but this build has no adapter for
/// the CLI, versus hooks installed and simply not firing (C11 — Claude
/// skips an http hook outright when the process's proxy/TLS/resolver
/// environment differs from the person's own, with nothing on screen).
/// `integrations.status` reads this, so an adapter that lands without its
/// entry makes that check answer the wrong question.
#[tokio::test]
async fn the_daemon_advertises_the_adapter_it_answers_for() {
    let daemon = Daemon::start().await;
    let (status, info) = daemon.http("GET", "/v1/info", &[], "").await;
    assert_eq!(status, 200, "{info}");
    let capabilities: Vec<&str> = info["capabilities"]
        .as_array()
        .expect("the capability list")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for built in ["hooks.claude", "hooks.codex"] {
        assert!(
            capabilities.contains(&built),
            "{built} is answered, so the daemon says so: {capabilities:?}"
        );
    }
    let unbuilt = "hooks.gemini";
    assert!(
        !capabilities.contains(&unbuilt),
        "{unbuilt} is advertised but the door refuses it: {capabilities:?}"
    );
    daemon.shutdown().await;
}

/// JSON-RPC `-32001` body, and a CLI this daemon has no adapter for is
/// refused by name with the release that will answer it.
#[tokio::test]
async fn the_hook_door_refuses_a_post_without_a_token() {
    let daemon = Daemon::start().await;
    let payload = fixture("session_start").to_string();
    let (status, body) = daemon.http("POST", "/v1/hooks/claude", &[], &payload).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["code"], deck_model::ApiError::UNAUTHORIZED);
    assert!(
        daemon.api.bus.snapshot().agents.is_empty(),
        "an unauthenticated post never reaches the core"
    );

    // A token is not enough for a CLI that has no adapter here.
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let (status, body) = daemon
        .http(
            "POST",
            "/v1/hooks/gemini",
            &[("Authorization", &auth)],
            &payload,
        )
        .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(
        body["error"]["code"],
        deck_model::ApiError::METHOD_NOT_FOUND
    );
    assert_eq!(body["error"]["data"]["available"], "v1.1");
    // The Codex adapter is on the other plan layer and reachable through
    // the same door: an event it does not know is `200 {}`, never a 404.
    let (status, body) = daemon
        .http("POST", "/v1/hooks/codex", &[("Authorization", &auth)], "{}")
        .await;
    assert_eq!((status, body), (200, json!({})), "the codex door answers");
    let (status, _) = daemon
        .http(
            "POST",
            "/v1/hooks/cursor",
            &[("Authorization", &auth)],
            &payload,
        )
        .await;
    assert_eq!(status, 404, "a CLI that is not in the method table");
    daemon.shutdown().await;
}

/// `hooks:ingest` and nothing else opens this door (§4.2). A token with
/// every other scope is refused `-32002 {need:"hooks:ingest"}` — by name,
/// rather than by pretending the route is missing.
#[tokio::test]
async fn the_hook_door_refuses_a_token_without_the_hooks_ingest_scope() {
    let daemon = Daemon::start().await;
    let payload = fixture("permission_request").to_string();
    for (name, scopes) in [
        ("watcher", vec![Scope::Observe]),
        ("bot", vec![Scope::Agent, Scope::Operate]),
        ("speaker", vec![Scope::Voice, Scope::Device]),
    ] {
        let auth = daemon.bearer(name, &scopes);
        let (status, body) = daemon
            .http(
                "POST",
                "/v1/hooks/claude",
                &[("Authorization", &auth)],
                &payload,
            )
            .await;
        assert_eq!(status, 403, "{name}: {body}");
        assert_eq!(body["error"]["code"], deck_model::ApiError::FORBIDDEN);
        assert_eq!(body["error"]["data"]["need"], "hooks:ingest", "{name}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        daemon.api.bus.snapshot().agents.is_empty(),
        "a refused post never reaches the core"
    );

    // `admin` stands in for every scope but `observe:raw` (§4.2), so it
    // opens this door too — asserted with a report-only event, because an
    // authorised held one would park for the whole budget.
    let auth = daemon.bearer("root", &[Scope::Admin]);
    let (status, body) = hook(&daemon, &auth, &fixture("user_prompt_submit")).await;
    assert_eq!(
        (status, body),
        (200, json!({})),
        "admin covers hooks:ingest"
    );
    daemon.shutdown().await;
}

/// S1 measured a `POST /v1/rpc` held open for 140.048 s, and nothing in the
/// door, hyper or axum cuts one: a held hook may park for its whole budget.
/// This row asserts both halves of that — the installed 120 s budget reaches
/// the card **intact** (nothing clips it on the way), and the door then
/// holds the request open for the whole of a budget a test can afford to
/// wait out, answering `{}` at the expiry rather than early.
#[tokio::test]
async fn a_held_hook_runs_its_whole_budget_and_the_door_never_cuts_it() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let mut deck = peer(&daemon, "deck", &["observe", "operate"]).await;

    // The pair, asserted rather than assumed: a change to either side
    // that broke the ten-second headroom would otherwise only show up on
    // a real terminal.
    assert!(
        u64::from(INSTALLED_HOLD_S) + 10 <= deckd::integrations::claude::HELD_TIMEOUT_S,
        "hold_s must end before the CLI's own hook timeout"
    );
    set_hold_s(&daemon, INSTALLED_HOLD_S);
    let posted = post_hook(
        daemon.port,
        auth.clone(),
        "claude",
        permission_payload("cargo test"),
    );
    let (agent, card) = card_where(&daemon, "the 120 s card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    assert_eq!(
        budget_ms(&card),
        u64::from(INSTALLED_HOLD_S) * 1000,
        "the whole 120 s reaches the card: nothing on the way clips it"
    );
    // Deny is never guarded, so the deck can end this one without waiting
    // 120 s for it.
    deck.call(
        "op.answer",
        json!({"agent": agent, "card": card.id, "choice": 1}),
    )
    .await;
    let (status, body) = posted.await.expect("the hook task lives");
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": "Denied from the deck"},
        }})
    );

    // The same path at a budget a test can sit through: the request stays
    // open for all of it and is answered by the expiry, not by a timeout of
    // ours.
    let budget = Duration::from_secs(2);
    set_hold_s(&daemon, 2);
    let started = Instant::now();
    let posted = post_hook(
        daemon.port,
        auth,
        "claude",
        permission_payload("cargo clippy"),
    );
    let (_, card) = card_where(&daemon, "the expiring card", |c| {
        parked(c, OriginKind::Permission)
    })
    .await;
    let (status, body) = posted.await.expect("the hook task lives");
    let held_for = started.elapsed();
    assert_eq!((status, body), (200, json!({})));
    assert!(
        held_for + Duration::from_millis(100) >= budget,
        "the door cut the hold after {held_for:?}, short of {budget:?}"
    );
    let (_, degraded) = card_where(&daemon, "the degraded card", |c| {
        c.id == card.id && c.deliver == Deliver::Focus
    })
    .await;
    assert_eq!(degraded.status, CardStatus::Degraded { focused_at: None });
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// The fixtures themselves

/// §9: a hook payload is the most sensitive data the daemon ever holds, and
/// a fixture is one that was written down. Every committed payload is
/// scrubbed — no real path, no real session, no prompt, no assistant reply,
/// no command output — and this is the test that keeps it that way when the
/// next fixture is recorded.
#[test]
fn every_fixture_is_scrubbed() {
    let names = fixture_names();
    assert!(
        names.len() >= 15,
        "the recorded set shrank: {names:?} — a fixture may be added, never quietly dropped"
    );
    // A real capture carries these; a scrubbed one never does.
    let forbidden = [
        "/users/",
        "/home/",
        "tony",
        "openwalkietalkie",
        ".claude/projects",
        "@",
    ];
    for name in &names {
        let payload = fixture(name);
        let mut found = Vec::new();
        strings(&payload, &mut found);
        for text in &found {
            let folded = text.to_lowercase();
            for needle in forbidden {
                assert!(
                    !folded.contains(needle),
                    "{name}.json still carries {needle:?}: {text:?}"
                );
            }
        }
        assert_eq!(payload["session_id"], SESSION, "{name}");
        assert_eq!(payload["cwd"], "/w/project", "{name}");
        assert!(
            payload["transcript_path"]
                .as_str()
                .is_some_and(|p| p.starts_with("/w/project/")),
            "{name}: {}",
            payload["transcript_path"]
        );
        assert!(
            payload["hook_event_name"].as_str().is_some(),
            "{name} has no event"
        );
    }
    // The fields that would carry the operator's own words are the marker
    // and nothing else.
    assert_eq!(fixture("user_prompt_submit")["prompt"], "SCRUBBED-PROMPT");
    assert_eq!(fixture("stop")["last_assistant_message"], "SCRUBBED-REPLY");
    assert_eq!(
        fixture("post_tool_use")["tool_response"]["stdout"],
        "SCRUBBED-OUTPUT"
    );
    assert_eq!(
        fixture("notification_computer_use_enter")["message"],
        "SCRUBBED-NOTIFICATION"
    );
}

/// The shape of an `Ingest`, for the golden table.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    Report,
    Beacon,
    Held,
    Nothing,
}

fn shape(ingest: &Ingest) -> Shape {
    match ingest {
        Ingest::Report(_) => Shape::Report,
        Ingest::Beacon(..) => Shape::Beacon,
        Ingest::Held { .. } => Shape::Held,
        Ingest::Nothing => Shape::Nothing,
    }
}

/// Every recorded payload, replayed through `ingest` against its golden
/// answer. A fresh adapter per fixture: C5's windows are a memo, and a
/// replay that shared one would be testing the order of the table.
///
/// A fixture with no row here fails the test rather than passing silently —
/// the next event someone records has to say what the adapter does with it.
#[test]
fn every_fixture_replays_to_its_golden_ingest() {
    let golden = [
        ("notification_agent_needs_input", Shape::Report),
        // C10: 17 types exist where the docs list 12. An unknown one is
        // ignored, never a panic and never a card.
        ("notification_computer_use_enter", Shape::Nothing),
        ("notification_idle_prompt", Shape::Report),
        ("notification_permission_prompt", Shape::Report),
        ("permission_request", Shape::Held),
        // C5: the question group's own follow-up.
        ("permission_request_askuserquestion", Shape::Nothing),
        ("permission_request_no_suggestions", Shape::Held),
        ("post_tool_use", Shape::Report),
        ("pre_tool_use_ask_user_question", Shape::Held),
        // Installed with matcher `AskUserQuestion`: any other tool on this
        // event is somebody else's hook reaching our door.
        ("pre_tool_use_bash", Shape::Nothing),
        // A session this adapter never saw has no agent to release.
        ("session_end", Shape::Nothing),
        ("session_start", Shape::Beacon),
        ("stop", Shape::Report),
        ("stop_failure", Shape::Report),
        ("user_prompt_submit", Shape::Report),
    ];
    let table: Vec<&str> = golden.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        fixture_names(),
        table,
        "every fixture needs a golden, and every golden a fixture"
    );

    let ctx = HookCtx {
        now: Some(Instant::now()),
        ..HookCtx::default()
    };
    for (name, want) in &golden {
        let payload = HookPayload(fixture(name));
        let adapter = Claude::default();
        let ingest = adapter.ingest(&payload, &ctx);
        assert_eq!(&shape(&ingest), want, "{name}");

        match (*name, ingest) {
            (
                "permission_request",
                Ingest::Held {
                    cmd, note, group, ..
                },
            ) => {
                assert_eq!(cmd.text, "Bash: rm -rf node_modules");
                assert_eq!(
                    cmd.choices
                        .iter()
                        .map(|c| c.label.as_str())
                        .collect::<Vec<_>>(),
                    vec!["Allow", "Allow always", "Deny"]
                );
                assert_eq!(cmd.deliver, Deliver::Hold);
                assert_eq!(cmd.origin.kind, OriginKind::Permission);
                assert_eq!(cmd.timeout_s, Some(120));
                assert!(group.is_none(), "a permission is a group of one");
                assert_eq!(note["event"], "PermissionRequest");
                assert_eq!(
                    note["suggestions"],
                    fixture("permission_request")["permission_suggestions"]
                );
            }
            ("permission_request_no_suggestions", Ingest::Held { cmd, .. }) => {
                assert_eq!(
                    cmd.choices
                        .iter()
                        .map(|c| c.label.as_str())
                        .collect::<Vec<_>>(),
                    vec!["Allow", "Deny"],
                    "nothing to persist, no `Allow always`"
                );
            }
            (
                "pre_tool_use_ask_user_question",
                Ingest::Held {
                    cmd, note, group, ..
                },
            ) => {
                let plan = group.expect("a three-question group");
                assert_eq!(cmd.origin.group, Some(Group { index: 0, count: 3 }));
                assert_eq!(plan.rest.len(), 2);
                assert_eq!(
                    plan.questions
                        .iter()
                        .map(|q| q.key.as_str())
                        .collect::<Vec<_>>(),
                    vec![
                        "Which colour do you prefer?",
                        "Which size fits?",
                        "Which extras do you want?",
                    ],
                    "answers are keyed by the question text"
                );
                // The labels the CLI sent are kept beside the ones the
                // deck draws: an answer is mapped back through these.
                for (question, card) in plan
                    .questions
                    .iter()
                    .zip(std::iter::once(&cmd).chain(plan.rest.iter()))
                {
                    assert_eq!(question.options.len(), card.choices.len());
                }
                // C7: 2–4 options, schema-enforced, so no truncation path.
                for card in std::iter::once(&cmd).chain(plan.rest.iter()) {
                    assert!((2..=deck_model::MAX_CHOICES).contains(&card.choices.len()));
                    assert_eq!(card.origin.kind, OriginKind::Question);
                    assert_eq!(card.deliver, Deliver::Hold);
                }
                assert_eq!(note["event"], "PreToolUse");
            }
            ("session_start", Ingest::Beacon(out, plan)) => {
                assert_eq!(
                    out.json(),
                    json!({
                        "hookSpecificOutput": {"hookEventName": "SessionStart"},
                        "terminalSequence": claude::terminal_sequence(SESSION),
                    })
                );
                assert!(
                    matches!(plan.first(), Some(deck_model::Request::AgentRegister(_))),
                    "every plan leads with the register"
                );
            }
            ("stop", Ingest::Report(plan)) => {
                assert!(
                    plan.iter()
                        .any(|r| matches!(r, deck_model::Request::Reply(_))),
                    "`last_assistant_message` is the reply, not the transcript"
                );
            }
            _ => {}
        }
    }
}

/// The settle window, end to end: an ask that an auto-approver settles —
/// the call's own `PostToolUse` arrives inside the window — is answered
/// `{}` and never becomes a card; one that nobody settles reaches the deck
/// when the window ends.
#[tokio::test]
async fn only_an_ask_nothing_else_decided_reaches_the_deck() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    set_settle_s(&daemon, 2);

    let approved = permission_payload("cargo build --release");
    let posted = post_hook(daemon.port, auth.clone(), "claude", approved.clone());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut ran = approved.clone();
    ran["hook_event_name"] = "PostToolUse".into();
    let (status, _) = hook(&daemon, &auth, &ran).await;
    assert_eq!(status, 200);
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), posted)
        .await
        .expect("settled inside the window")
        .expect("the hook task lives");
    assert_eq!((status, body), (200, json!({})), "the CLI's own decision stands");
    // Past the window: nothing ever reached the deck.
    tokio::time::sleep(Duration::from_millis(2_300)).await;
    assert!(
        daemon.api.bus.snapshot().agents.values().all(|a| a.card.is_none()),
        "an auto-approved ask never becomes a card"
    );

    // Nobody decides this one: the deck gets it after the window.
    let at = Instant::now();
    let _posted = post_hook(daemon.port, auth.clone(), "claude", permission_payload("psql -f drop.sql"));
    let (_, card) = card_where(&daemon, "the unsettled ask", |c| parked(c, OriginKind::Permission)).await;
    assert!(at.elapsed() >= Duration::from_secs(2), "not before the window: {:?}", at.elapsed());
    assert_eq!(card.origin.tool.as_deref(), Some("Bash"));
}

/// A card the terminal answered is gone with the call: `PostToolUse` of
/// **that** call takes it off, a parallel tool's does not, and a new
/// prompt takes off whatever an interrupted turn left behind.
#[tokio::test]
async fn a_card_goes_with_its_own_call_and_with_the_next_prompt() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let ask = permission_payload("rm -rf target");
    let _posted = post_hook(daemon.port, auth.clone(), "claude", ask.clone());
    let (agent, _) = card_where(&daemon, "the ask", |c| parked(c, OriginKind::Permission)).await;
    let on_deck = |daemon: &Daemon| {
        daemon.api.bus.snapshot().agents.values().find(|a| a.id.0 == agent).and_then(|a| a.card.clone())
    };
    // A parallel Read finishing leaves it alone.
    let mut read = ask.clone();
    read["hook_event_name"] = "PostToolUse".into();
    read["tool_name"] = "Read".into();
    read["tool_input"] = json!({"file_path": "/w/project/README.md"});
    hook(&daemon, &auth, &read).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(on_deck(&daemon).is_some(), "a parallel tool's PostToolUse is not this call");
    // The operator interrupts in the terminal (no `Stop`) and types again.
    let mut prompt = ask.clone();
    prompt["hook_event_name"] = "UserPromptSubmit".into();
    prompt["prompt"] = "never mind, do it another way".into();
    hook(&daemon, &auth, &prompt).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while on_deck(&daemon).is_some() {
        assert!(Instant::now() < deadline, "the new prompt takes the stale card off");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
