//! `deckd::grokbot` against a fake Grok Bot routine webhook on this
//! machine's loopback, with the in-process daemon (`common::Daemon`) and
//! the module run against its `Api` exactly as `deckd run` runs it.
//!
//! The promises (design §5.9):
//!
//! 1. A bot set up in `[grokbot.bots.<id>]` is on the roster as
//!    `grokbot:<id>`, kind `grok`, on the "Grok Bot" page, owned by
//!    `adapter:grok`.
//! 2. An order to it is a `POST` to its webhook with the routine's key as a
//!    bearer and a body that says how to answer; the bot shows working.
//! 3. The bot's `grokbot.say` is its reply on the deck; its `grokbot.ask` is
//!    a question card whose answer the call returns.
//! 4. A webhook that refuses, a bot with no webhook, and the switch turned
//!    off each say so on the deck or to the caller, and nothing is lost
//!    quietly.
//! 5. `grokbot.add` and `grokbot.remove` write the bot's table and its key,
//!    and the roster follows before the reply.
mod common;

use common::{Daemon, Options, PATIENCE};
use deck_model::{AdapterKind, AgentState, CardStatus, Kind, Principal};
use deckd::config::{GrokBot, GrokBotEntry};
use deckd::grokbot::keys::{BotKeys, MemoryKeys};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

/// One request the fake webhook was sent.
#[derive(Debug)]
struct Seen {
    line: String,
    headers: Vec<String>,
    body: Value,
}

/// The fake webhook: answers every POST with `status`, reports each one.
struct Hook {
    port: u16,
    seen: mpsc::UnboundedReceiver<Seen>,
    status: Arc<AtomicU16>,
}

impl Hook {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    async fn next(&mut self) -> Seen {
        tokio::time::timeout(PATIENCE, self.seen.recv())
            .await
            .expect("the webhook was called in time")
            .expect("the fake is running")
    }
}

async fn fake_hook() -> Hook {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let status = Arc::new(AtomicU16::new(200));
    let (tx, seen) = mpsc::unbounded_channel();
    let answer = status.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            let answer = answer.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).await.unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    headers.push(header.trim_end().to_string());
                }
                let length: usize = headers
                    .iter()
                    .find_map(|h| {
                        h.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                let status = answer.load(Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
                write.write_all(reply.as_bytes()).await.unwrap();
                let _ = tx.send(Seen {
                    line: line.trim_end().to_string(),
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                });
            });
        }
    });
    Hook { port, seen, status }
}

/// The agent with this key, once it is on the roster.
async fn agent_with_key(d: &Daemon, key: &str) -> deck_model::Agent {
    d.until(key, |s| {
        s.agents
            .values()
            .any(|a| a.session.key.as_deref() == Some(key))
    })
    .await;
    d.api
        .bus
        .snapshot()
        .agents
        .values()
        .find(|a| a.session.key.as_deref() == Some(key))
        .cloned()
        .unwrap()
}

/// `op.order`, as the app sends one, again after `retry_ms` when the
/// operator's rate limit says so (§11: a person does not press twice a
/// second, and these tests do).
async fn press(op: &mut common::UdsClient, agent: &deck_model::AgentId, text: &str) -> Value {
    loop {
        let resp = op
            .request(
                "op.order",
                json!({"agent": agent, "text": text, "ctx": "talk", "source": "gui"}),
            )
            .await;
        if resp["error"]["code"] == -32008 {
            let wait = resp["error"]["data"]["retry_ms"].as_u64().unwrap_or(500) + 50;
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            continue;
        }
        assert!(resp["error"].is_null(), "op.order: {resp}");
        return resp["result"].clone();
    }
}

