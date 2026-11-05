//! `op.order` → `via:"inject"` → the three guards → `order.dispatched` /
//! `order.failed`, driven through the **real core** with the desktop side
//! detached (HOST-CONTRACT-W5 §3, design §5.5, §4.6, §4.7, §4.12).
//!
//! `crates/deckd/src/desktop/` has the unit tests for each guard against
//! the scripted fakes. What those cannot prove is the *wiring*: that the
//! store routes an order to the injector at all, that `order.dispatched`
//! goes out **before** the job leaves the core, that a refusal coming back
//! 200 ms later becomes an `order.failed` with the right reason and the
//! right principal, and that the two guards the core owns — provenance and
//! the card half of the dialog check — refuse before a `DesktopJob` is
//! ever built.
//!
//! Nothing here opens a terminal. `DesktopHandle::detached()` hands the
//! test the far end of the same bounded channel the daemon uses, so a job
//! is an assertion and an answer is a `send`.
mod core_support;

use core_support::{agent_of, build, card_of, card_status, claude, hold_register, uid, Parts};
use deck_model::{
    AgentId, AgentRegister, AnswerCap, Caps, CardKind, CardSet, CardStatus, Choice, Command,
    Deliver, Event, EventKind, Kind, OpAnswer, OpOrder, OrderCap, OrderSource, Origin, OriginKind,
    PidKind, Principal, Request, SessionId, Terminal, VoiceIn, VoiceInput, VoiceListen,
};
use deckd::core::Core;
use deckd::desktop::{DesktopError, DesktopHandle, DesktopJob, FromDesktop};
use std::time::Instant;

/// The adapter session every hook-owned agent here is registered from.
const ADAPTER: SessionId = SessionId(9);

/// A Claude session the hook helper resolved completely: an `inject` cap,
/// a `cli` pid, a tty and a beacon nonce. This is the only agent shape
/// §5.5 allows as an injection target.
fn resolved_register(name: &str) -> AgentRegister {
    AgentRegister {
        pid: Some(std::process::id()),
        pid_kind: Some(PidKind::Cli),
        terminal: Some(Terminal {
            program: Some("ghostty".into()),
            tty: Some("/dev/ttys004".into()),
            tmux_pane: None,
            iterm_session: None,
            term_session: None,
            title_stamp: Some("n0nce".into()),
            inbox: None,
        }),
        key: Some(format!("claude:{name}")),
        ..hold_register(name)
    }
}

/// A virtual agent that *claims* a terminal and a pid: §4.6 says both are
/// informational, and §5.5 says the injector never uses them.
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
            tmux_pane: None,
            iterm_session: None,
            term_session: None,
            title_stamp: Some("stolen".into()),
            inbox: None,
        }),
        lease_ttl_s: 30,
        caps: Caps {
            answer: AnswerCap::Poll,
            // `inject` is adapter-only at registration; a script asking for
            // it is refused there. `push` is what it can have.
            order: OrderCap::Push,
            focus: false,
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

/// A permission card, held, from the adapter — guard 2's card half.
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

/// The happy path, end to end through the core: the store routes the order
/// to the injector, writes the exact string **before** anything is typed,
/// and the job that reaches the worker carries the resolved tty and pid
/// guard 3 will check.
#[test]
fn an_inject_order_dispatches_the_exact_string_before_the_job_leaves_the_core() {
    let (mut core, _parts, jobs, answers) = rig("desktop-dispatch");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());

    let result = core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "deploy the api"),
        Instant::now(),
    );
    assert_eq!(result.unwrap()["via"], "inject");

    // §5.5: the order history has the exact string, and it was written
    // before the job was queued — which is the only ordering a GUI showing
    // "this is about to be typed" can rely on.
    let dispatched = of_kind(&mut core, EventKind::OrderDispatched);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].data["text"], "deploy the api");
    assert_eq!(dispatched[0].data["via"], "inject");
    assert_eq!(dispatched[0].data["agent"], agent.0);
    assert_eq!(dispatched[0].principal, Principal::Device);
    assert!(
        of_kind(&mut core, EventKind::OrderFailed).is_empty(),
        "nothing has failed yet"
    );
    let job = jobs.try_recv().expect("the injector job");
    let DesktopJob::Inject {
        agent: for_agent,
        target,
        text,
        dialog_open,
    } = job
    else {
        panic!("{job:?}")
    };
    assert_eq!(for_agent, agent);
    assert_eq!(text, "deploy the api");
    assert!(!dialog_open);
    // Guard 3's two inputs come from the hook environment, not from
    // anything the caller said.
    assert_eq!(target.tty, "/dev/ttys004");
    assert_eq!(target.pid, std::process::id());
    assert_eq!(target.key, "claude:cohdl-api");
    assert_eq!(target.nonce.as_deref(), Some("n0nce"));
    assert_eq!(target.program, "ghostty");

    // The worker answers `Ok`, and the core publishes nothing more: the
    // dispatch already said the exact string, and a second event per order
    // would double the bus traffic of the one method used by voice.
    answers
        .send(FromDesktop::Order {
            agent: agent.clone(),
            text: "deploy the api".into(),
            result: Ok(()),
        })
        .unwrap();
    let msg = answers_recv(&mut core);
    core.on_from_desktop(msg, Instant::now());
    assert!(of_kind(&mut core, EventKind::OrderFailed).is_empty());
    assert_eq!(of_kind(&mut core, EventKind::OrderDispatched).len(), 1);
}

