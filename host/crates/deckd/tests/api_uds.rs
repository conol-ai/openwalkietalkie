//! Contract §5 rows 4 and 10: the Unix socket door — one session per
//! connection, ownership between two peers of the same uid, and the three
//! ways a first line can be wrong.
mod common;

use common::{register_params, Daemon, Options, UdsClient};
use deck_model::ApiError;

/// The scopes a UDS hello asks for in these tests.
fn hello_params(name: &str, scopes: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "client": {"name": name, "version": "1"},
        "scopes": scopes,
    })
}

/// Row 4: two peers of the same uid are two sessions; the key belongs to
/// the first until its connection goes away, then the second can claim it.
#[tokio::test]
async fn two_uds_clients_same_uid_get_a_key_conflict() {
    let daemon = Daemon::start().await;
    let mut first = daemon.connect().await;
    let hello = first
        .call("session.hello", hello_params("one", &["observe", "agent"]))
        .await;
    let uid = hello["principal"]["id"].as_str().unwrap().to_string();
    assert!(uid.starts_with("uid:"), "{hello}");
    assert_eq!(hello["principal"]["trusted"], false, "no verifier says so");
    assert_eq!(hello["api"], "1");
    let session_one = hello["session"].as_str().unwrap().to_string();
    assert!(session_one.starts_with("sess_"));

    let mut second = daemon.connect().await;
    let hello_two = second
        .call("session.hello", hello_params("two", &["observe", "agent"]))
        .await;
    let session_two = hello_two["session"].as_str().unwrap().to_string();
    assert_eq!(hello_two["principal"]["id"], uid, "the same uid");
    assert_ne!(session_one, session_two, "one session per connection");

    // The first registers the key; the second cannot take it.
    let mine = first
        .call("agent.register", register_params("one", "uds:key"))
        .await;
    let agent = mine["agent"].as_str().unwrap().to_string();
    assert_eq!(mine["owner"]["session"], session_one.as_str());
    assert_eq!(mine["owner"]["principal"], uid.as_str());
    let err = second
        .call_err("agent.register", register_params("two", "uds:key"))
        .await;
    assert_eq!(err["code"], ApiError::CONFLICT, "{err}");
    assert_eq!(err["data"]["owner"]["principal"], uid.as_str());
    assert_eq!(err["data"]["owner"]["session"], session_one.as_str());
    // Nor may it drive the agent.
    let err = second
        .call_err(
            "agent.update",
            serde_json::json!({"agent": agent, "state": "working"}),
        )
        .await;
    assert_eq!(err["code"], ApiError::CONFLICT);
    let err = second
        .call_err("agent.claim", serde_json::json!({"key": "uds:key"}))
        .await;
    assert_eq!(
        err["code"],
        ApiError::CONFLICT,
        "a live owner keeps its agent"
    );

    // The first connection goes away: the agent is orphaned and claimable.
    drop(first);
    let id = deck_model::AgentId(agent.clone());
    daemon
        .until("the first session to close", |snap| {
            snap.agents
                .get(&id)
                .is_some_and(|a| a.state == deck_model::AgentState::Idle)
        })
        .await;
    let claimed = second
        .call("agent.claim", serde_json::json!({"key": "uds:key"}))
        .await;
    assert_eq!(claimed["agent"], agent);
    assert_eq!(claimed["owner"]["session"], session_two.as_str());
    daemon.shutdown().await;
}

