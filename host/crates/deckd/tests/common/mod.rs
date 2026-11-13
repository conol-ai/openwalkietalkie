//! The in-process daemon the API exit tests (contract §5) run against:
//! a `deckd-core` thread with detached hid channels (`--no-device`), a
//! temp data dir under `$TMPDIR` with a short socket path, `[api] port =
//! 0` with `tcp = true` (the door is off unless a file switches it on) and
//! a `FakeVerifier`, plus the two tiny clients the tests speak
//! with (HTTP/1.1 and newline-delimited JSON over the Unix socket).
//!
//! Nothing here touches the real data dir, port 47201, or the network.
#![allow(dead_code)]

use deck_model::{
    AdapterKind, AgentRegister, AnswerCap, Caps, Kind, OrderCap, PidKind, Principal, Request,
    Scope, SessionId, Slot, Store, StoreConfig, Ts,
};
use deckd::api::{serve, Api, Served};
use deckd::config::{Config, Paths};
use deckd::core::handle::{ClientInfo, CoreHandle, CoreMsg, OpenSession, SessionKind};
use deckd::core::Core;
use deckd::hid::{FromHid, HidDebug, HidHandle};
use deckd::state::State;
use deckd::tokens::Tokens;
use deckd::trust::FakeVerifier;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// The wall clock every test store starts from (nothing reads the real one).
pub const WALL: Ts = Ts(1_789_760_523_000);
/// How long a test waits for a line, a response or a condition.
pub const PATIENCE: Duration = Duration::from_secs(5);

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A short unique temp dir: `sun_path` holds 103 bytes and `$TMPDIR` is
/// already ~50 of them on macOS.
fn temp_dir() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("dkt{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp data dir");
    dir
}

/// How the daemon under test is configured.
pub struct Options {
    pub verifier: FakeVerifier,
    pub allowed_origins: Vec<String>,
    pub mailbox_ttl_s: u32,
    pub uds_max_scope: String,
    pub uds_default_scope: String,
    /// `[codex] daemon_socket`: a fake app-server daemon for the client to
    /// dial. The daemon under test never spawns the client itself; a test
    /// that wants one runs `deckd::codexd::run` against `api`.
    pub codex_socket: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            verifier: FakeVerifier::default(),
            allowed_origins: vec!["http://localhost:5173".into()],
            mailbox_ttl_s: 600,
            uds_max_scope: "admin".into(),
            uds_default_scope: "observe".into(),
            codex_socket: None,
        }
    }
}

/// A running daemon: the core thread, both doors, the token table.
pub struct Daemon {
    pub api: Arc<Api>,
    pub port: u16,
    pub socket: PathBuf,
    pub dir: PathBuf,
    served: Option<Served>,
    core: CoreHandle,
    join: Option<std::thread::JoinHandle<Core>>,
    // The hid side, kept alive so the core's sends never fail.
    _ops: crossbeam_channel::Receiver<deck_model::DeviceOp>,
    _lines: crossbeam_channel::Sender<FromHid>,
    _debug: crossbeam_channel::Receiver<HidDebug>,
}

impl Daemon {
    pub async fn start() -> Daemon {
        Daemon::with(Options::default()).await
    }

    pub async fn with(options: Options) -> Daemon {
        let dir = temp_dir();
        let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        assert!(
            paths.socket.as_os_str().len() <= deckd::api::uds::SUN_PATH_MAX,
            "the test socket path must fit sun_path: {}",
            paths.socket.display()
        );
        let mut config = Config::default();
        config.api.port = 0;
        // Both doors: the TCP one is off unless the file switches it on,
        // and these tests drive it.
        config.api.tcp = Some(true);
        config.api.allowed_origins = options.allowed_origins;
        config.api.mailbox_ttl_s = options.mailbox_ttl_s;
        config.api.uds_max_scope = options.uds_max_scope;
        config.api.uds_default_scope = options.uds_default_scope;
        config.codex.daemon_socket = options.codex_socket.map(|p| p.display().to_string());
        // The hold mechanics are what these daemons are for: a permission
        // ask goes to the deck at once. `set_settle_s` turns the window on.
        config.hooks.claude.settle_s = 0;
        let config = Arc::new(config);
        let store = Store::new(StoreConfig::new(Instant::now(), WALL, 7));
        let (hid, ops, lines, debug) = HidHandle::detached();
        let mut core = Core::new(store, State::default(), hid, paths.clone());
        core.set_config(config.clone());
        // `--no-device` all the way through, whatever a test calls: no
        // firmware check opens a deck or fetches the release manifest, no
        // model download starts, and `care.checks` enumerates no HID.
        core.firmware = deckd::firmware::Updater::offline();
        core.stt_install = deckd::download::Installer::offline();
        core.tts_install = deckd::download::Installer::offline();
        core.no_device = true;
        let (handle, join) = core.spawn();
        let config = Arc::new(arc_swap::ArcSwap::from(config));
        let api = Arc::new(Api {
            core: handle.clone(),
            bus: handle.bus().clone(),
            config: config.clone(),
            tokens: Arc::new(arc_swap::ArcSwap::from_pointee(Tokens::default())),
            verifier: Arc::new(options.verifier),
            adapters: Arc::new(deckd::adapters::Adapters::new(config)),
            paths: paths.clone(),
            legacy_hooks: false,
        });
        let served = serve(api.clone()).await;
        let port = served.port.expect("an ephemeral port");
        assert!(served.uds, "the socket bound");
        Daemon {
            api,
            port,
            socket: paths.socket.clone(),
            dir,
            served: Some(served),
            core: handle,
            join: Some(join),
            _ops: ops,
            _lines: lines,
            _debug: debug,
        }
    }