/// Take the message the test just sent back off the core's own receiver, so
/// no sleep decides anything.
fn answers_recv(core: &mut Core) -> FromDesktop {
    core.desktop
        .events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the desktop answer")
}

/// Guard 3's refusal, arriving after `order.dispatched` has already gone
/// out: it becomes `order.failed{reason:"not_foreground", text_kept:true}`
/// for the principal that dictated it.
///
/// This is §15's "injection refused after the CLI exits to a shell" row as
/// the *wire* sees it.
#[test]
fn a_guard_three_refusal_becomes_an_order_failed_with_the_text_kept() {
    let (mut core, _parts, jobs, answers) = rig("desktop-not-foreground");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "restart the build"),
        Instant::now(),
    )
    .unwrap();
    assert!(jobs.try_recv().is_ok());

    answers
        .send(FromDesktop::Order {
            agent: agent.clone(),
            text: "restart the build".into(),
            result: Err(DesktopError::NotForeground),
        })
        .unwrap();
    let msg = answers_recv(&mut core);
    core.on_from_desktop(msg, Instant::now());

    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].data["reason"], "not_foreground");
    assert_eq!(failed[0].data["text"], "restart the build");
    assert_eq!(
        failed[0].data["text_kept"], true,
        "§4.8: the GUI keeps it with Copy and Retry"
    );
    assert_eq!(failed[0].data["agent"], agent.0);
    // The dispatch is still there: the operator can see exactly what was
    // about to be typed and what happened to it.
    assert_eq!(of_kind(&mut core, EventKind::OrderDispatched).len(), 1);
    assert!(
        failed[0].rev > of_kind(&mut core, EventKind::OrderDispatched)[0].rev,
        "the failure is later than the dispatch it answers"
    );
}

/// The other two `order.failed` words, from the same wiring: an ambiguous
/// resolution and an automation denial.
#[test]
fn an_ambiguous_resolution_and_an_automation_denial_each_fail_the_order_by_name() {
    let (mut core, _parts, jobs, answers) = rig("desktop-reasons");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    for (text, error, reason) in [
        ("one", DesktopError::Ambiguous(2), "ambiguous"),
        (
            "two",
            DesktopError::Script(deckd::desktop::ScriptError::Osa {
                number: deckd::desktop::errors::ERR_NOT_PERMITTED,
                message: "Not authorized".into(),
            }),
            "automation_denied",
        ),
        ("three", DesktopError::NoTerminal, "no_terminal"),
    ] {
        core.api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, text),
            Instant::now(),
        )
        .unwrap();
        assert!(jobs.try_recv().is_ok(), "{text}");
        answers
            .send(FromDesktop::Order {
                agent: agent.clone(),
                text: text.into(),
                result: Err(error),
            })
            .unwrap();
        let msg = answers_recv(&mut core);
        core.on_from_desktop(msg, Instant::now());
        let failed = of_kind(&mut core, EventKind::OrderFailed);
        assert_eq!(
            failed.last().unwrap().data["reason"],
            reason,
            "{text} should fail as {reason}"
        );
        assert_eq!(failed.last().unwrap().data["text"], text);
    }
    assert_eq!(of_kind(&mut core, EventKind::OrderFailed).len(), 3);
}

