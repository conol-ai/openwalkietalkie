//! The line session shared by the UDS and WS doors (contract §3.3): the
//! first line must be `session.hello`; it resolves the principal, the
//! scopes and the trust, opens a `Live` session and replays `since`; then
//! every line is dispatched concurrently (responses in completion order),
//! bus events are filtered by the subscription globs and redacted for
//! this session, deliveries from the core are written before any queued
//! event and confirmed with `delivered` after the flush; EOF closes the
//! session.
use super::dispatch::{answers_on_write, dispatch, ConnCtx};
use super::rpc::{self, Incoming, RpcRequest, RpcResponse};
use super::session::{self, ClientInfo};
use super::Api;
use crate::bus::{matches_any, Published};
use crate::core::handle::{Delivery, OpenSession, Outbound, SessionKind};
use crate::trust::PeerVerdict;
use deck_model::{redact_event, redact_for, ApiError, Principal, Scope};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};

/// A line (UDS) or a text frame (WS) may not exceed this.
pub const LINE_MAX: usize = 256 * 1024;
/// Notifications queued for a connection before the core sees it as full.
pub const OUTBOUND_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ConnError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The line/frame exceeded `LINE_MAX`.
    #[error("line too large")]
    TooLarge,
    #[error("closed")]
    Closed,
}

pub type LineStream = Pin<Box<dyn Stream<Item = Result<String, ConnError>> + Send>>;
pub type LineSink = Pin<Box<dyn Sink<String, Error = ConnError> + Send>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    Uds,
    Ws,
}

/// What the door knows before the hello.
#[derive(Clone, Debug)]
pub struct Peer {
    pub door: Door,
    /// The UDS peer's `uid:<n>`, or the WS bearer's `token:<name>` with
    /// its scopes; `None` on WS when the token comes in the hello.
    pub principal: Option<(Principal, Vec<Scope>)>,
    pub verdict: PeerVerdict,
}

/// `session.hello` params (§4.5).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HelloParams {
    #[serde(default)]
    pub client: ClientInfo,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    #[serde(default)]
    pub snapshot: bool,
    #[serde(default)]
    pub since: Option<u64>,
    /// `false`: show this session no content at all, whatever its scopes
    /// grant (`ConnCtx::content`). Absent is `true`, as before the field.
    #[serde(default)]
    pub content: Option<bool>,
}

/// The principal, scopes and trust of a hello (§3.4).
pub struct Resolved {
    pub principal: Principal,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
}

/// Resolve a hello against the peer and the config.
pub fn resolve_hello(api: &Api, peer: &Peer, hello: &HelloParams) -> Result<Resolved, ApiError> {
    let config = api.config.load();
    let (principal, maximum, default, trusted) = match (&peer.door, &peer.principal) {
        (Door::Uds, Some((principal, _))) => (
            principal.clone(),
            session::expand_scope(&config.api.uds_max_scope)?,
            session::expand_scope(&config.api.uds_default_scope)?,
            peer.verdict.trusted,
        ),
        (Door::Ws, Some((principal, scopes))) => {
            (principal.clone(), scopes.clone(), scopes.clone(), false)
        }
        (Door::Ws, None) => {
            let plaintext = hello.token.as_deref().ok_or_else(ApiError::unauthorized)?;
            let tokens = api.tokens.load();
            let (principal, scopes) =
                session::token_principal(&tokens, plaintext, crate::core::wall_now())
                    .ok_or_else(ApiError::unauthorized)?;
            (principal, scopes.clone(), scopes, false)
        }
        (Door::Uds, None) => return Err(ApiError::unauthorized()),
    };
    let requested = match &hello.scopes {
        Some(names) => session::parse_scopes(names)?,
        None => default,
    };
    let scopes = session::resolve(&requested, &maximum, peer.verdict.ancestry_capped)?;
    Ok(Resolved {
        principal,
        scopes,
        trusted,
    })
}

/// Write one line, ignoring a closed peer.
async fn put(writer: &mut LineSink, line: String) -> bool {
    writer.send(line).await.is_ok()
}

