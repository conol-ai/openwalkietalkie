//! `ApiClient` against a tiny fake NDJSON server (a tokio `UnixListener`
//! under `$TMPDIR`), in the shape `deck-client`'s own suite uses: the hello
//! and what it asks for, `principal.trusted` learned from the reply, a call
//! drained by the connection task and answered on its receiver, events
//! reaching the bus and the mirror, and the reconnect with `since: rev`.
//!
//! The client runs on a real [`TokioBridge`]; the fake daemon runs on the
//! test's own runtime. That split is the point: it is the same two-runtime
//! arrangement the app has, with gpui's foreground executor standing in for
//! the test's.
use deck_gui_core::api_client;
use deck_gui_core::{ApiClient, ApiEvent, AppEvent, HostState, TokioBridge};
use deck_model::{EventKind, Snapshot};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;

const WAIT: Duration = Duration::from_secs(5);

/// A short socket path under `$TMPDIR` (macOS `sun_path` is 104 bytes).
fn socket_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("dgc-{}-{name}.sock", std::process::id()));
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

    /// Read the hello, answer it at `rev` with `trusted`, and return its
    /// params so the test can check what was asked for.
    async fn hello(&mut self, rev: u64, trusted: bool) -> Value {
        let hello = self.read().await.expect("a hello line");
        assert_eq!(hello["method"], "session.hello");
        let params = hello["params"].clone();
        let mut result = json!({
            "api": "1",
            "daemon": "0.1.0",
            "principal": {
                "id": "uid:501",
                "scopes": params["scopes"].clone(),
                "trusted": trusted,
            },
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
        self.write(json!({"jsonrpc": "2.0", "id": hello["id"], "result": result}))
            .await;
        params
    }

    async fn event(&mut self, rev: u64, kind: &str, data: Value) {
        self.write(json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "rev": rev,
                "ts": "2026-09-18T19:42:03.120Z",
                "type": kind,
                "principal": "uid:501",
                "data": data,
            },
        }))
        .await;
    }
}

async fn next(rx: &deck_gui_core::EventReceiver) -> AppEvent {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("an app event within 5 s")
        .expect("the bus is open")
}

#[tokio::test]
async fn the_hello_asks_for_the_scopes_and_subscriptions_of_the_design() {
    let path = socket_path("hello");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path.clone()));
    assert_eq!(client.socket(), path.as_path());

    let mut peer = Peer::accept(&listener).await;
    let params = peer.hello(1202, true).await;
    assert_eq!(params["client"]["name"], "OpenWalkieTalkie");
    // Never a session: neither card text to read nor a card to answer.
    assert_eq!(params["scopes"], json!(["voice", "device", "admin"]));
    assert_eq!(
        params["subscribe"],
        json!([
            "*",
            "transcript.partial",
            "voice.level",
            "device.key",
            "device.macro",
            "op.confirm_requested"
        ])
    );
    assert_eq!(params["snapshot"], true);
    assert!(params.get("token").is_none(), "§8.1: no token on disk");
    assert!(
        params.get("since").is_none(),
        "the first hello has no since"
    );

    let AppEvent::Api(ApiEvent::Connected { snapshot }) = next(&rx).await else {
        panic!("the hello's snapshot is the mirror's first value");
    };
    assert_eq!(snapshot.rev, 1202);
    assert!(client.connected());
}

#[tokio::test]
async fn the_client_learns_principal_trusted_from_the_hello_reply() {
    let path = socket_path("trusted");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));
    let mut peer = Peer::accept(&listener).await;
    peer.hello(7, true).await;
    next(&rx).await;
    assert!(client.trusted(), "§4.2: the GUI was verified");
    // And the mirror's own badge reads it from here.
    let state = HostState::new(client);
    assert!(state.trusted());
}

