//! The injector chain: three guards, **in this order**, and only then
//! `input text` + `send key "enter"` (design §5.5; HOST-CONTRACT-W5 §3).
//!
//! The order is not a style choice. Each guard is cheaper and more certain
//! than the next, and each one that fails must fail before the more
//! expensive one can have a side effect:
//!
//! 1. **Provenance** — a pure function of the snapshot. No syscall, no
//!    script. It decides whether this text may be aimed at a terminal at
//!    all, and the answer never depends on anything that can change while
//!    we look.
//! 2. **No open dialog** — the store's own card status, plus (for a Claude
//!    session) what `claude agents --json` says. A process spawn at worst.
//! 3. **Foreground** — one `proc_pidinfo` per check, run **twice**:
//!    immediately before `input text` and again immediately before
//!    `send key`.
//!
//! **The TOCTOU window is documented, not closed.** M12 measured ~118 ms
//! per AppleScript round trip, so between the second foreground check and
//! the Return there is a ~118 ms gap in which a CLI could exit. Closing it
//! would need the check *inside* Ghostty, which the dictionary does not
//! offer. What the second check buys is the whole of the *first* round
//! trip's duration plus everything before it — and, more importantly, it
//! means the text and the Return are two separately guarded decisions, so
//! the case §5.5 actually cares about (the CLI exited some seconds ago and
//! a shell is sitting at a prompt) is caught before a single character is
//! sent.
//!
//! **On a refusal, nothing is sent — not even the text.** A `input text`
//! with no Return leaves the operator's sentence on a shell's command line
//! waiting for them to press Return themselves, which is worse than
//! refusing: it turns a guard into a trap.
use super::errors::DesktopError;
use super::fg::Foreground;
use super::ghostty;
use super::script::Scripting;
use deck_model::{Agent, OrderCap, PidKind, Source};

/// Everything the chain needs about where an order is going, resolved once
/// and then treated as read-only.
///
/// It is built from the snapshot by [`target_of`], which is guard 1: a
/// `Target` cannot be constructed for an agent that may not be injected
/// into, so there is no path that types into one by forgetting a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The `ag_…` id. Per daemon run, so it is **not** what
    /// `terminal_map` matches on across a restart — but it is what an
    /// `agent.removed` event names, and that is the one moment the unpin
    /// has to be addressed without a snapshot to read the key from.
    pub agent: String,
    /// The agent key (`claude:<session_id>`), which is what the resolver
    /// and `terminal_map` key on.
    pub key: String,
    /// The nonce `SessionStart` answered, for a first resolution.
    pub nonce: Option<String>,
    /// The last four characters of the agent id: the pin's value.
    pub id4: String,
    /// The tty guard 3 checks against. Not optional: an agent with no
    /// recorded tty has no `Target`.
    pub tty: String,
    /// …nor has one with no resolved **CLI** pid.
    pub pid: u32,
    /// `TERM_PROGRAM`, matched against `[desktop] inject`.
    pub program: String,
}

