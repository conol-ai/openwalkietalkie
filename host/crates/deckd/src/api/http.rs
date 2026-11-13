//! The TCP door (design §4.1, §4.14; contract §3.3): axum on
//! `127.0.0.1:<[api] port>`. Every route sits behind the `Host`/`Origin`
//! layer and the 1 MiB body cap; `GET /v1/info` is the only route without
//! a bearer token. `POST /v1/rpc` runs on a logical session `(token,
//! client.name)`, `GET /v1/ws` hands the socket to `conn::run` (one JSON
//! value per text frame), `GET /v1/events` is the SSE observer with the
//! ring replay, and the REST mirror reads the snapshot redacted for the
//! token. The squatter probe and the `lsof` listener lookup of the
//! port-in-use degrade (§4.1 D8) are here too; `serve` drives them.
use super::conn::{self, ConnError, Door, LineSink, LineStream, Peer, LINE_MAX};
use super::dispatch::{self, dispatch, ConnCtx};
use super::hooks;
use super::rpc::{self, Incoming, RpcRequest, RpcResponse};
use super::session::{self, ClientInfo};
use super::Api;
use crate::bus::{matches_any, Published};
use crate::core::handle::{CoreHandle, OpenSession, SessionKind};
use crate::trust::PeerVerdict;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use deck_model::{redact_event, redact_for, AgentId, ApiError, Principal, Scope, SessionId};
use futures_util::{SinkExt, StreamExt};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};

/// Request bodies above this are refused (413 / `-32012`).
pub const BODY_MAX: usize = 1024 * 1024;
/// A logical session without a `client.name` (§4.2).
pub const DEFAULT_CLIENT: &str = "default";
/// SSE comment cadence (§4.14).
pub const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
/// The squatter probe gives the listener this long to answer `/v1/info`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// A degraded TCP door retries the bind this often (§4.1 D8).
pub const RETRY_EVERY: Duration = Duration::from_secs(30);
/// `service.degraded` reasons of the TCP door (§4.1 D8).
pub const DEGRADED_PORT_IN_USE: &str = "port_in_use";
pub const DEGRADED_HIJACK_RISK: &str = "hooks_hijack_risk";
/// A probe reply is read up to this many bytes.
const PROBE_READ_MAX: usize = 64 * 1024;
/// The `client.name` of an SSE observer session.
const SSE_CLIENT: &str = "sse";

/// What every handler gets: the `Api` and the door's stop signal.
#[derive(Clone)]
pub struct HttpState {
    pub api: Arc<Api>,
    pub stop: watch::Receiver<bool>,
}

/// Bind `127.0.0.1:port` (`0` → ephemeral).
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await
}

/// The port this daemon holds, as the core recorded it. `None` while no
/// door is bound — `[api] tcp = false`, or an ephemeral port whose bind is
/// not reported yet — and while the port is somebody else's: the bind
/// failed (`port_in_use`) or a stranger answers on it (`hooks_hijack_risk`,
/// §4.1 D8). `service.info`, `/v1/info` and the `Host` check all ask this,
/// so none of them names a port that sends a client to a squatter or to
/// nobody.
pub fn held_port(service: &deck_model::ServiceState) -> Option<u16> {
    if matches!(
        service.degraded.as_deref(),
        Some(DEGRADED_PORT_IN_USE | DEGRADED_HIJACK_RISK)
    ) {
        return None;
    }
    (service.port != 0).then_some(service.port)
}

/// The `/v1/info` object (§4.1).
pub fn info(api: &Api) -> serde_json::Value {
    let service = api.bus.snapshot().service.clone();
    serde_json::json!({
        "api": "1",
        "daemon": crate::core::dispatch::VERSION,
        "platform": crate::core::dispatch::platform(),
        "capabilities": crate::core::dispatch::CAPABILITIES,
        "uds": api.paths.socket,
        "port": held_port(&service),
        "mcp": serde_json::Value::Null,
        "degraded": service.degraded,
    })
}

/// Whether a `/v1/info` reply came from this daemon: `api:"1"`, our
/// version as `daemon`, our socket as `uds` (§4.1: anything else on our
/// port is a squatter that receives hook payloads → `hooks_hijack_risk`).
pub fn is_ours(reply: &serde_json::Value, api: &Api) -> bool {
    reply["api"] == "1"
        && reply["daemon"] == crate::core::dispatch::VERSION
        && reply["uds"] == serde_json::json!(api.paths.socket)
}

pub fn router(api: Arc<Api>, stop: watch::Receiver<bool>) -> Router {
    let state = HttpState { api, stop };
    Router::new()
        .route("/v1/info", get(info_handler))
        .route("/v1/schema", get(schema_handler))
        .route("/v1/rpc", post(rpc_handler))
        .route("/v1/ws", get(ws_handler))
        .route("/v1/events", get(events_handler))
        .route("/v1/state", get(state_handler))
        .route("/v1/agents", get(agents_handler))
        .route("/v1/agents/{id}", get(agent_handler))
        .route("/v1/cards", get(cards_handler))
        .route("/v1/device", get(device_handler))
        .route("/v1/voice", get(voice_handler))
        // The hook door (design §4.11): what a CLI's own `type:"http"`
        // hooks post to. Same bearer, same `Host`/`Origin` guard and same
        // 1 MiB cap as `/v1/rpc`; the body is the event JSON itself.
        .route("/v1/hooks/{cli}", post(hooks_handler))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(BODY_MAX))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            host_and_origin,
        ))
        .with_state(state)
}

fn error_json(status: StatusCode, error: &str) -> Response {
    (status, Json(serde_json::json!({"error": error}))).into_response()
}

async fn not_found() -> Response {
    error_json(StatusCode::NOT_FOUND, "not_found")
}

/// `Host` must be `127.0.0.1:<port>` or `localhost:<port>` (400
/// `bad_host`); an `Origin` present and not allowed is 403
/// `origin_forbidden` — on every route, the WS upgrade included.
async fn host_and_origin(State(state): State<HttpState>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        if !host_ok(host, bound_port(&state.api)) {
            return error_json(StatusCode::BAD_REQUEST, "bad_host");
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        let allowed = state.api.config.load().api.allowed_origins.clone();
        if !allowed
            .iter()
            .any(|a| a.eq_ignore_ascii_case(origin.trim()))
        {
            return error_json(StatusCode::FORBIDDEN, "origin_forbidden");
        }
    }
    next.run(req).await
}

/// The port this door is bound to, as the core knows it. `None` when the
/// bind has not been reported yet or the door is degraded — the port on
/// the wire is then not ours to compare a `Host` against.
fn bound_port(api: &Api) -> Option<u16> {
    held_port(&api.bus.snapshot().service)
}

/// `127.0.0.1` or `localhost`, and — when the `Host` carries a port —
/// **our** port (§3.3: `127.0.0.1:<port>` or `localhost:<port>`). A
/// `Host` without a port names no wrong port, so it passes; anything
/// else, including a loopback name on some other port, is `bad_host`.
pub fn host_ok(host: &str, bound: Option<u16>) -> bool {
    let host = host.trim();
    let (name, port) = match host.rsplit_once(':') {
        Some((name, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            // All digits: it is a port, and one we cannot parse is not ours.
            let Ok(port) = digits.parse::<u16>() else {
                return false;
            };
            (name, Some(port))
        }
        _ => (host, None),
    };
    if !(name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")) {
        return false;
    }
    match (port, bound) {
        (Some(port), Some(bound)) => port == bound,
        _ => true,
    }
}

/// A verified bearer: the token's principal, scopes and name.
struct Auth {
    principal: Principal,
    scopes: Vec<Scope>,
    token: String,
}

/// `Authorization: Bearer dk_…` against `tokens.toml`; `None` when absent
/// or invalid (expired included). Tokens are never read from the URL.
fn authenticate(api: &Api, headers: &HeaderMap) -> Option<Auth> {
    let plaintext = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(session::parse_bearer)?;
    let (principal, scopes) =
        session::token_principal(&api.tokens.load(), plaintext, crate::core::wall_now())?;
    let token = match &principal {
        Principal::Token(name) => name.clone(),
        other => other.to_string(),
    };
    Some(Auth {
        principal,
        scopes,
        token,
    })
}

fn unauthorized() -> Response {
    error_json(StatusCode::UNAUTHORIZED, "unauthorized")
}

/// The logical session `(token, client)` of a stateless caller (§4.2).
async fn logical(api: &Api, auth: &Auth, client: &str) -> Result<SessionId, ApiError> {
    api.core
        .logical_session(
            auth.token.clone(),
            client.to_string(),
            auth.principal.clone(),
            auth.scopes.clone(),
        )
        .await
}

async fn info_handler(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(info(&state.api))
}

/// `deck_model::schema::schema()` plus the dispatch table (`methods`),
/// behind a token like everything but `/v1/info`.
async fn schema_handler(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if authenticate(&state.api, &headers).is_none() {
        return unauthorized();
    }
    let mut schema = deck_model::schema::schema();
    schema["methods"] = serde_json::Value::Array(dispatch::methods_json());
    Json(schema).into_response()
}

/// A JSON-RPC error as an HTTP response (`id: null`).
fn rpc_error(status: StatusCode, error: ApiError) -> Response {
    (
        status,
        Json(RpcResponse::err(serde_json::Value::Null, error)),
    )
        .into_response()
}

async fn rpc_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let api = &state.api;
    let body = match body {
        Ok(body) => body,
        // The cap is enforced while the body streams in: 413 with the
        // JSON-RPC `-32012 too_large` body, as on the line doors.
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return rpc_error(StatusCode::PAYLOAD_TOO_LARGE, ApiError::too_large(BODY_MAX))
        }
        Err(e) => return rpc_error(StatusCode::BAD_REQUEST, ApiError::parse(&e.to_string())),
    };
    let Some(auth) = authenticate(api, &headers) else {
        return rpc_error(StatusCode::UNAUTHORIZED, ApiError::unauthorized());
    };
    let text = match std::str::from_utf8(&body) {
        Ok(text) => text,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, ApiError::parse("not utf-8")),
    };
    let incoming = match rpc::parse(text) {
        Ok(incoming) => incoming,
        Err(e) => return rpc_error(StatusCode::OK, e.into()),
    };
    // JSON-RPC 2.0: a malformed batch member is answered in place with
    // its own `-32600` and `id:null`; the members beside it still run.
    let (requests, malformed, batch) = match incoming {
        Incoming::Single(req) => (vec![req], Vec::new(), false),
        Incoming::Batch(members) => {
            let mut requests = Vec::new();
            let mut malformed = Vec::new();
            for member in members {
                match member {
                    Ok(req) => requests.push(req),
                    Err(e) => malformed.push(RpcResponse::err(serde_json::Value::Null, e)),
                }
            }
            (requests, malformed, true)
        }
    };
    // The logical session: `client.name` from a hello in the batch, else
    // from any call's params, else "default".
    let client = requests
        .iter()
        .find(|r| r.method == "session.hello")
        .and_then(RpcRequest::client_name)
        .or_else(|| requests.iter().find_map(RpcRequest::client_name))
        .unwrap_or(DEFAULT_CLIENT)
        .to_string();
    // A `session.hello` in the body may narrow the token's scopes for the
    // calls beside it; asking past them is refused by name (§3.4: never a
    // silent intersection), as it is on the line doors.
    let scopes = match requested_scopes(&requests, &auth.scopes) {
        Ok(scopes) => scopes,
        Err(e) => return refuse_all(requests, malformed, batch, e),
    };
    let session_id = match logical(api, &auth, &client).await {
        Ok(id) => id,
        Err(e) => return rpc_error(StatusCode::SERVICE_UNAVAILABLE, e),
    };
    let ctx = ConnCtx {
        core: api.core.clone(),
        principal: auth.principal,
        session: session_id,
        scopes,
        trusted: false,
        adapters: api.adapters.clone(),
        client: ClientInfo {
            name: client,
            version: String::new(),
        },
        content: wants_content(&requests),
    };
    let answers =
        futures_util::future::join_all(requests.into_iter().map(|req| answer_http(api, &ctx, req)))
            .await;
    let responses: Vec<RpcResponse> = malformed
        .into_iter()
        .chain(answers.into_iter().flatten())
        .collect();
    rpc_body(responses, batch)
}