    /// Every device line the core has emitted since the last call.
    ///
    /// The daemon runs with detached hid channels (`--no-device`), but the
    /// core still encodes each line and sends it: these are exactly the
    /// bytes a connected deck would have been given, which is where §5.6's
    /// `⌨ ` prefix and the `Focus terminal` row are actually applied.
    pub fn device_input(&self, line: deck_model::DeviceLine) {
        self._lines.send(FromHid::Line(line)).unwrap();
    }

    /// Anything else the hid thread would say: a link event, what the deck
    /// reported about itself, why a link ended.
    pub fn hid(&self, message: FromHid) {
        self._lines.send(message).unwrap();
    }

    pub fn device_ops(&self) -> Vec<deck_model::DeviceOp> {
        self._ops.try_iter().collect()
    }

    /// Mint a bearer token into the daemon's table; the plaintext is
    /// returned once, as `deckd token create` prints it.
    pub fn token(&self, name: &str, scopes: &[Scope]) -> String {
        let mut tokens = (**self.api.tokens.load()).clone();
        let plain = tokens
            .create(name, scopes.to_vec(), None, None, WALL)
            .expect("a fresh token name");
        self.api.tokens.store(Arc::new(tokens));
        plain
    }

    pub fn bearer(&self, name: &str, scopes: &[Scope]) -> String {
        format!("Bearer {}", self.token(name, scopes))
    }

    /// One HTTP/1.1 request: the status and the parsed JSON body.
    pub async fn http(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, serde_json::Value) {
        http(self.port, method, path, headers, body).await
    }

    /// `POST /v1/rpc` with a bearer.
    pub async fn rpc(&self, auth: &str, body: &str) -> serde_json::Value {
        let (status, json) = self
            .http("POST", "/v1/rpc", &[("Authorization", auth)], body)
            .await;
        assert!(status == 200 || status == 401 || status == 413, "{status}");
        json
    }

    /// The §4.13 adapter agent the demo registers: `kind: claude`, owned
    /// by `adapter:claude` on an internal session, `caps.answer: hold` and
    /// `caps.order: inject` (adapter-only values the API itself refuses).
    /// Returns the agent id and the internal session that owns it — a held
    /// card may only be parked by the owner (B3).
    pub async fn adapter_agent(&self, name: &str) -> (String, SessionId) {
        self.adapter_agent_pid(name, None, None).await
    }

    /// The same, carrying the pid and the `pid_kind` the hook helper's
    /// parent walk resolved (B1). The liveness pollers key off exactly
    /// these two fields: `kill(pid, 0)` runs for `cli` and nothing else.
    pub async fn adapter_agent_pid(
        &self,
        name: &str,
        pid: Option<u32>,
        pid_kind: Option<PidKind>,
    ) -> (String, SessionId) {
        let principal = Principal::Adapter(AdapterKind::Claude);
        let session = self
            .api
            .core
            .open_session(OpenSession {
                kind: SessionKind::Internal,
                principal: principal.clone(),
                scopes: vec![Scope::Agent, Scope::Observe],
                trusted: false,
                client: ClientInfo {
                    name: "adapter".into(),
                    version: String::new(),
                },
            })
            .await
            .expect("an internal session");
        let registered = self
            .api
            .core
            .api(
                principal,
                session,
                false,
                Request::AgentRegister(AgentRegister {
                    kind: Kind::Claude,
                    name: name.to_string(),
                    key: Some(format!("claude:{name}")),
                    cwd: None,
                    pid,
                    pid_kind,
                    terminal: None,
                    lease_ttl_s: 300,
                    caps: Caps {
                        answer: AnswerCap::Hold,
                        order: OrderCap::Inject,
                        focus: true,
                        transcribe: Default::default(),
                    },
                    slot_hint: Some(Slot(6)),
                    voice: None,
                    meta: None,
                }),
            )
            .await
            .expect("the adapter registers");
        let id = registered["agent"]
            .as_str()
            .unwrap_or_else(|| panic!("{registered}"))
            .to_string();
        (id, session)
    }

    /// A UDS connection that has not said hello yet.
    pub async fn connect(&self) -> UdsClient {
        UdsClient::connect(&self.socket).await
    }

