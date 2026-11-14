//! Typed async client for the deckd API (design §3, contract §3.5):
//! newline-delimited JSON-RPC over the Unix socket or JSON text frames
//! over `GET /v1/ws`, `session.hello` first, calls matched to responses by
//! id, `event`/`resync` notifications as a broadcast, auto-reconnect with
//! backoff and `since: last rev`. Used by `deckd ctl/events/status --live`
//! and, later, the GUI. On Windows the "Unix socket" is the daemon's named
//! pipe (`windows`); `Transport::Uds` names either.
pub mod transport;
#[cfg(windows)]
pub mod windows;

use deck_model::{ApiError, Event, Scope, SessionId, Snapshot};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use transport::{LineReader, LineWriter, Lines};

/// The reconnect backoff: doubles from the first to the second (§3.5).
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A hello that is not answered within this is a failed attempt (keeps
/// the reconnect loop alive against a wedged peer).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Events buffered per subscriber before it lags (the bus's depth).
pub const EVENTS_CAPACITY: usize = 4096;

/// How to reach the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Uds(PathBuf),
    Ws { url: String, token: String },
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub transport: Transport,
    pub client_name: String,
    pub client_version: String,
    /// Empty: the daemon's default for the door.
    pub scopes: Vec<Scope>,
    /// Event type globs (`"card.*"`); high-volume types must be named.
    pub subscribe: Vec<String>,
    /// Ask for the snapshot in the hello.
    pub snapshot: bool,
    /// Reconnect with `since: last rev` on read error/EOF.
    pub reconnect: bool,
    /// `Some(false)`: ask the daemon to show this session no session text
    /// (titles, cards, replies, transcripts) whatever its scopes grant.
    /// `None` sends nothing, so the daemon's default applies.
    pub content: Option<bool>,
}

impl ClientConfig {
    /// Over the default socket, as `client_name`, asking for nothing special.
    pub fn uds(client_name: &str) -> Self {
        ClientConfig {
            transport: Transport::Uds(default_socket()),
            client_name: client_name.into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            scopes: Vec::new(),
            subscribe: Vec::new(),
            snapshot: false,
            reconnect: true,
            content: None,
        }
    }

    /// Over `GET /v1/ws` with a bearer token, as `client_name`.
    pub fn ws(url: &str, token: &str, client_name: &str) -> Self {
        ClientConfig {
            transport: Transport::Ws {
                url: url.into(),
                token: token.into(),
            },
            ..ClientConfig::uds(client_name)
        }
    }
}

/// What `session.hello` returned.
#[derive(Clone, Debug)]
pub struct Hello {
    pub api: String,
    pub daemon: String,
    pub session: SessionId,
    /// `"uid:501"`, `"token:gate"`.
    pub principal: String,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
    pub rev: u64,
    pub snapshot: Option<Snapshot>,
}

/// What a hello with `since` replayed (`connect_since`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Replay {
    /// Events the daemon replayed after the hello reply.
    pub replayed: u64,
    /// The ring no longer held `since`: nothing was replayed, `state.get`.
    pub gap: bool,
}

/// What `events()` delivers. A `Reconnected` carries the whole hello (the
/// snapshot included) by design: it is rare and the shape is the contract's.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ClientEvent {
    Event(Event),
    /// The daemon says we lagged: `state.get` again.
    Resync {
        rev: u64,
    },
    /// A new hello after a reconnect, with what the ring replayed.
    Reconnected {
        hello: Hello,
        replayed: u64,
        gap: bool,
    },
    Disconnected,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("rpc {code}: {message}")]
    Rpc {
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    },
    #[error("disconnected")]
    Disconnected,
    /// The hello was refused.
    #[error("refused: {0}")]
    Refused(ApiError),
}

