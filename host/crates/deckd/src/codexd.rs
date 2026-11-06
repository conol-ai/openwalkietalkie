//! `deckd::codexd` — the client of Codex's **shared local app-server
//! daemon**, and the primary Codex integration from codex-cli 0.161 on.
//!
//! Since 0.161 the Codex TUI runs through one app-server daemon per machine
//! (`codex --no-daemon` opts out; `codex agents` browses it), reachable on a
//! Unix socket under `$CODEX_HOME/app-server-control/` that speaks the
//! app-server's JSON-RPC over **WebSocket**. That daemon knows every live
//! thread, including the ones that started before deckd did, and it is the
//! one place a spoken order can reach a Codex session without a terminal:
//!
//! - `thread/loaded/list` + `thread/resume` are the roster — a thread per
//!   agent, keyed `codex:<thread id>`, the same key the hooks adapter uses,
//!   so a thread this client owns is one the hooks path leaves alone
//!   (`rt::Adapters::daemon_owns`);
//! - `turn/started` / `turn/completed` / `item/*` are the state, the title
//!   (the user message that opened the turn) and the last reply;
//! - the server's own requests — `item/commandExecution/requestApproval`,
//!   `item/fileChange/requestApproval`, `item/permissions/requestApproval`,
//!   `item/tool/requestUserInput` — are the cards, answered with the JSON-RPC
//!   response the server is blocked on;
//! - asynchronous `agentMessage.questions` are persistent question cards,
//!   answered by a structured user message and restored from thread history;
//! - `turn/steer` (a turn is running) and `turn/start` (idle) are the orders.
//!
//! The agents are **poll**-capability virtual agents owned by the Codex
//! adapter's own internal session (§4.6, §4.15): answers and orders reach
//! this client through `agent.wait`, exactly as they reach a third party's
//! script, so nothing in the core had to learn a new delivery mode.
//!
//! Shape: a pure [`Model`] turns notifications, server requests, answers
//! and orders into [`Effect`]s, and [`run`] is the IO shell that carries
//! them to the core and the socket — the same actor/normaliser split as the
//! hook adapters, for the same reason (every rule below is a table a test
//! can read).
//!
//! What is deliberately *not* here: the hooks path is kept as the fallback
//! (`--no-daemon` sessions, older CLIs); nothing is executed on the server's
//! behalf. Unhandled server requests are left to the native client: an
//! unsolicited error from this subscriber would dismiss its prompt too.
//!
//! Content discipline (§11): thread names, prompts, commands and replies are
//! content and never reach a log line; the log carries thread ids, methods
//! and counts.
use crate::api::Api;
use deck_model::session_controls::{ModelOption, SessionControls};
use deck_model::voice::{intent, Intent};
use deck_model::{
    sanitize, AdapterKind, AgentId, AgentRegister, AgentState, AgentUpdate, Answer, AnswerCap,
    Caps, CardClear, CardId, CardKind, CardSet, Choice, Deliver, Kind, OrderCap, Origin,
    OriginKind, Principal, ReplySet, Request, SessionId, TitleSource,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;

mod history;
mod questions;

/// `TITLE_MAX` of §5.1 ("the first 64 B of the prompt"), shared with the
/// hooks adapter.
pub const TITLE_MAX: usize = 64;
/// The choices an approval card offers, in order (`Allow` is `rec`).
pub const ALLOW: &str = "Allow";
pub const ALLOW_SESSION: &str = "Allow for session";
pub const DENY: &str = "Deny";
/// `item/permissions/requestApproval` grants rather than allows.
pub const GRANT: &str = "Grant";
/// How often the loaded-thread list is re-read: `thread/started` reaches
/// only the connection that started the thread, so a TUI opened after this
/// client connected is found by the poll.
pub const ROSTER_S: u64 = 10;
/// A quiet `agent.wait` is re-armed at this cadence (the mailbox TTL is
/// 600 s; a shorter wait just means more turns of the loop).
const WAIT_S: u64 = 300;
/// Reconnect backoff bounds: a daemon that is simply not running is retried
/// slowly and quietly; a connection that dropped is retried fast.
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const NOT_RUNNING_RETRY: Duration = Duration::from_secs(15);
/// A request the daemon has not answered by then is a dead connection.
const RPC_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Socket discovery
// ---------------------------------------------------------------------------

/// `[codex] daemon_socket`, else `$CODEX_HOME/app-server-control/
/// app-server-control.sock`, else `~/.codex/…`. The file is a symlink into
/// `/tmp/codex-daemon-<uid>/`, which `connect` follows itself.
#[must_use]
pub fn socket_path(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("CODEX_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".codex"))
        })?;
    Some(
        home.join("app-server-control")
            .join("app-server-control.sock"),
    )
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// What the shell does with the core or the socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Controls { thread: String, settled: bool, controls: SessionControls },
    Message { thread: String, message: deck_model::MessageMeta, text: String },
    /// `agent.register` for this thread; the shell calls back
    /// [`Model::registered`] with the id.
    Register {
        thread: String,
        name: String,
        cwd: Option<String>,
    },
    Update {
        thread: String,
        state: Option<AgentState>,
        title: Option<String>,
    },
    Reply {
        thread: String,
        text: String,
    },
    /// `agent.card.set` (poll delivery); the shell calls back
    /// [`Model::carded`] with the card id.
    Card {
        thread: String,
        spec: CardSpec,
    },
    /// `agent.card.clear`: the card is named here because the model has
    /// already forgotten the request it was for.
    Clear {
        thread: String,
        agent: Option<AgentId>,
        card: CardId,
    },
    /// `agent.release`: the agent is named here because the model has
    /// already forgotten the thread.
    Release {
        thread: String,
        agent: Option<AgentId>,
    },
    /// A JSON-RPC response to one of the server's requests.
    Respond {
        id: Value,
        result: Value,
    },
    /// A JSON-RPC request of ours (`turn/steer`, `turn/start`).
    Request {
        method: &'static str,
        params: Value,
    },
    /// Deliver an async question reply, retaining the question on failure.
    AsyncReply { thread: String, key: String, text: String },
    /// The account's rate-limit windows, for the Usage face
    /// (`crate::usage`): `account/rateLimits/updated`.
    Usage(Vec<crate::usage::Window>),
    /// Authentication changed: discard the previous account's windows
    /// and read a new account/usage pair, even inside the refresh gap.
    RefreshUsage,
}

/// A card as the model wants it, before the agent id is known to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardSpec {
    /// The model's own key for the card: the server request id, plus a
    /// question index for a multi-question `requestUserInput`.
    pub request: String,
    pub text: String,
    pub choices: Vec<String>,
    pub rec: Option<u8>,
    pub free_text: bool,
    pub origin: OriginKind,
    /// Async questions remain pending across turns and have no hold timeout.
    pub persistent: bool,
}

