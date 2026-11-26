//! `herdr` on the deck (`docs/orchestrator-adapters.md`, "herdr").
//!
//! Verified against herdr 0.9.5 (protocol 22): `herdr api schema --json` for
//! the shape, and `agent list` from an isolated `herdr --session` server for
//! the fixture. What the adapter relies on, and nothing else:
//!
//! - `herdr agent list`: JSON by default (there is no `--json`; herdr rejects
//!   it with exit 2). The envelope is
//!   `{"id":…,"result":{"type":"agent_list","agents":[AgentInfo…]}}`.
//! - Per agent: `name` (the live name an order is addressed to; absent or
//!   null until someone names the agent), `pane_id` (`w1:p1`, also accepted
//!   as a target), and `agent_status` ∈ `idle|working|blocked|done|unknown`.
//!   `unknown`, and any status herdr adds later, is `Idle`, never `Done`.
//! - `herdr agent prompt <target> <text> --wait --timeout <ms>`: one order,
//!   as argv.
//!
//! **Never the `agent` field.** It is the agent *kind* (`claude`, `codex`):
//! herdr refuses a bare kind as a target, and two panes share one. It is shown
//! in the title of an unnamed agent and is never a key.
//!
//! **Content.** `title` / `terminal_title` are the terminal's title, a command
//! line as often as not, and `cwd` is a path: none of them is used.
//! `workspace_id` is an id (`w1`), not a name; the workspace's label exists
//! only in `herdr workspace list`, so it is left out rather than printed as
//! `w1`. `blocked` means herdr "recognized an approval or question UI" and
//! nothing in `AgentInfo` says which or what is asked: the card says "waiting
//! for approval" and never what is being asked.
use super::{json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::OsString;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "herdr";
/// `agent list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long herdr waits for the agent to take a prompt (`--timeout`).
pub const PROMPT_TIMEOUT_MS: u64 = 10_000;

pub struct Herdr;

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    Some(Box::new(Herdr))
}

/// `herdr agent list`: JSON is its only output.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["agent", "list"].map(OsString::from).to_vec()
}

/// `herdr agent prompt <target> <text> --wait --timeout <ms>`: the text is
/// one argument, whatever it holds.
#[must_use]
pub fn prompt_argv(target: &str, text: &str, timeout_ms: u64) -> Vec<OsString> {
    vec![
        "agent".into(),
        "prompt".into(),
        target.into(),
        text.into(),
        "--wait".into(),
        "--timeout".into(),
        timeout_ms.to_string().into(),
    ]
}

/// One agent's `agent_status`.
fn state(word: &str) -> TaskState {
    match word {
        "working" => TaskState::Working,
        "blocked" => TaskState::Blocked(Blocked::Approval),
        "done" => TaskState::Done,
        // `idle`, `unknown`, and whatever a later herdr adds.
        _ => TaskState::Idle,
    }
}

/// A live agent name as herdr accepts it: `[a-z][a-z0-9_-]{0,31}`.
fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A pane id: `w<n>:p<n>`.
fn is_pane_id(s: &str) -> bool {
    let Some((w, p)) = s.split_once(':') else {
        return false;
    };
    let digits = |x: &str| !x.is_empty() && x.len() <= 10 && x.bytes().all(|b| b.is_ascii_digit());
    w.strip_prefix('w').is_some_and(digits) && p.strip_prefix('p').is_some_and(digits)
}

/// What an order to this agent is addressed to: its `name`, else its
/// `pane_id`. Never `agent`, which is the kind.
fn target(agent: &Value) -> Option<&str> {
    let name = agent["name"].as_str().map(str::trim).filter(|n| is_name(n));
    name.or_else(|| {
        agent["pane_id"]
            .as_str()
            .map(str::trim)
            .filter(|p| is_pane_id(p))
    })
}