impl ClientError {
    /// The `ApiError` of an `Rpc` or `Refused` error, if it is one.
    pub fn api_error(&self) -> Option<ApiError> {
        match self {
            ClientError::Rpc {
                code,
                message,
                data,
            } => Some(ApiError {
                code: *code,
                message: message.clone(),
                data: data.clone().unwrap_or(Value::Null),
            }),
            ClientError::Refused(e) => Some(e.clone()),
            _ => None,
        }
    }
}

/// A JSON-RPC `error` object as an `ApiError` (`data` may be absent).
fn api_error_of(error: &Value) -> ApiError {
    ApiError {
        code: error
            .get("code")
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok())
            .unwrap_or(ApiError::INTERNAL),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string(),
        data: error.get("data").cloned().unwrap_or(Value::Null),
    }
}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>;

/// What the session loop and the calls share.
struct Shared {
    writer: tokio::sync::Mutex<Option<LineWriter>>,
    pending: Mutex<Pending>,
    next_id: AtomicU64,
    /// The rev of the newest event seen (the next hello's `since`).
    last_rev: AtomicU64,
    /// The hello of the current connection.
    latest: Mutex<Hello>,
    connected: AtomicBool,
    closed: AtomicBool,
}

impl Shared {
    fn fail_pending(&self, why: impl Fn() -> ClientError) {
        let pending: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(why()));
        }
    }

    fn take_pending(&self, id: u64) -> Option<oneshot::Sender<Result<Value, ClientError>>> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
    }

    fn note_rev(&self, rev: u64) {
        self.last_rev.fetch_max(rev, Ordering::Relaxed);
    }

    /// One line from the daemon: a response (to a pending call), a batch
    /// of responses, or a notification (broadcast). Anything else is
    /// ignored (unknown fields and notifications are, by the design).
    fn on_line(&self, line: &str, events: &broadcast::Sender<ClientEvent>) {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return,
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    self.on_message(item, events);
                }
            }
            other => self.on_message(other, events),
        }
    }

    fn on_message(&self, value: Value, events: &broadcast::Sender<ClientEvent>) {
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.take_pending(id) {
                let result = match value.get("error") {
                    Some(error) if !error.is_null() => {
                        let e = api_error_of(error);
                        Err(ClientError::Rpc {
                            code: e.code,
                            message: e.message,
                            data: (!e.data.is_null()).then_some(e.data),
                        })
                    }
                    _ => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
            return;
        }
        match value.get("method").and_then(Value::as_str) {
            Some("event") => {
                if let Some(params) = value.get("params") {
                    if let Ok(event) = serde_json::from_value::<Event>(params.clone()) {
                        self.note_rev(event.rev);
                        let _ = events.send(ClientEvent::Event(event));
                    }
                }
            }
            Some("resync") => {
                let rev = value
                    .get("params")
                    .and_then(|p| p.get("rev"))
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| self.last_rev.load(Ordering::Relaxed));
                self.note_rev(rev);
                let _ = events.send(ClientEvent::Resync { rev });
            }
            _ => {}
        }
    }
}

/// A connection with its hello done: what the session loop takes over.
struct Opened {
    lines: Lines,
    hello: Hello,
    replay: Replay,
}

