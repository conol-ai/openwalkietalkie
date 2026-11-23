//! Contract §5 rows 1, 5 and 11: the TCP door as a script sees it —
//! `curl`-shaped JSON-RPC with a bearer token, the `Host`/`Origin` guards,
//! and the logical session `(token, client.name)` that owns agents but can
//! never be pushed to.
mod common;

use common::{card_params, register_params, Daemon, Options};
use deck_model::{ApiError, Scope};

/// Row 1: a bearer token round trip, the unauthenticated `/v1/info`, the
/// 401 without a token, and the schema that documents the method table.
#[tokio::test]
async fn curl_style_rpc_round_trip() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("gate", &[Scope::Observe, Scope::Agent, Scope::Operate]);

    // `/v1/info` is the only route without a token.
    let (status, info) = daemon.http("GET", "/v1/info", &[], "").await;
    assert_eq!(status, 200);
    assert_eq!(info["api"], "1");
    assert_eq!(
        info["platform"],
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
    );
    assert_eq!(info["port"], daemon.port);
    assert_eq!(info["degraded"], serde_json::Value::Null);
    assert_eq!(info["mcp"], serde_json::Value::Null);
    assert!(info["uds"].as_str().unwrap().ends_with("deckd.sock"));
    assert!(info["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "virtual_agents"));

    // Without a bearer, `/v1/rpc` is 401 with the JSON-RPC body.
    let (status, body) = daemon
        .http(
            "POST",
            "/v1/rpc",
            &[],
            r#"{"id":1,"method":"session.ping"}"#,
        )
        .await;
    assert_eq!(status, 401);
    assert_eq!(body["error"]["code"], ApiError::UNAUTHORIZED);
    assert_eq!(body["id"], serde_json::Value::Null);
    let (status, _) = daemon
        .http(
            "POST",
            "/v1/rpc",
            &[("Authorization", "Bearer dk_not_a_token")],
            r#"{"id":1,"method":"session.ping"}"#,
        )
        .await;
    assert_eq!(status, 401, "an unknown token is no token");

    // `session.ping` and `state.get` with the token.
    let ping = daemon
        .rpc(
            &auth,
            r#"{"jsonrpc":"2.0","id":1,"method":"session.ping","params":{}}"#,
        )
        .await;
    assert_eq!(ping["id"], 1);
    assert!(
        ping["result"]["ts"].as_str().unwrap().ends_with('Z'),
        "{ping}"
    );
    let state = daemon.rpc(&auth, r#"{"id":2,"method":"state.get"}"#).await;
    assert_eq!(state["result"]["rev"], daemon.api.bus.rev());
    assert!(state["result"]["snapshot"]["device"].is_object());

    // A batch of two: both answered, in one array.
    let batch = daemon
        .rpc(
            &auth,
            r#"[{"id":"a","method":"session.ping"},{"id":"b","method":"state.get"}]"#,
        )
        .await;
    let items = batch.as_array().expect("a batch answers with an array");
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|i| i["id"] == "a" && i["result"]["ts"].is_string()));
    assert!(items
        .iter()
        .any(|i| i["id"] == "b" && i["result"]["rev"].is_number()));

    // `/v1/schema`: the model schema plus the dispatch table, behind a token.
    let (status, _) = daemon.http("GET", "/v1/schema", &[], "").await;
    assert_eq!(status, 401, "the schema is not public");
    let (status, schema) = daemon
        .http("GET", "/v1/schema", &[("Authorization", &auth)], "")
        .await;
    assert_eq!(status, 200);
    assert!(schema["request"].is_object(), "the model schema is there");
    let methods = schema["methods"].as_array().expect("the methods table");
    assert!(methods.len() >= 40, "{}", methods.len());
    for (name, scope, reducer) in [
        ("session.ping", serde_json::Value::Null, false),
        ("state.get", "observe".into(), false),
        ("agent.register", "agent".into(), true),
        ("op.answer", "operate".into(), true),
        ("token.create", "admin".into(), false),
        ("hooks.claude", "hooks:ingest".into(), false),
    ] {
        let row = methods
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing from /v1/schema.methods"));
        assert_eq!(row["scope"], scope, "{name}");
        assert_eq!(row["reducer"], reducer, "{name}");
    }
    assert!(
        !methods.iter().any(|m| m["name"] == "service.bound"),
        "the internal bind call is never in the table"
    );
    // `hooks.*` is scoped like everything else: this token holds no
    // `hooks:ingest`, so it is refused by name and not by pretending the
    // method is missing.
    let hooks = daemon
        .rpc(&auth, r#"{"id":9,"method":"hooks.claude","params":{}}"#)
        .await;
    assert_eq!(hooks["error"]["code"], ApiError::FORBIDDEN, "{hooks}");
    assert_eq!(hooks["error"]["data"]["need"], "hooks:ingest");
    // A CLI whose adapter is not in this daemon is still -32601, with the
    // release that will answer it. Claude and Codex both have one now, so
    // the one left to name is Gemini's (§5.3, v1.1).
    let ingest = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let later = daemon
        .rpc(&ingest, r#"{"id":9,"method":"hooks.gemini","params":{}}"#)
        .await;
    assert_eq!(
        later["error"]["code"],
        ApiError::METHOD_NOT_FOUND,
        "{later}"
    );
    assert_eq!(later["error"]["data"]["available"], "v1.1");
    // …and the two that do answer, answer: `{}`, from a scoped caller, on
    // the line door both adapters share.
    for method in ["hooks.claude", "hooks.codex"] {
        let body = format!(r#"{{"id":9,"method":"{method}","params":{{}}}}"#);
        let answered = daemon.rpc(&ingest, &body).await;
        assert_eq!(answered["result"], serde_json::json!({}), "{method}");
    }
    daemon.shutdown().await;
}

/// Row 5: the `Origin` guard on every route (`/v1/info` and the WS upgrade
/// included), an allowed origin passing, and the `Host` guard.
#[tokio::test]
async fn foreign_origin_is_rejected() {
    let daemon = Daemon::with(Options {
        allowed_origins: vec!["http://localhost:5173".into()],
        ..Options::default()
    })
    .await;
    let auth = daemon.bearer("gui", &[Scope::Observe]);
    let evil = ("Origin", "http://evil.example");
    let good = ("Origin", "http://localhost:5173");

    for (method, path, body) in [
        ("POST", "/v1/rpc", r#"{"id":1,"method":"session.ping"}"#),
        ("GET", "/v1/info", ""),
        ("GET", "/v1/ws", ""),
        ("GET", "/v1/state", ""),
        ("GET", "/v1/events", ""),
    ] {
        let (status, json) = daemon
            .http(method, path, &[evil, ("Authorization", &auth)], body)
            .await;
        assert_eq!(status, 403, "{path} let a foreign origin in");
        assert_eq!(json["error"], "origin_forbidden", "{path}");
    }

    // An allowed origin passes the layer (the route answers as usual).
    let (status, info) = daemon.http("GET", "/v1/info", &[good], "").await;
    assert_eq!(status, 200);
    assert_eq!(info["api"], "1");
    let (status, body) = daemon
        .http(
            "POST",
            "/v1/rpc",
            &[good, ("Authorization", &auth)],
            r#"{"id":1,"method":"session.ping"}"#,
        )
        .await;
    assert_eq!(status, 200);
    assert!(body["result"]["ts"].is_string());
    let (status, _) = daemon
        .http("GET", "/v1/ws", &[good, ("Authorization", &auth)], "")
        .await;
    assert_ne!(status, 403, "an allowed origin reaches the upgrade");

    // No `Origin` at all is a script, not a browser: allowed.
    let (status, _) = daemon.http("GET", "/v1/info", &[], "").await;
    assert_eq!(status, 200);

    // A `Host` that is not loopback is 400 — before any auth.
    for host in ["evil.example", "evil.example:80", "127.0.0.1.evil.example"] {
        let (status, json) = daemon.http("GET", "/v1/info", &[("Host", host)], "").await;
        assert_eq!(status, 400, "{host}");
        assert_eq!(json["error"], "bad_host", "{host}");
    }
    // A loopback name on some other port is not this door (design §4.1:
    // `Host` must be `127.0.0.1:<port>` or `localhost:<port>`).
    let (status, json) = daemon
        .http("GET", "/v1/info", &[("Host", "localhost:1")], "")
        .await;
    assert_eq!(status, 400, "a loopback name on a foreign port");
    assert_eq!(json["error"], "bad_host");
    let host = format!("localhost:{}", daemon.port);
    let (status, _) = daemon
        .http("GET", "/v1/info", &[("Host", host.as_str())], "")
        .await;
    assert_eq!(status, 200, "the bound port by its loopback name");
    daemon.shutdown().await;
}

/// Row 11: `(token, client.name)` is the logical session — it owns agents
/// across requests, a different name is a different session, `push` caps
/// have nowhere to go, and the session expires `mailbox_ttl_s` after its
/// last call (2 s here), orphaning what it owned.
#[tokio::test]
async fn logical_session_keeps_ownership_and_refuses_push() {
    let daemon = Daemon::with(Options {
        mailbox_ttl_s: 2,
        ..Options::default()
    })
    .await;
    let auth = daemon.bearer("bots", &[Scope::Observe, Scope::Agent]);
    let call = |id: u32, method: &str, params: serde_json::Value| {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string()
    };
    let as_bot = |mut params: serde_json::Value| {
        params["client"] = serde_json::json!({"name": "bot"});
        params
    };

    // Register as `bot`; the owning session is the logical one.
    let registered = daemon
        .rpc(
            &auth,
            &call(
                1,
                "agent.register",
                as_bot(register_params("bot", "http:bot")),
            ),
        )
        .await;
    let agent = registered["result"]["agent"]
        .as_str()
        .unwrap_or_else(|| panic!("{registered}"))
        .to_string();
    let session = registered["result"]["owner"]["session"].clone();
    assert!(session.as_str().unwrap().starts_with("sess_"));
    assert_eq!(registered["result"]["owner"]["principal"], "token:bots");

    // A second request with the same client.name is the same session, so
    // the update it makes is allowed.
    let updated = daemon
        .rpc(
            &auth,
            &call(
                2,
                "agent.update",
                as_bot(serde_json::json!({"agent": agent, "state": "working"})),
            ),
        )
        .await;
    assert!(updated["error"].is_null(), "{updated}");
    // The reply's rev includes its own write. The core publishes the reply's
    // events just after it answers, so wait for the bus to reach that rev
    // before comparing (as src/api/conn.rs does).
    let rev = updated["result"]["rev"].as_u64().expect("the reply's rev");
    daemon
        .until("the update to reach the bus", |snap| snap.rev >= rev)
        .await;
    assert_eq!(daemon.api.bus.rev(), rev);
    let card = daemon
        .rpc(
            &auth,
            &call(
                3,
                "agent.card.set",
                as_bot(card_params(&agent, "still mine?")),
            ),
        )
        .await;
    assert!(card["result"]["card"]["id"].is_number(), "{card}");

    // Another client.name under the same token is another session: it does
    // not own the agent.
    let mut other = register_params("bot", "http:bot");
    other["client"] = serde_json::json!({"name": "other"});
    let conflict = daemon.rpc(&auth, &call(4, "agent.register", other)).await;
    assert_eq!(conflict["error"]["code"], ApiError::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["data"]["owner"]["session"], session);
    assert_eq!(
        conflict["error"]["data"]["owner"]["principal"],
        "token:bots"
    );
    let stolen = daemon
        .rpc(
            &auth,
            &call(
                5,
                "agent.update",
                serde_json::json!({"client": {"name": "other"}, "agent": agent, "state": "idle"}),
            ),
        )
        .await;
    assert_eq!(stolen["error"]["code"], ApiError::CONFLICT);

    // `push` needs a connection to push on: a logical session has none.
    for (caps, field) in [
        (
            serde_json::json!({"answer": "push", "order": "poll", "focus": false}),
            "caps.answer",
        ),
        (
            serde_json::json!({"answer": "poll", "order": "push", "focus": false}),
            "caps.order",
        ),
    ] {
        let mut params = register_params("pusher", "http:pusher");
        params["caps"] = caps;
        let refused = daemon
            .rpc(&auth, &call(6, "agent.register", as_bot(params)))
            .await;
        assert_eq!(refused["error"]["code"], ApiError::INVALID, "{refused}");
        assert_eq!(refused["error"]["data"]["field"], field);
        assert_eq!(
            refused["error"]["data"]["why"],
            "push needs a WS/UDS session"
        );
    }

    // Idle past `mailbox_ttl_s`: the session is closed and the agent orphaned.
    let id = deck_model::AgentId(agent.clone());
    daemon
        .until("the logical session to expire", |snap| {
            snap.agents
                .get(&id)
                .is_some_and(|a| a.state == deck_model::AgentState::Idle)
        })
        .await;
    // A fresh call under the same name is a *new* session, which may claim it.
    let claimed = daemon
        .rpc(
            &auth,
            &call(
                7,
                "agent.claim",
                as_bot(serde_json::json!({"key": "http:bot"})),
            ),
        )
        .await;
    assert_eq!(claimed["result"]["agent"], agent, "{claimed}");
    assert_ne!(claimed["result"]["owner"]["session"], session);
    daemon.shutdown().await;
}