/// A server request this client is answering through a card.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    /// The JSON-RPC id to answer.
    id: Value,
    kind: PendingKind,
    /// Cards raised for it, by the model's request key → the core's card id.
    cards: BTreeMap<String, Option<CardId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingKind {
    /// `accept | acceptForSession | decline`.
    Approval,
    /// `grant | deny`.
    Permission,
    /// `{ answers: { <wire id>: { answers: [labels] } } }`, one card per
    /// question, issued in order.
    UserInput {
        questions: Vec<WireQuestion>,
        answers: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Thread {
    controls: SessionControls,
    messages: Vec<(deck_model::MessageMeta, String)>,
    dirty_messages: Vec<String>,
    agent: Option<AgentId>,
    active_turn: Option<String>,
    last_message: Option<String>,
    /// The `userMessage` item the title was last taken from, so the
    /// completed echo of a started item does not re-title.
    last_title_item: Option<String>,
    pending: Vec<Pending>,
    async_questions: questions::Questions,
    /// The underlying activity while an asynchronous question is visible.
    base_state: Option<AgentState>,
}

/// The daemon-side mirror of the threads this client follows.
#[derive(Debug, Default)]
pub struct Model {
    catalog: Vec<ModelOption>,
    settings_unavailable: bool,
    usage_account: Option<String>,
    threads: BTreeMap<String, Thread>,
    /// A monotonically increasing suffix for the model's own request keys,
    /// so two cards of one request never share one.
    seq: u64,
}

impl Model {
    fn settings(&mut self, thread: &str, value: &Value) -> Vec<Effect> {
        let Some(t) = self.threads.get_mut(thread) else { return Vec::new(); };
        let Some(model) = value["model"].as_str() else { return Vec::new(); };
        let controls = SessionControls {
            model: model.into(),
            effort: value.get("effort").and_then(Value::as_str).or_else(|| value.get("reasoningEffort").and_then(Value::as_str)).unwrap_or_default().into(),
            models: self.catalog.clone(),
            editable: !self.settings_unavailable && !self.catalog.is_empty(),
            note: if self.settings_unavailable { "Update Codex to enable model and effort selection.".into() }
                else if self.catalog.is_empty() { "Model catalog unavailable. Reconnect Codex and try again.".into() }
                else { String::new() },
        };
        if t.controls == controls { return Vec::new(); }
        t.controls = controls.clone();
        vec![Effect::Controls { thread: thread.into(), settled: false, controls }]
    }

    /// Validate a device event against this connection's own thread and catalog.
    fn settings_change(&self, agent: &str, model: &str, effort: &str) -> Option<(String, Value)> {
        let (thread, t) = self.threads.iter().find(|(_, t)| t.agent.as_ref().is_some_and(|a| a.0 == agent))?;
        if !t.controls.editable { return None; }
        let option = self.catalog.iter().find(|m| m.id == model)?;
        if !option.efforts.iter().any(|e| e == effort) && !(effort.is_empty() && option.efforts.is_empty()) { return None; }
        let mut params = json!({"threadId": thread, "model": model});
        if !effort.is_empty() { params["effort"] = json!(effort); }
        Some((thread.clone(), params))
    }

    fn record_message(&mut self, thread: &str, message: deck_model::MessageMeta, text: String) {
        let Some(t) = self.threads.get_mut(thread) else { return; };
        let text = sanitize::cut(&text, 64 * 1024);
        if let Some(old) = t.messages.iter_mut().find(|(m, _)| m.id == message.id) {
            if old == &(message.clone(), text.clone()) { return; }
            *old = (message.clone(), text);
        } else {
            t.messages.push((message.clone(), text));
            if t.messages.len() > 64 { t.messages.remove(0); }
        }
        if !t.dirty_messages.contains(&message.id) { t.dirty_messages.push(message.id); }
    }

    /// At most one delivery per item per 400 ms, retaining arrival order.
    pub fn flush_messages(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (thread, t) in &mut self.threads {
            for id in std::mem::take(&mut t.dirty_messages) {
                if let Some((message, text)) = t.messages.iter().find(|(m, _)| m.id == id) {
                    effects.push(Effect::Message { thread: thread.clone(), message: message.clone(), text: text.clone() });
                }
            }
        }
        effects
    }

    fn chat_notification(&mut self, thread: &str, method: &str, params: &Value) {
        if method == "turn/plan/updated" {
            if let (Some(turn), Some(plan)) = (params["turnId"].as_str(), deck_model::workflow::Plan::codex(params)) {
                if self.active_turn(thread).is_some_and(|active| active != turn) {
                    return;
                }
                let text = plan.text();
                self.record_message(thread, deck_model::MessageMeta { summary: None, images: Vec::new(),
                    activity: None,
                    id: format!("plan:{turn}"),
                    role: deck_model::ChatRole::Tool,
                    streaming: false,
                    runs: None, plan: Some(plan),
                }, text);
            }
        } else if matches!(method, "item/started" | "item/completed") {
            if let Some((meta, text)) = chat_item(&params["item"], method == "item/started") {
                self.record_message(thread, meta, text);
            }
        } else if matches!(method, "item/agentMessage/delta" | "item/commandExecution/outputDelta") {
            let Some(id) = params["itemId"].as_str() else { return; };
            let Some(delta) = params["delta"].as_str() else { return; };
            let previous = self.threads.get(thread).and_then(|t| t.messages.iter().find(|(m, _)| m.id == id)).cloned();
            let (meta, mut text) = previous.unwrap_or_else(|| (deck_model::MessageMeta { summary: None, images: Vec::new(),
                activity: None,
                runs: None, plan: None,
                id: id.into(), role: if method == "item/agentMessage/delta" { deck_model::ChatRole::Assistant } else { deck_model::ChatRole::Tool }, streaming: true,
            }, String::new()));
            if text == "Working…" { text.clear(); }
            if text.len() < 64 * 1024 { text.push_str(delta); }
            self.record_message(thread, meta, text);
        } else if matches!(method, "turn/completed" | "turn/failed" | "turn/aborted") {
            if let Some(t) = self.threads.get_mut(thread) {
                for (meta, _) in &mut t.messages {
                    if meta.streaming {
                        meta.streaming = false;
                        if meta.role == deck_model::ChatRole::Tool {
                            meta.activity = Some(deck_model::workflow::ActivityStatus::Stopped);
                        }
                        if !t.dirty_messages.contains(&meta.id) { t.dirty_messages.push(meta.id.clone()); }
                    }
                }
            }
        }
    }
    /// The agent a thread is on the roster as.
    #[must_use]
    pub fn agent(&self, thread: &str) -> Option<&AgentId> {
        self.threads.get(thread).and_then(|t| t.agent.as_ref())
    }

    /// Every thread this client follows.
    #[must_use]
    pub fn threads(&self) -> Vec<String> {
        self.threads.keys().cloned().collect()
    }

    /// Is a turn in flight on this thread?
    #[must_use]
    pub fn active_turn(&self, thread: &str) -> Option<&str> {
        self.threads
            .get(thread)
            .and_then(|t| t.active_turn.as_deref())
    }

    /// The shell resolved a `Register`.
    pub fn registered(&mut self, thread: &str, agent: AgentId) {
        self.threads.entry(thread.to_string()).or_default().agent = Some(agent);
    }

    /// The shell set a card for `request`.
    pub fn carded(&mut self, thread: &str, request: &str, card: CardId) {
        if let Some(t) = self.threads.get_mut(thread) {
            t.async_questions.carded(request, card);
            for pending in &mut t.pending {
                if let Some(slot) = pending.cards.get_mut(request) {
                    *slot = Some(card);
                }
            }
        }
    }

    /// The daemon's loaded-thread list, reconciled: new threads are
    /// registered, threads no longer loaded are released. `names` is what
    /// `thread/resume` said about each (name, cwd, active).
    pub fn loaded(&mut self, listed: &[LoadedThread]) -> Vec<Effect> {
        let mut effects = Vec::new();
        let gone: Vec<String> = self
            .threads
            .keys()
            .filter(|id| !listed.iter().any(|l| &l.id == *id))
            .cloned()
            .collect();
        for id in gone {
            effects.extend(self.release(&id));
        }
        for thread in listed {
            let known = self.threads.contains_key(&thread.id);
            if !known {
                let state = if thread.active { AgentState::Working } else { AgentState::Idle };
                self.threads.insert(thread.id.clone(), Thread {
                    async_questions: thread.async_questions.clone(),
                    active_turn: thread.active_turn.clone(),
                    base_state: Some(state),
                    ..Thread::default()
                });
                effects.push(Effect::Register {
                    thread: thread.id.clone(),
                    name: name_of(thread.name.as_deref(), thread.cwd.as_deref(), &thread.id),
                    cwd: thread.cwd.clone(),
                });
                effects.push(Effect::Update {
                    thread: thread.id.clone(),
                    state: Some(state),
                    title: None,
                });
                for (meta, text) in &thread.messages { self.record_message(&thread.id, meta.clone(), text.clone()); }
                effects.extend(self.show_async_question(&thread.id));
            }
            if let Some(settings) = &thread.settings { effects.extend(self.settings(&thread.id, settings)); }
        }
        effects.extend(self.flush_messages());
        effects
    }

    /// A notification from the daemon.
    pub fn notification(&mut self, method: &str, params: &Value) -> Vec<Effect> {
        let mut effects = self.notification_inner(method, params);
        if let Some(thread) = thread_id(method, params) {
            if matches!(method, "item/started" | "item/completed") {
                if let Some(t) = self.threads.get_mut(&thread) {
                    if let Some(card) = t.async_questions.observe(&params["item"]) {
                        effects.push(Effect::Clear { thread: thread.clone(), agent: t.agent.clone(), card });
                    }
                }
            }
            if let Some(t) = self.threads.get_mut(&thread) {
                for effect in &mut effects {
                    if let Effect::Update { state: Some(state), .. } = effect {
                        t.base_state = Some(*state);
                        if t.async_questions.is_visible() { *state = AgentState::Question; }
                    }
                }
            }
            effects.extend(self.show_async_question(&thread));
        }
        effects
    }

    fn show_async_question(&mut self, thread: &str) -> Vec<Effect> {
        let Some(t) = self.threads.get_mut(thread) else { return Vec::new(); };
        if !t.pending.is_empty() { return Vec::new(); }
        t.async_questions.show().map(|spec| Effect::Card { thread: thread.into(), spec }).into_iter().collect()
    }

    fn async_reply_finished(&mut self, thread: &str, key: &str, delivered: bool) -> Vec<Effect> {
        if delivered {
            if let Some(t) = self.threads.get_mut(thread) { t.async_questions.resolve(key); }
        }
        let mut effects = self.threads.get(thread).map(|t| Effect::Update {
            thread: thread.into(), state: t.base_state, title: None,
        }).into_iter().collect::<Vec<_>>();
        effects.extend(self.show_async_question(thread));
        effects
    }

    fn notification_inner(&mut self, method: &str, params: &Value) -> Vec<Effect> {
        if method == "account/updated" {
            self.usage_account = None;
            return vec![Effect::RefreshUsage];
        }
        // The one notification about the account rather than a thread:
        // the server sends it after every turn, with or without the
        // `rateLimits` wrapper (`crate::usage::codex_windows` reads both).
        if method == "account/rateLimits/updated" {
            let windows = crate::usage::codex_windows(params);
            return if windows.is_empty() {
                Vec::new()
            } else {
                vec![Effect::Usage(windows)]
            };
        }
        let thread = thread_id(method, params).or_else(|| {
            if method != "turn/plan/updated" { return None; }
            let turn = params["turnId"].as_str()?;
            let mut matches = self.threads.iter().filter(|(_, t)| t.active_turn.as_deref() == Some(turn));
            let (id, _) = matches.next()?;
            matches.next().is_none().then(|| id.clone())
        });
        let Some(thread) = thread else {
            return Vec::new();
        };
        if method == "thread/settings/updated" { return self.settings(&thread, &params["threadSettings"]); }
        self.chat_notification(&thread, method, params);
        match method {
            "thread/started" => {
                if self.threads.contains_key(&thread) {
                    return Vec::new();
                }
                self.threads.insert(thread.clone(), Thread::default());
                let info = &params["thread"];
                vec![
                    Effect::Register {
                        thread: thread.clone(),
                        name: name_of(info["name"].as_str(), info["cwd"].as_str(), &thread),
                        cwd: info["cwd"].as_str().map(str::to_string),
                    },
                    Effect::Update {
                        thread,
                        state: Some(AgentState::Idle),
                        title: None,
                    },
                ]
            }
            "thread/closed" | "thread/archived" | "thread/deleted" => self.release(&thread),
            "thread/status/changed" => {
                if !self.threads.contains_key(&thread) {
                    return Vec::new();
                }
                match params["status"]["type"].as_str() {
                    Some("active") => vec![Effect::Update {
                        thread,
                        state: Some(AgentState::Working),
                        title: None,
                    }],
                    Some("idle") => {
                        // A thread going idle after a turn is `ready`
                        // (turn/completed said so); one with no turn behind
                        // it is plainly idle.
                        let had_turn = self
                            .threads
                            .get(&thread)
                            .is_some_and(|t| t.last_message.is_some());
                        vec![Effect::Update {
                            thread,
                            state: Some(if had_turn {
                                AgentState::Ready
                            } else {
                                AgentState::Idle
                            }),
                            title: None,
                        }]
                    }
                    Some("notLoaded") => self.release(&thread),
                    Some("systemError") => vec![Effect::Update {
                        thread,
                        state: Some(AgentState::Error),
                        title: None,
                    }],
                    _ => Vec::new(),
                }
            }
            "turn/started" => {
                let Some(t) = self.threads.get_mut(&thread) else {
                    return Vec::new();
                };
                let id = params["turn"]["id"].as_str().unwrap_or_default();
                t.active_turn = (!id.is_empty()).then(|| id.to_string());
                vec![Effect::Update {
                    thread,
                    state: Some(AgentState::Working),
                    title: None,
                }]
            }
            "turn/completed" | "turn/failed" | "turn/aborted" => {
                let Some(t) = self.threads.get_mut(&thread) else {
                    return Vec::new();
                };
                t.active_turn = None;
                let status = params["turn"]["status"].as_str().unwrap_or(match method {
                    "turn/failed" => "failed",
                    "turn/aborted" => "interrupted",
                    _ => "completed",
                });
                let mut effects = Vec::new();
                // The turn is over: every approval it was waiting on is moot.
                for pending in std::mem::take(&mut t.pending) {
                    for card in pending.cards.values().flatten() {
                        effects.push(Effect::Clear {
                            thread: thread.clone(),
                            agent: t.agent.clone(),
                            card: *card,
                        });
                    }
                }
                match status {
                    "failed" => effects.push(Effect::Update {
                        thread,
                        state: Some(AgentState::Error),
                        title: None,
                    }),
                    "interrupted" => effects.push(Effect::Update {
                        thread,
                        state: Some(AgentState::Idle),
                        title: None,
                    }),
                    _ => {
                        if let Some(text) = t.last_message.clone() {
                            effects.push(Effect::Reply {
                                thread: thread.clone(),
                                text,
                            });
                        }
                        effects.push(Effect::Update {
                            thread,
                            state: Some(AgentState::Ready),
                            title: None,
                        });
                    }
                }
                effects
            }
            "item/started" | "item/completed" => {
                let Some(t) = self.threads.get_mut(&thread) else {
                    return Vec::new();
                };
                let item = &params["item"];
                match item["type"].as_str().unwrap_or("") {
                    "userMessage" | "user_message"
                        if method == "item/started" || method == "item/completed" =>
                    {
                        let Some(text) = user_message_text(item) else {
                            return Vec::new();
                        };
                        // Once per message: the completed echo of a started
                        // item carries the same text.
                        if method == "item/completed"
                            && params["item"]["id"].is_string()
                            && t.last_title_item.as_deref() == item["id"].as_str()
                        {
                            return Vec::new();
                        }
                        t.last_title_item = item["id"].as_str().map(str::to_string);
                        vec![Effect::Update {
                            thread,
                            state: Some(AgentState::Working),
                            // A clarification answer continues the same task;
                            // its transport envelope is never a roster title.
                            title: questions::parse_reply(&text).is_none()
                                .then(|| sanitize::cut(&sanitize::collapse(&text), TITLE_MAX)),
                        }]
                    }
                    "agentMessage" | "agent_message" if method == "item/completed" => {
                        if let Some(text) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
                            t.last_message = Some(text.to_string());
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            "serverRequest/resolved" => {
                // Answered elsewhere (the TUI, another client): the card is
                // moot and the request must not be answered twice.
                let Some(t) = self.threads.get_mut(&thread) else {
                    return Vec::new();
                };
                let id = &params["requestId"];
                let mut effects = Vec::new();
                t.pending.retain(|p| {
                    if &p.id == id {
                        for card in p.cards.values().flatten() {
                            effects.push(Effect::Clear {
                                thread: thread.clone(),
                                agent: t.agent.clone(),
                                card: *card,
                            });
                        }
                        false
                    } else {
                        true
                    }
                });
                effects
            }
            _ => Vec::new(),
        }
    }

    /// A request shared with the native client. Only an explicit deck
    /// answer may resolve it; unsupported prompts remain live in Codex.
    pub fn server_request(&mut self, id: &Value, method: &str, params: &Value) -> Vec<Effect> {
        let mut effects = self.server_request_inner(id, method, params);
        if effects.iter().any(|e| matches!(e, Effect::Card { .. })) {
            if let Some(thread) = thread_id(method, params) {
                if let Some(t) = self.threads.get_mut(&thread) {
                    if let Some(card) = t.async_questions.pause() {
                        effects.insert(0, Effect::Clear { thread, agent: t.agent.clone(), card });
                    }
                }
            }
        }
        effects
    }

    fn server_request_inner(&mut self, id: &Value, method: &str, params: &Value) -> Vec<Effect> {
        let thread = thread_id(method, params).unwrap_or_default();
        let Some(t) = self.threads.get_mut(&thread) else {
            return Vec::new();
        };
        self.seq += 1;
        let seq = self.seq;
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let key = format!("{seq}");
                t.pending.push(Pending {
                    id: id.clone(),
                    kind: PendingKind::Approval,
                    cards: BTreeMap::from([(key.clone(), None)]),
                });
                vec![Effect::Card {
                    thread,
                    spec: CardSpec {
                        request: key,
                        text: approval_text(method, params),
                        choices: vec![ALLOW.into(), ALLOW_SESSION.into(), DENY.into()],
                        rec: Some(0),
                        free_text: true,
                        origin: OriginKind::Permission,
                        persistent: false,
                    },
                }]
            }
            "item/permissions/requestApproval" => {
                let key = format!("{seq}");
                t.pending.push(Pending {
                    id: id.clone(),
                    kind: PendingKind::Permission,
                    cards: BTreeMap::from([(key.clone(), None)]),
                });
                vec![Effect::Card {
                    thread,
                    spec: CardSpec {
                        request: key,
                        text: permissions_text(params),
                        choices: vec![GRANT.into(), DENY.into()],
                        rec: Some(0),
                        free_text: true,
                        origin: OriginKind::Permission,
                        persistent: false,
                    },
                }]
            }
            "item/tool/requestUserInput" => {
                let questions = wire_questions(params);
                if questions.is_empty() {
                    return Vec::new();
                }
                let first = question_card(seq, 0, &questions[0]);
                t.pending.push(Pending {
                    id: id.clone(),
                    kind: PendingKind::UserInput {
                        questions,
                        answers: Vec::new(),
                    },
                    cards: BTreeMap::from([(first.request.clone(), None)]),
                });
                vec![Effect::Card {
                    thread,
                    spec: first,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// The operator answered a card of this thread.
    pub fn answered(&mut self, thread: &str, card: CardId, answer: &Answer) -> Vec<Effect> {
        if let Some(q) = self.threads.get_mut(thread).and_then(|t| t.async_questions.answered(card)) {
            return match q.reply(answer) {
                Some(text) => vec![Effect::AsyncReply { thread: thread.into(), key: q.key, text }],
                None => self.show_async_question(thread),
            };
        }
        let mut effects = self.answered_inner(thread, card, answer);
        effects.extend(self.show_async_question(thread));
        effects
    }

    fn answered_inner(&mut self, thread: &str, card: CardId, answer: &Answer) -> Vec<Effect> {
        let Some(t) = self.threads.get_mut(thread) else {
            return Vec::new();
        };
        let Some(at) = t
            .pending
            .iter()
            .position(|p| p.cards.values().any(|c| *c == Some(card)))
        else {
            return Vec::new();
        };
        let mut pending = t.pending.remove(at);
        let request = pending
            .cards
            .iter()
            .find(|(_, c)| **c == Some(card))
            .map(|(k, _)| k.clone())
            .unwrap_or_default();
        let labels = chosen_labels(&pending, &request, answer);
        match &mut pending.kind {
            PendingKind::Approval => {
                let decision = match labels.first().map(String::as_str) {
                    Some(ALLOW) => "accept",
                    Some(ALLOW_SESSION) => "acceptForSession",
                    _ => "decline",
                };
                vec![Effect::Respond {
                    id: pending.id,
                    result: json!({"decision": decision}),
                }]
            }
            PendingKind::Permission => {
                let decision = match labels.first().map(String::as_str) {
                    Some(GRANT) | Some(ALLOW) => "grant",
                    _ => "deny",
                };
                vec![Effect::Respond {
                    id: pending.id,
                    result: json!({"decision": decision}),
                }]
            }
            PendingKind::UserInput { questions, answers } => {
                answers.push(labels);
                if answers.len() < questions.len() {
                    // The next question, under a fresh key; the request
                    // stays pending.
                    self.seq += 1;
                    let next = question_card(self.seq, answers.len(), &questions[answers.len()]);
                    pending.cards.insert(next.request.clone(), None);
                    t.pending.push(pending);
                    return vec![Effect::Card {
                        thread: thread.to_string(),
                        spec: next,
                    }];
                }
                let mut by_id = serde_json::Map::new();
                for (q, a) in questions.iter().zip(answers.iter()) {
                    by_id.insert(q.id.clone(), json!({"answers": a}));
                }
                vec![Effect::Respond {
                    id: pending.id,
                    result: json!({"answers": by_id}),
                }]
            }
        }
    }

    /// A spoken or typed order for this thread: steer the running turn,
    /// else start one.
    pub fn order(&self, thread: &str, text: &str, client_message_id: &str) -> Option<Effect> {
        let t = self.threads.get(thread)?;
        let input = json!([{"type": "text", "text": text}]);
        Some(match &t.active_turn {
            Some(turn) => Effect::Request {
                method: "turn/steer",
                params: json!({
                    "threadId": thread,
                    "expectedTurnId": turn,
                    "clientUserMessageId": client_message_id,
                    "input": input,
                }),
            },
            None => Effect::Request {
                method: "turn/start",
                params: json!({"threadId": thread, "input": input}),
            },
        })
    }

    /// A `turn/steer` the daemon rejected (the turn ended under it): the
    /// text goes out as the next turn instead.
    pub fn steer_rejected(&mut self, thread: &str, text: &str) -> Option<Effect> {
        if let Some(t) = self.threads.get_mut(thread) {
            t.active_turn = None;
        }
        self.order(thread, text, "")
    }

    /// Forget every thread (the connection dropped): the agents go, and
    /// come back on reconnect.
    pub fn disconnected(&mut self) -> Vec<Effect> {
        let threads: Vec<String> = self.threads.keys().cloned().collect();
        threads.iter().flat_map(|t| self.release(t)).collect()
    }

    fn release(&mut self, thread: &str) -> Vec<Effect> {
        match self.threads.remove(thread) {
            Some(mut t) => {
                let mut effects: Vec<Effect> = t
                    .pending
                    .iter()
                    .flat_map(|p| p.cards.values().flatten())
                    .map(|card| Effect::Clear {
                        thread: thread.to_string(),
                        agent: t.agent.clone(),
                        card: *card,
                    })
                    .collect();
                if let Some(card) = t.async_questions.pause() {
                    effects.push(Effect::Clear { thread: thread.into(), agent: t.agent.clone(), card });
                }
                effects.push(Effect::Release {
                    thread: thread.to_string(),
                    agent: t.agent,
                });
                effects
            }
            None => Vec::new(),
        }
    }
}

/// Only public chat and tool items. Reasoning items are intentionally absent.
fn chat_item(item: &Value, streaming: bool) -> Option<(deck_model::MessageMeta, String)> {
    use deck_model::ChatRole;
    let id = item["id"].as_str()?.to_string();
    let images = crate::chat_images::codex(item);
    let (role, text) = match item["type"].as_str()? {
        "userMessage" | "user_message" => {
            let text = user_message_text(item).or_else(|| (!images.is_empty()).then(String::new))
                .or_else(|| crate::chat_images::has_images(&item["content"]).then(|| "[Image preview unavailable]".into()))?;
            (ChatRole::User, questions::reply_text(&text).unwrap_or(text))
        }
        "agentMessage" | "agent_message" => (ChatRole::Assistant, questions::question_text(item)
            .unwrap_or_else(|| item["text"].as_str().filter(|s| !s.is_empty()).unwrap_or("Working…").to_string())),
        "commandExecution" => {
            let command = item["command"].as_str().unwrap_or("Command");
            let output = item["aggregatedOutput"].as_str().unwrap_or_default();
            let status = item["status"].as_str().unwrap_or(if streaming { "in progress" } else { "completed" });
            (ChatRole::Tool, format!("**Command · {status}**\n\n```\n{command}\n```\n\n{}", sanitize::cut(output, 8192)))
        }
        "fileChange" => {
            let mut text = String::from("**File changes**\n");
            for change in item["changes"].as_array().into_iter().flatten() {
                if let Some(path) = change["path"].as_str() { text.push_str(&format!("\n- `{path}`")); }
                if let Some(diff) = change["diff"].as_str() { text.push_str(&format!("\n\n```diff\n{}\n```", sanitize::cut(diff, 4096))); }
            }
            (ChatRole::Tool, sanitize::cut(&text, 8192))
        }
        "mcpToolCall" | "dynamicToolCall" => {
            let name = item["tool"].as_str().or_else(|| item["name"].as_str()).unwrap_or("Tool call");
            let status = item["status"].as_str().unwrap_or(if streaming { "in progress" } else { "completed" });
            let output = item["result"]["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
            (ChatRole::Tool, format!("**{name} · {status}**\n\n{}", sanitize::cut(&output, 8192)))
        }
        "imageView" | "imageGeneration" => (ChatRole::Tool, "**Image**".into()),
        "webSearch" => (ChatRole::Tool, format!("**Web search**\n\n{}", item["query"].as_str().unwrap_or("Searching the web"))),
        _ => return None,
    };
    let activity = (role == ChatRole::Tool).then(|| deck_model::workflow::ActivityStatus::provider(item["status"].as_str(), streaming));
    Some((deck_model::MessageMeta { summary: None, images, activity, runs: None, plan: None, id, role, streaming: streaming && role != ChatRole::User }, sanitize::cut(&text, 64 * 1024)))
}

/// One row of the daemon's loaded-thread list, as [`Model::loaded`] takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedThread {
    pub id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub active: bool,
    active_turn: Option<String>,
    pub messages: Vec<(deck_model::MessageMeta, String)>,
    pub settings: Option<Value>,
    async_questions: questions::Questions,
}

impl LoadedThread {
    /// From a `thread/resume` (or `thread/read`) result's `thread` object.
    #[must_use]
    pub fn from_thread(value: &Value) -> Option<LoadedThread> {
        let id = value["id"].as_str().filter(|s| !s.is_empty())?;
        let mut async_questions = questions::Questions::default();
        for item in value["turns"].as_array().into_iter().flatten()
            .flat_map(|turn| turn["items"].as_array().into_iter().flatten()) {
            async_questions.observe(item);
        }
        Some(LoadedThread { settings: None,
            async_questions,
            id: id.to_string(),
            name: value["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    value["preview"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .filter(|s| questions::parse_reply(s).is_none())
                        .map(str::to_string)
                }),
            cwd: value["cwd"].as_str().map(str::to_string),
            active: value["status"]["type"].as_str() == Some("active"),
            active_turn: value["turns"].as_array().into_iter().flatten().rev()
                .find(|turn| turn["status"] == "inProgress")
                .and_then(|turn| turn["id"].as_str()).map(str::to_string),
            messages: value["turns"].as_array().into_iter().flatten().rev()
                .flat_map(|turn| turn["items"].as_array().into_iter().flatten().rev())
                .filter_map(|item| chat_item(item, item["status"].as_str() == Some("inProgress")))
                .take(64).collect::<Vec<_>>().into_iter().rev().collect(),
        })
    }
}

/// The thread a message is addressed to: `thread/started` carries it at
/// `params.thread.id`, everything else at `params.threadId`.
fn thread_id(method: &str, params: &Value) -> Option<String> {
    if method == "thread/started" {
        return params["thread"]["id"].as_str().map(str::to_string);
    }
    params["threadId"]
        .as_str()
        .or_else(|| params["thread_id"].as_str())
        .map(str::to_string)
}

/// The agent's display name: the thread's name, else the cwd's last
/// component, else the id's head. Never empty (the roster refuses that).
fn name_of(name: Option<&str>, cwd: Option<&str>, id: &str) -> String {
    let text = name
        .map(|n| sanitize::cut(&sanitize::collapse(n), 32))
        .filter(|n| !n.is_empty())
        .or_else(|| {
            cwd.and_then(|c| {
                std::path::Path::new(c)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
            })
            .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| format!("codex {}", &id[..id.len().min(8)]));
    text
}

/// The text of a `userMessage` item: `text`, or the text blocks of
/// `content` (Codex builds have carried both).
fn user_message_text(item: &Value) -> Option<String> {
    if let Some(text) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
        return Some(text.to_string());
    }
    match &item["content"] {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(blocks) => {
            let joined: Vec<&str> = blocks
                .iter()
                .filter(|b| matches!(b["type"].as_str(), Some("text") | Some("input_text") | None))
                .filter_map(|b| b["text"].as_str())
                .collect();
            let joined = joined.join("\n");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// The question an approval request puts on the deck (§5.6: the command
/// head, or the files).
fn approval_text(method: &str, params: &Value) -> String {
    if method.contains("commandExecution") {
        let command = match &params["command"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if command.trim().is_empty() {
            "Run a command?".to_string()
        } else {
            format!("Run: {}", sanitize::cut(&sanitize::collapse(&command), 120))
        }
    } else {
        let paths: Vec<String> = params["changes"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c["path"].as_str())
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string())
            })
            .collect();
        if paths.is_empty() {
            "Edit files?".to_string()
        } else {
            format!("Edit: {}", sanitize::cut(&paths.join(", "), 120))
        }
    }
}

fn permissions_text(params: &Value) -> String {
    let what = params["permissions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().or_else(|| p["kind"].as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| params["reason"].as_str().map(str::to_string))
        .unwrap_or_else(|| "extra permissions".to_string());
    format!("Grant: {}", sanitize::cut(&sanitize::collapse(&what), 120))
}

/// `item/tool/requestUserInput`'s questions, tolerant of field spellings.
fn wire_questions(params: &Value) -> Vec<WireQuestion> {
    params["questions"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, q)| WireQuestion {
            id: q["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("q{i}")),
            header: q["header"]
                .as_str()
                .or_else(|| q["title"].as_str())
                .unwrap_or("Question")
                .to_string(),
            question: q["question"]
                .as_str()
                .or_else(|| q["prompt"].as_str())
                .unwrap_or("")
                .to_string(),
            options: q["options"]
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|o| o.as_str().or_else(|| o["label"].as_str()))
                .map(str::to_string)
                .collect(),
            multi: q["multiSelect"]
                .as_bool()
                .or_else(|| q["multi_select"].as_bool())
                .unwrap_or(false),
        })
        .collect()
}

fn question_card(seq: u64, index: usize, q: &WireQuestion) -> CardSpec {
    let text = if q.header.is_empty() || q.header == "Question" {
        q.question.clone()
    } else {
        format!("{}: {}", q.header, q.question)
    };
    CardSpec {
        request: format!("{seq}/{index}"),
        text: sanitize::cut(&sanitize::collapse(&text), 200),
        choices: q.options.clone(),
        rec: None,
        free_text: true,
        origin: OriginKind::Question,
        persistent: false,
    }
}

/// What the operator chose, as the labels the server wants: a chosen option
/// by its label (the deck's ≤32 B label may be a cut of the server's; the
/// request's own option list is the authority), or the free text itself,
/// which on an approval card goes through §5.6's intent grammar.
fn chosen_labels(pending: &Pending, request: &str, answer: &Answer) -> Vec<String> {
    let options: Vec<String> = match &pending.kind {
        PendingKind::Approval => vec![ALLOW.into(), ALLOW_SESSION.into(), DENY.into()],
        PendingKind::Permission => vec![GRANT.into(), DENY.into()],
        PendingKind::UserInput { questions, .. } => {
            let index: usize = request
                .rsplit('/')
                .next()
                .and_then(|i| i.parse().ok())
                .unwrap_or(0);
            questions
                .get(index)
                .map(|q| q.options.clone())
                .unwrap_or_default()
        }
    };
    if let Some(choice) = answer.choice {
        if let Some(label) = options.get(usize::from(choice)) {
            return vec![label.clone()];
        }
    }
    if let Some(label) = &answer.label {
        if let Some(full) = options
            .iter()
            .find(|o| o.starts_with(label.as_str()) || *o == label)
        {
            return vec![full.clone()];
        }
    }
    let text = answer.text.as_deref().unwrap_or("").trim();
    match &pending.kind {
        PendingKind::Approval | PendingKind::Permission => match intent(text) {
            Intent::Allow => vec![options[0].clone()],
            _ => vec![DENY.into()],
        },
        PendingKind::UserInput { .. } => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// What the per-agent wait loops hand the session loop.
enum Mail {
    Answer {
        thread: String,
        card: CardId,
        answer: Answer,
    },
    Order {
        thread: String,
        text: String,
    },
}

/// What the reader task hands the session loop.
enum Inbound {
    /// A notification or a server request (replies to ours never get here).
    Message(Value),
    /// The socket ended.
    Ended(End),
}

/// The daemon's half of the socket: a writer task fed by a channel, and the
/// pending-reply table for requests of ours.
/// Requests of ours awaiting their reply, by JSON-RPC id.
type PendingReplies = std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, Value>>>>;

struct Rpc {
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<PendingReplies>,
    next: std::sync::atomic::AtomicU64,
}

impl Rpc {
    fn send_raw(&self, value: &Value) {
        let _ = self.tx.send(value.to_string());
    }

    fn notify(&self, method: &str, params: Option<Value>) {
        let mut msg = json!({"method": method});
        if let Some(params) = params {
            msg["params"] = params;
        }
        self.send_raw(&msg);
    }

    fn respond(&self, id: &Value, result: Value) {
        // A `{"error": …}` result is an error response, not a result.
        let msg = match result.get("error") {
            Some(error) if result.as_object().is_some_and(|o| o.len() == 1) => {
                json!({"id": id, "error": error})
            }
            _ => json!({"id": id, "result": result}),
        };
        self.send_raw(&msg);
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);
        self.send_raw(&json!({"id": id, "method": method, "params": params}));
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(error.to_string()),
            Ok(Err(_)) => Err("the connection closed".into()),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                Err(format!("{method} did not answer in time"))
            }
        }
    }

    /// A message from the socket that answers one of ours.
    fn resolve(&self, msg: &Value) -> bool {
        let Some(id) = msg["id"].as_u64() else {
            return false;
        };
        if msg.get("result").is_none() && msg.get("error").is_none() {
            return false;
        }
        let Some(tx) = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        else {
            return false;
        };
        let _ = tx.send(match msg.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(msg["result"].clone()),
        });
        true
    }
}

/// `clientUserMessageId` for a `turn/steer`: unique per daemon run is all
/// the server asks of it.
fn message_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "deck-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Connect to the control socket and complete the WebSocket handshake.
async fn connect(
    path: &std::path::Path,
) -> Result<tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>, String> {
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|e| format!("connect {}: {e}", path.display()))?;
    let (ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .map_err(|e| format!("websocket handshake: {e}"))?;
    Ok(ws)
}

/// The client, until `stop`. Reconnects forever; a daemon that is not
/// running costs one connect attempt every [`NOT_RUNNING_RETRY`].
pub async fn run(api: Arc<Api>, mut stop: watch::Receiver<bool>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if *stop.borrow() {
            return;
        }
        let cfg = api.config.load().codex.clone();
        if !cfg.daemon {
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(NOT_RUNNING_RETRY) => continue,
            }
        }
        let Some(path) = socket_path(cfg.daemon_socket.as_deref()) else {
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(NOT_RUNNING_RETRY) => continue,
            }
        };
        if !path.exists() {
            tracing::debug!(socket = %path.display(), "codexd: no daemon socket");
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(NOT_RUNNING_RETRY) => continue,
            }
        }
        match connect(&path).await {
            Ok(ws) => {
                tracing::info!(socket = %path.display(), "codexd: connected to the codex app-server daemon");
                let started = std::time::Instant::now();
                let outcome = session(&api, ws, &mut stop).await;
                if *stop.borrow() {
                    return;
                }
                tracing::info!(why = outcome.reason(), "codexd: connection ended");
                // A connection that lived a while resets the backoff.
                backoff = if started.elapsed() > Duration::from_secs(60) {
                    BACKOFF_MIN
                } else {
                    (backoff * 2).min(BACKOFF_MAX)
                };
            }
            Err(why) => {
                tracing::debug!(why, "codexd: connect failed");
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
    }
}

/// Why a session ended.
#[derive(Debug)]
enum End {
    Closed,
    Stopped,
    Error(String),
}

impl End {
    fn reason(&self) -> String {
        match self {
            End::Closed => "the daemon closed the connection".into(),
            End::Stopped => "deckd is stopping".into(),
            End::Error(why) => why.clone(),
        }
    }
}

/// One connection, from `initialize` to the socket closing.
async fn session(
    api: &Arc<Api>,
    ws: tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    stop: &mut watch::Receiver<bool>,
) -> End {
    let (mut sink, mut source) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            tracing::trace!(bytes = text.len(), "codexd: sending a frame");
            if let Err(e) = sink.send(WsMessage::Text(text.into())).await {
                tracing::debug!(error = %e, "codexd: the socket refused a frame");
                break;
            }
        }
        let _ = sink.close().await;
    });
    let rpc = Arc::new(Rpc {
        tx: out_tx,
        pending: Arc::default(),
        next: std::sync::atomic::AtomicU64::new(1),
    });
    let adapters = api.core.adapters();
    let Some(session_id) = adapters
        .session(
            &api.core,
            AdapterKind::Codex,
            crate::adapters::codex::CLIENT,
        )
        .await
    else {
        writer.abort();
        return End::Error("no adapter session".into());
    };
    let principal = Principal::Adapter(AdapterKind::Codex);
    let mut model = Model::default();
    let (mail_tx, mut mail_rx) = mpsc::unbounded_channel::<Mail>();
    let mut waiters: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    // Orders in flight as `turn/steer`, by thread, so a rejection can fall
    // back to `turn/start` with the same text.
    let mut steering: HashMap<String, String> = HashMap::new();
    // History must never block live questions, answers, or connection upkeep.
    // Dropping the set cancels outstanding hydration on disconnect.
    let mut histories = tokio::task::JoinSet::new();
    // A thread/started notification can register a Desktop thread before
    // polling subscribes us. A short turn may finish in that gap, so roster
    // membership alone must never suppress its first history load.
    let mut history_requested = HashSet::new();

    // The reader is its own task: a request of ours is answered on the
    // same socket, so a loop that awaited a reply while also being the
    // only thing polling the socket would wait on itself. Replies resolve
    // here; everything else (notifications, the server's requests) is
    // queued for the session loop.
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Inbound>();
    let reader = {
        let rpc = rpc.clone();
        tokio::spawn(async move {
            let why = loop {
                match source.next().await {
                    Some(Ok(WsMessage::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if rpc.resolve(&value) {
                            continue;
                        }
                        if inbound_tx.send(Inbound::Message(value)).is_err() {
                            return;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break End::Closed,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break End::Error(e.to_string()),
                }
            };
            let _ = inbound_tx.send(Inbound::Ended(why));
        })
    };

    let end = 'run: {
        // The handshake.
        let init = rpc
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "openwalkietalkie-deckd",
                        "title": "OpenWalkieTalkie deck",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {"experimentalApi": true},
                }),
            )
            .await;
        if let Err(why) = init {
            break 'run End::Error(format!("initialize: {why}"));
        }
        rpc.notify("initialized", None);
        model.catalog = read_models(&rpc).await.unwrap_or_default();

        // The account's rate limits: read once here, again on the deck's
        // ENTER, and pushed by the server after every turn.
        let mut usage_events = api.bus.subscribe();
        let mut usage_read: Option<std::time::Instant> = None;
        read_rate_limits(api, &rpc, &mut usage_read, &mut model).await;

        let mut catalog_read = std::time::Instant::now();
        let mut roster = tokio::time::interval(Duration::from_secs(ROSTER_S));
        roster.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut chat_tick = tokio::time::interval(Duration::from_millis(400));
        chat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = chat_tick.tick() => {
                    let effects = model.flush_messages();
                    apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                }
                _ = stop.changed() => break 'run End::Stopped,
                published = usage_events.recv() => match published {
                    Ok(p) if p.event.kind == deck_model::EventKind::UsageRefreshRequested => {
                        read_rate_limits(api, &rpc, &mut usage_read, &mut model).await;
                    }
                    Ok(p) if p.event.kind == deck_model::EventKind::SessionSettingsRequested
                        && p.event.principal == Principal::Device => {
                        let d = &p.event.data;
                        let change = d["agent"].as_str().zip(d["model"].as_str()).zip(d["effort"].as_str())
                            .and_then(|((agent, name), effort)| model.settings_change(agent, name, effort));
                        let effects = if let Some((thread, params)) = change {
                            change_settings(&rpc, &mut model, &thread, params).await
                        } else {
                            model.threads.iter().find(|(_, t)| t.agent.as_ref().is_some_and(|a| d["agent"] == a.0))
                                .map(|(thread, t)| {
                                    let mut controls = t.controls.clone();
                                    controls.note = "Options changed. Reopen the picker and choose again.".into();
                                    Effect::Controls { thread: thread.clone(), settled: true, controls }
                                }).into_iter().collect()
                        };
                        apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        usage_events = usage_events.resubscribe();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break 'run End::Error("bus closed".into());
                    }
                },
                _ = roster.tick() => {
                    // Credit redemption is not included in rate-limit notifications.
                    if usage_read.is_none_or(|at| at.elapsed() >= Duration::from_secs(u64::from(api.config.load().usage.poll_s.max(60)))) {
                        read_rate_limits(api, &rpc, &mut usage_read, &mut model).await;
                    }
                    if catalog_read.elapsed() >= Duration::from_secs(60) {
                        if let Ok(catalog) = read_models(&rpc).await { model.catalog = catalog; }
                        catalog_read = std::time::Instant::now();
                    }
                    match loaded_threads(&rpc).await {
                        Ok(listed) => {
                            history_requested.retain(|id| listed.threads.iter().any(|t| &t.id == id));
                            for id in &listed.resumed {
                                if history_requested.insert(id.clone()) {
                                    let rpc = rpc.clone();
                                    let id = id.clone();
                                    histories.spawn(async move {
                                        let history = history::read(&rpc, &id).await;
                                        (id, history)
                                    });
                                }
                            }
                            let effects = model.loaded(&listed.threads);
                            apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                        }
                        Err(why) => {
                            tracing::debug!(why, "codexd: thread/loaded/list failed");
                            break 'run End::Error(why);
                        }
                    }
                }
                history = histories.join_next(), if !histories.is_empty() => {
                    if let Some(Ok((thread, history))) = history {
                        let effects = model.hydrated(&thread, history);
                        apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                    }
                }
                mail = mail_rx.recv() => {
                    let Some(mail) = mail else { break 'run End::Error("mailbox closed".into()) };
                    let effects = match mail {
                        Mail::Answer { thread, card, answer } => {
                            let effects = model.answered(&thread, card, &answer);
                            tracing::debug!(card = card.0, effects = effects.len(), "codexd: answer handled");
                            effects
                        }
                        Mail::Order { thread, text } => {
                            let id = message_id();
                            let effect = model.order(&thread, &text, &id);
                            if matches!(effect, Some(Effect::Request { method: "turn/steer", .. })) {
                                steering.insert(thread.clone(), text.clone());
                            }
                            effect.into_iter().collect()
                        }
                    };
                    apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                }
                inbound = inbound_rx.recv() => {
                    let value = match inbound {
                        Some(Inbound::Message(value)) => value,
                        Some(Inbound::Ended(why)) => break 'run why,
                        None => break 'run End::Closed,
                    };
                    let Some(method) = value["method"].as_str() else { continue };
                    let params = value.get("params").cloned().unwrap_or(Value::Null);
                    let effects = if value.get("id").is_some_and(|id| !id.is_null()) {
                        model.server_request(&value["id"], method, &params)
                    } else {
                        model.notification(method, &params)
                    };
                    let effects = if !method.ends_with("/delta") && !method.ends_with("/outputDelta") {
                        let mut messages = model.flush_messages(); messages.extend(effects); messages
                    } else { effects };
                    apply(api, &rpc, &principal, session_id, &mut model, &mail_tx, &mut waiters, &mut steering, effects).await;
                }
            }
        }
    };
    reader.abort();
    // Whatever ended it, the roster must not keep agents nobody is driving.
    let effects = model.disconnected();
    apply(
        api,
        &rpc,
        &principal,
        session_id,
        &mut model,
        &mail_tx,
        &mut waiters,
        &mut steering,
        effects,
    )
    .await;
    for (_, waiter) in waiters.drain() {
        waiter.abort();
    }
    writer.abort();
    end
}

