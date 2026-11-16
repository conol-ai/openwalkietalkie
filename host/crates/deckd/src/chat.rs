//! Read the public messages Claude commits to its session transcript.
//! Bounded tail reads, no content logging, and no reasoning/thinking blocks.
//!
//! Only for the sessions the deck reads, when the feed is lazy
//! (`crate::reader`): a hook posts its tool row and starts a watcher only for
//! one of those, a watcher stops once its session is not, and the first read
//! of a session goes to the store whole, in place of the history it kept.
use crate::{
    adapters::{Adapter, Adapters, HookPayload},
    api::Api,
    core::handle::CoreHandle,
    reader::Reader,
};
use deck_model::{AgentId, AgentState, ChatRole, MessageMeta, ReplySet, Request, SessionId};
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
        .or_else(|| crate::integrations::home_dir().ok().map(|h| h.join(".claude")))?
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
    // The service went quiet (`crate::dormancy`) while this hook's plan was
    // queued: nothing more of the session is read.
    if !core.adapters().collecting() {
        return;
    }
    let principal = adapters.claude.principal();
    let event = payload.event();
    if event == "SessionEnd" {
        if let Ok(mut watches) = adapters.transcripts.lock() {
            watches.remove(agent);
        }
        adapters.forget_transcripts(|id| id != agent);
        return;
    }
    // The transcript the session writes, remembered whether or not the deck
    // reads the session, so a watcher can start the moment it does
    // (`crate::reader::follow`).
    let transcript = payload.str("session_id").and_then(|cli_session| {
        payload
            .str("transcript_path")
            .and_then(|p| permitted_path(p, cli_session))
            .map(|path| (cli_session, path))
    });
    if let Some((cli_session, path)) = &transcript {
        adapters.remember_transcript(agent, cli_session, path);
    }
    // History, tool rows and images only for a session the deck reads
    // (`crate::reader`): an unread session's state, cards and replies come
    // from the hook's own plan.
    if !core.adapters().reads(agent) {
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
    let Some((cli_session, path)) = transcript else {
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

/// The transcript a Claude session writes, by its key and working
/// directory (`claude:<session id>` under `projects/<slug>/`): for a session
/// whose hooks have named none to this daemon yet.
fn transcript_of(agent: &deck_model::Agent) -> Option<(String, PathBuf)> {
    let id = agent
        .session
        .key
        .as_deref()
        .and_then(|k| k.strip_prefix("claude:"))?;
    let cwd = agent.session.cwd.as_deref()?;
    let dir = crate::liveness::claude_dir()?;
    let candidate = crate::liveness::transcript_path(&dir, cwd, id);
    let path = candidate.to_str().and_then(|p| permitted_path(p, id))?;
    Some((id.to_string(), path))
}

/// Recover already-running background workflows when deckd starts between
/// hooks, for a session the deck reads (`crate::reader`).
pub(crate) fn seed(
    adapters: &Adapters,
    core: &CoreHandle,
    session: SessionId,
    agent: &deck_model::Agent,
) {
    if !core.adapters().collecting() || !core.adapters().reads(&agent.id) {
        return;
    }
    let Some((id, path)) = transcript_of(agent) else {
        return;
    };
    start_watch(adapters, core, session, &agent.id, &id, path, None);
}

/// A watcher for every Claude session `reader` names that has none
/// (`crate::reader::follow`): from the transcript its hooks named, else the
/// one its key and working directory name. A session whose turn is over is
/// read as finished, the way its `Stop` would have marked the watcher. A
/// reader that is every session starts nothing: the hooks start every
/// watcher then, as they always did.
pub(crate) fn open(api: &Api, reader: &Reader) {
    let Some(agents) = reader.agents() else {
        return;
    };
    if !api.core.adapters().collecting() {
        return;
    }
    let Some(session) = api.adapters.claude_session() else {
        return;
    };
    let snapshot = api.bus.snapshot();
    for id in agents {
        let Some(agent) = snapshot
            .agents
            .get(id)
            .filter(|a| a.owner.session == session)
        else {
            continue;
        };
        if watching(&api.adapters, id) {
            continue;
        }
        let Some((cli_session, path)) = api
            .adapters
            .transcript(id)
            .or_else(|| transcript_of(agent))
        else {
            continue;
        };
        let finished = !matches!(agent.state, AgentState::Working | AgentState::Question);
        start_watch(
            &api.adapters,
            &api.core,
            session,
            id,
            &cli_session,
            path,
            Some(finished),
        );
    }
}

/// Whether a watcher is reading this agent's transcript now.
fn watching(adapters: &Adapters, agent: &AgentId) -> bool {
    adapters
        .transcripts
        .lock()
        .is_ok_and(|watches| watches.get(agent).is_some_and(|w| !w.handle.is_finished()))
}

/// What one read of a transcript sends the core.
#[derive(Debug, PartialEq)]
enum Batch {
    /// Nothing changed since the last read.
    Nothing,
    /// The session's whole history, in place of the one the store kept:
    /// the first read of a session fed lazily (`crate::reader`). The kept
    /// one is whatever the store had when the deck last read the session,
    /// and the messages missed since would otherwise land after newer ones.
    Replace(Vec<(MessageMeta, String)>),
    /// Each message that changed since the last read, in order.
    Each(Vec<(MessageMeta, String)>),
}

/// The watcher's decision for one read's messages: a lazily fed session's
/// first read that found any is a [`Batch::Replace`], everything after it
/// (and everything of an eager feed, as before) the messages that changed.
fn batch(
    lazy: bool,
    replaced: bool,
    messages: Vec<(MessageMeta, String)>,
    previous: &BTreeMap<String, (MessageMeta, String)>,
) -> Batch {
    if lazy && !replaced && !messages.is_empty() {
        return Batch::Replace(messages);
    }
    let changed: Vec<_> = messages
        .into_iter()
        .filter(|(message, text)| {
            previous
                .get(&message.id)
                .is_none_or(|(m, t)| m != message || t != text)
        })
        .collect();
    if changed.is_empty() {
        Batch::Nothing
    } else {
        Batch::Each(changed)
    }
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
    let order = adapters.order_lock();
    let finished = Arc::new(AtomicBool::new(finished_update.unwrap_or(false)));
    let turn_finished = finished.clone();
    let task = tokio::spawn(async move {
        let mut previous = BTreeMap::new();
        // A lazily fed session's history went to the store whole once.
        let mut replaced = false;
        let mut stamp = None;
        let mut workflows = Some(crate::claude_workflows::Tracker::new(
            &watched,
            &session_name,
        ));
        let mut previous_runs = Vec::new();
        let mut poll = 0u8;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            // Off with the hooks, with the collectors (`crate::dormancy`), and
            // once the deck no longer reads the session (`crate::reader`).
            let reader = core.adapters().reader();
            if !config.load().hooks.enabled
                || !core.adapters().collecting()
                || !reader.reads(&agent_id)
            {
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
            if done {
                for (message, _) in &mut updates {
                    message.streaming = false;
                    if message.activity == Some(deck_model::workflow::ActivityStatus::Running) {
                        message.activity = Some(deck_model::workflow::ActivityStatus::Stopped);
                    }
                }
            }
            let updates = match batch(reader.lazy(), replaced, updates, &previous) {
                Batch::Nothing => Vec::new(),
                Batch::Each(updates) => updates,
                Batch::Replace(messages) => {
                    // Under the plans' lock, as every post of a watcher is.
                    let posted = {
                        let _order = order.lock().await;
                        crate::adapters::rt::budgeted(core.chat_replace(
                            session,
                            agent_id.clone(),
                            messages.clone(),
                        ))
                        .await
                    };
                    let Some(Ok(_)) = posted else {
                        return;
                    };
                    replaced = true;
                    previous = messages
                        .into_iter()
                        .map(|(message, text)| (message.id.clone(), (message, text)))
                        .collect();
                    Vec::new()
                }
            };
            for (message, text) in updates {
                let value = (message.clone(), text.clone());
                let request = ReplySet {
                    agent: agent_id.clone(),
                    text,
                    message: Some(message.clone()),
                    speak: false,
                    announce: None,
                    lane: None,
                };
                // Never inside a hook's plan: a `Stop` stores its reply and
                // then turns the session ready, and a message posted between
                // the two would be the reply on file when the ready line is
                // spoken.
                let posted = {
                    let _order = order.lock().await;
                    crate::adapters::rt::budgeted(core.api(
                        principal.clone(),
                        session,
                        false,
                        Request::Reply(request),
                    ))
                    .await
                };
                match posted {
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

    /// The watcher posts under the hook plans' lock. A message of its own
    /// landing between a `Stop`'s reply and its `ready` would be the reply
    /// on file when the store speaks the ready line.
    #[tokio::test]
    async fn the_watcher_never_posts_inside_a_hook_plan() {
        let dir = std::env::temp_dir().join(format!("deckd-chat-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let line = serde_json::json!({"type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "All done."}]}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let adapters = Adapters::default();
        let (core, rx) = CoreHandle::detached();
        let agent = AgentId("ag_1".into());

        let plan = adapters.order().await;
        start_watch(
            &adapters,
            &core,
            SessionId(1),
            &agent,
            "s",
            path,
            Some(true),
        );
        // Past the first poll (500 ms): the message is read and waits.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(
            rx.try_recv().is_err(),
            "nothing is posted while a plan runs"
        );
        drop(plan);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let posted = loop {
            if let Ok(msg) = rx.try_recv() {
                break msg;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "posted once the plan is done"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let crate::core::CoreMsg::Api { req, .. } = posted else {
            panic!("an api call");
        };
        assert!(matches!(*req, Request::Reply(ref r) if r.text == "All done."));
        drop(adapters);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With no deck for ten minutes (`crate::dormancy`) nothing of a session
    /// is read: a watcher that was running ends at its next look instead of
    /// reading on, and a hook's plan that was queued when the service went
    /// quiet posts not even its tool row.
    #[tokio::test]
    async fn a_quiet_service_reads_no_transcript() {
        let dir = std::env::temp_dir().join(format!("deckd-chat-quiet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(&path, "").unwrap();
        let adapters = Adapters::default();
        let (core, rx) = CoreHandle::detached();
        let agent = AgentId("ag_1".into());
        start_watch(
            &adapters,
            &core,
            SessionId(1),
            &agent,
            "s",
            path.clone(),
            None,
        );
        let handle = adapters.transcripts.lock().unwrap()[&agent].handle.clone();
        core.adapters().set_gate(crate::dormancy::Gate::shut());
        // A line the watcher would have posted at its next look.
        let line = serde_json::json!({"type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "Read me."}]}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !handle.is_finished() {
            assert!(std::time::Instant::now() < deadline, "the watcher ends");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            rx.try_recv().is_err(),
            "nothing was read out of the transcript"
        );
        // A tool row is the first thing a plan's `observe` posts; with the
        // core not answering, posting it would also take the core budget.
        let tool = HookPayload(serde_json::json!({"hook_event_name": "PostToolUse",
            "session_id": "s", "tool_use_id": "toolu_1", "tool_name": "Bash",
            "tool_input": {"command": "ls"}, "tool_response": "a\nb"}));
        let started = std::time::Instant::now();
        observe(&adapters, &core, SessionId(1), &agent, &tool).await;
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "no tool row");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn message(id: &str, text: &str) -> (MessageMeta, String) {
        let meta = MessageMeta {
            summary: None,
            images: Vec::new(),
            activity: None,
            runs: None,
            plan: None,
            id: id.into(),
            role: ChatRole::Assistant,
            streaming: false,
        };
        (meta, text.into())
    }

    /// The watcher's decision (`crate::reader`): an eager feed posts every
    /// message that changed, as it always did; a lazily fed session's first
    /// read with anything in it is the whole history at once, and the reads
    /// after it are what changed.
    #[test]
    fn a_lazy_sessions_first_read_replaces_and_the_reads_after_it_add() {
        let none = BTreeMap::new();
        assert_eq!(
            batch(false, false, vec![message("a", "A")], &none),
            Batch::Each(vec![message("a", "A")])
        );
        let both = vec![message("a", "A"), message("b", "B")];
        assert_eq!(batch(true, false, both.clone(), &none), Batch::Replace(both));
        assert_eq!(batch(true, false, Vec::new(), &none), Batch::Nothing, "nothing to replace with yet");
        let previous: BTreeMap<String, (MessageMeta, String)> = [message("a", "A"), message("b", "B")]
            .into_iter()
            .map(|m| (m.0.id.clone(), m))
            .collect();
        let read = vec![message("a", "A"), message("b", "B, more"), message("c", "C")];
        assert_eq!(
            batch(true, true, read, &previous),
            Batch::Each(vec![message("b", "B, more"), message("c", "C")])
        );
        assert_eq!(batch(true, true, vec![message("a", "A")], &previous), Batch::Nothing);
    }

    /// A tool row as a hook carries it, with no transcript to follow.
    fn tool_hook() -> HookPayload {
        HookPayload(serde_json::json!({"hook_event_name": "PostToolUse",
            "session_id": "s", "tool_use_id": "toolu_1", "tool_name": "Bash",
            "tool_input": {"command": "ls"}, "tool_response": "a\nb"}))
    }

    /// A session the deck does not read gets no tool row and no watcher
    /// from its hooks (`crate::reader`); once it is read, the row is posted
    /// as before.
    #[tokio::test]
    async fn an_unread_session_gets_no_tool_row_and_no_watcher() {
        let adapters = Arc::new(Adapters::default());
        let (core, rx) = CoreHandle::detached();
        let agent = AgentId("ag_1".into());
        core.adapters()
            .set_reader(Reader::only([AgentId("ag_2".into())].into()));
        let started = std::time::Instant::now();
        observe(&adapters, &core, SessionId(1), &agent, &tool_hook()).await;
        assert!(started.elapsed() < Duration::from_millis(100), "nothing waited on the core");
        assert!(rx.try_recv().is_err(), "no tool row");
        assert!(adapters.transcripts.lock().unwrap().is_empty(), "no watcher");

        core.adapters().set_reader(Reader::only([agent.clone()].into()));
        let posting = {
            let (adapters, core, agent) = (adapters.clone(), core.clone(), agent.clone());
            tokio::spawn(async move {
                observe(&adapters, &core, SessionId(1), &agent, &tool_hook()).await;
            })
        };
        let posted = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .expect("the tool row");
        let crate::core::CoreMsg::Api { req, reply, .. } = posted else {
            panic!("an api call");
        };
        assert!(
            matches!(*req, Request::Reply(ref r) if r.message.as_ref().is_some_and(|m| m.id == "tool:toolu_1"))
        );
        let _ = reply.send(Ok(serde_json::json!({"rev": 1})));
        posting.await.unwrap();
    }

    /// The core's next message, within two seconds.
    async fn next(rx: &crossbeam_channel::Receiver<crate::core::CoreMsg>) -> crate::core::CoreMsg {
        let rx = rx.clone();
        tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .expect("the watcher posted")
    }

    /// A lazily fed session's first read goes to the core whole, in order,
    /// as one replacement; a message written after it is posted on its own;
    /// and once the deck no longer reads the session, its watcher ends at
    /// its next look and reads nothing more.
    #[tokio::test]
    async fn a_read_session_is_replaced_whole_then_followed_until_the_deck_stops_reading_it() {
        let dir = std::env::temp_dir().join(format!("deckd-chat-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let line = |uuid: &str, kind: &str, text: &str| {
            serde_json::json!({"type": kind, "uuid": uuid, "sessionId": "s",
                "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": text}]}})
            .to_string()
        };
        std::fs::write(
            &path,
            format!("{}\n{}\n", line("u1", "user", "Run it."), line("a1", "assistant", "Ran it.")),
        )
        .unwrap();
        let adapters = Adapters::default();
        let (core, rx) = CoreHandle::detached();
        let agent = AgentId("ag_1".into());
        core.adapters().set_reader(Reader::only([agent.clone()].into()));
        start_watch(&adapters, &core, SessionId(1), &agent, "s", path.clone(), Some(true));
        let crate::core::CoreMsg::ChatReplace { session, agent: replaced, messages, reply } =
            next(&rx).await
        else {
            panic!("one replacement");
        };
        assert_eq!((session, &replaced), (SessionId(1), &agent));
        let ids: Vec<&str> = messages.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, ["u1", "a1"]);
        let _ = reply.send(Ok(serde_json::json!({"rev": 1})));

        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, format!("{}\n", line("u2", "user", "Again.")).as_bytes()).unwrap();
        let crate::core::CoreMsg::Api { req, reply, .. } = next(&rx).await else {
            panic!("one message");
        };
        assert!(matches!(*req, Request::Reply(ref r) if r.message.as_ref().is_some_and(|m| m.id == "u2")));
        let _ = reply.send(Ok(serde_json::json!({"rev": 2})));

        let handle = adapters.transcripts.lock().unwrap()[&agent].handle.clone();
        core.adapters().set_reader(Reader::only(Default::default()));
        std::io::Write::write_all(&mut file, format!("{}\n", line("a2", "assistant", "Not read.")).as_bytes()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !handle.is_finished() {
            assert!(std::time::Instant::now() < deadline, "the watcher ends");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(rx.try_recv().is_err(), "nothing read after the deck stopped");
        let _ = std::fs::remove_dir_all(dir);
    }

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
