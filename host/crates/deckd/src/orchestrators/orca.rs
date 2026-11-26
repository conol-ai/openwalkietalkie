//! `orca` on the deck (`docs/orchestrator-adapters.md`, "orca").
//!
//! **Doc-derived, not verified against a running instance.** Sources, read
//! 2026-11-26: <https://orca.dev/docs/cli/reference>,
//! <https://orca.dev/docs/cli/orchestration>, and — because neither page shows
//! any output — the CLI's own source at `stablyai/orca@83014a0b56e1`:
//! `src/cli/orchestration/tasks/task-agent-views.ts` (the `task-list` row),
//! `src/cli/orchestration/tasks/task-handlers.ts` (the status words) and
//! `src/cli/orchestration/agent-view.ts` (the `--json` envelope). Product:
//! Orca by Stably AI. Every name below is quoted from one of them.
//!
//! - Runtime: "The orca CLI talks to a running Orca runtime"; orchestration
//!   is "Experimental": "Enable orchestration under Settings → Experimental
//!   before using these commands". Without either, `status` fails.
//! - Status: `orca orchestration task-list --json` (docs, "Recovery"). The
//!   envelope is `JSON.stringify({ ok, result: … })`; `result.tasks` rows are
//!   `{ id, status, title, deps, parentId, dispatchId, assignee }`, where
//!   `title` is `task.display_name ?? task.task_title ?? task.spec.slice(0,
//!   60)`. `ok: false` is a failure.
//! - Statuses: "a work item with a spec, dependencies, and status: pending,
//!   ready, dispatched, completed, failed, or blocked" (the source's
//!   `TASK_STATUS_VALUES` lists the same six). `dispatched` is `Working`,
//!   `completed` is `Done`, `failed` is `Blocked(Failed)`. `blocked` is
//!   `Idle`: a task is blocked by "a coordinator-owned question" (a decision
//!   gate, answered with `gate-resolve --id <gateId>`, an id the task row does
//!   not carry) or by hand (`task-update --status blocked`), and neither is
//!   something the deck can answer. `pending`, `ready`, and any word orca adds
//!   later are `Idle` too.
//! - Key: the task `id` ("Task IDs printed in terminals, such as task_...").
//! - Send: `orca orchestration send --to dispatch:<dispatchId> --subject "…"
//!   --body "…" --json` ("Later commands route by Dispatch ID"). The dispatch
//!   is the task row's `dispatchId`, read fresh from `task-list` when the
//!   order is sent; a task with none has no worker to tell, and the order is
//!   refused rather than sent somewhere else. `assignee` is not a target.
//!
//! **Not encoded, because nothing documents it:** `worker list`, a "Needs you"
//! queue, `worker-send`, `agent-context`, and any banner before the JSON.
//!
//! **Content.** `title` may be the first 60 characters of the spec, so it goes
//! through [`title`]; `deps`, `parentId` and `assignee` are never read.
use super::{json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::OsString;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "orca";
/// `task-list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// `send` gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// The subject of an order; the body is what the operator said.
const SUBJECT: &str = "From the deck";

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

/// `orca orchestration task-list --json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["orchestration", "task-list", "--json"]
        .map(OsString::from)
        .to_vec()
}

impl Orca {
    /// `orca orchestration send --to dispatch:<id> --subject … --body <text>
    /// --json`: the text is one argument, whatever it holds.
    #[must_use]
    pub fn order_argv(&self, dispatch: &str, text: &str) -> Vec<OsString> {
        vec![
            "orchestration".into(),
            "send".into(),
            "--to".into(),
            format!("dispatch:{dispatch}").into(),
            "--subject".into(),
            SUBJECT.into(),
            "--body".into(),
            text.into(),
            "--json".into(),
        ]
    }
}

/// One task's `status`.
fn state(word: &str) -> TaskState {
    match word {
        "dispatched" => TaskState::Working,
        "completed" => TaskState::Done,
        "failed" => TaskState::Blocked(Blocked::Failed),
        // `pending`, `ready`, `blocked` (see the module comment), and
        // whatever a later orca adds.
        _ => TaskState::Idle,
    }
}

/// The `result.tasks` rows of a `task-list --json` output.
fn rows(raw: &str) -> Result<Vec<Value>, String> {
    let value = json_payload(raw)?;
    if value.get("ok") == Some(&Value::Bool(false)) {
        return Err("backend_refused: orca reported ok:false".into());
    }
    value
        .get("result")
        .and_then(|r| r.get("tasks"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "malformed: no `result.tasks` array".to_string())
}

/// A task's `id`, when it is a usable key.
fn task_id(task: &Value) -> Option<&str> {
    task["id"].as_str().map(str::trim).filter(|id| safe_key(id))
}

/// `orca orchestration task-list --json` as tasks. A task with no usable
/// `id` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let mut tasks = Vec::new();
    for task in rows(raw)? {
        let Some(id) = task_id(&task) else {
            continue;
        };
        let Some(word) = task["status"].as_str() else {
            continue;
        };
        tasks.push(Task {
            key: id.to_string(),
            title: title(task["title"].as_str().unwrap_or_default(), id),
            state: state(word.trim()),
            activity: None,
        });
    }
    Ok(tasks)
}

