//! The GUI's one connection to `deckd` (§8.1).
//!
//! **The UDS only.** An unsandboxed app always has the socket, so there is no
//! WS fallback and no token on disk — §4.2's "no plaintext all-scope token on
//! disk" is kept by there being nothing to keep. The hello asks for the three
//! scopes of the companion ([`scopes`]) and names the high-volume types `*`
//! will not match (§4.5), and the reply says whether this build was verified
//! as §4.2's trusted principal: `false` on an ad-hoc dev build without
//! `[trust] dev_requirement`, which is not an error.
//!
//! Daemon → GUI is [`crate::events::EventSender::post`]. GUI → daemon is
//! [`ApiClient::call`], which is **not** async: it pushes the call onto a
//! `tokio::sync::mpsc` the connection task drains and hands back the
//! `oneshot::Receiver` for a view to await on gpui's own executor. A UI thread
//! that never awaits a socket never needs a runtime, which is the whole
//! reason the shape is this way round.
use crate::events::{ApiEvent, AppEvent, EventSender};
use crate::host_state::Backoff;
use crate::TokioBridge;
use deck_client::{Client, ClientConfig, ClientError, ClientEvent, Transport};
use deck_model::Scope;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// `client.name` in the hello (§8.1), and what a logical HTTP session would
/// be keyed on if this app ever used one.
pub const CLIENT_NAME: &str = "OpenWalkieTalkie";

/// How long a connection has to last to count as "a connection that came
/// up" (§8.1's reset condition). A daemon that answers the hello and drops
/// the socket in the same breath is crash looping, and a back-off that reset
/// on every *open* dialled it at the 250 ms floor for ever — 15 full hellos
/// (each with the whole snapshot) in 4 s, measured.
pub const HEALTHY: std::time::Duration = deck_client::BACKOFF_MAX;

/// Calls queued before `call` starts refusing. The GUI makes one call per
/// click; a queue this deep only fills if the daemon has stopped draining,
/// and then failing fast is the honest answer.
pub const CALL_QUEUE: usize = 256;

/// The JSON-RPC `{method, params}` a [`ApiClient::call`] queued. §8.1's
/// `(Request, oneshot::Sender<Result<Value>>)`; it is not `deck_model::Request`
/// because the GUI also calls `state.get`, `config.*` and `integrations.*`,
/// which never reach the reducer and so are not in that enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub params: Value,
}

/// A queued call and where its reply goes.
type Queued = (Request, oneshot::Sender<Result<Value, ClientError>>);

/// What the companion asks for: `voice` and `device` for the deck and its
/// voice, `admin` for the Integrations and Service pages. The window is
/// never a session's, so neither `observe:content` (card text, replies,
/// transcripts) nor `operate` (answers, orders, focus) is asked for. To the
/// service `admin` still stands for every scope but `observe:raw` (content
/// included, `Scope::grants_content`), so the list says what the window
/// uses rather than narrowing what it is sent; the mirror is what drops the
/// sessions a snapshot carries ([`crate::HostState`]). `observe:raw` is
/// **not** asked for either: the GUI shows no raw hook payloads, and §4.2
/// says it stays off unless ticked deliberately.
pub fn scopes() -> Vec<Scope> {
    vec![Scope::Voice, Scope::Device, Scope::Admin]
}