/// The agents of `herdr agent list` as tasks. An agent with neither a usable
/// `name` nor a `pane_id` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    if let Some(err) = value.get("error") {
        // herdr's error envelope; the code is a word, the message may hold a path.
        let code = err["code"]
            .as_str()
            .filter(|c| safe_key(c))
            .unwrap_or("error");
        return Err(format!("backend_refused: herdr {code}"));
    }
    let agents = value
        .get("result")
        .and_then(|r| r.get("agents"))
        .and_then(Value::as_array)
        .ok_or_else(|| "malformed: no `result.agents` array".to_string())?;
    let mut tasks = Vec::new();
    for agent in agents {
        let Some(key) = target(agent) else {
            continue;
        };
        let Some(word) = agent["agent_status"].as_str() else {
            continue;
        };
        let named = agent["name"].as_str().is_some_and(|n| n.trim() == key);
        let label = if named {
            key.to_string()
        } else {
            // Unnamed: the kind tells a person what it is, the pane id where.
            match agent["agent"]
                .as_str()
                .map(str::trim)
                .filter(|k| is_name(k))
            {
                Some(kind) => format!("{kind} {key}"),
                None => key.to_string(),
            }
        };
        tasks.push(Task {
            key: key.to_string(),
            title: title(&label, key),
            state: state(word),
            activity: None,
        });
    }
    Ok(tasks)
}

