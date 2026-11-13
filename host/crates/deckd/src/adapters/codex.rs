//! The Codex CLI adapter (design §5.2 with corrections C12–C21), measured
//! against `codex-cli 0.153.4`.
//!
//! What the measured facts changed, and why each one matters here:
//!
//! - **C13 — Codex hooks are shell form.** `hooks.json`'s `command` is a
//!   *string* (C12: the array form makes Codex discard the whole file with
//!   only a warning), so the hook's parent is a shell. The pid this
//!   adapter registers therefore comes from the helper's parent walk, not
//!   from `$PPID`.
//! - **C19 — `session_id` is a UUID**, byte-identical to the app-server
//!   `thread.id`, not the `thr_…` the vendor docs show. Nothing here
//!   prefix-matches it.
//! - **C21 — `PermissionRequest` also accepts exit 2 + stderr as a deny.**
//!   The JSON `decision` shape stays the primary path; the helper never
//!   exits non-zero, so we only ever use the JSON.
//! - **C17 — `notify` is a single command and is already occupied on this
//!   machine.** `Stop` carries the same `turn_id` and
//!   `last_assistant_message`, so the two are deduped **by the pair**
//!   `(session_id, turn_id)` — `turn_id` alone is unique only within a
//!   thread — and whichever arrives first wins.
//! - **C10's discipline, applied here:** Codex has twelve events and a
//!   thirteenth is a version away. Anything this adapter does not know is
//!   `Ingest::Nothing`: a hook that panicked on an unknown event would
//!   take a tool call down with it.
//!
//! Hooks only report state. A synchronous PermissionRequest hook runs
//! before Codex presents its own approval UI; holding it hides that UI.
//! Answers belong to the shared app-server connection, where the native
//! client and deck receive the same request and the first answer wins.
use super::rt::{Adapter, HookCtx, HookOutput, Ingest};
// §5.6's PTT grammar is `deck_model::voice`'s, not this adapter's (B13).
// This file used to carry its own list — the one with `sure` in it that
// `adapters::claude`'s did not have, and without the `yep|yeah|nope` that
// Claude's did — so one deck, one spoken word and two different answers.
use deck_model::voice::{intent, Intent};
use deck_model::{
    sanitize, AdapterKind, AgentRegister, AgentState, AgentUpdate, Answer, AnswerCap, Caps, Card,
    Kind, OrderCap, ReplySet, Request, TitleSource,
};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What this adapter calls itself on its internal session.
pub const CLIENT: &str = "adapter:codex";
/// The choices a Codex permission card offers, in order (`Allow` is
/// `rec`). There is deliberately no "Allow always": the hook protocol has
/// no session-scoped approval, and a row that silently meant "just this
/// once" would be the worst kind of lie on a hardware button.
pub const ALLOW: &str = "Allow";
pub const DENY: &str = "Deny";
/// What a `Deny` pressed on the deck tells the model.
pub const DENIED: &str = "Denied from the deck";
/// A title from a prompt or a tool is cut to this many bytes before the
/// §5.6 card rules see it (§5.1's "first 64 B of prompt").
pub const TITLE_MAX: usize = 64;

/// How long a `(session_id, turn_id)` pair is remembered, and how many.
/// Ten minutes covers any gap between the `Stop` hook and `notify` for
/// the same turn; 256 pairs is far more than the eight concurrent hooks
/// Codex allows per session.
pub const TURN_MEMORY: Duration = Duration::from_secs(600);
pub const TURN_MAX: usize = 256;

/// The Codex adapter. Its one piece of state is the turn memo: `Stop` and
/// `notify agent-turn-complete` report the same turn from two different
/// doors, and the second one must be a no-op rather than a second reply.
#[derive(Debug, Default)]
pub struct Codex {
    turns: Mutex<VecDeque<(String, Instant)>>,
}

