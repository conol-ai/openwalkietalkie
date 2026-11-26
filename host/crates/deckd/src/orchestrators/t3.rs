//! `t3` on the deck (`docs/orchestrator-adapters.md`, "t3").
//!
//! **Source-derived, not verified against a running instance.** T3 Code
//! (`pingdotgg/t3code`, the `t3` bin of `apps/server`, 0.1.12), read
//! 2026-11-26 at `147028a26595`: `apps/server/src/cli/thread.ts` (the verbs)
//! and `packages/contracts/src/orchestratorMcp.ts` + `orchestrationV2.ts`
//! (the shapes). Every name below is quoted from them.
//!
//! - `t3 thread list --json --project <dir>`: "List a project's threads,
//!   newest first." `--project` is "Project id or a path inside the project.
//!   Defaults to this directory." — deckd's directory is not the user's
//!   project, so it is always passed, from [`PROJECT_ENV`]; without it
//!   `status` fails with `no_project`. `--json` prints the bare array of
//!   `OrchestratorMcpThreadListItem` (`printJson(threads)`); the CLI already
//!   drops `relationshipToParent === "subagent"` rows. It talks to "the
//!   running server".
//! - Per thread: `threadId` (the key and the order's address), `title`,
//!   `status`, `pendingBackgroundTasks` ("Background work still open on the
//!   thread"; each with a `kind` of `subagent|command|monitor|background_task`)
//!   and `blockedOnThreadWait`. Both are "Absent from linked environments that
//!   predate it", so absent means "nothing open", not an error.
//! - `status` is `"idle"` or an `OrchestrationV2RunStatus`: `preparing`,
//!   `queued`, `starting`, `running`, `waiting`, `completed`, `interrupted`,
//!   `failed`, `cancelled`, `rolled_back`. `failed` is `Blocked(Failed)`; the
//!   five in-flight words are `Working`; `interrupted`/`cancelled`/
//!   `rolled_back` are over but not done: `Idle`, with the word as activity.
//! - `t3 thread send <thread> <message>`: both positional ("Send a message to
//!   a thread. A busy thread gets it as its next turn; nothing is
//!   interrupted."). A message of `-` is read from stdin, so text starting
//!   with `-` is refused.
//!
//! **Completion is not trusted** (upstream #25585, #31087, #5518: t3 says
//! done at intermediate stops and when a delegated child finishes). A
//! `completed` or `idle` thread with any `pendingBackgroundTasks` or with
//! `blockedOnThreadWait` is `Working`; only a thread with neither is `Done`.
//!
//! **No approval card.** The list item carries no approval signal; `thread
//! show --json` has `pendingRequestCount`, but that is one more CLI call per
//! thread per poll, so it is left out rather than faked.
//!
//! **Content.** `title` goes through [`title`]. A background task's
//! `description` may be a command line and is never read; only the count is.
use super::{activity, json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "t3";
/// The t3 project to list: an id or a path inside it (deckd's convention).
pub const PROJECT_ENV: &str = "DECKD_T3_PROJECT";
/// `thread list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// `thread send` gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

pub struct T3;

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    Some(Box::new(T3))
}

/// `t3 thread list --json --project <project>`.
#[must_use]
pub fn list_argv(project: &OsStr) -> Vec<OsString> {
    vec![
        "thread".into(),
        "list".into(),
        "--json".into(),
        "--project".into(),
        project.to_os_string(),
    ]
}

impl T3 {
    /// `t3 thread send <thread> <message>`: the text is one argument,
    /// whatever it holds.
    #[must_use]
    pub fn order_argv(&self, key: &str, text: &str) -> Vec<OsString> {
        vec!["thread".into(), "send".into(), key.into(), text.into()]
    }
}

/// A thread id as a key: non-empty, no spaces, nothing an argv would misread.
fn is_thread_id(s: &str) -> bool {
    safe_key(s) && !s.contains(' ')
}

/// How much is still open on the thread: background tasks, plus one for a
/// wait on another thread.
fn open_work(thread: &Value) -> usize {
    let tasks = thread["pendingBackgroundTasks"]
        .as_array()
        .map_or(0, Vec::len);
    tasks + usize::from(thread["blockedOnThreadWait"].as_bool() == Some(true))
}

/// A thread's state, and what it is doing when its status does not say.
fn state(thread: &Value) -> (TaskState, Option<String>) {
    let word = thread["status"].as_str().map_or("", str::trim);
    match word {
        "failed" => (TaskState::Blocked(Blocked::Failed), None),
        "preparing" | "queued" | "starting" | "running" | "waiting" => (TaskState::Working, None),
        "interrupted" | "cancelled" | "rolled_back" => (TaskState::Idle, activity(word)),
        // `completed`, `idle`: only settled when nothing is still open.
        "completed" | "idle" => match open_work(thread) {
            0 if word == "completed" => (TaskState::Done, None),
            0 => (TaskState::Idle, None),
            n => {
                let what = if n == 1 { "task" } else { "tasks" };
                (
                    TaskState::Working,
                    activity(&format!("{n} background {what} open")),
                )
            }
        },
        // Whatever a later t3 adds.
        _ => (TaskState::Idle, None),
    }
}

