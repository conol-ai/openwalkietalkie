//! `multica` on the deck (`docs/orchestrator-adapters.md`, "multica").
//!
//! What the adapter relies on, and nothing else (documented, not verified
//! against a real installation):
//!
//! - `multica issue list --output json`: issues, each with `key` (the
//!   address of an order), `title`, `status`, `labels`, the assigned agent
//!   and the agent's `run` (`status`, `stage`). The list is either the
//!   top-level array or the object's `issues` array.
//! - `multica issue comment --issue <KEY> --body <text>`: one order, as argv.
//! - Exit codes 3 (auth), 4 (not found) and 5 (validation) are stable; they
//!   become the order result's reason word and are never retried.
//!
//! **The unit is the issue.** A `blocked` or `failed` label (or status) is
//! `Blocked(Failed)`; `review` is `Blocked(Approval)`; an issue whose run is
//! active is `Working`, with the run's stage as its activity; `done` is
//! `Done`; anything else, including any status multica adds later, is
//! `Idle`. A run's stage is only ever an activity, through
//! [`activity`](super::activity), so a stage that reads as a command line is
//! dropped rather than shown.
use super::{json_payload, run, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::OsString;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "multica";
/// `issue list` gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// `issue comment` gets this long.
const COMMENT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Multica;

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    Some(Box::new(Multica))
}

/// `multica issue list --output json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["issue", "list", "--output", "json"]
        .map(OsString::from)
        .to_vec()
}

impl Multica {
    /// `multica issue comment --issue <KEY> --body <text>`: the text is one
    /// argument, whatever it holds.
    #[must_use]
    pub fn order_argv(&self, key: &str, text: &str) -> Vec<OsString> {
        vec![
            "issue".into(),
            "comment".into(),
            "--issue".into(),
            key.into(),
            "--body".into(),
            text.into(),
        ]
    }
}

/// multica's stable exit codes as reason words; any other failure keeps the
/// reason [`run`] gave it.
#[must_use]
pub fn exit_reason(err: String) -> String {
    let code = err
        .strip_prefix("backend_refused:")
        .and_then(|rest| rest.rsplit_once("exited with "))
        .and_then(|(_, code)| code.trim().parse::<i32>().ok());
    let word = match code {
        Some(3) => "auth",
        Some(4) => "not_found",
        Some(5) => "validation",
        _ => return err,
    };
    format!("{word}: {PROGRAM} exited with {}", code.unwrap_or_default())
}

/// A label's name: a plain string or an object's `name`.
fn label_name(label: &Value) -> Option<&str> {
    label.as_str().or_else(|| label["name"].as_str())
}

/// Is the issue marked failed: a `blocked`/`failed` label or status.
fn failed(issue: &Value, status: &str) -> bool {
    let marks = |w: &str| {
        let w = w.trim().to_ascii_lowercase();
        w == "blocked" || w == "failed"
    };
    marks(status)
        || issue["labels"]
            .as_array()
            .is_some_and(|labels| labels.iter().filter_map(label_name).any(marks))
}

/// The agent's run, when it is active.
fn active_run(issue: &Value) -> Option<&Value> {
    let run = &issue["run"];
    let status = run["status"].as_str()?.trim().to_ascii_lowercase();
    matches!(status.as_str(), "running" | "in_progress" | "active").then_some(run)
}

/// One issue's state.
fn state(issue: &Value, status: &str) -> TaskState {
    if failed(issue, status) {
        return TaskState::Blocked(Blocked::Failed);
    }
    let status = status.trim().to_ascii_lowercase();
    if matches!(status.as_str(), "review" | "in_review") {
        return TaskState::Blocked(Blocked::Approval);
    }
    if active_run(issue).is_some() {
        return TaskState::Working;
    }
    match status.as_str() {
        "done" => TaskState::Done,
        // `backlog`, `todo`, `in_progress` with no run, `cancelled`, and
        // whatever a later multica adds.
        _ => TaskState::Idle,
    }
}

/// The issues of `multica issue list --output json` as tasks. An issue with
/// no usable `key` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let issues = match &value {
        Value::Array(issues) => issues,
        Value::Object(map) => map
            .get("issues")
            .and_then(Value::as_array)
            .ok_or_else(|| "malformed: no `issues` array".to_string())?,
        _ => return Err("malformed: not a list of issues".into()),
    };
    let mut tasks = Vec::new();
    for issue in issues {
        let Some(key) = issue["key"].as_str().map(str::trim).filter(|k| safe_key(k)) else {
            continue;
        };
        let Some(status) = issue["status"].as_str() else {
            continue;
        };
        let state = state(issue, status);
        let activity = match state {
            TaskState::Working => active_run(issue)
                .and_then(|run| run["stage"].as_str())
                .and_then(super::activity),
            _ => None,
        };
        tasks.push(Task {
            key: key.to_string(),
            title: title(issue["title"].as_str().unwrap_or_default(), key),
            state,
            activity,
        });
    }
    Ok(tasks)
}