/// The hello of a hello reply, or why it is not one.
fn hello_of(result: &Value) -> Result<Hello, ClientError> {
    let field = |name: &str| {
        result
            .get(name)
            .ok_or_else(|| ClientError::Protocol(format!("hello reply without {name:?}")))
    };
    let principal = field("principal")?;
    let scopes: Vec<Scope> = serde_json::from_value(
        principal
            .get("scopes")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|e| ClientError::Protocol(format!("hello reply scopes: {e}")))?;
    let session: SessionId = serde_json::from_value(field("session")?.clone())
        .map_err(|e| ClientError::Protocol(format!("hello reply session: {e}")))?;
    let snapshot = match result.get("snapshot") {
        Some(Value::Null) | None => None,
        Some(v) => Some(
            serde_json::from_value::<Snapshot>(v.clone())
                .map_err(|e| ClientError::Protocol(format!("hello reply snapshot: {e}")))?,
        ),
    };
    Ok(Hello {
        api: field("api")?.as_str().unwrap_or_default().to_string(),
        daemon: field("daemon")?.as_str().unwrap_or_default().to_string(),
        session,
        principal: principal
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        scopes,
        trusted: principal
            .get("trusted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        rev: field("rev")?.as_u64().unwrap_or(0),
        snapshot,
    })
}

/// Open the transport and hello. Everything but the hello reply that
/// arrives before it (there should be nothing) is dropped.
async fn open_session(config: &ClientConfig, since: Option<u64>) -> Result<Opened, ClientError> {
    let mut lines = transport::open(&config.transport).await?;
    let mut params = serde_json::json!({
        "client": {"name": config.client_name, "version": config.client_version},
        "subscribe": config.subscribe,
        "snapshot": config.snapshot,
    });
    if !config.scopes.is_empty() {
        params["scopes"] = serde_json::to_value(&config.scopes).unwrap_or(Value::Null);
    }
    if let Some(since) = since {
        params["since"] = since.into();
    }
    if let Some(content) = config.content {
        params["content"] = content.into();
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "session.hello",
        "params": params,
    });
    use futures_util::{SinkExt, StreamExt};
    lines.writer.send(request.to_string()).await?;
    let reply = tokio::time::timeout(HELLO_TIMEOUT, async {
        loop {
            match lines.reader.next().await {
                Some(Ok(line)) => {
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(e) => return Err(ClientError::Protocol(format!("hello reply: {e}"))),
                    };
                    if value.get("id").and_then(Value::as_u64) == Some(0) {
                        return Ok(value);
                    }
                    // Not our reply (a notification before the hello?): skip.
                }
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(ClientError::Protocol(
                        "closed before the hello reply".into(),
                    ))
                }
            }
        }
    })
    .await
    .map_err(|_| ClientError::Protocol("no hello reply".into()))??;
    if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
        return Err(ClientError::Refused(api_error_of(error)));
    }
    let result = reply.get("result").cloned().unwrap_or(Value::Null);
    let hello = hello_of(&result)?;
    let replay = Replay {
        replayed: result.get("replayed").and_then(Value::as_u64).unwrap_or(0),
        gap: result.get("gap").and_then(Value::as_bool).unwrap_or(false),
    };
    Ok(Opened {
        lines,
        hello,
        replay,
    })
}

/// The session loop: read lines until the connection ends; then, when
/// configured, reconnect with backoff and `since` until closed.
async fn pump(
    config: ClientConfig,
    shared: Arc<Shared>,
    events: broadcast::Sender<ClientEvent>,
    mut reader: LineReader,
) {
    use futures_util::StreamExt;
    loop {
        while let Some(item) = reader.next().await {
            match item {
                Ok(line) => shared.on_line(&line, &events),
                Err(_) => break,
            }
        }
        shared.connected.store(false, Ordering::SeqCst);
        *shared.writer.lock().await = None;
        shared.fail_pending(|| ClientError::Disconnected);
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        let _ = events.send(ClientEvent::Disconnected);
        if !config.reconnect {
            return;
        }
        let mut backoff = BACKOFF_MIN;
        loop {
            tokio::time::sleep(backoff).await;
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            let since = shared.last_rev.load(Ordering::Relaxed);
            match open_session(&config, Some(since)).await {
                Ok(opened) => {
                    if shared.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    if opened.replay.gap {
                        // Nothing replayed: the consumer must `state.get`;
                        // the next `since` is the daemon's rev now.
                        shared.note_rev(opened.hello.rev);
                    }
                    *shared.writer.lock().await = Some(opened.lines.writer);
                    reader = opened.lines.reader;
                    *shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = opened.hello.clone();
                    shared.connected.store(true, Ordering::SeqCst);
                    let _ = events.send(ClientEvent::Reconnected {
                        hello: opened.hello,
                        replayed: opened.replay.replayed,
                        gap: opened.replay.gap,
                    });
                    break;
                }
                Err(_) => backoff = (backoff * 2).min(BACKOFF_MAX),
            }
        }
    }
}