/// The session loop. Returns when the peer goes away, the door stops, or
/// the hello was refused.
pub async fn run(
    api: Arc<Api>,
    peer: Peer,
    mut reader: LineStream,
    mut writer: LineSink,
    mut stop: watch::Receiver<bool>,
) {
    // 1. The hello, first or nothing.
    let first = tokio::select! {
        line = reader.next() => line,
        _ = stop.changed() => return,
    };
    let line = match first {
        Some(Ok(line)) => line,
        Some(Err(ConnError::TooLarge)) => {
            put(
                &mut writer,
                RpcResponse::err(serde_json::Value::Null, ApiError::too_large(LINE_MAX)).to_line(),
            )
            .await;
            return;
        }
        _ => return,
    };
    let (hello_id, hello) = match rpc::parse(&line) {
        Ok(Incoming::Single(req)) if req.method == "session.hello" => {
            let id = req.id.clone().unwrap_or(serde_json::Value::Null);
            match serde_json::from_value::<HelloParams>(if req.params.is_null() {
                serde_json::json!({})
            } else {
                req.params
            }) {
                Ok(hello) => (id, hello),
                Err(e) => {
                    put(
                        &mut writer,
                        RpcResponse::err(id, ApiError::invalid_params(&e.to_string())).to_line(),
                    )
                    .await;
                    return;
                }
            }
        }
        Ok(Incoming::Single(req)) => {
            let id = req.id.unwrap_or(serde_json::Value::Null);
            put(
                &mut writer,
                RpcResponse::err(id, ApiError::unauthorized_why("hello_first")).to_line(),
            )
            .await;
            return;
        }
        Ok(Incoming::Batch(_)) => {
            put(
                &mut writer,
                RpcResponse::err(
                    serde_json::Value::Null,
                    ApiError::unauthorized_why("hello_first"),
                )
                .to_line(),
            )
            .await;
            return;
        }
        Err(e) => {
            // Not JSON-RPC at all (an HTTP request line, say): one error, close.
            put(
                &mut writer,
                RpcResponse::err(serde_json::Value::Null, e).to_line(),
            )
            .await;
            return;
        }
    };
    let resolved = match resolve_hello(&api, &peer, &hello) {
        Ok(r) => r,
        Err(e) => {
            put(&mut writer, RpcResponse::err(hello_id, e).to_line()).await;
            return;
        }
    };
    tracing::debug!(
        door = ?peer.door,
        principal = %resolved.principal,
        scopes = ?resolved.scopes,
        trusted = resolved.trusted,
        client = %hello.client.name,
        "hello"
    );
    // 2. Subscribe before reading the rev so nothing falls between.
    let mut bus_rx = api.bus.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
    let session_id = match api
        .core
        .open_session(OpenSession {
            kind: SessionKind::Live { tx: out_tx },
            principal: resolved.principal.clone(),
            scopes: resolved.scopes.clone(),
            trusted: resolved.trusted,
            client: hello.client.clone(),
        })
        .await
    {
        Ok(id) => id,
        Err(e) => {
            put(&mut writer, RpcResponse::err(hello_id, e).to_line()).await;
            return;
        }
    };
    let ctx = ConnCtx {
        core: api.core.clone(),
        principal: resolved.principal,
        session: session_id,
        scopes: resolved.scopes,
        trusted: resolved.trusted,
        client: hello.client.clone(),
        adapters: api.adapters.clone(),
        content: hello.content.unwrap_or(true),
    };
    let mut subs: Vec<String> = hello.subscribe.clone();
    let snapshot = api.bus.snapshot();
    let rev = snapshot.rev;
    let mut reply = serde_json::json!({
        "api": "1",
        "daemon": crate::core::dispatch::VERSION,
        "principal": {
            "id": ctx.principal.to_string(),
            "scopes": ctx.scopes,
            "trusted": ctx.trusted,
        },
        "session": session_id,
        "rev": rev,
    });
    // Said back only to a hello that asked, so a client can tell this
    // daemon honoured it from one that never heard of it.
    if !ctx.content {
        reply["content"] = false.into();
    }
    if hello.snapshot {
        reply["snapshot"] =
            serde_json::to_value(redact_for(&snapshot, &ctx.seen_as(), &BTreeSet::new()))
                .unwrap_or(serde_json::Value::Null);
    }
    // The replay: the ring after `since`, as this session would have seen
    // it live (its subscriptions, its redaction); `replayed` counts what
    // is actually written. A `since` the ring no longer holds is `gap`.
    let mut replay: Vec<String> = Vec::new();
    if let Some(since) = hello.since {
        let (events, gap) = api.bus.replay(since, usize::MAX, None);
        if gap {
            reply["gap"] = true.into();
        } else {
            replay = events
                .iter()
                .filter_map(|event| event_for(&ctx, &subs, event, false))
                .collect();
            reply["replayed"] = replay.len().into();
        }
    }
    if !put(&mut writer, RpcResponse::ok(hello_id, reply).to_line()).await {
        api.core.close_session(session_id);
        return;
    }
    for line in replay {
        if !put(&mut writer, line).await {
            api.core.close_session(session_id);
            return;
        }
    }
    // 3. The session: reads, responses, deliveries, events. A response
    // carries the delivery it confirms once written — an `agent.wait`
    // result, or a held hook's decision (§4.9).
    let (resp_tx, mut resp_rx) = mpsc::channel::<(String, Option<Delivery>)>(OUTBOUND_CAPACITY);
    // Every request this session dispatches runs on its own task, and the
    // session **owns** them: a peer that goes away — Ctrl-C in the CLI, a
    // killed `deckd hook`, the helper hitting its own call timeout — must
    // take its in-flight requests with it. Dropping a held hook's task
    // drops its `HoldGuard`, which is what sends `CoreMsg::HoldGone`, and
    // §4.8/§12 promise that releases the hold and degrades the card
    // *immediately* rather than letting both linger for `hold_s` (120 s).
    // With a detached `tokio::spawn` nothing owned them and nothing ever
    // did: Codex hooks are shell form (C12/C13), so this door is the only
    // one they use.
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            out = out_rx.recv() => match out {
                Some(Outbound::Line(line)) => {
                    if !put(&mut writer, line).await { break; }
                }
                Some(Outbound::Deliver { line, delivery }) => {
                    let ok = put(&mut writer, line).await;
                    let Delivery { agent, card } = delivery;
                    api.core.delivered(agent, card, ok).await;
                    if !ok { break; }
                }
                // The core dropped this session's sender: nothing more will
                // ever arrive on it, and a `None` must not spin the loop.
                None => break,
            },
            resp = resp_rx.recv() => {
                if let Some((line, after_write)) = resp {
                    let ok = put(&mut writer, line).await;
                    if let Some(Delivery { agent, card }) = after_write {
                        api.core.delivered(agent, card, ok).await;
                    }
                    if !ok { break; }
                }
            },
            published = bus_rx.recv() => match published {
                Ok(p) => {
                    let own = p.owner == Some(session_id);
                    if let Some(line) = published_line(&ctx, &subs, &p, own) {
                        if !put(&mut writer, line).await { break; }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    bus_rx = bus_rx.resubscribe();
                    if !put(&mut writer, rpc::resync_line(api.bus.rev())).await { break; }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            line = reader.next() => match line {
                Some(Ok(line)) => {
                    if !handle_line(&ctx, &mut subs, &line, &resp_tx, &mut writer, &mut tasks).await { break; }
                }
                Some(Err(ConnError::TooLarge)) => {
                    put(&mut writer, RpcResponse::err(serde_json::Value::Null, ApiError::too_large(LINE_MAX)).to_line()).await;
                    break;
                }
                _ => break,
            },
            // Reap what finished, so a long session's set is the requests
            // actually in flight and not everything it ever ran. Last in
            // the `biased` order: a burst of completions must not starve
            // the reader.
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    // The peer is gone: cancel what it was still waiting on before the
    // session closes, so every armed `HoldGuard` reports now.
    tasks.shutdown().await;
    api.core.close_session(session_id);
    tracing::debug!(session = %session_id, "connection closed");
}

/// A response line on its way to the writer, with the delivery it confirms
/// once written (an `agent.wait` result, §4.8 `poll`).
type Pending = (String, Option<Delivery>);

/// One line after the hello: subscriptions inline, everything else
/// dispatched on its own task. False when the writer is gone.
async fn handle_line(
    ctx: &ConnCtx,
    subs: &mut Vec<String>,
    line: &str,
    resp_tx: &mpsc::Sender<Pending>,
    writer: &mut LineSink,
    tasks: &mut tokio::task::JoinSet<()>,
) -> bool {
    let incoming = match rpc::parse(line) {
        Ok(incoming) => incoming,
        Err(e) => {
            return put(
                writer,
                RpcResponse::err(serde_json::Value::Null, e).to_line(),
            )
            .await;
        }
    };
    match incoming {
        Incoming::Single(req) => {
            if let Some(resp) = subscription(subs, &req) {
                return match resp {
                    Some(line) => put(writer, line).await,
                    None => true,
                };
            }
            let ctx = ctx.clone();
            let tx = resp_tx.clone();
            tasks.spawn(async move {
                if let Some((resp, after_write)) = answer(&ctx, req).await {
                    if tx
                        .send((resp.to_line(), after_write.clone()))
                        .await
                        .is_err()
                    {
                        // The writer is gone, so the response this result
                        // *is* was never written: report the failure
                        // rather than leaving the card to time out.
                        if let Some(Delivery { agent, card }) = after_write {
                            ctx.core.delivered(agent, card, false).await;
                        }
                    }
                }
            });
        }
        Incoming::Batch(reqs) => {
            let mut inline = Vec::new();
            let mut spawned = Vec::new();
            for member in reqs {
                let req = match member {
                    Ok(req) => req,
                    // JSON-RPC 2.0: a malformed member is answered in
                    // place, with `id:null`, and the rest still run.
                    Err(e) => {
                        inline.push(RpcResponse::err(serde_json::Value::Null, e).to_line());
                        continue;
                    }
                };
                match subscription(subs, &req) {
                    Some(Some(line)) => inline.push(line),
                    Some(None) => {}
                    None => spawned.push(req),
                }
            }
            let ctx = ctx.clone();
            let tx = resp_tx.clone();
            tasks.spawn(async move {
                let answers = futures_util::future::join_all(
                    spawned.into_iter().map(|req| answer(&ctx, req)),
                )
                .await;
                let mut lines: Vec<serde_json::Value> = inline
                    .into_iter()
                    .filter_map(|l| serde_json::from_str(&l).ok())
                    .collect();
                // A batch can carry at most one `agent.wait` result per
                // agent; the first delivery is confirmed after the batch
                // line is written, the rest at once (best effort).
                let mut deliveries: Vec<Delivery> = Vec::new();
                for (resp, after_write) in answers.into_iter().flatten() {
                    if let Ok(value) = serde_json::to_value(resp) {
                        lines.push(value);
                    }
                    deliveries.extend(after_write);
                }
                let mut deliveries = deliveries.into_iter();
                let first = deliveries.next();
                for Delivery { agent, card } in deliveries {
                    ctx.core.delivered(agent, card, true).await;
                }
                if !lines.is_empty() {
                    if tx
                        .send((serde_json::Value::Array(lines).to_string(), first.clone()))
                        .await
                        .is_err()
                    {
                        if let Some(Delivery { agent, card }) = first {
                            ctx.core.delivered(agent, card, false).await;
                        }
                    }
                } else if let Some(Delivery { agent, card }) = first {
                    // No line to write (a batch of notifications), so the
                    // delivery cannot be honest: report the failure.
                    ctx.core.delivered(agent, card, false).await;
                }
            });
        }
    }
    true
}

/// `session.subscribe/unsubscribe` are answered here (`Some(response
/// line)`, `Some(None)` for a notification); anything else is `None`.
fn subscription(subs: &mut Vec<String>, req: &RpcRequest) -> Option<Option<String>> {
    let add = match req.method.as_str() {
        "session.subscribe" => true,
        "session.unsubscribe" => false,
        _ => return None,
    };
    let types: Vec<String> = req
        .params
        .get("types")
        .and_then(|t| serde_json::from_value(t.clone()).ok())
        .unwrap_or_default();
    if add {
        for t in types {
            if !subs.contains(&t) {
                subs.push(t);
            }
        }
    } else {
        subs.retain(|s| !types.contains(s));
    }
    let id = req.id.clone()?;
    Some(Some(
        RpcResponse::ok(id, serde_json::json!({"subscribed": subs})).to_line(),
    ))
}

/// Dispatch one request; `None` for a notification. The delivery an
/// `agent.wait` result carries is confirmed by the writer after the
/// response line is flushed (§4.8: the clear follows the written result).
///
/// A notification of a method that answers on write is **not run**: its
/// result would go nowhere, so popping the mailbox for it would lose the
/// answer behind a `Delivered{ok:true}` the client never saw, and parking
/// a hold for it would raise a card on the deck that no CLI is waiting on.
/// Refused here, the answer stays in the mailbox for a real `agent.wait`
/// and the card lives out its own 4 s poll budget (§4.8).
async fn answer(ctx: &ConnCtx, req: RpcRequest) -> Option<(RpcResponse, Option<Delivery>)> {
    if req.id.is_none() && answers_on_write(&req.method) {
        tracing::debug!(method = %req.method, "notification form not run: nothing would be written");
        return None;
    }
    let reply = dispatch(ctx, &req.method, req.params).await;
    let Some(id) = req.id else {
        if let Some(Delivery { agent, card }) = reply.after_write {
            // Nothing was written: the honest report is a failure.
            ctx.core.delivered(agent, card, false).await;
        }
        return None;
    };
    Some((
        RpcResponse::from_result(id, reply.result),
        reply.after_write,
    ))
}

/// A bus event as this session should see it, or nothing.
fn published_line(
    ctx: &ConnCtx,
    subs: &[String],
    published: &Published,
    own: bool,
) -> Option<String> {
    event_for(ctx, subs, &published.event, own)
}

fn event_for(
    ctx: &ConnCtx,
    subs: &[String],
    event: &deck_model::Event,
    own: bool,
) -> Option<String> {
    if !matches_any(subs, event.kind) {
        return None;
    }
    let redacted = redact_event(event, &ctx.seen_as(), own)?;
    Some(rpc::event_line(&redacted))
}

/// Lines from any async reader: `\n`-terminated, at most `LINE_MAX`
/// bytes (a longer one is `ConnError::TooLarge` and ends the stream).
pub fn line_stream<R>(reader: R) -> LineStream
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncBufReadExt;
    let reader = tokio::io::BufReader::new(reader);
    Box::pin(futures_util::stream::unfold(
        Some(reader),
        |state| async move {
            let mut reader = state?;
            let mut buf = Vec::new();
            let limit = (LINE_MAX + 1) as u64;
            let n = {
                let mut limited = tokio::io::AsyncReadExt::take(&mut reader, limit);
                match limited.read_until(b'\n', &mut buf).await {
                    Ok(n) => n,
                    Err(e) => return Some((Err(ConnError::Io(e)), None)),
                }
            };
            if n == 0 {
                return None;
            }
            if buf.last() != Some(&b'\n') && buf.len() as u64 >= limit {
                return Some((Err(ConnError::TooLarge), None));
            }
            if buf.last() == Some(&b'\n') {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            }
            match String::from_utf8(buf) {
                Ok(line) => Some((Ok(line), Some(reader))),
                Err(_) => Some((
                    Err(ConnError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "not utf-8",
                    ))),
                    None,
                )),
            }
        },
    ))
}

