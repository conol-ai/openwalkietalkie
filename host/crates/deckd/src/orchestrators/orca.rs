//! `orca` on the deck (`docs/orchestrator-adapters.md`, "orca").
//!
//! What the adapter relies on, and nothing else:
//!
//! - `orca worker list --json`: an object with `workers` (each with `id`,
//!   `task`, `state`, `worktree`) and the **"Needs you"** queue,
//!   `needs_you` (each with `worker`, `reason`). A worker in "Needs you" is
//!   `Blocked(Approval)` whatever its own state says; otherwise `running` is
//!   `Working`, `failed`/`error`/`errored` is `Blocked(Failed)`,
//!   `done`/`completed` is `Done`, and anything else is `Idle`.
//! - Some `--json` responses print a banner line before the JSON:
//!   [`json_payload`] parses from the first `{`.
//! - `orca worker-send --worker <id> <text>`: one order, as argv
//!   ([`Orca::order_argv`], the one place the verb is spelled).
//!
//! **Content.** A worker's `worktree` is a path and is never read; its
//! `task` goes through [`title`], so a task that reads as a command line
//! is shown by the worker's id. A "Needs you" `reason` is what orca wants
//! to run and never reaches the deck: the card says "waiting for approval".
use super::{json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "orca";
/// `worker list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// `worker-send` gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Orca;

/// `Some` when `orca` is on `PATH`: there is no `[orca]` table in
/// `config.toml`, so an installed CLI is what "configured" means.
pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    installed().then(|| Box::new(Orca) as Box<dyn Orchestrator>)
}

/// Is `orca` (or `orca.exe`) a file in a `PATH` directory?
fn installed() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|dir| dir.join(PROGRAM).is_file() || dir.join("orca.exe").is_file())
    })
}

/// `orca worker list --json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["worker", "list", "--json"].map(OsString::from).to_vec()
}

impl Orca {
    /// `orca worker-send --worker <id> <text>`: the text is one argument,
    /// whatever it holds.
    #[must_use]
    pub fn order_argv(&self, key: &str, text: &str) -> Vec<OsString> {
        vec![
            "worker-send".into(),
            "--worker".into(),
            key.into(),
            text.into(),
        ]
    }
}

/// A worker's own state word.
fn state(word: &str) -> TaskState {
    match word.to_ascii_lowercase().as_str() {
        "running" | "working" | "busy" => TaskState::Working,
        "failed" | "error" | "errored" => TaskState::Blocked(Blocked::Failed),
        "done" | "completed" | "finished" => TaskState::Done,
        // `idle`, `queued`, `stopped`, and whatever a later orca adds.
        _ => TaskState::Idle,
    }
}

/// `orca worker list --json` as tasks. A worker with no usable `id`
/// cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let workers = value
        .get("workers")
        .and_then(Value::as_array)
        .ok_or_else(|| "malformed: no `workers` array".to_string())?;
    let needs_you: HashSet<&str> = match value.get("needs_you") {
        None | Some(Value::Null) => HashSet::new(),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| entry["worker"].as_str())
            .collect(),
        Some(_) => return Err("malformed: `needs_you` is not a list".into()),
    };
    let mut tasks = Vec::new();
    for worker in workers {
        let Some(id) = worker["id"]
            .as_str()
            .map(str::trim)
            .filter(|id| safe_key(id))
        else {
            continue;
        };
        let state = if needs_you.contains(id) {
            TaskState::Blocked(Blocked::Approval)
        } else {
            match worker["state"].as_str() {
                Some(word) => state(word),
                None => continue,
            }
        };
        tasks.push(Task {
            key: id.to_string(),
            title: title(worker["task"].as_str().unwrap_or_default(), id),
            state,
            activity: None,
        });
    }
    Ok(tasks)
}

/// The tasks in a saved `orca worker list --json` output.
pub fn from_file(path: &Path) -> Result<Vec<Task>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("unreadable: {}: {}", path.display(), e.kind()))?;
    parse(&raw)
}