#[tokio::test]
async fn an_untrusted_dev_build_is_surfaced_and_is_not_an_error() {
    let path = socket_path("untrusted");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));
    let mut peer = Peer::accept(&listener).await;
    peer.hello(7, false).await;
    // The connection is up and the snapshot arrived: a red badge, not a
    // failure (an ad-hoc build without `[trust] dev_requirement`).
    assert!(matches!(
        next(&rx).await,
        AppEvent::Api(ApiEvent::Connected { .. })
    ));
    assert!(client.connected());
    assert!(!client.trusted());
}

#[tokio::test]
async fn a_call_is_drained_by_the_connection_task_and_answered_on_its_receiver() {
    let path = socket_path("call");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));
    let mut peer = Peer::accept(&listener).await;
    peer.hello(7, true).await;
    next(&rx).await;

    // `call` is synchronous: no await, no runtime, just a queue push.
    let reply = client.call("device.set", json!({"k": "announce", "v": 0}));
    let request = peer.read().await.expect("the call on the socket");
    assert_eq!(request["method"], "device.set");
    assert_eq!(request["params"]["k"], "announce");
    peer.write(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"rev": 8}}))
        .await;
    let answered = tokio::time::timeout(WAIT, reply)
        .await
        .expect("a reply within 5 s")
        .expect("the call succeeded");
    assert_eq!(answered["rev"], 8);

    // A JSON-RPC error comes back as one, not as a disconnect.
    let reply = client.call("care.support_file", json!({}));
    let request = peer.read().await.unwrap();
    peer.write(json!({
        "jsonrpc": "2.0", "id": request["id"],
        "error": {"code": -32602, "message": "invalid params", "data": {"field": "to"}},
    }))
    .await;
    let err = tokio::time::timeout(WAIT, reply)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.api_error().unwrap().code, -32602);
}

#[tokio::test]
async fn events_from_the_daemon_reach_the_bus_and_fold_into_the_mirror() {
    let path = socket_path("events");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));
    let mut peer = Peer::accept(&listener).await;
    peer.hello(10, true).await;

    let mut state = HostState::new(client);
    state.handle_event(next(&rx).await);
    assert_eq!(state.mirror().rev, 10);

    peer.event(11, "device.face", json!({"f": 3})).await;
    let AppEvent::Api(ApiEvent::Event(ref event)) = next(&rx).await else {
        panic!("an event notification reaches the bus as an AppEvent");
    };
    assert_eq!(event.kind, EventKind::DeviceFace);
    state.handle_event(AppEvent::Api(ApiEvent::Event(event.clone())));
    assert_eq!(state.mirror().rev, 11);
    assert_eq!(state.mirror().device.face, Some(3));

    // A `resync` notification is the same answer the reducer gives a gap.
    peer.write(json!({"jsonrpc": "2.0", "method": "resync", "params": {"rev": 40}}))
        .await;
    assert!(matches!(next(&rx).await, AppEvent::Api(ApiEvent::Resync)));
    assert_eq!(
        state.handle_event(AppEvent::Api(ApiEvent::Resync)),
        vec![deck_gui_core::HostEffect::StateGet]
    );
}

#[tokio::test]
async fn killing_the_daemon_posts_disconnected_and_the_client_hellos_again_with_since() {
    let path = socket_path("recover");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));

    let mut peer = Peer::accept(&listener).await;
    peer.hello(10, true).await;
    peer.event(11, "device.face", json!({"f": 3})).await;
    let mut state = HostState::new(client);
    state.handle_event(next(&rx).await);
    state.handle_event(next(&rx).await);
    assert_eq!(state.mirror().rev, 11);

    // The daemon dies: the socket closes under the client.
    drop(peer);
    let effects = state.handle_event(next(&rx).await);
    assert_eq!(
        effects,
        vec![deck_gui_core::HostEffect::Reconnect(Duration::from_millis(
            250
        ))]
    );
    assert!(!state.connected());

    // It comes back, and the client's own back-off brings it in with the
    // rev it had — no replay of what the mirror already holds.
    let mut peer = Peer::accept(&listener).await;
    let params = peer.hello(30, true).await;
    assert_eq!(params["since"], 11, "the reconnect asks since: last rev");
    let effects = state.handle_event(next(&rx).await);
    assert_eq!(
        effects,
        vec![deck_gui_core::HostEffect::Toast(
            "Service reconnected".into()
        )]
    );
    assert_eq!(state.mirror().rev, 30, "the mirror is whole again");
    assert!(state.connected());
}