/// **Guard 2, the card half**: while a permission card is open the order is
/// refused `-32006 undeliverable{reason:"dialog_open"}`, `order.failed`
/// says so, and **no job is built at all** — nothing is enumerated, nothing
/// is pinned, nothing is typed.
///
/// §5.5's reason: while a hook is held the TUI queues typed keys and the
/// dialog consumes them the moment the hook returns, with Allow
/// highlighted. `text + Return` would confirm it.
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
        "refused before the dispatch, so nothing claims it was typed"
    );
    assert_eq!(
        jobs.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty),
        "the injector was never asked"
    );
}

/// The same refusal for every card origin and status §5.5 lists, and *not*
/// for a `custom` card — a script's own card is not a terminal dialog and
/// must not be able to block dictation by existing.
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
            assert!(jobs.try_recv().is_err(), "{origin:?}");
        } else {
            assert_eq!(result.unwrap()["via"], "inject", "{origin:?}");
            assert!(jobs.try_recv().is_ok(), "{origin:?}");
        }
    }
}

/// **Guard 1, the trust half**: an untrusted external principal with
/// `operate` that calls `op.order` on an `inject`-only agent is refused
/// `-32002 {need:"trusted"}` and gets the GUI confirmation path
/// (`op.confirm_requested`) — and the injector is never asked.
#[test]
fn an_untrusted_operate_caller_is_refused_thirty_two_thousand_and_two() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-untrusted");
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
    assert!(jobs.try_recv().is_err(), "the injector was never asked");

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
    assert!(jobs.try_recv().is_ok());
}

/// **Guard 1, the provenance half**: a virtual agent's self-supplied
/// `terminal`/`pid` is informational (§4.6) and is never an injection
/// target, however complete it looks.
///
/// Two layers, both asserted: `caps.order:"inject"` is refused at
/// registration for a non-adapter principal, and — belt and braces — an
/// agent whose `source` is not `Hook` cannot produce a `Target` even if it
/// somehow held the cap.
#[test]
fn a_virtual_agents_self_supplied_terminal_is_never_an_injection_target() {
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

    // With the cap it can have, its order goes to its owner and not to a
    // terminal — even though it declared a tty and a `cli` pid.
    let agent = register(&mut core, self_declared_register("impostor"), uid());
    let claimed = &core.store.snapshot().agents[&agent].session;
    assert_eq!(
        claimed.terminal.as_ref().unwrap().tty.as_deref(),
        Some("/dev/ttys004"),
        "it is shown in the GUI …"
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
    assert!(
        jobs.try_recv().is_err(),
        "a script cannot aim dictation at a Ghostty tab of its choosing"
    );
    assert!(
        of_kind(&mut core, EventKind::OrderFailed).is_empty(),
        "its own owner is a route, so nothing failed"
    );

    // And the pure check says the same thing on its own.
    let agent_object = core.store.snapshot().agents[&agent].clone();
    assert_eq!(
        deckd::desktop::inject::target_of(&agent_object).unwrap_err(),
        DesktopError::NotOperator
    );
}

/// An agent whose hook helper resolved a `shell` pid, or no tty, is not an
/// injection target either — and the order fails with a reason rather than
/// being silently swallowed.
#[test]
fn an_agent_with_no_checkable_pid_or_tty_fails_the_order_instead_of_typing() {
    for (label, mutate) in [
        (
            "a shell pid",
            (|r: &mut AgentRegister| r.pid_kind = Some(PidKind::Shell)) as fn(&mut AgentRegister),
        ),
        ("no pid at all", |r: &mut AgentRegister| r.pid = None),
        ("no tty", |r: &mut AgentRegister| {
            r.terminal.as_mut().unwrap().tty = None;
        }),
        ("no terminal record", |r: &mut AgentRegister| {
            r.terminal = None;
        }),
    ] {
        let (mut core, _parts, jobs, _answers) = rig("desktop-unresolved");
        let mut r = resolved_register("cohdl-api");
        mutate(&mut r);
        let agent = register(&mut core, r, claude());
        let result = core.api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "go"),
            Instant::now(),
        );
        // The store still routes it — the cap says `inject` — so the
        // dispatch is written and then the core's own guard 1 refuses.
        assert_eq!(result.unwrap()["via"], "inject", "{label}");
        assert!(jobs.try_recv().is_err(), "{label}: a job was built");
        let failed = of_kind(&mut core, EventKind::OrderFailed);
        assert_eq!(failed.len(), 1, "{label}");
        assert_eq!(failed[0].data["reason"], "no_terminal", "{label}");
        assert_eq!(failed[0].data["text"], "go", "{label}");
    }
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
    assert!(jobs.try_recv().is_ok());
}