/// `POST /v1/hooks/{claude|codex}` — the TCP half of the hook door
/// (design §4.11, contract §1.2). Claude posts the event JSON as the body
/// with its bearer in `Authorization`, so there is no JSON-RPC envelope
/// and no `client.name`: the scope is the token's, and the adapter runs on
/// its own internal session rather than a logical one.
///
/// Every path out of here that is not a decision is `200 {}`. A hook must
/// never wait on a bug of ours, and a body this daemon cannot read is our
/// bug, not the operator's prompt's.
async fn hooks_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(cli): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let api = &state.api;
    let body = match body {
        Ok(body) => body,
        // The cap is enforced while the body streams in, as on `/v1/rpc`.
        // A non-2xx is a non-blocking error for Claude — it shows its own
        // dialog — so this fails open too, loudly rather than silently.
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return rpc_error(StatusCode::PAYLOAD_TOO_LARGE, ApiError::too_large(BODY_MAX))
        }
        Err(e) => return rpc_error(StatusCode::BAD_REQUEST, ApiError::parse(&e.to_string())),
    };
    let Some(auth) = authenticate(api, &headers) else {
        return rpc_error(StatusCode::UNAUTHORIZED, ApiError::unauthorized());
    };
    let method = format!("hooks.{cli}");
    let Some(spec) = dispatch::method(&method) else {
        return not_found().await;
    };
    if let Err(e) = dispatch::check_scope(&auth.scopes, spec) {
        return rpc_error(StatusCode::FORBIDDEN, e);
    }
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            // Never the body itself: a hook payload is the most sensitive
            // data the daemon holds (§11).
            tracing::warn!(cli, bytes = body.len(), "a hook body that is not json");
            return Json(serde_json::json!({})).into_response();
        }
    };
    let reply = hooks::ingest(
        &api.adapters,
        &api.core,
        &cli,
        serde_json::json!({"payload": payload}),
    )
    .await;
    if let Some(delivery) = reply.after_write {
        // Best effort, exactly as on `/v1/rpc` (documented there): the
        // body is handed to the server here and written behind us, so
        // `ok:true` is the most this door can honestly report. The line
        // doors, which own their writer, report the failures too.
        api.core
            .delivered(delivery.agent, delivery.card, true)
            .await;
    }
    match reply.result {
        Ok(value) => Json(value).into_response(),
        // The one refusal `ingest` can answer with: a CLI whose adapter is
        // not in this daemon.
        Err(e) => rpc_error(StatusCode::NOT_FOUND, e),
    }
}

/// What an answered `POST /v1/rpc` writes: the array for a batch, the one
/// response for a single request — and **nothing at all** when there is
/// no response to write. JSON-RPC 2.0 returns nothing for a body of
/// notifications alone (an empty array is not an answer), which on this
/// door is `204 No Content`.
fn rpc_body(mut responses: Vec<RpcResponse>, batch: bool) -> Response {
    if batch && !responses.is_empty() {
        return Json(responses).into_response();
    }
    match responses.pop() {
        Some(one) => Json(one).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// The scopes this body runs with: the `scopes` of a `session.hello` in it
/// resolved against the token's own list (the HTTP maximum, §3.4), or that
/// list when no hello names any.
fn requested_scopes(requests: &[RpcRequest], maximum: &[Scope]) -> Result<Vec<Scope>, ApiError> {
    let Some(asked) = requests
        .iter()
        .find(|r| r.method == "session.hello")
        .and_then(|r| r.params.get("scopes"))
    else {
        return Ok(maximum.to_vec());
    };
    let names: Vec<String> = serde_json::from_value(asked.clone())
        .map_err(|e| ApiError::invalid_params(&e.to_string()))?;
    let requested = session::parse_scopes(&names)?;
    // A token is never ancestry-capped: the cap is a UDS peer verdict.
    session::resolve(&requested, maximum, false)
}

/// Whether this body's calls may be shown content: not when a
/// `session.hello` in it says `"content": false`, which answers them as for
/// a principal without content, as the line doors do.
fn wants_content(requests: &[RpcRequest]) -> bool {
    requests
        .iter()
        .find(|r| r.method == "session.hello")
        .and_then(|r| r.params.get("content"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// Every answerable request in the body refused with the same error: the
/// hello could not be established, so nothing beside it may run either.
/// The malformed members keep their own `-32600`.
fn refuse_all(
    requests: Vec<RpcRequest>,
    malformed: Vec<RpcResponse>,
    batch: bool,
    error: ApiError,
) -> Response {
    let responses: Vec<RpcResponse> = malformed
        .into_iter()
        .chain(
            requests
                .into_iter()
                .filter_map(|r| r.id.map(|id| RpcResponse::err(id, error.clone()))),
        )
        .collect();
    rpc_body(responses, batch)
}

/// The `caps` field an `agent.register` sets to `push`, if any: a logical
/// session has no connection to push on (§4.2 → `-32009`).
pub fn push_cap(params: &serde_json::Value) -> Option<&'static str> {
    let caps = params.get("caps")?;
    if caps.get("answer").and_then(serde_json::Value::as_str) == Some("push") {
        return Some("caps.answer");
    }
    if caps.get("order").and_then(serde_json::Value::as_str) == Some("push") {
        return Some("caps.order");
    }
    None
}

/// One request on a logical session; `session.hello` answers here.
async fn answer_http(api: &Api, ctx: &ConnCtx, req: RpcRequest) -> Option<RpcResponse> {
    let id = req.id.clone();
    // §4.8 `poll`: the mailbox is emptied only for a call whose response
    // is written, and a held hook's product *is* that response. A
    // notification is never answered, so neither one ever runs.
    if id.is_none() && dispatch::answers_on_write(&req.method) {
        return None;
    }
    let result = if req.method == "session.hello" {
        let snapshot = api.bus.snapshot();
        let mut reply = serde_json::json!({
            "api": "1",
            "daemon": crate::core::dispatch::VERSION,
            "principal": {"id": ctx.principal.to_string(), "scopes": ctx.scopes, "trusted": false},
            "session": ctx.session,
            "rev": snapshot.rev,
        });
        if !ctx.content {
            reply["content"] = false.into();
        }
        // §4.5: the reply carries the snapshot when the hello asks for it,
        // redacted for this caller exactly as on the line doors.
        if req
            .params
            .get("snapshot")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            reply["snapshot"] =
                serde_json::to_value(redact_for(&snapshot, &ctx.seen_as(), &BTreeSet::new()))
                    .unwrap_or(serde_json::Value::Null);
        }
        Ok(reply)
    } else if let Some(field) = push_refusal(ctx, &req) {
        Err(ApiError::invalid(field, "push needs a WS/UDS session"))
    } else {
        let reply = dispatch(ctx, &req.method, req.params).await;
        if let Some(delivery) = reply.after_write {
            // Best effort (documented): the response is produced, not yet
            // read. A notification produces none, so it is never `true`.
            api.core
                .delivered(delivery.agent, delivery.card, id.is_some())
                .await;
        }
        reply.result
    };
    Some(RpcResponse::from_result(id?, result))
}

/// `agent.register` with a `push` cap from a caller that may register at
/// all: the field to refuse. A caller without `agent` gets its `-32002`
/// from the dispatch instead.
fn push_refusal(ctx: &ConnCtx, req: &RpcRequest) -> Option<&'static str> {
    if req.method != "agent.register" {
        return None;
    }
    let field = push_cap(&req.params)?;
    let spec = dispatch::method(&req.method)?;
    dispatch::check_scope(&ctx.scopes, spec).ok()?;
    Some(field)
}

// ---- GET /v1/ws ------------------------------------------------------

/// The bearer names the principal now; without one the hello must carry
/// `token` (`conn::resolve_hello`). An invalid bearer is 401 before the
/// upgrade; the `Origin` layer already ran.
async fn ws_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let has_bearer = headers.get(header::AUTHORIZATION).is_some();
    let principal = match authenticate(&state.api, &headers) {
        Some(auth) => Some((auth.principal, auth.scopes)),
        None if has_bearer => return unauthorized(),
        None => None,
    };
    let api = state.api.clone();
    let stop = state.stop.clone();
    // tungstenite's own cap sits above ours so that an oversized frame is
    // refused by `ws_lines` on a healthy socket (the `-32012` is written
    // before the close); anything past four times the line cap is cut by
    // tungstenite and mapped to the same refusal.
    ws.max_message_size(LINE_MAX * 4)
        .max_frame_size(LINE_MAX * 4)
        .on_upgrade(move |socket| async move {
            let (sink, stream) = socket.split();
            conn::run(
                api,
                Peer {
                    door: Door::Ws,
                    principal,
                    verdict: PeerVerdict::default(),
                },
                ws_lines(stream),
                ws_sink(sink),
                stop,
            )
            .await;
        })
}

/// Text frames as lines: a frame past `LINE_MAX` (ours or tungstenite's
/// refusal) is `TooLarge`, a close frame ends the session, pings and pongs
/// are the socket's own business.
fn ws_lines<S>(stream: S) -> LineStream
where
    S: futures_util::Stream<Item = Result<Message, axum::Error>> + Send + 'static,
{
    Box::pin(stream.filter_map(|item| async move {
        match item {
            Ok(Message::Text(text)) if text.len() > LINE_MAX => Some(Err(ConnError::TooLarge)),
            Ok(Message::Text(text)) => Some(Ok(text.as_str().to_string())),
            Ok(Message::Binary(bytes)) if bytes.len() > LINE_MAX => Some(Err(ConnError::TooLarge)),
            Ok(Message::Binary(bytes)) => Some(match String::from_utf8(bytes.to_vec()) {
                Ok(line) => Ok(line),
                Err(_) => Err(ConnError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not utf-8",
                ))),
            }),
            Ok(Message::Close(_)) => Some(Err(ConnError::Closed)),
            Ok(Message::Ping(_) | Message::Pong(_)) => None,
            Err(e) => {
                let why = e.to_string();
                Some(Err(if why.to_ascii_lowercase().contains("too long") {
                    ConnError::TooLarge
                } else {
                    ConnError::Io(std::io::Error::other(why))
                }))
            }
        }
    }))
}

