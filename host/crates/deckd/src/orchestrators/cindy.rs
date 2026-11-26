//! `cindy` on the deck (`docs/orchestrator-adapters.md`, "cindy").
//!
//! **Not implementable as a fleet source.** Doc-derived, not verified against
//! a running instance. Source: `makecindy/cindy` `docs/local-agent-api.md`
//! ("Cindy 本机 Agent API v1"), read in full 2026-11-26 at `9f570bf508fe`.
//!
//! The Local Agent API v1's methods are `capabilities`, `connect/start`,
//! `connect/poll`, `connect/cancel`, `connect/revoke`, `models`,
//! `session/open`, `send`, `steer`, `interrupt`, `instructions`, `events`,
//! `tool/reply` and `session/close`. There is **no list method**: a session's
//! `sessionId` "是客户端自定的" (is chosen by the client) and the API "不暴露跨应用
//! 列举接口" (exposes no cross-application listing). So Cindy cannot report on
//! sessions deckd did not open, and [`Orchestrator::status`] always fails with
//! [`NO_FLEET`] without touching the network. No cards, no invented sessions.
//!
//! What is kept, because the doc states it and a later list path would need
//! it:
//!
//! - Discovery: `<profile>/local-agent/discovery.json`,
//!   `{"protocol":1,"port":12345,"instance":"<opaque-instance>"}`. No file, no
//!   adapter ([`new`] is `None`).
//! - `POST http://127.0.0.1:<port>/rpc` with
//!   `{"version":1,"instance":…,"id":…,"method":…,"params":{}}`; "RPC 不使用
//!   JSON-RPC 的 `jsonrpc` 字段" and "任何带 Origin 的请求都拒绝", so
//!   [`http_request`] writes no `Origin`. Success is `{"result":…}`, failure
//!   `{"error":{"code":"...","message":"..."}}`; only the code is read.
//! - Request ids: "有副作用的请求编号必须是 `<process-nonce>:<counter>`，nonce 为
//!   8–64 位 `[A-Za-z0-9_-]`，counter 从 1 单调递增、最多 15 位" ([`next_id`]).
//! - `send`: `{sessionId,parts}`, `parts` being `{type:"text",text}` objects
//!   ([`Cindy::order_body`]). It only reaches sessions this grant opened.
//! - Auth: "后续请求带 `Authorization: Bearer <token>`". The token comes from
//!   `connect/start` and a person approving it in Cindy; this adapter never
//!   pairs: with no token ([`TOKEN_ENV`]) an order fails with `not_paired`.
//! - Approvals: "当前协议不转发原生人机确认：原生提问/计划批准会产生明确失败" — they end
//!   a turn as `turn_end {status:"failed"}` on `events`. `RESUME_UNAVAILABLE`
//!   is a resume error ("新会话并交接上下文"), never an approval.
//!
//! **Transport.** Hand-written HTTP/1.1 over a blocking
//! [`std::net::TcpStream`] to `127.0.0.1` only, as `crate::cli::info_probe`
//! does: no HTTP client crate.
use super::{json_payload, safe_key, Orchestrator, Task};
use crate::config::Config;
use serde_json::{json, Value};
use std::io::{Read, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Why Cindy cannot be a fleet source.
pub const NO_FLEET: &str = "no_fleet_api: Cindy's Local Agent API v1 has no list method; it cannot report on sessions deckd did not open";
/// The protocol a discovery file must name.
pub const PROTOCOL: u64 = 1;
/// The `version` of every request.
pub const VERSION: u64 = 1;
/// The method that sends a session one message.
pub const SEND_METHOD: &str = "send";
/// The client token Cindy's UI approved (deckd's convention, not Cindy's).
pub const TOKEN_ENV: &str = "DECKD_CINDY_TOKEN";
/// A discovery file to use instead of the profile's.
pub const DISCOVERY_ENV: &str = "DECKD_CINDY_DISCOVERY";
/// Cindy's profile directory, when it is not the default.
pub const PROFILE_ENV: &str = "DECKD_CINDY_PROFILE";
/// Connecting to a local port gets this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// An order gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of a reply that is read.
const REPLY_MAX: u64 = 4 * 1024 * 1024;

/// Where Cindy listens, from its discovery file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    /// Echoed back in every request, as Cindy wrote it.
    pub instance: Value,
}

/// No `Debug`: it holds the token.
pub struct Cindy {
    /// The discovery file's endpoint, or why it is unusable.
    endpoint: Result<Endpoint, String>,
    token: Option<String>,
}