impl Orchestrator for Multica {
    fn name(&self) -> &'static str {
        "multica"
    }

    fn label(&self) -> &'static str {
        "Multica"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        parse(&run(PROGRAM, &list_argv(), LIST_TIMEOUT).map_err(exit_reason)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        if !safe_key(key) {
            return Err("bad_key: not a multica issue key".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // multica would read it as an option, not as the body.
        if text.starts_with('-') {
            return Err("bad_text: a comment may not start with '-'".into());
        }
        // Exit codes are stable answers, not transient failures: no retry.
        run(PROGRAM, &self.order_argv(key, text), COMMENT_TIMEOUT)
            .map(|_| ())
            .map_err(exit_reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrators::{
        budget, plan, reason_word, status_json, Card, CARDS_MAX, DECK_SLOTS,
    };
    use deck_model::AgentState;

    /// Written from the documented shape, not captured from a live multica.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/multica.json");

    #[test]
    fn the_fixture_parses_into_the_documented_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("MUL-12", TaskState::Working),
                ("MUL-14", TaskState::Blocked(Blocked::Approval)),
                ("MUL-15", TaskState::Blocked(Blocked::Failed)),
                ("MUL-9", TaskState::Done),
                ("MUL-16", TaskState::Working),
                ("MUL-17", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "Refactor the billing API");
        assert_eq!(tasks[0].activity.as_deref(), Some("writing tests"));
        assert_eq!(tasks[1].activity, None, "only a working issue has one");
        assert_eq!(tasks[2].title, "Rotate [redacted] in [path]");
        assert_eq!(tasks[4].title, "MUL-16", "a command line is not a title");
        assert_eq!(tasks[4].activity, None, "a command line is not a stage");
    }

    #[test]
    fn nothing_from_a_command_path_or_token_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&Multica, &tasks, budget(1)).to_string();
        for leak in [
            "rm -rf",
            "/tmp",
            "~/",
            "ci.env",
            "ghp_",
            "GITHUB_TOKEN",
            "cargo",
            "npm",
            "run_8f",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&Multica, &tasks, budget(1));
        assert_eq!(deck.len(), 5, "idle is absent");
        assert_eq!(deck[0].key, "MUL-14");
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "Docs for the v2 webhooks: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
        assert_eq!(
            (deck[1].state, deck[1].card.clone()),
            (AgentState::Error, None)
        );
        let working = deck.iter().find(|e| e.key == "MUL-16").expect("MUL-16");
        assert_eq!(working.title, "working");
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse("[]").expect("array"), []);
        assert_eq!(parse(r#"{"issues":[]}"#).expect("object"), []);
        assert!(plan(&Multica, &[], budget(1)).is_empty());
    }

    #[test]
    fn a_forty_issue_fleet_fits_the_deck() {
        let issues: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "key": format!("MUL-{i}"),
                    "title": format!("Issue {i}"),
                    "status": if i % 4 == 0 { "in_progress" } else { "review" },
                    "assignee": { "type": "agent", "name": "claude" },
                    "run": { "status": "running", "stage": format!("cargo test --shard {i}") },
                })
            })
            .collect();
        let raw = serde_json::json!({ "issues": issues }).to_string();
        let tasks = parse(&raw).expect("fleet");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&Multica, &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
            let summary = deck.last().expect("summary");
            assert_eq!(summary.state, AgentState::Question);
            assert!(
                summary.title.ends_with("waiting: Multica"),
                "{}",
                summary.title
            );
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "multica: not logged in",
            "{\"issues\": 3}",
            "\"text\"",
            "{\"issues\":[",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Issues multica may grow: missing fields are skipped, not fatal.
        let tasks = parse(
            r#"[{"status":"review"},{"key":"../x","status":"done"},{"key":"ok"},
                {"key":"a","status":"thinking","labels":[3,{"x":1}],"run":"soon"}]"#,
        )
        .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn a_banner_before_the_json_is_skipped() {
        let raw = "multica v0.9.2 — a newer version is available\n[{\"key\":\"MUL-1\",\"title\":\"x\",\"status\":\"done\"}]";
        let tasks = parse(raw).expect("banner");
        assert_eq!(tasks[0].state, TaskState::Done);
    }

    #[test]
    fn the_order_is_argv_with_the_text_as_one_argument() {
        let argv = Multica.order_argv("MUL-14", "ship it; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "issue",
                "comment",
                "--issue",
                "MUL-14",
                "--body",
                "ship it; rm -rf / && echo $(id)"
            ]
        );
        assert_eq!(
            list_argv(),
            ["issue", "list", "--output", "json"].map(OsString::from)
        );
    }

    #[test]
    fn an_order_multica_would_misread_is_refused_before_it_runs() {
        assert!(Multica
            .send_order("-x", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Multica
            .send_order("MUL-1", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Multica
            .send_order("MUL-1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }

    #[test]
    fn stable_exit_codes_are_reason_words() {
        let at = |code: i32| exit_reason(format!("backend_refused: multica exited with {code}"));
        assert_eq!(reason_word(&at(3)), "auth");
        assert_eq!(reason_word(&at(4)), "not_found");
        assert_eq!(reason_word(&at(5)), "validation");
        assert_eq!(reason_word(&at(1)), "backend_refused");
        let other = "not_installed: multica is not on PATH".to_string();
        assert_eq!(exit_reason(other.clone()), other);
        assert_eq!(
            reason_word(&exit_reason(
                "backend_refused: multica exited with a signal".into()
            )),
            "backend_refused"
        );
    }

    /// The mapping reads the error [`run`] really produces, not a copy of it.
    #[cfg(unix)]
    #[test]
    fn the_exit_code_survives_the_real_runner() {
        let argv = ["-c", "exit 4"].map(OsString::from);
        let err = run("sh", &argv, Duration::from_secs(5)).expect_err("exit 4");
        assert_eq!(reason_word(&exit_reason(err)), "not_found");
    }
}
