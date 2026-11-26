//! `t3` on the deck (`docs/orchestrator-adapters.md`, "t3").
//!
//! What the adapter relies on, and nothing else:
//!
//! - `t3 thread list --json`: threads, each with `id` (the address of an
//!   order), `title`, `status`, `hasPendingChildRuns`, `children` (each a
//!   thread of the same shape, or a bare id) and, when it is waiting on the
//!   operator, a `pendingApproval` (also read as `pendingApprovals` or
//!   `pendingPermission`). The list is either the top-level array or the
//!   object's `threads` array.
//! - `t3 thread send --thread <id> --text <text> --json`: one order, as argv.
//!
//! **Completion is not trusted.** t3 says a thread is done at every
//! intermediate stop of a multi-stage job (upstream #25585) and when a
//! child finishes while the parent carries on (#31087, #5518). So its
//! `status` is the *last* thing read, never the first:
//!
//! 1. a pending approval, on the thread or on any child under it, is
//!    `Blocked(Approval)`;
//! 2. a `failed` thread is `Blocked(Failed)`;
//! 3. `hasPendingChildRuns`, or any child that is not finished (its own
//!    status, its own pending children, or a child t3 names but does not
//!    describe), is `Working`, whatever `status` says;
//! 4. only then does `status` count, and only then can a thread be `Done`.
//!
//! **Content.** A thread's `title` is usually the operator's first message,
//! so it goes through [`title`] (paths, tokens and command lines out) and
//! falls back to the id. What an approval asks for (`command`, `path`, …)
//! is never read: the card says "waiting for approval", nothing more.
//! Children are never tasks of their own and their titles are never read.
use super::{activity, json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::OsString;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "t3";
/// `thread list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// `thread send` gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// How deep a child tree is followed; deeper is "not finished".
const CHILD_DEPTH: usize = 8;
/// The fields that hold a request waiting on the operator.
const PENDING_FIELDS: [&str; 3] = ["pendingApproval", "pendingApprovals", "pendingPermission"];

pub struct T3;

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    Some(Box::new(T3))
}

/// `t3 thread list --json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["thread", "list", "--json"].map(OsString::from).to_vec()
}

impl T3 {
    /// `t3 thread send --thread <id> --text <text> --json`: the text is one
    /// argument, whatever it holds.
    #[must_use]
    pub fn order_argv(&self, key: &str, text: &str) -> Vec<OsString> {
        vec![
            "thread".into(),
            "send".into(),
            "--thread".into(),
            key.into(),
            "--text".into(),
            text.into(),
            "--json".into(),
        ]
    }
}

/// What a thread's own `status` word claims. `Done` here is a claim only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    Working,
    Done,
    Failed,
    Approval,
    Idle,
}

fn claim(word: &str) -> Claim {
    match word.trim().to_ascii_lowercase().as_str() {
        "running" | "working" | "in_progress" | "streaming" | "starting" | "queued" | "pending"
        | "busy" => Claim::Working,
        "completed" | "complete" | "done" | "finished" | "succeeded" | "success" => Claim::Done,
        "failed" | "error" | "errored" => Claim::Failed,
        "awaiting_approval" | "needs_approval" | "pending_approval" => Claim::Approval,
        // `idle`, `cancelled`, and whatever a later t3 adds.
        _ => Claim::Idle,
    }
}

/// Does `thread` itself hold a request waiting on the operator?
fn asks_approval(thread: &Value) -> bool {
    let own = PENDING_FIELDS.iter().any(|f| match thread.get(*f) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::String(s)) => !s.trim().is_empty(),
        _ => false,
    });
    own || thread["status"].as_str().map(claim) == Some(Claim::Approval)
}

fn children(thread: &Value) -> &[Value] {
    thread["children"].as_array().map_or(&[], Vec::as_slice)
}

/// Is an approval waiting anywhere in `thread`'s tree?
fn approval_in_tree(thread: &Value, depth: usize) -> bool {
    asks_approval(thread)
        || (depth < CHILD_DEPTH
            && children(thread)
                .iter()
                .any(|c| approval_in_tree(c, depth + 1)))
}