/// The adapter for `status --from-file` on a machine without Cindy.
#[must_use]
pub fn offline() -> Cindy {
    Cindy::with(
        Err(format!(
            "not_paired: no Cindy discovery file (set {DISCOVERY_ENV})"
        )),
        None,
    )
}

pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    // `Config` has no `[orchestrators.cindy]` table; the path comes from the
    // environment or Cindy's default profile.
    let path = discovery_path()?;
    let token = std::env::var(TOKEN_ENV).ok();
    Cindy::from_discovery_file(&path, token).map(|c| Box::new(c) as Box<dyn Orchestrator>)
}

/// `$DECKD_CINDY_DISCOVERY`, else `<profile>/local-agent/discovery.json`
/// with the profile from `$DECKD_CINDY_PROFILE` or `<data dir>/Cindy`.
fn discovery_path() -> Option<PathBuf> {
    if let Some(file) = std::env::var_os(DISCOVERY_ENV).filter(|f| !f.is_empty()) {
        return Some(PathBuf::from(file));
    }
    let profile = match std::env::var_os(PROFILE_ENV).filter(|p| !p.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => directories::BaseDirs::new()?.data_dir().join("Cindy"),
    };
    Some(profile.join("local-agent").join("discovery.json"))
}

/// The discovery file's endpoint: protocol 1, a port, an instance.
pub fn parse_discovery(raw: &str) -> Result<Endpoint, String> {
    let value = json_payload(raw)?;
    match value["protocol"].as_u64() {
        Some(PROTOCOL) => {}
        Some(n) => return Err(format!("unsupported: Cindy local agent protocol {n}")),
        None => return Err("malformed: discovery has no `protocol`".into()),
    }
    let port = value["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| "malformed: discovery has no usable `port`".to_string())?;
    let instance = value["instance"].clone();
    if !(instance.is_string() || instance.is_number()) {
        return Err("malformed: discovery has no `instance`".into());
    }
    Ok(Endpoint { port, instance })
}

/// This process's nonce: 8–64 of `[A-Za-z0-9_-]`, new on every start.
fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("deckd-{:x}-{:x}", std::process::id(), nanos)
    })
}

/// A request id, `<process-nonce>:<counter>`, the counter from 1 up.
fn next_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}:{}",
        process_nonce(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

impl Cindy {
    /// The adapter for this discovery file: `None` when there is no file.
    /// A file that is there but unusable still gives an adapter, whose
    /// orders say why.
    pub fn from_discovery_file(path: &Path, token: Option<String>) -> Option<Cindy> {
        if !path.is_file() {
            return None;
        }
        let endpoint = std::fs::read_to_string(path)
            .map_err(|e| format!("unreadable: Cindy discovery file: {}", e.kind()))
            .and_then(|raw| parse_discovery(&raw));
        Some(Cindy::with(endpoint, token))
    }

    /// An adapter for `endpoint`, with `token` when blank is none.
    #[must_use]
    pub fn with(endpoint: Result<Endpoint, String>, token: Option<String>) -> Cindy {
        let token = token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        Cindy { endpoint, token }
    }

    /// The instance a request echoes; `null` when discovery failed (no
    /// request is sent then).
    fn instance(&self) -> Value {
        self.endpoint
            .as_ref()
            .map_or(Value::Null, |e| e.instance.clone())
    }

    /// One Local Agent API v1 request body. Not JSON-RPC; never the token.
    #[must_use]
    pub fn rpc_body(&self, id: &str, method: &str, params: Value) -> Value {
        json!({
            "version": VERSION,
            "instance": self.instance(),
            "id": id,
            "method": method,
            "params": params,
        })
    }

    /// The `send` request for one order: `{sessionId, parts:[{type:"text",
    /// text}]}`; the text is one field, whatever it holds.
    #[must_use]
    pub fn order_body(&self, key: &str, text: &str) -> Value {
        self.rpc_body(
            &next_id(),
            SEND_METHOD,
            json!({ "sessionId": key, "parts": [{ "type": "text", "text": text }] }),
        )
    }

    /// The endpoint and token, or why a request cannot be made. Checked
    /// before anything touches the network.
    fn ready(&self) -> Result<(&Endpoint, &str), String> {
        let endpoint = self.endpoint.as_ref().map_err(Clone::clone)?;
        let token = self.token.as_deref().ok_or_else(|| {
            format!("not_paired: approve deckd in Cindy's UI and set {TOKEN_ENV}")
        })?;
        Ok((endpoint, token))
    }

    /// POST `body` and give the reply's JSON body.
    fn call(&self, body: &Value, timeout: Duration) -> Result<String, String> {
        let (endpoint, token) = self.ready()?;
        let request = http_request(endpoint.port, token, &body.to_string());
        let raw = post(endpoint.port, &request, timeout)?;
        let (status, body) = http_response(&raw)?;
        if status == 401 || status == 403 {
            return Err("not_paired: Cindy refused the client token".into());
        }
        // An error envelope says more than the status line does.
        if let Ok(value) = json_payload(&body) {
            reply_error(&value)?;
        }
        if !(200..300).contains(&status) {
            return Err(format!("backend_refused: Cindy answered HTTP {status}"));
        }
        Ok(body)
    }
}

/// `POST /rpc` with `body`: no `Origin` header (Cindy refuses one), the
/// token in `Authorization` and nowhere else.
#[must_use]
pub fn http_request(port: u16, token: &str, body: &str) -> String {
    format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Send `request` to `127.0.0.1:<port>` and read the whole reply.
fn post(port: u16, request: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("not_running: Cindy is not listening ({})", e.kind()))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("transport: cannot send to Cindy ({})", e.kind()))?;
    let mut raw = Vec::new();
    stream
        .take(REPLY_MAX)
        .read_to_end(&mut raw)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                format!("timeout: Cindy took longer than {timeout:?}")
            }
            kind => format!("transport: no reply from Cindy ({kind})"),
        })?;
    Ok(raw)
}