fn model_options(value: &Value) -> Vec<ModelOption> {
    value["data"].as_array().into_iter().flatten().filter(|m| m["hidden"] != true)
        .filter_map(|m| {
            let id = m["model"].as_str()?.to_string();
            if id.is_empty() || id.len() > 128 { return None; }
            let efforts: Vec<String> = m["supportedReasoningEfforts"].as_array().into_iter().flatten()
                .filter_map(|e| e["reasoningEffort"].as_str()).filter(|e| e.len() <= 16)
                .take(16).map(str::to_string).collect();
            let default_effort = m["defaultReasoningEffort"].as_str().filter(|e| efforts.iter().any(|v| v == e))
                .map(str::to_string).or_else(|| efforts.first().cloned()).unwrap_or_default();
            Some(ModelOption { label: m["displayName"].as_str().filter(|s| !s.is_empty()).unwrap_or(&id).into(),
                id, efforts, default_effort })
        }).take(64).collect()
}

async fn read_models(rpc: &Rpc) -> Result<Vec<ModelOption>, String> {
    let mut models = Vec::new();
    let mut cursor = Value::Null;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let result = rpc.request("model/list", json!({"limit": 64, "includeHidden": false, "cursor": cursor})).await?;
        for option in model_options(&result) {
            if !models.iter().any(|m: &ModelOption| m.id == option.id) { models.push(option); }
        }
        if models.len() >= 64 { models.truncate(64); break; }
        let Some(next) = result["nextCursor"].as_str() else { break; };
        if !seen.insert(next.to_owned()) { break; }
        cursor = json!(next);
    }
    Ok(models)
}