/// Lines as text frames, one per frame, flushed.
fn ws_sink<S>(sink: S) -> LineSink
where
    S: futures_util::Sink<Message, Error = axum::Error> + Send + 'static,
{
    Box::pin(
        sink.sink_map_err(|e| ConnError::Io(std::io::Error::other(e.to_string())))
            .with(|line: String| async move { Ok::<_, ConnError>(Message::Text(line.into())) }),
    )
}

// ---- GET /v1/events --------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
struct EventsQuery {
    since: Option<u64>,
    types: Option<String>,
}

/// Closes the observer session when the SSE stream is dropped.
struct SessionGuard {
    core: CoreHandle,
    session: SessionId,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.core.close_session(self.session);
    }
}

struct SseState {
    api: Arc<Api>,
    scopes: Vec<Scope>,
    types: Vec<String>,
    rx: broadcast::Receiver<Published>,
    pending: VecDeque<SseEvent>,
    stop: watch::Receiver<bool>,
    _guard: SessionGuard,
}

fn sse_event(event: &deck_model::Event) -> SseEvent {
    SseEvent::default()
        .id(event.rev.to_string())
        .event("event")
        .data(serde_json::to_string(event).unwrap_or_else(|_| "null".into()))
}

fn sse_resync(rev: u64) -> SseEvent {
    SseEvent::default()
        .event("resync")
        .data(serde_json::json!({"rev": rev}).to_string())
}

/// The event globs of `?types=a,b` (`*` when absent).
fn types_of(spec: Option<&str>) -> Vec<String> {
    let types: Vec<String> = spec
        .unwrap_or("*")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    if types.is_empty() {
        vec!["*".into()]
    } else {
        types
    }
}

/// SSE: an `Observer` session with the token's scopes; the ring replayed
/// from `since` (`id:` = rev, `event: event`), then live, filtered by
/// `types`, redacted; a lagged reader gets `event: resync`; a comment
/// every 15 s keeps the connection alive.
async fn events_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    let api = state.api.clone();
    let Some(auth) = authenticate(&api, &headers) else {
        return unauthorized();
    };
    let types = types_of(query.types.as_deref());
    // Subscribe before the replay so nothing falls between the two.
    let rx = api.bus.subscribe();
    let session = match api
        .core
        .open_session(OpenSession {
            kind: SessionKind::Observer,
            principal: auth.principal.clone(),
            scopes: auth.scopes.clone(),
            trusted: false,
            client: ClientInfo {
                name: SSE_CLIENT.into(),
                version: String::new(),
            },
        })
        .await
    {
        Ok(id) => id,
        Err(e) => return error_json(StatusCode::SERVICE_UNAVAILABLE, &e.message),
    };
    let guard = SessionGuard {
        core: api.core.clone(),
        session,
    };
    let mut pending = VecDeque::new();
    if let Some(since) = query.since {
        let (events, gap) = api.bus.replay(since, usize::MAX, Some(&types));
        if gap {
            pending.push_back(sse_resync(api.bus.rev()));
        } else {
            pending.extend(
                events
                    .iter()
                    .filter_map(|e| redact_event(e, &auth.scopes, false))
                    .map(|e| sse_event(&e)),
            );
        }
    }
    tracing::debug!(principal = %auth.principal, %session, ?types, since = ?query.since, "sse");
    let state = SseState {
        api,
        scopes: auth.scopes,
        types,
        rx,
        pending,
        stop: state.stop,
        _guard: guard,
    };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(event) = st.pending.pop_front() {
                return Some((Ok::<_, std::convert::Infallible>(event), st));
            }
            tokio::select! {
                _ = st.stop.changed() => return None,
                published = st.rx.recv() => match published {
                    Ok(p) => {
                        if !matches_any(&st.types, p.event.kind) {
                            continue;
                        }
                        // An observer owns no agent: nothing is ever "own".
                        if let Some(event) = redact_event(&p.event, &st.scopes, false) {
                            st.pending.push_back(sse_event(&event));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        st.rx = st.rx.resubscribe();
                        st.pending.push_back(sse_resync(st.api.bus.rev()));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keep-alive"))
        .into_response()
}

// ---- The REST mirror -------------------------------------------------

/// The token's redacted view of the snapshot: `(token, ?client)` is the
/// logical session whose own agents keep their content.
// The `Err` is the HTTP response to send, built once per refusal; boxing it
// would only move the same bytes.
#[allow(clippy::result_large_err)]
async fn view(
    api: &Api,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<deck_model::Snapshot, Response> {
    let auth = authenticate(api, headers).ok_or_else(unauthorized)?;
    let client = query
        .get("client")
        .map(String::as_str)
        .filter(|c| !c.is_empty())
        .unwrap_or(DEFAULT_CLIENT);
    let session = logical(api, &auth, client)
        .await
        .map_err(|e| error_json(StatusCode::SERVICE_UNAVAILABLE, &e.message))?;
    let snapshot = api.bus.snapshot();
    let own: BTreeSet<AgentId> = snapshot
        .agents
        .values()
        .filter(|a| a.owner.session == session)
        .map(|a| a.id.clone())
        .collect();
    Ok(redact_for(&snapshot, &auth.scopes, &own))
}

type RestQuery = Query<HashMap<String, String>>;

async fn state_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => {
            Json(serde_json::json!({"rev": snapshot.rev, "snapshot": snapshot})).into_response()
        }
        Err(response) => response,
    }
}

async fn agents_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => Json(snapshot.agents.values().collect::<Vec<_>>()).into_response(),
        Err(response) => response,
    }
}

async fn agent_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => match snapshot.agents.get(&AgentId(id)) {
            Some(agent) => Json(agent).into_response(),
            None => not_found().await,
        },
        Err(response) => response,
    }
}

async fn cards_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => {
            let cards: Vec<serde_json::Value> = snapshot
                .agents
                .values()
                .filter_map(|a| {
                    a.card
                        .as_ref()
                        .map(|card| serde_json::json!({"agent": a.id, "card": card}))
                })
                .collect();
            Json(cards).into_response()
        }
        Err(response) => response,
    }
}

async fn device_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => Json(snapshot.device).into_response(),
        Err(response) => response,
    }
}

async fn voice_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): RestQuery,
) -> Response {
    match view(&state.api, &headers, &query).await {
        Ok(snapshot) => Json(snapshot.voice).into_response(),
        Err(response) => response,
    }
}

// ---- The port-in-use degrade (§4.1 D8) --------------------------------

/// `GET /v1/info` on the port with a hand-written HTTP/1.1 request (no
/// `reqwest`): `None` when nothing answered within `timeout`, else the
/// reply's JSON body (`Null` when the reply was not JSON at all — still a
/// reply, so still a squatter).
pub async fn probe_info(port: u16, timeout: Duration) -> Option<serde_json::Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let exchange = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let request = format!(
            "GET /v1/info HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut raw = Vec::new();
        let mut limited = stream.take(PROBE_READ_MAX as u64);
        let _ = limited.read_to_end(&mut raw).await;
        if raw.is_empty() {
            return None;
        }
        let text = String::from_utf8_lossy(&raw);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        Some(parse_probe_body(body))
    };
    tokio::time::timeout(timeout, exchange).await.ok().flatten()
}