/// An HTTP/1.1 reply's status and body, chunked or not.
pub fn http_response(raw: &[u8]) -> Result<(u16, String), String> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed: Cindy's reply has no body".to_string())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|l| l.starts_with("HTTP/1."))
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| "malformed: Cindy's reply is not HTTP".to_string())?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
        })
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_string()
    };
    Ok((status, body))
}

/// A chunked body, joined.
fn dechunk(mut rest: &str) -> Result<String, String> {
    let bad = || "malformed: Cindy's chunked reply".to_string();
    let mut out = String::new();
    loop {
        let (size, after) = rest.split_once("\r\n").ok_or_else(bad)?;
        let size = size.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| bad())?;
        if size == 0 {
            return Ok(out);
        }
        let chunk = after.get(..size).ok_or_else(bad)?;
        out.push_str(chunk);
        rest = after
            .get(size..)
            .and_then(|r| r.strip_prefix("\r\n"))
            .ok_or_else(bad)?;
    }
}

/// An error `code` as Cindy documents them (`UNAUTHORIZED`): one word of
/// letters, digits and `_`. Free text (it may hold paths) is dropped.
fn reason_code(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let ok = (2..=40).contains(&raw.len())
        && raw.starts_with(|c: char| c.is_ascii_alphabetic())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(raw)
}

/// The reply's `{"error":{"code","message"}}`, as an adapter `Err`; `Ok` when
/// there is none. The message is never quoted (it may hold paths).
fn reply_error(reply: &Value) -> Result<(), String> {
    let Some(error) = reply.get("error").filter(|e| !e.is_null()) else {
        return Ok(());
    };
    let code = error["code"]
        .as_str()
        .and_then(reason_code)
        .unwrap_or("UNKNOWN");
    // The documented codes with a meaning for an order; the rest is a refusal.
    let word = match code {
        "UNAUTHORIZED" => "not_paired",
        "INSTANCE_CHANGED" => "instance_changed",
        "SESSION_UNAVAILABLE" => "no_such_task",
        _ => "backend_refused",
    };
    Err(format!("{word}: Cindy answered {code}"))
}

/// What `status --from-file` reads: a documented error envelope is that
/// error; anything else is [`NO_FLEET`], because no reply lists a fleet.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    if let Ok(value) = json_payload(raw) {
        reply_error(&value)?;
    }
    Err(NO_FLEET.into())
}