impl Codex {
    /// True the first time this turn is seen, false every time after.
    /// Bounded in both time and size: a memo that grew with the session
    /// would be a leak in a daemon that runs for weeks.
    fn claim_turn(&self, key: &str, now: Instant) -> bool {
        let Ok(mut turns) = self.turns.lock() else {
            // A poisoned memo must not silence a turn: report it twice
            // rather than not at all.
            return true;
        };
        while turns
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > TURN_MEMORY)
        {
            turns.pop_front();
        }
        if turns.iter().any(|(seen, _)| seen == key) {
            return false;
        }
        if turns.len() >= TURN_MAX {
            turns.pop_front();
        }
        turns.push_back((key.to_string(), now));
        true
    }

    /// The assistant's last message, then `state: ready`, once per turn.
    ///
    /// In that order because the store speaks the ready line from the
    /// reply on file the moment the state changes: the other way round,
    /// the deck read out the previous turn's answer.
    fn turn_complete(&self, p: &serde_json::Value, ctx: &HookCtx) -> Ingest {
        let (Some(agent), Some(session), Some(turn)) =
            (ctx.agent.clone(), session_id(p), turn_id(p))
        else {
            return Ingest::Nothing;
        };
        if !self.claim_turn(&format!("{session}/{turn}"), Instant::now()) {
            return Ingest::Nothing;
        }
        let mut reqs = Vec::with_capacity(2);
        if let Some(text) = last_message(p) {
            reqs.push(Request::Reply(ReplySet {
                message: None,
                agent: agent.clone(),
                text,
                speak: false,
                announce: None,
                lane: None,
            }));
        }
        reqs.push(state(agent, AgentState::Ready));
        Ingest::Report(reqs)
    }
}

impl Adapter for Codex {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Codex
    }

    fn client(&self) -> &'static str {
        CLIENT
    }

    fn key(&self, payload: &serde_json::Value) -> Option<String> {
        session_id(payload).map(|id| format!("codex:{id}"))
    }

    fn register(&self, payload: &serde_json::Value, ctx: &HookCtx) -> Option<AgentRegister> {
        let key = self.key(payload)?;
        // A `SessionEnd` (or a stray `notify`) for a session we never saw
        // must not put one on the roster: the only thing it could do is
        // register an agent and immediately release it.
        if matches!(event(payload), Event::SessionEnd | Event::Unknown) {
            return None;
        }
        let cwd = payload["cwd"].as_str().map(str::to_string);
        Some(AgentRegister {
            kind: Kind::Codex,
            name: name_of(cwd.as_deref()),
            key: Some(key),
            cwd,
            pid: ctx.cli_pid,
            pid_kind: Some(ctx.pid_kind),
            terminal: Some(ctx.terminal()),
            // An adapter's agents live by hook events, not by a lease:
            // the roster drops them on `SessionEnd` or when the pid goes.
            lease_ttl_s: 0,
            caps: Caps {
                answer: AnswerCap::None,
                order: OrderCap::Inject,
                // Nothing to bring forward without a terminal or an app (a
                // Desktop-hosted thread), and §4.15 says an agent that does
                // not claim it cannot be focused.
                focus: ctx.tty.is_some() || ctx.app.is_some(),
                transcribe: Default::default(),
            },
            slot_hint: None,
            voice: None,
            meta: None,
        })
    }

    fn ingest(&self, payload: &serde_json::Value, ctx: &HookCtx) -> Ingest {
        let Some(agent) = ctx.agent.clone() else {
            // Nothing to attach to: either the register was refused or
            // this is a `SessionEnd` for a session the daemon never saw.
            return Ingest::Nothing;
        };
        match event(payload) {
            // The session is on the roster by the time we get here; the
            // only thing left to say is that it is not busy yet. C18: this
            // fires on the session's **first turn**, not when it opens.
            Event::SessionStart => Ingest::Report(vec![state(agent, AgentState::Idle)]),
            Event::SessionEnd => Ingest::Report(vec![Request::AgentRelease {
                agent,
                reason: Some("session_end".into()),
            }]),
            Event::UserPromptSubmit => Ingest::Report(vec![working(
                agent,
                payload["prompt"].as_str(),
                TitleSource::Prompt,
            )]),
            Event::PreToolUse => Ingest::Report(vec![working(
                agent,
                Some(&summary(payload)),
                TitleSource::Tool,
            )]),
            // Always fall through to Codex's own approval flow, including
            // while the app-server is reconnecting or discovering a new thread.
            Event::PermissionRequest => Ingest::Report(vec![state(agent, AgentState::Question)]),
            Event::Stop | Event::TurnComplete => self.turn_complete(payload, ctx),
            // An interrupted turn is not a finished one: no reply, no
            // ready card, just a session that is waiting for its operator.
            Event::Interrupt => Ingest::Report(vec![state(agent, AgentState::Idle)]),
            Event::Unknown => Ingest::Nothing,
        }
    }

    fn answer_to_output(&self, card: &Card, ans: Option<&Answer>) -> HookOutput {
        // A release — snooze, the hold budget, the hook client going away
        // — decides nothing, and Codex then runs its own approval flow
        // (facts §4.2: "if no hook decides, the normal approval flow
        // runs"), which is exactly what the degraded card points at.
        let Some(ans) = ans else {
            return HookOutput::Empty;
        };
        HookOutput::Json(serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": decision(card, ans),
            }
        }))
    }
}

