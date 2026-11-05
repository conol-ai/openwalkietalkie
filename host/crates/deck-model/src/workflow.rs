//! Public plans and observed activity, shared by host views. Progress comes
//! from provider events; finishing a tool never invents a completed plan step.
use crate::{sanitize, Agent, AgentState, ChatMessage, ChatRole, MessageMeta};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_STEPS: usize = 64;
pub const MAX_RUNS: usize = 16;

/// Display text only, not raw tool arguments or results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivitySummary {
    pub label: String,
    #[serde(default)]
    pub detail: String,
}

impl ActivitySummary {
    pub fn bounded(self) -> Self {
        Self {
            label: sanitize::cut(&sanitize::collapse(&self.label), 160),
            detail: sanitize::cut(&sanitize::collapse(&self.detail), 320),
        }
    }
}

pub fn message_summary(message: &ChatMessage) -> (String, String) {
    if let Some(summary) = &message.message.summary {
        if !summary.label.trim().is_empty() {
            return (summary.label.clone(), summary.detail.clone());
        }
    }
    tool_summary(&message.text)
}

/// A provider-reported background run. Counts are workers, not guessed plan steps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub id: String,
    pub name: String,
    pub status: ActivityStatus,
    pub completed: u32,
    pub total: u32,
    pub elapsed_ms: u64,
    pub tokens: u64,
}

impl Run {
    pub fn bounded(mut self) -> Self {
        self.id = sanitize::cut(&sanitize::collapse(&self.id), 128);
        self.name = sanitize::cut(&sanitize::collapse(&self.name), 160);
        self.completed = self.completed.min(self.total);
        self
    }