/// The subscription of §8.1, kept whole although the window folds far less
/// of it than it once did: every event bumps `rev`, so every kind with a
/// producer must reach the mirror for a jump in `rev` to keep meaning a
/// hole. `*` deliberately does not match the high-volume types (§4.5), so
/// they are named: `transcript.partial`, `voice.level`, `device.key`,
/// `device.macro` and `op.confirm_requested`.
pub fn subscribe() -> Vec<String> {
    // Every high-volume kind is named one by one, and `"transcript.*"` would
    // not do: the bus keeps high-volume kinds out of a prefix glob as well as
    // out of `"*"` (§4.5 — they must be *named*), so a glob here would deliver
    // no partial at all and the live dictation would never populate. The two
    // `device.*` kinds are named for a second reason: `Store::emit` bumps `rev`
    // for every event, including the ones `"*"` filters out, so an un-named
    // kind that has a producer leaves a hole the reducer must answer with a
    // `state.get` — one per deck key press. `device.raw`/`hook.raw` are absent
    // deliberately: nothing emits them in v1 and they would need `observe:raw`.
    vec![
        "*".into(),
        "transcript.partial".into(),
        "voice.level".into(),
        "device.key".into(),
        "device.macro".into(),
        "op.confirm_requested".into(),
    ]
}

/// The hello this app sends, over `socket`.
pub fn config(socket: PathBuf) -> ClientConfig {
    ClientConfig {
        transport: Transport::Uds(socket),
        client_name: CLIENT_NAME.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        scopes: scopes(),
        subscribe: subscribe(),
        snapshot: true,
        // This crate owns the reconnect: the back-off has to be the same one
        // the window counts down (`host_state::Backoff`), and every attempt
        // has to post a `Connected`/`Disconnected` the mirror can fold.
        reconnect: false,
        // The window shows counts and times, never a session (decision 1):
        // the daemon is asked not to send session text at all, rather than
        // the mirror dropping it on arrival. An older daemon ignores the
        // field, and the mirror still drops it then.
        content: Some(false),
    }
}

/// The default socket (`~/Library/Application Support/OpenWalkieTalkie/deckd.sock`,
/// `DECKD_DATA_DIR` honoured) — the same rule as `deckd` and `deckd ctl`.
pub fn default_socket() -> PathBuf {
    deck_client::default_socket()
}

/// What the connection task publishes and `ApiClient` reads.
#[derive(Debug, Default)]
struct Shared {
    connected: AtomicBool,
    /// §4.2's verified GUI. Learned from every hello; a reconnect can change
    /// it (a rebuilt app with a different signature).
    trusted: AtomicBool,
    /// The newest rev seen, which the next hello asks `since`.
    rev: AtomicU64,
    /// The `ApiClient` was dropped: stop reconnecting.
    closed: AtomicBool,
}

/// One long-lived session to the daemon.
pub struct ApiClient {
    calls: mpsc::Sender<Queued>,
    shared: Arc<Shared>,
    socket: PathBuf,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("socket", &self.socket)
            .field("connected", &self.connected())
            .field("trusted", &self.trusted())
            .finish()
    }
}

/// Where a call's reply lands.
///
/// It is a newtype and not the `oneshot::Receiver` itself for one reason:
/// a receiver carries `blocking_recv()`, which parks the calling thread, and
/// the one thread that must never park is the one drawing the window (§8.1).
/// `Reply` implements `Future` and exposes nothing else, so the rule is the
/// compiler's rather than a grep's — a view can `.await` it and cannot block
/// on it. The `RecvError` case is folded into `Disconnected`: a dropped sender
/// means the connection task is gone, which is the same thing to a caller.
pub struct Reply(oneshot::Receiver<Result<Value, ClientError>>);

impl std::future::Future for Reply {
    type Output = Result<Value, ClientError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0)
            .poll(cx)
            .map(|r| r.unwrap_or(Err(ClientError::Disconnected)))
    }
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply")
    }
}

impl ApiClient {
    /// Start the connection task on `bridge`; it posts every `ApiEvent` to
    /// `bus` and reconnects on its own until this `ApiClient` is dropped.
    pub fn start(bridge: &TokioBridge, bus: EventSender, config: ClientConfig) -> ApiClient {
        let socket = match &config.transport {
            Transport::Uds(path) => path.clone(),
            // `config()` is the only supported constructor and it is UDS.
            Transport::Ws { .. } => PathBuf::new(),
        };
        let (calls, queue) = mpsc::channel(CALL_QUEUE);
        let shared = Arc::new(Shared::default());
        bridge.spawn(connect_loop(config, bus, shared.clone(), queue));
        ApiClient {
            calls,
            shared,
            socket,
        }
    }