/// Row 10: the first line must be `session.hello`; anything else — a plain
/// call, an HTTP request line, or a line past the 256 KiB cap — is
/// answered once and the connection closes.
#[tokio::test]
async fn hello_must_be_first_and_http_is_refused() {
    let daemon = Daemon::start().await;

    // A call before the hello: -32001 {why:"hello_first"}, then EOF.
    let mut c = daemon.connect().await;
    c.write_raw(r#"{"method":"session.ping"}"#).await;
    let resp = c.line().await;
    assert_eq!(resp["error"]["code"], ApiError::UNAUTHORIZED, "{resp}");
    assert_eq!(resp["error"]["data"]["why"], "hello_first");
    assert_eq!(resp["id"], serde_json::Value::Null);
    assert!(c.closed().await, "the door hung up");

    // An HTTP request line is not JSON: -32700, then EOF (no HTTP parser
    // lives on the socket by design).
    let mut c = daemon.connect().await;
    c.write_raw("GET /v1/info HTTP/1.1\r").await;
    let resp = c.line().await;
    assert_eq!(resp["error"]["code"], ApiError::PARSE, "{resp}");
    assert!(c.closed().await);

    // A 300 KiB first line: -32012 {max}, then EOF.
    let mut c = daemon.connect().await;
    c.write_raw(&format!(
        r#"{{"id":1,"method":"session.hello","params":{{"pad":"{}"}}}}"#,
        "x".repeat(300 * 1024)
    ))
    .await;
    let resp = c.line().await;
    assert_eq!(resp["error"]["code"], ApiError::TOO_LARGE, "{resp}");
    assert_eq!(resp["error"]["data"]["max"], deckd::api::conn::LINE_MAX);
    assert!(c.closed().await);

    // A batch first is refused too (a hello is one request).
    let mut c = daemon.connect().await;
    c.write_raw(r#"[{"id":1,"method":"session.hello","params":{}}]"#)
        .await;
    let resp = c.line().await;
    assert_eq!(resp["error"]["code"], ApiError::UNAUTHORIZED);
    assert_eq!(resp["error"]["data"]["why"], "hello_first");
    assert!(c.closed().await);

    // After a good hello the very same connection serves calls, and a
    // line past the cap then also closes it.
    let mut c = daemon.connect().await;
    c.call("session.hello", hello_params("t", &["observe"]))
        .await;
    assert!(c.call("session.ping", serde_json::json!({})).await["ts"].is_string());
    c.write_raw(&"y".repeat(deckd::api::conn::LINE_MAX + 1))
        .await;
    let resp = c.line().await;
    assert_eq!(resp["error"]["code"], ApiError::TOO_LARGE);
    assert!(c.closed().await);
    daemon.shutdown().await;
}

/// The UDS maximum is `[api] uds_max_scope`: asking past it is refused by
/// name, and omitting `scopes` grants `uds_default_scope`.
#[tokio::test]
async fn the_uds_maximum_is_the_configured_scope() {
    let daemon = Daemon::with(Options {
        uds_max_scope: "observe".into(),
        uds_default_scope: "observe".into(),
        ..Options::default()
    })
    .await;
    let mut c = UdsClient::connect(&daemon.socket).await;
    let err = c
        .call_err("session.hello", hello_params("t", &["operate"]))
        .await;
    assert_eq!(err["code"], ApiError::FORBIDDEN, "{err}");
    assert_eq!(err["data"]["need"], "operate");
    assert!(err["data"].get("reason").is_none(), "not the ancestry cap");
    assert!(c.closed().await, "a refused hello closes the connection");

    let mut c = daemon.connect().await;
    let hello = c
        .call(
            "session.hello",
            serde_json::json!({"client": {"name": "t"}}),
        )
        .await;
    assert_eq!(hello["principal"]["scopes"], serde_json::json!(["observe"]));
    let err = c
        .call_err("agent.register", register_params("t", "k"))
        .await;
    assert_eq!(err["code"], ApiError::FORBIDDEN);
    assert_eq!(err["data"]["need"], "agent");
    daemon.shutdown().await;
}

/// `observe` alone never sees content; `observe:content` does. The same
/// snapshot, two sessions (§4.12).
#[tokio::test]
async fn the_uds_view_is_redacted_per_session_scope() {
    let daemon = Daemon::start().await;
    let mut owner = daemon.connect().await;
    owner
        .call(
            "session.hello",
            hello_params("owner", &["agent", "observe"]),
        )
        .await;
    let agent = owner
        .call("agent.register", register_params("owner", "uds:red"))
        .await["agent"]
        .as_str()
        .unwrap()
        .to_string();
    owner
        .call("agent.card.set", common::card_params(&agent, "secret?"))
        .await;

    let mut watcher = daemon.connect().await;
    watcher
        .call("session.hello", hello_params("watch", &["observe"]))
        .await;
    let state = watcher.call("state.get", serde_json::json!({})).await;
    let card = &state["snapshot"]["agents"][&agent]["card"];
    assert_eq!(card["text"], serde_json::Value::Null, "{state}");
    assert_eq!(card["redacted"], true);
    assert_eq!(card["choices_n"], 2);

    // The owner keeps its own content even with bare `observe`.
    let mine = owner.call("state.get", serde_json::json!({})).await;
    assert_eq!(
        mine["snapshot"]["agents"][&agent]["card"]["text"],
        "secret?"
    );
    daemon.shutdown().await;
}

/// A connected app that could not deliver an order says so, and the deck
/// shows it. The id comes with `agent.order`; `agent.order.result` reports
/// it once; the app hears the failure and sees the card the daemon put on
/// its agent, marked as the daemon's.
#[tokio::test]
async fn an_app_reports_an_order_it_could_not_deliver_and_the_deck_shows_it() {
    let daemon = Daemon::start().await;
    let mut app = daemon.connect().await;
    app.call(
        "session.hello",
        serde_json::json!({
            "client": {"name": "conol", "version": "1"},
            "scopes": ["observe", "agent"],
            "subscribe": ["order.failed", "card.set"],
        }),
    )
    .await;
    let mut params = register_params("chat", "conol:chat-1");
    params["caps"]["order"] = "push".into();
    let agent = app.call("agent.register", params).await["agent"]
        .as_str()
        .unwrap()
        .to_string();

    let mut deck = daemon.connect().await;
    deck.call(
        "session.hello",
        hello_params("deck", &["observe", "operate"]),
    )
    .await;
    let dispatched = deck
        .call(
            "op.order",
            serde_json::json!({"agent": agent, "text": "summarise the thread", "ctx": "talk", "source": "ptt"}),
        )
        .await;
    let order = dispatched["order"]
        .as_str()
        .expect("op.order names its order")
        .to_string();
    let pushed = app
        .line_where("agent.order", |l| l["params"]["type"] == "agent.order")
        .await;
    assert_eq!(pushed["params"]["data"]["order"], order.as_str());

    // The report, then everything it caused, in whatever order the door
    // writes them: the reply, the failure and the card.
    app.write_raw(
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 100, "method": "agent.order.result",
            "params": {"agent": agent, "order": order, "ok": false, "reason": "backend_refused"},
        })
        .to_string(),
    )
    .await;
    let (mut reply, mut failed, mut card) = (None, None, None);
    while reply.is_none() || failed.is_none() || card.is_none() {
        let line = app.line().await;
        let data = line["params"]["data"].clone();
        if line["id"] == 100 {
            reply = Some(line);
        } else if line["params"]["type"] == "order.failed" {
            failed = Some(data);
        } else if line["params"]["type"] == "card.set" {
            card = Some(data);
        }
    }
    let reply = reply.unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    assert_eq!(reply["result"]["shown"], "card");
    let failed = failed.unwrap();
    assert_eq!(failed["order"], order.as_str());
    assert_eq!(failed["reason"], "backend_refused");
    assert_eq!(failed["by"], "owner");
    let card = card.unwrap();
    assert_eq!(card["agent"], agent.as_str());
    assert_eq!(card["host"], deck_model::HOST_FAILURE);
    assert_eq!(card["card"]["text"], deck_model::NOT_DELIVERED.text[0]);

    // Once.
    let err = app
        .call_err(
            "agent.order.result",
            serde_json::json!({"agent": agent, "order": order, "ok": false}),
        )
        .await;
    assert_eq!(err["code"], ApiError::NOT_FOUND);
    assert_eq!(err["data"]["what"], "order");
    // Only the owner may report: not the deck, which holds no `agent`
    // scope, and not another app's session.
    let err = deck
        .call_err(
            "agent.order.result",
            serde_json::json!({"agent": agent, "order": order, "ok": false}),
        )
        .await;
    assert_eq!(err["code"], ApiError::FORBIDDEN);
    let mut stranger = daemon.connect().await;
    stranger
        .call("session.hello", hello_params("stranger", &["agent"]))
        .await;
    let err = stranger
        .call_err(
            "agent.order.result",
            serde_json::json!({"agent": agent, "order": order, "ok": false}),
        )
        .await;
    assert_eq!(err["code"], ApiError::CONFLICT);
    daemon.shutdown().await;
}