/// The JSON of a reply body, tolerating a chunked encoding (each chunk's
/// hex size line is dropped); anything else is `Null`.
fn parse_probe_body(body: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str(body.trim()) {
        return value;
    }
    let joined: String = body
        .split("\r\n")
        .filter(|line| !line.is_empty() && !line.chars().all(|c| c.is_ascii_hexdigit() || c == ';'))
        .collect();
    serde_json::from_str(joined.trim()).unwrap_or(serde_json::Value::Null)
}

/// Tell the core what the TCP door bound (an internal `Call`; the method
/// is not in the public table). `listener` rides along for the log and for
/// whoever extends `Command::ServiceBound`.
async fn report_bind(
    api: &Api,
    port: Option<u16>,
    degraded: Option<&str>,
    listener: Option<serde_json::Value>,
) {
    // Port 0 is no door at all (`tcp_off`): the core records it, the
    // file names no port.
    super::write_port_file(&api.paths.port, port.filter(|&port| port != 0));
    let reply = api
        .core
        .call(
            Api::internal_ctx(),
            "service.bound",
            serde_json::json!({
                "port": port,
                "degraded": degraded,
                "listener": listener,
            }),
        )
        .await;
    if let Err(e) = reply.result {
        tracing::warn!(error = %e, "the core did not record the bind");
    }
}

/// Serve the router on `listener` until `stop` flips.
async fn serve_router(api: Arc<Api>, listener: TcpListener, mut stop: watch::Receiver<bool>) {
    let router = router(api, stop.clone());
    let served = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = stop.changed().await;
    });
    if let Err(e) = served.await {
        tracing::warn!(error = %e, "tcp door ended");
    }
}

/// The TCP door. On a clean bind: the port and the task serving it. On a
/// bind failure (§4.1 D8): `None`, `degraded = "port_in_use"` with the
/// `lsof` listener, and a task that probes `GET /v1/info` on the port
/// (a reply that is not ours → `hooks_hijack_risk`) and retries the bind
/// every `RETRY_EVERY` until it succeeds or the door stops.
pub async fn tcp_door(
    api: Arc<Api>,
    stop: watch::Receiver<bool>,
) -> (Option<u16>, tokio::task::JoinHandle<()>) {
    let configured = api.config.load().api.port;
    match bind(configured).await {
        Ok(listener) => {
            let port = listener
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(configured);
            tracing::info!(port, "tcp door");
            report_bind(&api, Some(port), None, None).await;
            (Some(port), tokio::spawn(serve_router(api, listener, stop)))
        }
        Err(e) => {
            tracing::error!(error = %e, port = configured, "port in use: tcp door degraded");
            let listener = listener_of(configured).await;
            if let Some(who) = &listener {
                tracing::warn!(port = configured, listener = %who, "the port belongs to someone else");
            }
            report_bind(&api, None, Some(DEGRADED_PORT_IN_USE), listener).await;
            (None, tokio::spawn(retry_bind(api, configured, stop)))
        }
    }
}

/// No TCP door (off: the default, or `[api] tcp = false`; D3 and the
/// owner's decision 12): nothing is bound, so nothing can collide and no
/// degrade is raised. The core records port 0, which [`held_port`] reads as
/// none, and the port file is emptied, so a port an earlier run left in it
/// never sends a client to whatever holds it now.
pub async fn tcp_off(api: &Api) {
    tracing::info!("no tcp door: the socket is the only door");
    report_bind(api, Some(0), None, None).await;
}

/// Probe the squatter, then retry the bind every `RETRY_EVERY`; a late
/// bind clears the degradation and serves from there on.
async fn retry_bind(api: Arc<Api>, port: u16, mut stop: watch::Receiver<bool>) {
    let mut degraded = DEGRADED_PORT_IN_USE;
    loop {
        // Who is on our port? A reply to `/v1/info` that is not ours means
        // the hooks a client sends there reach a stranger.
        if let Some(reply) = probe_info(port, PROBE_TIMEOUT).await {
            if !is_ours(&reply, &api) && degraded != DEGRADED_HIJACK_RISK {
                tracing::error!(
                    port,
                    "something else answers /v1/info on our port: hooks may reach it"
                );
                degraded = DEGRADED_HIJACK_RISK;
                report_bind(&api, None, Some(degraded), listener_of(port).await).await;
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(RETRY_EVERY) => {}
        }
        match bind(port).await {
            Ok(listener) => {
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
                tracing::info!(port = bound, "tcp door bound after all");
                report_bind(&api, Some(bound), None, None).await;
                serve_router(api, listener, stop).await;
                return;
            }
            Err(e) => tracing::debug!(error = %e, port, "the port is still taken"),
        }
    }
}

/// Who listens on the port: `{pid, command}` from `lsof -nP -iTCP:<port>
/// -sTCP:LISTEN` (best effort, `None` when `lsof` is absent or silent).
pub async fn listener_of(port: u16) -> Option<serde_json::Value> {
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"])
            .output()
    })
    .await
    .ok()?
    .ok()?;
    parse_lsof(&String::from_utf8_lossy(&output.stdout))
}