/// `op.focus` becomes a focus job for the resolved terminal, and the
/// outcome is silent on the wire: the `focus{ack}` already went to the
/// device (§6.4) and a failure is the GUI's to show.
#[test]
fn op_focus_becomes_a_focus_job_whose_outcome_is_silent_on_the_bus() {
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
    let job = jobs.try_recv().expect("the focus job");
    let DesktopJob::Focus {
        agent: for_agent,
        target,
    } = job
    else {
        panic!("{job:?}")
    };
    assert_eq!(for_agent, agent);
    assert_eq!(target.key, "claude:cohdl-api");

    let before = events(&mut core).len();
    answers
        .send(FromDesktop::Focus {
            agent,
            selected: false,
            reason: Some("ambiguous".into()),
        })
        .unwrap();
    let msg = answers_recv(&mut core);
    core.on_from_desktop(msg, Instant::now());
    assert_eq!(
        events(&mut core).len(),
        before,
        "a failed focus publishes nothing: the ack already went out"
    );
}

/// §5.5 step 7 and §6: an agent that goes away has its tab unpinned,
/// however it went — and the daemon leaves the operator's Ghostty as it
/// found it.
#[test]
fn an_agent_that_goes_away_has_its_tab_unpinned() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-unpin");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    // Drain the resolution job so the release is the only one left.
    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "hello"),
        Instant::now(),
    )
    .unwrap();
    assert!(jobs.try_recv().is_ok());

    core.api_call(
        claude(),
        ADAPTER,
        false,
        Request::AgentRelease {
            agent: agent.clone(),
            reason: Some("session_end".into()),
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(
        jobs.try_recv(),
        Ok(DesktopJob::Release {
            key: agent.0.clone()
        }),
        "the unpin is addressed by the id `agent.removed` names"
    );
    assert_eq!(of_kind(&mut core, EventKind::AgentRemoved).len(), 1);
}

/// `state.json.terminal_map` is written when the worker reports a new pin,
/// and is what a restart seeds the resolver from (§5.5 step 4).
#[test]
fn a_reported_pin_is_persisted_under_its_terminal_id() {
    let (mut core, _parts, _jobs, answers) = rig("desktop-pins");
    assert!(core.state.terminal_map.is_empty());
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "487743E9-B8B2-432E-8E91-16FFC489112A".to_string(),
        serde_json::json!({
            "key": "claude:cohdl-api", "id4": "cd34", "agent": "ag_cd34",
            "tty": "/dev/ttys004", "pid": 4321, "manual": false,
        }),
    );
    answers.send(FromDesktop::Pins(map.clone())).unwrap();
    let msg = answers_recv(&mut core);
    core.on_from_desktop(msg, Instant::now());
    assert_eq!(core.state.terminal_map, map);
    // Keyed on the UUID and never on a name (M6: "the name" is three
    // different strings).
    assert!(core
        .state
        .terminal_map
        .keys()
        .all(|k| k.len() == 36 && k.contains('-')));

    // …and the resolver reads exactly that shape back.
    let mut resolver = deckd::desktop::Resolver::new();
    resolver.restore(&core.state.terminal_map);
    let (terminal, pinned) = resolver.known("claude:cohdl-api").expect("restored");
    assert_eq!(terminal, "487743E9-B8B2-432E-8E91-16FFC489112A");
    assert_eq!(pinned.tty.as_deref(), Some("/dev/ttys004"));
    assert_eq!(pinned.pid, Some(4321));
    // The `ag_…` id addresses it too, which is how an unpin finds it.
    assert!(resolver.known("ag_cd34").is_some());
}

