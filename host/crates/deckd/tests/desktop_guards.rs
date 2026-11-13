//! `op.order` → `via:"inject"` → the guards → `order.dispatched` /
//! `order.failed`, and `op.focus` → the hand-off, driven through the **real
//! core** with the desktop side detached (design §5.5, §4.6, §4.7, §4.12).
//!
//! An `inject` order is one the daemon delivers itself, by Claude Channels or
//! the session's inbox; with neither it fails `no_route` after its dispatch,
//! and nothing is ever typed into a terminal. A hand-off raises the
//! session's own app on an explicit press. What the unit tests beside each
//! piece cannot prove is the *wiring*: that the store routes an order at
//! all, that `order.dispatched` goes out **before** the route reports, that
//! a refusal becomes an `order.failed` with the right reason and the right
//! principal, that the guards the core owns refuse before anything leaves
//! it, and that only a hook session's app is ever raised.
//!
//! Nothing here raises an app. `DesktopHandle::detached()` hands the test
//! the far end of the same bounded channel the daemon uses, so a job is an
//! assertion and an answer is a `send`.
mod core_support;

use core_support::{agent_of, build, card_of, card_status, claude, hold_register, uid, Parts};
use deck_model::{
    AgentId, AgentRegister, AnswerCap, Caps, CardKind, CardSet, CardStatus, Choice, Command,
    Deliver, Event, EventKind, Kind, OpAnswer, OpOrder, OrderCap, OrderSource, Origin, OriginKind,
    PidKind, Principal, Request, SessionId, Terminal, VoiceIn, VoiceInput, VoiceListen,
};
use deckd::core::Core;
use deckd::desktop::{DesktopHandle, DesktopJob, FromDesktop, Outcome};
use std::time::Instant;

/// The adapter session every hook-owned agent here is registered from.
const ADAPTER: SessionId = SessionId(9);

/// A Claude session as its hook helper reports it: an `inject` cap, a
/// `cli` pid, a tty, and the app it runs in.
fn resolved_register(name: &str) -> AgentRegister {
    AgentRegister {
        pid: Some(std::process::id()),
        pid_kind: Some(PidKind::Cli),
        terminal: Some(Terminal {
            program: Some("ghostty".into()),
            tty: Some("/dev/ttys004".into()),
            app: Some("com.mitchellh.ghostty".into()),
            ..Terminal::default()
        }),
        key: Some(format!("claude:{name}")),
        ..hold_register(name)
    }
}

/// A connected app's agent that *claims* a terminal, a pid and an app:
/// §4.6 says all of them are informational.
fn self_declared_register(name: &str) -> AgentRegister {
    AgentRegister {
        kind: Kind::Other,
        name: name.into(),
        key: Some(format!("myapp:{name}")),
        cwd: None,
        pid: Some(std::process::id()),
        pid_kind: Some(PidKind::Cli),
        terminal: Some(Terminal {
            program: Some("ghostty".into()),
            tty: Some("/dev/ttys004".into()),
            app: Some("com.apple.Terminal".into()),
            ..Terminal::default()
        }),
        lease_ttl_s: 30,
        caps: Caps {
            answer: AnswerCap::Poll,
            // `inject` is adapter-only at registration; a script asking for
            // it is refused there. `push` is what it can have.
            order: OrderCap::Push,
            focus: true,
            transcribe: Default::default(),
        },
        slot_hint: None,
        voice: None,
        meta: None,
    }
}

fn order(agent: &AgentId, text: &str) -> Request {
    Request::OpOrder(OpOrder {
        agent: agent.clone(),
        text: text.into(),
        ctx: "talk".into(),
        source: OrderSource::Ptt,
        utterance: None,
    })
}

/// A permission card from the adapter — guard 2's card half.
fn permission(agent: &AgentId) -> CardSet {
    CardSet {
        agent: agent.clone(),
        id: None,
        kind: CardKind::Q,
        text: "Bash: psql -f migrate.sql staging".into(),
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
        rec: Some(0),
        free_text: true,
        deliver: Deliver::Push,
        timeout_s: None,
        origin: Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: None,
            group: None,
        },
    }
}

