//! `herdr` on the deck (`docs/orchestrator-adapters.md`, "herdr").
//!
//! What the adapter relies on, and nothing else:
//!
//! - `herdr agent list --json`: panes, each with `agent` (the name an order
//!   is addressed to), `state` ∈ `idle|working|blocked|done|unknown`,
//!   `workspace` and `title`. The list is either the top-level array or the
//!   object's `panes` array. `unknown`, and any state herdr adds later, is
//!   `Idle`: the state comes from the CLI only, never from the screen.
//! - `herdr agent prompt <name> <text> --wait --timeout <ms>`: one order,
//!   as argv.
//!
//! **Content.** A pane's `title` is the terminal's title, which is a
//! command line as often as not, so it is never used: a task is called by
//! its agent name and, when it is a plain word, its workspace. `blocked` is
//! not proof that anything may be approved: the card says "waiting for
//! approval" and never what is being asked.
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

/// `herdr agent list --json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["agent", "list", "--json"].map(OsString::from).to_vec()
}

/// `herdr agent prompt <name> <text> --wait --timeout <ms>`: the text is
/// one argument, whatever it holds.
#[must_use]
pub fn prompt_argv(name: &str, text: &str, timeout_ms: u64) -> Vec<OsString> {
    vec![
        "agent".into(),
        "prompt".into(),
        name.into(),
        text.into(),
        "--wait".into(),
        "--timeout".into(),
        timeout_ms.to_string().into(),
    ]
}

/// One pane's state word.
fn state(word: &str) -> TaskState {
    match word {
        "working" => TaskState::Working,
        "blocked" => TaskState::Blocked(Blocked::Approval),
        "done" => TaskState::Done,
        // `idle`, `unknown`, and whatever a later herdr adds.
        _ => TaskState::Idle,
    }
}

/// The workspace, when it is a plain word (never a path).
fn workspace(pane: &Value) -> Option<&str> {
    pane["workspace"]
        .as_str()
        .map(str::trim)
        .filter(|w| !w.is_empty() && safe_key(w) && !w.contains(' '))
}

/// The panes of `herdr agent list --json` as tasks. A pane with no usable
/// `agent` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let panes = match &value {
        Value::Array(panes) => panes,
        Value::Object(map) => map
            .get("panes")
            .and_then(Value::as_array)
            .ok_or_else(|| "malformed: no `panes` array".to_string())?,
        _ => return Err("malformed: not a list of panes".into()),
    };
    let mut tasks = Vec::new();
    for pane in panes {
        let Some(name) = pane["agent"]
            .as_str()
            .map(str::trim)
            .filter(|n| safe_key(n))
        else {
            continue;
        };
        let Some(word) = pane["state"].as_str() else {
            continue;
        };
        let label = match workspace(pane) {
            Some(ws) => format!("{name} · {ws}"),
            None => name.to_string(),
        };
        tasks.push(Task {
            key: name.to_string(),
            title: title(&label, name),
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
        if !safe_key(key) {
            return Err("bad_key: not a herdr agent name".into());
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

    /// Written from the documented shape, not captured from a live herdr.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/herdr.json");

    #[test]
    fn the_fixture_parses_into_the_documented_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("api-refactor", TaskState::Working),
                ("docs", TaskState::Blocked(Blocked::Approval)),
                ("release-notes", TaskState::Done),
                ("scratch", TaskState::Idle),
                ("reviewer", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "api-refactor · owt");
        assert_eq!(tasks[3].title, "scratch", "a path workspace is left out");
    }

    #[test]
    fn nothing_from_a_pane_title_or_path_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&Herdr, &tasks, budget(1)).to_string();
        for leak in [
            "cargo",
            "rm -rf",
            "/tmp",
            "~/",
            "/home",
            "sk-live",
            "OPENAI",
            "CHANGELOG",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&Herdr, &tasks, budget(1));
        assert_eq!(deck.len(), 3, "idle and unknown are absent");
        assert_eq!(deck[0].key, "docs");
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "docs · owt: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse("[]").expect("array"), []);
        assert_eq!(parse(r#"{"panes":[]}"#).expect("object"), []);
        assert!(plan(&Herdr, &[], budget(1)).is_empty());
    }

    #[test]
    fn a_forty_pane_fleet_fits_the_deck() {
        let panes: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "agent": format!("worker-{i}"),
                    "state": if i % 4 == 0 { "working" } else { "blocked" },
                    "workspace": "owt",
                    "title": format!("npm test --shard {i}"),
                })
            })
            .collect();
        let raw = serde_json::json!({ "panes": panes }).to_string();
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
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "herdr: not running",
            "{\"panes\": 3}",
            "\"text\"",
            "{\"panes\":[",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Panes herdr may grow: missing fields are skipped, not fatal.
        let tasks = parse(r#"[{"state":"blocked"},{"agent":"../x","state":"done"},{"agent":"ok"},{"agent":"a","state":"thinking"}]"#)
            .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn the_prompt_is_argv_with_the_text_as_one_argument() {
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
        assert_eq!(list_argv(), ["agent", "list", "--json"].map(OsString::from));
    }

    #[test]
    fn an_order_herdr_would_misread_is_refused_before_it_runs() {
        assert!(Herdr
            .send_order("-x", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Herdr
            .send_order("docs", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Herdr
            .send_order("docs", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }
}
