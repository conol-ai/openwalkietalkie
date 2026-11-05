//! The Claude Code adapter (design §5.1, corrected by C1–C11 and C25 of
//! the week-3 contract, all measured against the installed `claude
//! 2.1.276`).
//!
//! Five facts from that measurement shape everything below, and each one
//! is a bug if it is dropped:
//!
//! * **C2** — `hookSpecificOutput.hookEventName` must equal the event that
//!   fired or Claude throws the **whole** response away, decision and all.
//!   Every response here is built from the payload's own event name and
//!   the fixtures assert it.
//! * **C5** — `PermissionRequest` carries **no `tool_use_id`**, so the
//!   dedupe key is `(session_id, tool_name, blake3(tool_input))` inside a
//!   5 s window, and the "belongs to a released question group" rule
//!   becomes `tool_name == "AskUserQuestion"` **or** a group released for
//!   this session in the last 30 s.
//! * **C7** — `AskUserQuestion` is schema-bounded to 1–4 questions and 2–4
//!   options, so a Claude question card can never need the `MAX_CHOICES`
//!   truncation path; a multi-select answer is a **comma-joined string**,
//!   never an array.
//! * **C8** — "held" is not uniform. A held `PermissionRequest` does
//!   **not** suppress the terminal dialog (it is drawn 5–22 ms after the
//!   hook fires), so that card is *first answer wins* and is cleared by
//!   the `PostToolUse`/`Stop` that says the operator answered in the
//!   terminal. A held `PreToolUse{AskUserQuestion}` **does** suppress its
//!   dialog and owns it for the whole hold.
//! * **C10** — 2.1.276 has 17 `Notification` types where the docs list 12.
//!   Anything unknown is ignored, never a panic.
//!
//! And **C25**: the OSC 2 stamp does not survive the next title write, so
//! `terminalSequence` is a one-shot beacon on `SessionStart` alone — §5.1's
//! plan of re-emitting it on `UserPromptSubmit`/`Stop` is dropped.
use super::{
    is_pending, pending, Adapter, GroupPlan, GroupQuestion, HookCfg, HookCtx, HookOutput,
    HookPayload, Ingest,
};
use deck_model::{
    sanitize, AdapterKind, AgentId, AgentRegister, AgentState, AgentUpdate, Answer, AnswerCap,
    Caps, CardClear, CardId, CardKind, CardSet, Choice, Deliver, Group, Kind, OrderCap, Origin,
    OriginKind, Principal, Release, ReplySet, Request, SessionId, Terminal, TitleSource,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The tool whose `PermissionRequest` is always answered `{}`: it is the
/// question group's own follow-up and would otherwise raise a second,
/// permission-origin card on top of the one already on the deck (C5, S2).
pub const ASK: &str = "AskUserQuestion";

/// How long two identical `PermissionRequest`s count as one (C5).
pub const DEDUPE: Duration = Duration::from_secs(5);
/// How long after a released question group a `PermissionRequest` from
/// that session is still the group's own follow-up (C5).
pub const RELEASED: Duration = Duration::from_secs(30);
/// How long a CLI session's memo outlives its last hook.
///
/// `SessionEnd` is the tidy end of a session and `forget`s everything at
/// once — but a `SIGKILL`ed, crashed or power-cut CLI never sends one, and
/// a daemon that runs for weeks would keep that session's agent id and its
/// three card ids for ever. So the memo is a window like the dedupe ones:
/// a session nothing has been heard from for this long is forgotten, and a
/// later hook from it simply registers again (`agent.register` is
/// idempotent by `key`, §4.6, so nothing is lost but the guards).
///
/// An hour is chosen from what the guards are for, not from taste: the
/// longest-lived thing in the memo is the Ready card, which the store
/// itself expires after `[cards] ready_ttl_s` (900 s), and the shortest is
/// a parked hold, bounded by `[hooks.claude] hold_s` (120 s). An hour is
/// clear of both, so pruning can never drop a guard for a card that is
/// still on the deck.
pub const MEMO_IDLE: Duration = Duration::from_secs(3600);
/// The longest prompt head that becomes a title (§5.1).
pub const TITLE_MAX: usize = 64;
/// The choices of a permission card, in the order the deck shows them.
pub const ALLOW: &str = "Allow";
pub const ALLOW_ALWAYS: &str = "Allow always";
pub const DENY: &str = "Deny";
/// The deny message a card answer carries back to Claude.
pub const DENIED: &str = "Denied from the deck";
/// The one choice of an informational focus card (§5.6).
pub const FOCUS: &str = "Focus terminal";

/// What the adapter remembers between payloads.
///
/// All of it is a memo — something the store already knows, or a window
/// the design defines — and none of it is an effect. It is keyed by the
/// CLI's own `session_id` and dropped at `SessionEnd`.
#[derive(Debug, Default)]
struct Memo {
    /// `adapter:claude`'s internal session, opened once by the handler.
    session: Option<SessionId>,
    /// `session_id` → the agent its `agent.register` resolved.
    agents: HashMap<String, AgentId>,
    /// `session_id` → the card this adapter has on the deck that the CLI's
    /// own turn is allowed to take off it: a live **permission** card (C8:
    /// first answer wins, so the `PostToolUse`/`Stop` that says somebody
    /// answered in the terminal clears it) and any card a release left
    /// behind degraded (§5.6: it points at the dialog until the turn moves
    /// on).
    ///
    /// A **live question** card is deliberately not here. C8's other half:
    /// the group owns its dialog for the whole hold, and Claude runs tools
    /// in parallel — a `PostToolUse` from some other tool would otherwise
    /// clear a card the operator is in the middle of answering, and
    /// clearing it releases the group.
    open: HashMap<String, CardId>,
    /// `session_id` → the card of this session that is **parked on a hold
    /// right now**, whatever its origin: a `PermissionRequest`, or the
    /// question of an `AskUserQuestion` group the operator is part way
    /// through.
    ///
    /// It is a guard, never a handle: nothing clears a card through it.
    /// That is the difference from `open`, and why a live question card
    /// may be here while it must not be there. What it guards is the one
    /// way this adapter can hurt the CLI on its own: `agent.card.set`
    /// **retires** the agent's current card, and retiring a parked hold
    /// emits `ReleaseHold{Expiry}` (`cards.rs:314`) — so a card pushed
    /// while a hold is parked answers that CLI's still-open hook with
    /// `{}` and takes the operator's half-answered group off the deck.
    parked: HashMap<String, CardId>,
    /// `session_id` → the Ready card the last `Stop` put up, expired by
    /// the next `UserPromptSubmit` (§5.1).
    ready: HashMap<String, CardId>,
    /// C5's 5 s window: the dedupe key → when it was first seen.
    seen: HashMap<String, Instant>,
    /// C5's 30 s window: `session_id` → when its group was released.
    released: HashMap<String, Instant>,
    /// `session_id` → the last hook this adapter saw from it.
    ///
    /// Every other map above is keyed by `session_id` and, without this,
    /// emptied only by `SessionEnd` — which a `SIGKILL`ed CLI never sends.
    /// This is what makes them windows too (`MEMO_IDLE`): one timestamp
    /// per session rather than one per entry, because they all live and
    /// die with the session.
    touched: HashMap<String, Instant>,
    /// `session_id` → the `session_name` the status line last reported,
    /// so a line that repeats it (every 300 ms while Claude works) is not
    /// another `agent.update`.
    names: HashMap<String, String>,
}

/// One `AskUserQuestion` call, normalised: a card per question, and per
/// question the key its answer is filed under together with the option
/// labels the CLI actually sent (before §5.6 cut them to the deck's line
/// budget).
#[derive(Debug)]
struct Questions {
    cards: Vec<CardSet>,
    questions: Vec<GroupQuestion>,
}

/// The Claude Code normaliser. Cheap to share: the doors call it from
/// every hook task at once.
#[derive(Debug, Default)]
pub struct Claude {
    memo: Mutex<Memo>,
}

/// The tab stamp of a CLI session: `deck:<4 hex>`, derived from the
/// session id so the beacon a test reads is the beacon the daemon wrote.
///
/// C26 resolves the terminal by finding this string in a Ghostty title and
/// then **pins** the tab; the stamp itself is one-shot (C25), so it only
/// has to be unique among the sessions alive at once, which a per-session
/// digest is.
#[must_use]
pub fn stamp(session_id: &str) -> String {
    let digest = blake3::hash(session_id.as_bytes());
    let bytes = digest.as_bytes();
    format!("deck:{:02x}{:02x}", bytes[0], bytes[1])
}

/// The complete OSC 2 sequence `ESC ] 2 ; <stamp> BEL`.
///
/// Claude's allowlist takes OSC 0/1/2/9/99/777 terminated by BEL or ST and
/// **silently drops** the whole field for anything else, so this is built
/// in one place and asserted byte for byte.
#[must_use]
pub fn terminal_sequence(session_id: &str) -> String {
    format!("\u{1b}]2;{}\u{7}", stamp(session_id))
}

/// `{"hookSpecificOutput":{"hookEventName":<event>, …}}` — C2: the name is
/// always the event that fired, never a constant.
fn specific(event: &str, mut fields: serde_json::Map<String, serde_json::Value>) -> HookOutput {
    fields.insert("hookEventName".into(), event.into());
    HookOutput::Json(serde_json::json!({
        "hookSpecificOutput": serde_json::Value::Object(fields),
    }))
}

fn fields(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// How much of a `session_id` names an agent that has no `cwd` to be named
/// after, in **bytes** (§5.6 counts bytes because the device's line budget
/// does).
pub const ID_IN_NAME: usize = 6;

/// The last path component of a cwd, as the agent's name (§5.1: the
/// statusLine's `session_name` takes over when it reports one).
///
/// Both halves are payload-derived and neither may be cut with a byte
/// slice: a `session_id` is whatever the CLI put in the JSON, and
/// `&id[..6]` panics the moment byte 6 lands inside a multi-byte character
/// — which is a panic **inside the hook handler**, so the CLI reads an
/// empty reply from a door that promised it `{}` (contract §9; for Codex an
/// empty reply is an exit-2 deny). `sanitize::cut` is §5.6's own cut and
/// lands on a character boundary, so that is what does it here too, rather
/// than a second cutter written beside it.
pub fn name_of(cwd: Option<&str>, session_id: &str) -> String {
    let base = cwd
        .map(|c| c.trim_end_matches('/'))
        .and_then(|c| c.rsplit('/').find(|p| !p.trim().is_empty()))
        .map(sanitize::label)
        .filter(|n| !n.trim().is_empty());
    base.unwrap_or_else(|| {
        format!(
            "claude-{}",
            sanitize::cut(&sanitize::collapse(session_id), ID_IN_NAME)
        )
    })
}

/// A short, human summary of a tool call for the card's first line.
///
/// Bash → the command, the file tools → the path, MCP → `server/tool`,
/// anything else → whatever string the input leads with. `sanitize::text`
/// then strips ANSI, folds a code fence and cuts to 128 B on a character
/// boundary (§5.6).
#[must_use]
pub fn tool_summary(tool: &str, input: &serde_json::Value) -> String {
    let pick = |key: &str| {
        input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    let head = match tool {
        "Bash" | "BashOutput" | "KillShell" => pick("command").or_else(|| pick("description")),
        "Read" | "Write" | "Edit" | "MultiEdit" => pick("file_path"),
        "NotebookEdit" => pick("notebook_path").or_else(|| pick("file_path")),
        "Glob" | "Grep" => pick("pattern"),
        "WebFetch" => pick("url"),
        "WebSearch" => pick("query"),
        "Task" => pick("description"),
        _ => None,
    };
    let head = head.or_else(|| {
        input
            .as_object()
            .and_then(|o| o.values().find_map(serde_json::Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    });
    // `mcp__<server>__<tool>` reads as `server/tool` (§5.1).
    let label = match tool.strip_prefix("mcp__") {
        Some(rest) => match rest.split_once("__") {
            Some((server, name)) => format!("{server}/{name}"),
            None => rest.to_string(),
        },
        None => tool.to_string(),
    };
    match head {
        Some(head) => sanitize::text(&format!("{label}: {head}")),
        None => sanitize::text(&label),
    }
}

/// C5's key: the payload carries no `tool_use_id`, so two asks are "the
/// same ask" when the session, the tool and the input all match. The
/// input is hashed rather than kept — it is the most sensitive field on
/// the wire and the memo outlives the request (§11).
fn dedupe_key(session_id: &str, tool: &str, input: &serde_json::Value) -> String {
    let digest = blake3::hash(input.to_string().as_bytes());
    format!("{session_id}\u{0}{tool}\u{0}{}", digest.to_hex())
}

impl Claude {
    /// The agent a payload's session resolved to last time, if any.
    #[must_use]
    pub fn agent(&self, payload: &HookPayload) -> Option<AgentId> {
        let session = payload.str("session_id")?;
        self.memo.lock().ok()?.agents.get(session).cloned()
    }

    /// Drop every window and card this CLI session owned.
    fn forget(&self, session_id: &str) {
        if let Ok(mut memo) = self.memo.lock() {
            memo.agents.remove(session_id);
            memo.open.remove(session_id);
            memo.parked.remove(session_id);
            memo.ready.remove(session_id);
            memo.released.remove(session_id);
            memo.touched.remove(session_id);
            memo.names.remove(session_id);
            memo.seen
                .retain(|key, _| !key.starts_with(&format!("{session_id}\u{0}")));
        }
    }

    /// Drop the windows whose time is up, so the memo is bounded by the
    /// sessions that are actually live.
    ///
    /// Three windows, not two: the dedupe key's 5 s (C5), the released
    /// group's 30 s (C5), and the session's own `MEMO_IDLE`. The third is
    /// what keeps `agents`/`open`/`parked`/`ready` bounded, because the
    /// only other thing that empties them is `SessionEnd` and a CLI that
    /// was killed, crashed or lost its machine never sends one.
    fn prune(&self, now: Instant) {
        let Ok(mut memo) = self.memo.lock() else {
            return;
        };
        memo.seen
            .retain(|_, at| now.saturating_duration_since(*at) < DEDUPE);
        memo.released
            .retain(|_, at| now.saturating_duration_since(*at) < RELEASED);
        let stale: Vec<String> = memo
            .touched
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) >= MEMO_IDLE)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            memo.agents.remove(&id);
            memo.open.remove(&id);
            memo.parked.remove(&id);
            memo.ready.remove(&id);
            memo.released.remove(&id);
            memo.touched.remove(&id);
            memo.names.remove(&id);
            let prefix = format!("{id}\u{0}");
            memo.seen.retain(|key, _| !key.starts_with(&prefix));
        }
    }

    /// Mark this session as heard from, which is what `MEMO_IDLE` ages.
    ///
    /// Called from `ingest` before anything else touches the memo, so a
    /// session id can only reach the other maps behind a timestamp: there
    /// is no entry the sweep above cannot see the age of.
    fn touch(&self, session_id: &str, now: Instant) {
        if let Ok(mut memo) = self.memo.lock() {
            memo.touched.insert(session_id.to_string(), now);
        }
    }

    /// C5: is this `PermissionRequest` one we have already raised, or the
    /// follow-up of a group that was released moments ago?
    fn suppressed(&self, session_id: &str, key: &str, now: Instant) -> bool {
        let Ok(mut memo) = self.memo.lock() else {
            // A poisoned memo must never make a hook wait: raise the card.
            return false;
        };
        if memo
            .released
            .get(session_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < RELEASED)
        {
            return true;
        }
        match memo.seen.get(key) {
            Some(at) if now.saturating_duration_since(*at) < DEDUPE => true,
            _ => {
                memo.seen.insert(key.to_string(), now);
                false
            }
        }
    }

    /// The `agent.register` every plan leads with: idempotent by `key`
    /// from this adapter's own session, so it is how a payload names its
    /// agent as well as how the session joins the roster (§4.6).
    fn register(payload: &HookPayload, ctx: &HookCtx, session_id: &str) -> AgentRegister {
        let cwd = payload.str("cwd");
        // Only an event that ran through the helper carries its environment
        // (a command hook; an http hook has none, C1), so only such an event
        // may write the terminal — an http event would otherwise blank what
        // the helper learned. With the plugin every event is a command
        // hook, which is what lets a session that started before the daemon
        // get its terminal and inbox on its *next* hook rather than never.
        let terminal = Self::terminal(payload, ctx, session_id);
        AgentRegister {
            kind: Kind::Claude,
            name: name_of(cwd, session_id),
            key: Some(format!("claude:{session_id}")),
            cwd: cwd.map(str::to_string),
            pid: ctx.cli_pid,
            pid_kind: ctx.pid_kind,
            terminal,
            // An adapter's agents live by hook events, never by a lease
            // (§4.6): `SessionEnd` and the liveness poll remove them.
            lease_ttl_s: 0,
            caps: Caps {
                answer: AnswerCap::Hold,
                order: OrderCap::Inject,
                focus: true,
                transcribe: Default::default(),
            },
            slot_hint: None,
            voice: None,
            meta: None,
        }
    }

    fn terminal(payload: &HookPayload, ctx: &HookCtx, session_id: &str) -> Option<Terminal> {
        // C25: the beacon is one shot, on the event whose answer carries it.
        let stamped = payload.event() == "SessionStart";
        // An http hook ran no helper: nothing to learn, and `None` leaves
        // what an earlier command hook recorded untouched (`roster.rs`
        // writes the terminal only when the register carries one). The
        // stamp is not environment-derived, so `SessionStart` always
        // writes one.
        if !stamped && ctx.env.is_empty() && ctx.tty.is_none() {
            return None;
        }
        let terminal = Terminal {
            program: ctx.env("TERM_PROGRAM"),
            tty: ctx.tty.clone(),
            tmux_pane: ctx.env("TMUX_PANE"),
            // C27: a Ghostty surface carries no session identifier, so
            // these two stay empty there and are captured for iTerm and
            // Apple Terminal only.
            iterm_session: ctx.env("ITERM_SESSION_ID"),
            term_session: ctx.env("TERM_SESSION_ID"),
            title_stamp: stamped.then(|| stamp(session_id)),
            // The session's message inbox (`crate::inbox`): the one field
            // a Desktop-app session has, since it has no terminal at all.
            inbox: ctx.env("CLAUDE_CODE_MESSAGING_SOCKET"),
        };
        (terminal != Terminal::default()).then_some(terminal)
    }

    /// `agent.update` for this plan's agent.
    fn update(state: Option<AgentState>, title: Option<(String, TitleSource)>) -> Request {
        let (title, title_source) = match title {
            Some((text, source)) => (Some(text), Some(source)),
            None => (None, None),
        };
        Request::AgentUpdate(AgentUpdate {
            agent: pending(),
            state,
            title,
            title_source,
            since: None,
            name: None,
            meta: None,
        })
    }

    /// The card a `PermissionRequest` becomes (§5.1, §5.6).
    fn permission_card(
        payload: &HookPayload,
        cfg: HookCfg,
        tool: &str,
        input: &serde_json::Value,
    ) -> CardSet {
        let suggestions = payload
            .get("permission_suggestions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|s| !s.is_empty());
        // "Allow always" is offered only when Claude handed us a rule to
        // persist: without `permission_suggestions` there is nothing to
        // put in `updatedPermissions` and the row would be a lie.
        let labels: Vec<&str> = if suggestions {
            vec![ALLOW, ALLOW_ALWAYS, DENY]
        } else {
            vec![ALLOW, DENY]
        };
        CardSet {
            agent: pending(),
            id: None,
            kind: CardKind::Q,
            text: tool_summary(tool, input),
            choices: labels
                .into_iter()
                .map(|l| Choice {
                    label: l.to_string(),
                    detail: None,
                })
                .collect(),
            rec: Some(0),
            free_text: true,
            deliver: Deliver::Hold,
            timeout_s: Some(cfg.hold_s),
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some(tool.to_string()),
                request_id: None,
                group: None,
            },
        }
    }

    /// An informational card whose ENTER focuses the terminal (§5.6): the
    /// dialog is already on screen there and the deck only points at it.
    fn focus_card(text: &str, tool: Option<&str>) -> CardSet {
        CardSet {
            agent: pending(),
            id: None,
            kind: CardKind::Q,
            text: sanitize::text(text),
            choices: vec![Choice {
                label: FOCUS.to_string(),
                detail: None,
            }],
            rec: Some(0),
            free_text: false,
            deliver: Deliver::Focus,
            timeout_s: None,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: tool.map(str::to_string),
                request_id: None,
                group: None,
            },
        }
    }

    /// The cards of one `AskUserQuestion` call: one per question, in
    /// order, each with the group's index so the store inherits the one
    /// budget across them (facts-seams §2e).
    ///
    /// The **original** option labels come back beside the cards. What
    /// goes onto a card is `sanitize::labels`' output — 32 bytes, `…`,
    /// `#n` on a collision (§5.6) — and that is a *deck* budget: Claude
    /// will only accept one of the strings it sent us, so the answer has
    /// to be mapped back through these (`GroupPlan::option`).
    fn question_cards(
        input: &serde_json::Value,
        request_id: Option<&str>,
        cfg: HookCfg,
    ) -> Option<Questions> {
        let questions = input.get("questions")?.as_array()?;
        // C7: 1–4 questions, 2–4 options, both schema-enforced by the
        // binary — a payload outside that is not an AskUserQuestion we
        // know how to answer, and answering it wrong loses the turn.
        if questions.is_empty() || questions.len() > 4 {
            return None;
        }
        let count = u8::try_from(questions.len()).ok()?;
        let mut cards = Vec::with_capacity(questions.len());
        let mut filed = Vec::with_capacity(questions.len());
        for (index, question) in questions.iter().enumerate() {
            let text = question.get("question")?.as_str()?;
            let header = question
                .get("header")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let options: Vec<String> = question
                .get("options")?
                .as_array()?
                .iter()
                .filter_map(|o| o.get("label").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect();
            if options.len() < 2 || options.len() > deck_model::MAX_CHOICES {
                return None;
            }
            let labels = sanitize::labels(&options);
            if labels.iter().any(|l| l.trim().is_empty()) {
                return None;
            }
            let title = if header.trim().is_empty() {
                text.to_string()
            } else {
                format!("{header}: {text}")
            };
            cards.push(CardSet {
                agent: pending(),
                id: None,
                kind: CardKind::Q,
                text: sanitize::text(&title),
                choices: labels
                    .into_iter()
                    .map(|label| Choice {
                        label,
                        detail: None,
                    })
                    .collect(),
                rec: None,
                free_text: true,
                deliver: Deliver::Hold,
                timeout_s: Some(cfg.hold_s),
                origin: Origin {
                    kind: OriginKind::Question,
                    tool: Some(ASK.to_string()),
                    request_id: request_id.map(str::to_string),
                    group: Some(Group {
                        index: u8::try_from(index).ok()?,
                        count,
                    }),
                },
            });
            filed.push(GroupQuestion {
                key: text.to_string(),
                options,
            });
        }
        Some(Questions {
            cards,
            questions: filed,
        })
    }

    /// Whether this session already has a card of ours on the deck that a
    /// new one would take off it.
    ///
    /// Two kinds count. A **parked** card is a hold the CLI is waiting on
    /// right now, and replacing it releases that hold with `{}` — the
    /// operator's dialog answers itself while they are still reading it.
    /// An **open** card is a live permission card or the degraded
    /// `⌨ Focus terminal` a release left behind; replacing that one loses
    /// nothing the store cannot rebuild, but it is a second card saying
    /// what the first already says, with a chime of its own.
    ///
    /// Every `Notification` that would raise a card asks this first.
    fn carded_already(&self, session_id: &str) -> bool {
        self.memo
            .lock()
            .is_ok_and(|m| m.parked.contains_key(session_id) || m.open.contains_key(session_id))
    }

    /// Whether this session has a card **parked on a hold right now**.
    ///
    /// The guard on every held event, and the narrower half of
    /// [`Self::carded_already`]: a degraded card in `open` is a card the
    /// next ask may legitimately replace, a parked one never is.
    ///
    /// Claude fires these concurrently. A `PermissionRequest` for one tool
    /// arrives while an `AskUserQuestion` group is half answered, or a
    /// second permission ask lands beside the first; each would park its
    /// own card, and `agent.card.set` **retires** the agent's current card
    /// — releasing the parked hold with `Release::Expiry` and pulling the
    /// card off the deck. The operator would watch the question they were
    /// answering vanish, the still-open hook behind it would be answered
    /// `{}`, and for a group every answer already given would be lost
    /// (`updatedInput.answers` is written only after the last card, so
    /// `group_to_output` is never reached). One held card per CLI session
    /// is the rule; the second event answers `{}` and Claude shows its own
    /// dialog, which is what it would have done anyway.
    fn parked_already(&self, session_id: &str) -> bool {
        self.memo
            .lock()
            .is_ok_and(|m| m.parked.contains_key(session_id))
    }

    /// `Notification` (§5.1, C10): the five types we act on, everything
    /// else ignored.
    fn notification(&self, payload: &HookPayload, session_id: &str, plan: Vec<Request>) -> Ingest {
        let kind = payload.str("notification_type").unwrap_or_default();
        let message = payload.str("message").unwrap_or("needs your attention");
        let mut plan = plan;
        match kind {
            "idle_prompt" => {
                plan.push(Self::update(Some(AgentState::Idle), None));
                Ingest::Report(plan)
            }
            "permission_prompt" | "worker_permission_prompt" => {
                // It fires ~6 s after the ask, long after our own
                // `PermissionRequest` card: raising a second one would put
                // a duplicate on top of the live card — and if that card
                // is still parked, replacing it answers the hook the CLI
                // is blocked on.
                if self.carded_already(session_id) {
                    return Ingest::Nothing;
                }
                plan.push(Request::CardSet(Self::focus_card(message, None)));
                Ingest::Report(plan)
            }
            "agent_needs_input" | "elicitation_dialog" | "elicitation_url_dialog" => {
                // The state is true either way and costs no card, so it is
                // reported whatever else is on the deck. The card is not:
                // these three fire while a tool call is in flight, which is
                // exactly when a `PermissionRequest` or an `AskUserQuestion`
                // group of ours is parked, and pushing a focus card there
                // releases that hold with `{}` and clobbers the card the
                // operator is answering.
                plan.push(Self::update(Some(AgentState::Question), None));
                if !self.carded_already(session_id) {
                    plan.push(Request::CardSet(Self::focus_card(message, None)));
                }
                Ingest::Report(plan)
            }
            "agent_completed" => {
                plan.push(Self::update(Some(AgentState::Ready), None));
                Ingest::Report(plan)
            }
            // C10: 2.1.276 ships 17 types where the docs list 12, and the
            // five undocumented ones have never been observed. An unknown
            // type is not an error — it is a type this build does not know
            // yet, and a hook that panicked on one would take the daemon
            // down on a Claude upgrade.
            _ => Ingest::Nothing,
        }
    }

    /// A status-line payload: the fields Claude writes for its status
    /// line and for no hook.
    fn is_statusline(payload: &HookPayload) -> bool {
        ["rate_limits", "workspace", "context_window", "model"]
            .iter()
            .any(|field| payload.get(field).is_some())
    }

    /// The status line as the session's report: the register (a session
    /// that started before the daemon gets onto the roster from its first
    /// line, hooks or no hooks), and the session's name as its title the
    /// first time the line carries one or when it changes (`/rename`). A
    /// line every 300 ms must not be a plan every 300 ms, so a session
    /// already resolved whose name has not moved is nothing. The
    /// `rate_limits` are the door's (`api::hooks`), read before this.
    fn statusline(&self, payload: &HookPayload, session_id: &str, mut plan: Vec<Request>) -> Ingest {
        let name = payload
            .str("session_name")
            .map(sanitize::text)
            .filter(|name| !name.is_empty());
        let Ok(mut memo) = self.memo.lock() else {
            return Ingest::Nothing;
        };
        let resolved = memo.agents.contains_key(session_id);
        if resolved && memo.names.get(session_id) == name.as_ref() {
            return Ingest::Nothing;
        }
        match &name {
            Some(name) => {
                memo.names.insert(session_id.to_string(), name.clone());
            }
            None => {
                memo.names.remove(session_id);
            }
        }
        drop(memo);
        if let Some(name) = name {
            plan.push(Self::update(
                None,
                Some((name, TitleSource::SessionName)),
            ));
        }
        Ingest::Report(plan)
    }
}

impl Adapter for Claude {
    fn principal(&self) -> Principal {
        Principal::Adapter(AdapterKind::Claude)
    }

    fn client(&self) -> &'static str {
        "adapter:claude"
    }

    fn session(&self) -> Option<SessionId> {
        self.memo.lock().ok().and_then(|m| m.session)
    }

    fn set_session(&self, session: SessionId) -> SessionId {
        match self.memo.lock() {
            Ok(mut memo) => *memo.session.get_or_insert(session),
            Err(_) => session,
        }
    }

    fn ingest(&self, payload: &HookPayload, ctx: &HookCtx) -> Ingest {
        let now = ctx.now();
        self.prune(now);
        let Some(session_id) = payload.str("session_id") else {
            return Ingest::Nothing;
        };
        self.touch(session_id, now);
        let event = payload.event();
        let cfg = ctx.cfg;
        // Every plan leads with the register: it resolves `session_id` to
        // the agent id the rest of the plan is addressed to.
        let lead = || {
            vec![Request::AgentRegister(Self::register(
                payload, ctx, session_id,
            ))]
        };
        match event {
            "SessionStart" => {
                let mut plan = lead();
                if let Some(title) = payload.str("session_title") {
                    plan.push(Self::update(
                        Some(AgentState::Idle),
                        Some((sanitize::text(title), TitleSource::SessionName)),
                    ));
                } else {
                    plan.push(Self::update(Some(AgentState::Idle), None));
                }
                // C25: one shot. The tab's title is rewritten by Claude
                // itself and by the shell prompt, so re-emitting this on
                // every turn (as §5.1 planned) would only cost a field on
                // every response; deckd pins the tab instead (C26).
                Ingest::Beacon(
                    HookOutput::Json(serde_json::json!({
                        "hookSpecificOutput": {"hookEventName": "SessionStart"},
                        "terminalSequence": terminal_sequence(session_id),
                    })),
                    plan,
                )
            }
            "UserPromptSubmit" => {
                let mut plan = lead();
                let title = payload.str("prompt").map(|p| {
                    (
                        sanitize::cut(&sanitize::collapse(p), TITLE_MAX),
                        TitleSource::Prompt,
                    )
                });
                plan.push(Self::update(Some(AgentState::Working), title));
                // The turn that just started expires the Ready card the
                // last one left behind (§5.1).
                if let Some(card) = self
                    .memo
                    .lock()
                    .ok()
                    .and_then(|mut m| m.ready.remove(session_id))
                {
                    plan.push(Request::CardClear(CardClear {
                        agent: pending(),
                        card,
                        delivered: false,
                        reason: Some("prompt".into()),
                    }));
                }
                Ingest::Report(plan)
            }
            "PermissionRequest" => {
                // One hold per session (`parked_already`): a second card
                // would take the first off the deck mid-answer.
                if self.parked_already(session_id) {
                    return Ingest::Nothing;
                }
                let tool = payload.str("tool_name").unwrap_or_default();
                let input = payload
                    .get("tool_input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                // C5, half one: the question group's own follow-up. S2
                // measured it 3 ms behind a released `PreToolUse`, with
                // the dialog 7 ms behind that — a card here lands on top
                // of the degraded group card.
                if tool == ASK {
                    return Ingest::Nothing;
                }
                // C5, half two: no `tool_use_id` to dedupe on.
                if self.suppressed(session_id, &dedupe_key(session_id, tool, &input), now) {
                    return Ingest::Nothing;
                }
                Ingest::Held {
                    before: lead(),
                    cmd: Self::permission_card(payload, cfg, tool, &input),
                    note: serde_json::json!({
                        "event": "PermissionRequest",
                        "suggestions": payload.get("permission_suggestions").cloned(),
                    }),
                    group: None,
                }
            }
            "PreToolUse" => {
                // Installed with matcher `AskUserQuestion` only: any other
                // tool here is somebody else's hook reaching our door.
                if payload.str("tool_name") != Some(ASK) {
                    return Ingest::Nothing;
                }
                // One hold per session (`parked_already`): a group raised
                // on top of a parked card releases that card's hook.
                if self.parked_already(session_id) {
                    return Ingest::Nothing;
                }
                let Some(input) = payload.get("tool_input") else {
                    return Ingest::Nothing;
                };
                let Some(Questions {
                    mut cards,
                    questions,
                }) = Self::question_cards(input, payload.str("tool_use_id"), cfg)
                else {
                    return Ingest::Nothing;
                };
                let cmd = cards.remove(0);
                Ingest::Held {
                    before: lead(),
                    cmd,
                    note: serde_json::json!({
                        "event": "PreToolUse",
                        "input": input.clone(),
                    }),
                    group: Some(GroupPlan {
                        rest: cards,
                        questions,
                    }),
                }
            }
            "PostToolUse" => {
                let mut plan = lead();
                // C8: the tool ran, so somebody answered — in the terminal
                // if it was not us. First answer wins: the card goes.
                if let Some(card) = self
                    .memo
                    .lock()
                    .ok()
                    .and_then(|mut m| m.open.remove(session_id))
                {
                    plan.push(Request::CardClear(CardClear {
                        agent: pending(),
                        card,
                        delivered: false,
                        reason: Some("tool_ran".into()),
                    }));
                }
                if cfg.tool_titles {
                    let tool = payload.str("tool_name").unwrap_or_default();
                    let input = payload
                        .get("tool_input")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    plan.push(Self::update(
                        Some(AgentState::Working),
                        Some((tool_summary(tool, &input), TitleSource::Tool)),
                    ));
                }
                Ingest::Report(plan)
            }
            "Stop" => {
                let mut plan = lead();
                plan.push(Self::update(Some(AgentState::Ready), None));
                if let Some(card) = self
                    .memo
                    .lock()
                    .ok()
                    .and_then(|mut m| m.open.remove(session_id))
                {
                    plan.push(Request::CardClear(CardClear {
                        agent: pending(),
                        card,
                        delivered: false,
                        reason: Some("turn_ended".into()),
                    }));
                }
                // `last_assistant_message` is the documented source; the
                // transcript lags the turn and is never read here.
                if let Some(raw) = payload.str("last_assistant_message") {
                    // Two different budgets on one string. The **reply**
                    // is kept whole (§4.3: the store holds all of it, the
                    // GUI shows all of it, and the device's own 200 bytes
                    // are applied by the wire encoder, `Caps::reply`);
                    // §5.6's 128 bytes are a **card text** rule and belong
                    // only to the Ready card beside it, which has to fit a
                    // deck line. Cutting the reply to 128 threw away most
                    // of what the model said before anything could show
                    // it. The Codex adapter already keeps its reply whole,
                    // so this also makes the two agree.
                    let reply = sanitize::clean(raw);
                    let text = sanitize::cut(&reply, sanitize::TEXT_MAX);
                    if !text.trim().is_empty() {
                        plan.push(Request::Reply(ReplySet {
                            message: None,
                            agent: pending(),
                            text: reply,
                            speak: false,
                            announce: None,
                            lane: None,
                        }));
                        // A Ready card with no steps: no CLI provides them.
                        plan.push(Request::CardSet(CardSet {
                            agent: pending(),
                            id: None,
                            kind: CardKind::R,
                            text,
                            choices: Vec::new(),
                            rec: None,
                            free_text: false,
                            deliver: Deliver::None,
                            timeout_s: None,
                            origin: Origin {
                                kind: OriginKind::Ready,
                                tool: None,
                                request_id: None,
                                group: None,
                            },
                        }));
                    }
                }
                Ingest::Report(plan)
            }
            "StopFailure" => {
                let mut plan = lead();
                let error = payload.str("error").unwrap_or("unknown");
                plan.push(Self::update(
                    Some(AgentState::Error),
                    Some((sanitize::text(error), TitleSource::Api)),
                ));
                // The title above is a card-shaped field and keeps §5.6's
                // 128 bytes; the reply below is the whole of what the API
                // said, for the same reason as `Stop`'s — a 429's detail
                // is the one thing that says when to try again, and it
                // does not fit in 128 bytes.
                if let Some(raw) = payload
                    .str("error_details")
                    .or_else(|| payload.str("last_assistant_message"))
                {
                    let text = sanitize::clean(raw);
                    if !text.trim().is_empty() {
                        plan.push(Request::Reply(ReplySet {
                            message: None,
                            agent: pending(),
                            text,
                            speak: false,
                            announce: None,
                            lane: None,
                        }));
                    }
                }
                Ingest::Report(plan)
            }
            "Notification" => self.notification(payload, session_id, lead()),
            "SessionEnd" => {
                // No register: a session we never saw has no agent to
                // release, and registering one here only to remove it
                // would put an `agent.added` on the bus for a CLI that is
                // already gone.
                let Some(agent) = self.agent(payload) else {
                    self.forget(session_id);
                    return Ingest::Nothing;
                };
                self.forget(session_id);
                Ingest::Report(vec![Request::AgentRelease {
                    agent,
                    reason: payload.str("reason").map(str::to_string),
                }])
            }
            // The chained status line (`hooks.claude_statusline`) is not a
            // hook: Claude runs it after every API response with the
            // session's state and no `hook_event_name` — `"Status"` in the
            // builds that set one.
            "Status" | "" if Self::is_statusline(payload) => {
                self.statusline(payload, session_id, lead())
            }
            _ => Ingest::Nothing,
        }
    }

    fn answer_to_output(
        &self,
        note: &serde_json::Value,
        cmd: &CardSet,
        ans: Option<&Answer>,
    ) -> HookOutput {
        let event = note["event"].as_str().unwrap_or_default();
        // A release is always `{}`. S1 measured the terminal dialog live
        // through the whole hold, so the operator is looking at it: a
        // decision written now would be one nobody asked for.
        let Some(ans) = ans else {
            return HookOutput::Empty;
        };
        if event != "PermissionRequest" {
            return HookOutput::Empty;
        }
        // The label as shown on the deck (§4.6). Claude can never exceed
        // `MAX_CHOICES` (C7), so the shown list is the list we sent and
        // the label is enough to name the row.
        let label = ans
            .label
            .as_deref()
            .or_else(|| {
                ans.choice
                    .and_then(|n| cmd.choices.get(usize::from(n)))
                    .map(|c| c.label.as_str())
            })
            .unwrap_or_default();
        let decision = match label {
            ALLOW => allow(None),
            ALLOW_ALWAYS => allow(note.get("suggestions").filter(|s| !s.is_null()).cloned()),
            DENY => deny(DENIED),
            // Free text from PTT or the GUI: the §5.6 intent grammar.
            _ => match ans.text.as_deref() {
                Some(text) => match intent(text) {
                    Intent::Allow => allow(None),
                    Intent::Deny => deny(DENIED),
                    // Anything else is an instruction, not a yes: deny and
                    // hand Claude the words so it can re-plan.
                    Intent::Say => deny(&sanitize::text(text)),
                },
                None => return HookOutput::Empty,
            },
        };
        specific(event, fields(&[("decision", decision)]))
    }

    fn group_to_output(
        &self,
        note: &serde_json::Value,
        plan: &GroupPlan,
        answers: &[Option<Answer>],
    ) -> HookOutput {
        // C2: the name is the event that fired, read back from the note
        // `ingest` wrote, never a constant somebody could copy into the
        // next adapter. Only `PreToolUse` produces a group today, and a
        // response naming any other event is one Claude throws away whole
        // — so it is `{}`, which at least leaves the dialog working.
        let event = note["event"].as_str().unwrap_or_default();
        if event != "PreToolUse" {
            return HookOutput::Empty;
        }
        if answers.len() != plan.questions.len() {
            return HookOutput::Empty;
        }
        let mut input = note["input"].clone();
        if !input.is_object() {
            return HookOutput::Empty;
        }
        let mut filed = serde_json::Map::new();
        for (k, (question, ans)) in plan.questions.iter().zip(answers).enumerate() {
            // One released card releases the group: a half-answered
            // `updatedInput` would consume the tool call with questions
            // the operator never saw.
            let Some(ans) = ans else {
                return HookOutput::Empty;
            };
            // C7: the value is a string — the operator's own words, or
            // **the option as Claude wrote it**. A multi-select is a
            // comma-joined string, never an array, and free text is
            // already in that form.
            //
            // `Answer.label` is what the deck *drew*, and §5.6 cut it to
            // 32 bytes (`…`, and `#n` when two cuts collide) before it
            // ever reached the store. Filing that here hands Claude an
            // answer matching none of the options the same response echoes
            // back in `questions`, so the index is mapped through the
            // originals instead and the drawn label is only the last
            // resort. Claude's cards are never truncated (C7 caps
            // `options` at 4), so the empty `choice_map` below is the
            // identity it stands for.
            let value = ans
                .text
                .clone()
                .or_else(|| {
                    ans.choice
                        .and_then(|n| plan.option(k, n, &[]))
                        .map(str::to_string)
                })
                .or_else(|| ans.label.clone());
            let Some(value) = value else {
                return HookOutput::Empty;
            };
            filed.insert(question.key.clone(), value.into());
        }
        // `updatedInput` replaces the whole tool input, so `questions` is
        // echoed verbatim and only `answers` is added (§2.3).
        input["answers"] = serde_json::Value::Object(filed);
        specific(
            event,
            fields(&[
                ("permissionDecision", "allow".into()),
                ("updatedInput", input),
            ]),
        )
    }

    // What the handler tells the adapter once the core has answered: the
    // agent a register resolved, the card a plan created and the release
    // of a group. All of it is a memo write, never an effect.
    /// The agent `session_id` resolved to (from `agent.register`).
    fn resolved(&self, payload: &HookPayload, agent: &AgentId) {
        if is_pending(agent) {
            return;
        }
        let Some(session_id) = payload.str("session_id") else {
            return;
        };
        if let Ok(mut memo) = self.memo.lock() {
            memo.agents.insert(session_id.to_string(), agent.clone());
        }
    }

    /// A card this payload's plan created. The permission card is the one
    /// C8 clears from `PostToolUse`/`Stop`; a Ready card is expired by the
    /// next prompt. A **held** card of either origin is also remembered as
    /// parked, which is what stops a `Notification` from replacing it
    /// while the CLI waits (`carded_already`). Core replies can resume their
    /// handlers in either order, so an older callback cannot replace a newer id.
    fn carded(&self, payload: &HookPayload, cmd: &CardSet, card: CardId) {
        let Some(session_id) = payload.str("session_id") else {
            return;
        };
        let Ok(mut memo) = self.memo.lock() else {
            return;
        };
        if cmd.deliver == Deliver::Hold {
            memo.parked
                .entry(session_id.to_string())
                .and_modify(|current| *current = (*current).max(card))
                .or_insert(card);
        }
        match (cmd.kind, cmd.origin.kind) {
            (CardKind::R, _) => {
                memo.ready
                    .entry(session_id.to_string())
                    .and_modify(|current| *current = (*current).max(card))
                    .or_insert(card);
            }
            (_, OriginKind::Permission) => {
                memo.open
                    .entry(session_id.to_string())
                    .and_modify(|current| *current = (*current).max(card))
                    .or_insert(card);
            }
            _ => {}
        }
    }

    /// The card is off the deck: answered, and `hold_written` cleared it.
    fn uncarded(&self, payload: &HookPayload, card: CardId) {
        let Some(session_id) = payload.str("session_id") else {
            return;
        };
        if let Ok(mut memo) = self.memo.lock() {
            if memo.open.get(session_id) == Some(&card) {
                memo.open.remove(session_id);
            }
            // The hold is resolved either way; a group's next card parks
            // itself in `carded` a moment later.
            if memo.parked.get(session_id) == Some(&card) {
                memo.parked.remove(session_id);
            }
        }
    }

    /// The hold ended with no decision. Two things follow.
    ///
    /// **The card, if there still is one.** A snooze, the hold budget, a
    /// dropped handler and an orphaned agent all leave the same card id on
    /// the deck, degraded to `⌨ Focus terminal` and pointing at the dialog
    /// Claude is still showing. It belongs to the turn now, not to the
    /// hold, so it becomes this session's open card and the next
    /// `PostToolUse`/`Stop` takes it off (§5.6). A release that **cleared**
    /// the card leaves nothing: filing its id would make `carded_already`
    /// permanently true for the session and silence every card-raising
    /// `Notification` until the next turn boundary or `MEMO_IDLE`. C8's own
    /// path is exactly that — `PostToolUse` clears the permission card, and
    /// clearing it is what releases the hold.
    ///
    /// And the claim is only ours to make while this card is still the one
    /// we parked, and only while nothing newer is open: two held events for
    /// one CLI session retire each other's cards, and the two handlers
    /// race. A `released` for a card a newer `carded` has already displaced
    /// must not overwrite it — C8's clear would then spend itself on a dead
    /// id and leave the live card up.
    ///
    /// **The window.** If it was a **question**, the `PermissionRequest`
    /// Claude fires 3 ms later is that group's own follow-up: C5's 30 s
    /// window starts here, whatever became of the card.
    fn released(
        &self,
        payload: &HookPayload,
        cmd: &CardSet,
        card: CardId,
        reason: Release,
        now: Instant,
    ) {
        let Some(session_id) = payload.str("session_id") else {
            return;
        };
        let Ok(mut memo) = self.memo.lock() else {
            return;
        };
        if memo.parked.get(session_id) == Some(&card) {
            // Nothing is parked any more: this hold is over either way.
            memo.parked.remove(session_id);
            // A release can also land after the turn moved on — the hold
            // budget expiring behind a `PostToolUse`, say — and claiming
            // `open` back for the older id would point the next clear at a
            // card that is already gone while the live one stayed on the
            // deck. Card ids only go up, so "newer" is decidable here.
            let newer = memo.open.get(session_id).is_some_and(|open| *open > card);
            if reason.degrades_card() && !newer {
                memo.open.insert(session_id.to_string(), card);
            }
        }
        if cmd.origin.kind == OriginKind::Question {
            memo.released.insert(session_id.to_string(), now);
        }
    }
}

fn allow(permissions: Option<serde_json::Value>) -> serde_json::Value {
    match permissions {
        Some(updated) => serde_json::json!({"behavior": "allow", "updatedPermissions": updated}),
        None => serde_json::json!({"behavior": "allow"}),
    }
}

fn deny(message: &str) -> serde_json::Value {
    serde_json::json!({"behavior": "deny", "message": message})
}

/// §5.6's grammar is `deck_model::voice`'s, not this adapter's (B13): this
/// file used to carry its own list with `yep|yeah|nope` in it while
/// `adapters::codex` carried one with `sure`, so the same spoken word
/// answered one CLI's permission card and became free text to the other.
pub use deck_model::voice::{intent, Intent};

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::AnswerSource;

    fn payload(value: serde_json::Value) -> HookPayload {
        HookPayload(value)
    }

    fn ctx(now: Instant) -> HookCtx {
        HookCtx {
            now: Some(now),
            cfg: HookCfg {
                hold_s: 120,
                tool_titles: false,
            },
            ..HookCtx::default()
        }
    }

    fn permission(tool: &str, input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "session_id": "abc123",
            "cwd": "/w/project",
            "hook_event_name": "PermissionRequest",
            "tool_name": tool,
            "tool_input": input,
        })
    }

    /// A one-question group's first card.
    fn question_card() -> CardSet {
        let input = serde_json::json!({"questions": [
            {"question": "Which colour?", "header": "Colour",
             "options": [{"label": "Red"}, {"label": "Green"}]}
        ]});
        Claude::question_cards(&input, None, HookCfg::default())
            .expect("one card")
            .cards
            .remove(0)
    }

    fn choice(label: &str) -> Answer {
        Answer {
            choice: Some(0),
            label: Some(label.to_string()),
            text: None,
            source: AnswerSource::Deck,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        }
    }

    /// An agent's name comes from two payload-derived strings and neither
    /// may be cut by byte index. `&session_id[..6]` panicked on the first
    /// multi-byte id that reached the fallback — inside the hook handler,
    /// so the CLI read an empty reply from the door (contract §9).
    #[test]
    fn a_name_is_cut_on_a_character_boundary_never_by_byte_index() {
        // The live repro: 5 ASCII bytes then a 2-byte character across the
        // cut, with no cwd to be named after.
        assert_eq!(name_of(None, "aaaaaé"), "claude-aaa…");
        // Every offset of a multi-byte id, and a 4-byte one.
        for id in ["é", "éé", "ééé", "éééé", "🙂", "🙂🙂", "日本語の"] {
            let name = name_of(None, id);
            assert!(name.starts_with("claude-"), "{id}: {name}");
            assert!(!name.trim().is_empty(), "{id}");
        }
        // An ASCII id still reads as it always did.
        assert_eq!(name_of(None, "abc123def"), "claude-abc…");
        assert_eq!(name_of(None, "abc12"), "claude-abc12");
        assert_eq!(name_of(None, ""), "claude-");
        // The cwd wins when it names anything at all, and a cwd that names
        // only whitespace or separators does not (the roster refuses a
        // blank name with `-32009 invalid{name, empty}`).
        assert_eq!(name_of(Some("/w/project/"), "abc123"), "project");
        assert_eq!(name_of(Some("///"), "abc123"), "claude-abc123");
        assert_eq!(name_of(Some("/   /"), "abc123"), "claude-abc123");
        // A hostile cwd is bounded by §5.6's label rule rather than
        // reaching the roster whole.
        let huge = format!("/w/{}", "p".repeat(10_000));
        assert!(name_of(Some(&huge), "abc123").len() <= deck_model::sanitize::LABEL_MAX);
    }

    /// C2: the event name of every response is the event that fired.
    /// Claude throws away a whole response whose name does not match, so
    /// a constant here would silently stop every decision the deck makes.
    /// The status line (`hooks.claude_statusline`) registers the session
    /// from its first line and retitles it when its name changes, and a
    /// line that repeats what the adapter already told the core is
    /// nothing: Claude writes one every 300 ms while it works.
    #[test]
    fn the_status_line_registers_once_and_retitles_on_rename() {
        let claude = Claude::default();
        let now = Instant::now();
        let line = |name: Option<&str>| {
            let mut v = serde_json::json!({
                "session_id": "abc123",
                "cwd": "/w/project",
                "model": {"id": "claude-opus-4-1", "display_name": "Opus"},
                "workspace": {"current_dir": "/w/project", "project_dir": "/w/project"},
                "rate_limits": {"five_hour": {"used_percentage": 1, "resets_at": 1}}
            });
            if let Some(name) = name {
                v["session_name"] = name.into();
            }
            payload(v)
        };
        // First sight: the register alone, until the core has resolved it.
        let Ingest::Report(plan) = claude.ingest(&line(None), &ctx(now)) else {
            panic!("the first line registers")
        };
        assert_eq!(plan.len(), 1);
        assert!(
            matches!(&plan[0], Request::AgentRegister(r) if r.key.as_deref() == Some("claude:abc123"))
        );
        assert!(matches!(claude.ingest(&line(None), &ctx(now)), Ingest::Report(_)));
        claude.resolved(&line(None), &AgentId("ag_1".into()));
        assert!(matches!(claude.ingest(&line(None), &ctx(now)), Ingest::Nothing));
        // A name: the register and the title, once.
        let Ingest::Report(plan) = claude.ingest(&line(Some("Deploy")), &ctx(now)) else {
            panic!("a new name retitles")
        };
        assert_eq!(plan.len(), 2);
        assert!(matches!(
            &plan[1],
            Request::AgentUpdate(u)
                if u.title.as_deref() == Some("Deploy")
                    && u.title_source == Some(TitleSource::SessionName)
                    && u.state.is_none()
        ));
        assert!(matches!(claude.ingest(&line(Some("Deploy")), &ctx(now)), Ingest::Nothing));
        // `/rename`: the new name goes out.
        let Ingest::Report(plan) = claude.ingest(&line(Some("Ship it")), &ctx(now)) else {
            panic!("a rename retitles")
        };
        assert!(matches!(&plan[1], Request::AgentUpdate(u) if u.title.as_deref() == Some("Ship it")));
        // A `Status` event name, as newer builds write it, is the same line.
        let mut status = line(Some("Ship it")).0;
        status["hook_event_name"] = "Status".into();
        assert!(matches!(claude.ingest(&HookPayload(status), &ctx(now)), Ingest::Nothing));
        // A hook payload that merely has no event name is not a status line.
        let bare = payload(serde_json::json!({"session_id": "abc123", "cwd": "/w/project"}));
        assert!(matches!(claude.ingest(&bare, &ctx(now)), Ingest::Nothing));
    }

    #[test]
    fn every_response_carries_the_event_that_fired() {
        let claude = Claude::default();
        let cmd = Claude::permission_card(
            &payload(permission("Bash", serde_json::json!({"command": "ls"}))),
            HookCfg::default(),
            "Bash",
            &serde_json::json!({"command": "ls"}),
        );
        let note = serde_json::json!({"event": "PermissionRequest", "suggestions": null});
        let out = claude
            .answer_to_output(&note, &cmd, Some(&choice(ALLOW)))
            .json();
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        let plan = GroupPlan {
            rest: Vec::new(),
            questions: vec![GroupQuestion {
                key: "Which colour do you prefer?".into(),
                options: vec!["Green".into(), "Blue".into()],
            }],
        };
        let note = serde_json::json!({
            "event": "PreToolUse",
            "input": {"questions": [{"question": "Which colour do you prefer?"}]},
        });
        let out = claude
            .group_to_output(&note, &plan, &[Some(choice("Green"))])
            .json();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        // Neither path will name an event that did not fire. A response
        // Claude throws away is worse than no response: the decision is
        // lost *and* the operator thinks the deck answered.
        let foreign = serde_json::json!({
            "event": "PostToolUse",
            "input": {"questions": []},
            "suggestions": null,
        });
        assert!(claude
            .group_to_output(&foreign, &plan, &[Some(choice("Green"))])
            .is_empty());
        let cmd = Claude::permission_card(
            &payload(permission("Bash", serde_json::json!({"command": "ls"}))),
            HookCfg::default(),
            "Bash",
            &serde_json::json!({"command": "ls"}),
        );
        assert!(claude
            .answer_to_output(&foreign, &cmd, Some(&choice(ALLOW)))
            .is_empty());
    }

    /// C5: the same ask twice inside 5 s is one card; past the window it
    /// is a new one, and a different input is never the same ask.
    #[test]
    fn identical_permission_requests_inside_the_window_raise_one_card() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(permission("Bash", serde_json::json!({"command": "ls"})));
        assert!(matches!(claude.ingest(&ask, &ctx(t0)), Ingest::Held { .. }));
        assert!(matches!(
            claude.ingest(&ask, &ctx(t0 + Duration::from_secs(4))),
            Ingest::Nothing
        ));
        let other = payload(permission(
            "Bash",
            serde_json::json!({"command": "rm -rf /"}),
        ));
        assert!(matches!(
            claude.ingest(&other, &ctx(t0 + Duration::from_secs(4))),
            Ingest::Held { .. }
        ));
        assert!(
            matches!(
                claude.ingest(&ask, &ctx(t0 + DEDUPE + Duration::from_millis(1))),
                Ingest::Held { .. }
            ),
            "past the window the operator is being asked again"
        );
    }

    /// C5: `AskUserQuestion` never raises a permission card, and neither
    /// does anything from a session whose group was just released.
    #[test]
    fn askuserquestion_and_a_just_released_group_raise_nothing() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(permission(ASK, serde_json::json!({"questions": []})));
        assert!(matches!(claude.ingest(&ask, &ctx(t0)), Ingest::Nothing));
        let bash = payload(permission("Bash", serde_json::json!({"command": "date"})));
        claude.carded(&bash, &question_card(), CardId(3));
        claude.released(&bash, &question_card(), CardId(3), Release::Snooze, t0);
        assert!(matches!(
            claude.ingest(&bash, &ctx(t0 + Duration::from_secs(29))),
            Ingest::Nothing
        ));
        assert!(
            matches!(
                claude.ingest(&bash, &ctx(t0 + RELEASED + Duration::from_millis(1))),
                Ingest::Held { .. }
            ),
            "past 30 s it is a permission of its own again"
        );
        // A released *permission* opens no such window: nothing followed it
        // that could be mistaken for its own follow-up.
        let claude = Claude::default();
        let cmd =
            Claude::permission_card(&bash, HookCfg::default(), "Bash", &serde_json::json!({}));
        claude.carded(&bash, &cmd, CardId(4));
        claude.released(&bash, &cmd, CardId(4), Release::Snooze, t0);
        assert!(matches!(
            claude.ingest(&bash, &ctx(t0 + Duration::from_secs(1))),
            Ingest::Held { .. }
        ));
    }

    /// §5.6: a released hold leaves its card on the deck as `⌨ Focus
    /// terminal`, pointing at the dialog the CLI is still showing. It
    /// belongs to the turn from then on, and the turn's end takes it off —
    /// otherwise it sits there until something else replaces it.
    #[test]
    fn a_released_card_is_cleared_when_the_turn_moves_on() {
        for (event, reason) in [("PostToolUse", "tool_ran"), ("Stop", "turn_ended")] {
            let claude = Claude::default();
            let t0 = Instant::now();
            let ask = payload(permission("Bash", serde_json::json!({"command": "date"})));
            let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
                panic!("a permission is held");
            };
            claude.carded(&ask, &cmd, CardId(21));
            claude.released(&ask, &cmd, CardId(21), Release::Snooze, t0);
            let next = payload(serde_json::json!({
                "session_id": "abc123", "cwd": "/w/project", "hook_event_name": event,
            }));
            let Ingest::Report(plan) = claude.ingest(&next, &ctx(t0)) else {
                panic!("{event} is report-only");
            };
            let cleared: Vec<(CardId, Option<&str>)> = plan
                .iter()
                .filter_map(|r| match r {
                    Request::CardClear(c) => Some((c.card, c.reason.as_deref())),
                    _ => None,
                })
                .collect();
            assert_eq!(cleared, vec![(CardId(21), Some(reason))], "{event}");
            // Once: the next event has nothing left to clear.
            let Ingest::Report(plan) = claude.ingest(&next, &ctx(t0)) else {
                panic!("{event} is report-only");
            };
            assert!(!plan.iter().any(|r| matches!(r, Request::CardClear(_))));
        }
    }

    /// A live question card is **not** the turn's to clear (C8). Claude
    /// runs tools in parallel, so a `PostToolUse` from another tool arrives
    /// mid-group; clearing the card would release the group under the
    /// operator's hands.
    #[test]
    fn a_live_question_card_survives_a_parallel_tool_finishing() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PreToolUse", "tool_name": ASK,
            "tool_input": {"questions": [
                {"question": "Which colour?", "header": "Colour",
                 "options": [{"label": "Red"}, {"label": "Green"}]}
            ]},
        }));
        let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("a question group is held");
        };
        claude.carded(&ask, &cmd, CardId(31));
        let post = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PostToolUse", "tool_name": "Read",
            "tool_input": {"file_path": "/w/project/src/a.rs"},
        }));
        let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
            panic!("PostToolUse is report-only");
        };
        assert!(
            !plan.iter().any(|r| matches!(r, Request::CardClear(_))),
            "the group owns its dialog for the whole hold: {plan:?}"
        );
    }

    /// "Allow always" is offered only when Claude handed us a rule to
    /// persist, and it is the only answer that writes `updatedPermissions`.
    #[test]
    fn allow_always_appears_and_persists_only_with_permission_suggestions() {
        let claude = Claude::default();
        let bare = payload(permission("Bash", serde_json::json!({"command": "ls"})));
        let cmd = Claude::permission_card(
            &bare,
            HookCfg::default(),
            "Bash",
            &serde_json::json!({"command": "ls"}),
        );
        let labels: Vec<&str> = cmd.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec![ALLOW, DENY]);
        let suggestions = serde_json::json!([{
            "type": "addRules",
            "rules": [{"toolName": "Bash", "ruleContent": "ls"}],
            "behavior": "allow",
            "destination": "localSettings",
        }]);
        let mut value = permission("Bash", serde_json::json!({"command": "ls"}));
        value["permission_suggestions"] = suggestions.clone();
        let rich = payload(value);
        let cmd = Claude::permission_card(
            &rich,
            HookCfg::default(),
            "Bash",
            &serde_json::json!({"command": "ls"}),
        );
        let labels: Vec<&str> = cmd.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec![ALLOW, ALLOW_ALWAYS, DENY]);
        assert_eq!(cmd.rec, Some(0), "Allow is the recommended row");
        let note = serde_json::json!({
            "event": "PermissionRequest",
            "suggestions": suggestions.clone(),
        });
        let out = claude
            .answer_to_output(&note, &cmd, Some(&choice(ALLOW_ALWAYS)))
            .json();
        let decision = &out["hookSpecificOutput"]["decision"];
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(decision["updatedPermissions"], suggestions);
        let out = claude
            .answer_to_output(&note, &cmd, Some(&choice(ALLOW)))
            .json();
        assert!(
            out["hookSpecificOutput"]["decision"]["updatedPermissions"].is_null(),
            "a plain Allow persists nothing: {out}"
        );
        // A release is `{}` whatever the card was.
        assert!(claude.answer_to_output(&note, &cmd, None).is_empty());
    }

    /// §5.6's grammar: a spoken "yes" is an allow, "no" a deny, and
    /// anything else a deny that hands Claude the operator's words.
    #[test]
    fn free_text_on_a_permission_card_follows_the_intent_grammar() {
        let claude = Claude::default();
        let cmd = Claude::permission_card(
            &payload(permission("Bash", serde_json::json!({"command": "ls"}))),
            HookCfg::default(),
            "Bash",
            &serde_json::json!({"command": "ls"}),
        );
        let note = serde_json::json!({"event": "PermissionRequest", "suggestions": null});
        let spoken = |text: &str| Answer {
            choice: None,
            label: None,
            text: Some(text.to_string()),
            source: AnswerSource::Ptt,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        };
        for yes in ["yes", "Allow", "go ahead", "Okay!", "do it"] {
            let out = claude
                .answer_to_output(&note, &cmd, Some(&spoken(yes)))
                .json();
            assert_eq!(
                out["hookSpecificOutput"]["decision"]["behavior"], "allow",
                "{yes}"
            );
        }
        for no in ["no", "deny", "Stop.", "don't"] {
            let out = claude
                .answer_to_output(&note, &cmd, Some(&spoken(no)))
                .json();
            assert_eq!(
                out["hookSpecificOutput"]["decision"]["behavior"], "deny",
                "{no}"
            );
            assert_eq!(
                out["hookSpecificOutput"]["decision"]["message"], DENIED,
                "{no}"
            );
        }
        let out = claude
            .answer_to_output(&note, &cmd, Some(&spoken("run the tests first")))
            .json();
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            out["hookSpecificOutput"]["decision"]["message"], "run the tests first",
            "the agent reads the instruction and re-plans"
        );
        assert_eq!(intent("maybe"), Intent::Say);
    }

    /// C7: a multi-select answer is a comma-joined **string**; an array
    /// would fail Claude's schema and lose the whole turn.
    #[test]
    fn a_group_answer_is_a_string_keyed_by_the_question_text() {
        let claude = Claude::default();
        let input = serde_json::json!({
            "questions": [
                {"question": "Which colours?", "header": "Colour",
                 "options": [{"label": "Red"}, {"label": "Green"}, {"label": "Blue"}],
                 "multiSelect": true},
            ]
        });
        let note = serde_json::json!({"event": "PreToolUse", "input": input.clone()});
        let plan = GroupPlan {
            rest: Vec::new(),
            questions: vec![GroupQuestion {
                key: "Which colours?".into(),
                options: vec!["Red".into(), "Green".into(), "Blue".into()],
            }],
        };
        let typed = Answer {
            choice: None,
            label: None,
            text: Some("Red, Blue".into()),
            source: AnswerSource::Gui,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        };
        let out = claude.group_to_output(&note, &plan, &[Some(typed)]).json();
        let updated = &out["hookSpecificOutput"]["updatedInput"];
        assert_eq!(
            updated["questions"], input["questions"],
            "updatedInput replaces the whole input, so questions is echoed verbatim"
        );
        let answers = &updated["answers"];
        assert!(answers["Which colours?"].is_string(), "{answers}");
        assert_eq!(answers["Which colours?"], "Red, Blue");
        // One unanswered card releases the whole group with `{}`.
        assert!(claude.group_to_output(&note, &plan, &[None]).is_empty());
        assert!(claude.group_to_output(&note, &plan, &[]).is_empty());
    }

    /// §2.3/C7: the value filed under a question is one of the **option
    /// labels Claude sent**, byte for byte. §5.6 cuts a label to 32 bytes
    /// for the deck's line (and numbers the collisions `#n`), so the label
    /// the deck drew — the one that arrives on `Answer` — is a string the
    /// model never wrote. Filing it hands Claude an answer matching none
    /// of the options the same response echoes back in `questions`, and
    /// the turn is lost.
    #[test]
    fn a_group_answer_is_one_of_the_options_claude_sent_not_the_cut_label() {
        // Two options that are identical for the first 32 bytes and
        // differ past them: `sanitize::labels` cuts both to the same
        // string and numbers them `#1`/`#2`.
        let long_a = "Rewrite the migration in place and keep the old column";
        let long_b = "Rewrite the migration in place and drop the old column";
        assert!(long_a.len() > sanitize::LABEL_MAX && long_b.len() > sanitize::LABEL_MAX);
        let input = serde_json::json!({
            "questions": [{
                "question": "How should the migration land?",
                "header": "Migration",
                "options": [{"label": long_a}, {"label": long_b}],
            }]
        });
        let Questions { cards, questions } =
            Claude::question_cards(&input, Some("toolu_9"), HookCfg::default())
                .expect("one question card");
        let drawn: Vec<&str> = cards[0].choices.iter().map(|c| c.label.as_str()).collect();
        assert_ne!(drawn[0], drawn[1], "the collision is disambiguated");
        for label in &drawn {
            assert!(label.len() <= sanitize::LABEL_MAX, "{label}");
            assert!(
                *label != long_a && *label != long_b,
                "the deck's label is not the option: {label}"
            );
        }
        let plan = GroupPlan {
            rest: Vec::new(),
            questions,
        };
        let note = serde_json::json!({"event": "PreToolUse", "input": input.clone()});
        // The deck answers row 1: `label` is what it drew, `choice` is the
        // index of the row.
        let picked = Answer {
            choice: Some(1),
            label: Some(drawn[1].to_string()),
            text: None,
            source: AnswerSource::Deck,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        };
        let out = Claude::default()
            .group_to_output(&note, &plan, &[Some(picked)])
            .json();
        let updated = &out["hookSpecificOutput"]["updatedInput"];
        let filed = updated["answers"]["How should the migration land?"]
            .as_str()
            .expect("a string answer");
        assert_eq!(filed, long_b, "the option Claude sent, untouched");
        // And it is byte-identical to one of the labels this very response
        // echoes back, which is the only thing Claude will accept.
        let echoed: Vec<&str> = updated["questions"][0]["options"]
            .as_array()
            .expect("the echoed options")
            .iter()
            .map(|o| o["label"].as_str().expect("a label"))
            .collect();
        assert!(echoed.contains(&filed), "{filed:?} not in {echoed:?}");
        // Free text still wins over any row: it is the operator's own
        // words, and a multi-select is already comma-joined.
        let typed = Answer {
            choice: Some(0),
            label: Some(drawn[0].to_string()),
            text: Some("neither, ask me again".into()),
            source: AnswerSource::Gui,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        };
        let out = Claude::default()
            .group_to_output(&note, &plan, &[Some(typed)])
            .json();
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"]["answers"]["How should the migration land?"],
            "neither, ask me again"
        );
        // With no index and no text there is nothing but the drawn label,
        // which is better than dropping the turn.
        let bare = Answer {
            choice: None,
            label: Some(drawn[0].to_string()),
            text: None,
            source: AnswerSource::Gui,
            principal: deck_model::Principal::Device,
            session: None,
            utterance: None,
        };
        let out = Claude::default()
            .group_to_output(&note, &plan, &[Some(bare)])
            .json();
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"]["answers"]["How should the migration land?"],
            drawn[0]
        );
    }

    /// C8's own path: the operator answers in the terminal, `PostToolUse`
    /// takes the permission card off the deck, and **clearing it** is what
    /// releases the parked hook. Nothing is left on the deck, so nothing
    /// may be filed as this session's open card — a dead id there makes
    /// `carded_already` true for ever and silences every card-raising
    /// `Notification` until the next turn boundary or `MEMO_IDLE` (1 h).
    #[test]
    fn a_hold_released_by_its_card_being_cleared_leaves_no_open_card() {
        let t0 = Instant::now();
        let ask = payload(permission("Bash", serde_json::json!({"command": "date"})));
        let post = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project", "hook_event_name": "PostToolUse",
        }));
        let prompt = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project", "hook_event_name": "Notification",
            "notification_type": "permission_prompt", "message": "Claude needs your permission",
        }));

        let claude = Claude::default();
        let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("a permission is held");
        };
        claude.carded(&ask, &cmd, CardId(9));
        // The operator answers in the terminal: the card is cleared…
        let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
            panic!("PostToolUse is report-only");
        };
        assert!(
            plan.iter()
                .any(|r| matches!(r, Request::CardClear(c) if c.card == CardId(9))),
            "the turn takes the card off the deck"
        );
        // …and the clear releases the hook that was parked on it.
        claude.released(&ask, &cmd, CardId(9), Release::Cleared, t0);
        let Ingest::Report(plan) = claude.ingest(&prompt, &ctx(t0)) else {
            panic!("a Notification is report-only");
        };
        assert!(
            plan.iter().any(|r| matches!(r, Request::CardSet(_))),
            "a card-raising Notification is not silenced by a card that is gone"
        );

        // The contrast: a release that really did leave `⌨ Focus terminal`
        // on the deck still owns the session, and a second card there
        // would be a duplicate with a chime of its own.
        let claude = Claude::default();
        let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("a permission is held");
        };
        claude.carded(&ask, &cmd, CardId(10));
        claude.released(&ask, &cmd, CardId(10), Release::Expiry, t0);
        assert!(
            matches!(claude.ingest(&prompt, &ctx(t0)), Ingest::Nothing),
            "the degraded card already says this"
        );
    }

    /// Two held events for one CLI session displace each other: parking
    /// the second retires the first, and the two handlers then run with no
    /// ordering between them. A `released` for the **displaced** card must
    /// not claim the session's open card back from the live one — C8's
    /// clear would spend itself on a dead id (`-32005 stale_card`) and the
    /// card the operator is looking at would stay up until `hold_s`.
    ///
    /// `parked_already` refuses the *sequential* form of this, but it is
    /// read on the way in and `parked` is written only once the core has
    /// allocated the card, so two hooks that arrive inside that window
    /// both pass the guard and both park. That is the window staged here:
    /// both events are ingested before either is carded.
    #[test]
    fn a_displaced_hold_does_not_take_the_live_cards_place() {
        for reversed in [false, true] {
            let t0 = Instant::now();
            let claude = Claude::default();
            let first = payload(permission("Bash", serde_json::json!({"command": "date"})));
            let second = payload(permission("Bash", serde_json::json!({"command": "uname"})));
            let Ingest::Held { cmd: one, .. } = claude.ingest(&first, &ctx(t0)) else {
                panic!("a permission is held");
            };
            let Ingest::Held { cmd: two, .. } = claude.ingest(&second, &ctx(t0)) else {
                panic!("the second permission is held too: nothing is parked yet");
            };
            // Both are parked in the core; the second retired the first in the
            // same step, and the two handlers come back in either order.
            if reversed {
                claude.carded(&second, &two, CardId(2));
                claude.carded(&first, &one, CardId(1));
            } else {
                claude.carded(&first, &one, CardId(1));
                claude.carded(&second, &two, CardId(2));
            }
            // Handler A's release lands last, and with the reason that *does*
            // leave a degraded card behind — it still is not this session's.
            claude.released(&first, &one, CardId(1), Release::Expiry, t0);

            let post = payload(serde_json::json!({
                "session_id": "abc123", "cwd": "/w/project", "hook_event_name": "PostToolUse",
            }));
            let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
                panic!("PostToolUse is report-only");
            };
            let cleared: Vec<CardId> = plan
                .iter()
                .filter_map(|r| match r {
                    Request::CardClear(c) => Some(c.card),
                    _ => None,
                })
                .collect();
            assert_eq!(
                cleared,
                vec![CardId(2)],
                "the live card is cleared, never the one the store already retired"
            );
        }
    }

    /// C7: 1–4 questions, 2–4 options. A payload outside the bounds is
    /// not answered at all rather than answered wrong.
    #[test]
    fn question_cards_take_the_schema_bounds_and_carry_the_group() {
        let input = serde_json::json!({
            "questions": [
                {"question": "Which colour?", "header": "Colour",
                 "options": [{"label": "Red"}, {"label": "Green"}]},
                {"question": "Which size?", "header": "Size",
                 "options": [{"label": "S"}, {"label": "M"}, {"label": "L"}]},
            ]
        });
        let Questions { cards, questions } =
            Claude::question_cards(&input, Some("toolu_1"), HookCfg::default()).expect("two cards");
        assert_eq!(
            questions.iter().map(|q| q.key.as_str()).collect::<Vec<_>>(),
            vec!["Which colour?", "Which size?"]
        );
        assert_eq!(
            questions
                .iter()
                .map(|q| q.options.clone())
                .collect::<Vec<_>>(),
            vec![vec!["Red", "Green"], vec!["S", "M", "L"]],
            "the labels Claude sent, kept beside the ones the deck draws"
        );
        assert_eq!(cards[0].text, "Colour: Which colour?");
        assert_eq!(cards[0].origin.kind, OriginKind::Question);
        assert_eq!(cards[0].origin.request_id.as_deref(), Some("toolu_1"));
        assert_eq!(cards[0].origin.group, Some(Group { index: 0, count: 2 }));
        assert_eq!(cards[1].origin.group, Some(Group { index: 1, count: 2 }));
        assert!(cards.iter().all(|c| c.deliver == Deliver::Hold));
        assert!(cards.iter().all(|c| c.free_text));
        assert!(cards
            .iter()
            .all(|c| c.choices.len() <= deck_model::MAX_CHOICES));
        // Out of bounds in either direction: nothing.
        let one = serde_json::json!({"questions": [
            {"question": "q", "options": [{"label": "only"}]}
        ]});
        assert!(Claude::question_cards(&one, None, HookCfg::default()).is_none());
        let five = serde_json::json!({"questions": [
            {"question": "q", "options": [
                {"label": "a"}, {"label": "b"}, {"label": "c"}, {"label": "d"}, {"label": "e"}]}
        ]});
        assert!(Claude::question_cards(&five, None, HookCfg::default()).is_none());
        let none = serde_json::json!({"questions": []});
        assert!(Claude::question_cards(&none, None, HookCfg::default()).is_none());
    }

    /// C10: 17 notification types exist where the docs list 12, so an
    /// unknown one is ignored — never a panic, never a card.
    #[test]
    fn unknown_notification_types_are_ignored() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let note = |kind: &str| {
            payload(serde_json::json!({
                "session_id": "abc123",
                "cwd": "/w/project",
                "hook_event_name": "Notification",
                "message": "Claude needs your permission",
                "notification_type": kind,
            }))
        };
        for unknown in [
            "computer_use_enter",
            "computer_use_exit",
            "push_notification",
            "model_refusal_fallback",
            "quota_auto_resume_stale",
            "a_type_from_a_later_claude",
            "",
        ] {
            assert!(
                matches!(claude.ingest(&note(unknown), &ctx(t0)), Ingest::Nothing),
                "{unknown}"
            );
        }
        assert!(matches!(
            claude.ingest(&note("idle_prompt"), &ctx(t0)),
            Ingest::Report(_)
        ));
        assert!(matches!(
            claude.ingest(&note("permission_prompt"), &ctx(t0)),
            Ingest::Report(_)
        ));
        // An unknown *event* is the same: ignored.
        let odd = payload(serde_json::json!({
            "session_id": "abc123", "hook_event_name": "WorktreeCreate",
        }));
        assert!(matches!(claude.ingest(&odd, &ctx(t0)), Ingest::Nothing));
        let headless = payload(serde_json::json!({"hook_event_name": "Stop"}));
        assert!(matches!(
            claude.ingest(&headless, &ctx(t0)),
            Ingest::Nothing
        ));
    }

    /// C25: the stamp is a complete OSC 2 sequence and it is emitted on
    /// `SessionStart` and nowhere else.
    #[test]
    fn the_beacon_is_a_complete_osc2_sequence_on_session_start_alone() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let start = payload(serde_json::json!({
            "session_id": "abc123",
            "cwd": "/w/project",
            "hook_event_name": "SessionStart",
            "source": "startup",
        }));
        let Ingest::Beacon(out, plan) = claude.ingest(&start, &ctx(t0)) else {
            panic!("SessionStart answers with the beacon");
        };
        let json = out.json();
        let seq = json["terminalSequence"].as_str().expect("a sequence");
        assert!(seq.starts_with("\u{1b}]2;"), "{seq:?}");
        assert!(seq.ends_with('\u{7}'), "{seq:?}");
        assert_eq!(seq, terminal_sequence("abc123"));
        assert_eq!(&seq[4..seq.len() - 1], stamp("abc123"));
        assert_eq!(stamp("abc123").len(), 9, "deck: plus four hex");
        assert_eq!(json["hookSpecificOutput"]["hookEventName"], "SessionStart");
        // The register carries the same stamp, which is how the resolver
        // finds the tab (C26).
        let Request::AgentRegister(register) = &plan[0] else {
            panic!("the plan leads with the register: {plan:?}");
        };
        assert_eq!(
            register
                .terminal
                .as_ref()
                .and_then(|t| t.title_stamp.clone()),
            Some(stamp("abc123"))
        );
        assert_eq!(register.key.as_deref(), Some("claude:abc123"));
        assert_eq!(register.name, "project");
        assert_eq!(register.caps.answer, AnswerCap::Hold);
        // No other event re-emits it (C25).
        for event in ["UserPromptSubmit", "Stop", "PostToolUse"] {
            let other = payload(serde_json::json!({
                "session_id": "abc123", "cwd": "/w/project", "hook_event_name": event,
            }));
            let ingest = claude.ingest(&other, &ctx(t0));
            assert!(
                ingest.report_output().is_empty(),
                "{event} re-emitted the stamp"
            );
        }
    }

    /// An http event carries no environment, so it must never blank the
    /// terminal identity the `SessionStart` command hook captured (C1).
    #[test]
    fn an_http_event_writes_no_terminal() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let prompt = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "UserPromptSubmit", "prompt": "hello",
        }));
        let Ingest::Report(plan) = claude.ingest(&prompt, &ctx(t0)) else {
            panic!("a prompt is report-only");
        };
        let Request::AgentRegister(register) = &plan[0] else {
            panic!("{plan:?}");
        };
        assert!(
            register.terminal.is_none(),
            "nothing to say, nothing written"
        );
        assert!(register.pid.is_none());
        let mut ctx = ctx(t0);
        ctx.tty = Some("ttys004".into());
        ctx.cli_pid = Some(4242);
        ctx.pid_kind = Some(deck_model::PidKind::Cli);
        ctx.env.insert("TERM_PROGRAM".into(), "ghostty".into());
        let Ingest::Report(plan) = claude.ingest(&prompt, &ctx) else {
            panic!("a prompt is report-only");
        };
        let Request::AgentRegister(register) = &plan[0] else {
            panic!("{plan:?}");
        };
        let terminal = register.terminal.as_ref().expect("the helper's capture");
        assert_eq!(terminal.tty.as_deref(), Some("ttys004"));
        assert_eq!(terminal.program.as_deref(), Some("ghostty"));
        assert!(
            terminal.title_stamp.is_none(),
            "the stamp is SessionStart's"
        );
        assert_eq!(register.pid, Some(4242));
        assert_eq!(register.pid_kind, Some(deck_model::PidKind::Cli));
    }

    /// §4.3: the store keeps the whole reply and the **projection** cuts
    /// it — the device's 200 bytes are the wire encoder's (`Caps::reply`),
    /// and §5.6's 128 bytes are a card-text rule that belongs to the Ready
    /// card beside the reply, not to the reply.
    #[test]
    fn the_reply_keeps_what_the_model_said_and_only_the_card_is_cut() {
        let claude = Claude::default();
        let long = format!("{} and then some more words after it", "x".repeat(400));
        let stop = payload(serde_json::json!({
            "session_id": "abc123",
            "hook_event_name": "Stop",
            "last_assistant_message": format!("\u{1b}[32m{long}\u{1b}[0m"),
        }));
        let Ingest::Report(plan) = claude.ingest(&stop, &ctx(Instant::now())) else {
            panic!("Stop is report-only");
        };
        let reply = plan
            .iter()
            .find_map(|r| match r {
                Request::Reply(r) => Some(r),
                _ => None,
            })
            .expect("a reply");
        assert_eq!(reply.text, long, "ANSI stripped, nothing else taken");
        assert!(reply.text.len() > sanitize::TEXT_MAX * 3);
        let card = plan
            .iter()
            .find_map(|r| match r {
                Request::CardSet(c) if c.kind == CardKind::R => Some(c),
                _ => None,
            })
            .expect("a ready card");
        assert_eq!(card.text.len(), sanitize::TEXT_MAX);
        assert!(card.text.ends_with('…'), "{}", card.text);
        assert!(long.starts_with(card.text.trim_end_matches('…')));

        // `StopFailure`'s detail is a reply too: a 429's retry advice is
        // the one thing worth reading and does not fit in 128 bytes.
        let failed = payload(serde_json::json!({
            "session_id": "abc123",
            "hook_event_name": "StopFailure",
            "error": "rate_limit",
            "error_details": long.clone(),
        }));
        let Ingest::Report(plan) = claude.ingest(&failed, &ctx(Instant::now())) else {
            panic!("StopFailure is report-only");
        };
        let reply = plan
            .iter()
            .find_map(|r| match r {
                Request::Reply(r) => Some(r),
                _ => None,
            })
            .expect("a reply");
        assert_eq!(reply.text, long);
        // The title is card-shaped and still keeps §5.6's cut.
        let title = plan
            .iter()
            .find_map(|r| match r {
                Request::AgentUpdate(u) => u.title.clone(),
                _ => None,
            })
            .expect("a title");
        assert!(title.len() <= sanitize::TEXT_MAX, "{title}");
    }

    #[test]
    fn a_tool_summary_is_sanitised_and_named_after_the_tool() {
        assert_eq!(
            tool_summary(
                "Bash",
                &serde_json::json!({"command": "rm -rf node_modules"})
            ),
            "Bash: rm -rf node_modules"
        );
        assert_eq!(
            tool_summary("Edit", &serde_json::json!({"file_path": "/w/p/src/a.rs"})),
            "Edit: /w/p/src/a.rs"
        );
        assert_eq!(
            tool_summary("mcp__memory__read", &serde_json::json!({"key": "k"})),
            "memory/read: k"
        );
        assert_eq!(tool_summary("Unknown", &serde_json::json!({})), "Unknown");
        let ansi = tool_summary(
            "Bash",
            &serde_json::json!({"command": "\u{1b}[31mred\u{1b}[0m"}),
        );
        assert_eq!(ansi, "Bash: red", "ANSI is stripped (§5.6)");
        let long = "x".repeat(400);
        let cut = tool_summary("Bash", &serde_json::json!({"command": long}));
        assert!(cut.len() <= sanitize::TEXT_MAX, "{}", cut.len());
    }

    /// C8: the permission card is first-answer-wins, so `PostToolUse` and
    /// `Stop` clear the card the adapter raised — and clear it only once.
    #[test]
    fn post_tool_use_and_stop_clear_the_permission_card() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(permission("Bash", serde_json::json!({"command": "date"})));
        let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("a permission is held");
        };
        claude.carded(&ask, &cmd, CardId(11));
        let post = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "date"},
        }));
        let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
            panic!("PostToolUse is report-only");
        };
        let cleared: Vec<CardId> = plan
            .iter()
            .filter_map(|r| match r {
                Request::CardClear(c) => Some(c.card),
                _ => None,
            })
            .collect();
        assert_eq!(cleared, vec![CardId(11)]);
        // Once: a second PostToolUse has nothing left to clear.
        let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
            panic!("PostToolUse is report-only");
        };
        assert!(!plan.iter().any(|r| matches!(r, Request::CardClear(_))));
        // `uncarded` is the same forgetting, for the answered path.
        claude.carded(&ask, &cmd, CardId(12));
        claude.uncarded(&ask, CardId(12));
        let Ingest::Report(plan) = claude.ingest(&post, &ctx(t0)) else {
            panic!("PostToolUse is report-only");
        };
        assert!(!plan.iter().any(|r| matches!(r, Request::CardClear(_))));
    }

    /// One held card per CLI session, whatever raised it.
    ///
    /// `agent.card.set` **retires** the agent's current card, and retiring
    /// a parked hold releases it with `Release::Expiry` and pulls it off
    /// the deck. So a second held event for a session that already has one
    /// parked is not a second card: it is the first card taken away from
    /// the operator mid-answer, the still-open hook behind it answered
    /// `{}`, and — for a question group — every answer already given lost,
    /// because `updatedInput.answers` is written only after the last card.
    /// `Notification` has been guarded since §5.1; the two held events were
    /// not, and they are the ones that park.
    #[test]
    fn a_second_held_event_is_refused_while_one_is_parked() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PreToolUse", "tool_name": ASK,
            "tool_use_id": "toolu_1",
            "tool_input": {"questions": [
                {"question": "Which colour?", "header": "Colour",
                 "options": [{"label": "Red"}, {"label": "Green"}]},
                {"question": "Which size?", "header": "Size",
                 "options": [{"label": "S"}, {"label": "L"}]}
            ]},
        }));
        let Ingest::Held { cmd, group, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("an AskUserQuestion group is held");
        };
        assert_eq!(group.expect("the rest of the group").rest.len(), 1);
        claude.carded(&ask, &cmd, CardId(1));

        // The operator is on question 1 of 2 when Claude asks to run a
        // tool. Before the guard this parked a second card, which retired
        // the first and answered the group's hook `{}`.
        let perm = payload(permission("Bash", serde_json::json!({"command": "ls"})));
        assert!(
            matches!(claude.ingest(&perm, &ctx(t0)), Ingest::Nothing),
            "a PermissionRequest may not displace a parked card"
        );
        // And the other way round: a group may not displace a parked
        // permission card either.
        let other = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PreToolUse", "tool_name": ASK,
            "tool_use_id": "toolu_2",
            "tool_input": {"questions": [
                {"question": "Which colour?", "header": "Colour",
                 "options": [{"label": "Red"}]}
            ]},
        }));
        assert!(
            matches!(claude.ingest(&other, &ctx(t0)), Ingest::Nothing),
            "a second group may not displace a parked card"
        );

        // The group's own next card is not a new ingest — the handler
        // issues it from `GroupPlan` — so answering card 1 lets the next
        // ask through again.
        claude.uncarded(&ask, CardId(1));
        assert!(
            matches!(claude.ingest(&perm, &ctx(t0)), Ingest::Held { .. }),
            "nothing is parked any more"
        );
    }

    /// A release hands the degraded card to `open`, where the next
    /// `PostToolUse`/`Stop` will clear it — unless something newer is
    /// already there. A hold that expires behind a turn that has already
    /// moved on would otherwise point the next clear at a card that is
    /// gone and leave the live one on the deck. Ids only go up.
    ///
    /// This is the second of the two guards, and the one
    /// [`a_displaced_hold_does_not_take_the_live_cards_place`] cannot
    /// reach: there the released card is no longer the parked one, here it
    /// still is — a card can be parked while `open` belongs to a newer one,
    /// because only a `Permission` origin writes `open` when it is carded.
    #[test]
    fn a_late_release_does_not_overwrite_a_newer_open_card() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let ask = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "PreToolUse", "tool_name": ASK,
            "tool_use_id": "toolu_1",
            "tool_input": {"questions": [
                {"question": "Which colour?", "header": "Colour",
                 "options": [{"label": "Red"}, {"label": "Green"}]}
            ]},
        }));
        let Ingest::Held { cmd, .. } = claude.ingest(&ask, &ctx(t0)) else {
            panic!("an AskUserQuestion is held");
        };
        // A question card parks without claiming `open`...
        claude.carded(&ask, &cmd, CardId(4));
        assert_eq!(
            claude.memo.lock().expect("the memo").parked.get("abc123"),
            Some(&CardId(4))
        );
        // ...so the turn can move on to a newer permission card under it.
        claude
            .memo
            .lock()
            .expect("the memo")
            .open
            .insert("abc123".to_string(), CardId(9));

        claude.released(&ask, &cmd, CardId(4), Release::Expiry, t0);
        let memo = claude.memo.lock().expect("the memo");
        assert_eq!(
            memo.open.get("abc123"),
            Some(&CardId(9)),
            "the newer card keeps the slot"
        );
        assert!(
            !memo.parked.contains_key("abc123"),
            "the hold is over either way"
        );
    }

    /// `SessionEnd` releases the agent it knows and forgets the session;
    /// one it never saw is not registered just to be removed.
    #[test]
    fn session_end_releases_only_a_session_the_adapter_saw() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let end = payload(serde_json::json!({
            "session_id": "abc123", "cwd": "/w/project",
            "hook_event_name": "SessionEnd", "reason": "clear",
        }));
        assert!(matches!(claude.ingest(&end, &ctx(t0)), Ingest::Nothing));
        claude.resolved(&end, &AgentId("ag_7".into()));
        let Ingest::Report(plan) = claude.ingest(&end, &ctx(t0)) else {
            panic!("SessionEnd is report-only");
        };
        assert_eq!(
            plan,
            vec![Request::AgentRelease {
                agent: AgentId("ag_7".into()),
                reason: Some("clear".into()),
            }]
        );
        assert!(
            matches!(claude.ingest(&end, &ctx(t0)), Ingest::Nothing),
            "the session is forgotten"
        );
        claude.resolved(&end, &pending());
        assert!(matches!(claude.ingest(&end, &ctx(t0)), Ingest::Nothing));
    }

    /// `SessionEnd` is the only tidy way out of the memo, and a `SIGKILL`ed
    /// CLI never sends one: `kill -9`, a closed laptop, a crashed session.
    /// Without an age the agent id and the three card ids of every such
    /// session would sit in the maps for the daemon's whole uptime — weeks,
    /// by design. `MEMO_IDLE` makes them windows like the dedupe ones.
    #[test]
    fn a_session_that_was_killed_is_aged_out_of_the_memo() {
        let claude = Claude::default();
        let t0 = Instant::now();
        let killed = payload(serde_json::json!({
            "session_id": "killed", "cwd": "/w/project",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash", "tool_input": {"command": "ls"},
        }));
        // One session with something in every map: an agent, a Ready card
        // from the turn that ended, a permission card that is both open
        // and parked, and the dedupe key of the ask that raised it. The
        // `Stop` goes first because C8 has it clear the permission card.
        let ready = payload(serde_json::json!({
            "session_id": "killed", "hook_event_name": "Stop",
            "last_assistant_message": "done",
        }));
        let Ingest::Report(plan) = claude.ingest(&ready, &ctx(t0)) else {
            panic!("Stop is report-only");
        };
        claude.resolved(&ready, &AgentId("ag_1".into()));
        let ready_card = plan
            .iter()
            .find_map(|r| match r {
                Request::CardSet(c) if c.kind == CardKind::R => Some(c.clone()),
                _ => None,
            })
            .expect("a ready card");
        claude.carded(&ready, &ready_card, CardId(2));
        let Ingest::Held { cmd, .. } = claude.ingest(&killed, &ctx(t0)) else {
            panic!("a permission is held");
        };
        claude.carded(&killed, &cmd, CardId(1));
        {
            let memo = claude.memo.lock().expect("the memo");
            assert!(memo.agents.contains_key("killed"));
            assert!(memo.open.contains_key("killed"));
            assert!(memo.parked.contains_key("killed"));
            assert!(memo.ready.contains_key("killed"));
            assert_eq!(memo.touched.len(), 1);
            assert_eq!(memo.seen.len(), 1);
        }
        // …and then nothing, ever again: no `SessionEnd` is coming. The
        // sweep runs on the next hook of **any** session, which is the
        // only clock this adapter has.
        let other = payload(serde_json::json!({
            "session_id": "alive", "hook_event_name": "UserPromptSubmit",
            "prompt": "next",
        }));
        // Not yet: a session that is merely quiet keeps its guards, so a
        // card still on the deck is still recognised.
        claude.ingest(&other, &ctx(t0 + MEMO_IDLE - Duration::from_secs(1)));
        assert!(claude.carded_already("killed"), "still guarded at 59:59");
        claude.ingest(&other, &ctx(t0 + MEMO_IDLE + Duration::from_secs(1)));
        let memo = claude.memo.lock().expect("the memo");
        assert!(!memo.agents.contains_key("killed"));
        assert!(!memo.open.contains_key("killed"));
        assert!(!memo.parked.contains_key("killed"));
        assert!(!memo.ready.contains_key("killed"));
        assert!(!memo.touched.contains_key("killed"));
        assert!(memo.seen.is_empty(), "its dedupe key went with it");
        // The live session is untouched: the sweep is per session, not a
        // clear of the whole memo.
        assert!(memo.touched.contains_key("alive"));
    }
}