/// The `dispatchId` of task `key` in a `task-list --json` output.
pub fn dispatch_of(raw: &str, key: &str) -> Result<String, String> {
    let rows = rows(raw)?;
    let task = rows
        .iter()
        .find(|t| task_id(t) == Some(key))
        .ok_or_else(|| "not_found: no such orca task".to_string())?;
    task["dispatchId"]
        .as_str()
        .map(str::trim)
        .filter(|d| safe_key(d) && !d.contains(' '))
        .map(str::to_string)
        .ok_or_else(|| "no_worker: the task has no dispatch to message".to_string())
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
        if !safe_key(key) || key.contains(' ') {
            return Err("bad_key: not an orca task id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // orca would read it as an option, not as the body.
        if text.starts_with('-') {
            return Err("bad_text: a message may not start with '-'".into());
        }
        let dispatch = dispatch_of(&run(PROGRAM, &list_argv(), LIST_TIMEOUT)?, key)?;
        run(PROGRAM, &self.order_argv(&dispatch, text), SEND_TIMEOUT).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{budget, plan, status_json, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;

    /// Doc- and source-derived (`TASK_LIST_VIEW`, 2026-11-26), not captured.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/orca.json");

    fn list(tasks: Vec<Value>) -> String {
        serde_json::json!({"ok": true, "result": {"tasks": tasks, "runId": "run_1"}}).to_string()
    }

    #[test]
    fn the_fixture_parses_into_the_documented_statuses() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("task_01", TaskState::Working),
                ("task_02", TaskState::Blocked(Blocked::Failed)),
                ("task_03", TaskState::Done),
                ("task_04", TaskState::Idle),
                ("task_05", TaskState::Idle),
                ("task_06", TaskState::Idle),
                ("task_07", TaskState::Working),
            ]
        );
        assert_eq!(tasks[0].title, "Billing audit");
        assert_eq!(
            tasks[6].title, "task_07",
            "a spec that reads as a command line is not a title"
        );
    }

    #[test]
    fn the_envelope_is_ok_and_result_tasks() {
        assert_eq!(parse(&list(vec![])).expect("empty"), []);
        assert!(parse(r#"{"ok":false,"result":{"tasks":[]}}"#)
            .is_err_and(|e| e.starts_with("backend_refused")));
        // The shapes the first version read are not what orca prints.
        assert!(parse(r#"{"workers":[{"id":"wk-1","state":"running"}],"needs_you":[]}"#).is_err());
        assert!(parse(r#"{"ok":true,"result":{"count":0}}"#).is_err());
    }

    #[test]
    fn each_documented_status_maps_and_the_rest_are_idle() {
        for (word, want) in [
            ("pending", TaskState::Idle),
            ("ready", TaskState::Idle),
            ("dispatched", TaskState::Working),
            ("completed", TaskState::Done),
            ("failed", TaskState::Blocked(Blocked::Failed)),
            ("blocked", TaskState::Idle),
            ("running", TaskState::Idle),
            ("needs_you", TaskState::Idle),
        ] {
            let tasks = parse(&list(vec![
                serde_json::json!({"id": "task_1", "status": word, "title": "x"}),
            ]))
            .expect(word);
            assert_eq!(tasks[0].state, want, "{word}");
        }
    }

    #[test]
    fn the_order_goes_to_the_dispatch_never_the_assignee() {
        assert_eq!(
            dispatch_of(FIXTURE, "task_01").expect("dispatched"),
            "disp_a1"
        );
        assert!(
            dispatch_of(FIXTURE, "task_05").is_err_and(|e| e.starts_with("no_worker")),
            "ready: no dispatch"
        );
        assert!(dispatch_of(FIXTURE, "task_99").is_err_and(|e| e.starts_with("not_found")));
        // An assignee handle is never used as the address.
        let raw = list(vec![
            serde_json::json!({"id": "task_1", "status": "dispatched", "assignee": "term_abc"}),
        ]);
        assert!(dispatch_of(&raw, "task_1").is_err_and(|e| e.starts_with("no_worker")));
        let argv = Orca.order_argv("disp_a1", "ship it; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "orchestration",
                "send",
                "--to",
                "dispatch:disp_a1",
                "--subject",
                "From the deck",
                "--body",
                "ship it; rm -rf / && echo $(id)",
                "--json"
            ]
        );
        assert_eq!(
            list_argv(),
            ["orchestration", "task-list", "--json"].map(OsString::from)
        );
    }

    #[test]
    fn nothing_from_a_spec_path_or_token_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&Orca, &tasks, budget(1)).to_string();
        for leak in [
            "rm -rf",
            "/tmp",
            "~/",
            "ghp_",
            "GITHUB_TOKEN",
            "disp_",
            "term_",
            "run_",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&Orca, &tasks, budget(1));
        assert_eq!(deck[0].key, "task_02");
        assert_eq!(deck[0].state, AgentState::Error);
    }

    #[test]
    fn a_forty_task_run_fits_the_deck() {
        let tasks: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "id": format!("task_{i}"),
                    "status": if i % 4 == 0 { "dispatched" } else { "failed" },
                    "title": format!("npm test --shard {i}"),
                    "dispatchId": format!("disp_{i}"),
                })
            })
            .collect();
        let tasks = parse(&list(tasks)).expect("run");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&Orca, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "orca: runtime not running",
            "{\"ok\":true,\"result\":{\"tasks\":3}}",
            "\"text\"",
            "{\"ok\":true,",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Rows orca may grow: missing fields are skipped, not fatal.
        let tasks = parse(&list(vec![
            serde_json::json!({"status": "failed"}),
            serde_json::json!({"id": "../x", "status": "completed"}),
            serde_json::json!({"id": "task_2"}),
            serde_json::json!({"id": "task_3", "status": "thinking", "title": 7}),
        ]))
        .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
        assert_eq!(tasks[0].title, "task_3");
    }

    #[test]
    fn an_order_orca_would_misread_is_refused_before_it_runs() {
        assert!(Orca
            .send_order("-x", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Orca
            .send_order("task 1", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Orca
            .send_order("task_1", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Orca
            .send_order("task_1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }
}