/// A daemon with **no** desktop side fails the order honestly rather than
/// letting the operator believe it was typed.
///
/// This is the `--no-device` daemon and every door test: `op.order` still
/// answers `{via:"inject"}` (the cap says so and the store's routing is not
/// the injector's business), the order history still has the exact string,
/// and then `order.failed{reason:"no_injector"}` says the truth.
#[test]
fn a_daemon_with_no_injector_fails_the_order_instead_of_pretending() {
    let (mut core, _parts) = build("desktop-absent", 4096);
    core.sessions
        .open_internal(ADAPTER, claude(), "claude-adapter", Instant::now());
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    assert!(!core.desktop.engine());

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
    assert_eq!(of_kind(&mut core, EventKind::OrderDispatched).len(), 1);
    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].data["reason"], "no_injector");
    assert_eq!(failed[0].data["text"], "deploy the api");
    assert_eq!(failed[0].data["text_kept"], true);
}

/// A full desktop queue drops the order and says so, rather than queueing
/// dictation the operator has stopped waiting for.
#[test]
fn a_full_desktop_queue_drops_the_order_and_counts_it() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-full");
    let agent = register(&mut core, resolved_register("cohdl-api"), claude());
    for _ in 0..deckd::desktop::CMDS_CAPACITY {
        core.api_call(
            Principal::Device,
            ADAPTER,
            true,
            order(&agent, "queued"),
            Instant::now(),
        )
        .unwrap();
    }
    assert_eq!(jobs.len(), deckd::desktop::CMDS_CAPACITY);
    assert_eq!(core.desktop_dropped(), 0);

    core.api_call(
        Principal::Device,
        ADAPTER,
        true,
        order(&agent, "one too many"),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(core.desktop_dropped(), 1);
    let failed = of_kind(&mut core, EventKind::OrderFailed);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].data["reason"], "undeliverable");
    assert_eq!(failed[0].data["text"], "one too many");
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

/// **The adapter↔resolver seam.** The beacon string the *real* Claude
/// adapter writes must be the string the *real* resolver looks for.
///
/// Every resolver test builds its fake surface with
/// `FakeGhostty::with_beacon(id, nonce)`, which calls the same
/// `ghostty::beacon_name` the resolver calls — so both sides can be wrong
/// together and the suite stays green. This test takes the producer's own
/// output (`adapters::claude::stamp`, which is what `SessionStart` puts in
/// the OSC 2 *and* in `Terminal::title_stamp`) and puts it on
/// `terminal.name` where the OSC 2 lands, then asks the resolver to find
/// it.
#[test]
fn the_beacon_the_claude_adapter_writes_is_the_string_the_resolver_looks_for() {
    use deckd::adapters::claude;
    use deckd::desktop::fakes::{FakeForeground, FakeGhostty};
    use deckd::desktop::{inject, Desktop, DesktopConfig, Verdict};
    use std::sync::Arc;

    const SESSION: &str = "abc123";
    // What the CLI's own terminal shows after `SessionStart` answered
    // `terminalSequence`: the OSC 2's payload, byte for byte.
    let seq = claude::terminal_sequence(SESSION);
    let live_title = seq
        .trim_start_matches('\u{1b}')
        .trim_start_matches("]2;")
        .trim_end_matches('\u{7}')
        .to_string();
    assert_eq!(live_title, claude::stamp(SESSION), "the OSC 2's payload");

    // The agent the adapter registers, with that same stamp.
    let (mut core, _parts, _jobs, _answers) = rig("desktop-seam");
    let agent = register(
        &mut core,
        AgentRegister {
            terminal: Some(Terminal {
                title_stamp: Some(claude::stamp(SESSION)),
                ..resolved_register("cohdl-api").terminal.unwrap()
            }),
            ..resolved_register("cohdl-api")
        },
        claude(),
    );
    let snapshot = core.store.snapshot();
    let target = inject::target_of(&snapshot.agents[&agent]).expect("an injection target");

    // One Ghostty surface, carrying exactly the title the OSC 2 left.
    let gh = FakeGhostty::new();
    gh.with_surface("UUID-MINE", &live_title);
    let watch = gh.handle();
    let mut desktop = Desktop::with(
        Arc::new(gh),
        Arc::new(FakeForeground::always(Verdict::Foreground)),
        DesktopConfig {
            claude_bin: None,
            ..DesktopConfig::default()
        },
    );
    let out = desktop.on_job(
        DesktopJob::Inject {
            agent: agent.clone(),
            target,
            text: "deploy the api".into(),
            dialog_open: false,
        },
        Instant::now(),
    );
    let FromDesktop::Order { result, .. } = &out[0] else {
        panic!("{out:?}")
    };
    assert_eq!(
        *result,
        Ok(()),
        "the resolver did not find the beacon the adapter wrote"
    );
    assert_eq!(watch.typed_into("UUID-MINE"), "deploy the api");
    // …and the tab it found is now pinned with this agent's own four
    // characters, not with the beacon it was found by.
    assert_eq!(
        watch.tab_name("UUID-MINE"),
        Some(format!("deck:{}", inject::id4(&agent.0)))
    );
}

