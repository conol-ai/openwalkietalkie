//! `deck_client::Client` against a tiny fake NDJSON server (a tokio
//! `UnixListener` under `$TMPDIR`) and a fake WS server (tokio-tungstenite
//! on an ephemeral loopback port): the hello round trip, calls matched by
//! id out of order, event and resync notifications, the reconnect with
//! `since: rev` (contract §5 test 15, `client::reconnects_with_since_rev`),
//! a refused hello, close. No daemon, no real data dir, no port 47201.
use deck_client::{Client, ClientConfig, ClientError, ClientEvent, Transport};
use deck_model::{ApiError, EventKind, Scope, SessionId, Snapshot};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

const WAIT: Duration = Duration::from_secs(5);

/// A short socket path under `$TMPDIR` (macOS `sun_path` is 104 bytes).
fn socket_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("dc-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

struct Peer {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Peer {
    async fn accept(listener: &UnixListener) -> Peer {
        let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .expect("a connection within 5 s")
            .unwrap();
        let (r, w) = stream.into_split();
        Peer {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// The next line as JSON; `None` at EOF.
    async fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(WAIT, self.reader.read_line(&mut line))
            .await
            .expect("a line within 5 s")
            .unwrap();
        if n == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).unwrap_or_else(|_| panic!("not json: {line:?}")))
    }

    async fn write(&mut self, value: Value) {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.writer.flush().await.unwrap();
    }

    /// Read the hello, check its shape, answer it with `rev` and `extra`
    /// merged into the result. Returns the hello's params.
    async fn hello(&mut self, rev: u64, extra: Value) -> Value {
        let hello = self.read().await.expect("a hello line");
        assert_eq!(hello["jsonrpc"], "2.0");
        assert_eq!(hello["method"], "session.hello");
        assert!(hello["id"].is_number(), "{hello}");
        let params = hello["params"].clone();
        let mut result = json!({
            "api": "1",
            "daemon": "0.1.0",
            "principal": {"id": "uid:501", "scopes": params["scopes"].clone(), "trusted": false},
            "session": "sess_2",
            "rev": rev,
        });
        if params["snapshot"] == true {
            result["snapshot"] = serde_json::to_value(Snapshot {
                rev,
                ..Snapshot::default()
            })
            .unwrap();
        }
        if let Value::Object(extra) = extra {
            for (k, v) in extra {
                result[k] = v;
            }
        }
        self.write(json!({"jsonrpc": "2.0", "id": hello["id"], "result": result}))
            .await;
        params
    }

    async fn event(&mut self, rev: u64, kind: &str) {
        self.write(json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "rev": rev,
                "ts": "2026-09-18T19:42:03.120Z",
                "type": kind,
                "principal": "uid:501",
                "data": {"rev": rev},
            },
        }))
        .await;
    }
}

fn config(path: &Path, name: &str) -> ClientConfig {
    ClientConfig {
        transport: Transport::Uds(path.to_path_buf()),
        client_name: name.into(),
        client_version: "t".into(),
        scopes: vec![Scope::Observe, Scope::Agent],
        subscribe: vec!["card.*".into()],
        snapshot: false,
        reconnect: false,
        content: None,
    }
}

async fn next(rx: &mut broadcast::Receiver<ClientEvent>) -> ClientEvent {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("a client event within 5 s")
        .expect("the client's event channel is open")
}