/// One connection = one session. Dropping it ends the session.
pub struct Client {
    config: ClientConfig,
    /// The hello of the connection `connect` made.
    hello: Hello,
    events: broadcast::Sender<ClientEvent>,
    /// The receiver created before the session loop started: the first
    /// `events()` gets it, so nothing (a `since` replay, say) is missed.
    first: Mutex<Option<broadcast::Receiver<ClientEvent>>>,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("transport", &self.config.transport)
            .field("session", &self.hello.session.to_string())
            .field("connected", &self.connected())
            .finish()
    }
}

impl Client {
    /// Connect and hello; `Err` when the daemon is unreachable or refuses.
    pub async fn connect(config: ClientConfig) -> Result<Client, ClientError> {
        Client::connect_since(config, None)
            .await
            .map(|(client, _)| client)
    }

    /// Connect and hello with `since: rev`: the daemon replays the events
    /// after `rev` (they arrive on the first `events()` receiver) and
    /// `Replay` says how many, or that the ring no longer held `rev`.
    pub async fn connect_since(
        config: ClientConfig,
        since: Option<u64>,
    ) -> Result<(Client, Replay), ClientError> {
        let opened = open_session(&config, since).await?;
        let (events, first) = broadcast::channel(EVENTS_CAPACITY);
        let shared = Arc::new(Shared {
            writer: tokio::sync::Mutex::new(Some(opened.lines.writer)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            last_rev: AtomicU64::new(if opened.replay.gap || since.is_none() {
                opened.hello.rev
            } else {
                since.unwrap_or(0)
            }),
            latest: Mutex::new(opened.hello.clone()),
            connected: AtomicBool::new(true),
            closed: AtomicBool::new(false),
        });
        let task = tokio::spawn(pump(
            config.clone(),
            shared.clone(),
            events.clone(),
            opened.lines.reader,
        ));
        Ok((
            Client {
                config,
                hello: opened.hello,
                events,
                first: Mutex::new(Some(first)),
                shared,
                task,
            },
            opened.replay,
        ))
    }

    /// The hello of the connection `connect` made. A reconnect's hello is
    /// broadcast as `ClientEvent::Reconnected` and kept in `latest_hello`.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// The hello of the current connection (a reconnect replaces it).
    pub fn latest_hello(&self) -> Hello {
        self.shared
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// The transport is up (false between a drop and the reconnect).
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// The rev of the newest event seen (what the next reconnect asks `since`).
    pub fn last_rev(&self) -> u64 {
        self.shared.last_rev.load(Ordering::Relaxed)
    }

    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Disconnected);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        {
            use futures_util::SinkExt;
            let mut writer = self.shared.writer.lock().await;
            let Some(writer) = writer.as_mut() else {
                self.shared.take_pending(id);
                return Err(ClientError::Disconnected);
            };
            if let Err(e) = writer.send(line).await {
                self.shared.take_pending(id);
                return Err(e);
            }
        }
        rx.await.unwrap_or(Err(ClientError::Disconnected))
    }

    pub async fn call_as<T: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, ClientError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Events, resyncs and reconnects. The first call gets everything
    /// since the connection was made; later calls start from now.
    pub fn events(&self) -> broadcast::Receiver<ClientEvent> {
        self.first
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| self.events.subscribe())
    }

    /// End the session: the connection is closed and no reconnect follows.
    pub async fn close(self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        *self.shared.writer.lock().await = None;
        self.shared.connected.store(false, Ordering::SeqCst);
        self.task.abort();
        self.shared.fail_pending(|| ClientError::Disconnected);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.task.abort();
        self.shared.fail_pending(|| ClientError::Disconnected);
    }
}