async fn change_settings(rpc: &Rpc, model: &mut Model, thread: &str, params: Value) -> Vec<Effect> {
    let expected_model = params["model"].as_str().unwrap_or_default().to_string();
    let expected_effort = params["effort"].as_str().map(str::to_string);
    let result = rpc.request("thread/settings/update", params).await;
    let mut note = None;
    match result {
        Ok(_) => {
            // Read back actual session defaults, without creating a turn or
            // changing any permission, collaboration or terminal settings.
            match rpc.request("thread/resume", json!({"threadId": thread, "excludeTurns": true})).await {
                Ok(value) => {
                    let verified = value["model"].as_str() == Some(expected_model.as_str())
                        && expected_effort.as_deref().is_none_or(|e| value["reasoningEffort"].as_str() == Some(e));
                    let _ = model.settings(thread, &value);
                    if !verified { note = Some("Change not confirmed. Refresh and try again."); }
                }
                Err(_) => note = Some("Could not confirm the change. Check the session."),
            }
        }
        Err(why) => {
            tracing::debug!(why, "codexd: settings update refused");
            if why.contains("-32601") {
                model.settings_unavailable = true;
                note = Some("Update Codex to enable model and effort selection.");
            } else { note = Some("Codex could not apply this choice. Try another option."); }
        }
    }
    let Some(t) = model.threads.get_mut(thread) else { return Vec::new(); };
    if let Some(note) = note { t.controls.note = note.into(); }
    if model.settings_unavailable { t.controls.editable = false; }
    vec![Effect::Controls { thread: thread.into(), settled: true, controls: t.controls.clone() }]
}