/// `{"behavior":"allow"}` or `{"behavior":"deny","message":…}`.
///
/// A deny always carries a message: it is the only thing the model is
/// told, and "denied" with no reason makes it retry the same command.
fn decision(card: &Card, ans: &Answer) -> serde_json::Value {
    if let Some(label) = chosen(card, ans) {
        return match label.as_str() {
            ALLOW => serde_json::json!({"behavior": "allow"}),
            _ => serde_json::json!({"behavior": "deny", "message": DENIED}),
        };
    }
    // Free text (a PTT transcript, or the GUI): §5.6's intent grammar,
    // and anything it does not recognise is a deny that hands the model
    // the operator's own words to re-plan with.
    let text = ans.text.as_deref().unwrap_or("").trim();
    match intent(text) {
        Intent::Allow => serde_json::json!({"behavior": "allow"}),
        Intent::Deny => serde_json::json!({"behavior": "deny", "message": DENIED}),
        Intent::Say if text.is_empty() => {
            serde_json::json!({"behavior": "deny", "message": DENIED})
        }
        Intent::Say => serde_json::json!({"behavior": "deny", "message": sanitize::collapse(text)}),
    }
}

/// The option the operator picked, mapped back through the shown→sent
/// index map (B10) so a card that had to be truncated still answers with
/// the option this adapter sent.
fn chosen(card: &Card, ans: &Answer) -> Option<String> {
    let shown = usize::from(ans.choice?);
    let original = if card.choice_map.is_empty() {
        shown
    } else {
        usize::from(*card.choice_map.get(shown)?)
    };
    // Two choices, both ours, so the sent list is the source of truth;
    // `Answer.label` is what the deck drew and can have been cut.
    match original {
        0 => Some(ALLOW.into()),
        1 => Some(DENY.into()),
        _ => ans.label.clone(),
    }
}

/// The twelve events this adapter knows, plus `notify`'s synthetic one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    Stop,
    Interrupt,
    /// `notify`'s `agent-turn-complete` — not a hook event, the same turn.
    TurnComplete,
    /// `PostToolUse`, `PreCompact`, `PostCompact`, `SubagentStart`,
    /// `SubagentStop`, and whatever the next release adds.
    Unknown,
}

/// The event a payload is, from `hook_event_name` (PascalCase on the wire,
/// facts §1.4) or from `notify`'s `type`.
fn event(p: &serde_json::Value) -> Event {
    if p["type"].as_str() == Some("agent-turn-complete") {
        return Event::TurnComplete;
    }
    match p["hook_event_name"].as_str().unwrap_or_default() {
        "SessionStart" => Event::SessionStart,
        "SessionEnd" => Event::SessionEnd,
        "UserPromptSubmit" => Event::UserPromptSubmit,
        "PreToolUse" => Event::PreToolUse,
        "PermissionRequest" => Event::PermissionRequest,
        "Stop" => Event::Stop,
        "Interrupt" => Event::Interrupt,
        _ => Event::Unknown,
    }
}

/// The thread id: `session_id` on a hook payload, `thread-id` on a notify
/// one. They are the same id (facts §4.1), kebab-cased on the notify side.
fn session_id(p: &serde_json::Value) -> Option<&str> {
    p["session_id"].as_str().or_else(|| p["thread-id"].as_str())
}

/// The turn id, same two spellings.
fn turn_id(p: &serde_json::Value) -> Option<&str> {
    p["turn_id"].as_str().or_else(|| p["turn-id"].as_str())
}