/// A core with the adapter's session open and the desktop side in the
/// test's hands.
fn rig(
    name: &str,
) -> (
    Core,
    Parts,
    crossbeam_channel::Receiver<DesktopJob>,
    crossbeam_channel::Sender<FromDesktop>,
) {
    let (mut core, parts) = build(name, 4096);
    core.sessions
        .open_internal(ADAPTER, claude(), "claude-adapter", Instant::now());
    let (handle, jobs, answers) = DesktopHandle::detached();
    core.set_desktop(handle);
    (core, parts, jobs, answers)
}

fn register(core: &mut Core, r: AgentRegister, principal: Principal) -> AgentId {
    agent_of(&core.api_call(
        principal,
        ADAPTER,
        false,
        Request::AgentRegister(r),
        Instant::now(),
    ))
}

/// Every event the bus has, newest last.
///
/// `&mut` because the core batches a step's events in `pending` and the
/// daemon's loop publishes them between iterations; a test driving
/// `api_call` directly has to do the same, or it reads an empty ring.
fn events(core: &mut Core) -> Vec<Event> {
    core.publish();
    core.bus
        .replay(0, 500, None)
        .0
        .into_iter()
        .map(|e| (*e).clone())
        .collect()
}

fn of_kind(core: &mut Core, kind: EventKind) -> Vec<Event> {
    events(core)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

/// Take the message the test just sent back off the core's own receiver, so
/// no sleep decides anything.
fn answers_recv(core: &mut Core) -> FromDesktop {
    core.desktop
        .events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the desktop answer")
}

/// The happy path's first half, and then the truth about the rest: the
/// store routes the order and writes the exact string **before** any route
/// runs; a session with neither Claude Channels nor an inbox has no route,
/// so the order fails `no_route` with the words kept — whatever the hook
/// helper said about its terminal — and nothing reaches the desktop.
#[test]
fn an_inject_order_is_dispatched_then_fails_no_route_with_no_job() {
    for (label, mutate) in [
        (
            "a full terminal record",
            (|_: &mut AgentRegister| {}) as fn(&mut AgentRegister),
        ),
        ("a shell pid", |r: &mut AgentRegister| {
            r.pid_kind = Some(PidKind::Shell)
        }),
        ("no tty", |r: &mut AgentRegister| {
            r.terminal.as_mut().unwrap().tty = None;
        }),
        ("no terminal record", |r: &mut AgentRegister| {
            r.terminal = None;
        }),
    ] {
        let (mut core, _parts, jobs, _answers) = rig("desktop-no-route");
        let mut r = resolved_register("cohdl-api");
        mutate(&mut r);
        let agent = register(&mut core, r, claude());
        let result = core.api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "deploy the api"),
            Instant::now(),
        );
        assert_eq!(result.unwrap()["via"], "inject", "{label}");
        let all = events(&mut core);
        let dispatched = all
            .iter()
            .position(|e| e.kind == EventKind::OrderDispatched)
            .expect("order.dispatched");
        let failed = all
            .iter()
            .position(|e| e.kind == EventKind::OrderFailed)
            .expect("order.failed");
        assert!(dispatched < failed, "{label}");
        assert_eq!(all[dispatched].data["text"], "deploy the api");
        assert_eq!(all[dispatched].principal, Principal::Device);
        assert_eq!(all[failed].data["reason"], "no_route", "{label}");
        assert_eq!(all[failed].data["text"], "deploy the api", "{label}");
        assert_eq!(all[failed].data["text_kept"], true);
        assert_eq!(all[failed].data["order"], all[dispatched].data["order"]);
        assert_eq!(all[failed].principal, Principal::Device);
        assert!(jobs.try_recv().is_err(), "{label}: nothing is typed");
    }
}

/// **Guard 2, the card half**: while a permission card is open the order is
/// refused `-32006 undeliverable{reason:"dialog_open"}`, `order.failed`
/// says so, and nothing is dispatched — a message posted into an open
/// dialog would cancel it.
#[test]
fn an_open_permission_card_refuses_the_order_before_any_job_is_built() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-dialog-open");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    core.api_call(
        claude(),
        ADAPTER,
        false,
        Request::CardSet(permission(&agent)),
        Instant::now(),
    )
    .unwrap();

    let err = core
        .api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "yes do it"),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(err.code, deck_model::ApiError::UNDELIVERABLE);
    assert_eq!(err.data["reason"], "dialog_open");

    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].data["reason"], "dialog_open");
    assert_eq!(failed[0].data["text_kept"], true);
    assert!(
        of_kind(&mut core, EventKind::OrderDispatched).is_empty(),
        "refused before the dispatch, so nothing claims it was sent"
    );
    assert_eq!(
        jobs.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty),
        "the desktop was never asked"
    );
}