impl Orchestrator for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn label(&self) -> &'static str {
        "Herdr"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        parse(&run(PROGRAM, &list_argv(), LIST_TIMEOUT)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        if !(is_name(key) || is_pane_id(key)) {
            return Err("bad_key: not a herdr agent name or pane id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // herdr would read it as an option, not as the prompt.
        if text.starts_with('-') {
            return Err("bad_text: a prompt may not start with '-'".into());
        }
        // herdr's own timeout, and a little longer for ours.
        let guard = Duration::from_millis(PROMPT_TIMEOUT_MS) + Duration::from_secs(5);
        run(PROGRAM, &prompt_argv(key, text, PROMPT_TIMEOUT_MS), guard).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{budget, plan, status_json, Card, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;

    /// Captured from herdr 0.9.5: `herdr --session <tmp> agent list` against an
    /// isolated server whose shell panes self-reported their state with
    /// `herdr pane report-agent` and were named with `herdr agent rename`.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/herdr.json");
    /// Captured from the same server before any workspace existed.
    const EMPTY: &str = include_str!("../../tests/orchestrators/herdr-empty.json");

    fn one(agent: &Value) -> String {
        serde_json::json!({"id": "cli:agent:list", "result": {"type": "agent_list", "agents": [agent]}})
            .to_string()
    }

    #[test]
    fn the_capture_parses_into_herdrs_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("api-refactor", TaskState::Working),
                ("docs", TaskState::Blocked(Blocked::Approval)),
                ("release-notes", TaskState::Done),
                ("w4:p1", TaskState::Idle),
                ("reviewer", TaskState::Working),
                ("scratch", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "api-refactor", "no `· w1` suffix");
        assert_eq!(
            tasks[3].title, "pi w4:p1",
            "an unnamed agent: kind and pane"
        );
    }

    #[test]
    fn the_envelope_is_result_agents() {
        assert_eq!(parse(EMPTY).expect("captured empty"), []);
        // The shapes the first version read are not what herdr prints.
        assert!(parse(r#"{"panes":[{"agent":"docs","state":"blocked"}]}"#).is_err());
        assert!(parse(r#"[{"name":"docs","agent_status":"blocked"}]"#).is_err());
        assert!(parse(r#"{"id":"x","result":{"type":"agent_list"}}"#).is_err());
    }

    #[test]
    fn a_null_or_absent_name_falls_back_to_the_pane_id() {
        for agent in [
            serde_json::json!({"agent": "claude", "name": null, "pane_id": "w2:p3", "agent_status": "idle"}),
            serde_json::json!({"agent": "claude", "pane_id": "w2:p3", "agent_status": "idle"}),
            serde_json::json!({"agent": "claude", "name": "Not A Name", "pane_id": "w2:p3", "agent_status": "idle"}),
        ] {
            let tasks = parse(&one(&agent)).expect("one");
            assert_eq!(tasks.len(), 1, "{agent}");
            assert_eq!(tasks[0].key, "w2:p3", "{agent}");
        }
    }

    #[test]
    fn the_agent_kind_is_never_a_target() {
        // No name, no pane id: the kind alone must not make it addressable.
        let tasks = parse(&one(
            &serde_json::json!({"agent": "claude", "agent_status": "blocked", "workspace_id": "w1"}),
        ))
        .expect("one");
        assert_eq!(tasks, []);
        // A name and a kind: the name wins, and the kind is nowhere in the key.
        let tasks = parse(FIXTURE).expect("fixture");
        for t in &tasks {
            assert!(
                !["claude", "codex", "pi"].contains(&t.key.as_str()),
                "{}",
                t.key
            );
        }
        let argv = prompt_argv(&tasks[1].key, "go on", 10_000);
        assert_eq!(argv[2], OsString::from("docs"));
    }

    #[test]
    fn each_status_word_maps_and_unknown_is_never_done() {
        for (word, want) in [
            ("working", TaskState::Working),
            ("blocked", TaskState::Blocked(Blocked::Approval)),
            ("done", TaskState::Done),
            ("idle", TaskState::Idle),
            ("unknown", TaskState::Idle),
            ("thinking", TaskState::Idle),
        ] {
            let agent = serde_json::json!({"name": "a", "pane_id": "w1:p1", "agent_status": word});
            let tasks = parse(&one(&agent)).expect(word);
            assert_eq!(tasks[0].state, want, "{word}");
        }
    }

    #[test]
    fn nothing_from_a_title_or_path_reaches_the_json() {
        let mut tasks = parse(FIXTURE).expect("fixture");
        tasks.extend(
            parse(&one(&serde_json::json!({
                "agent": "claude", "name": "leaky", "pane_id": "w9:p1", "agent_status": "blocked",
                "workspace_id": "w9", "cwd": "/home/someone/src",
                "title": "export OPENAI_API_KEY=sk-live-0123456789abcdef",
                "terminal_title": "Allow: rm -rf /tmp/owt-build?",
            })))
            .expect("leaky"),
        );
        let out = status_json(&Herdr, &tasks, budget(1)).to_string();
        for leak in [
            "/tmp", "/home", "sandbox", "rm -rf", "sk-live", "OPENAI", "term_", "w1:t1", "· w",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&Herdr, &tasks, budget(1));
        assert_eq!(deck[0].key, "docs");
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "docs: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
    }

    #[test]
    fn a_forty_agent_fleet_fits_the_deck() {
        let agents: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "agent": "claude",
                    "name": format!("worker-{i}"),
                    "pane_id": format!("w{i}:p1"),
                    "agent_status": if i % 4 == 0 { "working" } else { "blocked" },
                    "workspace_id": format!("w{i}"),
                    "title": format!("npm test --shard {i}"),
                })
            })
            .collect();
        let raw = serde_json::json!({"id": "cli:agent:list", "result": {"type": "agent_list", "agents": agents}})
            .to_string();
        let tasks = parse(&raw).expect("fleet");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&Herdr, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
            let summary = deck.last().expect("summary");
            assert_eq!(summary.state, AgentState::Question);
            assert!(
                summary.title.ends_with("waiting: Herdr"),
                "{}",
                summary.title
            );
        }
    }

    #[test]
    fn a_malformed_payload_or_herdr_error_is_an_error_not_a_panic() {
        for raw in [
            "",
            "herdr: not running",
            "{\"result\": 3}",
            "\"text\"",
            "{\"result\":{",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // herdr's own error envelope, as 0.9.5 prints it with no server.
        let err = parse(r#"{"id":"cli:agent:list","error":{"code":"server_not_running","message":"no herdr server is running at /home/x/.config/herdr/herdr.sock; run `herdr` to start or attach it"}}"#)
            .expect_err("error envelope");
        assert_eq!(err, "backend_refused: herdr server_not_running");
        // Agents herdr may grow: missing fields are skipped, not fatal.
        let raw = r#"{"result":{"type":"agent_list","agents":[{"agent_status":"blocked"},{"name":"../x","pane_id":"nope","agent_status":"done"},{"name":"ok"},{"name":"a","agent_status":"thinking"}]}}"#;
        let tasks = parse(raw).expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn the_argv_is_what_herdr_0_9_5_accepts() {
        assert_eq!(list_argv(), ["agent", "list"].map(OsString::from));
        let argv = prompt_argv("docs", "ship it; rm -rf / && echo $(id)", 10_000);
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "agent",
                "prompt",
                "docs",
                "ship it; rm -rf / && echo $(id)",
                "--wait",
                "--timeout",
                "10000"
            ]
        );
    }

    #[test]
    fn an_order_herdr_would_misread_is_refused_before_it_runs() {
        for key in ["-x", "Docs", "w1:p", "w1:p1:x", "../docs", "a b", ""] {
            assert!(
                Herdr
                    .send_order(key, "hi")
                    .is_err_and(|e| e.starts_with("bad_key")),
                "{key:?}"
            );
        }
        assert!(Herdr
            .send_order("docs", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Herdr
            .send_order("w4:p1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }
}