/// **Guard 2's card half, the status §5.5's list forgets.** A held
/// permission card whose answer never reached the CLI ends `failed`, and
/// `failed` means exactly that: the hook returned nothing, so Claude has
/// fallen back to asking in the terminal with Allow highlighted. An order
/// injected now is `text + Return` on that dialog — the worst outcome in
/// this design (§5.5's own words for why guard 2 exists).
///
/// Before `Card::is_open_dialog` gained `CardStatus::Failed`, this order
/// answered `{via:"inject"}`, wrote `order.dispatched{text}` and handed the
/// worker a job with `dialog_open:false`.
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
        "nothing claims it was typed"
    );
    assert!(jobs.try_recv().is_err(), "the injector was asked");
}

/// **Guard 1's trust half, by its second door.** `op.order` from an
/// untrusted external principal is refused `-32002 {need:"trusted"}` — and
/// so is a `voice.listen` aimed at the same agent, because
/// `Store::dispatch_order` calls `op_order` **directly** rather than
/// through `Request::OpOrder`. Without the listen arm an untrusted `voice`
/// caller could aim the operator's dictation at a terminal of its choosing
/// (harm 1) while holding no `operate` scope at all.
#[test]
fn an_untrusted_voice_capture_aimed_at_an_inject_agent_is_refused_too() {
    let (mut core, _parts, jobs, _answers) = rig("desktop-voice-trust");
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

    // …and a capture the trusted GUI starts still dispatches into the
    // injector, which is what makes the refusal about the principal.
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
    let job = jobs.try_recv().expect("the trusted capture's order");
    assert!(matches!(
        job,
        DesktopJob::Inject { ref text, .. } if text == "deploy the api"
    ));
}