#[tokio::test]
async fn hello_round_trip() {
    let path = socket_path("hello");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        let params = peer.hello(7, json!({})).await;
        assert_eq!(params["client"]["name"], "t");
        assert_eq!(params["client"]["version"], "t");
        assert_eq!(params["scopes"], json!(["observe", "agent"]));
        assert_eq!(params["subscribe"], json!(["card.*"]));
        assert_eq!(params["snapshot"], true);
        assert!(params.get("since").is_none(), "{params}");
        assert!(params.get("token").is_none(), "no token on the uds");
        // Not asked for: the daemon's own default applies, as it always did.
        assert!(params.get("content").is_none(), "{params}");
        // close() ends the session: EOF on the server side.
        assert!(peer.read().await.is_none(), "closed after close()");
    });
    let mut cfg = config(&path, "t");
    cfg.snapshot = true;
    let client = Client::connect(cfg).await.unwrap();
    let hello = client.hello();
    assert_eq!(hello.api, "1");
    assert_eq!(hello.daemon, "0.1.0");
    assert_eq!(hello.session, SessionId(2));
    assert_eq!(hello.principal, "uid:501");
    assert_eq!(hello.scopes, vec![Scope::Observe, Scope::Agent]);
    assert!(!hello.trusted);
    assert_eq!(hello.rev, 7);
    assert_eq!(hello.snapshot.as_ref().map(|s| s.rev), Some(7));
    assert_eq!(client.latest_hello().rev, 7);
    assert_eq!(client.last_rev(), 7);
    assert!(client.connected());
    assert!(format!("{client:?}").contains("sess_2"));
    client.close().await;
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_client_that_wants_no_session_text_says_so_in_its_hello() {
    let path = socket_path("content");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        let params = peer.hello(1, json!({})).await;
        assert_eq!(params["content"], false, "{params}");
        assert!(peer.read().await.is_none(), "closed after close()");
    });
    let mut cfg = config(&path, "t");
    cfg.content = Some(false);
    let client = Client::connect(cfg).await.unwrap();
    client.close().await;
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn calls_match_responses_by_id_out_of_order() {
    let path = socket_path("calls");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.hello(1, json!({})).await;
        let a = peer.read().await.unwrap();
        let b = peer.read().await.unwrap();
        assert_eq!(a["method"], "state.get");
        assert_eq!(b["method"], "agent.get");
        assert_eq!(b["params"]["agent"], "ag_1");
        assert_ne!(a["id"], b["id"]);
        // A notification in between, then b before a, then a as an error.
        peer.event(2, "card.set").await;
        peer.write(json!({"jsonrpc": "2.0", "id": b["id"], "result": {"id": "ag_1"}}))
            .await;
        peer.write(json!({
            "jsonrpc": "2.0", "id": a["id"],
            "error": {"code": -32002, "message": "forbidden", "data": {"need": "observe"}},
        }))
        .await;
        // A batch response and a call answered with a null result.
        let c = peer.read().await.unwrap();
        let d = peer.read().await.unwrap();
        peer.write(json!([
            {"jsonrpc": "2.0", "id": d["id"], "result": null},
            {"jsonrpc": "2.0", "id": c["id"], "result": {"ts": "now"}},
        ]))
        .await;
        // A response nobody waits for is ignored; the session goes on.
        peer.write(json!({"jsonrpc": "2.0", "id": 999, "result": 1}))
            .await;
        let e = peer.read().await.unwrap();
        peer.write(json!({"jsonrpc": "2.0", "id": e["id"], "result": {"rev": 3}}))
            .await;
        assert!(peer.read().await.is_none());
    });
    let client = Client::connect(config(&path, "t")).await.unwrap();
    let mut events = client.events();
    let (a, b) = tokio::join!(
        client.call("state.get", json!({})),
        client.call_as::<Value>("agent.get", json!({"agent": "ag_1"})),
    );
    let err = a.unwrap_err();
    match &err {
        ClientError::Rpc {
            code,
            message,
            data,
        } => {
            assert_eq!(*code, ApiError::FORBIDDEN);
            assert_eq!(message, "forbidden");
            assert_eq!(data.as_ref().unwrap()["need"], "observe");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(err.api_error().unwrap().code, ApiError::FORBIDDEN);
    assert_eq!(b.unwrap()["id"], "ag_1");
    #[derive(serde::Deserialize, Debug)]
    struct Rev {
        rev: u64,
    }
    let (c, d) = tokio::join!(
        client.call("session.ping", json!({})),
        client.call_as::<Rev>("nothing", Value::Null),
    );
    assert_eq!(c.unwrap()["ts"], "now");
    assert!(
        matches!(d, Err(ClientError::Protocol(_))),
        "a null result is not a Rev: {d:?}"
    );
    let rev: Rev = client.call_as("agent.update", json!({})).await.unwrap();
    assert_eq!(rev.rev, 3);
    match next(&mut events).await {
        ClientEvent::Event(e) => assert_eq!(e.kind, EventKind::CardSet),
        other => panic!("{other:?}"),
    }
    client.close().await;
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn event_and_resync_notifications_are_broadcast() {
    let path = socket_path("events");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.hello(10, json!({})).await;
        peer.event(11, "card.set").await;
        peer.event(12, "agent.changed").await;
        peer.write(json!({"jsonrpc": "2.0", "method": "resync", "params": {"rev": 40}}))
            .await;
        // Unknown notifications and non-JSON lines are ignored.
        peer.write(json!({"jsonrpc": "2.0", "method": "whatever", "params": {}}))
            .await;
        peer.writer.write_all(b"not json\n").await.unwrap();
        peer.event(41, "card.cleared").await;
        assert!(peer.read().await.is_none());
    });
    let client = Client::connect(config(&path, "t")).await.unwrap();
    let mut rx = client.events();
    let mut second = client.events();
    match next(&mut rx).await {
        ClientEvent::Event(e) => {
            assert_eq!(e.rev, 11);
            assert_eq!(e.kind, EventKind::CardSet);
            assert_eq!(e.data["rev"], 11);
        }
        other => panic!("{other:?}"),
    }
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.kind, EventKind::AgentChanged),
        other => panic!("{other:?}"),
    }
    match next(&mut rx).await {
        ClientEvent::Resync { rev } => assert_eq!(rev, 40),
        other => panic!("{other:?}"),
    }
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.rev, 41),
        other => panic!("{other:?}"),
    }
    assert_eq!(client.last_rev(), 41);
    // The second receiver started at the first `events()` call's tail or
    // later: it sees the same stream from its subscription point on.
    let mut seen = 0;
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(50), second.recv()).await {
        seen += 1;
    }
    assert!(seen <= 4);
    client.close().await;
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn reconnects_with_since_rev() {
    let path = socket_path("reconnect");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        // 1. The first connection: rev 3, one event, then the drop.
        let mut peer = Peer::accept(&listener).await;
        let params = peer.hello(3, json!({})).await;
        assert!(params.get("since").is_none());
        peer.event(4, "card.set").await;
        // A call nobody answers: the drop fails it with Disconnected.
        let pending = peer.read().await.unwrap();
        assert_eq!(pending["method"], "state.get");
        drop(peer);
        // 2. The reconnect hellos with since: 4; the ring replays 5 and 6.
        let mut peer = Peer::accept(&listener).await;
        let params = peer.hello(6, json!({"replayed": 2})).await;
        assert_eq!(params["since"], 4, "{params}");
        assert_eq!(params["client"]["name"], "t");
        peer.event(5, "card.answered").await;
        peer.event(6, "card.cleared").await;
        let ping = peer.read().await.unwrap();
        assert_eq!(ping["method"], "session.ping");
        peer.write(json!({"jsonrpc": "2.0", "id": ping["id"], "result": {"ts": "t"}}))
            .await;
        drop(peer);
        // 3. A second drop: the backoff restarted at 250 ms; since is 6 now
        //    and the ring has moved on: gap.
        let mut peer = Peer::accept(&listener).await;
        let params = peer.hello(20, json!({"gap": true})).await;
        assert_eq!(params["since"], 6, "{params}");
        // The client closes: EOF here and no further connection.
        assert!(peer.read().await.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(700), listener.accept())
                .await
                .is_err(),
            "no reconnect after close()"
        );
    });
    let mut cfg = config(&path, "t");
    cfg.reconnect = true;
    let client = Client::connect(cfg).await.unwrap();
    assert_eq!(client.hello().rev, 3);
    let mut rx = client.events();
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.rev, 4),
        other => panic!("{other:?}"),
    }
    let started = std::time::Instant::now();
    let pending = client.call("state.get", json!({}));
    let (pending, first) = tokio::join!(pending, next(&mut rx));
    assert!(
        matches!(pending, Err(ClientError::Disconnected)),
        "{pending:?}"
    );
    assert!(matches!(first, ClientEvent::Disconnected), "{first:?}");
    assert!(!client.connected());
    // Between the drop and the reconnect a call fails at once.
    assert!(matches!(
        client.call("session.ping", json!({})).await,
        Err(ClientError::Disconnected)
    ));
    match next(&mut rx).await {
        ClientEvent::Reconnected {
            hello,
            replayed,
            gap,
        } => {
            assert_eq!(hello.rev, 6);
            assert_eq!(replayed, 2);
            assert!(!gap);
        }
        other => panic!("{other:?}"),
    }
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(240),
        "the backoff waited: {took:?}"
    );
    assert!(client.connected());
    assert_eq!(client.latest_hello().rev, 6);
    assert_eq!(client.hello().rev, 3, "the connect-time hello is kept");
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.rev, 5),
        other => panic!("{other:?}"),
    }
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.rev, 6),
        other => panic!("{other:?}"),
    }
    assert_eq!(client.last_rev(), 6);
    assert_eq!(
        client.call("session.ping", json!({})).await.unwrap()["ts"],
        "t"
    );
    assert!(matches!(next(&mut rx).await, ClientEvent::Disconnected));
    match next(&mut rx).await {
        ClientEvent::Reconnected { hello, gap, .. } => {
            assert_eq!(hello.rev, 20);
            assert!(gap);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        client.last_rev(),
        20,
        "a gap moves `since` to the daemon's rev"
    );
    client.close().await;
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_refused_hello_is_refused_and_a_dropped_client_closes() {
    let path = socket_path("refused");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        let hello = peer.read().await.unwrap();
        peer.write(json!({
            "jsonrpc": "2.0", "id": hello["id"],
            "error": {"code": -32002, "message": "forbidden", "data": {"need": "operate", "reason": "agent_ancestry"}},
        }))
        .await;
        // A peer that closes before answering.
        let mut peer = Peer::accept(&listener).await;
        let _ = peer.read().await.unwrap();
        drop(peer);
        // A hello that is answered: the client is dropped without close().
        let mut peer = Peer::accept(&listener).await;
        peer.hello(1, json!({})).await;
        assert!(peer.read().await.is_none(), "dropping the client closes");
    });
    let err = Client::connect(config(&path, "t")).await.unwrap_err();
    match err {
        ClientError::Refused(e) => {
            assert_eq!(e.code, ApiError::FORBIDDEN);
            assert_eq!(e.data["reason"], "agent_ancestry");
        }
        other => panic!("{other:?}"),
    }
    let err = Client::connect(config(&path, "t")).await.unwrap_err();
    assert!(
        matches!(err, ClientError::Protocol(ref why) if why.contains("closed")),
        "{err}"
    );
    let client = Client::connect(config(&path, "t")).await.unwrap();
    drop(client);
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn ws_transport_hellos_with_the_bearer_in_the_handshake() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::http::StatusCode;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        // 1. No bearer: 401 at the handshake.
        let (stream, _) = listener.accept().await.unwrap();
        let refused = tokio_tungstenite::accept_hdr_async(
            stream,
            // The `Err` is tungstenite's own rejection response, sized by it.
            #[allow(clippy::result_large_err)]
            |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
             res: tokio_tungstenite::tungstenite::handshake::server::Response| {
                if req
                    .headers()
                    .get("authorization")
                    .is_some_and(|h| h == "Bearer dk_test")
                {
                    Ok(res)
                } else {
                    let mut err =
                        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(None);
                    *err.status_mut() = StatusCode::UNAUTHORIZED;
                    Err(err)
                }
            },
        )
        .await;
        assert!(refused.is_err());
        // 2. The bearer: hello over a text frame, a call, an event, close.
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_hdr_async(
            stream,
            // The `Err` is tungstenite's own rejection response, sized by it.
            #[allow(clippy::result_large_err)]
            |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
             res: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert_eq!(req.uri().path(), "/v1/ws");
                assert_eq!(req.headers()["authorization"], "Bearer dk_test");
                Ok(res)
            },
        )
        .await
        .unwrap();
        let hello: Value = match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(hello["method"], "session.hello");
        assert!(
            hello["params"].get("token").is_none(),
            "the token is in the header only"
        );
        assert_eq!(hello["params"]["client"]["name"], "gui");
        ws.send(Message::text(
            json!({"jsonrpc": "2.0", "id": hello["id"], "result": {
                "api": "1", "daemon": "0.1.0",
                "principal": {"id": "token:gate", "scopes": ["observe"], "trusted": false},
                "session": "sess_9", "rev": 2,
            }})
            .to_string(),
        ))
        .await
        .unwrap();
        // A ping frame is answered by the client transparently.
        ws.send(Message::Ping(vec![1].into())).await.unwrap();
        let call: Value = match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
            Message::Pong(_) => match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        assert_eq!(call["method"], "state.get");
        ws.send(Message::text(
            json!({"jsonrpc": "2.0", "id": call["id"], "result": {"rev": 2}}).to_string(),
        ))
        .await
        .unwrap();
        ws.send(Message::text(
            json!({"jsonrpc": "2.0", "method": "event", "params": {
                "rev": 3, "ts": "2026-09-18T19:42:03.120Z", "type": "card.set",
                "principal": "uid:501", "data": {},
            }})
            .to_string(),
        ))
        .await
        .unwrap();
        // The client closes: a close frame or the end of the stream.
        loop {
            match ws.next().await {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }
        }
    });
    let url = format!("ws://127.0.0.1:{port}/v1/ws");
    let err = Client::connect(ClientConfig::ws(&url, "dk_wrong", "gui"))
        .await
        .unwrap_err();
    match err {
        ClientError::Refused(e) => assert_eq!(e.code, ApiError::UNAUTHORIZED),
        other => panic!("{other:?}"),
    }
    let mut cfg = ClientConfig::ws(&url, "dk_test", "gui");
    cfg.scopes = vec![Scope::Observe];
    cfg.reconnect = false;
    let client = Client::connect(cfg).await.unwrap();
    assert_eq!(client.hello().session, SessionId(9));
    assert_eq!(client.hello().principal, "token:gate");
    let mut rx = client.events();
    assert_eq!(client.call("state.get", json!({})).await.unwrap()["rev"], 2);
    match next(&mut rx).await {
        ClientEvent::Event(e) => assert_eq!(e.rev, 3),
        other => panic!("{other:?}"),
    }
    client.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn a_disconnect_without_reconnect_ends_the_session() {
    let path = socket_path("noreconnect");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.hello(1, json!({})).await;
        drop(peer);
        assert!(
            tokio::time::timeout(Duration::from_millis(700), listener.accept())
                .await
                .is_err(),
            "no reconnect when reconnect is off"
        );
    });
    let client = Client::connect(config(&path, "t")).await.unwrap();
    let mut rx = client.events();
    assert!(matches!(next(&mut rx).await, ClientEvent::Disconnected));
    assert!(!client.connected());
    assert!(matches!(
        client.call("session.ping", json!({})).await,
        Err(ClientError::Disconnected)
    ));
    server.await.unwrap();
    client.close().await;
    let _ = std::fs::remove_file(&path);
}
