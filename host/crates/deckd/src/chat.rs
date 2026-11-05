//! Read the public messages Claude commits to its session transcript.
//! Bounded tail reads, no content logging, and no reasoning/thinking blocks.
use crate::{
    adapters::{Adapter, Adapters, HookPayload},
    core::handle::CoreHandle,
};
use deck_model::{AgentId, ChatRole, MessageMeta, ReplySet, Request, SessionId};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub(crate) struct Watch {
    path: PathBuf,
    handle: tokio::task::AbortHandle,
    finished: Arc<AtomicBool>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
pub(crate) type Watches = std::sync::Mutex<BTreeMap<AgentId, Watch>>;

fn text_blocks(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            (b["type"].as_str() == Some("text"))
                .then(|| b["text"].as_str())
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn transcript_messages(bytes: &[u8], session: &str) -> Vec<(MessageMeta, String)> {
    let mut messages: Vec<(MessageMeta, String)> = Vec::new();
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if v["sessionId"].as_str().is_some_and(|s| s != session)
            || v["isSidechain"].as_bool() == Some(true)
        {
            continue;
        }
        let Some(uuid) = v["uuid"].as_str() else {
            continue;
        };
        let role = match v["type"].as_str() {
            Some("user") => ChatRole::User,
            Some("assistant") => ChatRole::Assistant,
            _ => continue,
        };
        let content = &v["message"]["content"];
        let mut text = text_blocks(content);
        let images = crate::chat_images::blocks(content);
        if text.trim().is_empty() && images.is_empty() && crate::chat_images::has_images(content) { text = "[Image preview unavailable]".into(); }
        if !text.trim().is_empty() || !images.is_empty() {
            let streaming = role == ChatRole::Assistant && v["message"]["stop_reason"].is_null();
            upsert(
                &mut messages,
                MessageMeta { summary: None, images,
                    activity: None,
                    runs: None,
                    plan: None,
                    id: uuid.into(),
                    role,
                    streaming,
                },
                text,
            );
        }
        for block in content.as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("tool_use") => {
                    let Some(id) = block["id"].as_str() else {
                        continue;
                    };
                    let name = block["name"].as_str().unwrap_or("Tool");
                    let input = deck_model::sanitize::cut(&block["input"].to_string(), 2048);
                    upsert(
                        &mut messages,
                        MessageMeta { summary: None, images: Vec::new(),
                            activity: Some(deck_model::workflow::ActivityStatus::Running),
                            runs: None,
                            plan: (name == "TodoWrite")
                                .then(|| deck_model::workflow::Plan::claude(&block["input"]))
                                .flatten(),
                            id: format!("tool:{id}"),
                            role: ChatRole::Tool,
                            streaming: true,
                        },
                        format!("**{name}**\n\n```json\n{input}\n```"),
                    );
                }
                Some("tool_result") => {
                    let Some(id) = block["tool_use_id"].as_str() else {
                        continue;
                    };
                    let status = if block["is_error"].as_bool() == Some(true) {
                        "failed"
                    } else {
                        "completed"
                    };
                    let text = text_blocks(&block["content"]);
                    let images = crate::chat_images::blocks(&block["content"]);
                    let key = format!("tool:{id}");
                    let plan = messages
                        .iter()
                        .find(|(m, _)| m.id == key)
                        .and_then(|(meta, _)| meta.plan.clone());
                    let previous = messages
                        .iter()
                        .find(|(m, _)| m.id == key)
                        .map(|(_, text)| text.as_str());
                    let (name, input) = previous
                        .and_then(|s| s.split_once("\n\n"))
                        .map_or(("Tool", ""), |(name, input)| {
                            (name.trim_matches('*'), input)
                        });
                    let body = format!(
                        "**{name} · {status}**\n\n{input}\n\n{}",
                        deck_model::sanitize::cut(&text, 8192)
                    );
                    upsert(
                        &mut messages,
                        MessageMeta { summary: None, images,
                            activity: Some(deck_model::workflow::ActivityStatus::provider(
                                Some(status),
                                false,
                            )),
                            runs: None,
                            plan,
                            id: key,
                            role: ChatRole::Tool,
                            streaming: false,
                        },
                        body,
                    );
                }
                _ => {}
            }
        }
    }
    // A later item means the preceding assistant block is committed, even
    // when Claude omitted stop_reason on that JSONL record.
    let last = messages.len().saturating_sub(1);
    for (i, (meta, _)) in messages.iter_mut().enumerate() {
        if i != last && meta.role == ChatRole::Assistant {
            meta.streaming = false;
        }
    }
    messages
}