    /// Wait until `f` holds, polling the core's snapshot.
    pub async fn until(&self, what: &str, mut f: impl FnMut(&deck_model::Snapshot) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if f(&self.api.bus.snapshot()) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Stop the doors and the core (also done on drop, without awaiting).
    pub async fn shutdown(mut self) {
        if let Some(served) = self.served.take() {
            served.shutdown().await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(served) = self.served.take() {
            served.stop();
        }
        self.core.send_blocking(CoreMsg::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One HTTP/1.1 request on `127.0.0.1:<port>`; `Host`, `Content-Type`,
/// `Content-Length` and `Connection: close` are added unless given.
pub async fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, serde_json::Value) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the tcp door");
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
    tokio::time::timeout(PATIENCE, stream.read_to_end(&mut raw))
        .await
        .expect("a response in time")
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

/// A newline-delimited JSON-RPC client over the Unix socket (what
/// `deck-client` will be once B4 lands; the exit tests need one now).
pub struct UdsClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    id: u64,
}

impl UdsClient {
    pub async fn connect(socket: &std::path::Path) -> UdsClient {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("the uds door");
        let (r, w) = stream.into_split();
        UdsClient {
            reader: BufReader::new(r),
            writer: w,
            id: 0,
        }
    }

    /// Write one raw line (the tests send malformed ones on purpose). A
    /// write error is the door having hung up already, which is exactly
    /// what the oversized-line cases expect: the response is read next.
    pub async fn write_raw(&mut self, line: &str) {
        let _ = self.writer.write_all(line.as_bytes()).await;
        let _ = self.writer.write_all(b"\n").await;
    }

    /// Read one line as JSON; panics when the peer closed or stalled.
    pub async fn line(&mut self) -> serde_json::Value {
        let mut text = String::new();
        let n = tokio::time::timeout(PATIENCE, self.reader.read_line(&mut text))
            .await
            .expect("a line in time")
            .unwrap();
        assert!(n > 0, "the connection closed while a line was expected");
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not json: {text:?}"))
    }

    /// Read lines until one satisfies `f` (skipping notifications).
    pub async fn line_where(
        &mut self,
        what: &str,
        mut f: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let line = self.line().await;
            if f(&line) {
                return line;
            }
            assert!(Instant::now() < deadline, "no line matched {what}");
        }
    }

    /// True when the peer closed the connection: EOF, or a reset. Linux
    /// reports a reset instead of EOF when the door hangs up with part of
    /// what we sent still unread (a line past the cap); macOS gives EOF.
    pub async fn closed(&mut self) -> bool {
        let mut rest = String::new();
        match tokio::time::timeout(PATIENCE, self.reader.read_line(&mut rest)).await {
            Ok(Ok(0)) => true,
            Ok(Err(e)) => e.kind() == std::io::ErrorKind::ConnectionReset,
            _ => false,
        }
    }

    /// `session.hello` with these params; returns the whole response.
    pub async fn hello(&mut self, params: serde_json::Value) -> serde_json::Value {
        self.request("session.hello", params).await
    }

    /// One request/response round trip (notifications are not used here).
    pub async fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let id = self.id;
        let line = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        })
        .to_string();
        self.write_raw(&line).await;
        self.line_where(method, |v| v["id"] == id).await
    }

    /// The `result` of a call, or the test fails with the error.
    pub async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let resp = self.request(method, params).await;
        assert!(resp["error"].is_null(), "{method}: {resp}");
        resp["result"].clone()
    }

    /// The `error` of a call, or the test fails with the result.
    pub async fn call_err(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let resp = self.request(method, params).await;
        assert!(
            resp["result"].is_null(),
            "{method} unexpectedly succeeded: {resp}"
        );
        resp["error"].clone()
    }
}

/// A `poll`/`poll` virtual agent's `agent.register` params.
pub fn register_params(name: &str, key: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "other",
        "name": name,
        "key": key,
        "lease_ttl_s": 30,
        "caps": {"answer": "poll", "order": "poll", "focus": false},
    })
}

/// A two-choice custom `q` card, delivered by `poll`.
pub fn card_params(agent: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "agent": agent,
        "type": "q",
        "text": text,
        "choices": [{"label": "Yes"}, {"label": "No"}],
        "free_text": false,
        "deliver": "poll",
        "origin": {"kind": "custom"},
    })
}

/// A permission card held by an adapter (§4.13): the guarded shape.
pub fn permission_card_params(agent: &str) -> serde_json::Value {
    serde_json::json!({
        "agent": agent,
        "type": "q",
        "text": "Bash: psql -f migrate.sql staging",
        "choices": [{"label": "Allow"}, {"label": "Allow always"}, {"label": "Deny"}],
        "rec": 0,
        "free_text": false,
        "deliver": "hold",
        "origin": {"kind": "permission", "tool": "Bash", "request_id": "toolu_9"},
    })
}