/// The same refusal for every card origin and status §5.5 lists, and *not*
/// for a `custom` card — a script's own card is not a terminal dialog and
/// must not be able to block dictation by existing. An order that is not
/// blocked is dispatched, and with no route it fails `no_route`.
#[test]
fn every_dialog_origin_and_status_blocks_an_order_and_a_custom_card_does_not() {
    for (origin, blocks) in [
        (OriginKind::Permission, true),
        (OriginKind::Question, true),
        (OriginKind::Elicitation, true),
        (OriginKind::Custom, false),
        (OriginKind::Ready, false),
    ] {
        let (mut core, _parts, jobs, _answers) = rig(&format!("desktop-origin-{origin:?}"));
        let agent = register(&mut core, resolved_register("cohdl-api"), claude());
        let mut card = permission(&agent);
        card.origin.kind = origin;
        if origin == OriginKind::Ready {
            card.kind = CardKind::R;
        }
        core.api_call(
            claude(),
            ADAPTER,
            false,
            Request::CardSet(card),
            Instant::now(),
        )
        .unwrap();
        let result = core.api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "carry on"),
            Instant::now(),
        );
        if blocks {
            assert_eq!(
                result.unwrap_err().data["reason"],
                "dialog_open",
                "{origin:?} is a terminal dialog"
            );
        } else {
            assert_eq!(result.unwrap()["via"], "inject", "{origin:?}");
            let failed = of_kind(&mut core, EventKind::OrderFailed);
            assert_eq!(failed[0].data["reason"], "no_route", "{origin:?}");
        }
        assert!(jobs.try_recv().is_err(), "{origin:?}");
    }
}

/// **Guard 1, the trust half**: an untrusted external principal with
/// `operate` that calls `op.order` on an `inject`-only agent is refused
/// `-32002 {need:"trusted"}` and gets the GUI confirmation path
/// (`op.confirm_requested`) — and nothing is dispatched.
#[test]
fn an_untrusted_operate_caller_is_refused_thirty_two_thousand_and_two() {
    let (mut core, _parts, _jobs, _answers) = rig("desktop-untrusted");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    core.guard.require_trusted = true;

    let err = core
        .api_call(
            uid(),
            ADAPTER,
            false,
            order(&agent, "rm -rf the staging bucket"),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(err.code, deck_model::ApiError::FORBIDDEN);
    assert_eq!(err.data["need"], "trusted");
    assert_eq!(err.data["how"], "gui_confirm");

    let asked = of_kind(&mut core, EventKind::OpConfirmRequested);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].data["agent"], agent.0);
    assert_eq!(asked[0].data["order"], "rm -rf the staging bucket");
    assert_eq!(asked[0].data["need"], "trusted");
    assert!(
        of_kind(&mut core, EventKind::OrderDispatched).is_empty(),
        "refused before the routing, so the order history never claims it"
    );

    // The trusted GUI's identical call goes through, which is what makes
    // the refusal about the principal and not about the text.
    core.api_call(
        uid(),
        ADAPTER,
        true,
        order(&agent, "run the tests"),
        Instant::now(),
    )
    .unwrap();
    let dispatched = of_kind(&mut core, EventKind::OrderDispatched);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].data["text"], "run the tests");
}

