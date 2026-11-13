//! `deckd::codexd` against a fake Codex app-server daemon: a WebSocket on a
//! Unix socket that answers `initialize`, lists one loaded thread, raises an
//! approval and watches what comes back. The daemon under test is the
//! in-process one (`common::Daemon`); the client is run against its `Api`
//! exactly as `deckd run` runs it.
//!
//! Three promises (design §5.2 after the daemon client):
//!
//! 1. A thread that was already loaded when the client connected is on the
//!    roster as `codex:<thread id>`, with the daemon's name and state.
//! 2. A server approval request is a permission card, and the operator's
//!    answer is the JSON-RPC response the server was blocked on.
//! 3. An order becomes `turn/steer` while a turn runs and `turn/start`
//!    otherwise — and the hook door answers `{}` for a thread the client
//!    owns, so the two paths never fight.
mod common;

use common::{Daemon, Options};
use deck_model::{AgentState, CardStatus, Deliver, Kind, OriginKind};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const PATIENCE: Duration = Duration::from_secs(10);

/// What the fake received from the client, in order.
#[derive(Debug)]
enum Seen {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    #[allow(dead_code)]
    Notification { method: String },
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
}

/// The fake daemon: one connection, scripted.
struct Fake {
    loaded: Arc<Mutex<Vec<String>>>,
    methods: Arc<Mutex<Vec<String>>>,
    settings: Arc<Mutex<(String, String)>>,
    reject_settings: Arc<std::sync::atomic::AtomicBool>,
    reject_steer: Arc<std::sync::atomic::AtomicBool>,
    reject_turns: Arc<std::sync::atomic::AtomicBool>,
    stall_history: Arc<std::sync::atomic::AtomicBool>,
    history: Arc<Mutex<Vec<Value>>>,
    seen: mpsc::UnboundedReceiver<Seen>,
    /// Push a server→client message (a notification or a request).
    push: mpsc::UnboundedSender<Value>,
    account: Arc<Mutex<Option<(String, u8)>>>,
}

fn socket_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dk-codexd-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("ctl.sock")
}