/// The socket `deckd` listens on, by the daemon's own rule
/// (`config::Paths::detect`): `$DECKD_DATA_DIR/deckd.sock` when that is
/// set; else on macOS `~/Library/Application Support/OpenWalkieTalkie/
/// deckd.sock`, and elsewhere `$XDG_RUNTIME_DIR/openwalkietalkie/deckd.sock`,
/// or, with no runtime dir, `$XDG_CACHE_HOME/openwalkietalkie/deckd.sock`,
/// else `~/.cache/openwalkietalkie/deckd.sock`. On Windows it is the pipe
/// of `$DECKD_DATA_DIR`, else of `%LOCALAPPDATA%\OpenWalkieTalkie`
/// ([`local_door_for`]).
pub fn default_socket() -> PathBuf {
    #[cfg(windows)]
    {
        let data = std::env::var_os("DECKD_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("OpenWalkieTalkie")
            });
        local_door_for(&data)
    }
    #[cfg(not(windows))]
    socket_for(cfg!(target_os = "macos"), |name| std::env::var_os(name))
}

/// The local door of the daemon whose data directory is `data`:
/// `<data>/deckd.sock` on Unix, the data directory's named pipe on Windows.
/// The daemon's `Paths::from_data_dir` calls this too, so the two cannot
/// disagree.
pub fn local_door_for(data: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        windows::pipe_name_for(data)
    }
    #[cfg(not(windows))]
    {
        data.join("deckd.sock")
    }
}