/// Is `child` still going? Only a child that says it finished, with no
/// pending runs and no open child of its own, is finished. A bare id, a
/// missing status, or a tree too deep to follow is open.
fn child_open(child: &Value, depth: usize) -> bool {
    if depth >= CHILD_DEPTH || !child.is_object() {
        return true;
    }
    let settled = matches!(
        child["status"].as_str().map(claim),
        Some(Claim::Done | Claim::Failed)
    ) || child["status"].as_str().map(str::trim) == Some("cancelled");
    !settled || pending_children(child) > 0 || open_children(child, depth) > 0
}

fn open_children(thread: &Value, depth: usize) -> usize {
    children(thread)
        .iter()
        .filter(|c| child_open(c, depth + 1))
        .count()
}

/// `hasPendingChildRuns`, as a count of 1 when true.
fn pending_children(thread: &Value) -> usize {
    usize::from(thread["hasPendingChildRuns"].as_bool() == Some(true))
}

/// A thread's state, and what it is doing when the children say more than
/// its status does. See the module note for the order.
fn state(thread: &Value) -> (TaskState, Option<String>) {
    if approval_in_tree(thread, 0) {
        return (TaskState::Blocked(Blocked::Approval), None);
    }
    let claimed = thread["status"].as_str().map_or(Claim::Idle, claim);
    if claimed == Claim::Failed {
        return (TaskState::Blocked(Blocked::Failed), None);
    }
    let open = open_children(thread, 0);
    if open > 0 {
        let word = if open == 1 { "child run" } else { "child runs" };
        return (TaskState::Working, activity(&format!("{open} {word} open")));
    }
    if pending_children(thread) > 0 {
        return (TaskState::Working, activity("child runs pending"));
    }
    let state = match claimed {
        Claim::Working => TaskState::Working,
        Claim::Done => TaskState::Done,
        // Handled above; listed so a new claim is a compile error.
        Claim::Failed => TaskState::Blocked(Blocked::Failed),
        Claim::Approval => TaskState::Blocked(Blocked::Approval),
        Claim::Idle => TaskState::Idle,
    };
    (state, None)
}