/// Accept one WebSocket client on `path` and script the app-server surface:
/// `initialize` → ok, `thread/loaded/list` → `[t1]`, `thread/resume t1` →
/// an idle thread named "Refactor UI", everything else → `{}`. Every
/// message the client sends is reported on `seen`; `push` injects
/// server-side messages.
async fn fake_daemon(path: PathBuf) -> Fake {
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let (seen_tx, seen_rx) = mpsc::unbounded_channel();
    let (push_tx, mut push_rx) = mpsc::unbounded_channel::<Value>();
    let account: Arc<Mutex<Option<(String, u8)>>> = Arc::default();
    let current_account = account.clone();
    let methods: Arc<Mutex<Vec<String>>> = Arc::default();
    let observed = methods.clone();
    let settings = Arc::new(Mutex::new(("model-a".to_string(), "high".to_string())));
    let current_settings = settings.clone();
    let reject_settings = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rejected = reject_settings.clone();
    let reject_steer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let steer_rejected = reject_steer.clone();
    let reject_turns = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let turns_rejected = reject_turns.clone();
    let stall_history = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let history_stalled = stall_history.clone();
    let history: Arc<Mutex<Vec<Value>>> = Arc::default();
    let current_history = history.clone();
    let loaded = Arc::new(Mutex::new(vec!["t1".to_string()]));
    let current_loaded = loaded.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let ws = match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => ws,
            Err(_) => return,
        };
        let (mut sink, mut source) = ws.split();
        loop {
            tokio::select! {
                pushed = push_rx.recv() => {
                    let Some(value) = pushed else { break };
                    if sink.send(tokio_tungstenite::tungstenite::Message::Text(value.to_string().into())).await.is_err() {
                        break;
                    }
                }
                msg = source.next() => {
                    let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = msg else { break };
                    let value: Value = serde_json::from_str(&text).unwrap();
                    let method = value["method"].as_str().map(str::to_string);
                    let id = value.get("id").cloned();
                    match (method, id) {
                        (Some(method), Some(id)) => {
                            let params = value.get("params").cloned().unwrap_or(Value::Null);
                            observed.lock().unwrap().push(method.clone());
                            if method == "thread/items/list" && history_stalled.load(std::sync::atomic::Ordering::SeqCst) {
                                let _ = seen_tx.send(Seen::Request { id, method, params });
                                continue;
                            }
                            if (method == "turn/steer" && steer_rejected.load(std::sync::atomic::Ordering::SeqCst))
                                || (matches!(method.as_str(), "turn/start" | "turn/steer") && turns_rejected.load(std::sync::atomic::Ordering::SeqCst)) {
                                let _ = seen_tx.send(Seen::Request { id: id.clone(), method, params });
                                let _ = sink.send(tokio_tungstenite::tungstenite::Message::Text(
                                    json!({"id": id, "error": {"code": -32000, "message": "turn unavailable"}}).to_string().into())).await;
                                continue;
                            }
                            if method == "thread/settings/update" && rejected.load(std::sync::atomic::Ordering::SeqCst) {
                                let _ = seen_tx.send(Seen::Request { id: id.clone(), method, params });
                                let _ = sink.send(tokio_tungstenite::tungstenite::Message::Text(
                                    json!({"id": id, "error": {"code": -32000, "message": "unsupported choice"}}).to_string().into())).await;
                                continue;
                            }
                            let reply = match method.as_str() {
                                "initialize" => json!({"userAgent": "fake codex"}),
                                "account/read" => match current_account.lock().unwrap().as_ref() {
                                    Some((email, _)) => json!({"account": {"type": "chatgpt", "email": email, "planType": "pro"}}),
                                    None => json!({"account": null}),
                                },
                                "account/rateLimits/read" => match current_account.lock().unwrap().as_ref() {
                                    Some((_, percent)) => json!({"rateLimits": {"primary": {"usedPercent": percent, "windowDurationMins": 300}}}),
                                    None => json!({}),
                                },
                                "model/list" => json!({"data": [
                                    {"model":"model-a", "displayName":"Model A", "hidden":false, "defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]},
                                    {"model":"model-b", "displayName":"Model B", "hidden":false, "defaultReasoningEffort":"low", "supportedReasoningEfforts":[{"reasoningEffort":"low"}]}
                                ], "nextCursor":null}),
                                "thread/settings/update" => {
                                    let mut selected = current_settings.lock().unwrap();
                                    if let Some(m) = params["model"].as_str() { selected.0 = m.into(); }
                                    if let Some(e) = params["effort"].as_str() { selected.1 = e.into(); }
                                    json!({})
                                }
                                "thread/loaded/list" => json!({"data": *current_loaded.lock().unwrap(), "nextCursor": null}),
                                "thread/items/list" => {
                                    let history = current_history.lock().unwrap();
                                    let mut entries: Vec<Value> = history.iter().flat_map(|turn| {
                                        turn["items"].as_array().into_iter().flatten().map(|item|
                                            json!({"turnId":turn["id"],"item":item})
                                        )
                                    }).collect();
                                    if params["sortDirection"] == "desc" { entries.reverse(); }
                                    let start = params["cursor"].as_str().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
                                    let limit = params["limit"].as_u64().unwrap_or(8) as usize;
                                    assert!(limit <= 8, "history pages stay small");
                                    let end = (start + limit).min(entries.len());
                                    json!({"data":entries[start..end], "nextCursor":(end < entries.len()).then(|| end.to_string())})
                                },
                                "thread/resume" => {
                                    assert_eq!(params["excludeTurns"], true, "never hydrate a whole thread in one frame");
                                    let selected = current_settings.lock().unwrap();
                                    json!({"model":selected.0, "reasoningEffort":selected.1, "thread": {
                                        "id": params["threadId"], "name": "Refactor UI",
                                        "cwd": "/Users/t/code/api", "status": {"type": "idle"},
                                        "turns": if params["excludeTurns"] == true { Vec::<Value>::new() } else { current_history.lock().unwrap().clone() },
                                    }})
                                },
                                _ => json!({}),
                            };
                            let _ = seen_tx.send(Seen::Request { id: id.clone(), method, params });
                            let _ = sink.send(tokio_tungstenite::tungstenite::Message::Text(
                                json!({"id": id, "result": reply}).to_string().into(),
                            )).await;
                        }
                        (Some(method), None) => {
                            let _ = seen_tx.send(Seen::Notification { method });
                        }
                        (None, Some(id)) => {
                            let _ = seen_tx.send(Seen::Response {
                                id,
                                result: value.get("result").cloned(),
                                error: value.get("error").cloned(),
                            });
                        }
                        (None, None) => {}
                    }
                }
            }
        }
    });
    Fake {
        loaded,
        methods,
        settings,
        reject_settings,
        reject_steer,
        reject_turns,
        stall_history,
        history,
        seen: seen_rx,
        push: push_tx,
        account,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_turn_finished_before_subscription_is_restored_from_history() {
    let path = socket_path("desktop-fast");
    let mut fake = fake_daemon(path.clone()).await;
    let daemon = Daemon::with(Options {
        codex_socket: Some(path),
        ..Options::default()
    }).await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    agent_with_key(&daemon, "codex:t1").await;
    fake.request("thread/items/list").await;

    // Desktop announces a new thread immediately, but other connections do
    // not receive its items until they resume/subscribe on the next poll.
    fake.push.send(json!({"method":"thread/started", "params":{"thread":{
        "id":"desktop-fast", "name":"Desktop link test", "cwd":"/test"
    }}})).unwrap();
    let agent = agent_with_key(&daemon, "codex:desktop-fast").await;
    *fake.history.lock().unwrap() = vec![json!({"id":"fast-turn", "status":"completed", "items":[
        {"id":"test-prompt", "type":"userMessage", "content":[{"type":"text", "text":"Reply exactly DESKTOP-LINK-OK."}]},
        {"id":"test-reply", "type":"agentMessage", "text":"DESKTOP-LINK-OK"}
    ]})];
    fake.loaded.lock().unwrap().push("desktop-fast".into());
    tokio::time::timeout(Duration::from_secs(deckd::codexd::ROSTER_S + 5), async {
        loop {
            let restored = daemon.api.bus.snapshot().agents.get(&agent.id).is_some_and(|a|
                a.messages.iter().any(|m| m.message.id == "test-reply" && m.text == "DESKTOP-LINK-OK"));
            if restored { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("the completed Desktop reply must survive the subscription gap");
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_pending_questions_restore_deliver_and_resolve_across_clients() {
    let path = socket_path("async-questions");
    let mut fake = fake_daemon(path.clone()).await;
    let key = json!(["request_user_input_async", "old-call", 0]).to_string();
    let old_reply = format!("<send_user_message_question_reply>{}</send_user_message_question_reply>",
        json!([{"answer":"Yes","question":"Already answered?","questionItemId":key}]));
    *fake.history.lock().unwrap() = vec![json!({"id":"old-turn","status":"completed","items":[
        {"id":"old-call","type":"agentMessage","delivery":"async","text":"Questions","questions":[
            {"title":"Already answered?","options":["Yes","No"]}, {"title":"Project name?","options":null}]},
        {"id":"old-reply","type":"userMessage","content":[{"type":"text","text":old_reply}]}
    ]})];
    // Force the answer and original question onto separate history pages.
    // A full resume response is forbidden by the fake above.
    {
        let mut history = fake.history.lock().unwrap();
        let items = history[0]["items"].as_array_mut().unwrap();
        let reply = items.pop().unwrap();
        for i in 0..20 {
            items.push(
                json!({"id":format!("history-{i}"), "type":"agentMessage", "text":"Earlier work"}),
            );
        }
        items.push(reply);
    }
    let daemon = Daemon::with(Options {
        codex_socket: Some(path),
        verifier: deckd::trust::FakeVerifier { trusted: true, capped: false },
        ..Options::default()
    }).await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let agent = agent_with_key(&daemon, "codex:t1").await;
    daemon.until("restored unanswered question", |s| s.agents[&agent.id].card.as_ref()
        .is_some_and(|c| c.text.as_deref() == Some("Project name?") && c.status == CardStatus::Pending)).await;
    let card = daemon.api.bus.snapshot().agents[&agent.id].card.clone().unwrap();
    assert_eq!(card.origin.kind, OriginKind::Question);
    assert_eq!(card.expires, None);
    assert!(card.free_text && card.choices.is_empty());
    daemon.until("readable restored answer", |s| s.agents[&agent.id].messages.iter()
        .any(|m| m.message.id == "old-reply" && m.text.contains("Your answer"))).await;
    let restored = daemon.api.bus.snapshot();
    let answer = restored.agents[&agent.id].messages.iter().find(|m| m.message.id == "old-reply").unwrap();
    assert!(answer.text.contains("Already answered") && answer.text.contains("> Yes"));
    assert!(!answer.text.contains("send_user_message_question_reply") && !answer.text.contains("questionItemId"));
    let mut peer = daemon.connect().await;
    peer.call("session.hello", json!({"client":{"name":"gui"},"scopes":["observe","operate"]})).await;

    // Failed delivery must offer the same question again, not lose it.
    fake.reject_turns.store(true, std::sync::atomic::Ordering::SeqCst);
    peer.call("op.answer", json!({"agent":agent.id,"card":card.id,"text":"Walkie"})).await;
    fake.request("turn/start").await;
    daemon.until("question restored after failed send", |s| s.agents[&agent.id].card.as_ref()
        .is_some_and(|c| c.id != card.id && c.text.as_deref() == Some("Project name?") && c.status == CardStatus::Pending)).await;
    fake.reject_turns.store(false, std::sync::atomic::Ordering::SeqCst);
    let retry = daemon.api.bus.snapshot().agents[&agent.id].card.clone().unwrap();
    peer.call("op.answer", json!({"agent":agent.id,"card":retry.id,"text":"Walkie"})).await;
    let (_, params) = fake.request("turn/start").await;
    let text = params["input"][0]["text"].as_str().unwrap();
    let body: Value = serde_json::from_str(text.strip_prefix("<send_user_message_question_reply>").unwrap()
        .strip_suffix("</send_user_message_question_reply>").unwrap()).unwrap();
    assert_eq!(body, json!([{"answer":"Walkie","question":"Project name?",
        "questionItemId":json!(["request_user_input_async","old-call",1]).to_string()}]));

    // Live async questions survive turn completion and status updates.
    fake.push.send(json!({"method":"item/completed","params":{"threadId":"t1","item":{
        "id":"new-call","type":"agentMessage","delivery":"async","text":"Please choose","questions":[
            {"title":"Which database?","options":["Postgres (Recommended)","SQLite"]},
            {"title":"Anything else?","options":null}]
    }}})).unwrap();
    fake.push.send(json!({"method":"turn/completed","params":{"threadId":"t1","turn":{"id":"done","status":"completed"}}})).unwrap();
    fake.push.send(json!({"method":"thread/status/changed","params":{"threadId":"t1","status":{"type":"idle"}}})).unwrap();
    daemon.until("live question after turn ends", |s| s.agents[&agent.id].card.as_ref()
        .is_some_and(|c| c.text.as_deref() == Some("Which database?") && c.status == CardStatus::Pending)).await;
    let live = daemon.api.bus.snapshot().agents[&agent.id].card.clone().unwrap();
    assert_eq!(live.expires, None);
    fake.push.send(json!({"method":"turn/started","params":{"threadId":"t1","turn":{"id":"busy"}}})).unwrap();
    // A harmless title update provides a barrier after turn/started.
    fake.push.send(json!({"method":"item/started","params":{"threadId":"t1","item":{
        "id":"busy-input","type":"userMessage","content":[{"type":"text","text":"Working in parallel"}]
    }}})).unwrap();
    daemon.until("working turn with question still visible", |s| s.agents[&agent.id].state == AgentState::Question
        && s.agents[&agent.id].title.as_deref() == Some("Working in parallel")).await;
    fake.reject_steer.store(true, std::sync::atomic::Ordering::SeqCst);
    peer.call("op.answer", json!({"agent":agent.id,"card":live.id,"choice":0})).await;
    let (_, steer) = fake.request("turn/steer").await;
    assert_eq!(steer["expectedTurnId"], "busy");
    let (_, start) = fake.request("turn/start").await;
    assert_eq!(start["input"], steer["input"]);
    assert!(start["input"][0]["text"].as_str().unwrap().contains("Postgres (Recommended)"));
    daemon.until("next queued question", |s| s.agents[&agent.id].card.as_ref()
        .is_some_and(|c| c.text.as_deref() == Some("Anything else?") && c.status == CardStatus::Pending)).await;

    let reply = format!("<send_user_message_question_reply>{}</send_user_message_question_reply>",
        json!([{"answer":"No","question":"Anything else?","questionItemId":json!(["request_user_input_async","new-call",1]).to_string()}]));
    fake.push.send(json!({"method":"item/completed","params":{"threadId":"t1","item":{
        "id":"external-reply","type":"userMessage","content":[{"type":"text","text":reply}]
    }}})).unwrap();
    daemon.until("answer in Codex clears device question", |s| s.agents[&agent.id].card.as_ref()
        .is_none_or(|c| c.status != CardStatus::Pending)).await;
    daemon.until("readable live answer", |s| s.agents[&agent.id].messages.iter()
        .any(|m| m.message.id == "external-reply" && m.text.contains("Your answer"))).await;
    let snapshot = daemon.api.bus.snapshot();
    assert_eq!(snapshot.agents[&agent.id].title.as_deref(), Some("Working in parallel"));
    let answer = snapshot.agents[&agent.id].messages.iter().find(|m| m.message.id == "external-reply").unwrap();
    assert!(answer.text.contains("Anything else") && answer.text.contains("> No"));
    assert!(!answer.text.contains("send_user_message_question_reply") && !answer.text.contains("questionItemId"));
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_follows_the_active_account_and_clears_on_logout() {
    let path = socket_path("usage-accounts");
    let mut fake = fake_daemon(path.clone()).await;
    *fake.account.lock().unwrap() = Some(("personal@example.com".into(), 12));
    let daemon = Daemon::with(Options { codex_socket: Some(path), ..Options::default() }).await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let (_, params) = fake.request("account/read").await;
    assert_eq!(params, json!({"refreshToken": false}));
    daemon.until("personal usage", |s| s.usage.as_ref().is_some_and(|u|
        u.items.len() == 1 && u.items[0].account.as_deref() == Some("personal@example.com") && u.items[0].percent == 12
    )).await;

    // Switch inside the normal refresh gap: the account notification must
    // fetch both fields again, without applying the new label to 12%.
    *fake.account.lock().unwrap() = Some(("work@company.example".into(), 78));
    fake.push.send(json!({"method": "account/updated", "params": {"authMode": "chatgpt", "planType": "pro"}})).unwrap();
    fake.request("account/read").await;
    daemon.until("work usage", |s| s.usage.as_ref().is_some_and(|u|
        u.items.len() == 1 && u.items[0].account.as_deref() == Some("work@company.example") && u.items[0].percent == 78
    )).await;

    *fake.account.lock().unwrap() = None;
    fake.push.send(json!({"method": "account/updated", "params": {"authMode": null, "planType": null}})).unwrap();
    fake.request("account/rateLimits/read").await;
    daemon.until("cleared usage", |s| s.usage.as_ref().is_some_and(|u| u.items.is_empty())).await;
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}

impl Fake {
    /// The next thing the client sent, within `PATIENCE`.
    async fn next(&mut self) -> Seen {
        tokio::time::timeout(PATIENCE, self.seen.recv())
            .await
            .expect("the client said something in time")
            .expect("the fake is still up")
    }

    /// Skip ahead to the next request of this method.
    async fn request(&mut self, want: &str) -> (Value, Value) {
        loop {
            match self.next().await {
                Seen::Request { id, method, params } if method == want => return (id, params),
                _ => {}
            }
        }
    }

    /// Skip ahead to the next response to server request `want`.
    async fn response(&mut self, want: &Value) -> (Option<Value>, Option<Value>) {
        loop {
            match self.next().await {
                Seen::Response { id, result, error } if &id == want => return (result, error),
                _ => {}
            }
        }
    }
}

async fn picker(daemon: &Daemon, matches: impl Fn(&deck_proto::SessionMenu) -> bool) -> deck_proto::SessionMenu {
    let end = tokio::time::Instant::now() + PATIENCE;
    loop {
        for op in daemon.device_ops() {
            if let deck_model::DeviceOp::Control(deck_model::Control::SessionMenu(menu)) = op {
                if matches(&menu) { return menu; }
            }
        }
        assert!(tokio::time::Instant::now() < end, "picker response arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_model_selection_updates_only_session_settings_and_confirms_the_result() {
    let path = socket_path("settings");
    let mut fake = fake_daemon(path.clone()).await;
    let daemon = Daemon::with(Options { codex_socket: Some(path), ..Options::default() }).await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let agent = agent_with_key(&daemon, "codex:t1").await;
    let slot = agent.slot.unwrap().0;
    daemon.device_input(deck_model::DeviceLine::Session { slot, field: 1, page: 0, rev: 0, choice: 255 });
    let menu = picker(&daemon, |m| m.status == 1).await;
    assert_eq!(menu.choices, ["Model A", "Model B"]);
    daemon.device_input(deck_model::DeviceLine::Session { slot, field: 1, page: 0, rev: menu.rev, choice: 1 });
    let (_, params) = fake.request("thread/settings/update").await;
    assert_eq!(params, json!({"threadId":"t1", "model":"model-b", "effort":"low"}));
    let confirmed = picker(&daemon, |m| m.status == 1 && m.model == "Model B").await;
    assert_eq!(confirmed.effort, "low");
    assert_eq!(*fake.settings.lock().unwrap(), ("model-b".into(), "low".into()));

    fake.reject_settings.store(true, std::sync::atomic::Ordering::SeqCst);
    daemon.device_input(deck_model::DeviceLine::Session { slot, field: 1, page: 0, rev: confirmed.rev, choice: 0 });
    fake.request("thread/settings/update").await;
    let failed = picker(&daemon, |m| m.status == 3).await;
    assert_eq!(failed.model, "Model B");
    assert!(failed.note.contains("could not apply"));
    assert!(!fake.methods.lock().unwrap().iter().any(|m| matches!(m.as_str(), "turn/start" | "turn/steer")),
        "changing settings never sends a prompt or starts a turn");
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}

/// Wait until the roster has an agent with this key.
async fn agent_with_key(daemon: &Daemon, key: &str) -> deck_model::Agent {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if let Some(agent) = daemon
            .api
            .bus
            .snapshot()
            .agents
            .values()
            .find(|a| a.session.key.as_deref() == Some(key))
        {
            return agent.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no agent {key} on the roster"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loaded_thread_is_an_agent_its_approval_a_card_and_an_order_a_steer() {
    let path = socket_path("e2e");
    let mut fake = fake_daemon(path.clone()).await;
    let daemon = Daemon::with(Options {
        codex_socket: Some(path.clone()),
        // An allow is a guarded operation: the peer below is trusted, as
        // the deck itself is.
        verifier: deckd::trust::FakeVerifier {
            trusted: true,
            capped: false,
        },
        ..Options::default()
    })
    .await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));

    // 1. The handshake and the roster.
    let (_, params) = fake.request("initialize").await;
    assert_eq!(params["capabilities"]["experimentalApi"], true);
    assert_eq!(params["clientInfo"]["name"], "openwalkietalkie-deckd");
    fake.request("thread/loaded/list").await;
    let (_, params) = fake.request("thread/resume").await;
    assert_eq!(params["threadId"], "t1");
    let agent = agent_with_key(&daemon, "codex:t1").await;
    assert_eq!(agent.kind, Kind::Codex);
    assert_eq!(agent.name, "Refactor UI");
    assert_eq!(agent.caps.answer, deck_model::AnswerCap::Poll);
    assert_eq!(agent.caps.order, deck_model::OrderCap::Poll);
    assert_eq!(agent.session.cwd.as_deref(), Some("/Users/t/code/api"));
    assert_eq!(agent.meta["via"], "app-server");

    // 2. A turn: working, titled from the user message, then an approval.
    fake.push
        .send(json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}))
        .unwrap();
    fake.push
        .send(json!({"method": "item/started", "params": {"threadId": "t1", "item": {
            "id": "u1", "type": "userMessage", "content": [{"type": "text", "text": "run the migration against staging"}]
        }}}))
        .unwrap();
    daemon
        .until("working + titled", |s| {
            s.agents.get(&agent.id).is_some_and(|a| {
                a.state == AgentState::Working
                    && a.title.as_deref() == Some("run the migration against staging")
            })
        })
        .await;
    fake.push
        .send(
            json!({"id": 41, "method": "item/commandExecution/requestApproval", "params": {
                "threadId": "t1", "turnId": "turn-1", "itemId": "c1",
                "command": ["psql", "-f", "migrate.sql", "staging"], "cwd": "/Users/t/code/api"
            }}),
        )
        .unwrap();
    daemon
        .until("a permission card", |s| {
            s.agents.get(&agent.id).is_some_and(|a| a.card.is_some())
        })
        .await;
    let card = daemon.api.bus.snapshot().agents[&agent.id]
        .card
        .clone()
        .unwrap();
    assert_eq!(card.origin.kind, OriginKind::Permission);
    assert_eq!(card.deliver, Deliver::Poll);
    assert_eq!(
        card.text.as_deref(),
        Some("Run: psql -f migrate.sql staging")
    );
    assert_eq!(
        card.choices
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>(),
        ["Allow", "Allow for session", "Deny"]
    );
    assert_eq!(card.status, CardStatus::Pending);

    // 3. The hook door stays out of a thread the client owns: a hook for
    //    the same thread is answered `{}` and raises nothing.
    let mut peer = daemon.connect().await;
    peer.call(
        "session.hello",
        json!({"client": {"name": "gui"}, "scopes": ["observe", "operate", "hooks:ingest"]}),
    )
    .await;
    let hook = peer
        .call(
            "hooks.codex",
            json!({"payload": {
                "session_id": "t1", "hook_event_name": "PermissionRequest",
                "tool_name": "Bash", "tool_input": {"command": "rm -rf /"}, "cwd": "/w"
            }}),
        )
        .await;
    assert_eq!(hook, json!({}), "the hook path yields to the daemon client");

    // 4. The operator allows it: the fake receives the decision as the
    //    response to request 41.
    let answer = peer
        .call(
            "op.answer",
            json!({"agent": agent.id, "card": card.id, "choice": 0}),
        )
        .await;
    assert_eq!(answer["delivered"], true, "{answer}");
    let (result, error) = fake.response(&json!(41)).await;
    assert_eq!(error, None);
    assert_eq!(result, Some(json!({"decision": "accept"})));
    daemon
        .until("the card off the deck", |s| {
            s.agents.get(&agent.id).is_some_and(|a| {
                a.card.as_ref().is_none_or(|c| {
                    !matches!(c.status, CardStatus::Pending | CardStatus::Answering { .. })
                })
            })
        })
        .await;

    // 5. An order mid-turn steers; after the turn, it starts one.
    let order = peer
        .call(
            "op.order",
            json!({"agent": agent.id, "text": "also update the README", "ctx": "talk", "source": "ptt"}),
        )
        .await;
    assert_eq!(order["via"], "poll", "{order}");
    let (_, params) = fake.request("turn/steer").await;
    assert_eq!(params["threadId"], "t1");
    assert_eq!(params["expectedTurnId"], "turn-1");
    assert_eq!(params["input"][0]["text"], "also update the README");
    fake.push.send(json!({"method":"item/started","params":{"threadId":"t1","item":{"id":"tool1","type":"commandExecution","command":"cargo test","status":"inProgress"}}})).unwrap();
    daemon.until("live tool activity", |s| s.agents[&agent.id].messages.iter().any(|m| m.message.id == "tool1" && m.message.streaming && m.message.role == deck_model::ChatRole::Tool)).await;
    fake.push.send(json!({"method":"item/completed","params":{"threadId":"t1","item":{"id":"tool1","type":"commandExecution","command":"cargo test","status":"completed","aggregatedOutput":"All tests passed"}}})).unwrap();
    fake.push.send(json!({"method":"item/started","params":{"threadId":"t1","item":{"id":"a1","type":"agentMessage","text":""}}})).unwrap();
    for delta in ["Migration applied; ", "README updated"] {
        fake.push.send(json!({"method":"item/agentMessage/delta","params":{"threadId":"t1","itemId":"a1","delta":delta}})).unwrap();
    }
    daemon.until("streaming text before turn completion", |s| s.agents[&agent.id].messages.iter().any(|m| m.message.id == "a1" && m.message.streaming && m.text == "Migration applied; README updated")).await;
    fake.push
        .send(
            json!({"method": "item/completed", "params": {"threadId": "t1", "item": {
                "id": "a1", "type": "agentMessage", "text": "Migration applied; README updated."
            }}}),
        )
        .unwrap();
    fake.push
        .send(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}}))
        .unwrap();
    daemon
        .until("ready with the reply", |s| {
            s.agents.get(&agent.id).is_some_and(|a| {
                a.state == AgentState::Ready
                    && a.reply.as_ref().and_then(|r| r.text.as_deref())
                        == Some("Migration applied; README updated.")
            })
        })
        .await;
    let order = peer
        .call(
            "op.order",
            json!({"agent": agent.id, "text": "now run the tests", "ctx": "talk", "source": "gui"}),
        )
        .await;
    assert_eq!(order["via"], "poll", "{order}");
    let (_, params) = fake.request("turn/start").await;
    assert_eq!(params["threadId"], "t1");
    assert_eq!(params["input"][0]["text"], "now run the tests");

    let history = &daemon.api.bus.snapshot().agents[&agent.id].messages;
    assert_eq!(history.iter().filter(|m| m.message.role == deck_model::ChatRole::Assistant).count(), 1, "final notification must update the streamed message");
    assert!(history.iter().any(|m| m.message.id == "tool1" && !m.message.streaming && m.text.contains("All tests passed")));

    // 6. The thread unloads: the agent goes, and the hooks path has it back.
    fake.push
        .send(json!({"method": "thread/closed", "params": {"threadId": "t1"}}))
        .unwrap();
    daemon
        .until("the agent released", |s| !s.agents.contains_key(&agent.id))
        .await;
    assert!(!daemon.api.core.adapters().daemon_owns("codex:t1"));

    let _ = stop.send(true);
    let _ = tokio::time::timeout(PATIENCE, client).await;
    daemon.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// An order the app-server will not take fails on the deck instead of
/// vanishing: the client reports it as the agent's owner, under the order's
/// id. A steer refused because the turn ended still becomes a start first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_turn_reports_the_order_failed() {
    let path = socket_path("refused-order");
    let mut fake = fake_daemon(path.clone()).await;
    let daemon = Daemon::with(Options {
        codex_socket: Some(path.clone()),
        ..Options::default()
    })
    .await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let agent = agent_with_key(&daemon, "codex:t1").await;
    let mut peer = daemon.connect().await;
    peer.call(
        "session.hello",
        json!({"client": {"name": "gui"}, "scopes": ["observe", "operate"], "subscribe": ["order.failed"]}),
    )
    .await;

    // A turn is running and the steer is refused: the same words start the
    // next turn, and nothing failed.
    fake.push
        .send(json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-9"}}}))
        .unwrap();
    daemon
        .until("working", |s| {
            s.agents[&agent.id].state == AgentState::Working
        })
        .await;
    fake.reject_steer
        .store(true, std::sync::atomic::Ordering::SeqCst);
    peer.call(
        "op.order",
        json!({"agent": agent.id, "text": "also update the README", "ctx": "talk", "source": "ptt"}),
    )
    .await;
    let (_, steer) = fake.request("turn/steer").await;
    let (_, start) = fake.request("turn/start").await;
    assert_eq!(start["input"], steer["input"]);
    fake.reject_steer
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // The server refuses the turn itself: the order fails, by its owner.
    fake.reject_turns
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let order = peer
        .call(
            "op.order",
            json!({"agent": agent.id, "text": "run the tests", "ctx": "talk", "source": "ptt"}),
        )
        .await;
    assert_eq!(order["via"], "poll", "{order}");
    fake.request("turn/start").await;
    let failed = peer
        .line_where("order.failed", |l| l["params"]["type"] == "order.failed")
        .await;
    let data = &failed["params"]["data"];
    assert_eq!(data["reason"], "turn_refused");
    assert_eq!(data["order"], order["order"]);
    assert_eq!(data["by"], "owner");
    assert_eq!(data["agent"], agent.id.0.as_str());
    daemon
        .until("the failure card", |s| {
            s.agents[&agent.id]
                .card
                .as_ref()
                .is_some_and(|c| c.text.as_deref() == Some(deck_model::NOT_DELIVERED.text[0]))
        })
        .await;

    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_history_does_not_block_questions_or_native_answers() {
    let path = socket_path("history-prompt");
    let mut fake = fake_daemon(path.clone()).await;
    fake.stall_history
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let daemon = Daemon::with(Options {
        codex_socket: Some(path),
        ..Options::default()
    })
    .await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let agent = agent_with_key(&daemon, "codex:t1").await;
    fake.request("thread/items/list").await;
    fake.push.send(json!({"id":"native-question", "method":"item/tool/requestUserInput", "params":{
        "threadId":"t1", "questions":[{"id":"q", "question":"Which database?", "options":[{"label":"SQLite"},{"label":"Postgres"}]}]
    }})).unwrap();
    daemon
        .until("question available while history is stalled", |s| {
            s.agents[&agent.id]
                .card
                .as_ref()
                .is_some_and(|c| c.origin.kind == OriginKind::Question)
        })
        .await;
    let card = daemon.api.bus.snapshot().agents[&agent.id]
        .card
        .clone()
        .unwrap();
    fake.push
        .send(json!({"method":"serverRequest/resolved", "params":{
            "threadId":"t1", "requestId":"native-question"
        }}))
        .unwrap();
    daemon
        .until("native answer clears the deck", |s| {
            s.agents[&agent.id].card.is_none()
        })
        .await;
    assert_eq!(card.status, CardStatus::Pending);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            fake.response(&json!("native-question"))
        )
        .await
        .is_err(),
        "observing or clearing a native question must not answer it"
    );
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}

/// R6, the lazy reader feed (`deckd::reader`) on the Codex side. A thread
/// the deck does not read records no history: its first read restores only
/// its questions, and a turn it runs leaves its reply and nothing else. Once
/// the deck opens it, its latest items are read again, newest first
/// (`sortDirection:"desc"`), and replace what the store kept, in order; the
/// reply on file stays the turn's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_the_deck_opens_is_read_again_and_one_it_does_not_keeps_its_replies() {
    let path = socket_path("lazy");
    let mut fake = fake_daemon(path.clone()).await;
    let user = |id: &str, text: &str| {
        json!({"id": id, "type": "userMessage", "content": [{"type": "text", "text": text}]})
    };
    let said = |id: &str, text: &str| json!({"id": id, "type": "agentMessage", "text": text});
    *fake.history.lock().unwrap() = vec![json!({"id": "old", "status": "completed",
        "items": [user("h-u", "Earlier prompt"), said("h-a", "Earlier answer")]})];
    let daemon = Daemon::with(Options {
        codex_socket: Some(path),
        lazy: true,
        ..Options::default()
    })
    .await;
    daemon
        .until("the deck is linked and the collectors run", |s| {
            s.service.dormancy.is_some_and(|d| d.collecting)
        })
        .await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let agent = agent_with_key(&daemon, "codex:t1").await;
    let (_, first) = fake.request("thread/items/list").await;
    assert_eq!(first["sortDirection"], "asc", "the first read, for questions");

    // A turn, nobody reading: its reply is stored before `ready`, and that
    // is all of it the store has.
    for (method, params) in [
        ("turn/started", json!({"threadId": "t1", "turn": {"id": "now"}})),
        ("item/started", json!({"threadId": "t1", "item": user("n-u", "Run the tests")})),
        ("item/completed", json!({"threadId": "t1", "item": said("n-a", "All green.")})),
        ("turn/completed", json!({"threadId": "t1", "turn": {"id": "now", "status": "completed"}})),
    ] {
        fake.push.send(json!({"method": method, "params": params})).unwrap();
    }
    daemon
        .until("the turn ended with its reply", |s| {
            let a = &s.agents[&agent.id];
            a.state == AgentState::Ready
                && a.reply.as_ref().and_then(|r| r.text.as_deref()) == Some("All green.")
        })
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let snapshot = daemon.api.bus.snapshot();
    let kept: Vec<&str> = snapshot.agents[&agent.id]
        .messages
        .iter()
        .map(|m| m.message.id.as_str())
        .collect();
    assert_eq!(kept.len(), 1, "the reply alone: {kept:?}");
    assert!(kept[0].starts_with("reply-"), "{kept:?}");

    // The deck opens the thread.
    fake.history.lock().unwrap().push(json!({"id": "now", "status": "completed",
        "items": [user("n-u", "Run the tests"), said("n-a", "All green.")]}));
    let slot = snapshot.agents[&agent.id].slot.expect("on the deck");
    daemon.device_input(deck_model::DeviceLine::Channel { slot: slot.0, seq: 1 });
    let (_, refill) = fake.request("thread/items/list").await;
    assert_eq!(refill["sortDirection"], "desc", "newest first");
    daemon
        .until("the thread's history, in order", |s| {
            let ids: Vec<&str> = s.agents[&agent.id]
                .messages
                .iter()
                .map(|m| m.message.id.as_str())
                .collect();
            ids == ["h-u", "h-a", "n-u", "n-a"]
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    assert_eq!(
        snapshot.agents[&agent.id].reply.as_ref().and_then(|r| r.text.as_deref()),
        Some("All green.")
    );
    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}