    /// Queue a call and hand back where its reply will land. Never blocks and
    /// never awaits: a full queue or a dead connection task resolves the
    /// reply with `ClientError::Disconnected` straight away, so a view
    /// awaiting it always gets an answer.
    pub fn call(&self, method: &str, params: Value) -> Reply {
        let (reply, rx) = oneshot::channel();
        let request = Request {
            method: method.to_string(),
            params,
        };
        if let Err(e) = self.calls.try_send((request, reply)) {
            let (_, reply) = e.into_inner();
            let _ = reply.send(Err(ClientError::Disconnected));
        }
        Reply(rx)
    }

    /// The socket is up.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// Whether the last hello was answered `principal.trusted:true` (§4.2).
    pub fn trusted(&self) -> bool {
        self.shared.trusted.load(Ordering::SeqCst)
    }

    /// The rev the next reconnect will ask `since`.
    pub fn rev(&self) -> u64 {
        self.shared.rev.load(Ordering::Relaxed)
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }
}

impl Drop for ApiClient {
    fn drop(&mut self) {
        // The task sees the closed flag, or the queue's sender going away.
        self.shared.closed.store(true, Ordering::SeqCst);
    }
}

/// Why a connection's service loop returned.
enum Stop {
    /// The socket ended: reconnect.
    Disconnected,
    /// The `ApiClient` is gone: stop.
    Closed,
}

/// §8.1's reconnect: connect, hello, serve, and on the way down wait
/// 250 ms doubling to 5 s. Every attempt that ends or fails posts exactly one
/// `Disconnected`, so the mirror's own back-off (`host_state::Backoff`)
/// steps in lockstep with this one and the window's countdown is the truth.
async fn connect_loop(
    config: ClientConfig,
    bus: EventSender,
    shared: Arc<Shared>,
    mut queue: mpsc::Receiver<Queued>,
) {
    let mut backoff = Backoff::new();
    loop {
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        let since = match shared.rev.load(Ordering::Relaxed) {
            0 => None,
            rev => Some(rev),
        };
        let opened = Client::connect_since(config.clone(), since).await;
        let stop = match opened {
            Ok((client, _replay)) => {
                let up = std::time::Instant::now();
                let hello = client.hello().clone();
                shared.trusted.store(hello.trusted, Ordering::SeqCst);
                shared.rev.fetch_max(hello.rev, Ordering::Relaxed);
                shared.connected.store(true, Ordering::SeqCst);
                // `snapshot:true` is in the hello, so this is the daemon's
                // own snapshot; a daemon that answered without one leaves the
                // mirror at `default()` until the first `state.get`.
                let snapshot = hello.snapshot.unwrap_or_default();
                bus.post(AppEvent::Api(ApiEvent::Connected { snapshot }));
                let stop = serve(Arc::new(client), &bus, &shared, &mut queue).await;
                // Reset here and not at the open: see `HEALTHY`.
                if up.elapsed() >= HEALTHY {
                    backoff.reset();
                }
                shared.connected.store(false, Ordering::SeqCst);
                bus.post(AppEvent::Api(ApiEvent::Disconnected));
                stop
            }
            Err(_) => {
                // The daemon is not running (or refused the hello): the
                // window's yellow bar is the same either way.
                shared.connected.store(false, Ordering::SeqCst);
                bus.post(AppEvent::Api(ApiEvent::Disconnected));
                Stop::Disconnected
            }
        };
        if matches!(stop, Stop::Closed) || shared.closed.load(Ordering::SeqCst) {
            return;
        }
        if matches!(
            wait_failing_calls(backoff.wait(), &mut queue).await,
            Stop::Closed
        ) {
            return;
        }
    }
}