/// `-Fpc` output: `p<pid>` then `c<command>` per process.
pub fn parse_lsof(text: &str) -> Option<serde_json::Value> {
    let mut pid: Option<u64> = None;
    let mut command: Option<String> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            if pid.is_none() {
                pid = p.trim().parse().ok();
            }
        } else if let Some(c) = line.strip_prefix('c') {
            if command.is_none() {
                command = Some(c.trim().to_string());
            }
        }
        if pid.is_some() && command.is_some() {
            break;
        }
    }
    pid.map(|pid| serde_json::json!({"pid": pid, "command": command}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::testkit::test_api;
    use crate::core::handle::{ClientInfo, OpenSession, SessionKind};
    use crate::trust::FakeVerifier;
    use deck_model::{Principal, Request, Scope};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    /// A minimal HTTP/1.1 client: one request, the status and the body.
    pub async fn http(
        port: u16,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, serde_json::Value) {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut req = format!("{method} {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            stream.read_to_end(&mut raw),
        )
        .await
        .expect("a response within 3 s")
        .unwrap();
        let text = String::from_utf8_lossy(&raw);
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        let json = if body.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(body.trim()).unwrap_or_else(|_| serde_json::json!({"raw": body}))
        };
        (status, json)
    }

    #[tokio::test]
    async fn info_answers_without_a_token_and_rpc_without_one_is_401() {
        let daemon = test_api("http-info", FakeVerifier::default());
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.expect("bound on an ephemeral port");
        assert_ne!(port, 0);
        assert_eq!(
            std::fs::read_to_string(&daemon.api.paths.port)
                .unwrap()
                .trim(),
            port.to_string()
        );
        let (status, info) = http(port, "GET", "/v1/info", &[], "").await;
        assert_eq!(status, 200);
        assert_eq!(info["api"], "1");
        assert_eq!(info["port"], port);
        assert_eq!(info["degraded"], serde_json::Value::Null);
        assert!(info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "virtual_agents"));
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[],
            r#"{"id":1,"method":"session.ping"}"#,
        )
        .await;
        assert_eq!(status, 401);
        assert_eq!(body["error"]["code"], ApiError::UNAUTHORIZED);
        assert_eq!(body["id"], serde_json::Value::Null);
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", "Bearer dk_nope")],
            r#"{"id":1,"method":"session.ping"}"#,
        )
        .await;
        assert_eq!(status, 401);
        assert_eq!(body["error"]["code"], ApiError::UNAUTHORIZED);
        // Host and Origin layers, on /v1/info too.
        let (status, body) =
            http(port, "GET", "/v1/info", &[("Host", "evil.example:80")], "").await;
        assert_eq!(status, 400);
        assert_eq!(body["error"], "bad_host");
        let (status, body) = http(
            port,
            "GET",
            "/v1/info",
            &[("Origin", "http://evil.example")],
            "",
        )
        .await;
        assert_eq!(status, 403);
        assert_eq!(body["error"], "origin_forbidden");
        let (status, _) = http(port, "GET", "/v1/nope", &[], "").await;
        assert_eq!(status, 404);
        // /v1/schema, the REST mirror, SSE and WS all need a token.
        for path in [
            "/v1/schema",
            "/v1/state",
            "/v1/agents",
            "/v1/agents/ag_1",
            "/v1/cards",
            "/v1/device",
            "/v1/voice",
            "/v1/events",
        ] {
            let (status, body) = http(port, "GET", path, &[], "").await;
            assert_eq!(status, 401, "{path}");
            assert_eq!(body["error"], "unauthorized", "{path}");
        }
        let plain = daemon.token("t", &[Scope::Observe]);
        let auth = format!("Bearer {plain}");
        let (status, schema) =
            http(port, "GET", "/v1/schema", &[("Authorization", &auth)], "").await;
        assert_eq!(status, 200);
        assert!(schema["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "session.ping"));
        assert!(schema["request"].is_object());
        // `docs/deckd-api.md` is generated from this object, so the
        // generator's input has to *be* this object and not a second copy
        // assembled the same way by accident. `tests/api_doc.rs` compares
        // the checked-in file with `apidoc::render` of it; this is the
        // other half — that what the endpoint serves is what that render
        // was given.
        assert_eq!(
            schema,
            crate::apidoc::schema(),
            "/v1/schema is the API doc's source"
        );
        served.shutdown().await;
        assert_eq!(std::fs::read_to_string(&daemon.api.paths.port).unwrap(), "");
    }

    #[tokio::test]
    async fn a_bearer_token_gets_a_logical_session_for_ping_state_get_and_a_batch() {
        let daemon = test_api("http-token", FakeVerifier::default());
        let plain = daemon.token("gate", &[Scope::Observe, Scope::Agent]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let auth = format!("Bearer {plain}");
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            r#"{"jsonrpc":"2.0","id":1,"method":"session.ping","params":{}}"#,
        )
        .await;
        assert_eq!(status, 200);
        assert!(body["result"]["ts"].is_string(), "{body}");
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            r#"{"id":2,"method":"state.get"}"#,
        )
        .await;
        assert_eq!(status, 200);
        assert!(body["result"]["snapshot"]["device"].is_object());
        // A batch with a hello: the same logical session; an unknown method
        // in it is answered in place; a notification gets no response.
        let batch = r#"[{"id":"h","method":"session.hello","params":{"client":{"name":"bot"}}},{"id":3,"method":"nope"},{"method":"session.ping"},{"id":4,"method":"op.answer","params":{}}]"#;
        let (status, body) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], batch).await;
        assert_eq!(status, 200);
        let items = body.as_array().unwrap();
        assert_eq!(items.len(), 3);
        let hello = items.iter().find(|i| i["id"] == "h").unwrap();
        assert_eq!(hello["result"]["principal"]["id"], "token:gate");
        assert_eq!(hello["result"]["principal"]["trusted"], false);
        assert!(hello["result"]["session"]
            .as_str()
            .unwrap()
            .starts_with("sess_"));
        assert!(items
            .iter()
            .any(|i| i["id"] == 3 && i["error"]["code"] == ApiError::METHOD_NOT_FOUND));
        assert!(items
            .iter()
            .any(|i| i["id"] == 4 && i["error"]["code"] == ApiError::FORBIDDEN));
        // Two calls with the same client.name share the session: the
        // second registers the same key without a conflict.
        let register = r#"{"id":5,"method":"agent.register","params":{"client":{"name":"bot"},"kind":"other","name":"bot","key":"http:bot","lease_ttl_s":30,"caps":{"answer":"poll","order":"poll","focus":false}}}"#;
        let (_, first) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            register,
        )
        .await;
        let session = first["result"]["owner"]["session"].clone();
        assert_eq!(session, hello["result"]["session"], "{first}");
        let (_, again) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            register,
        )
        .await;
        assert_eq!(again["result"]["agent"], first["result"]["agent"]);
        let other = register.replace("\"name\":\"bot\"},", "\"name\":\"other\"},");
        let (_, conflict) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], &other).await;
        assert_eq!(conflict["error"]["code"], ApiError::CONFLICT, "{conflict}");
        // `push` caps have nowhere to go on a logical session.
        for (caps, field) in [
            (
                r#"{"answer":"push","order":"poll","focus":false}"#,
                "caps.answer",
            ),
            (
                r#"{"answer":"poll","order":"push","focus":false}"#,
                "caps.order",
            ),
        ] {
            let pushy = format!(
                r#"{{"id":6,"method":"agent.register","params":{{"client":{{"name":"pusher"}},"kind":"other","name":"pusher","key":"http:pusher","lease_ttl_s":30,"caps":{caps}}}}}"#
            );
            let (_, refused) =
                http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], &pushy).await;
            assert_eq!(refused["error"]["code"], ApiError::INVALID, "{refused}");
            assert_eq!(refused["error"]["data"]["field"], field);
            assert_eq!(
                refused["error"]["data"]["why"],
                "push needs a WS/UDS session"
            );
        }
        // A parse error is a 200 with -32700, an allowed origin passes.
        let (status, body) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], "nope").await;
        assert_eq!(status, 200);
        assert_eq!(body["error"]["code"], ApiError::PARSE);
        let (status, _) = http(
            port,
            "GET",
            "/v1/info",
            &[("Origin", "http://localhost:5173")],
            "",
        )
        .await;
        assert_eq!(status, 403);
        // The body cap: 413 with the JSON-RPC too_large body.
        let big = format!(
            r#"{{"id":7,"method":"session.ping","params":{{"pad":"{}"}}}}"#,
            "x".repeat(BODY_MAX)
        );
        let (status, body) = http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], &big).await;
        assert_eq!(status, 413);
        assert_eq!(body["error"]["code"], ApiError::TOO_LARGE);
        assert_eq!(body["error"]["data"]["max"], BODY_MAX);
        assert_eq!(body["jsonrpc"], "2.0");
        // §3.4 on this door too: a `session.hello` in the body may narrow
        // the token's scopes for the calls beside it, and asking past them
        // is refused by name rather than cut in silence.
        let narrowed = r#"[{"id":"n","method":"session.hello","params":{"client":{"name":"bot"},"scopes":["observe"]}},{"id":8,"method":"agent.heartbeat","params":{"agents":[]}}]"#;
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            narrowed,
        )
        .await;
        assert_eq!(status, 200);
        let items = body.as_array().unwrap();
        let hello = items.iter().find(|i| i["id"] == "n").unwrap();
        assert_eq!(
            hello["result"]["principal"]["scopes"],
            serde_json::json!(["observe"]),
            "{body}"
        );
        assert!(
            items
                .iter()
                .any(|i| i["id"] == 8 && i["error"]["code"] == ApiError::FORBIDDEN),
            "the narrowed body lost `agent`: {body}"
        );
        let over = r#"{"id":9,"method":"session.hello","params":{"client":{"name":"bot"},"scopes":["admin"]}}"#;
        let (status, body) = http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], over).await;
        assert_eq!(status, 200);
        assert_eq!(body["id"], 9);
        assert_eq!(body["error"]["code"], ApiError::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["data"]["need"], "admin");
        served.shutdown().await;
    }

    /// JSON-RPC 2.0: a body of nothing but notifications is answered with
    /// nothing at all — 204, never an empty array.
    #[tokio::test]
    async fn a_batch_of_notifications_alone_is_answered_with_nothing() {
        let daemon = test_api("http-note-batch", FakeVerifier::default());
        let plain = daemon.token("gate", &[Scope::Observe]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let auth = format!("Bearer {plain}");
        for body in [
            r#"[{"method":"session.ping"},{"method":"session.ping"}]"#,
            r#"{"method":"session.ping"}"#,
        ] {
            let (status, answer) =
                http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], body).await;
            assert_eq!(status, 204, "{body}: {answer}");
            assert_eq!(answer, serde_json::Value::Null, "no body at all: {answer}");
        }
        // One answerable member is still a batch: an array of one.
        let (status, answer) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            r#"[{"method":"session.ping"},{"id":1,"method":"session.ping"}]"#,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(answer.as_array().map(Vec::len), Some(1), "{answer}");
        served.shutdown().await;
    }

    /// JSON-RPC 2.0: one malformed member never refuses the whole batch —
    /// it is answered in place with `id:null` and `-32600`.
    #[tokio::test]
    async fn a_malformed_batch_member_is_answered_in_place() {
        let daemon = test_api("http-bad-member", FakeVerifier::default());
        let plain = daemon.token("gate", &[Scope::Observe]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let auth = format!("Bearer {plain}");
        let body = r#"[{"id":1,"method":"session.ping"},{"id":2},"nope",{"id":3,"method":"nope"}]"#;
        let (status, answer) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], body).await;
        assert_eq!(status, 200);
        let items = answer.as_array().unwrap_or_else(|| panic!("{answer}"));
        assert_eq!(items.len(), 4, "{answer}");
        assert!(
            items
                .iter()
                .any(|i| i["id"] == 1 && i["result"]["ts"].is_string()),
            "the good member ran: {answer}"
        );
        assert!(
            items
                .iter()
                .any(|i| i["id"] == 3 && i["error"]["code"] == ApiError::METHOD_NOT_FOUND),
            "{answer}"
        );
        let bad: Vec<&serde_json::Value> = items
            .iter()
            .filter(|i| i["error"]["code"] == ApiError::INVALID_REQUEST)
            .collect();
        assert_eq!(bad.len(), 2, "{answer}");
        for one in bad {
            assert_eq!(one["id"], serde_json::Value::Null, "{one}");
            assert!(one["error"]["data"]["index"].is_number(), "{one}");
        }
        served.shutdown().await;
    }

    /// §4.5: `session.hello{snapshot:true}` carries the snapshot on this
    /// door too, redacted for the token's scopes; without the flag the
    /// reply has no `snapshot` at all.
    #[tokio::test]
    async fn a_hello_over_rpc_carries_the_snapshot_only_when_asked() {
        let daemon = test_api("http-hello-snapshot", FakeVerifier::default());
        let observe = daemon.token("obs", &[Scope::Observe]);
        let content = daemon.token("gui", &[Scope::ObserveContent]);
        // An agent with a pending question, so the redaction has something
        // to hide from the `observe`-only token.
        let session = daemon
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        let registered = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                Request::AgentRegister(crate::core::testkit::register("bot", Some("k"))),
            )
            .await
            .unwrap();
        let agent: AgentId = serde_json::from_value(registered["agent"].clone()).unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                Request::CardSet(crate::core::testkit::card(&agent, "secret?")),
            )
            .await
            .unwrap();
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let hello = r#"{"id":1,"method":"session.hello","params":{"client":{"name":"bot"},"snapshot":true}}"#;
        let (status, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &format!("Bearer {observe}"))],
            hello,
        )
        .await;
        assert_eq!(status, 200);
        let snapshot = &body["result"]["snapshot"];
        assert!(snapshot["device"].is_object(), "{body}");
        assert_eq!(snapshot["rev"], body["result"]["rev"], "{body}");
        assert_eq!(
            snapshot["agents"][&agent.0]["card"]["text"],
            serde_json::Value::Null,
            "an observe token never sees the card text: {body}"
        );
        // The same hello from a token that may see content.
        let (_, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &format!("Bearer {content}"))],
            hello,
        )
        .await;
        assert_eq!(
            body["result"]["snapshot"]["agents"][&agent.0]["card"]["text"], "secret?",
            "{body}"
        );
        // No flag, no snapshot.
        let (_, body) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &format!("Bearer {observe}"))],
            r#"{"id":2,"method":"session.hello","params":{"client":{"name":"bot"}}}"#,
        )
        .await;
        assert!(body["result"].get("snapshot").is_none(), "{body}");
        served.shutdown().await;
    }

    /// R2 on this door: a `session.hello{content:false}` in a body answers
    /// the whole body as for a principal without content — the hello's
    /// snapshot and a `state.get` beside it — and the hello says so. The
    /// same body without the flag, from the same `admin` token, still sees
    /// the content.
    #[tokio::test]
    async fn a_hello_without_content_over_rpc_answers_its_body_without_any() {
        let daemon = test_api("http-hello-no-content", FakeVerifier::default());
        let gui = daemon.token("gui", &[Scope::Admin]);
        let session = daemon
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        let registered = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                Request::AgentRegister(crate::core::testkit::register("bot", Some("k"))),
            )
            .await
            .unwrap();
        let agent: AgentId = serde_json::from_value(registered["agent"].clone()).unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                Request::CardSet(crate::core::testkit::card(&agent, "CARD-SECRET?")),
            )
            .await
            .unwrap();
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let body = |flag: &str| {
            format!(
                r#"[{{"id":1,"method":"session.hello","params":{{"client":{{"name":"gui"}},"snapshot":true{flag}}}}},{{"id":2,"method":"state.get"}}]"#
            )
        };
        let bearer = format!("Bearer {gui}");
        let answer = |reply: &serde_json::Value, id: u64| {
            reply
                .as_array()
                .and_then(|all| all.iter().find(|one| one["id"] == id))
                .cloned()
                .unwrap_or_else(|| panic!("no answer {id}: {reply}"))
        };
        let (status, blind) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &bearer)],
            &body(r#","content":false"#),
        )
        .await;
        assert_eq!(status, 200, "{blind}");
        assert!(!blind.to_string().contains("CARD-SECRET"), "{blind}");
        assert_eq!(answer(&blind, 1)["result"]["content"], false, "{blind}");
        let state = answer(&blind, 2);
        let card = &state["result"]["snapshot"]["agents"][&agent.0]["card"];
        assert_eq!(card["redacted"], true, "{blind}");
        let (_, seeing) = http(port, "POST", "/v1/rpc", &[("Authorization", &bearer)], &body("")).await;
        assert!(seeing.to_string().contains("CARD-SECRET"), "{seeing}");
        assert!(answer(&seeing, 1)["result"].get("content").is_none(), "{seeing}");
        served.shutdown().await;
    }

    /// The same §4.8 rule on this door: a notification-form `agent.wait`
    /// in the body is never answered, so it never empties the mailbox.
    #[tokio::test]
    async fn a_notification_agent_wait_over_rpc_never_consumes_the_mailbox() {
        let daemon = test_api("http-note-wait", FakeVerifier::default());
        let plain = daemon.token("gate", &[Scope::Observe, Scope::Agent]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let auth = format!("Bearer {plain}");
        let register = r#"{"id":1,"method":"agent.register","params":{"client":{"name":"bot"},"kind":"other","name":"bot","key":"http:bot","lease_ttl_s":30,"caps":{"answer":"poll","order":"poll","focus":false}}}"#;
        let (_, reply) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &auth)],
            register,
        )
        .await;
        let agent: AgentId = serde_json::from_value(reply["result"]["agent"].clone()).unwrap();
        let session: SessionId =
            serde_json::from_value(reply["result"]["owner"]["session"].clone()).unwrap();
        let principal = Principal::Token("gate".into());
        let set = daemon
            .api
            .core
            .api(
                principal.clone(),
                session,
                false,
                Request::CardSet(crate::core::testkit::card(&agent, "go?")),
            )
            .await
            .unwrap();
        let card = crate::core::testkit::card_id(&set);
        let operator = daemon
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Operate],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                operator,
                false,
                Request::OpAnswer(deck_model::OpAnswer {
                    agent: agent.clone(),
                    card,
                    choice: Some(0),
                    text: None,
                }),
            )
            .await
            .unwrap();
        // The notification: 204, and the mailbox untouched.
        let note = format!(
            r#"{{"method":"agent.wait","params":{{"client":{{"name":"bot"}},"agent":"{}"}}}}"#,
            agent.0
        );
        let (status, body) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], &note).await;
        assert_eq!(status, 204, "{body}");
        assert!(
            daemon
                .api
                .bus
                .snapshot()
                .agents
                .get(&agent)
                .and_then(|a| a.card.clone())
                .is_some(),
            "the card was cleared for a response nobody received"
        );
        let wait = format!(
            r#"{{"id":2,"method":"agent.wait","params":{{"client":{{"name":"bot"}},"agent":"{}","timeout_s":2}}}}"#,
            agent.0
        );
        let (status, body) =
            http(port, "POST", "/v1/rpc", &[("Authorization", &auth)], &wait).await;
        assert_eq!(status, 200);
        assert_eq!(body["result"]["kind"], "answer", "{body}");
        assert_eq!(body["result"]["choice"], 0, "{body}");
        served.shutdown().await;
    }

    #[tokio::test]
    async fn the_rest_mirror_is_the_token_view_of_the_snapshot() {
        let daemon = test_api("http-rest", FakeVerifier::default());
        let observe = daemon.token("obs", &[Scope::Observe]);
        let content = daemon.token("gui", &[Scope::ObserveContent]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        // An agent with a card, owned by another session.
        let other = daemon
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        let theirs = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::AgentRegister(crate::core::testkit::register("theirs", Some("k"))),
            )
            .await
            .unwrap();
        let theirs: AgentId = serde_json::from_value(theirs["agent"].clone()).unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::CardSet(crate::core::testkit::card(&theirs, "secret?")),
            )
            .await
            .unwrap();
        let obs = format!("Bearer {observe}");
        let gui = format!("Bearer {content}");
        let (status, state) = http(port, "GET", "/v1/state", &[("Authorization", &obs)], "").await;
        assert_eq!(status, 200);
        assert_eq!(state["rev"], daemon.api.bus.rev());
        let agent = &state["snapshot"]["agents"][&theirs.0];
        assert_eq!(agent["card"]["text"], serde_json::Value::Null);
        assert_eq!(agent["card"]["redacted"], true);
        assert_eq!(agent["card"]["choices_n"], 2);
        let (_, agents) = http(port, "GET", "/v1/agents", &[("Authorization", &gui)], "").await;
        let agents = agents.as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["card"]["text"], "secret?");
        let (status, one) = http(
            port,
            "GET",
            &format!("/v1/agents/{}", theirs.0),
            &[("Authorization", &obs)],
            "",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(one["id"], theirs.0);
        assert_eq!(one["redacted"], true);
        let (status, missing) = http(
            port,
            "GET",
            "/v1/agents/ag_nope",
            &[("Authorization", &obs)],
            "",
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(missing["error"], "not_found");
        let (_, cards) = http(port, "GET", "/v1/cards", &[("Authorization", &gui)], "").await;
        assert_eq!(cards[0]["agent"], theirs.0);
        assert_eq!(cards[0]["card"]["text"], "secret?");
        let (_, device) = http(port, "GET", "/v1/device", &[("Authorization", &obs)], "").await;
        assert_eq!(device["link"], "offline");
        let (_, voice) = http(port, "GET", "/v1/voice", &[("Authorization", &obs)], "").await;
        assert_eq!(voice["route"], "none");
        // A logical session's own agent keeps its content for `observe`.
        let register = r#"{"id":1,"method":"agent.register","params":{"client":{"name":"bot"},"kind":"other","name":"mine","key":"k:mine","lease_ttl_s":30,"caps":{"answer":"poll","order":"poll","focus":false}}}"#;
        let agent_token = daemon.token("bot", &[Scope::Observe, Scope::Agent]);
        let bot = format!("Bearer {agent_token}");
        let (_, mine) = http(
            port,
            "POST",
            "/v1/rpc",
            &[("Authorization", &bot)],
            register,
        )
        .await;
        let mine = mine["result"]["agent"].as_str().unwrap().to_string();
        let card = format!(
            r#"{{"id":2,"method":"agent.card.set","params":{{"client":{{"name":"bot"}},"agent":"{mine}","id":null,"type":"q","text":"mine?","choices":[{{"label":"a","detail":null}}],"rec":null,"free_text":false,"deliver":"poll","timeout_s":null,"origin":{{"kind":"custom","tool":null,"request_id":null,"group":null}}}}}}"#
        );
        let (_, set) = http(port, "POST", "/v1/rpc", &[("Authorization", &bot)], &card).await;
        assert!(set["result"]["card"]["id"].is_number(), "{set}");
        let (_, agents) = http(
            port,
            "GET",
            "/v1/agents?client=bot",
            &[("Authorization", &bot)],
            "",
        )
        .await;
        let own = agents
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == mine)
            .unwrap();
        assert_eq!(own["card"]["text"], "mine?");
        let (_, agents) = http(port, "GET", "/v1/agents", &[("Authorization", &bot)], "").await;
        let default_view = agents
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == mine)
            .unwrap();
        assert_eq!(
            default_view["card"]["text"],
            serde_json::Value::Null,
            "the `default` logical session owns nothing"
        );
        served.shutdown().await;
    }

    /// Read SSE frames from a raw stream until `n` events with a name in
    /// `wanted` arrived (comments and keep-alives skipped).
    async fn sse_events(
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
        n: usize,
    ) -> Vec<(Option<String>, String, serde_json::Value)> {
        let mut out = Vec::new();
        let (mut id, mut name, mut data) = (None, None, None);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while out.len() < n {
            let mut line = String::new();
            let n_read = tokio::time::timeout_at(deadline, reader.read_line(&mut line))
                .await
                .expect("an SSE line within 5 s")
                .unwrap();
            assert!(n_read > 0, "SSE stream closed");
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let (Some(name), Some(data)) = (name.take(), data.take()) {
                    let data: String = data;
                    out.push((id.take(), name, serde_json::from_str(&data).unwrap()));
                }
                id = None;
                continue;
            }
            if let Some(v) = line.strip_prefix("id:") {
                id = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("event:") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("data:") {
                data = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix(':') {
                let _ = v; // a comment (the keep-alive)
            }
        }
        out
    }

    #[tokio::test]
    async fn sse_replays_since_then_streams_filtered_redacted_events_and_resyncs() {
        let daemon = test_api("http-sse", FakeVerifier::default());
        let plain = daemon.token("obs", &[Scope::Observe]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        // Two events before the reader connects: agent.added and card.set.
        let other = daemon
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Agent],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        let rev0 = daemon.api.bus.rev();
        let theirs = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::AgentRegister(crate::core::testkit::register("theirs", Some("k"))),
            )
            .await
            .unwrap();
        let theirs: AgentId = serde_json::from_value(theirs["agent"].clone()).unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::CardSet(crate::core::testkit::card(&theirs, "secret?")),
            )
            .await
            .unwrap();
        let rev1 = daemon.api.bus.rev();
        assert!(rev1 > rev0);
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(
            format!(
                "GET /v1/events?since={rev0}&types=card.* HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {plain}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut status = String::new();
        r.read_line(&mut status).await.unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        let mut content_type = None;
        loop {
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-type:") {
                content_type = Some(v.trim().to_string());
            }
        }
        assert_eq!(content_type.as_deref(), Some("text/event-stream"));
        // The replay: only card.set (types), redacted (observe), id = rev.
        let replayed = sse_events(&mut r, 1).await;
        let (id, name, data) = &replayed[0];
        assert_eq!(name, "event");
        assert_eq!(data["type"], "card.set");
        assert_eq!(id.as_deref(), Some(data["rev"].to_string().as_str()));
        assert_eq!(data["data"]["card"]["text"], serde_json::Value::Null);
        assert_eq!(data["data"]["redacted"], true);
        // Live: an agent.changed is filtered out, the next card.set arrives.
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::AgentUpdate(deck_model::AgentUpdate {
                    agent: theirs.clone(),
                    state: Some(deck_model::AgentState::Working),
                    title: None,
                    title_source: None,
                    since: None,
                    name: None,
                    meta: None, task: None, subagents: None,
                }),
            )
            .await
            .unwrap();
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                Request::CardSet(crate::core::testkit::card(&theirs, "again?")),
            )
            .await
            .unwrap();
        // Replacing a card emits `card.cleared` before `card.set`: both are
        // `card.*`, the `agent.changed` in between is not.
        let live = sse_events(&mut r, 2).await;
        assert_eq!(
            live.iter().map(|e| e.2["type"].clone()).collect::<Vec<_>>(),
            vec!["card.cleared", "card.set"]
        );
        assert!(live[1].2["rev"].as_u64().unwrap() > rev1);
        // The observer session exists while the stream is open.
        let sessions_open = daemon
            .api
            .core
            .call(
                crate::core::testkit::ctx(other.0, &[Scope::Observe]),
                "service.info",
                serde_json::json!({}),
            )
            .await
            .result
            .unwrap()["sessions"]
            .as_u64()
            .unwrap();
        assert!(sessions_open >= 2, "{sessions_open}");
        // Overflow the broadcast without yielding (a current-thread runtime:
        // the SSE task cannot consume meanwhile): the next thing it sees is
        // `Lagged`, so the next frame is a resync.
        for i in 0..(crate::bus::BROADCAST_CAPACITY as u64 + 8) {
            daemon.api.bus.publish(Published {
                event: Arc::new(deck_model::Event {
                    rev: rev1 + 1000 + i,
                    ts: deck_model::Ts(0),
                    kind: deck_model::EventKind::CardSet,
                    principal: Principal::Device,
                    data: serde_json::json!({"agent": "ag_x", "card": {"text": "t"}}),
                }),
                owner: None,
            });
        }
        let after = sse_events(&mut r, 1).await;
        assert_eq!(after[0].1, "resync", "{after:?}");
        assert!(after[0].2["rev"].is_number());
        // Dropping the reader closes the observer session.
        drop(w);
        drop(r);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let sessions = daemon
                .api
                .core
                .call(
                    crate::core::testkit::ctx(other.0, &[Scope::Observe]),
                    "service.info",
                    serde_json::json!({}),
                )
                .await
                .result
                .unwrap()["sessions"]
                .as_u64()
                .unwrap();
            if sessions < sessions_open || std::time::Instant::now() > deadline {
                assert!(sessions < sessions_open, "{sessions}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // A `since` the ring no longer holds starts with a resync.
        let bus = crate::bus::Bus::new(4);
        for rev in 1..=8 {
            bus.publish(Published {
                event: Arc::new(deck_model::Event {
                    rev,
                    ts: deck_model::Ts(0),
                    kind: deck_model::EventKind::Error,
                    principal: Principal::Device,
                    data: serde_json::json!({}),
                }),
                owner: None,
            });
        }
        assert!(bus.replay(1, 10, None).1, "gap");
        assert_eq!(types_of(None), vec!["*"]);
        assert_eq!(types_of(Some(" card.*, error ,")), vec!["card.*", "error"]);
        assert_eq!(types_of(Some(",")), vec!["*"]);
        served.shutdown().await;
    }

    #[tokio::test]
    async fn ws_needs_a_valid_bearer_or_a_hello_token_and_runs_the_line_session() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let daemon = test_api("http-ws", FakeVerifier::default());
        let plain = daemon.token("ws", &[Scope::Observe, Scope::Agent]);
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let url = format!("ws://127.0.0.1:{port}/v1/ws");
        // A bad bearer is refused before the upgrade.
        let mut bad = url.as_str().into_client_request().unwrap();
        bad.headers_mut()
            .insert("Authorization", "Bearer dk_nope".parse().unwrap());
        let err = tokio_tungstenite::connect_async(bad).await.unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");
        // A foreign Origin is refused on the upgrade too.
        let mut foreign = url.as_str().into_client_request().unwrap();
        foreign
            .headers_mut()
            .insert("Origin", "http://evil.example".parse().unwrap());
        let err = tokio_tungstenite::connect_async(foreign).await.unwrap_err();
        assert!(err.to_string().contains("403"), "{err}");
        // No bearer: the hello carries the token.
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .unwrap();
        ws.send(WsMessage::text(
            r#"{"id":1,"method":"session.hello","params":{"client":{"name":"t","version":"1"},"scopes":["observe"],"subscribe":["agent.*"],"token":"dk_nope"}}"#,
        ))
        .await
        .unwrap();
        let refused: serde_json::Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(refused["error"]["code"], ApiError::UNAUTHORIZED);
        assert!(
            ws.next()
                .await
                .is_none_or(|m| m.is_err() || m.unwrap().is_close()),
            "closed after the refusal"
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .unwrap();
        ws.send(WsMessage::text(format!(
            r#"{{"id":1,"method":"session.hello","params":{{"client":{{"name":"t","version":"1"}},"scopes":["observe","agent"],"subscribe":["agent.*"],"token":"{plain}"}}}}"#
        )))
        .await
        .unwrap();
        let hello: serde_json::Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(hello["result"]["principal"]["id"], "token:ws", "{hello}");
        assert_eq!(hello["result"]["principal"]["trusted"], false);
        ws.send(WsMessage::text(r#"{"id":2,"method":"session.ping"}"#))
            .await
            .unwrap();
        let pong: serde_json::Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(pong["result"]["ts"].is_string());
        // A frame past the cap: -32012 and the socket closes.
        let big = format!(
            r#"{{"id":3,"method":"session.ping","params":{{"pad":"{}"}}}}"#,
            "x".repeat(LINE_MAX)
        );
        ws.send(WsMessage::text(big)).await.unwrap();
        let mut too_large = false;
        while let Some(Ok(msg)) = ws.next().await {
            if let Ok(text) = msg.to_text() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                    if v["error"]["code"] == ApiError::TOO_LARGE {
                        too_large = true;
                    }
                }
            }
            if msg.is_close() {
                break;
            }
        }
        assert!(too_large, "the -32012 was written before the close");
        // With the bearer, the hello needs no token: the session is Live
        // and its own agent's events come back with content.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut()
            .insert("Authorization", format!("Bearer {plain}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        ws.send(WsMessage::text(
            r#"{"id":1,"method":"session.hello","params":{"client":{"name":"t","version":"1"},"subscribe":["agent.added"]}}"#,
        ))
        .await
        .unwrap();
        let hello: serde_json::Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(
            hello["result"]["principal"]["scopes"],
            serde_json::json!(["observe", "agent"]),
            "omitted scopes: the token's list"
        );
        ws.send(WsMessage::text(
            r#"{"id":2,"method":"agent.register","params":{"kind":"other","name":"bot","key":"k","lease_ttl_s":30,"caps":{"answer":"push","order":"poll","focus":false}}}"#,
        ))
        .await
        .unwrap();
        let mut saw_reply = false;
        let mut saw_added = false;
        for _ in 0..2 {
            let v: serde_json::Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            if v["id"] == 2 {
                assert_eq!(v["result"]["owner"]["session"], hello["result"]["session"]);
                saw_reply = true;
            } else if v["method"] == "event" {
                assert_eq!(v["params"]["data"]["agent"]["name"], "bot");
                saw_added = true;
            }
        }
        assert!(saw_reply && saw_added);
        ws.close(None).await.unwrap();
        served.shutdown().await;
    }

    /// The degraded reason the core recorded, polled until it is `want`.
    async fn wait_degraded(daemon: &crate::api::testkit::TestDaemon, want: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let got = daemon.api.bus.snapshot().service.degraded.clone();
            if got.as_deref() == Some(want) || std::time::Instant::now() > deadline {
                return got.unwrap_or_default();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_taken_port_degrades_and_a_foreign_v1_info_is_a_hijack_risk() {
        // A squatter holds the port and answers `/v1/info` with someone
        // else's object.
        let squatter = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = squatter.local_addr().unwrap().port();
        let answering = tokio::spawn(async move {
            while let Ok((mut stream, _)) = squatter.accept().await {
                let body = r#"{"api":"1","daemon":"not-deckd","uds":"/tmp/other.sock"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        let daemon =
            crate::api::testkit::test_api_on("http-degraded", FakeVerifier::default(), port);
        let served = crate::api::serve(daemon.api.clone()).await;
        assert_eq!(served.port, None, "the port was taken");
        assert_eq!(
            std::fs::read_to_string(&daemon.api.paths.port).unwrap(),
            "",
            "the port file is empty while degraded"
        );
        // `serve` awaited the core's `service.bound` before it returned.
        assert_eq!(
            daemon.api.bus.snapshot().service.degraded.as_deref(),
            Some(DEGRADED_PORT_IN_USE)
        );
        // The probe runs at once: the answer is not ours → hijack risk, and
        // `/v1/info` hides the port in both degradations.
        assert_eq!(
            wait_degraded(&daemon, DEGRADED_HIJACK_RISK).await,
            DEGRADED_HIJACK_RISK
        );
        let info = info(&daemon.api);
        assert_eq!(info["degraded"], DEGRADED_HIJACK_RISK);
        assert_eq!(info["port"], serde_json::Value::Null);
        assert!(!is_ours(
            &serde_json::json!({"api": "1", "daemon": "not-deckd", "uds": "/tmp/other.sock"}),
            &daemon.api
        ));
        assert!(is_ours(&info, &daemon.api), "our own object is ours");
        served.shutdown().await;
        answering.abort();
    }

    /// D3: with the door off the daemon serves its socket and binds no TCP
    /// port. The configured port is this test's the whole time, so a door
    /// that so much as tried it would have degraded `port_in_use`; this one
    /// records no port and no degrade, empties the port file an earlier run
    /// left behind, and answers on the socket with `port: null`.
    #[tokio::test]
    async fn with_the_tcp_door_off_only_the_socket_is_served() {
        let held = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = held.local_addr().unwrap().port();
        let daemon =
            crate::api::testkit::test_api_on("http-tcp-off", FakeVerifier::default(), port);
        let mut config = (**daemon.api.config.load()).clone();
        config.api.tcp = Some(false);
        daemon.api.config.store(Arc::new(config));
        std::fs::write(&daemon.api.paths.port, "47201").unwrap();
        let served = crate::api::serve(daemon.api.clone()).await;
        assert!(served.uds, "the socket bound");
        assert_eq!(served.port, None, "no tcp door");
        assert_eq!(
            std::fs::read_to_string(&daemon.api.paths.port).unwrap(),
            "",
            "no port named, the stale one included"
        );
        let service = daemon.api.bus.snapshot().service.clone();
        assert_eq!((service.port, service.degraded.as_deref()), (0, None));
        assert_eq!(held_port(&service), None);
        assert_eq!(info(&daemon.api)["port"], serde_json::Value::Null);
        let stream = tokio::net::UnixStream::connect(&daemon.api.paths.socket)
            .await
            .unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(
            b"{\"id\":1,\"method\":\"session.hello\",\"params\":{\"client\":{\"name\":\"t\",\"version\":\"0\"},\"scopes\":[\"observe\"]}}\n\
              {\"id\":2,\"method\":\"service.info\"}\n",
        )
        .await
        .unwrap();
        let reply = loop {
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(3), r.read_line(&mut line))
                .await
                .expect("an answer within 3 s")
                .unwrap();
            assert_ne!(read, 0, "the socket closed");
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["id"] == 2 {
                break value;
            }
        };
        assert_eq!(reply["result"]["port"], serde_json::Value::Null, "{reply}");
        assert_eq!(
            reply["result"]["degraded"],
            serde_json::Value::Null,
            "{reply}"
        );
        drop(w);
        served.shutdown().await;
        drop(held);
    }

    /// The door is off unless the file switches it on (the owner's decision
    /// 12, which ended D3's "on by default"), or `deckd run` found the
    /// pre-plugin Claude hooks that post to it while the file leaves the key
    /// out; an explicit value is honoured either way. A default daemon binds
    /// nothing; the gate's own API daemon (`tools/host-test.sh`, which names
    /// `tcp = true`) and one kept open for the old hooks bind their door.
    #[tokio::test]
    async fn the_door_is_bound_only_when_the_file_or_the_old_claude_hooks_ask() {
        use crate::config::Config;
        let gate = Config::parse(
            "[api]\nport = 0\ntcp = true\nallowed_origins = [\"http://localhost:5173\"]\n\n[liveness]\nseed = false\n\n[codex]\ndaemon = false\n",
        )
        .unwrap();
        let mut left_out = Config::default();
        left_out.api.port = 0;
        let mut off = left_out.clone();
        off.api.tcp = Some(false);
        for (name, config, legacy_hooks, bound) in [
            ("http-tcp-default", left_out.clone(), false, false),
            ("http-tcp-gate", gate, false, true),
            ("http-tcp-legacy", left_out, true, true),
            ("http-tcp-off-legacy", off, true, false),
        ] {
            let mut daemon = test_api(name, FakeVerifier::default());
            Arc::get_mut(&mut daemon.api)
                .expect("the testkit's Api is not shared yet")
                .legacy_hooks = legacy_hooks;
            daemon.api.config.store(Arc::new(config));
            let served = crate::api::serve(daemon.api.clone()).await;
            let file = std::fs::read_to_string(&daemon.api.paths.port).unwrap();
            if bound {
                let port = served.port.expect("the door is bound");
                assert_eq!(file.trim(), port.to_string(), "{name}");
                let (status, body) = http(port, "GET", "/v1/info", &[], "").await;
                assert_eq!(status, 200, "{name}: {body}");
                assert_eq!(body["port"], port, "{name}");
            } else {
                assert_eq!(served.port, None, "{name}");
                assert_eq!(file, "", "{name}: no port named");
                assert_eq!(info(&daemon.api)["port"], serde_json::Value::Null, "{name}");
                let service = daemon.api.bus.snapshot().service.clone();
                assert_eq!((service.port, service.degraded), (0, None), "{name}");
            }
            served.shutdown().await;
        }
    }

    #[test]
    fn hosts_are_loopback_only_and_on_our_port() {
        // The port the door is bound to is part of the check (§3.3).
        assert!(host_ok("127.0.0.1:47201", Some(47201)));
        assert!(host_ok("localhost:47201", Some(47201)));
        assert!(!host_ok("127.0.0.1:47202", Some(47201)));
        assert!(!host_ok("localhost:0", Some(47201)));
        assert!(!host_ok("127.0.0.1:99999", Some(47201)), "not even a port");
        // A `Host` without a port names no wrong port; a door that does
        // not know its own port yet checks the name alone.
        assert!(host_ok("LOCALHOST", Some(47201)));
        assert!(host_ok("127.0.0.1", Some(47201)));
        assert!(host_ok("127.0.0.1:47202", None));
        // The name half is unchanged.
        assert!(!host_ok("evil.example:47201", Some(47201)));
        assert!(!host_ok("127.0.0.1.evil.example", Some(47201)));
        assert!(!host_ok("[::1]:47201", Some(47201)));
        assert!(!host_ok("", Some(47201)));
        assert!(!host_ok("", None));
    }

    /// A loopback `Host` naming some other port is not this door's (§3.3);
    /// the port the caller actually reached is the only one accepted.
    #[tokio::test]
    async fn a_host_naming_another_port_is_refused() {
        let daemon = test_api("http-host-port", FakeVerifier::default());
        let served = crate::api::serve(daemon.api.clone()).await;
        let port = served.port.unwrap();
        let (status, _) = http(
            port,
            "GET",
            "/v1/info",
            &[("Host", &format!("127.0.0.1:{port}"))],
            "",
        )
        .await;
        assert_eq!(status, 200, "our own port");
        let (status, body) = http(
            port,
            "GET",
            "/v1/info",
            &[("Host", &format!("localhost:{}", port ^ 1))],
            "",
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"], "bad_host");
        served.shutdown().await;
    }

    #[test]
    fn push_caps_lsof_output_and_probe_bodies_parse() {
        assert_eq!(
            push_cap(&serde_json::json!({"caps": {"answer": "push", "order": "poll"}})),
            Some("caps.answer")
        );
        assert_eq!(
            push_cap(&serde_json::json!({"caps": {"answer": "poll", "order": "push"}})),
            Some("caps.order")
        );
        assert_eq!(
            push_cap(&serde_json::json!({"caps": {"answer": "poll", "order": "poll"}})),
            None
        );
        assert_eq!(push_cap(&serde_json::json!({})), None);
        let lsof = parse_lsof("p4242\ncnode\nf12\np4243\ncother\n").unwrap();
        assert_eq!(lsof["pid"], 4242);
        assert_eq!(lsof["command"], "node");
        assert!(parse_lsof("").is_none());
        assert!(parse_lsof("cnode\n").is_none());
        assert_eq!(parse_probe_body("{\"api\":\"1\"}")["api"], "1");
        assert_eq!(
            parse_probe_body("b\r\n{\"api\":\"1\"}\r\n0\r\n\r\n")["api"],
            "1"
        );
        assert_eq!(
            parse_probe_body("<html>nope</html>"),
            serde_json::Value::Null
        );
    }
}