/// **A connected app's self-declared app is never raised.** What a
/// connected app says about its own terminal, pid and app is informational
/// (§4.6): it is shown, and used for nothing else. Such an app brings
/// itself forward on `card.focused` / `focus.changed`, so `op.focus` on its
/// agent asks nothing of the desktop and claims no failure.
///
/// Its orders are its own too: the adapter-only `inject` cap is refused at
/// registration, and `push` goes to its owner.
#[test]
fn a_connected_apps_self_declared_app_is_never_raised() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-virtual");

    // A script asking for the adapter-only cap is refused there (§4.6).
    let mut greedy = self_declared_register("impostor");
    greedy.caps.order = OrderCap::Inject;
    let err = core
        .api_call(
            uid(),
            ADAPTER,
            true,
            Request::AgentRegister(greedy),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(err.code, deck_model::ApiError::INVALID);
    assert_eq!(err.data["field"], "caps.order");
    assert_eq!(err.data["why"], "adapter_only");

    let agent = register(&mut core, self_declared_register("impostor"), uid());
    let claimed = &core.store.snapshot().agents[&agent].session;
    assert_eq!(
        claimed.terminal.as_ref().unwrap().app.as_deref(),
        Some("com.apple.Terminal"),
        "it is kept …"
    );
    let result = core
        .api_call(
            uid(),
            ADAPTER,
            true,
            order(&agent, "type this into the other tab"),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(result["via"], "push", "… and used for nothing else");
    core.api_call(
        uid(),
        ADAPTER,
        true,
        Request::OpFocus {
            agent: agent.clone(),
        },
        Instant::now(),
    )
    .unwrap();
    assert!(
        jobs.try_recv().is_err(),
        "a script cannot raise an app of its choosing"
    );
    assert!(
        core.desktop.events.try_recv().is_err(),
        "and nothing claims a hand-off failed"
    );
    assert!(
        of_kind(&mut core, EventKind::OrderFailed).is_empty(),
        "its own owner is a route, so nothing failed"
    );
}

/// Control characters are refused `-32009` before anything is dispatched:
/// a `\r` in an order is a Return the operator did not ask for.
#[test]
fn a_control_character_in_an_order_is_refused_thirty_two_thousand_and_nine() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-control-chars");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    for bad in ["deploy\rrm -rf /", "a\nb", "a\u{1b}[2Jb", "bell\u{7}"] {
        let err = core
            .api_call(
                Principal::Device,
                ADAPTER,
                true,
                order(&agent, bad),
                Instant::now(),
            )
            .unwrap_err();
        assert_eq!(err.code, deck_model::ApiError::INVALID, "{bad:?}");
        assert_eq!(err.data["field"], "text");
        assert_eq!(err.data["why"], "control_chars");
        assert!(jobs.try_recv().is_err(), "{bad:?}");
    }
    assert!(of_kind(&mut core, EventKind::OrderDispatched).is_empty());
    // A tab is not a control character for this purpose: it is a character
    // a dictated sentence can carry and it does nothing on its own.
    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "col\tumns"),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(of_kind(&mut core, EventKind::OrderDispatched).len(), 1);
}

/// `op.focus` on a hook session becomes a raise job for the app its helper
/// reported, and a hand-off that happened is silent on the wire: the app is
/// in front of the operator.
#[test]
fn op_focus_becomes_a_raise_job_that_is_silent_on_the_bus() {
    let (mut core, _parts, jobs, answers) = rig("desktop-focus");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    core.api_call(
        uid(),
        ADAPTER,
        true,
        Request::OpFocus {
            agent: agent.clone(),
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(
        jobs.try_recv(),
        Ok(DesktopJob::Raise {
            agent: agent.clone(),
            app: "com.mitchellh.ghostty".into(),
        })
    );

    let before = events(&mut core).len();
    answers
        .send(FromDesktop::Raised {
            agent,
            app: Some("com.mitchellh.ghostty".into()),
            outcome: Outcome::Raised,
        })
        .unwrap();
    let msg = answers_recv(&mut core);
    core.on_from_desktop(msg, Instant::now());
    assert_eq!(
        events(&mut core).len(),
        before,
        "a hand-off that happened publishes nothing"
    );
}

/// A daemon with **no** desktop side (`--no-device`, Linux) still delivers
/// and fails orders honestly: `op.order` answers `{via:"inject"}`, the
/// order history has the exact string, and with no route
/// `order.failed{reason:"no_route"}` says the truth and the deck is shown
/// it. A hand-off asks for nothing and claims nothing.
#[test]
fn a_daemon_with_no_desktop_fails_the_order_with_no_route_and_raises_nothing() {
    let (mut core, parts) = build("desktop-absent", 4096);
    core.sessions
        .open_internal(ADAPTER, claude(), "claude-adapter", Instant::now());
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    assert!(!core.desktop.engine());
    parts.drain();

    let result = core
        .api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "deploy the api"),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(result["via"], "inject");
    let dispatched = of_kind(&mut core, EventKind::OrderDispatched);
    assert_eq!(dispatched.len(), 1);
    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].data["reason"], "no_route");
    assert_eq!(failed[0].data["text"], "deploy the api");
    assert_eq!(failed[0].data["text_kept"], true);
    // The failure names the order it failed, and the deck is told: the
    // agent has nothing waiting on it, so it gets the failure card.
    assert_eq!(failed[0].data["order"], dispatched[0].data["order"]);
    assert_eq!(failed[0].data["order"], result["order"]);
    assert_eq!(failed[0].data["shown"], "card");
    assert!(parts.taken().iter().any(|op| matches!(
        op,
        deck_model::DeviceOp::Put(deck_model::Key::Card(..), deck_model::HostLine::Card { text, .. })
            if text == deck_model::NOT_DELIVERED.text[0]
    )));

    core.api_call(
        uid(),
        ADAPTER,
        true,
        Request::OpFocus { agent },
        Instant::now(),
    )
    .unwrap();
    assert!(
        core.desktop.events.try_recv().is_err(),
        "no desktop, so no hand-off and no claim about one"
    );
}