/// `thread/loaded/list`, then `thread/resume` on each id for its name and
/// status — resume is also what subscribes this connection to the thread.
struct LoadedRoster {
    threads: Vec<LoadedThread>,
    resumed: Vec<String>,
}

async fn loaded_threads(rpc: &Rpc) -> Result<LoadedRoster, String> {
    let listed = rpc.request("thread/loaded/list", json!({})).await?;
    let ids: Vec<String> = listed["data"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().or_else(|| v["id"].as_str()))
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    let mut resumed = Vec::new();
    for id in ids {
        match rpc
            .request(
                "thread/resume",
                json!({"threadId": id, "excludeTurns": true,
            "initialTurnsPage": {"limit": 1, "sortDirection": "desc", "itemsView": "notLoaded"}}),
            )
            .await
        {
            Ok(result) => {
                resumed.push(id.clone());
                let thread = result.get("thread").unwrap_or(&result);
                let mut loaded = LoadedThread::from_thread(thread).unwrap_or(LoadedThread {
                    settings: None,
                    async_questions: questions::Questions::default(),
                    active_turn: None,
                    id: id.clone(),
                    name: None,
                    cwd: None,
                    active: false,
                    messages: Vec::new(),
                });
                loaded.active_turn = result["initialTurnsPage"]["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|turn| turn["status"] == "inProgress")
                    .and_then(|turn| turn["id"].as_str())
                    .map(str::to_string)
                    .or(loaded.active_turn);
                loaded.settings = Some(
                    json!({"model":result["model"],"reasoningEffort":result["reasoningEffort"],"effort":result["effort"]}),
                );
                out.push(loaded);
            }
            Err(why) => {
                tracing::debug!(why, "codexd: thread/resume failed");
                out.push(LoadedThread { settings: None,
                    async_questions: questions::Questions::default(), active_turn: None,
                    id,
                    name: None,
                    cwd: None,
                    active: false,
                    messages: Vec::new(),
                });
            }
        }
    }
    Ok(LoadedRoster {
        threads: out,
        resumed,
    })
}

/// ENTER on the deck reads again only when the last read is older than
/// this: the server pushes `account/rateLimits/updated` on its own.
const USAGE_READ_GAP: Duration = Duration::from_secs(10);

/// `account/rateLimits/read` → the core's Usage face, when `[usage]
/// providers` names Codex. A daemon signed in with an API key has no
/// windows to report, and that is a debug line.
async fn read_rate_limits(api: &Arc<Api>, rpc: &Arc<Rpc>, last: &mut Option<std::time::Instant>, model: &mut Model) {
    if !crate::usage::Provider::Codex.enabled(&api.config.load().usage) {
        return;
    }
    if last.is_some_and(|at| at.elapsed() < USAGE_READ_GAP) {
        return;
    }
    *last = Some(std::time::Instant::now());
    let account = match rpc.request("account/read", json!({"refreshToken": false})).await {
        Ok(result) => crate::usage::codex_account(&result),
        Err(_) => None,
    };
    if model.usage_account != account {
        feed_usage(api, Vec::new(), None, Some(Vec::new())).await;
    }
    model.usage_account = account;
    match rpc.request("account/rateLimits/read", json!({})).await {
        Ok(result) => {
            let windows = crate::usage::codex_windows(&result);
            let resets = crate::usage::codex_reset_grants(&result);
            tracing::debug!(windows = windows.len(), resets = resets.len(), "codexd: account/rateLimits/read");
            feed_usage(api, windows, model.usage_account.clone(), Some(resets)).await;
        }
        Err(why) => {
            tracing::debug!(why, "codexd: account/rateLimits/read refused");
            feed_usage(api, Vec::new(), None, Some(Vec::new())).await;
        }
    }
}

async fn feed_usage(api: &Arc<Api>, windows: Vec<crate::usage::Window>, account: Option<String>, resets: Option<Vec<crate::usage::ResetGrant>>) {
    if let Err(e) = api
        .core
        .send(crate::core::handle::CoreMsg::Usage {
            partial: false,
            resets,
            provider: crate::usage::Provider::Codex,
            windows,
            account,
        })
        .await
    {
        tracing::debug!(code = e.code, "codexd: the core did not take the usage windows");
    }
}

/// Carry the model's effects to the core and the socket.
#[allow(clippy::too_many_arguments)]
async fn apply(
    api: &Arc<Api>,
    rpc: &Arc<Rpc>,
    principal: &Principal,
    session: SessionId,
    model: &mut Model,
    mail: &mpsc::UnboundedSender<Mail>,
    waiters: &mut HashMap<String, tokio::task::JoinHandle<()>>,
    steering: &mut HashMap<String, String>,
    effects: Vec<Effect>,
) {
    let adapters = api.core.adapters();
    let mut effects = std::collections::VecDeque::from(effects);
    while let Some(effect) = effects.pop_front() {
        match effect {
            Effect::Controls { thread, settled, controls } => {
                if let Some(agent) = model.agent(&thread).cloned() {
                    let _ = api.core.send(crate::core::CoreMsg::SessionControls { agent, session, settled, controls }).await;
                }
            }
            Effect::Register { thread, name, cwd } => {
                let register = AgentRegister {
                    kind: Kind::Codex,
                    name,
                    key: Some(format!("codex:{thread}")),
                    cwd,
                    pid: None,
                    pid_kind: None,
                    terminal: None,
                    lease_ttl_s: 0,
                    caps: Caps {
                        answer: AnswerCap::Poll,
                        order: OrderCap::Poll,
                        focus: false,
                        transcribe: Default::default(),
                    },
                    slot_hint: None,
                    voice: None,
                    meta: Some(json!({"via": "app-server"})),
                };
                match api
                    .core
                    .api(
                        principal.clone(),
                        session,
                        false,
                        Request::AgentRegister(register),
                    )
                    .await
                {
                    Ok(result) => {
                        if let Some(id) = result["agent"].as_str() {
                            let agent = AgentId(id.to_string());
                            model.registered(&thread, agent.clone());
                            adapters.claim_thread(&format!("codex:{thread}"));
                            // One wait loop per agent: answers and orders
                            // arrive through it (§4.15).
                            let core = api.core.clone();
                            let mail = mail.clone();
                            let thread_id = thread.clone();
                            let waiter = tokio::spawn(async move {
                                loop {
                                    match core
                                        .wait(session, agent.clone(), Duration::from_secs(WAIT_S))
                                        .await
                                    {
                                        crate::core::handle::WaitResult::Answer {
                                            card,
                                            answer,
                                            ..
                                        } => {
                                            tracing::debug!(
                                                card = card.0,
                                                "codexd: an answer from the wait"
                                            );
                                            let _ = mail.send(Mail::Answer {
                                                thread: thread_id.clone(),
                                                card,
                                                answer,
                                            });
                                            // §4.9: a polled answer is delivered once the
                                            // waiter has it — the door reports the same
                                            // after writing its response.
                                            core.delivered(agent.clone(), card, true).await;
                                        }
                                        crate::core::handle::WaitResult::Order {
                                            text,
                                            card,
                                            delivery,
                                            ..
                                        } => {
                                            let _ = mail.send(Mail::Order {
                                                thread: thread_id.clone(),
                                                text,
                                            });
                                            if let (Some(card), Some(_)) = (card, delivery) {
                                                core.delivered(agent.clone(), card, true).await;
                                            }
                                        }
                                        crate::core::handle::WaitResult::Snooze { .. }
                                        | crate::core::handle::WaitResult::Timeout => {}
                                        other => {
                                            // Refused: the agent is gone or not ours.
                                            tracing::debug!(?other, "codexd: the wait was refused");
                                            tokio::time::sleep(Duration::from_secs(5)).await;
                                        }
                                    }
                                }
                            });
                            if let Some(old) = waiters.insert(thread, waiter) {
                                old.abort();
                            }
                        }
                    }
                    Err(e) => tracing::debug!(code = e.code, "codexd: agent.register refused"),
                }
            }
            Effect::Update {
                thread,
                state,
                title,
            } => {
                let Some(agent) = model.agent(&thread).cloned() else {
                    continue;
                };
                let (title, title_source) = match title {
                    Some(t) => (Some(t), Some(TitleSource::Prompt)),
                    None => (None, None),
                };
                let _ = api
                    .core
                    .api(
                        principal.clone(),
                        session,
                        false,
                        Request::AgentUpdate(AgentUpdate {
                            agent,
                            state,
                            title,
                            title_source,
                            since: None,
                            name: None,
                            meta: None, task: None, subagents: None,
                        }),
                    )
                    .await;
            }
            Effect::Message { thread, message, text } => {
                let Some(agent) = model.agent(&thread).cloned() else { continue; };
                let _ = api.core.api(principal.clone(), session, false, Request::Reply(ReplySet {
                    agent, text, message: Some(message), speak: false, announce: None, lane: None,
                })).await;
            }
            Effect::Reply { thread, text } => {
                let Some(agent) = model.agent(&thread).cloned() else {
                    continue;
                };
                let _ = api
                    .core
                    .api(
                        principal.clone(),
                        session,
                        false,
                        Request::Reply(ReplySet {
                            message: None,
                            agent,
                            text,
                            speak: false,
                            announce: None,
                            lane: None,
                        }),
                    )
                    .await;
            }
            Effect::Card { thread, spec } => {
                let Some(agent) = model.agent(&thread).cloned() else {
                    continue;
                };
                let hold_s = adapters.hold_s(AdapterKind::Codex);
                let cmd = CardSet {
                    agent,
                    id: None,
                    kind: CardKind::Q,
                    text: sanitize::text(&spec.text),
                    choices: sanitize::labels(&spec.choices)
                        .into_iter()
                        .map(|label| Choice {
                            label,
                            detail: None,
                        })
                        .collect(),
                    rec: spec.rec,
                    free_text: spec.free_text,
                    deliver: Deliver::Poll,
                    timeout_s: (!spec.persistent).then_some(hold_s),
                    origin: Origin {
                        kind: spec.origin,
                        tool: None,
                        request_id: Some(spec.request.clone()),
                        group: None,
                    },
                };
                match api
                    .core
                    .api(principal.clone(), session, false, Request::CardSet(cmd))
                    .await
                {
                    Ok(result) => {
                        // `{card:{id}}` (§4.6); tolerate a bare number too.
                        let id = result["card"]["id"]
                            .as_u64()
                            .or_else(|| result["card"].as_u64())
                            .or_else(|| result["id"].as_u64());
                        match id {
                            Some(id) => model.carded(&thread, &spec.request, CardId(id as u16)),
                            None => {
                                tracing::debug!("codexd: agent.card.set answered without a card id")
                            }
                        }
                    }
                    Err(e) => tracing::debug!(code = e.code, "codexd: agent.card.set refused"),
                }
            }
            Effect::Clear {
                thread,
                agent,
                card,
            } => {
                let Some(agent) = agent.or_else(|| model.agent(&thread).cloned()) else {
                    continue;
                };
                let _ = api
                    .core
                    .api(
                        principal.clone(),
                        session,
                        false,
                        Request::CardClear(CardClear {
                            agent,
                            card,
                            delivered: false,
                            reason: Some("resolved".into()),
                        }),
                    )
                    .await;
            }
            Effect::Release { thread, agent } => {
                adapters.release_thread(&format!("codex:{thread}"));
                if let Some(waiter) = waiters.remove(&thread) {
                    waiter.abort();
                }
                let Some(agent) = agent else { continue };
                let _ = api
                    .core
                    .api(
                        principal.clone(),
                        session,
                        false,
                        Request::AgentRelease {
                            agent,
                            reason: Some("thread unloaded".into()),
                        },
                    )
                    .await;
            }
            Effect::Respond { id, result } => rpc.respond(&id, result),
            Effect::AsyncReply { thread, key, text } => {
                let mut result = Err("thread unavailable".to_string());
                if let Some(Effect::Request { method, params }) = model.order(&thread, &text, &key) {
                    result = rpc.request(method, params).await;
                    // As with a spoken order, the active turn may have
                    // completed between tapping the choice and delivery.
                    if result.is_err() && method == "turn/steer" {
                        if let Some(Effect::Request { method, params }) = model.steer_rejected(&thread, &text) {
                            result = rpc.request(method, params).await;
                        }
                    }
                }
                if let Err(why) = &result { tracing::debug!(why, "codexd: async question reply refused; keeping card"); }
                effects.extend(model.async_reply_finished(&thread, &key, result.is_ok()));
            }
            Effect::Request { method, params } => {
                let thread = params["threadId"].as_str().unwrap_or_default().to_string();
                if let Err(why) = rpc.request(method, params).await {
                    tracing::debug!(method, why, "codexd: request refused");
                    if method == "turn/steer" {
                        // The turn ended under the steer: the same text
                        // opens the next turn instead (zeron's rule).
                        if let Some(text) = steering.remove(&thread) {
                            if let Some(Effect::Request { method, params }) =
                                model.steer_rejected(&thread, &text)
                            {
                                if let Err(why) = rpc.request(method, params).await {
                                    tracing::debug!(
                                        why,
                                        "codexd: turn/start after a rejected steer failed"
                                    );
                                }
                            }
                        }
                    }
                } else if method == "turn/steer" {
                    steering.remove(&thread);
                }
            }
            Effect::Usage(windows) => {
                if crate::usage::Provider::Codex.enabled(&api.config.load().usage) {
                    feed_usage(api, windows, model.usage_account.clone(), None).await;
                }
            }
            Effect::RefreshUsage => {
                model.catalog = read_models(rpc).await.unwrap_or_default();
                if crate::usage::Provider::Codex.enabled(&api.config.load().usage) {
                    feed_usage(api, Vec::new(), None, Some(Vec::new())).await;
                    read_rate_limits(api, rpc, &mut None, model).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{AnswerSource, Principal};

    #[test]
    fn codex_image_only_input_tool_results_and_generations_keep_previews() {
        let data = crate::chat_images::fixture();
        for item in [
            json!({"id":"u","type":"userMessage","content":[{"type":"image","url":format!("data:image/png;base64,{data}")}]}),
            json!({"id":"m","type":"mcpToolCall","result":{"content":[{"type":"image","data":data}]}}),
            json!({"id":"g","type":"imageGeneration","result":data}),
            json!({"id":"d","type":"dynamicToolCall","contentItems":[{"type":"inputImage","imageUrl":format!("data:image/png;base64,{data}")}]}),
        ] {
            let (meta,text) = chat_item(&item,false).unwrap();
            assert_eq!(meta.images.len(),1);
            assert!(!text.contains(&data));
        }
    }

    #[test]
    fn workflow_plans_coalesce_and_route_by_turn_without_accepting_a_stale_turn() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        model.registered("t1", AgentId("ag_1".into()));
        model.notification("turn/started", &json!({"threadId":"t1", "turn":{"id":"turn-1"}}));
        for status in ["pending", "inProgress", "completed"] {
            model.notification("turn/plan/updated", &json!({"turnId":"turn-1", "plan":[{"step":"Verify", "status":status}]}));
        }
        let messages = model.flush_messages();
        assert_eq!(messages.len(), 1);
        let Effect::Message { thread, message, .. } = &messages[0] else { panic!("plan message"); };
        assert_eq!(thread, "t1");
        assert_eq!(message.id, "plan:turn-1");
        assert_eq!(message.plan.as_ref().unwrap().completed(), 1);
        model.notification("turn/started", &json!({"threadId":"t1", "turn":{"id":"turn-2"}}));
        model.notification("turn/plan/updated", &json!({"threadId":"t1", "turnId":"turn-1", "plan":[{"step":"Old plan", "status":"pending"}]}));
        assert!(model.flush_messages().is_empty());
        model.notification("turn/plan/updated", &json!({"threadId":"t1", "turnId":"turn-2", "plan":[]}));
        let messages = model.flush_messages();
        let Effect::Message { message, .. } = &messages[0] else { panic!("clear plan"); };
        assert!(message.plan.as_ref().unwrap().steps.is_empty());
    }

    #[test]
    fn an_account_change_discards_the_old_identity_and_requests_fresh_usage() {
        let mut model = Model { usage_account: Some("old@example.com".into()), ..Model::default() };
        assert_eq!(model.notification("account/updated", &json!({"authMode": "chatgpt"})), vec![Effect::RefreshUsage]);
        assert_eq!(model.usage_account, None);
        assert_eq!(model.notification("account/updated", &json!({"authMode": null})), vec![Effect::RefreshUsage]);
    }

    fn answer(choice: Option<u8>, label: Option<&str>, text: Option<&str>) -> Answer {
        Answer {
            choice,
            label: label.map(str::to_string),
            text: text.map(str::to_string),
            source: AnswerSource::Deck,
            principal: Principal::Uid(501),
            session: None,
            utterance: None,
        }
    }

    fn loaded(id: &str, name: Option<&str>, active: bool) -> LoadedThread {
        LoadedThread { settings: None,
            async_questions: questions::Questions::default(), active_turn: None,
            id: id.into(),
            name: name.map(str::to_string),
            cwd: Some("/Users/t/code/api".into()),
            active,
            messages: Vec::new(),
        }
    }

    fn async_item() -> Value {
        json!({"id":"call_async","type":"agentMessage","delivery":"async","text":"Please choose",
            "questions":[{"title":"Which database?","options":["Postgres (Recommended)","SQLite"]},
                         {"title":"Project name?","options":null}]})
    }

    #[test]
    fn structured_answers_have_the_same_readable_view_live_and_after_reconnect() {
        let raw = format!("<send_user_message_question_reply>{}</send_user_message_question_reply>",
            json!([{"question":"Which database?", "answer":"SQLite", "questionItemId":"private-id"}]));
        let item = json!({"id":"answer","type":"userMessage","content":[{"type":"text","text":raw}]});
        let (meta, shown) = chat_item(&item, false).unwrap();
        assert_eq!(meta.role, deck_model::ChatRole::User);
        assert!(shown.starts_with("### Your answer"));
        assert!(!shown.contains("send_user_message") && !shown.contains("private-id"));
        // The underlying text still carries the exact protocol needed by Codex.
        assert_eq!(user_message_text(&item).unwrap(), raw);

        let history = LoadedThread::from_thread(&json!({"id":"t1", "cwd":"/projects/demo", "preview":raw,
            "turns":[{"items":[item]}]})).unwrap();
        assert_eq!(history.name, None);
        assert_eq!(history.messages[0].1, shown);

        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        let params = json!({"threadId":"t1","item":item});
        let effects = model.notification("item/started", &params);
        assert!(effects.iter().any(|e| matches!(e, Effect::Update { title: None, .. })));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Update { title: Some(_), .. })));
        let messages = model.flush_messages();
        assert!(messages.iter().any(|e| matches!(e, Effect::Message { text, .. } if text == &shown)));
        model.notification("item/completed", &params);
        assert!(model.flush_messages().is_empty());
    }

    fn card_spec(effects: &[Effect]) -> CardSpec {
        effects.iter().find_map(|e| match e { Effect::Card { spec, .. } => Some(spec.clone()), _ => None }).expect("question card")
    }

    #[test]
    fn async_questions_survive_turns_and_answer_in_order_with_exact_identity() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        let params = json!({"threadId":"t1","item":async_item()});
        let first = card_spec(&model.notification("item/started", &params));
        assert_eq!(first.origin, OriginKind::Question);
        assert!(first.persistent && first.free_text);
        assert_eq!(first.text, "Which database?");
        assert_eq!(first.rec, None); // A suggested option is not an answer.
        model.carded("t1", &first.request, CardId(1));
        assert!(model.notification("item/completed", &params).is_empty());
        let done = model.notification("turn/completed", &json!({"threadId":"t1","turn":{"status":"completed"}}));
        assert!(!done.iter().any(|e| matches!(e, Effect::Clear { .. } | Effect::Card { .. })));
        let effects = model.answered("t1", CardId(1), &answer(Some(0), Some("Postgres"), None));
        let [Effect::AsyncReply { key, text, .. }] = effects.as_slice() else { panic!("{effects:?}"); };
        assert_eq!(key, "[\"request_user_input_async\",\"call_async\",0]");
        let body: Value = serde_json::from_str(text.trim().strip_prefix("<send_user_message_question_reply>").unwrap()
            .strip_suffix("</send_user_message_question_reply>").unwrap()).unwrap();
        assert_eq!(body, json!([{"answer":"Postgres (Recommended)","question":"Which database?","questionItemId":key}]));
        // A failed delivery restores the same question, with a fresh card.
        let retry = card_spec(&model.async_reply_finished("t1", key, false));
        assert_eq!(retry, first);
        model.carded("t1", &retry.request, CardId(2));
        model.answered("t1", CardId(2), &answer(Some(0), None, None));
        let second = card_spec(&model.async_reply_finished("t1", key, true));
        assert_eq!(second.text, "Project name?");
        assert!(second.choices.is_empty() && second.free_text);
        model.carded("t1", &second.request, CardId(3));
        let effects = model.answered("t1", CardId(3), &answer(None, None, Some("My \"project\"\n第二行")));
        let [Effect::AsyncReply { key, text, .. }] = effects.as_slice() else { panic!("{effects:?}"); };
        assert!(text.contains("My \\\"project\\\"\\n第二行"));
        assert!(!model.async_reply_finished("t1", key, true).iter().any(|e| matches!(e, Effect::Card { .. })));
        assert!(model.notification("item/completed", &params).is_empty());
        assert!(model.answered("t1", CardId(1), &answer(Some(1), None, None)).is_empty());
    }

    #[test]
    fn permissions_take_priority_then_return_to_unanswered_async_questions() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        let first = card_spec(&model.notification("item/completed", &json!({"threadId":"t1","item":async_item()})));
        model.carded("t1", &first.request, CardId(1));
        let effects = model.server_request(&json!(42), "item/commandExecution/requestApproval", &json!({"threadId":"t1","command":"ls"}));
        assert!(matches!(effects.first(), Some(Effect::Clear { card: CardId(1), .. })));
        let approval = card_spec(&effects);
        assert_eq!(approval.origin, OriginKind::Permission);
        model.carded("t1", &approval.request, CardId(2));
        let effects = model.answered("t1", CardId(2), &answer(Some(2), None, None));
        assert!(matches!(&effects[0], Effect::Respond { result, .. } if result["decision"] == "decline"));
        assert_eq!(card_spec(&effects), first);
    }

    #[test]
    fn history_restores_only_pending_questions_and_external_answers_clear_them() {
        let key = json!(["request_user_input_async","call_async",0]).to_string();
        let reply = format!("<send_user_message_question_reply>\n{}\n</send_user_message_question_reply>",
            json!([{"answer":"SQLite","question":"Which database?","questionItemId":key}]));
        let thread = LoadedThread::from_thread(&json!({"id":"t1","status":{"type":"active"},"turns":[
            {"id":"old","status":"completed","items":[async_item(),{"id":"reply","type":"userMessage","content":[{"type":"text","text":reply}]}]},
            {"id":"running","status":"inProgress","items":[]}
        ]})).unwrap();
        let mut model = Model::default();
        let effects = model.loaded(std::slice::from_ref(&thread));
        let spec = card_spec(&effects);
        assert_eq!(spec.text, "Project name?");
        assert_eq!(model.active_turn("t1"), Some("running"));
        assert!(!model.loaded(&[thread]).iter().any(|e| matches!(e, Effect::Card { .. })));
        model.carded("t1", &spec.request, CardId(5));
        let reply = format!("<send_user_message_question_reply>{}</send_user_message_question_reply>",
            json!([{"answer":"Walkie","question":"Project name?","questionItemId":spec.request}]));
        let params = json!({"threadId":"t1","item":{"id":"elsewhere","type":"userMessage","content":[{"type":"text","text":reply}]}});
        let effects = model.notification("item/started", &params);
        assert!(effects.iter().any(|e| matches!(e, Effect::Clear { card: CardId(5), .. })));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Card { .. })));
        assert!(model.notification("item/completed", &params).is_empty());
        assert!(model.answered("t1", CardId(5), &answer(None, None, Some("stale"))).is_empty());
    }

    #[test]
    fn session_settings_use_the_live_catalog_and_notifications_without_changing_turn_state() {
        let catalog = model_options(&json!({"data": [
            {"model": "hidden", "hidden": true},
            {"model": "model-a", "displayName": "Model A", "defaultReasoningEffort": "high",
                "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}]},
            {"model": "model-b", "displayName": "Model B", "supportedReasoningEfforts": []}
        ]}));
        assert_eq!(catalog.len(), 2);
        let mut model = Model { catalog, ..Default::default() };
        model.loaded(&[loaded("t1", Some("Session"), true)]);
        model.registered("t1", AgentId("ag_1".into()));
        let before = model.threads["t1"].active_turn.clone();
        let effects = model.notification("thread/settings/updated", &json!({
            "threadId": "t1", "threadSettings": {"model": "model-a", "effort": "high"}
        }));
        assert!(matches!(effects.as_slice(), [Effect::Controls { settled: false, controls, .. }]
            if controls.model == "model-a" && controls.effort == "high" && controls.editable));
        assert_eq!(model.threads["t1"].active_turn, before);
        assert_eq!(model.settings_change("ag_1", "model-a", "low").unwrap().1,
            json!({"threadId": "t1", "model": "model-a", "effort": "low"}));
        assert_eq!(model.settings_change("ag_1", "model-b", "").unwrap().1,
            json!({"threadId": "t1", "model": "model-b"}));
        assert!(model.settings_change("another-agent", "model-a", "low").is_none());
        assert!(model.settings_change("ag_1", "hidden", "low").is_none());
        assert!(model.settings_change("ag_1", "model-b", "high").is_none());
        model.catalog.clear();
        assert!(model.settings_change("ag_1", "model-a", "low").is_none());
    }

    #[test]
    fn loaded_threads_register_once_and_unloaded_ones_are_released() {
        let mut model = Model::default();
        let effects = model.loaded(&[loaded("t1", Some("Refactor UI"), true)]);
        assert_eq!(
            effects,
            vec![
                Effect::Register {
                    thread: "t1".into(),
                    name: "Refactor UI".into(),
                    cwd: Some("/Users/t/code/api".into()),
                },
                Effect::Update {
                    thread: "t1".into(),
                    state: Some(AgentState::Working),
                    title: None,
                },
            ]
        );
        model.registered("t1", AgentId("ag_1".into()));
        // Listed again: nothing new.
        assert!(model
            .loaded(&[loaded("t1", Some("Refactor UI"), true)])
            .is_empty());
        // Gone from the list: released, and the hooks path gets it back.
        let effects = model.loaded(&[]);
        assert_eq!(
            effects,
            vec![Effect::Release {
                thread: "t1".into(),
                agent: Some(AgentId("ag_1".into())),
            }]
        );
        assert!(model.threads().is_empty());
    }

    #[test]
    fn the_name_prefers_the_thread_name_then_the_cwd_then_the_id() {
        assert_eq!(
            name_of(Some("Fix the build"), Some("/w/api"), "01a1"),
            "Fix the build"
        );
        assert_eq!(name_of(None, Some("/w/api"), "01a1"), "api");
        assert_eq!(name_of(Some("  "), None, "01a11f33-ba6b"), "codex 01a11f33");
    }

    #[test]
    fn a_turn_is_working_then_ready_with_the_last_agent_message_as_the_reply() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        model.registered("t1", AgentId("ag_1".into()));
        let started = model.notification(
            "turn/started",
            &json!({"threadId": "t1", "turn": {"id": "turn-1"}}),
        );
        assert_eq!(
            started,
            vec![Effect::Update {
                thread: "t1".into(),
                state: Some(AgentState::Working),
                title: None,
            }]
        );
        assert_eq!(model.active_turn("t1"), Some("turn-1"));
        let titled = model.notification(
            "item/started",
            &json!({"threadId": "t1", "item": {"id": "u1", "type": "userMessage", "content": [{"type": "text", "text": "please   refactor the\nlogin form"}]}}),
        );
        assert_eq!(
            titled,
            vec![Effect::Update {
                thread: "t1".into(),
                state: Some(AgentState::Working),
                title: Some("please refactor the login form".into()),
            }]
        );
        // The completed echo of the same item does not re-title.
        assert!(model
            .notification(
                "item/completed",
                &json!({"threadId": "t1", "item": {"id": "u1", "type": "userMessage", "content": [{"type": "text", "text": "please refactor the login form"}]}}),
            )
            .is_empty());
        assert!(model
            .notification(
                "item/completed",
                &json!({"threadId": "t1", "item": {"id": "a1", "type": "agentMessage", "text": "Done: the form uses the shared input."}}),
            )
            .is_empty());
        let done = model.notification(
            "turn/completed",
            &json!({"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}),
        );
        assert_eq!(
            done,
            vec![
                Effect::Reply {
                    thread: "t1".into(),
                    text: "Done: the form uses the shared input.".into(),
                },
                Effect::Update {
                    thread: "t1".into(),
                    state: Some(AgentState::Ready),
                    title: None,
                },
            ]
        );
        assert_eq!(model.active_turn("t1"), None);
        let failed = model.notification(
            "turn/completed",
            &json!({"threadId": "t1", "turn": {"id": "turn-2", "status": "failed", "error": {"message": "boom"}}}),
        );
        assert_eq!(
            failed,
            vec![Effect::Update {
                thread: "t1".into(),
                state: Some(AgentState::Error),
                title: None,
            }]
        );
    }

    #[test]
    fn an_approval_request_is_a_card_and_the_answer_is_the_servers_decision() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, true)]);
        model.registered("t1", AgentId("ag_1".into()));
        let effects = model.server_request(
            &json!(7),
            "item/commandExecution/requestApproval",
            &json!({"threadId": "t1", "turnId": "turn-1", "itemId": "c1", "command": ["psql", "-f", "migrate.sql"], "cwd": "/w"}),
        );
        let Some(Effect::Card { thread, spec }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        assert_eq!(thread, "t1");
        assert_eq!(spec.text, "Run: psql -f migrate.sql");
        assert_eq!(spec.choices, vec![ALLOW, ALLOW_SESSION, DENY]);
        assert_eq!(spec.origin, OriginKind::Permission);
        model.carded("t1", &spec.request, CardId(3));
        // Chosen by index.
        let out = model.answered("t1", CardId(3), &answer(Some(1), Some(ALLOW_SESSION), None));
        assert_eq!(
            out,
            vec![Effect::Respond {
                id: json!(7),
                result: json!({"decision": "acceptForSession"}),
            }]
        );
        // Answered once: a second answer finds nothing.
        assert!(model
            .answered("t1", CardId(3), &answer(Some(0), None, None))
            .is_empty());

        // Free text goes through the intent grammar; nonsense is a decline.
        let effects = model.server_request(
            &json!(8),
            "item/fileChange/requestApproval",
            &json!({"threadId": "t1", "changes": [{"path": "/w/src/main.rs"}, {"path": "/w/README.md"}]}),
        );
        let Some(Effect::Card { spec, .. }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        assert_eq!(spec.text, "Edit: main.rs, README.md");
        model.carded("t1", &spec.request, CardId(4));
        let out = model.answered("t1", CardId(4), &answer(None, None, Some("go ahead")));
        assert_eq!(
            out[0],
            Effect::Respond {
                id: json!(8),
                result: json!({"decision": "accept"})
            }
        );
        let effects = model.server_request(
            &json!(9),
            "item/fileChange/requestApproval",
            &json!({"threadId": "t1", "changes": []}),
        );
        let Some(Effect::Card { spec, .. }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        model.carded("t1", &spec.request, CardId(5));
        let out = model.answered(
            "t1",
            CardId(5),
            &answer(None, None, Some("what time is it")),
        );
        assert_eq!(
            out[0],
            Effect::Respond {
                id: json!(9),
                result: json!({"decision": "decline"})
            }
        );
    }

    #[test]
    fn a_resolved_or_finished_request_clears_its_card_and_is_never_answered() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, true)]);
        model.registered("t1", AgentId("ag_1".into()));
        let effects = model.server_request(
            &json!("req-1"),
            "item/commandExecution/requestApproval",
            &json!({"threadId": "t1", "command": "ls"}),
        );
        let Some(Effect::Card { spec, .. }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        model.carded("t1", &spec.request, CardId(3));
        let cleared = model.notification(
            "serverRequest/resolved",
            &json!({"threadId": "t1", "requestId": "req-1"}),
        );
        assert_eq!(
            cleared,
            vec![Effect::Clear {
                thread: "t1".into(),
                agent: Some(AgentId("ag_1".into())),
                card: CardId(3),
            }]
        );
        assert!(model
            .answered("t1", CardId(3), &answer(Some(0), None, None))
            .is_empty());
        // And a turn ending clears whatever it was still asking.
        let effects = model.server_request(
            &json!(2),
            "item/commandExecution/requestApproval",
            &json!({"threadId": "t1", "command": "ls"}),
        );
        let Some(Effect::Card { spec, .. }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        model.carded("t1", &spec.request, CardId(4));
        let done = model.notification(
            "turn/completed",
            &json!({"threadId": "t1", "turn": {"id": "turn-1", "status": "interrupted"}}),
        );
        assert_eq!(
            done[0],
            Effect::Clear {
                thread: "t1".into(),
                agent: Some(AgentId("ag_1".into())),
                card: CardId(4),
            }
        );
        let _ = spec;
    }

    #[test]
    fn request_user_input_asks_one_question_at_a_time_and_answers_by_wire_id() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, true)]);
        model.registered("t1", AgentId("ag_1".into()));
        let effects = model.server_request(
            &json!(11),
            "item/tool/requestUserInput",
            &json!({"threadId": "t1", "questions": [
                {"id": "db", "header": "Database", "question": "Which database?", "options": [{"label": "Postgres"}, {"label": "SQLite"}]},
                {"id": "name", "header": "Name", "question": "Project name?", "options": []}
            ]}),
        );
        let Some(Effect::Card { spec, .. }) = effects.first().cloned() else {
            panic!("{effects:?}");
        };
        assert_eq!(spec.text, "Database: Which database?");
        assert_eq!(spec.choices, vec!["Postgres", "SQLite"]);
        assert_eq!(spec.origin, OriginKind::Question);
        model.carded("t1", &spec.request, CardId(6));
        let next = model.answered("t1", CardId(6), &answer(Some(1), Some("SQLite"), None));
        let Some(Effect::Card { spec: second, .. }) = next.first().cloned() else {
            panic!("the second question: {next:?}");
        };
        assert_eq!(second.text, "Name: Project name?");
        model.carded("t1", &second.request, CardId(7));
        let out = model.answered("t1", CardId(7), &answer(None, None, Some("deckd")));
        assert_eq!(
            out,
            vec![Effect::Respond {
                id: json!(11),
                result: json!({"answers": {"db": {"answers": ["SQLite"]}, "name": {"answers": ["deckd"]}}}),
            }]
        );
    }

    #[test]
    fn an_order_steers_a_running_turn_and_starts_one_otherwise() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        model.registered("t1", AgentId("ag_1".into()));
        assert_eq!(
            model.order("t1", "also update the docs", "m1"),
            Some(Effect::Request {
                method: "turn/start",
                params: json!({"threadId": "t1", "input": [{"type": "text", "text": "also update the docs"}]}),
            })
        );
        model.notification(
            "turn/started",
            &json!({"threadId": "t1", "turn": {"id": "turn-1"}}),
        );
        assert_eq!(
            model.order("t1", "stop and explain", "m2"),
            Some(Effect::Request {
                method: "turn/steer",
                params: json!({
                    "threadId": "t1", "expectedTurnId": "turn-1", "clientUserMessageId": "m2",
                    "input": [{"type": "text", "text": "stop and explain"}],
                }),
            })
        );
        // A rejected steer becomes the next turn.
        assert!(matches!(
            model.steer_rejected("t1", "stop and explain"),
            Some(Effect::Request {
                method: "turn/start",
                ..
            })
        ));
        assert_eq!(model.order("nope", "x", "m"), None);
    }

    #[test]
    fn unknown_server_requests_are_left_to_the_native_client() {
        let mut model = Model::default();
        model.loaded(&[loaded("t1", None, false)]);
        let out = model.server_request(
            &json!(1),
            "mcpServer/elicitation/request",
            &json!({"threadId": "t1"}),
        );
        assert!(
            out.is_empty(),
            "an error here would dismiss the native prompt"
        );
        assert!(model
            .server_request(
                &json!(2),
                "item/tool/requestUserInput",
                &json!({"threadId":"t1", "questions":[]})
            )
            .is_empty());
        assert!(model
            .server_request(
                &json!(3),
                "item/tool/requestUserInput",
                &json!({"threadId":"not-yet-discovered"})
            )
            .is_empty());
        assert!(model
            .notification("thread/tokenUsage/updated", &json!({"threadId": "t1"}))
            .is_empty());
        // The account's rate limits are about no thread and still land.
        let out = model.notification(
            "account/rateLimits/updated",
            &json!({"rateLimits": {
                "limitId": "codex",
                "primary": {"usedPercent": 17, "windowDurationMins": 300, "resetsAt": 1_791_644_400},
                "secondary": {"usedPercent": 63, "windowDurationMins": 10080, "resetsAt": 1_791_900_000}
            }}),
        );
        let [Effect::Usage(windows)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(
            windows.iter().map(|w| (w.label.as_str(), w.percent)).collect::<Vec<_>>(),
            vec![("session", 17), ("week", 63)]
        );
        assert!(model
            .notification("account/rateLimits/updated", &json!({"rateLimits": null}))
            .is_empty());
        assert!(model
            .notification(
                "turn/started",
                &json!({"threadId": "stranger", "turn": {"id": "x"}})
            )
            .is_empty());
        // A disconnect releases everything.
        assert_eq!(
            model.disconnected(),
            vec![Effect::Release {
                thread: "t1".into(),
                agent: None,
            }]
        );
    }

    #[test]
    fn the_socket_path_defaults_under_codex_home() {
        let explicit = socket_path(Some("/tmp/x.sock")).unwrap();
        assert_eq!(explicit, PathBuf::from("/tmp/x.sock"));
        let default = socket_path(None).unwrap();
        assert!(
            default.ends_with("app-server-control/app-server-control.sock"),
            "{default:?}"
        );
    }

    #[test]
    fn a_thread_object_reads_its_name_preview_and_activity() {
        let thread = LoadedThread::from_thread(&json!({
            "id": "t1", "name": "", "preview": "Refactor UI with shadcn", "status": {"type": "active", "activeFlags": ["waitingOnApproval"]}
        }))
        .unwrap();
        assert_eq!(thread.name.as_deref(), Some("Refactor UI with shadcn"));
        assert!(thread.active);
        assert!(LoadedThread::from_thread(&json!({"name": "x"})).is_none());
    }
    #[test]
    fn chat_deltas_are_coalesced_and_completed_items_keep_their_identity() {
        let mut model = Model::default();
        model.loaded(&[loaded("t", None, false)]);
        model.notification("item/started", &json!({"threadId":"t","item":{"type":"agentMessage","id":"a","text":""}}));
        model.notification("item/agentMessage/delta", &json!({"threadId":"t","itemId":"a","delta":"Hello "}));
        model.notification("item/agentMessage/delta", &json!({"threadId":"t","itemId":"a","delta":"**world**"}));
        let effects = model.flush_messages();
        assert!(matches!(&effects[..], [Effect::Message { message, text, .. }] if message.id == "a" && message.streaming && text == "Hello **world**"));
        assert!(model.flush_messages().is_empty());
        model.notification("item/completed", &json!({"threadId":"t","item":{"type":"agentMessage","id":"a","text":"Hello **world**!"}}));
        assert!(matches!(&model.flush_messages()[..], [Effect::Message { message, text, .. }] if message.id == "a" && !message.streaming && text.ends_with('!')));
    }

    #[test]
    fn resuming_hydrates_public_history_in_order_once() {
        let thread = LoadedThread::from_thread(&json!({"id":"t", "turns":[{"items":[
            {"id":"u","type":"userMessage","content":[{"type":"text","text":"Test this"}]},
            {"id":"r","type":"reasoning","text":"private"},
            {"id":"c","type":"commandExecution","command":"cargo test","status":"completed","aggregatedOutput":"12 passed"},
            {"id":"a","type":"agentMessage","text":"All passed"}
        ]}]})).unwrap();
        let mut model = Model::default();
        let effects = model.loaded(std::slice::from_ref(&thread));
        let ids = effects.iter().filter_map(|e| match e { Effect::Message { message, .. } => Some(message.id.as_str()), _ => None }).collect::<Vec<_>>();
        assert_eq!(ids, ["u", "c", "a"]);
        assert!(model.loaded(&[thread]).is_empty());
    }

}