impl Orchestrator for Cindy {
    fn name(&self) -> &'static str {
        "cindy"
    }

    fn label(&self) -> &'static str {
        "Cindy"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        Err(NO_FLEET.into())
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
        // "sessionId 是客户端自定的非空字符串，最多 128 字符"; ours are argv-safe too.
        if !safe_key(key) {
            return Err("bad_key: not a Cindy session id".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("empty: nothing to send".into());
        }
        self.call(&self.order_body(key, text), SEND_TIMEOUT)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const TOKEN: &str = "cindy-client-token-0123456789abcdef";

    fn endpoint(port: u16) -> Endpoint {
        Endpoint {
            port,
            instance: json!("inst-7"),
        }
    }

    fn paired(port: u16) -> Cindy {
        Cindy::with(Ok(endpoint(port)), Some(TOKEN.into()))
    }

    #[test]
    fn status_is_no_fleet_api_and_touches_no_network() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        for cindy in [
            paired(port),
            Cindy::with(Ok(endpoint(port)), None),
            offline(),
        ] {
            assert_eq!(cindy.status(), Err(NO_FLEET.to_string()));
        }
        assert!(listener.accept().is_err(), "status must not connect");
        assert_eq!(super::super::reason_word(NO_FLEET), "no_fleet_api");
    }

    #[test]
    fn no_reply_is_read_as_a_fleet() {
        // The session list the first version invented: not a fleet either.
        let invented =
            r#"{"result":{"sessions":[{"id":"s-1","state":"running","orca_workers":[]}]}}"#;
        for raw in [
            "",
            "[]",
            invented,
            r#"{"result":{"protocol":1,"methods":[]}}"#,
        ] {
            assert_eq!(parse(raw), Err(NO_FLEET.to_string()), "{raw:?}");
        }
    }

    #[test]
    fn an_error_envelope_is_a_reason_word_without_the_message() {
        let err = parse(r#"{"error":{"code":"UNAUTHORIZED","message":"see ~/.cindy/token"}}"#)
            .expect_err("error");
        assert_eq!(err, "not_paired: Cindy answered UNAUTHORIZED");
        let err =
            parse(r#"{"error":{"code":"SESSION_UNAVAILABLE","message":"x"}}"#).expect_err("error");
        assert!(err.starts_with("no_such_task"), "{err}");
        // A resume error is a refusal, not an approval.
        let err =
            parse(r#"{"error":{"code":"RESUME_UNAVAILABLE","message":"x"}}"#).expect_err("error");
        assert!(err.starts_with("backend_refused"), "{err}");
        let err = parse(r#"{"error":{"code":"rm -rf / please"}}"#).expect_err("error");
        assert_eq!(err, "backend_refused: Cindy answered UNKNOWN");
    }

    #[test]
    fn request_ids_are_nonce_colon_counter() {
        let (a, b) = (next_id(), next_id());
        for id in [&a, &b] {
            let (nonce, counter) = id.split_once(':').expect("colon");
            assert!((8..=64).contains(&nonce.len()), "{nonce}");
            assert!(
                nonce
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{nonce}"
            );
            assert!(
                !counter.is_empty()
                    && counter.len() <= 15
                    && counter.bytes().all(|c| c.is_ascii_digit())
            );
        }
        let n = |id: &str| {
            id.rsplit(':')
                .next()
                .and_then(|c| c.parse::<u64>().ok())
                .expect("n")
        };
        assert!(n(&b) > n(&a), "the counter only goes up");
        assert_eq!(
            a.split(':').next(),
            b.split(':').next(),
            "one nonce per process"
        );
    }

    #[test]
    fn the_send_body_is_session_id_and_text_parts() {
        let text = "ship it\"; rm -rf / && echo $(id) {\"method\":\"x\"}";
        let body = paired(4100).order_body("notes-1", text);
        assert_eq!(body["version"], 1);
        assert_eq!(body["instance"], "inst-7");
        assert_eq!(body["method"], SEND_METHOD);
        assert_eq!(
            body["params"],
            json!({"sessionId": "notes-1", "parts": [{"type": "text", "text": text}]})
        );
        assert!(body.get("jsonrpc").is_none(), "not JSON-RPC");
        assert!(body["params"].get("session_id").is_none());
        assert!(!body.to_string().contains(TOKEN));
    }

    #[test]
    fn with_no_token_orders_fail_as_not_paired() {
        for token in [None, Some(String::new()), Some("   ".to_string())] {
            let cindy = Cindy::with(Ok(endpoint(1)), token);
            let err = cindy.send_order("s-01", "hi").expect_err("unpaired");
            assert!(err.starts_with("not_paired"), "{err}");
        }
    }

    #[test]
    fn an_order_cindy_would_misread_is_refused_before_it_runs() {
        let cindy = paired(1);
        assert!(cindy
            .send_order("../s", "hi")
            .is_err_and(|e| e.starts_with("bad_key")));
        assert!(cindy
            .send_order("s-01", "  ")
            .is_err_and(|e| e.starts_with("empty")));
    }

    #[test]
    fn discovery_needs_protocol_one_a_port_and_an_instance() {
        assert_eq!(
            parse_discovery(r#"{"protocol":1,"port":4100,"instance":"inst-7"}"#),
            Ok(endpoint(4100))
        );
        for raw in [
            r#"{"protocol":2,"port":4100,"instance":"i"}"#,
            r#"{"port":4100,"instance":"i"}"#,
            r#"{"protocol":1,"port":0,"instance":"i"}"#,
            r#"{"protocol":1,"port":70000,"instance":"i"}"#,
            r#"{"protocol":1,"port":4100}"#,
            "",
        ] {
            assert!(parse_discovery(raw).is_err(), "{raw:?} parsed");
        }
    }

    #[test]
    fn no_discovery_file_is_no_adapter() {
        let dir = std::env::temp_dir().join(format!("deckd-cindy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("discovery.json");
        let _ = std::fs::remove_file(&file);
        assert!(Cindy::from_discovery_file(&file, None).is_none());
        std::fs::write(&file, r#"{"protocol":1,"port":4100,"instance":"i"}"#).expect("write");
        let cindy = Cindy::from_discovery_file(&file, None).expect("adapter");
        assert_eq!(cindy.status(), Err(NO_FLEET.to_string()));
        assert!(cindy
            .send_order("s", "hi")
            .is_err_and(|e| e.starts_with("not_paired")));
        std::fs::write(&file, r#"{"protocol":9}"#).expect("write");
        let cindy = Cindy::from_discovery_file(&file, Some(TOKEN.into())).expect("adapter");
        assert!(cindy
            .send_order("s", "hi")
            .is_err_and(|e| e.starts_with("unsupported")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_request_has_no_origin_and_the_token_only_in_a_header() {
        let body = paired(4100).order_body("s-01", "hi").to_string();
        let req = http_request(4100, TOKEN, &body);
        assert!(req.starts_with("POST /rpc HTTP/1.1\r\n"));
        assert!(!req.to_ascii_lowercase().contains("origin:"));
        let (head, sent) = req.split_once("\r\n\r\n").expect("head");
        assert!(head.contains(&format!("Authorization: Bearer {TOKEN}")));
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        assert_eq!(sent, body);
    }

    #[test]
    fn replies_plain_and_chunked_are_read() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"result\":{}}";
        assert_eq!(http_response(plain), Ok((200, "{\"result\":{}}".into())));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"res\r\n8\r\nult\":{}}\r\n0\r\n\r\n";
        assert_eq!(http_response(chunked), Ok((200, "{\"result\":{}}".into())));
        for bad in [
            &b"garbage"[..],
            b"SSH-2.0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        ] {
            assert!(http_response(bad).is_err());
        }
    }

    /// One reply from a fake Cindy on a local port; gives the request it got.
    fn serve(reply: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            // Read the head, then Content-Length bytes of body.
            loop {
                let n = conn.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let _ = conn.write_all(reply.as_bytes());
            String::from_utf8_lossy(&got).into_owned()
        });
        (port, handle)
    }

    #[test]
    fn send_goes_over_local_http_with_the_documented_body() {
        let (port, server) = serve("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"result\":{}}");
        paired(port).send_order("notes-1", "go on").expect("send");
        let got = server.join().expect("server");
        assert!(got.starts_with("POST /rpc HTTP/1.1\r\n"));
        assert!(!got.to_ascii_lowercase().contains("origin:"));
        let (_, body) = got.split_once("\r\n\r\n").expect("body");
        let body: Value = serde_json::from_str(body).expect("json");
        assert_eq!(body["method"], "send");
        assert_eq!(body["params"]["parts"][0]["text"], "go on");
        assert!(body["id"].as_str().is_some_and(|id| id.contains(':')));

        let (port, server) = serve(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"error\":{\"code\":\"SESSION_UNAVAILABLE\",\"message\":\"/home/x\"}}",
        );
        let err = paired(port)
            .send_order("someone-elses", "hi")
            .expect_err("not ours");
        assert_eq!(err, "no_such_task: Cindy answered SESSION_UNAVAILABLE");
        let _ = server.join();

        let (port, server) = serve("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n{}");
        let err = paired(port)
            .send_order("notes-1", "hi")
            .expect_err("refused");
        assert!(err.starts_with("not_paired"), "{err}");
        let _ = server.join();
    }
}