    pub fn metrics(&self) -> String {
        let seconds = self.elapsed_ms / 1000;
        let elapsed = if seconds >= 3600 {
            format!("{}h{}m", seconds / 3600, seconds % 3600 / 60)
        } else if seconds >= 60 {
            format!("{}m{}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        };
        let tokens = if self.tokens >= 1_000_000 {
            format!("{:.1}m", self.tokens as f64 / 1_000_000.)
        } else if self.tokens >= 1000 {
            format!("{:.1}k", self.tokens as f64 / 1000.)
        } else {
            self.tokens.to_string()
        };
        format!(
            "{}/{} · {elapsed} · {tokens} tokens",
            self.completed, self.total
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlanStep {
    pub step: String,
    pub status: StepStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
}

impl Plan {
    pub fn codex(value: &Value) -> Option<Self> {
        Self::parse(value, "plan", "step")
    }

    pub fn claude(value: &Value) -> Option<Self> {
        Self::parse(value, "todos", "content")
    }

    fn parse(value: &Value, list: &str, label: &str) -> Option<Self> {
        let mut steps = Vec::new();
        for entry in value.get(list)?.as_array()?.iter().take(MAX_STEPS) {
            let step = entry.get(label)?.as_str()?.to_owned();
            let status = match entry.get("status")?.as_str()? {
                "pending" => StepStatus::Pending,
                "in_progress" | "inProgress" => StepStatus::InProgress,
                "completed" => StepStatus::Completed,
                _ => return None,
            };
            steps.push(PlanStep { step, status });
        }
        Some(
            Self {
                explanation: value
                    .get("explanation")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                steps,
            }
            .bounded(),
        )
    }

    pub fn bounded(mut self) -> Self {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.step = sanitize::cut(&sanitize::collapse(&step.step), 256);
        }
        self.steps.retain(|step| !step.step.is_empty());
        self.explanation = self
            .explanation
            .map(|text| sanitize::cut(&sanitize::collapse(&text), 1024))
            .filter(|text| !text.is_empty());
        self
    }

    pub fn completed(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == StepStatus::Completed)
            .count()
    }

    pub fn text(&self) -> String {
        let mut text = String::from("**Plan**\n");
        if let Some(explanation) = &self.explanation {
            text.push_str(&format!("\n{explanation}\n"));
        }
        for step in &self.steps {
            let mark = match step.status {
                StepStatus::Pending => " ",
                StepStatus::InProgress => ">",
                StepStatus::Completed => "x",
            };
            text.push_str(&format!("\n- [{mark}] {}", step.step));
        }
        text
    }
}

/// Called before the message is upserted, by both the store and GUI mirror.
/// Retain the latest plan even after its chat item leaves the bounded tail.
pub fn observe(agent: &mut Agent, message: &MessageMeta) {
    if message.role == ChatRole::Tool {
        if let Some(runs) = &message.runs {
            agent.workflow_runs = runs
                .iter()
                .take(MAX_RUNS)
                .cloned()
                .map(Run::bounded)
                .collect();
        }
    }
    let position = agent
        .messages
        .iter()
        .position(|entry| entry.message.id == message.id);
    if message.role == ChatRole::User && position.is_none() {
        agent.workflow = None;
    }
    let Some(plan) = message
        .plan
        .as_ref()
        .filter(|_| message.role == ChatRole::Tool)
    else {
        return;
    };
    // A late result for an older TodoWrite must not reclaim a newer plan or
    // the next turn. The same plan id can still update in place.
    if position.is_some_and(|index| {
        agent.messages[index + 1..]
            .iter()
            .any(|entry| entry.message.role == ChatRole::User || entry.message.plan.is_some())
    }) {
        return;
    }
    let plan = plan.clone().bounded();
    agent.workflow = (!plan.steps.is_empty()).then_some(plan);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

impl ActivityStatus {
    pub fn provider(status: Option<&str>, streaming: bool) -> Self {
        match status {
            Some("failed" | "error" | "errored") => Self::Failed,
            Some("interrupted" | "cancelled" | "canceled" | "declined" | "aborted") => {
                Self::Stopped
            }
            Some("completed" | "success") => Self::Completed,
            _ if streaming => Self::Running,
            _ => Self::Completed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
}

pub struct Activity<'a> {
    pub message: &'a ChatMessage,
    pub title: String,
    pub status: ActivityStatus,
}

/// Current turn only when its user message is retained; otherwise the recent
/// bounded activity. Stale running flags must not animate an idle session.
pub fn activity(agent: &Agent) -> Vec<Activity<'_>> {
    let start = agent
        .messages
        .iter()
        .rposition(|entry| entry.message.role == ChatRole::User)
        .map_or(0, |index| index + 1);
    agent.messages[start..]
        .iter()
        .filter(|entry| {
            entry.message.role == ChatRole::Tool
                && entry.message.plan.is_none()
                && entry.message.runs.is_none()
        })
        .map(|message| {
            let heading = message
                .text
                .lines()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let reported = heading
                .split('·')
                .nth(1)
                .map(|s| s.trim().trim_matches('*').trim());
            let mut status = message
                .message
                .activity
                .unwrap_or_else(|| ActivityStatus::provider(reported, message.message.streaming));
            if status == ActivityStatus::Running
                && !matches!(agent.state, AgentState::Working | AgentState::Question)
            {
                status = ActivityStatus::Stopped;
            }
            let (verb, target) = message_summary(message);
            Activity {
                message,
                title: format!("{verb} {target}").trim().to_owned(),
                status,
            }
        })
        .collect()
}

/// Only inputs and tool names become summaries; output stays behind disclosure.
pub fn tool_summary(text: &str) -> (String, String) {
    let heading = text
        .lines()
        .next()
        .unwrap_or("Tool")
        .split('·')
        .next()
        .unwrap_or("Tool")
        .trim_matches(|c: char| c == '*' || c == '#' || c.is_whitespace());
    let lower = heading.to_ascii_lowercase();
    let fence = text
        .split("```")
        .nth(1)
        .and_then(|s| s.split_once('\n'))
        .map_or("", |(_, s)| s.trim());
    let input = serde_json::from_str::<serde_json::Value>(fence).ok();
    let value = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| input.as_ref()?.get(*k)?.as_str())
            .unwrap_or("")
    };
    let path = value(&["file_path", "path", "file"]);
    let basename = |p: &str| {
        p.trim_matches(|c| c == '\'' || c == '"' || c == '`')
            .rsplit('/')
            .next()
            .unwrap_or(p)
            .to_string()
    };
    if lower.contains("read") {
        return (
            "Read".into(),
            if path.is_empty() {
                "file".into()
            } else {
                basename(path)
            },
        );
    }
    if lower.contains("search") || lower.contains("grep") || lower.contains("glob") {
        return ("Search".into(), value(&["pattern", "query"]).to_string());
    }
    if lower.contains("list") {
        return (
            "List".into(),
            if path.is_empty() {
                "files".into()
            } else {
                basename(path)
            },
        );
    }
    if lower.contains("edit")
        || lower.contains("write")
        || lower.contains("file change")
        || lower.contains("patch")
    {
        let path = if path.is_empty() {
            text.lines()
                .find_map(|l| l.strip_prefix("- `"))
                .unwrap_or("")
        } else {
            path
        };
        return (
            "Edit".into(),
            if path.is_empty() {
                "files".into()
            } else {
                basename(path)
            },
        );
    }
    if lower.contains("command") || lower.contains("bash") || lower.contains("exec") {
        let cmd = value(&["command", "cmd"]);
        let cmd = if cmd.is_empty() { fence } else { cmd };
        let first = cmd.lines().next().unwrap_or("");
        let program = first.split_whitespace().next().unwrap_or("");
        if matches!(program, "cat" | "sed" | "head" | "tail") {
            return (
                "Read".into(),
                basename(first.split_whitespace().last().unwrap_or("file")),
            );
        }
        if matches!(program, "rg" | "grep" | "find" | "ls") {
            return (
                if program == "ls" || first.contains("--files") {
                    "List"
                } else {
                    "Search"
                }
                .into(),
                first.chars().take(80).collect(),
            );
        }
        return (
            "Run".into(),
            if first.is_empty() {
                heading.into()
            } else {
                first.chars().take(80).collect()
            },
        );
    }
    ("Use".into(), heading.into())
}

/// The device's compact workflow document. Reserved disclosure indices 64/65
/// address the plan and older activity; 0..63 still address retained messages.
/// It shares the chat viewer's font, windowing and syntax-highlighted output.
pub fn device_lines(
    agent: &Agent,
    size: u8,
    expanded: Option<&std::collections::BTreeSet<String>>,
) -> (deck_proto::DocFont, Vec<String>) {
    let open = |key: &str| expanded.is_some_and(|set| set.contains(key));
    let (font, mut lines) = deck_wrap::wrap_rich_reply("## Workflow", size);
    for run in &agent.workflow_runs {
        lines.extend(deck_wrap::wrap_rich_reply(&format!("### {}", run.name), size).1);
        lines.extend(deck_wrap::wrap_rich_reply(&run.metrics(), size).1);
        if run.total > 0 {
            let total = run.total.min(64);
            let done = u64::from(run.completed) * u64::from(total) / u64::from(run.total);
            lines.push(format!("w0{done:02x}{total:02x}"));
        }
        if run.status != ActivityStatus::Running {
            lines.extend(deck_wrap::wrap_rich_reply(run.status.label(), size).1);
        }
    }
    if let Some(plan) = agent
        .workflow
        .as_ref()
        .filter(|plan| !plan.steps.is_empty())
    {
        lines.extend(
            deck_wrap::wrap_rich_reply(
                &format!(
                    "## Plan · {}/{} complete",
                    plan.completed(),
                    plan.steps.len()
                ),
                size,
            )
            .1,
        );
        lines.push(format!(
            "w0{:02x}{:02x}",
            plan.completed(),
            plan.steps.len()
        ));
        let all = open("workflow:plan");
        let current = plan
            .steps
            .iter()
            .position(|step| step.status == StepStatus::InProgress)
            .or_else(|| {
                plan.steps
                    .iter()
                    .position(|step| step.status == StepStatus::Pending)
            })
            .unwrap_or(plan.steps.len() - 1);
        let start = if all {
            0
        } else {
            current
                .saturating_sub(1)
                .min(plan.steps.len().saturating_sub(3))
        };
        let end = if all {
            plan.steps.len()
        } else {
            (start + 3).min(plan.steps.len())
        };
        // Keep the disclosure before the changing content so expanding it
        // preserves its scroll anchor and reveals the first hidden step.
        if plan.steps.len() > 3 || plan.explanation.is_some() {
            lines.push(deck_wrap::disclosure(
                &format!(
                    "{} plan · {} steps",
                    if all { "Compact" } else { "Full" },
                    plan.steps.len()
                ),
                size,
                64,
                all,
            ));
        }
        if all {
            if let Some(text) = &plan.explanation {
                lines.extend(deck_wrap::wrap_rich_reply(text, size).1);
            }
        }
        for (i, step) in plan.steps.iter().enumerate().take(end).skip(start) {
            let (prefix, state) = match step.status {
                StepStatus::Completed => ("", "Done"),
                StepStatus::InProgress => (
                    "> ",
                    if agent.state == AgentState::Working {
                        "Now"
                    } else {
                        "Paused"
                    },
                ),
                StepStatus::Pending => ("", "Next"),
            };
            lines.extend(
                deck_wrap::wrap_rich_reply(
                    &format!("{prefix}{}. {state}: {}", i + 1, step.step),
                    size,
                )
                .1,
            );
        }
    }
    let tools = activity(agent);
    if !tools.is_empty() {
        lines.extend(deck_wrap::wrap_rich_reply("## Recent activity", size).1);
        let all = open("workflow:tools");
        let first = tools.len().saturating_sub(3);
        if tools.len() > 3 {
            lines.push(deck_wrap::disclosure(
                &format!(
                    "{} tools · {}",
                    if all { "Recent" } else { "All" },
                    tools.len()
                ),
                size,
                65,
                all,
            ));
        }
        for (i, tool) in tools.iter().enumerate() {
            let expanded = open(&tool.message.message.id);
            if !all && i < first && tool.status != ActivityStatus::Running && !expanded {
                continue;
            }
            let index = agent
                .messages
                .iter()
                .position(|m| m.message.id == tool.message.message.id)
                .unwrap_or(0) as u8;
            let title = format!("{} · {}", tool.title, tool.status.label());
            // Bound by the real firmware font while preserving stopped/failed status.
            let row = deck_wrap::tool_header(
                &title,
                size,
                index,
                expanded,
                tool.status == ActivityStatus::Running,
            );
            lines.push(row);
            if expanded {
                lines.extend(deck_wrap::wrap_rich_reply(&tool.message.text, size).1);
            }
        }
    } else if agent.workflow.is_none() && agent.workflow_runs.is_empty() {
        lines.extend(deck_wrap::wrap_rich_reply("Waiting for plan or tool activity.", size).1);
    }
    (font, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_plans_normalize_bound_and_validate_the_public_schema() {
        let codex = Plan::codex(&json!({"explanation":"  Checking\n inputs ","plan":[
            {"step":"Inspect\n code", "status":"completed"},
            {"step":"Implement", "status":"inProgress"},
            {"step":"Verify", "status":"pending"}
        ]}))
        .unwrap();
        let claude = Plan::claude(&json!({"todos":[
            {"content":"Inspect code", "status":"completed", "activeForm":"Inspecting code"},
            {"content":"Implement", "status":"in_progress"},
            {"content":"Verify", "status":"pending"}
        ]}))
        .unwrap();
        assert_eq!(codex.steps, claude.steps);
        assert_eq!(codex.explanation.as_deref(), Some("Checking inputs"));
        assert_eq!(codex.completed(), 1);
        assert!(Plan::codex(&json!({"plan":[{"step":"x", "status":"unknown"}]})).is_none());
        assert!(Plan::codex(&json!({"plan":"not a list"})).is_none());
        assert!(Plan::codex(&json!({"plan":[]})).unwrap().steps.is_empty());
        let bounded = Plan::codex(
            &json!({"plan":vec![json!({"step":"界".repeat(1000), "status":"pending"});100]}),
        )
        .unwrap();
        assert_eq!(bounded.steps.len(), MAX_STEPS);
        assert!(bounded.steps.iter().all(|s| s.step.len() <= 256));
    }

    #[test]
    fn activity_is_current_turn_only_and_never_animates_an_idle_agent() {
        let mut agent = crate::testkit::fixture_agent("ag_workflow", 1, AgentState::Working);
        agent.state = AgentState::Working;
        let message = |id: &str, role, activity| ChatMessage {
            message: MessageMeta { summary: None, images: Vec::new(),
                id: id.into(),
                role,
                streaming: activity == Some(ActivityStatus::Running),
                activity,
                runs: None,
                plan: None,
            },
            text: "**Bash**\n\n```json\n{\"command\":\"cargo test\"}\n```".into(),
            at: agent.since,
        };
        agent.messages = vec![
            message("old", ChatRole::Tool, Some(ActivityStatus::Failed)),
            message("u1", ChatRole::User, None),
            message("t1", ChatRole::Tool, Some(ActivityStatus::Running)),
        ];
        let tools = activity(&agent);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].title, "Run cargo test");
        assert_eq!(tools[0].status, ActivityStatus::Running);
        agent.state = AgentState::Ready;
        assert_eq!(activity(&agent)[0].status, ActivityStatus::Stopped);
        agent.messages[2].message.activity = Some(ActivityStatus::Failed);
        assert_eq!(activity(&agent)[0].status, ActivityStatus::Failed);
    }
}