/// A daemon that answers the hello and drops the socket in the same breath
/// (launchd restarting a crashing agent, something else squatting the path)
/// must not be dialled at the 250 ms floor for ever: the back-off resets on a
/// connection that *stayed up*, not on one that merely opened. Measured
/// before the reset moved: 15 full hellos, each carrying the whole snapshot,
/// in 4 s.
#[tokio::test]
async fn a_daemon_that_dies_as_soon_as_it_answers_is_not_dialled_at_the_floor_for_ever() {
    let path = socket_path("flap");
    let listener = UnixListener::bind(&path).unwrap();
    let bridge = TokioBridge::start().unwrap();
    let (tx, _rx) = deck_gui_core::bus();
    let _client = ApiClient::start(&bridge, tx, api_client::config(path));
    let mut gaps = Vec::new();
    let mut last = tokio::time::Instant::now();
    for _ in 0..3 {
        let mut peer = Peer::accept(&listener).await;
        peer.hello(7, true).await;
        drop(peer);
        let now = tokio::time::Instant::now();
        gaps.push(now - last);
        last = now;
    }
    assert!(
        gaps[1] >= Duration::from_millis(200),
        "the second dial waited {:?}",
        gaps[1]
    );
    assert!(
        gaps[2] >= Duration::from_millis(400),
        "and the third waited longer still: {:?}",
        gaps[2]
    );
}

#[tokio::test]
async fn a_call_made_while_the_daemon_is_down_fails_instead_of_queueing() {
    // No listener at all: every attempt fails and the back-off runs.
    let path = socket_path("down");
    let bridge = TokioBridge::start().unwrap();
    let (tx, rx) = deck_gui_core::bus();
    let client = ApiClient::start(&bridge, tx, api_client::config(path));
    assert!(matches!(
        next(&rx).await,
        AppEvent::Api(ApiEvent::Disconnected)
    ));
    assert!(!client.connected());

    // A click now must not fire minutes later at whatever the deck has
    // become by then.
    let reply = client.call("device.set", json!({"k": "announce", "v": 0}));
    let err = tokio::time::timeout(WAIT, reply)
        .await
        .expect("a refusal within 5 s")
        .unwrap_err();
    assert!(
        matches!(err, deck_client::ClientError::Disconnected),
        "{err}"
    );
}

/// A high-volume kind that has a producer and is not named here is a hole in
/// the mirror's rev sequence: `Store::emit` counts it, `"*"` does not deliver
/// it, and the reducer can only answer the gap with a `state.get`. Iterating
/// `EventKind::ALL` guards the *rule*, so a seventh kind cannot slip in.
#[test]
fn every_high_volume_kind_with_a_producer_is_named_in_the_subscribe_list() {
    let subs = deck_gui_core::api_client::subscribe();
    for kind in deck_model::EventKind::ALL {
        if !kind.is_high_volume() {
            continue; // `"*"` covers it
        }
        let named = subs.iter().any(|s| s == kind.as_str());
        match kind {
            // No producer in v1, and they would need `observe:raw`, which this
            // app neither holds nor asks for.
            deck_model::EventKind::DeviceRaw | deck_model::EventKind::HookRaw => assert!(
                !named,
                "{} is subscribed but nothing emits it and we lack observe:raw",
                kind.as_str()
            ),
            _ => assert!(
                named,
                "{} bumps rev and `*` filters it out, so it must be named",
                kind.as_str()
            ),
        }
    }
}