/// [`default_socket`]'s rule over an environment lookup `var`, for the
/// platform `macos` says. An XDG variable counts only when it is an
/// absolute path, as the `directories` crate the daemon resolves its paths
/// with reads it; a relative or empty one is passed over.
#[cfg_attr(windows, allow(dead_code))]
fn socket_for(macos: bool, var: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    if let Some(dir) = var("DECKD_DATA_DIR") {
        return PathBuf::from(dir).join("deckd.sock");
    }
    let home = var("HOME").map(PathBuf::from).unwrap_or_default();
    if macos {
        return home.join("Library/Application Support/OpenWalkieTalkie/deckd.sock");
    }
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|dir| dir.is_absolute());
    absolute("XDG_RUNTIME_DIR")
        .or_else(|| absolute("XDG_CACHE_HOME"))
        .unwrap_or_else(|| home.join(".cache"))
        .join("openwalkietalkie/deckd.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_socket_or_port_is_an_io_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = ClientConfig::uds("test");
        config.transport = Transport::Uds(PathBuf::from("/nonexistent/deckd.sock"));
        config.scopes = vec![Scope::Observe];
        let err = rt.block_on(Client::connect(config)).unwrap_err();
        assert!(matches!(err, ClientError::Io(_)), "{err}");
        // A closed port (bound then dropped) refuses the TCP connection.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let ws = ClientConfig::ws(&format!("ws://127.0.0.1:{port}/v1/ws"), "dk_x", "gui");
        let err = rt.block_on(Client::connect(ws)).unwrap_err();
        assert!(matches!(err, ClientError::Io(_)), "{err}");
        assert!(err.api_error().is_none());
    }

    #[test]
    fn the_default_socket_follows_the_data_dir_rule() {
        let path = default_socket();
        if cfg!(windows) {
            assert!(
                path.to_string_lossy().starts_with(r"\\.\pipe\openwalkietalkie-deckd-"),
                "{path:?}"
            );
            return;
        }
        assert!(path.ends_with("deckd.sock"), "{path:?}");
        if std::env::var_os("DECKD_DATA_DIR").is_none() && cfg!(target_os = "macos") {
            assert!(path
                .to_string_lossy()
                .contains("Library/Application Support/OpenWalkieTalkie"));
        }
    }

    /// The daemon's rule (`config::Paths::detect`) on both platforms,
    /// against a made-up environment: the data dir first; the Library
    /// folder on macOS; elsewhere the runtime dir, else the cache dir, else
    /// `~/.cache`, each XDG variable only when it is an absolute path.
    /// (Windows names a pipe instead: `windows::tests`.)
    #[cfg(not(windows))]
    #[test]
    fn the_socket_rule_is_the_daemons_on_either_platform() {
        let rule = |macos: bool, env: &[(&str, &str)]| {
            let env: HashMap<String, std::ffi::OsString> = env
                .iter()
                .map(|(name, value)| (name.to_string(), (*value).into()))
                .collect();
            socket_for(macos, |name| env.get(name).cloned())
        };
        let home = ("HOME", "/home/u");
        for macos in [true, false] {
            assert_eq!(
                rule(macos, &[home, ("DECKD_DATA_DIR", "/data"), ("XDG_RUNTIME_DIR", "/run/user/1")]),
                PathBuf::from("/data/deckd.sock"),
                "macos {macos}: the data dir wins"
            );
        }
        assert_eq!(
            rule(true, &[home, ("XDG_RUNTIME_DIR", "/run/user/1")]),
            PathBuf::from("/home/u/Library/Application Support/OpenWalkieTalkie/deckd.sock")
        );
        let linux = [
            (
                vec![home, ("XDG_RUNTIME_DIR", "/run/user/1"), ("XDG_CACHE_HOME", "/c")],
                "/run/user/1/openwalkietalkie/deckd.sock",
            ),
            (
                vec![home, ("XDG_CACHE_HOME", "/c")],
                "/c/openwalkietalkie/deckd.sock",
            ),
            (
                vec![home, ("XDG_RUNTIME_DIR", "run/user/1"), ("XDG_CACHE_HOME", "c")],
                "/home/u/.cache/openwalkietalkie/deckd.sock",
            ),
            (
                vec![home, ("XDG_RUNTIME_DIR", "")],
                "/home/u/.cache/openwalkietalkie/deckd.sock",
            ),
            (vec![home], "/home/u/.cache/openwalkietalkie/deckd.sock"),
        ];
        for (env, want) in linux {
            assert_eq!(rule(false, &env), PathBuf::from(want), "{env:?}");
        }
    }

    #[test]
    fn hello_replies_parse_and_incomplete_ones_are_protocol_errors() {
        let full = serde_json::json!({
            "api": "1", "daemon": "0.1.0",
            "principal": {"id": "uid:501", "scopes": ["observe", "agent"], "trusted": true},
            "session": "sess_2", "rev": 7, "snapshot": Snapshot::default(),
        });
        let hello = hello_of(&full).unwrap();
        assert_eq!(hello.session, SessionId(2));
        assert_eq!(hello.scopes, vec![Scope::Observe, Scope::Agent]);
        assert!(hello.trusted);
        assert_eq!(hello.rev, 7);
        assert!(hello.snapshot.is_some());
        let minimal = serde_json::json!({
            "api": "1", "daemon": "0.1.0", "principal": {"id": "token:gate"},
            "session": 3, "rev": 0,
        });
        let hello = hello_of(&minimal).unwrap();
        assert_eq!(hello.session, SessionId(3));
        assert!(hello.scopes.is_empty() && !hello.trusted && hello.snapshot.is_none());
        for missing in ["api", "daemon", "principal", "session", "rev"] {
            let mut v = full.clone();
            v.as_object_mut().unwrap().remove(missing);
            let err = hello_of(&v).unwrap_err();
            assert!(matches!(err, ClientError::Protocol(ref why) if why.contains(missing)));
        }
        let e = api_error_of(&serde_json::json!({"code": -32002, "message": "forbidden"}));
        assert_eq!(e.code, ApiError::FORBIDDEN);
        assert_eq!(e.data, Value::Null);
        let rpc = ClientError::Rpc {
            code: -32003,
            message: "not_found".into(),
            data: Some(serde_json::json!({"agent": "ag_1"})),
        };
        assert_eq!(rpc.api_error().unwrap().data["agent"], "ag_1");
    }
}