/// The assistant's closing message, sanitised, or `None` when there was
/// none (a turn that failed, or a `Stop` with nothing to say).
fn last_message(p: &serde_json::Value) -> Option<String> {
    let raw = p["last_assistant_message"]
        .as_str()
        .or_else(|| p["last-assistant-message"].as_str())?;
    // §5.6 without the 128-byte cut: that cut is a card-text rule, and a
    // reply is kept whole in the store (`sanitize::clean`).
    let text = sanitize::clean(raw);
    (!text.is_empty()).then_some(text)
}

/// `state: working` with a title, which is content (§4.3) and is classed
/// as such so `observe` without `observe:content` never sees it.
fn working(agent: deck_model::AgentId, raw: Option<&str>, source: TitleSource) -> Request {
    let title = raw
        .map(|t| sanitize::cut(&sanitize::text(t), TITLE_MAX))
        .filter(|t| !t.is_empty());
    Request::AgentUpdate(AgentUpdate {
        agent,
        state: Some(AgentState::Working),
        title_source: title.is_some().then_some(source),
        title,
        since: None,
        name: None,
        meta: None, task: None, subagents: None,
    })
}

/// A bare state change.
fn state(agent: deck_model::AgentId, to: AgentState) -> Request {
    Request::AgentUpdate(AgentUpdate {
        agent,
        state: Some(to),
        title: None,
        title_source: None,
        since: None,
        name: None,
        meta: None, task: None, subagents: None,
    })
}