fn upsert(messages: &mut Vec<(MessageMeta, String)>, meta: MessageMeta, text: String) {
    let text = deck_model::sanitize::cut(&text, 64 * 1024);
    if let Some(old) = messages.iter_mut().find(|(m, _)| m.id == meta.id) {
        *old = (meta, text);
    } else {
        messages.push((meta, text));
    }
    while messages.len() > 64 {
        messages.remove(0);
    }
}

async fn tail(path: &Path) -> std::io::Result<Vec<u8>> {
    const MAX: u64 = 12 * 1024 * 1024;
    let mut file = tokio::fs::File::open(path).await?;
    let start = file.metadata().await?.len().saturating_sub(MAX);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut bytes = Vec::new();
    file.take(MAX).read_to_end(&mut bytes).await?;
    if start != 0 {
        let first = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..first);
    }
    Ok(bytes)
}

fn permitted_path(path: &str, session: &str) -> Option<PathBuf> {
    if session.is_empty() || session.contains(['/', '\\']) {
        return None;
    }
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))?
        .join("projects")
        .canonicalize()
        .ok()?;
    let path = Path::new(path).canonicalize().ok()?;
    (path.starts_with(root)
        && path.file_name()?.to_str()? == format!("{session}.jsonl")
        && path.is_file())
    .then_some(path)
}

/// Called after resolving ownership through the normal hook adapter.
pub(crate) async fn observe(
    adapters: &Adapters,
    core: &CoreHandle,
    session: SessionId,
    agent: &AgentId,
    payload: &HookPayload,
) {
    let principal = adapters.claude.principal();
    let event = payload.event();
    if event == "SessionEnd" {
        if let Ok(mut watches) = adapters.transcripts.lock() {
            watches.remove(agent);
        }
        return;
    }
    // Hooks give immediate start/end status even while a transcript is quiet.
    if matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure") {
        if let Some(id) = payload.str("tool_use_id") {
            let name = payload.str("tool_name").unwrap_or("Tool");
            let live = event == "PreToolUse";
            let status = if live {
                "running"
            } else if event == "PostToolUseFailure" {
                "failed"
            } else {
                "completed"
            };
            let input = payload
                .get("tool_input")
                .map(Value::to_string)
                .unwrap_or_default();
            let response = (!live).then(|| payload.get("tool_response").or_else(|| payload.get("error"))).flatten();
            let image_content = response.map(|v| if v.is_array() { v } else { &v["content"] });
            let images = image_content.map(crate::chat_images::blocks).unwrap_or_default();
            let result = response.map(|v| {
                if v.is_array() || v["content"].is_array() { text_blocks(if v.is_array() { v } else { &v["content"] }) }
                else { v.to_string() }
            }).unwrap_or_default();
            let details = format!(
                "{}\n```\n\n{}",
                deck_model::sanitize::cut(&input, 2048),
                deck_model::sanitize::cut(&result, 8192)
            );
            let req = ReplySet {
                agent: agent.clone(),
                text: format!("**{name} · {status}**\n\n```json\n{details}"),
                message: Some(MessageMeta { summary: None, images,
                    activity: Some(deck_model::workflow::ActivityStatus::provider(
                        Some(status),
                        live,
                    )),
                    runs: None,
                    plan: (name == "TodoWrite")
                        .then(|| deck_model::workflow::Plan::claude(&payload.0["tool_input"]))
                        .flatten(),
                    id: format!("tool:{id}"),
                    role: ChatRole::Tool,
                    streaming: live,
                }),
                speak: false,
                announce: None,
                lane: None,
            };
            let _ = crate::adapters::rt::budgeted(core.api(
                principal.clone(),
                session,
                false,
                Request::Reply(req),
            ))
            .await;
        }
    }
    let Some(cli_session) = payload.str("session_id") else {
        return;
    };
    let Some(path) = payload
        .str("transcript_path")
        .and_then(|p| permitted_path(p, cli_session))
    else {
        return;
    };
    start_watch(
        adapters,
        core,
        session,
        agent,
        cli_session,
        path,
        matches!(event, "Stop" | "UserPromptSubmit" | "PreToolUse").then_some(event == "Stop"),
    );
}