fn bots(entries: &[(&str, &str, String)]) -> GrokBot {
    GrokBot {
        enabled: true,
        bots: entries
            .iter()
            .map(|(id, name, webhook)| {
                (
                    (*id).to_string(),
                    GrokBotEntry {
                        name: (*name).to_string(),
                        webhook: webhook.clone(),
                    },
                )
            })
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_reaches_the_bot_and_the_bot_answers_on_the_deck() {
    let mut hook = fake_hook().await;
    let d = Daemon::with(Options {
        grokbot: bots(&[("research", "Research", hook.url("/hooks/research"))]),
        ..Options::default()
    })
    .await;
    let keys = Arc::new(MemoryKeys::default());
    keys.set("research", "k-123").unwrap();
    let (stop, stop_rx) = watch::channel(false);
    let module = tokio::spawn(deckd::grokbot::run(d.api.clone(), keys.clone(), stop_rx));

    // 1. On the roster, as a Grok Bot.
    let agent = agent_with_key(&d, "grokbot:research").await;
    assert_eq!(agent.kind, Kind::Grok);
    assert_eq!(agent.name, "Research");
    assert_eq!(agent.project().as_deref(), Some("Grok Bot"));
    assert_eq!(agent.owner.principal, Principal::Adapter(AdapterKind::Grok));
    assert!(agent.slot.is_some(), "a bot holds a place on the deck");

    // 2. An order is a POST of the transcript to the webhook.
    let mut op = d.connect().await;
    op.call(
        "session.hello",
        json!({"client": {"name": "gui"}, "scopes": ["observe", "operate"], "subscribe": ["order.failed"]}),
    )
    .await;
    let order = press(&mut op, &agent.id, "Find flights to Tokyo").await;
    assert_eq!(order["via"], "poll", "{order}");
    let seen = hook.next().await;
    assert_eq!(seen.line, "POST /hooks/research HTTP/1.1");
    assert!(seen.headers.iter().any(|h| h == "Authorization: Bearer k-123"), "{seen:?}");
    assert_eq!(seen.body["text"], "Find flights to Tokyo");
    assert_eq!(seen.body["bot"], "Research");
    assert_eq!(seen.body["bot_id"], "research");
    assert_eq!(seen.body["order"], order["order"]);
    assert_eq!(seen.body["test"], false);
    assert!(
        seen.body["reply_with"]
            .as_str()
            .unwrap()
            .ends_with("grokbot say --bot 'research' '<your reply>'"),
        "{}",
        seen.body
    );
    // The test service runs on a directory of its own, as Omarchy's does:
    // the bot's commands name it, since a bot's shell has no DECKD_DATA_DIR.
    assert!(
        seen.body["reply_with"].as_str().unwrap().contains(" --data-dir '"),
        "{}",
        seen.body
    );
    assert!(seen.body["how_to_answer"].as_str().unwrap().contains("in this chat"));
    d.until("the bot working on the order", |s| {
        let a = &s.agents[&agent.id];
        a.state == AgentState::Working && a.title.as_deref() == Some("Find flights to Tokyo")
    })
    .await;

    // 3. The bot's reply, from a process it ran: `agent` scope is enough.
    let mut bot = d.connect().await;
    bot.call("session.hello", json!({"client": {"name": "deckd-ctl"}, "scopes": ["agent"]}))
        .await;
    let said = bot
        .call(
            "grokbot.say",
            json!({"bot": "research", "text": "Three flights; the cheapest leaves at 9.", "speak": false}),
        )
        .await;
    assert_eq!(said["bot"], "research");
    d.until("the reply on the deck", |s| {
        let a = &s.agents[&agent.id];
        a.state == AgentState::Ready
            && a.reply.as_ref().and_then(|r| r.text.as_deref())
                == Some("Three flights; the cheapest leaves at 9.")
    })
    .await;

    // The bot asks; the operator answers; the call returns the answer.
    let asking = tokio::spawn(async move {
        let answer = bot
            .call(
                "grokbot.ask",
                json!({"bot": "Research", "question": "Book the 9am?", "choices": ["Yes", "No"], "timeout_s": 30}),
            )
            .await;
        (bot, answer)
    });
    d.until("the question on the deck", |s| {
        s.agents[&agent.id]
            .card
            .as_ref()
            .is_some_and(|c| {
                c.status == CardStatus::Pending
                    && c.text.as_deref().is_some_and(|t| t.contains("Book the 9am?"))
            })
    })
    .await;
    let card = d.api.bus.snapshot().agents[&agent.id].card.clone().unwrap();
    assert_eq!(card.choices.len(), 2);
    let answered = op
        .call(
            "op.answer",
            json!({"agent": agent.id, "card": card.id, "choice": 0}),
        )
        .await;
    assert_eq!(answered["delivered"], true, "{answered}");
    let (mut bot, answer) = tokio::time::timeout(PATIENCE, asking).await.unwrap().unwrap();
    assert_eq!(answer["answer"]["label"], "Yes", "{answer}");
    assert_eq!(answer["answer"]["choice"], 0);
    d.until("the bot back at work", |s| s.agents[&agent.id].state == AgentState::Working)
        .await;

    // 4a. Grok Bot refuses the call: the order is reported, by its owner.
    hook.status.store(401, Ordering::SeqCst);
    let order = press(&mut op, &agent.id, "and hotels").await;
    hook.next().await;
    let failed = op
        .line_where("order.failed", |l| l["params"]["type"] == "order.failed")
        .await;
    assert_eq!(failed["params"]["data"]["reason"], "webhook_401", "{failed}");
    assert_eq!(failed["params"]["data"]["order"], order["order"]);

    // 4b. A bot that is not set up talks to the deck all the same; an order
    // to it fails as `no_webhook`.
    let unlisted = bot
        .call("grokbot.say", json!({"bot": "Travel Desk", "text": "Your visa is ready.", "speak": false}))
        .await;
    assert_eq!(unlisted["bot"], "travel-desk");
    let travel = agent_with_key(&d, "grokbot:travel-desk").await;
    assert_eq!((travel.kind, travel.name.as_str()), (Kind::Grok, "Travel Desk"));
    press(&mut op, &travel.id, "thanks").await;
    let failed = op
        .line_where("order.failed", |l| {
            l["params"]["type"] == "order.failed" && l["params"]["data"]["reason"] == "no_webhook"
        })
        .await;
    assert_eq!(failed["params"]["data"]["agent"], json!(travel.id));

    // A bot's commands are refused a question it may not ask.
    let refused = bot
        .call_err(
            "grokbot.ask",
            json!({"bot": "research", "question": "Pick?", "choices": ["a", "b", "c", "d", "e"]}),
        )
        .await;
    assert_eq!(refused["code"], -32009, "{refused}");
    // …and the owner's methods outright.
    let refused = bot.call_err("grokbot.status", json!({})).await;
    assert_eq!(refused["code"], -32002, "{refused}");

    // 4c. The switch off: the bots leave, their commands are refused.
    let mut off = (**d.api.config.load()).clone();
    off.grokbot.enabled = false;
    d.api.config.store(Arc::new(off));
    d.until("the bots off the deck", |s| {
        !s.agents
            .values()
            .any(|a| a.session.key.as_deref().is_some_and(|k| k.starts_with("grokbot:")))
    })
    .await;
    let refused = bot
        .call_err("grokbot.say", json!({"bot": "research", "text": "hello?"}))
        .await;
    assert_eq!(refused["data"]["reason"], "grokbot_off", "{refused}");

    let _ = stop.send(true);
    let _ = tokio::time::timeout(PATIENCE, module).await;
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_is_set_up_tested_and_removed_through_the_api() {
    let mut hook = fake_hook().await;
    let d = Daemon::with(Options::default()).await;
    let keys = Arc::new(MemoryKeys::default());
    let (stop, stop_rx) = watch::channel(false);
    let module = tokio::spawn(deckd::grokbot::run(d.api.clone(), keys.clone(), stop_rx));
    let mut owner = d.connect().await;
    owner
        .call("session.hello", json!({"client": {"name": "gui"}, "scopes": ["admin", "observe"]}))
        .await;

    let status = owner.call("grokbot.status", json!({})).await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["key_store"], "memory");
    assert_eq!(status["bots"], json!([]));

    // A URL that is not one, and a name that is not one, are refused.
    let refused = owner
        .call_err("grokbot.add", json!({"name": "Inbox", "webhook": "http://example.com/x", "key": "k"}))
        .await;
    assert_eq!(refused["data"]["field"], "webhook", "{refused}");
    let refused = owner
        .call_err("grokbot.add", json!({"name": " ", "webhook": hook.url("/x"), "key": "k"}))
        .await;
    assert_eq!(refused["code"], -32602, "{refused}");

    let added = owner
        .call(
            "grokbot.add",
            json!({"name": "Inbox Zero", "webhook": hook.url("/hooks/inbox"), "key": "k-inbox"}),
        )
        .await;
    assert_eq!(added, json!({"id": "inbox-zero", "name": "Inbox Zero", "has_key": true}));
    assert_eq!(keys.get("inbox-zero").unwrap().as_deref(), Some("k-inbox"));
    // The file says so, and the bot is on the deck before the reply.
    let file = std::fs::read_to_string(d.dir.join("config.toml")).unwrap();
    assert!(file.contains("[grokbot.bots.inbox-zero]"), "{file}");
    assert!(file.contains("name = \"Inbox Zero\""), "{file}");
    assert!(!file.contains("k-inbox"), "the key is never in config.toml");
    let agent = agent_with_key(&d, "grokbot:inbox-zero").await;
    assert_eq!(agent.name, "Inbox Zero");

    let status = owner.call("grokbot.status", json!({})).await;
    let row = &status["bots"][0];
    assert_eq!(row["id"], "inbox-zero");
    assert_eq!(row["webhook_host"], format!("127.0.0.1:{}", hook.port));
    assert_eq!(row["has_key"], true);
    assert_eq!(row["on_deck"], true);
    assert!(row["routine"].as_str().unwrap().contains("grokbot say --bot 'inbox-zero'"));
    assert!(!status.to_string().contains("k-inbox"), "no key in a status");

    // The test call is a POST with `test: true`, and its status comes back.
    let tested = owner.call("grokbot.test", json!({"bot": "Inbox Zero"})).await;
    assert_eq!(tested["ok"], true, "{tested}");
    assert_eq!(tested["status"], 200);
    let seen = hook.next().await;
    assert_eq!(seen.body["test"], true);
    assert!(seen.headers.iter().any(|h| h == "Authorization: Bearer k-inbox"));
    hook.status.store(404, Ordering::SeqCst);
    let tested = owner.call("grokbot.test", json!({"bot": "inbox-zero"})).await;
    assert_eq!((tested["ok"].clone(), tested["reason"].clone()), (json!(false), json!("webhook_404")));

    // Renamed in place: same id, same agent, the new name on the deck.
    owner
        .call(
            "grokbot.add",
            json!({"id": "inbox-zero", "name": "Inbox", "webhook": hook.url("/hooks/inbox")}),
        )
        .await;
    d.until("the new name", |s| s.agents[&agent.id].name == "Inbox").await;
    assert_eq!(keys.get("inbox-zero").unwrap().as_deref(), Some("k-inbox"), "the key was kept");

    let removed = owner.call("grokbot.remove", json!({"bot": "Inbox"})).await;
    assert_eq!(removed["id"], "inbox-zero");
    assert_eq!(removed["key_removed"], true);
    assert_eq!(keys.get("inbox-zero").unwrap(), None);
    d.until("the bot off the deck", |s| !s.agents.contains_key(&agent.id)).await;
    let file = std::fs::read_to_string(d.dir.join("config.toml")).unwrap();
    assert!(!file.contains("inbox-zero"), "{file}");
    let refused = owner.call_err("grokbot.remove", json!({"bot": "Inbox"})).await;
    assert_eq!(refused["code"], -32003, "{refused}");

    let _ = stop.send(true);
    let _ = tokio::time::timeout(PATIENCE, module).await;
    d.shutdown().await;
}
