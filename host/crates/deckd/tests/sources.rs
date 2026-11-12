//! The source switches against the in-process daemon (`common::Daemon`) and
//! a fake Codex app-server daemon: a WebSocket on a Unix socket that answers
//! the client's handshake, lists no thread and reports when the client
//! hangs up.
//!
//! The promise: `[codex] daemon` is the Codex switch of the Integrations
//! page, and switching it off ends a live connection at once, not at the
//! next reconnect, with `sources.list` saying so.
mod common;

use common::{Daemon, Options};
use deck_model::{Event, EventKind, Principal};
use deckd::bus::Published;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Longer than the client's handshake, far shorter than its roster tick
/// (`codexd::ROSTER_S`), which would end the connection by itself.
const PATIENCE: Duration = Duration::from_secs(5);

fn socket_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dk-sources-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("ctl.sock")
}

/// One connection, answered: `initialize` with a user agent, the thread and
/// model lists empty, everything else `{}`. The receiver fires once the
/// client has gone.
async fn fake_daemon(path: PathBuf) -> tokio::sync::oneshot::Receiver<()> {
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let (gone, closed) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (mut sink, mut source) = ws.split();
        while let Some(Ok(message)) = source.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            let (Some(id), Some(method)) = (request.get("id"), request["method"].as_str()) else {
                continue;
            };
            let result = match method {
                "initialize" => json!({"userAgent": "fake codex"}),
                "thread/loaded/list" | "model/list" => json!({"data": [], "nextCursor": null}),
                _ => json!({}),
            };
            let reply = json!({"id": id, "result": result}).to_string();
            if sink.send(Message::Text(reply.into())).await.is_err() {
                break;
            }
        }
        let _ = gone.send(());
    });
    closed
}

/// `sources.list`'s Codex row, read over the socket as an observer.
async fn codex_row(daemon: &Daemon) -> Value {
    let mut client = daemon.connect().await;
    client
        .hello(json!({"client": {"name": "gui", "version": "1"}, "scopes": ["observe"]}))
        .await;
    let reply = client.call("sources.list", json!({})).await;
    reply["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "codex")
        .cloned()
        .expect("a codex row")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_codex_off_ends_a_live_connection_to_its_daemon() {
    let path = socket_path("switch-off");
    let mut closed = fake_daemon(path.clone()).await;
    let daemon = Daemon::with(Options {
        codex_socket: Some(path),
        ..Options::default()
    })
    .await;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let client = tokio::spawn(deckd::codexd::run(daemon.api.clone(), stop_rx));
    let adapters = daemon.api.core.adapters().clone();
    tokio::time::timeout(PATIENCE, async {
        while !adapters.codex_connected() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the client finishes its handshake");
    let row = codex_row(&daemon).await;
    assert_eq!((&row["enabled"], &row["state"]), (&json!(true), &json!("connected")));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        matches!(closed.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)),
        "the connection holds while the switch is on"
    );

    // What the config watcher does with `config.set {"codex.daemon": false}`:
    // the new config into the cell the doors and the client read, to the
    // core, then `config.changed` on the bus.
    let mut off = (**daemon.api.config.load()).clone();
    off.codex.daemon = false;
    let off = Arc::new(off);
    daemon.api.config.store(off.clone());
    daemon.api.core.config(off).await;
    daemon.api.bus.publish(Published {
        event: Arc::new(Event {
            rev: daemon.api.bus.rev(),
            ts: deckd::core::wall_now(),
            kind: EventKind::ConfigChanged,
            principal: Principal::Uid(501),
            data: json!({"keys": ["codex.daemon"]}),
        }),
        owner: None,
    });
    tokio::time::timeout(PATIENCE, &mut closed)
        .await
        .expect("the client hangs up at once")
        .expect("the fake saw it");
    assert!(!adapters.codex_connected());
    let row = codex_row(&daemon).await;
    assert_eq!((&row["enabled"], &row["state"]), (&json!(false), &json!("off")));

    stop.send(true).unwrap();
    client.await.unwrap();
    daemon.shutdown().await;
}