/// The four characters a pin carries, from an agent id.
///
/// The *last* four, because an `ag_…` id's tail is the part that varies.
#[must_use]
pub fn id4(agent: &str) -> String {
    let chars: Vec<char> = agent.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

/// **Guard 1, provenance** (§5.5): may this agent's terminal be an
/// injection target at all?
///
/// Four refusals, each for a different reason the design gives:
///
/// * `caps.order != "inject"` — this agent's orders go to its owner by
///   `push`/`poll`, and a card choice is *never* injected (§4.8: "`inject`
///   is not a card delivery mode"). The store already routes on this; the
///   check is repeated here because a second route into this module must
///   not be able to skip it.
/// * `source != Hook` — **a virtual agent's self-supplied `terminal`/`pid`
///   is informational** (§4.6) and is never used here, so a script cannot
///   aim dictation at a Ghostty tab of its choosing. `caps.order:"inject"`
///   is adapter-only at registration, which makes this belt and braces —
///   and belt and braces is the right amount for the one check that stops
///   an API caller choosing the target.
/// * `pid_kind != Cli` — the hook helper's parent walk resolved `shell` or
///   `unknown`, so the pid is the `sh -c` that ran the hook and exited.
///   Guard 3 against such a pid is meaningless, and a meaningless guard 3
///   is worse than none because it *passes*.
/// * no tty — there is nothing for guard 3's third clause (M10) to compare
///   against, and a pgid-only check is the case M10 exists to refuse.
///
/// The trust half of guard 1 — an untrusted `operate` caller gets
/// `-32002 {need:"trusted"}` and the GUI confirmation path — is
/// [`crate::core::guard::Guard::guarded`], which runs *before* the reducer
/// so there is no window between the check and the routing. It is asserted
/// there and again in `tests/desktop_guards.rs`.
pub fn target_of(agent: &Agent) -> Result<Target, DesktopError> {
    if agent.caps.order != OrderCap::Inject {
        return Err(DesktopError::NotOperator);
    }
    if agent.source != Source::Hook {
        // §4.6: informational, and only that.
        return Err(DesktopError::NotOperator);
    }
    let session = &agent.session;
    if session.pid_kind != Some(PidKind::Cli) {
        return Err(DesktopError::NoTerminal);
    }
    let (Some(pid), Some(terminal)) = (session.pid, session.terminal.as_ref()) else {
        return Err(DesktopError::NoTerminal);
    };
    let Some(tty) = terminal.tty.as_deref().filter(|t| !t.is_empty()) else {
        return Err(DesktopError::NoTerminal);
    };
    let Some(key) = session.key.as_deref().filter(|k| !k.is_empty()) else {
        return Err(DesktopError::NoTerminal);
    };
    Ok(Target {
        agent: agent.id.0.clone(),
        key: key.to_string(),
        // **The one seam the two beacon conventions meet at.** The adapter's
        // `Terminal::title_stamp` is the whole beacon *name*
        // (`adapters::claude::stamp` → `deck:<4 hex>`, and that is what the
        // `terminalSequence` OSC 2 puts on `terminal.name`), while
        // `ghostty::beacon_name` adds the `deck:` prefix itself. Handing the
        // stamp through unchanged makes the resolver look for
        // `deck:deck:<4 hex>`, which matches nothing: every first resolution
        // is `ambiguous`, nothing is ever pinned, and no dictation reaches a
        // session. Strip it once, here, so `Target::nonce` really is the
        // nonce every `ghostty` builder documents it to be.
        nonce: terminal.title_stamp.as_deref().map(|stamp| {
            stamp
                .strip_prefix(ghostty::PIN_PREFIX)
                .unwrap_or(stamp)
                .to_string()
        }),
        id4: id4(&agent.id.0),
        tty: tty.to_string(),
        pid,
        program: terminal
            .program
            .clone()
            .unwrap_or_else(|| ghostty::APP.to_lowercase()),
    })
}

/// `[desktop] inject` lists this agent's terminal program.
///
/// Case-folded, because `TERM_PROGRAM` is `ghostty` and the config is the
/// operator's prose.
#[must_use]
pub fn enabled_for(program: &str, allowed: &[String]) -> bool {
    allowed
        .iter()
        .any(|a| a.eq_ignore_ascii_case(program) || a == "*")
}

/// Control characters `op.order` rejects with `-32009`.
///
/// A tab is allowed — it is a character an operator can dictate into a
/// sentence and it does nothing on its own — and everything else is not,
/// because `\r` *is* a Return and `\n` *is* a Return and an order carrying
/// one would be several commands rather than one sentence. The reducer
/// applies this before `order.dispatched`; it is here as well so that
/// nothing reaching this module can be typed without having passed it.
#[must_use]
pub fn has_control_chars(text: &str) -> bool {
    text.chars().any(|c| c.is_control() && c != '\t')
}

/// **Guard 2's roster half** (§5.5): does the CLI's own roster say this
/// session is waiting for the operator?
///
/// The card half lives in the reducer (`deck_model`'s `op_order` →
/// `-32006 undeliverable{reason:"dialog_open"}`), because it is a pure
/// function of the snapshot and belongs where the routing decision is made.
/// This half needs a process spawn, so it runs out here.
///
/// **`waitingFor` is present only when `status == "waiting"`** (measured
/// against `claude 2.1.278`). An ordinary row carries `status` (`idle` /
/// `busy`) and no `waitingFor` key at all, so the predicate keys off
/// `status` and reads `waitingFor` only for the *reason*. A parser that
/// required the field would refuse every single injection — and a fixture
/// that always included it would never notice.
#[must_use]
pub fn roster_waiting<'a>(rows: &'a [crate::liveness::Row], session_id: &str) -> Option<&'a str> {
    let row = rows
        .iter()
        .find(|r| r.session_id.as_deref() == Some(session_id))?;
    if row.status.as_deref() != Some("waiting") {
        return None;
    }
    // Waiting with no stated reason is still waiting.
    Some(row.waiting_for.as_deref().unwrap_or("waiting"))
}