/// The threads of `t3 thread list --json` as tasks. A thread with no usable
/// `id` cannot be addressed and is left out; its children are never tasks.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let threads = match &value {
        Value::Array(threads) => threads,
        Value::Object(map) => map
            .get("threads")
            .and_then(Value::as_array)
            .ok_or_else(|| "malformed: no `threads` array".to_string())?,
        _ => return Err("malformed: not a list of threads".into()),
    };
    let mut tasks = Vec::new();
    for thread in threads {
        let Some(id) = thread["id"]
            .as_str()
            .map(str::trim)
            .filter(|id| safe_key(id))
        else {
            continue;
        };
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

/// What `thread send --json` printed: an `error`, or `ok: false`, is a
/// refusal even when t3 exits 0. Its words are never quoted (they may hold
/// paths). Output that is not JSON is taken as sent: the exit code said so.
fn send_result(raw: &str) -> Result<(), String> {
    let Ok(value) = json_payload(raw) else {
        return Ok(());
    };
    let refused = value["ok"].as_bool() == Some(false)
        || value
            .get("error")
            .is_some_and(|e| !e.is_null() && e != false);
    if refused {
        return Err("backend_refused: t3 did not take the message".into());
    }
    Ok(())
}

impl Orchestrator for T3 {
    fn name(&self) -> &'static str {
        "t3"
    }

    fn label(&self) -> &'static str {
        "T3"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        parse(&run(PROGRAM, &list_argv(), LIST_TIMEOUT)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        if !safe_key(key) {
            return Err("bad_key: not a t3 thread id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // t3 would read it as an option, not as `--text`'s value.
        if text.starts_with('-') {
            return Err("bad_text: a message may not start with '-'".into());
        }
        send_result(&run(PROGRAM, &self.order_argv(key, text), SEND_TIMEOUT)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{budget, plan, status_json, Card, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;
    use serde_json::json;

    /// Written from the documented shape, not captured from a live t3.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/t3.json");

    fn one(thread: Value) -> Task {
        let raw = json!({ "threads": [thread] }).to_string();
        let mut tasks = parse(&raw).expect("thread");
        assert_eq!(tasks.len(), 1);
        tasks.remove(0)
    }

    #[test]
    fn the_fixture_parses_into_the_documented_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("thr_review", TaskState::Working),
                ("thr_migrate", TaskState::Working),
                ("thr_docs", TaskState::Blocked(Blocked::Approval)),
                ("thr_release", TaskState::Done),
                ("thr_build", TaskState::Blocked(Blocked::Failed)),
                ("thr_scratch", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[1].title, "Migrate the deck schema");
        assert_eq!(tasks[1].activity.as_deref(), Some("1 child run open"));
        assert_eq!(tasks[3].title, "Release notes [path]");
        assert_eq!(tasks[4].title, "thr_build", "a command line is left out");
        assert_eq!(tasks[5].title, "thr_scratch", "a token is left out");
    }

    // Rule: `hasPendingChildRuns` beats a `completed` status (#25585).
    #[test]
    fn a_completed_thread_with_pending_child_runs_is_working_not_done() {
        let task = one(json!({
            "id": "t1", "title": "Stage one of three", "status": "completed",
            "hasPendingChildRuns": true, "children": [],
        }));
        assert_eq!(task.state, TaskState::Working);
        assert_eq!(task.activity.as_deref(), Some("child runs pending"));
    }

    // Rule: the case upstream gets wrong (#31087, #5518): one child has
    // finished, t3 marks the parent done, and the parent is still going.
    #[test]
    fn child_finished_parent_still_working_is_not_reported_done() {
        let task = one(json!({
            "id": "parent", "title": "Fan out the tests", "status": "completed",
            "hasPendingChildRuns": false,
            "children": [
                { "id": "c1", "status": "completed" },
                { "id": "c2", "status": "running" },
            ],
        }));
        assert_eq!(task.state, TaskState::Working);
        // The same with the flag set, and with the open child two deep.
        let task = one(json!({
            "id": "parent", "status": "done", "hasPendingChildRuns": true,
            "children": [{ "id": "c1", "status": "completed" }],
        }));
        assert_eq!(task.state, TaskState::Working);
        let task = one(json!({
            "id": "parent", "status": "completed",
            "children": [{ "id": "c1", "status": "completed",
                "children": [{ "id": "g1", "status": "running" }] }],
        }));
        assert_eq!(task.state, TaskState::Working);
        let task = one(json!({
            "id": "parent", "status": "completed",
            "children": [{ "id": "c1", "status": "completed", "hasPendingChildRuns": true }],
        }));
        assert_eq!(task.state, TaskState::Working);
    }

    // Rule: a child t3 names but does not describe is not proof of anything.
    #[test]
    fn a_child_with_no_status_keeps_the_parent_working() {
        for child in [
            json!("c1"),
            json!({ "id": "c1" }),
            json!({ "id": "c1", "status": "thinking" }),
        ] {
            let task = one(json!({ "id": "p", "status": "completed", "children": [child] }));
            assert_eq!(task.state, TaskState::Working, "{child}");
        }
    }

    // Rule: a pending approval is the operator's, whatever else is true.
    #[test]
    fn a_pending_approval_is_blocked_on_approval() {
        for (field, value) in [
            ("pendingApproval", json!({ "command": "rm -rf /" })),
            ("pendingApprovals", json!([{ "tool": "bash" }])),
            ("pendingPermission", json!(true)),
        ] {
            let mut thread =
                json!({ "id": "t", "status": "completed", "hasPendingChildRuns": true });
            thread[field] = value;
            assert_eq!(
                one(thread).state,
                TaskState::Blocked(Blocked::Approval),
                "{field}"
            );
        }
        // On a child: the job is waiting on the operator.
        let task = one(json!({
            "id": "t", "status": "running",
            "children": [{ "id": "c", "status": "running", "pendingApproval": { "tool": "edit" } }],
        }));
        assert_eq!(task.state, TaskState::Blocked(Blocked::Approval));
        // An empty or null request is not one.
        for value in [json!(null), json!(false), json!([]), json!({}), json!("")] {
            let task = one(json!({ "id": "t", "status": "running", "pendingApproval": value }));
            assert_eq!(task.state, TaskState::Working);
        }
    }

    // Rule: only no pending children and no pending approval is `Done`.
    #[test]
    fn only_a_settled_thread_is_done() {
        let task = one(json!({
            "id": "t", "status": "completed", "hasPendingChildRuns": false,
            "children": [
                { "id": "a", "status": "completed" },
                { "id": "b", "status": "failed" },
                { "id": "c", "status": "cancelled" },
            ],
        }));
        assert_eq!(task.state, TaskState::Done);
        assert_eq!(task.activity, None);
        assert_eq!(
            one(json!({ "id": "t", "status": "completed" })).state,
            TaskState::Done
        );
    }

    #[test]
    fn nothing_from_a_title_or_an_approval_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&T3, &tasks, budget(1)).to_string();
        for leak in [
            "cargo",
            "npm",
            "rm -rf",
            "/tmp",
            "~/",
            "/home",
            "sk-live",
            "OPENAI",
            "CHANGELOG",
            "command",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&T3, &tasks, budget(1));
        assert_eq!(deck.len(), 5, "idle is absent");
        assert_eq!(deck[0].key, "thr_docs");
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "Docs pass: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse("[]").expect("array"), []);
        assert_eq!(parse(r#"{"threads":[]}"#).expect("object"), []);
        assert!(plan(&T3, &[], budget(1)).is_empty());
    }

    #[test]
    fn a_forty_thread_fleet_fits_the_deck() {
        let threads: Vec<Value> = (0..40)
            .map(|i| {
                let mut t = json!({
                    "id": format!("thr_{i}"),
                    "title": format!("npm test --shard {i}"),
                    "status": "completed",
                    "hasPendingChildRuns": i % 4 == 0,
                });
                if i % 4 != 0 {
                    t["pendingApproval"] = json!({ "command": "git push --force" });
                }
                t
            })
            .collect();
        let raw = json!({ "threads": threads }).to_string();
        let tasks = parse(&raw).expect("fleet");
        assert_eq!(tasks.len(), 40);
        assert!(tasks.iter().all(|t| t.state != TaskState::Done));
        for configured in [1, 2, 5] {
            let deck = plan(&T3, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
            let summary = deck.last().expect("summary");
            assert_eq!(summary.state, AgentState::Question);
            assert!(summary.title.ends_with("waiting: T3"), "{}", summary.title);
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "t3: not running",
            "{\"threads\": 3}",
            "\"text\"",
            "{\"threads\":[",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Threads t3 may grow: missing fields are skipped, not fatal.
        let tasks = parse(
            r#"[{"status":"running"},{"id":"../x","status":"done"},{"id":"ok"},{"id":"a","children":7,"hasPendingChildRuns":"yes"}]"#,
        )
        .expect("partial");
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|t| t.state == TaskState::Idle));
        // A tree deeper than we follow is open, not a stack overflow.
        let mut deep = json!({ "id": "leaf", "status": "completed" });
        for i in 0..40 {
            deep = json!({ "id": format!("n{i}"), "status": "completed", "children": [deep] });
        }
        let raw = json!([deep]).to_string();
        assert_eq!(parse(&raw).expect("deep")[0].state, TaskState::Working);
    }

    #[test]
    fn a_banner_before_the_json_is_skipped() {
        let raw = format!("t3 v0.9 (update available)\n{FIXTURE}");
        assert_eq!(parse(&raw).expect("banner").len(), 6);
    }

    #[test]
    fn the_order_is_argv_with_the_text_as_one_argument() {
        let argv = T3.order_argv("thr_docs", "ship it; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "thread",
                "send",
                "--thread",
                "thr_docs",
                "--text",
                "ship it; rm -rf / && echo $(id)",
                "--json"
            ]
        );
        assert_eq!(
            list_argv(),
            ["thread", "list", "--json"].map(OsString::from)
        );
    }

    #[test]
    fn an_order_t3_would_misread_is_refused_before_it_runs() {
        assert!(T3
            .send_order("-x", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(T3
            .send_order("thr_docs", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(T3
            .send_order("thr_docs", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }

    #[test]
    fn a_send_t3_refuses_is_an_error_without_its_words() {
        assert!(send_result(r#"{"ok":true,"messageId":"m1"}"#).is_ok());
        assert!(send_result("sent").is_ok());
        for raw in [
            r#"{"ok":false}"#,
            r#"{"error":"thread /home/x/secret not found"}"#,
        ] {
            let err = send_result(raw).expect_err(raw);
            assert!(err.starts_with("backend_refused"), "{err}");
            assert!(!err.contains("/home"), "{err}");
        }
    }
}