/// A full desktop queue drops a hand-off and the core goes on: a press
/// that waited behind thirty-two others is not one the operator still
/// means, and nothing is queued for later.
#[test]
fn a_full_queue_drops_a_hand_off_and_the_core_goes_on() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-full");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    let focus = |core: &mut Core| {
        core.api_call(
            uid(),
            ADAPTER,
            true,
            Request::OpFocus {
                agent: agent.clone(),
            },
            Instant::now(),
        )
    };
    for _ in 0..deckd::desktop::CMDS_CAPACITY {
        focus(&mut core).unwrap();
    }
    assert_eq!(jobs.len(), deckd::desktop::CMDS_CAPACITY);
    focus(&mut core).expect("answered all the same");
    assert_eq!(jobs.len(), deckd::desktop::CMDS_CAPACITY, "dropped");
    assert!(core.desktop.events.try_recv().is_err(), "nothing claimed");
    // The core is still answering.
    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "still here"),
        Instant::now(),
    )
    .unwrap();
}

/// The reducer's own arm, driven directly: a successful result publishes
/// nothing and a failed one publishes exactly one `order.failed`.
#[test]
fn the_order_result_command_publishes_only_on_a_failure() {
    let (mut core, _parts) = build("desktop-order-result", 4096);
    let before = core.store.snapshot().rev;
    core.step(
        Command::OrderResult {
            principal: Principal::Device,
            agent: AgentId("ag_1".into()),
            text: "quiet".into(),
            ok: true,
            reason: None,
            order: None,
        },
        Instant::now(),
    );
    assert_eq!(
        core.store.snapshot().rev,
        before,
        "a success bumps no rev: `order.dispatched` already said it"
    );
    let events = core.step(
        Command::OrderResult {
            principal: Principal::Token("scripts".into()),
            agent: AgentId("ag_1".into()),
            text: "loud".into(),
            ok: false,
            reason: None,
            order: None,
        },
        Instant::now(),
    );
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::OrderFailed);
    assert_eq!(
        events[0].data["reason"], "undeliverable",
        "a refusal with no stated reason still has one"
    );
    assert_eq!(events[0].principal, Principal::Token("scripts".into()));
}