/// **Guard 2's roster half, through a real child process.** The predicate
/// has a unit test over hand-built rows; what that cannot show is that the
/// spawn, the JSON and `Desktop::set_roster` are wired to each other at
/// all — `set_roster` had no caller anywhere before this row.
///
/// M14 is the reason the `idle` case is here: an ordinary row carries no
/// `waitingFor` key, and a parser that required one would refuse every
/// injection.
#[test]
fn guard_two_reads_the_claude_roster_through_a_real_spawn() {
    use deckd::desktop::fakes::{FakeForeground, FakeGhostty};
    use deckd::desktop::{Desktop, DesktopConfig, Verdict};

    fn fake_claude(name: &str, rows: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("deckd-w5-roster-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\ncat <<'JSON'\n{rows}\nJSON\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn run(bin: &std::path::Path) -> (FakeGhostty, Result<(), DesktopError>) {
        let gh = FakeGhostty::new();
        gh.with_beacon("UUID-MINE", "n0nce");
        let watch = gh.handle();
        let mut desktop = Desktop::with(
            std::sync::Arc::new(gh),
            std::sync::Arc::new(FakeForeground::always(Verdict::Foreground)),
            DesktopConfig {
                claude_bin: None,
                ..DesktopConfig::default()
            },
        );
        desktop.set_roster(Some(deckd::liveness::Probe::new(
            bin.to_string_lossy().to_string(),
        )));
        let out = desktop.on_job(
            DesktopJob::Inject {
                agent: AgentId("ag_cd34".into()),
                target: deckd::desktop::Target {
                    agent: "ag_cd34".into(),
                    key: "claude:sess-1".into(),
                    nonce: Some("n0nce".into()),
                    id4: "cd34".into(),
                    tty: "/dev/ttys004".into(),
                    pid: std::process::id(),
                    program: "ghostty".into(),
                },
                text: "INERT_MARKER".into(),
                dialog_open: false,
            },
            Instant::now(),
        );
        let FromDesktop::Order { result, .. } = out[0].clone() else {
            panic!("{out:?}")
        };
        (watch, result)
    }

    // `status:"waiting"` → refuse, and **nothing is sent**.
    let waiting = fake_claude(
        "waiting",
        r#"[{"pid":1,"kind":"interactive","sessionId":"sess-1","status":"waiting","waitingFor":"permission"}]"#,
    );
    let (gh, result) = run(&waiting);
    assert_eq!(result.unwrap_err().reason(), "dialog_open");
    assert!(gh.typed().is_empty(), "not even the text");
    assert!(gh.keys().is_empty());

    // An ordinary row: `status`, and no `waitingFor` key at all (M14).
    let idle = fake_claude(
        "idle",
        r#"[{"pid":1,"kind":"interactive","sessionId":"sess-1","status":"idle"}]"#,
    );
    let (gh, result) = run(&idle);
    assert_eq!(
        result,
        Ok(()),
        "an absent `waitingFor` is not a parse error"
    );
    assert_eq!(gh.typed_into("UUID-MINE"), "INERT_MARKER");

    // A probe that cannot run is **inconclusive**, not a refusal: a
    // `claude` missing from the daemon's `PATH` must not make every order
    // fail. The card half has already run either way.
    let (gh, result) = run(std::path::Path::new("/nonexistent/claude"));
    assert_eq!(result, Ok(()));
    assert_eq!(gh.typed_into("UUID-MINE"), "INERT_MARKER");
}

/// **The quoter, against a real AppleScript engine.** `script::quote` has a
/// string-equality unit test; what that cannot show is that the engine
/// agrees with it. A dictated sentence is arbitrary text from a recogniser,
/// and the injector puts it inside a `tell application "Ghostty" to input
/// text "<here>" to …` — so a payload that closed the literal would be
/// running AppleScript as the daemon, not typing into a tab.
///
/// No `tell` and no Ghostty: the script is `return <quoted>`, which needs
/// no Automation grant and touches nothing. What is asserted is that every
/// payload comes back **byte for byte** — a `do shell script` that had
/// escaped the quoting would come back as its own output instead.
#[cfg(target_os = "macos")]
#[test]
fn a_hostile_transcript_cannot_escape_the_applescript_string_literal() {
    use deckd::desktop::script::{quote, OsaRunner, Scripting};

    let osa = OsaRunner::new();
    if osa.run("return \"probe\"").is_err() {
        println!("skip  no usable /usr/bin/osascript");
        return;
    }
    let payloads = [
        // The obvious one: close the literal, run a command, reopen it.
        r#"" & (do shell script "echo ESCAPED") & ""#,
        // Backslash first, so a quoter that escaped the quote but not the
        // backslash would leave the quote live.
        r#"\" & (do shell script "echo ESCAPED") & \""#,
        r#"ends with a backslash \"#,
        r#"quote " in the middle"#,
        // AppleScript's own statement separators, as text.
        "a; return \"ESCAPED\"",
        // U+2028/U+2029 are not control characters, so `-32009` lets them
        // through — and they are ordinary characters to the parser, not
        // statement terminators (measured: `length of "a\u{2028}b"` is 3).
        "line\u{2028}separator",
        "paragraph\u{2029}separator",
        "café ☕ — em dash",
        "tab\there",
    ];
    for payload in payloads {
        let out = osa
            .run(&format!("return {}\n", quote(payload)))
            .unwrap_or_else(|e| panic!("{payload:?}: {e}"));
        // Byte for byte: a payload that had escaped the literal would come
        // back as the *result* of whatever it ran, not as itself.
        assert_eq!(out, payload, "{payload:?} escaped the literal");
    }
}