impl Orchestrator for Orca {
    fn name(&self) -> &'static str {
        "orca"
    }

    fn label(&self) -> &'static str {
        "Orca"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        parse(&run(PROGRAM, &list_argv(), LIST_TIMEOUT)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        if !safe_key(key) {
            return Err("bad_key: not an orca worker id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // orca would read it as an option, not as the message.
        if text.starts_with('-') {
            return Err("bad_text: a message may not start with '-'".into());
        }
        run(PROGRAM, &self.order_argv(key, text), SEND_TIMEOUT).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{budget, plan, status_json, Card, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;

    /// Written from the documented shape, not captured from a live orca.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/orca.json");

    fn fixture_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/orchestrators/orca.json")
    }

    #[test]
    fn the_fixture_parses_into_the_documented_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("wk-101", TaskState::Working),
                ("wk-102", TaskState::Blocked(Blocked::Approval)),
                ("wk-103", TaskState::Blocked(Blocked::Failed)),
                ("wk-104", TaskState::Done),
                ("wk-105", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "Refactor the billing API");
        assert_eq!(
            tasks[2].title, "wk-103",
            "a command line is shown by its id"
        );
        assert_eq!(from_file(&fixture_path()).expect("from file"), tasks);
    }

    #[test]
    fn needs_you_is_an_approval_card_not_an_idle_worker() {
        let tasks = parse(FIXTURE).expect("fixture");
        let deck = plan(&Orca, &tasks, budget(1));
        assert_eq!(deck[0].key, "wk-102");
        assert_eq!(deck[0].state, AgentState::Question);
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "Migrate the users table: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
        assert!(deck.iter().all(|e| e.key != "wk-105"), "idle is absent");
    }

    #[test]
    fn nothing_from_a_worktree_command_or_token_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&Orca, &tasks, budget(1)).to_string();
        for leak in [
            "/home",
            "worktrees",
            ".orca",
            "npm",
            "git push",
            "psql",
            "DROP",
            "sk_live",
            "STRIPE_KEY",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
    }

    #[test]
    fn a_banner_before_the_json_is_skipped() {
        let raw = format!("orca v1.4.180 (update available: v1.4.181)\n{FIXTURE}");
        assert_eq!(parse(&raw).expect("banner"), parse(FIXTURE).expect("plain"));
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse(r#"{"workers":[]}"#).expect("empty"), []);
        assert_eq!(
            parse(r#"{"workers":[],"needs_you":[]}"#).expect("empty"),
            []
        );
        assert!(plan(&Orca, &[], budget(1)).is_empty());
    }

    #[test]
    fn a_forty_worker_fleet_fits_the_deck() {
        let workers: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "id": format!("wk-{i}"),
                    "task": format!("cargo test --shard {i}"),
                    "state": "running",
                    "worktree": format!("/srv/orca/wk-{i}"),
                })
            })
            .collect();
        let needs: Vec<Value> = (0..30)
            .map(|i| serde_json::json!({"worker": format!("wk-{i}"), "reason": "rm -rf /"}))
            .collect();
        let raw = serde_json::json!({"workers": workers, "needs_you": needs}).to_string();
        let tasks = parse(&raw).expect("fleet");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&Orca, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
            let summary = deck.last().expect("summary");
            assert!(
                summary.title.ends_with("waiting: Orca"),
                "{}",
                summary.title
            );
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "orca: not logged in",
            "[]",
            r#"{"workers": 3}"#,
            r#"{"workers":[],"needs_you":"wk-1"}"#,
            r#"{"workers":["#,
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        let tasks = parse(
            r#"{"workers":[{"state":"running"},{"id":"../x","state":"done"},{"id":"a"},{"id":"b","state":"paused"}]}"#,
        )
        .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
        assert!(from_file(Path::new("/nonexistent/orca.json")).is_err());
    }

    #[test]
    fn the_order_is_argv_with_the_text_as_one_argument() {
        let argv = Orca.order_argv("wk-102", "yes; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "worker-send",
                "--worker",
                "wk-102",
                "yes; rm -rf / && echo $(id)"
            ]
        );
        assert_eq!(
            list_argv(),
            ["worker", "list", "--json"].map(OsString::from)
        );
    }

    #[test]
    fn an_order_orca_would_misread_is_refused_before_it_runs() {
        assert!(Orca
            .send_order("-x", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Orca
            .send_order("wk-1", " ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Orca
            .send_order("wk-1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }
}
