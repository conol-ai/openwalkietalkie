//! `multica` on the deck (`docs/orchestrator-adapters.md`, "multica").
//!
//! **Doc-derived, not verified against a running instance.** Source:
//! <https://github.com/multica-ai/multica/blob/main/CLI_AND_DAEMON.md>, read
//! 2026-11-26 at commit `0c5ee1f07d87`. Every verb, flag and field below is
//! quoted from that file; anything it does not name is not read.
//!
//! - `multica issue list --limit 100 --output json`. "With `--output json`
//!   the envelope carries `total`, `limit`, `offset` and `has_more`"; the page
//!   is `issues` ("The number of issues in this page is `issues | length`").
//!   "`--limit` is the page size, 1 to 100": one page, board order.
//! - Per issue: `identifier` (the doc's `jq '.issues[] | {identifier, …}'`),
//!   `id` ("canonical UUIDs"), `title`, `status` (`--fields=id,title,status,
//!   priority`). The assignee is "`assignee_type`/`assignee_id` rather than a
//!   single `assignee` field" — never read, and never a target. `properties`
//!   is "a map from definition id to the stored value" — ids, not words, so
//!   it is not read either. The doc names no labels and no run or stage on
//!   the list, so there are none here.
//! - `status`: "Built-in statuses: `backlog`, `todo`, `in_progress`,
//!   `in_review`, `done`, `blocked`, `cancelled`. A workspace can define
//!   custom statuses on top of these". `in_review` is `Blocked(Approval)` (our
//!   reading: review is the human's turn); `in_progress` is `Working` (the
//!   name only — the list carries no run); `done` is `Done`. The doc does not
//!   say what `blocked` means for an operator — `--blocked` is "issues waiting
//!   for unfinished manual blockers or earlier stages", which is neither an
//!   approval nor a failure — so it is `Idle`, like `backlog`, `todo`,
//!   `cancelled` and every custom status.
//! - The address: "Table output shows a routable issue `KEY` such as
//!   `MUL-123`; copy that key into follow-up commands", so an order goes to
//!   `identifier`, else to `id` ("Ids may be issue keys or full UUIDs").
//! - `multica issue comment add <issue-id> --content "…"`: one order, as argv.
//! - Exit codes, verbatim from the doc's table: `2` network, `3`
//!   "authentication / authorization (HTTP 401, 403)", `4` "not found (HTTP
//!   404)", `5` "validation (HTTP 400, 422)". They become the order result's
//!   reason word and are never retried.
//!
//! **Content.** A `title` goes through [`title`], so one that reads as a
//! command line is shown by its key.
use super::{json_payload, run, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::Value;
use std::ffi::OsString;
use std::time::Duration;

/// The program, found on `PATH`.
pub const PROGRAM: &str = "multica";
/// `issue list` gets this long ("API requests use a default timeout of 30
/// seconds"; the deck polls, so it gives up sooner).
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// `issue comment add` gets this long.
const COMMENT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Multica;

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    Some(Box::new(Multica))
}

/// `multica issue list --limit 100 --output json`.
#[must_use]
pub fn list_argv() -> Vec<OsString> {
    ["issue", "list", "--limit", "100", "--output", "json"]
        .map(OsString::from)
        .to_vec()
}

impl Multica {
    /// `multica issue comment add <issue-id> --content <text>`: the text is
    /// one argument, whatever it holds.
    #[must_use]
    pub fn order_argv(&self, key: &str, text: &str) -> Vec<OsString> {
        vec![
            "issue".into(),
            "comment".into(),
            "add".into(),
            key.into(),
            "--content".into(),
            text.into(),
        ]
    }
}

/// multica's tiered exit codes as reason words; any other failure keeps the
/// reason [`run`] gave it.
#[must_use]
pub fn exit_reason(err: String) -> String {
    let code = err
        .strip_prefix("backend_refused:")
        .and_then(|rest| rest.rsplit_once("exited with "))
        .and_then(|(_, code)| code.trim().parse::<i32>().ok());
    let word = match code {
        Some(2) => "network",
        Some(3) => "auth",
        Some(4) => "not_found",
        Some(5) => "validation",
        _ => return err,
    };
    format!("{word}: {PROGRAM} exited with {}", code.unwrap_or_default())
}