/// **Guard 2's card half, the status §5.5's list forgets.** A held
/// permission card whose answer never reached the CLI ends `failed`, and
/// `failed` means exactly that: the hook returned nothing, so Claude has
/// fallen back to asking in the terminal with Allow highlighted. An order
/// posted now would land on that dialog — the worst outcome in this design
/// (§5.5's own words for why guard 2 exists).
///
/// Before `Card::is_open_dialog` gained `CardStatus::Failed`, this order
/// answered `{via:"inject"}` and wrote `order.dispatched{text}`.
#[test]
fn a_permission_card_whose_answer_never_landed_still_blocks_an_order() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-dialog-failed");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    let mut card = permission(&agent);
    // A held card, because that is the shape the permission hook parks:
    // and a held `q` card has no TTL, so this state persists.
    card.deliver = Deliver::Hold;
    let card_id = card_of(&core.api_call(
        claude(),
        ADAPTER,
        false,
        Request::CardSet(card),
        Instant::now(),
    ));
    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        Request::OpAnswer(OpAnswer {
            agent: agent.clone(),
            card: card_id,
            choice: Some(0),
            text: None,
        }),
        Instant::now(),
    )
    .unwrap();
    // The answer could not be written into the CLI.
    core.step(
        Command::HoldGone {
            agent: agent.clone(),
            card: card_id,
        },
        Instant::now(),
    );
    assert!(
        matches!(card_status(&core, &agent), Some(CardStatus::Failed { .. })),
        "{:?}",
        card_status(&core, &agent)
    );

    let err = core
        .api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "yes"),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(err.code, deck_model::ApiError::UNDELIVERABLE);
    assert_eq!(
        err.data["reason"], "dialog_open",
        "the dialog is still on screen: the answer never reached the CLI"
    );
    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.last().unwrap().data["reason"], "dialog_open");
    assert_eq!(failed.last().unwrap().data["text_kept"], true);
    assert!(
        of_kind(&mut core, EventKind::OrderDispatched).is_empty(),
        "nothing claims it was sent"
    );
    assert!(jobs.try_recv().is_err(), "the desktop was never asked");
}

/// **Guard 1's trust half, by its second door.** `op.order` from an
/// untrusted external principal is refused `-32002 {need:"trusted"}` — and
/// so is a `voice.listen` aimed at the same agent, because
/// `Store::dispatch_order` calls `op_order` **directly** rather than
/// through `Request::OpOrder`. Without the listen arm an untrusted `voice`
/// caller could aim the operator's dictation at a session of its choosing
/// (harm 1) while holding no `operate` scope at all.
#[test]
fn an_untrusted_voice_capture_aimed_at_an_inject_agent_is_refused_too() {
    let (mut core, _parts, _jobs, _answers) = rig("desktop-voice-trust");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    core.guard.require_trusted = true;

    // The front door.
    let err = core
        .api_call(
            uid(),
            ADAPTER,
            false,
            order(&agent, "rm -rf the staging bucket"),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(err.code, deck_model::ApiError::FORBIDDEN);
    assert_eq!(err.data["need"], "trusted");

    // The side door, for both contexts: `talk` is an order by definition,
    // and `answer` on an agent with no card falls back to one.
    for ctx in ["talk", "answer"] {
        let err = core
            .api_call(
                uid(),
                ADAPTER,
                false,
                Request::VoiceListen(VoiceListen {
                    agent: agent.clone(),
                    ctx: ctx.to_string(),
                    card: None,
                    max_s: 60,
                    input: VoiceInput::Mac,
                }),
                Instant::now(),
            )
            .unwrap_err();
        assert_eq!(err.code, deck_model::ApiError::FORBIDDEN, "ctx:{ctx:?}");
        assert_eq!(err.data["need"], "trusted", "ctx:{ctx:?}");
        assert_eq!(err.data["how"], "gui_confirm", "ctx:{ctx:?}");
        assert!(
            core.store.snapshot().voice.ptt.is_none(),
            "ctx:{ctx:?}: the microphone was opened"
        );
    }

    // …and a capture the trusted GUI starts still dispatches its order,
    // which is what makes the refusal about the principal.
    core.api_call(
        uid(),
        ADAPTER,
        true,
        Request::VoiceListen(VoiceListen {
            agent: agent.clone(),
            ctx: "talk".into(),
            card: None,
            max_s: 60,
            input: VoiceInput::Mac,
        }),
        Instant::now(),
    )
    .unwrap();
    core.step(
        Command::Voice(VoiceIn::Final {
            text: "deploy the api".into(),
        }),
        Instant::now(),
    );
    let dispatched = of_kind(&mut core, EventKind::OrderDispatched);
    assert_eq!(dispatched.len(), 1, "the trusted capture's order");
    assert_eq!(dispatched[0].data["text"], "deploy the api");
}