/// The `claude:<session_id>` an agent key names, for [`roster_waiting`].
#[must_use]
pub fn claude_session_of(key: &str) -> Option<&str> {
    key.strip_prefix("claude:").filter(|id| !id.is_empty())
}

/// **Guard 3**, once (M9, M10). `Ok(())` or
/// [`DesktopError::NotForeground`], with the verdict logged.
pub fn foreground(fg: &dyn Foreground, target: &Target) -> Result<(), DesktopError> {
    let verdict = fg.check(target.pid, &target.tty);
    if verdict.allows() {
        return Ok(());
    }
    tracing::info!(
        pid = target.pid,
        tty = %target.tty,
        verdict = verdict.code(),
        "guard 3 refused an order"
    );
    Err(DesktopError::NotForeground)
}

/// Type `text` into `terminal`, guard 3 on both sides of it.
///
/// `terminal` is a resolved `terminal.id`; resolution and guards 1 and 2
/// happened before this was called. What is left is the sequence §5.5
/// specifies, with the check §3 requires "immediately before `input text`
/// **and** again immediately before `send key`".
pub fn type_into(
    script: &dyn Scripting,
    fg: &dyn Foreground,
    target: &Target,
    terminal: &str,
    text: &str,
) -> Result<(), DesktopError> {
    if has_control_chars(text) {
        // Unreachable through `op.order`, which refuses it `-32009`. Kept
        // because "unreachable" is a property of today's callers.
        return Err(DesktopError::NotOperator);
    }
    foreground(fg, target)?;
    ghostty::input_text(script, terminal, text)?;
    // The second check. Its whole value is that the first round trip took
    // ~118 ms (M12) and the CLI may have exited during it — and if it did,
    // the text is sitting on a shell's command line and the Return is the
    // thing that would run it. Refusing here leaves the sentence unexecuted
    // on the prompt, which the GUI's Copy and Retry path exists for.
    foreground(fg, target)?;
    ghostty::send_key(script, terminal, ghostty::ENTER)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::fakes::{FakeForeground, FakeGhostty};
    use crate::desktop::fg::Verdict;
    use crate::liveness::Row;
    use deck_model::{
        AgentId, AnswerCap, Caps, Kind, Owner, Principal, SessionId, SessionInfo, Terminal, Ts,
    };

    fn hook_agent() -> Agent {
        Agent {
            id: AgentId("ag_cd34".into()),
            slot: None,
            kind: Kind::Claude,
            name: "cohdl-api".into(),
            state: deck_model::AgentState::Idle,
            title: None,
            title_source: deck_model::TitleSource::SessionName,
            since: Ts(0),
            source: Source::Hook,
            owner: Owner {
                principal: Principal::Adapter(deck_model::AdapterKind::Claude),
                session: SessionId(1),
                lease_expires: None,
            },
            session: SessionInfo {
                key: Some("claude:sess-1".into()),
                cwd: Some("/Users/tony".into()),
                pid: Some(4321),
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
            meta: serde_json::Value::Null,
            redacted: false,
        }
    }

    /// **Guard 1.** The one agent shape that may be injected into, and the
    /// four that may not — each refused for the reason §5.5 gives.
    #[test]
    fn guard_one_builds_a_target_only_for_an_adapter_owned_cli_session() {
        let target = target_of(&hook_agent()).unwrap();
        assert_eq!(target.agent, "ag_cd34");
        assert_eq!(target.key, "claude:sess-1");
        assert_eq!(target.tty, "/dev/ttys004");
        assert_eq!(target.pid, 4321);
        assert_eq!(target.id4, "cd34");
        assert_eq!(target.nonce.as_deref(), Some("n0nce"));
        assert_eq!(target.program, "ghostty");

        // A card choice's route: the store sends those `push`/`poll`, and
        // an agent whose cap is not `inject` has no target here either.
        let mut pushy = hook_agent();
        pushy.caps.order = OrderCap::Push;
        assert_eq!(target_of(&pushy).unwrap_err(), DesktopError::NotOperator);
        let mut mute = hook_agent();
        mute.caps.order = OrderCap::None;
        assert_eq!(target_of(&mute).unwrap_err(), DesktopError::NotOperator);

        // §4.6: a virtual agent's own `terminal`/`pid` is informational.
        // Even with a perfect-looking terminal record, it is not a target.
        let mut virtual_agent = hook_agent();
        virtual_agent.source = Source::Api;
        assert_eq!(
            target_of(&virtual_agent).unwrap_err(),
            DesktopError::NotOperator,
            "a script cannot aim dictation at a tab of its choosing"
        );
        let mut mcp = hook_agent();
        mcp.source = Source::Mcp;
        assert_eq!(target_of(&mcp).unwrap_err(), DesktopError::NotOperator);
    }

    /// …and the shapes that are missing what guard 3 needs. These are
    /// `no_terminal` rather than `not_operator`: the caller was allowed to
    /// ask, there is simply nothing checkable to aim at.
    #[test]
    fn guard_one_refuses_an_agent_guard_three_could_not_check() {
        // A `shell` pid is the `sh -c` that ran the hook and exited a
        // millisecond later; guard 3 against it would *pass* on the shell.
        for kind in [Some(PidKind::Shell), Some(PidKind::Unknown), None] {
            let mut a = hook_agent();
            a.session.pid_kind = kind;
            assert_eq!(
                target_of(&a).unwrap_err(),
                DesktopError::NoTerminal,
                "{kind:?}"
            );
        }
        // No pid, no tty, an empty tty, no terminal record, no key.
        let mut no_pid = hook_agent();
        no_pid.session.pid = None;
        assert_eq!(target_of(&no_pid).unwrap_err(), DesktopError::NoTerminal);
        let mut no_terminal = hook_agent();
        no_terminal.session.terminal = None;
        assert_eq!(
            target_of(&no_terminal).unwrap_err(),
            DesktopError::NoTerminal
        );
        for tty in [None, Some(String::new())] {
            let mut a = hook_agent();
            a.session.terminal.as_mut().unwrap().tty = tty.clone();
            assert_eq!(target_of(&a).unwrap_err(), DesktopError::NoTerminal);
        }
        let mut no_key = hook_agent();
        no_key.session.key = None;
        assert_eq!(target_of(&no_key).unwrap_err(), DesktopError::NoTerminal);
        // A short agent id still yields a pin label.
        let mut short = hook_agent();
        short.id = AgentId("ag".into());
        assert_eq!(target_of(&short).unwrap().id4, "ag");
        assert_eq!(id4("ag_0123456789"), "6789");
    }

    /// `[desktop] inject` is an allow-list, and it is what makes "Ghostty
    /// is the only terminal on this Mac" a configuration rather than an
    /// assumption.
    #[test]
    fn injection_is_only_enabled_for_a_listed_terminal_program() {
        let allowed = vec!["ghostty".to_string()];
        assert!(enabled_for("ghostty", &allowed));
        assert!(enabled_for("Ghostty", &allowed), "TERM_PROGRAM is folded");
        assert!(!enabled_for("iTerm.app", &allowed));
        assert!(!enabled_for("Apple_Terminal", &allowed));
        assert!(!enabled_for("ghostty", &[]));
        assert!(enabled_for("anything", &["*".to_string()]));
    }

    /// `-32009`'s predicate. A `\r` or a `\n` in an order is a Return the
    /// operator did not ask for, and the tab is the one control character
    /// a dictated sentence can legitimately contain.
    #[test]
    fn control_characters_are_refused_and_a_tab_is_not_one() {
        assert!(!has_control_chars("deploy the api"));
        assert!(!has_control_chars("a\tb"));
        assert!(!has_control_chars(""));
        assert!(!has_control_chars("café ☕ — ok"));
        for bad in ["a\rb", "a\nb", "a\u{7}b", "a\u{1b}[2Jb", "a\u{0}b"] {
            assert!(has_control_chars(bad), "{bad:?}");
        }
    }

    /// **Guard 2's roster half**, with the fixture the measurement
    /// demands: one row that carries `waitingFor` and one that does not.
    #[test]
    fn guard_two_reads_the_roster_status_and_treats_an_absent_waiting_for_as_not_waiting() {
        let rows = vec![
            // An ordinary row: `status`, and no `waitingFor` key at all.
            Row {
                pid: Some(100),
                kind: Some("interactive".into()),
                session_id: Some("sess-busy".into()),
                status: Some("busy".into()),
                ..Row::default()
            },
            Row {
                pid: Some(101),
                kind: Some("interactive".into()),
                session_id: Some("sess-idle".into()),
                status: Some("idle".into()),
                ..Row::default()
            },
            // A waiting row with its reason …
            Row {
                pid: Some(102),
                kind: Some("interactive".into()),
                session_id: Some("sess-perm".into()),
                status: Some("waiting".into()),
                waiting_for: Some("permission".into()),
                ..Row::default()
            },
            // … and a waiting row without one, which is still waiting.
            Row {
                pid: Some(103),
                kind: Some("interactive".into()),
                session_id: Some("sess-bare".into()),
                status: Some("waiting".into()),
                ..Row::default()
            },
        ];
        assert_eq!(roster_waiting(&rows, "sess-busy"), None);
        assert_eq!(roster_waiting(&rows, "sess-idle"), None);
        assert_eq!(roster_waiting(&rows, "sess-perm"), Some("permission"));
        assert_eq!(roster_waiting(&rows, "sess-bare"), Some("waiting"));
        // A session the roster does not mention at all is not waiting: the
        // roster is evidence about sessions it lists and nothing else.
        assert_eq!(roster_waiting(&rows, "sess-unknown"), None);
        assert_eq!(roster_waiting(&[], "sess-perm"), None);
        // The key → session id mapping the lookup uses.
        assert_eq!(claude_session_of("claude:sess-perm"), Some("sess-perm"));
        assert_eq!(claude_session_of("claude:"), None);
        assert_eq!(claude_session_of("myapp:1"), None);
    }

    /// **Guard 3, allowed**: text then Return, in that order, aimed at the
    /// resolved terminal — and the check ran **twice**, against the
    /// recorded pid and the recorded tty.
    #[test]
    fn guard_three_runs_before_the_text_and_again_before_the_return() {
        let gh = FakeGhostty::new();
        gh.with_beacon("UUID-MINE", "n0nce")
            .with_beacon("UUID-OTHER", "other");
        let fg = FakeForeground::always(Verdict::Foreground);
        let target = target_of(&hook_agent()).unwrap();
        type_into(&gh, &fg, &target, "UUID-MINE", "deploy the api").unwrap();

        assert_eq!(fg.calls(), 2, "immediately before each of the two verbs");
        assert!(fg
            .seen()
            .iter()
            .all(|(pid, tty)| *pid == 4321 && tty == "/dev/ttys004"));
        assert_eq!(gh.typed_into("UUID-MINE"), "deploy the api");
        assert_eq!(gh.keys(), vec![("UUID-MINE".to_string(), "enter".into())]);
        assert_eq!(gh.typed_into("UUID-OTHER"), "", "nothing leaked (M4)");
        // The order of the two verbs, from the sources themselves.
        let sources = gh.sources();
        let text_at = sources
            .iter()
            .position(|s| s.contains("input text"))
            .unwrap();
        let key_at = sources.iter().position(|s| s.contains("send key")).unwrap();
        assert!(text_at < key_at);
    }

    /// **Guard 3, refused up front**: nothing is sent, *not even the
    /// text*. This is §15's "injection refused after the CLI exits to a
    /// shell" row, against the mechanism.
    #[test]
    fn a_refused_foreground_check_sends_nothing_not_even_the_text() {
        for verdict in [
            Verdict::Background,
            Verdict::Dead,
            Verdict::OtherTty,
            Verdict::NoTty,
            Verdict::TtyGone,
            Verdict::Unknown,
        ] {
            let gh = FakeGhostty::new();
            gh.with_beacon("UUID-MINE", "n0nce");
            let fg = FakeForeground::always(verdict);
            let target = target_of(&hook_agent()).unwrap();
            let err = type_into(&gh, &fg, &target, "UUID-MINE", "rm the thing").unwrap_err();
            assert_eq!(err, DesktopError::NotForeground, "{verdict}");
            assert_eq!(err.reason(), "not_foreground");
            assert!(gh.typed().is_empty(), "{verdict}: the text was sent");
            assert!(gh.keys().is_empty(), "{verdict}: a key was sent");
            assert_eq!(fg.calls(), 1, "{verdict}: it stopped at the first check");
        }
    }

    /// …and **refused between** the two verbs: the CLI exited during the
    /// ~118 ms `input text` took (M12). The text is out — it cannot be
    /// recalled — and the Return is not, which is the whole point.
    #[test]
    fn a_cli_that_exits_between_the_text_and_the_return_never_gets_the_return() {
        let gh = FakeGhostty::new();
        gh.with_beacon("UUID-MINE", "n0nce");
        let fg = FakeForeground::scripted(&[Verdict::Foreground, Verdict::Background]);
        let target = target_of(&hook_agent()).unwrap();
        let err = type_into(&gh, &fg, &target, "UUID-MINE", "deploy the api").unwrap_err();
        assert_eq!(err, DesktopError::NotForeground);
        assert_eq!(fg.calls(), 2);
        assert_eq!(gh.typed_into("UUID-MINE"), "deploy the api");
        assert!(
            gh.keys().is_empty(),
            "the sentence sits on the prompt, unexecuted — Copy and Retry, \
             never a Return nobody guarded"
        );
    }

    /// A control character that reached this far is refused before any
    /// script runs, and `-1700`/`-1728` from the two verbs come back as
    /// themselves rather than as a guard refusal.
    #[test]
    fn the_typing_path_reports_a_script_error_as_itself_and_not_as_a_guard() {
        let target = target_of(&hook_agent()).unwrap();
        let gh = FakeGhostty::new();
        gh.with_beacon("UUID-MINE", "n0nce");
        let fg = FakeForeground::always(Verdict::Foreground);
        assert_eq!(
            type_into(&gh, &fg, &target, "UUID-MINE", "a\rb").unwrap_err(),
            DesktopError::NotOperator
        );
        assert_eq!(fg.calls(), 0, "refused before the first check even ran");

        // The tab closed between the resolution and the text.
        let gone = FakeGhostty::new();
        gone.with_beacon("UUID-MINE", "n0nce").fail_on(
            "input text",
            crate::desktop::errors::ScriptError::Osa {
                number: crate::desktop::errors::ERR_NO_SUCH_OBJECT,
                message: "gone".into(),
            },
        );
        let err = type_into(&gone, &fg, &target, "UUID-MINE", "ok").unwrap_err();
        assert_eq!(err.reason(), "stale");
        assert!(err.invalidates_resolution());
        assert!(gone.keys().is_empty());

        // `-1743` from either verb is the automation denial, not a guard.
        let denied = FakeGhostty::new();
        denied.with_beacon("UUID-MINE", "n0nce").fail_always(
            crate::desktop::errors::ScriptError::Osa {
                number: crate::desktop::errors::ERR_NOT_PERMITTED,
                message: "Not authorized".into(),
            },
        );
        assert_eq!(
            type_into(&denied, &fg, &target, "UUID-MINE", "ok")
                .unwrap_err()
                .reason(),
            "automation_denied"
        );
    }
}