/// Recover already-running background workflows when deckd starts between hooks.
pub(crate) fn seed(
    adapters: &Adapters,
    core: &CoreHandle,
    session: SessionId,
    agent: &deck_model::Agent,
) {
    let Some(id) = agent
        .session
        .key
        .as_deref()
        .and_then(|k| k.strip_prefix("claude:"))
    else {
        return;
    };
    let Some(cwd) = agent.session.cwd.as_deref() else {
        return;
    };
    let Some(dir) = crate::liveness::claude_dir() else {
        return;
    };
    let candidate = crate::liveness::transcript_path(&dir, cwd, id);
    let Some(path) = candidate.to_str().and_then(|p| permitted_path(p, id)) else {
        return;
    };
    start_watch(adapters, core, session, &agent.id, id, path, None);
}

#[allow(clippy::too_many_arguments)]
fn start_watch(
    adapters: &Adapters,
    core: &CoreHandle,
    session: SessionId,
    agent: &AgentId,
    cli_session: &str,
    path: PathBuf,
    finished_update: Option<bool>,
) {
    let principal = adapters.claude.principal();
    let Ok(mut watches) = adapters.transcripts.lock() else {
        return;
    };
    watches.retain(|_, w| !w.handle.is_finished());
    if let Some(watch) = watches.get(agent).filter(|w| w.path == path) {
        if let Some(done) = finished_update {
            watch.finished.store(done, Ordering::Relaxed);
        }
        return;
    }
    watches.remove(agent);
    if watches.len() >= 64 {
        return;
    }
    let core = core.clone();
    let agent_id = agent.clone();
    let session_name = cli_session.to_string();
    let watched = path.clone();
    let config = adapters.config.clone();
    let finished = Arc::new(AtomicBool::new(finished_update.unwrap_or(false)));
    let turn_finished = finished.clone();
    let task = tokio::spawn(async move {
        let mut previous = BTreeMap::new();
        let mut stamp = None;
        let mut workflows = Some(crate::claude_workflows::Tracker::new(
            &watched,
            &session_name,
        ));
        let mut previous_runs = Vec::new();
        let mut poll = 0u8;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !config.load().hooks.enabled {
                return;
            }
            let Ok(meta) = tokio::fs::metadata(&watched).await else {
                return;
            };
            let done = turn_finished.load(Ordering::Relaxed);
            let current = (meta.len(), meta.modified().ok(), done);
            let mut updates = Vec::new();
            if stamp != Some(current) {
                stamp = Some(current);
                let Ok(bytes) = tail(&watched).await else {
                    return;
                };
                let session = session_name.clone();
                updates = tokio::task::spawn_blocking(move || transcript_messages(&bytes, &session)).await.unwrap_or_default();
            }
            if poll == 0 {
                let mut tracker = workflows.take().unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let Ok((tracker, runs)) = tokio::task::spawn_blocking(move || {
                    let runs = tracker.poll(now);
                    (tracker, runs)
                })
                .await
                else {
                    return;
                };
                workflows = Some(tracker);
                if runs != previous_runs {
                    let text = if runs.is_empty() {
                        "Workflow runs finished".into()
                    } else {
                        runs.iter()
                            .map(|r| format!("{} · {} · {}", r.name, r.status.label(), r.metrics()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    updates.push((
                        MessageMeta { summary: None, images: Vec::new(),
                            id: "workflow:runs".into(),
                            role: ChatRole::Tool,
                            streaming: false,
                            plan: None,
                            activity: None,
                            runs: Some(runs.clone()),
                        },
                        text,
                    ));
                    previous_runs = runs;
                }
            }
            poll = (poll + 1) % 4;
            for (mut message, text) in updates {
                if done {
                    message.streaming = false;
                    if message.activity == Some(deck_model::workflow::ActivityStatus::Running) {
                        message.activity = Some(deck_model::workflow::ActivityStatus::Stopped);
                    }
                }
                let value = (message.clone(), text.clone());
                if previous.get(&message.id) == Some(&value) {
                    continue;
                }
                let request = ReplySet {
                    agent: agent_id.clone(),
                    text,
                    message: Some(message.clone()),
                    speak: false,
                    announce: None,
                    lane: None,
                };
                match crate::adapters::rt::budgeted(core.api(
                    principal.clone(),
                    session,
                    false,
                    Request::Reply(request),
                ))
                .await
                {
                    Some(Ok(_)) => {
                        if let Some(runs) = message
                            .runs
                            .as_ref()
                            .filter(|_| !previous.contains_key(&message.id))
                        {
                            tracing::info!(agent = %agent_id, runs = runs.len(),
                                running = runs.iter().filter(|r| r.status == deck_model::workflow::ActivityStatus::Running).count(),
                                "Recovered background workflow counters");
                        }
                        previous.insert(message.id, value);
                    }
                    _ => return,
                }
            }
            // Never retain content outside the bounded tail's messages.
            if previous.len() > 128 {
                previous.clear();
            }
        }
    });
    watches.insert(
        agent.clone(),
        Watch {
            path,
            handle: task.abort_handle(),
            finished,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_only_messages_and_tool_results_are_preserved() {
        let image = serde_json::json!({"type":"image","source":{"type":"base64","data":crate::chat_images::fixture()}});
        let user = serde_json::json!({"type":"user","sessionId":"s","uuid":"u","message":{"content":[image.clone()]}});
        let tool = serde_json::json!({"type":"user","sessionId":"s","uuid":"t","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":[image]}]}});
        let messages = transcript_messages(format!("{user}\n{tool}\n").as_bytes(), "s");
        assert_eq!(messages.len(),2);
        assert_eq!(messages[0].0.role, ChatRole::User);
        assert_eq!(messages[1].0.role, ChatRole::Tool);
        assert!(messages.iter().all(|(m,t)| m.images.len() == 1 && !t.contains("base64")));
    }

    #[test]
    fn workflow_todowrite_keeps_its_plan_when_the_result_arrives() {
        let lines = [
            serde_json::json!({"type":"assistant", "uuid":"a1", "sessionId":"s", "message":{"content":[
                {"type":"thinking", "thinking":"private reasoning"},
                {"type":"tool_use", "id":"p1", "name":"TodoWrite", "input":{"todos":[{"content":"Test the fix", "status":"in_progress"}]}}
            ]}}),
            serde_json::json!({"type":"user", "uuid":"u1", "sessionId":"s", "message":{"content":[
                {"type":"tool_result", "tool_use_id":"p1", "content":"Updated todos"}
            ]}}),
            serde_json::json!({"type":"user", "uuid":"u2", "sessionId":"s", "message":{"content":[
                {"type":"tool_result", "tool_use_id":"t2", "content":"Command failed", "is_error":true}
            ]}}),
        ].map(|v| v.to_string()).join("\n");
        let messages = transcript_messages(lines.as_bytes(), "s");
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].0.plan.as_ref().unwrap().steps[0].step,
            "Test the fix"
        );
        assert_eq!(
            messages[0].0.activity,
            Some(deck_model::workflow::ActivityStatus::Completed)
        );
        assert_eq!(
            messages[1].0.activity,
            Some(deck_model::workflow::ActivityStatus::Failed)
        );
        assert!(!messages
            .iter()
            .any(|(_, text)| text.contains("private reasoning")));
    }

    #[test]
    fn transcript_history_tracks_tools_and_excludes_private_blocks() {
        let lines = [
            serde_json::json!({"type":"user","uuid":"u1","sessionId":"s","message":{"content":"Hello"}}),
            serde_json::json!({"type":"assistant","uuid":"a1","sessionId":"s","message":{"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"**Working**"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"pwd"}}]}}),
            serde_json::json!({"type":"user","uuid":"u2","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"/project"}]}}),
            serde_json::json!({"type":"assistant","uuid":"a2","sessionId":"elsewhere","message":{"content":"unrelated"}}),
        ].map(|v| v.to_string()).join("\n");
        let messages = transcript_messages(lines.as_bytes(), "s");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].0.role, ChatRole::User);
        assert_eq!(messages[1].1, "**Working**");
        assert!(!messages[1].0.streaming);
        assert_eq!(messages[2].0.id, "tool:t1");
        assert!(!messages[2].0.streaming);
        assert!(messages[2].1.contains("/project"));
        assert!(messages[2].1.starts_with("**Bash · completed**"));
        assert!(
            messages[2].1.contains("pwd"),
            "expansion keeps the input and result together"
        );
    }
}