/// One connection: pump the daemon's events onto the bus and the queued
/// calls onto the socket, until one of the two ends.
async fn serve(
    client: Arc<Client>,
    bus: &EventSender,
    shared: &Arc<Shared>,
    queue: &mut mpsc::Receiver<Queued>,
) -> Stop {
    let mut events = client.events();
    loop {
        tokio::select! {
            item = events.recv() => match item {
                Ok(ClientEvent::Event(event)) => {
                    shared.rev.fetch_max(event.rev, Ordering::Relaxed);
                    bus.post(AppEvent::Api(ApiEvent::Event(event)));
                }
                Ok(ClientEvent::Resync { rev }) => {
                    shared.rev.fetch_max(rev, Ordering::Relaxed);
                    bus.post(AppEvent::Api(ApiEvent::Resync));
                }
                Ok(ClientEvent::Disconnected) => return Stop::Disconnected,
                // `reconnect:false`, so the client never makes one itself.
                Ok(ClientEvent::Reconnected { .. }) => {}
                // We fell behind our own ring: the same answer the daemon's
                // `resync` gets, because the mirror is stale either way.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    bus.post(AppEvent::Api(ApiEvent::Resync));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Stop::Disconnected
                }
            },
            queued = queue.recv() => match queued {
                Some((request, reply)) => {
                    // Spawned, not awaited: a slow `state.get` must not
                    // stall the event pump behind it.
                    let client = client.clone();
                    tokio::spawn(async move {
                        let result = client.call(&request.method, request.params).await;
                        let _ = reply.send(result);
                    });
                }
                None => return Stop::Closed,
            },
        }
    }
}

/// Wait out the back-off, failing anything the operator clicks meanwhile. A
/// click while the daemon is down has to fail now: silently firing it minutes
/// later, at whatever the deck has become, is worse than an error.
async fn wait_failing_calls(wait: std::time::Duration, queue: &mut mpsc::Receiver<Queued>) -> Stop {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Stop::Disconnected,
            queued = queue.recv() => match queued {
                Some((_, reply)) => {
                    let _ = reply.send(Err(ClientError::Disconnected));
                }
                None => return Stop::Closed,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hello_is_the_uds_only_with_three_scopes_and_the_named_types() {
        let config = config(PathBuf::from("/tmp/deckd-gui-core.sock"));
        assert!(matches!(config.transport, Transport::Uds(_)));
        assert_eq!(config.client_name, "OpenWalkieTalkie");
        assert!(config.snapshot);
        assert!(!config.reconnect, "this crate owns the reconnect");
        assert_eq!(
            config.scopes,
            vec![Scope::Voice, Scope::Device, Scope::Admin]
        );
        // Never a session: no card text to read and no card to answer.
        for session in [Scope::ObserveContent, Scope::Operate, Scope::Agent] {
            assert!(!config.scopes.contains(&session), "{session:?}");
        }
        // And the daemon is asked to send no session text at all, although
        // admin would grant it.
        assert_eq!(config.content, Some(false));
        assert!(
            !config.scopes.contains(&Scope::ObserveRaw),
            "§4.2: raw hook payloads are not the GUI's"
        );
        // The subscription is the whole stream it always was: `rev` counts
        // every event, so an unsubscribed kind would be a hole.
        assert_eq!(
            config.subscribe,
            vec![
                "*",
                "transcript.partial",
                "voice.level",
                "device.key",
                "device.macro",
                "op.confirm_requested"
            ]
        );
        assert!(
            !config.subscribe.iter().any(|s| s.ends_with(".*")),
            "a glob cannot carry a high-volume kind (§4.5): they must be named"
        );
        // §8.1: no WS fallback and no token anywhere in the hello.
        let json = serde_json::to_string(&config.scopes).unwrap();
        assert!(!json.contains("token"));
    }
}