/// An issue key: `MUL-123` (a prefix of capitals and digits, a dash, digits).
fn is_identifier(s: &str) -> bool {
    let Some((prefix, n)) = s.split_once('-') else {
        return false;
    };
    prefix.len() <= 16
        && prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !n.is_empty()
        && n.len() <= 10
        && n.bytes().all(|b| b.is_ascii_digit())
}

/// A canonical UUID: 8-4-4-4-12 hex.
fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What an order to this issue is addressed to: `identifier`, else `id`.
/// Never an `assignee_*`, which names who works on it, not the issue.
fn target(issue: &Value) -> Option<&str> {
    let key = issue["identifier"]
        .as_str()
        .map(str::trim)
        .filter(|k| is_identifier(k));
    key.or_else(|| issue["id"].as_str().map(str::trim).filter(|k| is_uuid(k)))
}

/// One issue's `status`.
fn state(status: &str) -> TaskState {
    match status {
        "in_progress" => TaskState::Working,
        "in_review" => TaskState::Blocked(Blocked::Approval),
        "done" => TaskState::Done,
        // `backlog`, `todo`, `blocked`, `cancelled`, and custom statuses.
        _ => TaskState::Idle,
    }
}

/// The issues of `multica issue list --output json` as tasks. An issue with
/// neither a usable `identifier` nor `id` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let issues = value
        .get("issues")
        .and_then(Value::as_array)
        .ok_or_else(|| "malformed: no `issues` array".to_string())?;
    let mut tasks = Vec::new();
    for issue in issues {
        let Some(key) = target(issue) else {
            continue;
        };
        let Some(status) = issue["status"].as_str() else {
            continue;
        };
        tasks.push(Task {
            key: key.to_string(),
            title: title(issue["title"].as_str().unwrap_or_default(), key),
            state: state(status.trim()),
            activity: None,
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
        if !(is_identifier(key) || is_uuid(key)) {
            return Err("bad_key: not a multica issue key or id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        // multica would read it as an option, not as the content.
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

    /// Doc-derived (CLI_AND_DAEMON.md, 2026-11-26), not captured: the envelope
    /// and only the issue keys the doc names.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/multica.json");

    fn page(issues: Vec<Value>) -> String {
        let n = issues.len();
        serde_json::json!({"total": n, "limit": 100, "offset": 0, "has_more": false, "issues": issues})
            .to_string()
    }

    #[test]
    fn the_fixture_parses_into_the_documented_statuses() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("MUL-12", TaskState::Working),
                ("MUL-14", TaskState::Blocked(Blocked::Approval)),
                ("MUL-15", TaskState::Idle),
                ("MUL-9", TaskState::Done),
                ("MUL-16", TaskState::Working),
                ("MUL-17", TaskState::Idle),
                ("MUL-18", TaskState::Idle),
                ("MUL-19", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "Refactor the billing API");
        assert_eq!(tasks[2].title, "Rotate [redacted] in [path]");
        assert_eq!(tasks[4].title, "MUL-16", "a command line is not a title");
        assert!(
            tasks.iter().all(|t| t.activity.is_none()),
            "the list has no run or stage"
        );
    }

    #[test]
    fn the_envelope_is_issues_with_paging_fields() {
        assert_eq!(parse(&page(vec![])).expect("empty page"), []);
        // The shapes the first version read are not what the doc describes.
        assert!(parse(r#"[{"key":"MUL-1","status":"done"}]"#).is_err());
        assert!(parse(r#"{"total":0,"limit":100,"offset":0,"has_more":false}"#).is_err());
        // `key` is not a field the doc names: an issue addressed only by it is left out.
        assert_eq!(
            parse(&page(vec![
                serde_json::json!({"key": "MUL-1", "status": "done"})
            ]))
            .expect("page"),
            []
        );
    }

    #[test]
    fn each_built_in_status_maps_and_the_rest_are_idle() {
        for (word, want) in [
            ("backlog", TaskState::Idle),
            ("todo", TaskState::Idle),
            ("in_progress", TaskState::Working),
            ("in_review", TaskState::Blocked(Blocked::Approval)),
            ("done", TaskState::Done),
            ("blocked", TaskState::Idle),
            ("cancelled", TaskState::Idle),
            ("review", TaskState::Idle),
            ("qa_signoff", TaskState::Idle),
        ] {
            let tasks = parse(&page(vec![
                serde_json::json!({"identifier": "MUL-1", "status": word}),
            ]))
            .expect(word);
            assert_eq!(tasks[0].state, want, "{word}");
        }
    }

    #[test]
    fn the_target_is_the_identifier_or_id_never_the_assignee() {
        let uuid = "5fb87ac7-23b5-4a7a-81fa-ed295a54545d";
        // No identifier: the canonical id.
        let tasks = parse(&page(vec![
            serde_json::json!({"id": uuid, "status": "todo"}),
        ]))
        .expect("id");
        assert_eq!(tasks[0].key, uuid);
        // Only an assignee: not addressable at all.
        let tasks = parse(&page(vec![serde_json::json!({
            "status": "in_review", "assignee_type": "agent", "assignee_id": uuid,
            "assignee": "Lambda", "key": "Lambda",
        })]))
        .expect("assignee only");
        assert_eq!(tasks, []);
        // Both: the identifier, and the order addresses it.
        let tasks = parse(FIXTURE).expect("fixture");
        for t in &tasks {
            assert!(is_identifier(&t.key) || is_uuid(&t.key), "{}", t.key);
        }
        let argv = Multica.order_argv(&tasks[1].key, "lgtm");
        assert_eq!(argv[3], OsString::from("MUL-14"));
        assert!(Multica
            .send_order("Lambda", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(Multica
            .send_order("agent", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
    }

    #[test]
    fn nothing_from_a_command_path_token_or_property_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let out = status_json(&Multica, &tasks, budget(1)).to_string();
        for leak in [
            "rm -rf",
            "/tmp",
            "~/",
            "ci.env",
            "ghp_",
            "GITHUB_TOKEN",
            "5fb87ac7",
            "member:",
            "urgent",
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&Multica, &tasks, budget(1));
        assert_eq!(deck[0].key, "MUL-14");
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "Docs for the v2 webhooks: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
    }

    #[test]
    fn a_forty_issue_fleet_fits_the_deck() {
        let issues: Vec<Value> = (0..40)
            .map(|i| {
                serde_json::json!({
                    "id": format!("00000000-0000-0000-0000-{i:012}"),
                    "identifier": format!("MUL-{i}"),
                    "title": format!("Issue {i}"),
                    "status": if i % 4 == 0 { "in_progress" } else { "in_review" },
                    "assignee_type": "agent",
                    "assignee_id": "5fb87ac7-23b5-4a7a-81fa-ed295a54545d",
                })
            })
            .collect();
        let tasks = parse(&page(issues)).expect("fleet");
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
        let tasks = parse(&page(vec![
            serde_json::json!({"status": "in_review"}),
            serde_json::json!({"identifier": "../x", "status": "done"}),
            serde_json::json!({"identifier": "MUL-2"}),
            serde_json::json!({"identifier": "MUL-3", "status": "thinking", "properties": 3}),
        ]))
        .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn the_argv_is_what_the_doc_spells() {
        assert_eq!(
            list_argv(),
            ["issue", "list", "--limit", "100", "--output", "json"].map(OsString::from)
        );
        let argv = Multica.order_argv("MUL-14", "ship it; rm -rf / && echo $(id)");
        let argv: Vec<&str> = argv.iter().map(|a| a.to_str().unwrap_or("?")).collect();
        assert_eq!(
            argv,
            [
                "issue",
                "comment",
                "add",
                "MUL-14",
                "--content",
                "ship it; rm -rf / && echo $(id)"
            ]
        );
    }

    #[test]
    fn an_order_multica_would_misread_is_refused_before_it_runs() {
        for key in ["-x", "mul-1", "MUL-", "MUL-1x", "../MUL-1", ""] {
            assert!(
                Multica
                    .send_order(key, "hi")
                    .is_err_and(|e| e.starts_with("bad_key")),
                "{key:?}"
            );
        }
        assert!(Multica
            .send_order("MUL-1", "  ")
            .is_err_and(|e| e.starts_with("empty")));
        assert!(Multica
            .send_order("MUL-1", "--help")
            .is_err_and(|e| e.starts_with("bad_text")));
    }

    #[test]
    fn tiered_exit_codes_are_reason_words() {
        let at = |code: i32| exit_reason(format!("backend_refused: multica exited with {code}"));
        assert_eq!(reason_word(&at(2)), "network");
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