/// A sink that writes each line with `\n` and flushes.
pub fn line_sink<W>(writer: W) -> LineSink
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::AsyncWriteExt;
    Box::pin(futures_util::sink::unfold(
        writer,
        |mut writer, line: String| async move {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            Ok::<_, ConnError>(writer)
        },
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::testkit::{test_api, TestDaemon};
    use crate::trust::FakeVerifier;
    use deck_model::EventKind;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    type Reader = BufReader<tokio::io::ReadHalf<DuplexStream>>;
    type Writer = tokio::io::WriteHalf<DuplexStream>;

    /// An in-memory connection to `conn::run` as a UDS peer would see it.
    pub fn connect(daemon: &TestDaemon, verdict: PeerVerdict) -> (Reader, Writer) {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_r, server_w) = tokio::io::split(server_side);
        tokio::spawn(run(
            daemon.api.clone(),
            Peer {
                door: Door::Uds,
                principal: Some((Principal::Uid(501), vec![])),
                verdict,
            },
            line_stream(server_r),
            line_sink(server_w),
            daemon.stop.subscribe(),
        ));
        let (client_r, client_w) = tokio::io::split(client_side);
        (BufReader::new(client_r), client_w)
    }

    pub async fn read_line(reader: &mut Reader) -> serde_json::Value {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            reader.read_line(&mut line),
        )
        .await
        .expect("a line within 3 s")
        .unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("not json: {line:?}"))
    }

    #[tokio::test]
    async fn hello_first_then_ping_subscribe_and_redacted_events() {
        let daemon = test_api("conn-hello", FakeVerifier::default());
        let (mut r, mut w) = connect(&daemon, PeerVerdict::default());
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session.hello\",\"params\":{\"client\":{\"name\":\"t\",\"version\":\"1\"},\"scopes\":[\"observe\",\"agent\"],\"subscribe\":[\"card.*\",\"agent.added\"],\"snapshot\":true}}\n").await.unwrap();
        let hello = read_line(&mut r).await;
        assert_eq!(hello["id"], 1);
        assert_eq!(hello["result"]["api"], "1");
        assert_eq!(hello["result"]["principal"]["id"], "uid:501");
        assert_eq!(
            hello["result"]["principal"]["scopes"],
            serde_json::json!(["observe", "agent"])
        );
        assert_eq!(hello["result"]["principal"]["trusted"], false);
        assert_eq!(hello["result"]["session"], "sess_2");
        assert!(hello["result"]["snapshot"]["agents"].is_object());
        assert!(hello["result"].get("replayed").is_none());
        // ping, subscribe, an unknown method, a notification (no answer), a batch.
        w.write_all(b"{\"id\":2,\"method\":\"session.ping\"}\n")
            .await
            .unwrap();
        assert!(read_line(&mut r).await["result"]["ts"].is_string());
        w.write_all(
            b"{\"id\":3,\"method\":\"session.subscribe\",\"params\":{\"types\":[\"error\"]}}\n",
        )
        .await
        .unwrap();
        assert_eq!(
            read_line(&mut r).await["result"]["subscribed"],
            serde_json::json!(["card.*", "agent.added", "error"])
        );
        w.write_all(b"{\"method\":\"session.ping\"}\n{\"id\":4,\"method\":\"nope\"}\n")
            .await
            .unwrap();
        let resp = read_line(&mut r).await;
        assert_eq!(resp["id"], 4);
        assert_eq!(resp["error"]["code"], ApiError::METHOD_NOT_FOUND);
        w.write_all(
            b"[{\"id\":5,\"method\":\"session.ping\"},{\"id\":6,\"method\":\"state.get\"}]\n",
        )
        .await
        .unwrap();
        let batch = read_line(&mut r).await;
        let items = batch.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .any(|i| i["id"] == 5 && i["result"]["ts"].is_string()));
        assert!(items
            .iter()
            .any(|i| i["id"] == 6 && i["result"]["rev"].is_number()));
        // Register an agent from this session: the agent.added event comes
        // back with content (own agent) and the reply carries our session.
        w.write_all(br#"{"id":7,"method":"agent.register","params":{"kind":"other","name":"bot","key":"k","lease_ttl_s":30,"caps":{"answer":"poll","order":"poll","focus":false}}}"#).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        let mut got_reply = None;
        let mut got_added = None;
        for _ in 0..2 {
            let line = read_line(&mut r).await;
            if line["id"] == 7 {
                got_reply = Some(line);
            } else if line["method"] == "event" {
                got_added = Some(line);
            }
        }
        let reply = got_reply.expect("the register reply");
        assert_eq!(reply["result"]["owner"]["session"], "sess_2");
        let added = got_added.expect("the agent.added event");
        assert_eq!(added["params"]["type"], "agent.added");
        assert_eq!(added["params"]["data"]["agent"]["name"], "bot");
        assert_eq!(added["params"]["data"]["agent"].get("redacted"), None);
        // Another session sets a card on its own agent: we see card.set redacted.
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
                deck_model::Request::AgentRegister(crate::core::testkit::register(
                    "theirs",
                    Some("k2"),
                )),
            )
            .await
            .unwrap();
        let theirs: deck_model::AgentId = serde_json::from_value(theirs["agent"].clone()).unwrap();
        // agent.added for theirs is subscribed: it arrives redacted (not our agent).
        let line = read_line(&mut r).await;
        assert_eq!(line["params"]["type"], "agent.added");
        assert_eq!(line["params"]["data"]["agent"]["redacted"], true);
        daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                other,
                false,
                deck_model::Request::CardSet(crate::core::testkit::card(&theirs, "secret?")),
            )
            .await
            .unwrap();
        // card.set (subscribed) redacted; the agent.changed in between is not subscribed.
        let line = read_line(&mut r).await;
        assert_eq!(line["params"]["type"], "card.set", "{line}");
        assert_eq!(
            line["params"]["data"]["card"]["text"],
            serde_json::Value::Null
        );
        assert_eq!(line["params"]["data"]["card"]["redacted"], true);
        assert_eq!(line["params"]["data"]["redacted"], true);
        // A second hello is refused as an invalid request; an oversized line closes.
        w.write_all(b"{\"id\":8,\"method\":\"session.hello\",\"params\":{}}\n")
            .await
            .unwrap();
        assert_eq!(
            read_line(&mut r).await["error"]["code"],
            ApiError::INVALID_REQUEST
        );
        let big = vec![b'x'; LINE_MAX + 10];
        w.write_all(&big).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        let resp = read_line(&mut r).await;
        assert_eq!(resp["error"]["code"], ApiError::TOO_LARGE);
        let mut rest = String::new();
        assert_eq!(
            r.read_line(&mut rest).await.unwrap(),
            0,
            "closed after -32012"
        );
        // The session is closed on the core: its agent is orphaned.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let snap = daemon.api.bus.snapshot();
            let orphaned = snap
                .agents
                .values()
                .any(|a| a.name == "bot" && a.state == deck_model::AgentState::Idle);
            if orphaned || std::time::Instant::now() > deadline {
                assert!(orphaned);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let _ = EventKind::Error;
    }

    /// Everything one session is shown, from its hello on: the hello reply,
    /// the replay of `since: 0`, a `state.get` and a `state.events`, then
    /// the events that a live reply (by `agent`'s `owner`) and a heard
    /// transcript raise, up to a marker event published last. The session
    /// asks for what the companion app asks for (`admin` among it), and
    /// `content` is what its hello says, or nothing.
    async fn shown(
        daemon: &TestDaemon,
        agent: &deck_model::AgentId,
        owner: deck_model::SessionId,
        content: Option<bool>,
    ) -> String {
        let (mut r, mut w) = connect(daemon, PeerVerdict::default());
        let mut params = serde_json::json!({
            "client": {"name": "OpenWalkieTalkie", "version": "1"},
            "scopes": ["observe", "voice", "device", "admin"],
            "subscribe": ["*"],
            "snapshot": true,
            "since": 0,
        });
        if let Some(content) = content {
            params["content"] = content.into();
        }
        let hello = serde_json::json!({"id": 1, "method": "session.hello", "params": params});
        w.write_all(format!("{hello}\n").as_bytes()).await.unwrap();
        let reply = read_line(&mut r).await;
        assert_eq!(reply["result"]["api"], "1", "{reply}");
        let mut lines = vec![reply.to_string()];
        for _ in 0..reply["result"]["replayed"].as_u64().expect("a replay") {
            lines.push(read_line(&mut r).await.to_string());
        }
        w.write_all(b"{\"id\":2,\"method\":\"state.get\"}\n")
            .await
            .unwrap();
        w.write_all(b"{\"id\":3,\"method\":\"state.events\",\"params\":{\"since\":0}}\n")
            .await
            .unwrap();
        let replied = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                owner,
                false,
                deck_model::Request::Reply(deck_model::ReplySet {
                    agent: agent.clone(),
                    text: "REPLY-SECRET again".into(),
                    message: None,
                    speak: false,
                    announce: None,
                    lane: None,
                }),
            )
            .await
            .unwrap();
        // The core publishes the reply's events just after it answers: wait
        // for them, so the marker below comes after them on the bus.
        let rev = replied["rev"].as_u64().expect("the reply's rev");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while daemon.api.bus.rev() < rev {
            assert!(std::time::Instant::now() < deadline, "the reply never reached the bus");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        for (kind, data) in [
            (
                EventKind::TranscriptFinal,
                serde_json::json!({"utterance": "u1", "text": "HEARD-SECRET"}),
            ),
            (EventKind::Error, serde_json::json!({"code": "marker"})),
        ] {
            daemon.api.bus.publish(Published {
                event: Arc::new(deck_model::Event {
                    rev: daemon.api.bus.rev(),
                    ts: crate::core::wall_now(),
                    kind,
                    principal: Principal::Device,
                    data,
                }),
                owner: None,
            });
        }
        let (mut answered, mut marked) = (0, false);
        while answered < 2 || !marked {
            let line = read_line(&mut r).await;
            answered += usize::from(line["id"] == 2 || line["id"] == 3);
            marked |= line["params"]["data"]["code"] == "marker";
            lines.push(line.to_string());
        }
        lines.join("\n")
    }

    /// R2: `session.hello{content:false}` shows a session no content,
    /// whatever its scopes grant. The companion app holds `admin`, which
    /// grants content: with the flag no title, card, reply or transcript
    /// text reaches it — not in the hello snapshot, the replay, `state.get`,
    /// `state.events` or a live event — and the hello says it was honoured.
    /// The same hello without the flag is shown all of it, as before.
    #[tokio::test]
    async fn a_hello_without_content_is_shown_no_session_text_whatever_its_scopes() {
        const SECRETS: [&str; 4] = ["TITLE-SECRET", "CARD-SECRET", "REPLY-SECRET", "HEARD-SECRET"];
        let daemon = test_api("conn-no-content", FakeVerifier::default());
        // Another client's agent: a title from its prompt, a question on
        // the deck and a reply.
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
        let call = |req| daemon.api.core.api(Principal::Uid(501), other, false, req);
        let registered = call(deck_model::Request::AgentRegister(
            crate::core::testkit::register("api", Some("k")),
        ))
        .await
        .unwrap();
        let agent: deck_model::AgentId =
            serde_json::from_value(registered["agent"].clone()).unwrap();
        call(deck_model::Request::AgentUpdate(deck_model::AgentUpdate {
            agent: agent.clone(),
            state: None,
            title: Some("TITLE-SECRET".into()),
            title_source: Some(deck_model::TitleSource::Prompt),
            since: None,
            name: None,
            meta: None,
            task: None,
            subagents: None,
        }))
        .await
        .unwrap();
        call(deck_model::Request::CardSet(crate::core::testkit::card(
            &agent,
            "CARD-SECRET?",
        )))
        .await
        .unwrap();
        call(deck_model::Request::Reply(deck_model::ReplySet {
            agent: agent.clone(),
            text: "REPLY-SECRET".into(),
            message: None,
            speak: false,
            announce: None,
            lane: None,
        }))
        .await
        .unwrap();

        let blind = shown(&daemon, &agent, other, Some(false)).await;
        for secret in SECRETS {
            assert!(!blind.contains(secret), "{secret} reached it: {blind}");
        }
        assert!(blind.contains("\"content\":false"), "honoured, and said so");
        assert!(blind.contains("\"redacted\":true"), "{blind}");
        assert!(blind.contains("\"name\":\"api\""), "state still shows: {blind}");

        let seeing = shown(&daemon, &agent, other, None).await;
        for secret in SECRETS {
            assert!(seeing.contains(secret), "{secret}: the default changed: {seeing}");
        }
        assert!(!seeing.contains("\"content\":false"), "{seeing}");
        let explicit = shown(&daemon, &agent, other, Some(true)).await;
        assert!(SECRETS.iter().all(|secret| explicit.contains(secret)));
    }

    /// JSON-RPC 2.0 on the line door: a malformed member is answered in
    /// place (`id:null`, `-32600`) and the members beside it still run.
    #[tokio::test]
    async fn a_malformed_batch_member_is_answered_in_place() {
        let daemon = test_api("conn-bad-member", FakeVerifier::default());
        let (mut r, mut w) = connect(&daemon, PeerVerdict::default());
        w.write_all(b"{\"id\":1,\"method\":\"session.hello\",\"params\":{}}\n")
            .await
            .unwrap();
        assert_eq!(read_line(&mut r).await["result"]["api"], "1");
        w.write_all(br#"[{"id":2,"method":"session.ping"},{"id":3},{"method":"session.ping"},42]"#)
            .await
            .unwrap();
        w.write_all(b"\n").await.unwrap();
        let batch = read_line(&mut r).await;
        let items = batch.as_array().unwrap_or_else(|| panic!("{batch}"));
        assert_eq!(
            items.len(),
            3,
            "the notification alone is unanswered: {batch}"
        );
        assert!(
            items
                .iter()
                .any(|i| i["id"] == 2 && i["result"]["ts"].is_string()),
            "{batch}"
        );
        let bad: Vec<&serde_json::Value> = items
            .iter()
            .filter(|i| i["error"]["code"] == ApiError::INVALID_REQUEST)
            .collect();
        assert_eq!(bad.len(), 2, "{batch}");
        assert!(bad.iter().all(|i| i["id"] == serde_json::Value::Null));
    }

    /// §4.8 `poll`: the card is cleared "when the `agent.wait` response is
    /// written". A notification-form `agent.wait` is never written, so it
    /// must not pop the session's mailbox — the answer waits there for a
    /// real `agent.wait`, and the card is never reported `delivered`.
    #[tokio::test]
    async fn a_notification_agent_wait_never_consumes_the_mailbox() {
        let daemon = test_api("conn-note-wait", FakeVerifier::default());
        let (mut r, mut w) = connect(&daemon, PeerVerdict::default());
        w.write_all(br#"{"id":1,"method":"session.hello","params":{"client":{"name":"t","version":"1"},"scopes":["observe","agent"]}}"#).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        let hello = read_line(&mut r).await;
        let session: deck_model::SessionId =
            serde_json::from_value(hello["result"]["session"].clone()).unwrap();
        // Our own poll agent, with a poll card the operator answers: with
        // no waiter parked, the answer lands in this session's mailbox.
        let registered = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                deck_model::Request::AgentRegister(crate::core::testkit::register(
                    "bot",
                    Some("k"),
                )),
            )
            .await
            .unwrap();
        let agent: deck_model::AgentId =
            serde_json::from_value(registered["agent"].clone()).unwrap();
        let set = daemon
            .api
            .core
            .api(
                Principal::Uid(501),
                session,
                false,
                deck_model::Request::CardSet(crate::core::testkit::card(&agent, "go?")),
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
                deck_model::Request::OpAnswer(deck_model::OpAnswer {
                    agent: agent.clone(),
                    card,
                    choice: Some(0),
                    text: None,
                }),
            )
            .await
            .unwrap();
        // The notification form: no response, and nothing consumed.
        w.write_all(
            format!(
                r#"{{"method":"agent.wait","params":{{"agent":"{}"}}}}"#,
                agent.0
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        w.write_all(b"\n").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let card_after = daemon
            .api
            .bus
            .snapshot()
            .agents
            .get(&agent)
            .and_then(|a| a.card.clone());
        assert!(
            card_after.is_some(),
            "the card was cleared for a response nobody received"
        );
        // The real wait still finds the answer.
        w.write_all(
            format!(
                r#"{{"id":2,"method":"agent.wait","params":{{"agent":"{}","timeout_s":2}}}}"#,
                agent.0
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        w.write_all(b"\n").await.unwrap();
        let waited = read_line(&mut r).await;
        assert_eq!(waited["id"], 2);
        assert_eq!(waited["result"]["kind"], "answer", "{waited}");
        assert_eq!(waited["result"]["choice"], 0, "{waited}");
        assert_eq!(waited["result"]["card"], card.0);
    }

    #[tokio::test]
    async fn a_non_hello_first_line_is_refused_with_hello_first_and_closed() {
        let daemon = test_api("conn-first", FakeVerifier::default());
        for (first, expect_code, expect_why) in [
            (
                "{\"id\":1,\"method\":\"session.ping\"}\n",
                ApiError::UNAUTHORIZED,
                Some("hello_first"),
            ),
            (
                "[{\"id\":1,\"method\":\"session.hello\"}]\n",
                ApiError::UNAUTHORIZED,
                Some("hello_first"),
            ),
            ("GET /v1/info HTTP/1.1\r\n", ApiError::PARSE, None),
        ] {
            let (mut r, mut w) = connect(&daemon, PeerVerdict::default());
            w.write_all(first.as_bytes()).await.unwrap();
            let resp = read_line(&mut r).await;
            assert_eq!(resp["error"]["code"], expect_code, "{first:?}");
            if let Some(why) = expect_why {
                assert_eq!(resp["error"]["data"]["why"], why);
            }
            let mut rest = String::new();
            assert_eq!(
                r.read_line(&mut rest).await.unwrap(),
                0,
                "closed after the refusal"
            );
        }
        // Asking for more than the maximum is refused at the hello, explicitly.
        let (mut r, mut w) = connect(
            &daemon,
            PeerVerdict {
                trusted: false,
                ancestry_capped: true,
                exe: None,
            },
        );
        w.write_all(
            b"{\"id\":1,\"method\":\"session.hello\",\"params\":{\"scopes\":[\"operate\"]}}\n",
        )
        .await
        .unwrap();
        let resp = read_line(&mut r).await;
        assert_eq!(resp["error"]["code"], ApiError::FORBIDDEN);
        assert_eq!(resp["error"]["data"]["reason"], "agent_ancestry");
    }
}