/// What the card (and the `PreToolUse` title) says a tool is about to do:
/// `"<tool>: <the command, the description, or the arguments>"`.
///
/// `tool_input.command` first because it is what `Bash` and `apply_patch`
/// carry and the only thing an operator can decide on; `description` is
/// "when Codex has one" (facts §4.1) and must not be relied on; an MCP
/// tool sends its whole argument object, which is better shown compact
/// than not at all. The §5.6 sanitiser does the rest.
fn summary(p: &serde_json::Value) -> String {
    let tool = p["tool_name"].as_str().unwrap_or("tool");
    let input = &p["tool_input"];
    let what = match &input["command"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(words) => words
            .iter()
            .filter_map(|w| w.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => input["description"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| match input {
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            }),
    };
    if what.trim().is_empty() {
        tool.to_string()
    } else {
        format!("{tool}: {what}")
    }
}

/// The agent's name: the working directory's basename, as §5.1 does it,
/// because a Codex session has no name of its own until the thread gets a
/// title. Never empty — the roster refuses a blank name.
fn name_of(cwd: Option<&str>) -> String {
    let base = cwd
        .map(|c| c.trim_end_matches('/'))
        .and_then(|c| c.rsplit('/').next())
        .map(str::trim)
        .unwrap_or_default();
    if base.is_empty() {
        "codex".into()
    } else {
        sanitize::cut(&sanitize::collapse(base), TITLE_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::rt::HookCall;
    use deck_model::{AnswerSource, CardId, CardStatus, Choice, Deliver, Origin, Principal};

    fn ctx(agent: &str) -> HookCtx {
        let mut ctx = HookCtx::of(&HookCall::default(), 120);
        ctx.agent = Some(deck_model::AgentId(agent.into()));
        ctx
    }

    fn card(choices: &[&str], map: &[u8]) -> Card {
        Card {
            id: CardId(1),
            kind: deck_model::CardKind::Q,
            status: CardStatus::Pending,
            deliver: Deliver::Hold,
            origin: Origin::custom(),
            text: Some("Bash: ls".into()),
            choices: choices
                .iter()
                .map(|l| Choice {
                    label: (*l).to_string(),
                    detail: None,
                })
                .collect(),
            choices_n: u8::try_from(choices.len()).unwrap_or(u8::MAX),
            choice_map: map.to_vec(),
            rec: Some(0),
            free_text: true,
            page: deck_model::Page { index: 0, count: 1 },
            created: deck_model::Ts(0),
            expires: None,
            redacted: false,
        }
    }

    fn answer(choice: Option<u8>, label: Option<&str>, text: Option<&str>) -> Answer {
        Answer {
            choice,
            label: label.map(str::to_string),
            text: text.map(str::to_string),
            source: AnswerSource::Deck,
            principal: Principal::Device,
            session: None,
            utterance: None,
        }
    }

    #[test]
    fn the_event_name_is_read_from_both_spellings() {
        assert_eq!(
            event(&serde_json::json!({"hook_event_name": "PermissionRequest"})),
            Event::PermissionRequest
        );
        assert_eq!(
            event(&serde_json::json!({"type": "agent-turn-complete"})),
            Event::TurnComplete
        );
        // C10's discipline: an event from a newer Codex is ignored, never
        // guessed at and never a panic.
        for unknown in ["PostToolUse", "PreCompact", "SubagentStart", "Whatever"] {
            assert_eq!(
                event(&serde_json::json!({"hook_event_name": unknown})),
                Event::Unknown,
                "{unknown}"
            );
        }
        assert_eq!(event(&serde_json::json!({})), Event::Unknown);
    }

    #[test]
    fn the_session_id_is_a_uuid_and_is_never_prefix_matched() {
        // C19: the vendor docs show `thr_123`; 0.153.4 sends a UUID.
        let uuid = "01a0bcad-d448-7a72-abe6-0a776fb0b80c";
        let codex = Codex::default();
        assert_eq!(
            codex.key(&serde_json::json!({"session_id": uuid})),
            Some(format!("codex:{uuid}"))
        );
        // `notify` spells it `thread-id`, and it is the same id.
        assert_eq!(
            codex.key(&serde_json::json!({"thread-id": uuid})),
            Some(format!("codex:{uuid}"))
        );
        assert_eq!(codex.key(&serde_json::json!({})), None);
    }

    #[test]
    fn an_unknown_event_reports_nothing() {
        let codex = Codex::default();
        let out = codex.ingest(
            &serde_json::json!({"hook_event_name": "PostToolUse", "session_id": "u"}),
            &ctx("ag_1"),
        );
        assert!(matches!(out, Ingest::Nothing));
    }

    #[test]
    fn a_session_end_never_registers_a_session_we_never_saw() {
        let codex = Codex::default();
        let payload = serde_json::json!({"hook_event_name": "SessionEnd", "session_id": "u"});
        assert!(codex.register(&payload, &ctx("ag_1")).is_none());
        // Everything else does register, so a daemon that started mid
        // session still picks the session up.
        for event in ["SessionStart", "UserPromptSubmit", "PermissionRequest"] {
            let payload = serde_json::json!({"hook_event_name": event, "session_id": "u"});
            assert!(codex.register(&payload, &ctx("ag_1")).is_some(), "{event}");
        }
    }

    #[test]
    fn the_registration_carries_the_walk_and_the_terminal() {
        let codex = Codex::default();
        let mut ctx = HookCtx::of(
            &HookCall {
                payload: serde_json::Value::Null,
                env: [("TERM_PROGRAM".to_string(), "ghostty".to_string())]
                    .into_iter()
                    .collect(),
                tty: Some("/dev/ttys004".into()),
                cli_pid: Some(4242),
                pid_kind: Some(deck_model::PidKind::Cli),
                app: Some("com.mitchellh.ghostty".into()),
            },
            120,
        );
        ctx.agent = None;
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "01a0bca9-1344-7d20-a793-10903639be67",
            "cwd": "/Users/someone/projects/cohdl-api"
        });
        let reg = codex.register(&payload, &ctx).expect("a registration");
        assert_eq!(reg.kind, Kind::Codex);
        assert_eq!(reg.name, "cohdl-api", "the cwd basename, as §5.1 does it");
        assert_eq!(reg.pid, Some(4242), "the walk's pid, never $PPID");
        assert_eq!(reg.pid_kind, Some(deck_model::PidKind::Cli));
        assert_eq!(reg.lease_ttl_s, 0, "an adapter's agents never lease");
        assert_eq!(reg.caps.answer, AnswerCap::None);
        assert!(reg.caps.focus, "there is a tty to focus");
        let terminal = reg.terminal.expect("a terminal");
        assert_eq!(terminal.program.as_deref(), Some("ghostty"));
        assert_eq!(terminal.tty.as_deref(), Some("/dev/ttys004"));
        assert_eq!(terminal.app.as_deref(), Some("com.mitchellh.ghostty"));
    }

    /// A session the helper found an app for but no tty (a Desktop-hosted
    /// CLI) can still be brought forward: it claims focus.
    #[test]
    fn an_app_without_a_tty_still_claims_focus() {
        let codex = Codex::default();
        let ctx = HookCtx::of(
            &HookCall {
                app: Some("com.openai.codex".into()),
                ..HookCall::default()
            },
            120,
        );
        let payload =
            serde_json::json!({"hook_event_name": "SessionStart", "session_id": "u", "cwd": "/"});
        let reg = codex.register(&payload, &ctx).expect("a registration");
        assert!(reg.caps.focus);
        assert_eq!(
            reg.terminal.and_then(|t| t.app).as_deref(),
            Some("com.openai.codex")
        );
    }

    #[test]
    fn a_session_without_a_terminal_does_not_claim_it_can_be_focused() {
        let codex = Codex::default();
        let ctx = HookCtx::of(&HookCall::default(), 120);
        let payload =
            serde_json::json!({"hook_event_name": "SessionStart", "session_id": "u", "cwd": "/"});
        let reg = codex.register(&payload, &ctx).expect("a registration");
        assert!(!reg.caps.focus);
        assert_eq!(reg.pid_kind, Some(deck_model::PidKind::Unknown));
        assert_eq!(reg.name, "codex", "a blank basename is never a blank name");
    }

    #[test]
    fn a_permission_hook_reports_attention_without_holding_the_native_ui() {
        let codex = Codex::default();
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "u",
            "tool_name": "Bash", "tool_input": {"command": "ls"}
        });
        let Ingest::Report(requests) = codex.ingest(&payload, &ctx("ag_1")) else {
            panic!("permission hooks must return immediately");
        };
        assert!(matches!(requests.as_slice(), [Request::AgentUpdate(update)]
            if update.state == Some(AgentState::Question)));
    }

    #[test]
    fn the_card_text_survives_the_shapes_a_tool_input_comes_in() {
        let table = [
            (
                serde_json::json!({"tool_name":"Bash","tool_input":{"command":"ls -la"}}),
                "Bash: ls -la",
            ),
            (
                serde_json::json!({"tool_name":"apply_patch","tool_input":{"command":["git","apply","x.patch"]}}),
                "apply_patch: git apply x.patch",
            ),
            (
                serde_json::json!({"tool_name":"Bash","tool_input":{"description":"delete the build"}}),
                "Bash: delete the build",
            ),
            (
                serde_json::json!({"tool_name":"mcp__chrome__click","tool_input":{"selector":"#go"}}),
                "mcp__chrome__click: {\"selector\":\"#go\"}",
            ),
            (serde_json::json!({"tool_name":"Bash"}), "Bash"),
            (serde_json::json!({}), "tool"),
        ];
        for (payload, want) in table {
            assert_eq!(summary(&payload), want, "{payload}");
        }
    }

    #[test]
    fn an_allow_and_a_deny_are_the_two_shapes_codex_accepts() {
        let codex = Codex::default();
        let card = card(&[ALLOW, DENY], &[]);
        let allow = codex.answer_to_output(&card, Some(&answer(Some(0), Some(ALLOW), None)));
        assert_eq!(
            allow,
            HookOutput::Json(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "allow"}
                }
            }))
        );
        let deny = codex.answer_to_output(&card, Some(&answer(Some(1), Some(DENY), None)));
        let HookOutput::Json(deny) = deny else {
            panic!("a deny is JSON")
        };
        assert_eq!(
            deny["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(deny["hookSpecificOutput"]["decision"]["message"], DENIED);
    }

    #[test]
    fn a_release_decides_nothing() {
        let codex = Codex::default();
        assert_eq!(
            codex.answer_to_output(&card(&[ALLOW, DENY], &[]), None),
            HookOutput::Empty
        );
    }

    #[test]
    fn a_truncated_card_maps_its_answer_back_through_the_shown_index() {
        // Codex never sends more than two options, but the mapping is the
        // seam B10 built: the store moves the recommended option to row 0
        // and records where each shown row came from. An adapter that
        // read `Answer.choice` as an index into what it *sent* would
        // allow what the operator denied, the day a card is reordered.
        let codex = Codex::default();
        // Shown `[Deny, Allow]`; row 0 was sent at index 1, row 1 at 0.
        let mapped = card(&[DENY, ALLOW], &[1, 0]);
        let behavior = |row: u8, label: &str| {
            let out = codex.answer_to_output(&mapped, Some(&answer(Some(row), Some(label), None)));
            let HookOutput::Json(out) = out else {
                panic!("JSON")
            };
            out["hookSpecificOutput"]["decision"]["behavior"].clone()
        };
        assert_eq!(behavior(0, DENY), "deny", "shown row 0 is the sent Deny");
        assert_eq!(behavior(1, ALLOW), "allow", "shown row 1 is the sent Allow");
        // Without the map the same two rows read the other way round.
        let flat = card(&[DENY, ALLOW], &[]);
        let out = codex.answer_to_output(&flat, Some(&answer(Some(0), Some(DENY), None)));
        let HookOutput::Json(out) = out else {
            panic!("JSON")
        };
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
    }

    #[test]
    fn spoken_words_answer_a_permission_card_and_anything_else_is_a_reason() {
        let codex = Codex::default();
        let card = card(&[ALLOW, DENY], &[]);
        let say = |text: &str| {
            let out = codex.answer_to_output(&card, Some(&answer(None, None, Some(text))));
            let HookOutput::Json(out) = out else {
                panic!("JSON")
            };
            out["hookSpecificOutput"]["decision"].clone()
        };
        assert_eq!(say("yes")["behavior"], "allow");
        assert_eq!(say("Go ahead.")["behavior"], "allow");
        assert_eq!(say("no")["behavior"], "deny");
        assert_eq!(say("don't")["behavior"], "deny");
        let instruction = say("use the staging database instead");
        assert_eq!(instruction["behavior"], "deny");
        assert_eq!(
            instruction["message"], "use the staging database instead",
            "the operator's words reach the model so it can re-plan"
        );
        assert_eq!(say("")["message"], DENIED, "a blank is not an instruction");
    }

    #[test]
    fn a_turn_is_reported_once_however_many_doors_report_it() {
        let codex = Codex::default();
        let c = ctx("ag_1");
        let stop = serde_json::json!({
            "hook_event_name": "Stop", "session_id": "s1", "turn_id": "t1",
            "last_assistant_message": "Done: the migration ran."
        });
        let notify = serde_json::json!({
            "type": "agent-turn-complete", "thread-id": "s1", "turn-id": "t1",
            "last-assistant-message": "Done: the migration ran."
        });
        let Ingest::Report(first) = codex.ingest(&stop, &c) else {
            panic!("Stop reports")
        };
        assert_eq!(first.len(), 2, "a state change and a reply");
        assert!(matches!(codex.ingest(&notify, &c), Ingest::Nothing));
        // A different turn of the same thread is a different turn.
        let next = serde_json::json!({
            "type": "agent-turn-complete", "thread-id": "s1", "turn-id": "t2",
            "last-assistant-message": "And the second one."
        });
        assert!(matches!(codex.ingest(&next, &c), Ingest::Report(_)));
        // …and the same turn id in another thread, too: `turn_id` is
        // unique only within a thread (facts §5.2).
        let elsewhere = serde_json::json!({
            "hook_event_name": "Stop", "session_id": "s2", "turn_id": "t1"
        });
        assert!(matches!(codex.ingest(&elsewhere, &c), Ingest::Report(_)));
    }

    /// The store speaks the ready line from the reply on file the moment
    /// the state turns `ready`, so a finished turn stores its reply first,
    /// whichever door reported it.
    #[test]
    fn a_finished_turn_stores_its_reply_before_it_turns_ready() {
        let codex = Codex::default();
        let c = ctx("ag_1");
        let stop = serde_json::json!({
            "hook_event_name": "Stop", "session_id": "s1", "turn_id": "t1",
            "last_assistant_message": "Done: the migration ran."
        });
        let notify = serde_json::json!({
            "type": "agent-turn-complete", "thread-id": "s1", "turn-id": "t2",
            "last-assistant-message": "And the second one."
        });
        for (payload, said) in [
            (stop, "Done: the migration ran."),
            (notify, "And the second one."),
        ] {
            let Ingest::Report(reqs) = codex.ingest(&payload, &c) else {
                panic!("a finished turn reports")
            };
            assert!(
                matches!(reqs.as_slice(), [Request::Reply(r), Request::AgentUpdate(u)]
                    if r.text == said && u.state == Some(AgentState::Ready)),
                "{reqs:?}"
            );
        }
    }

    #[test]
    fn a_turn_with_nothing_to_say_still_reports_ready() {
        let codex = Codex::default();
        let stop = serde_json::json!({
            "hook_event_name": "Stop", "session_id": "s9", "turn_id": "t1",
            "last_assistant_message": serde_json::Value::Null
        });
        let Ingest::Report(reqs) = codex.ingest(&stop, &ctx("ag_1")) else {
            panic!("Stop reports")
        };
        assert_eq!(reqs.len(), 1, "no empty reply");
        assert!(matches!(
            &reqs[0],
            Request::AgentUpdate(u) if u.state == Some(AgentState::Ready)
        ));
    }

    #[test]
    fn the_turn_memo_is_bounded_in_time_and_in_size() {
        let codex = Codex::default();
        let t0 = Instant::now();
        assert!(codex.claim_turn("s/t", t0));
        assert!(!codex.claim_turn("s/t", t0 + Duration::from_secs(60)));
        // Past the memory the pair is new again: a daemon that ran for a
        // week must not carry every turn it ever saw.
        assert!(codex.claim_turn("s/t", t0 + TURN_MEMORY + Duration::from_secs(1)));
        for i in 0..TURN_MAX * 2 {
            codex.claim_turn(&format!("s/{i}"), t0);
        }
        assert!(codex.turns.lock().unwrap().len() <= TURN_MAX);
    }

    #[test]
    fn an_interrupt_is_idle_and_not_a_finished_turn() {
        let codex = Codex::default();
        let payload =
            serde_json::json!({"hook_event_name": "Interrupt", "session_id": "s", "turn_id": "t"});
        let Ingest::Report(reqs) = codex.ingest(&payload, &ctx("ag_1")) else {
            panic!("Interrupt reports")
        };
        assert!(matches!(
            &reqs[0],
            Request::AgentUpdate(u) if u.state == Some(AgentState::Idle)
        ));
        assert_eq!(reqs.len(), 1, "no reply: the turn did not finish");
    }

    #[test]
    fn a_prompt_becomes_a_working_title_that_is_classed_as_content() {
        let codex = Codex::default();
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "s",
            "prompt": "\u{1b}[1mrefactor\u{1b}[0m   the   header\n\nplease"
        });
        let Ingest::Report(reqs) = codex.ingest(&payload, &ctx("ag_1")) else {
            panic!("reports")
        };
        let Request::AgentUpdate(u) = &reqs[0] else {
            panic!("an update")
        };
        assert_eq!(u.state, Some(AgentState::Working));
        assert_eq!(u.title.as_deref(), Some("refactor the header please"));
        assert_eq!(u.title_source, Some(TitleSource::Prompt));
        assert!(
            u.title_source
                .is_some_and(deck_model::TitleSource::is_content),
            "a prompt title is content and is redacted without observe:content"
        );
    }

    #[test]
    fn a_title_is_cut_to_sixty_four_bytes_on_a_character_boundary() {
        let codex = Codex::default();
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "s",
            "prompt": "é".repeat(200)
        });
        let Ingest::Report(reqs) = codex.ingest(&payload, &ctx("ag_1")) else {
            panic!("reports")
        };
        let Request::AgentUpdate(u) = &reqs[0] else {
            panic!("an update")
        };
        let title = u.title.as_deref().expect("a title");
        assert!(title.len() <= TITLE_MAX, "{}", title.len());
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_pre_tool_use_titles_the_agent_with_what_the_tool_will_do() {
        let codex = Codex::default();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "s",
            "tool_name": "Bash", "tool_input": {"command": "cargo test"}
        });
        let Ingest::Report(reqs) = codex.ingest(&payload, &ctx("ag_1")) else {
            panic!("reports")
        };
        let Request::AgentUpdate(u) = &reqs[0] else {
            panic!("an update")
        };
        assert_eq!(u.title.as_deref(), Some("Bash: cargo test"));
        assert_eq!(u.title_source, Some(TitleSource::Tool));
    }

    #[test]
    fn an_assistant_message_is_sanitised_before_it_becomes_a_reply() {
        let raw = "\u{1b}[32mdone\u{1b}[0m\n\n```rust\nfn main() {}\n```\n";
        let p = serde_json::json!({"last_assistant_message": raw});
        let text = last_message(&p).expect("a message");
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(text.contains("code: fn main() {}"), "{text}");
        assert_eq!(last_message(&serde_json::json!({})), None);
        assert_eq!(
            last_message(&serde_json::json!({"last_assistant_message": "  "})),
            None
        );
    }
}