/// The threads of `t3 thread list --json` as tasks. A thread with no usable
/// `threadId` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let threads = value
        .as_array()
        .ok_or_else(|| "malformed: not an array of threads".to_string())?;
    let mut tasks = Vec::new();
    for thread in threads {
        let Some(id) = thread["threadId"]
            .as_str()
            .map(str::trim)
            .filter(|id| is_thread_id(id))
        else {
            continue;
        };
        if thread["status"].as_str().is_none() {
            continue;
        }
        let (state, activity) = state(thread);
        tasks.push(Task {
            key: id.to_string(),
            title: title(thread["title"].as_str().unwrap_or_default(), id),
            state,
            activity,
        });
    }
    Ok(tasks)
}

impl Orchestrator for T3 {
    fn name(&self) -> &'static str {
        "t3"
    }

    fn label(&self) -> &'static str {
        "T3"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        let project = std::env::var_os(PROJECT_ENV)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                format!("no_project: set {PROJECT_ENV} to a t3 project id or a path inside it")
            })?;
        parse(&run(PROGRAM, &list_argv(&project), LIST_TIMEOUT)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        if !is_thread_id(key) {
            return Err("bad_key: not a t3 thread id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // t3 would read `-` as stdin and `-x` as an option.
        if text.starts_with('-') {
            return Err("bad_text: a message may not start with '-'".into());
        }
        run(PROGRAM, &self.order_argv(key, text), SEND_TIMEOUT).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{budget, plan, status_json, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;
    use serde_json::json;

    /// Source-derived (`OrchestratorMcpThreadListItem`, 2026-11-26), not captured.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/t3.json");

    fn one(thread: Value) -> Task {
        let mut tasks = parse(&json!([thread]).to_string()).expect("thread");
        assert_eq!(tasks.len(), 1);
        tasks.remove(0)
    }

    #[test]
    fn the_fixture_parses_into_the_source_statuses() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("thr_review", TaskState::Working),
                ("thr_migrate", TaskState::Working),
                ("thr_fanout", TaskState::Working),
                ("thr_release", TaskState::Done),
                ("thr_build", TaskState::Blocked(Blocked::Failed)),
                ("thr_scratch", TaskState::Idle),
                ("thr_spike", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[1].title, "Migrate the deck schema");
        assert_eq!(
            tasks[1].activity.as_deref(),
            Some("2 background tasks open")
        );
        assert_eq!(tasks[3].title, "Release notes [path]");
        assert_eq!(tasks[4].title, "thr_build", "a command line is left out");
        assert_eq!(tasks[6].activity.as_deref(), Some("rolled_back"));
    }

    // #25585: t3 says done at every intermediate stop of a multi-stage
    // background job. A `completed` thread with background work open is not.
    #[test]
    fn a_completed_thread_with_pending_background_tasks_is_working_not_done() {
        let task = one(json!({
            "threadId": "t1", "title": "Stage one of three", "status": "completed",
            "pendingBackgroundTasks": [{"taskId": "bg1", "kind": "background_task"}],
        }));
        assert_eq!(task.state, TaskState::Working);
        assert_eq!(task.activity.as_deref(), Some("1 background task open"));
    }

    // #31087, #5518: a delegated child finishes, t3 marks the parent done,
    // and the parent is about to continue.
    #[test]
    fn child_finished_parent_still_working_is_not_reported_done() {
        let subagent = json!({"taskId": "bg2", "kind": "subagent", "childThreadId": "thr_child"});
        let task = one(json!({
            "threadId": "parent", "status": "completed", "pendingBackgroundTasks": [subagent],
        }));
        assert_eq!(task.state, TaskState::Working);
        let task =
            one(json!({"threadId": "parent", "status": "completed", "blockedOnThreadWait": true}));
        assert_eq!(task.state, TaskState::Working);
        let task =
            one(json!({"threadId": "parent", "status": "idle", "blockedOnThreadWait": true}));
        assert_eq!(task.state, TaskState::Working);
    }

    #[test]
    fn only_a_settled_thread_is_done() {
        let task = one(json!({
            "threadId": "t1", "status": "completed",
            "pendingBackgroundTasks": [], "blockedOnThreadWait": false,
        }));
        assert_eq!(task.state, TaskState::Done);
        // Fields "absent from linked environments that predate it" are not open work.
        assert_eq!(
            one(json!({"threadId": "t1", "status": "completed"})).state,
            TaskState::Done
        );
    }

    #[test]
    fn each_status_word_maps() {
        for (word, want) in [
            ("idle", TaskState::Idle),
            ("preparing", TaskState::Working),
            ("queued", TaskState::Working),
            ("starting", TaskState::Working),
            ("running", TaskState::Working),
            ("waiting", TaskState::Working),
            ("completed", TaskState::Done),
            ("interrupted", TaskState::Idle),
            ("failed", TaskState::Blocked(Blocked::Failed)),
            ("cancelled", TaskState::Idle),
            ("rolled_back", TaskState::Idle),
            ("awaiting_approval", TaskState::Idle),
        ] {
            assert_eq!(
                one(json!({"threadId": "t", "status": word})).state,
                want,
                "{word}"
            );
        }
        for over in ["interrupted", "cancelled", "rolled_back"] {
            assert_eq!(
                one(json!({"threadId": "t", "status": over}))
                    .activity
                    .as_deref(),
                Some(over)
            );
        }
    }

    #[test]
    fn the_key_is_thread_id_and_the_invented_fields_are_gone() {
        // `id`, `children`, `hasPendingChildRuns` and `pendingApproval` are not t3's.
        let raw = json!([
            {"id": "t1", "status": "running"},
            {"threadId": "t2", "status": "completed", "hasPendingChildRuns": true,
             "children": [{"id": "c", "status": "running"}], "pendingApproval": {"command": "rm"}},
        ])
        .to_string();
        let tasks = parse(&raw).expect("list");
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            (tasks[0].key.as_str(), tasks[0].state),
            ("t2", TaskState::Done)
        );
        // The envelope is a bare array; the object form the first version read is not.
        assert!(parse(r#"{"threads":[]}"#).is_err());
        // The parent is never the address.
        let task = one(
            json!({"threadId": "thr_child", "parentThreadId": "thr_parent", "status": "running"}),
        );
        assert_eq!(task.key, "thr_child");
    }

    #[test]
    fn nothing_from_a_title_or_a_background_task_reaches_the_json() {
        let mut tasks = parse(FIXTURE).expect("fixture");
        tasks.push(one(json!({
            "threadId": "leaky", "status": "completed",
            "title": "export GITHUB_TOKEN=ghp_0123456789abcdefABCDEF",
            "pendingBackgroundTasks": [{"taskId": "bg", "kind": "command", "description": "rm -rf /tmp/x"}],
        })));
        let out = status_json(&T3, &tasks, budget(1)).to_string();
        for leak in [
            "rm -rf",
            "/tmp",
            "~/",
            "ghp_",
            "GITHUB_TOKEN",
            "npm",
            "proj_",
            "/home",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&T3, &tasks, budget(1));
        assert_eq!(deck[0].key, "thr_build");
        assert_eq!(
            (deck[0].state, deck[0].card.clone()),
            (AgentState::Error, None)
        );
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse("[]").expect("array"), []);
        assert!(plan(&T3, &[], budget(1)).is_empty());
    }

    #[test]
    fn a_forty_thread_fleet_fits_the_deck() {
        let threads: Vec<Value> = (0..40)
            .map(|i| {
                json!({
                    "threadId": format!("thr_{i}"),
                    "title": format!("cargo test --shard {i}"),
                    "status": if i % 4 == 0 { "running" } else { "failed" },
                })
            })
            .collect();
        let tasks = parse(&Value::Array(threads).to_string()).expect("fleet");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&T3, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "t3: server not running",
            "\"text\"",
            "[{",
            "42",
            "{\"threadId\":\"t\"}",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Threads t3 may grow: missing fields are skipped, not fatal.
        let raw = r#"[{"status":"running"},{"threadId":"a b","status":"running"},{"threadId":"ok"},
                      {"threadId":"t","status":"thinking","pendingBackgroundTasks":3}]"#;
        let tasks = parse(raw).expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn the_argv_is_what_thread_ts_declares() {
        assert_eq!(
            list_argv(OsStr::new("/srv/app")),
            ["thread", "list", "--json", "--project", "/srv/app"].map(OsString::from)
        );
        let argv = T3.order_argv("thr_docs", "ship it; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "thread",
                "send",
                "thr_docs",
                "ship it; rm -rf / && echo $(id)"
            ]
        );
    }

    #[test]
    fn an_order_t3_would_misread_is_refused_before_it_runs() {
        for key in ["-x", "a b", "../t", ""] {
            assert!(
                T3.send_order(key, "hi")
                    .is_err_and(|e| e.starts_with("bad_key")),
                "{key:?}"
            );
        }
        assert!(T3
            .send_order("t1", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(T3
            .send_order("t1", "-")
            .is_err_and(|e| e.starts_with("bad_text")));
        assert!(T3
            .send_order("t1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }
}
