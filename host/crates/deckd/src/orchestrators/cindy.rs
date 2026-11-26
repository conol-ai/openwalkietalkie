//! `cindy` on the deck (`docs/orchestrator-adapters.md`, "cindy").
//!
//! Cindy is the one adapter that is not a CLI shell-out: its surface is the
//! **Local Agent API v1**, `POST http://127.0.0.1:<port>/rpc`. What the
//! adapter relies on, and nothing else:
//!
//! - Discovery: `<profile>/local-agent/discovery.json`, `{"protocol":1,
//!   "port":…, "instance":…}`. No file, no adapter ([`new`] is `None`).
//! - A request is `{"version":1,"instance":…,"id":"…","method":…,
//!   "params":{…}}` ([`Cindy::rpc_body`]). It is **not** JSON-RPC (no
//!   `jsonrpc` field), and Cindy rejects any request carrying an `Origin`
//!   header, so [`http_request`] never writes one.
//! - [`LIST_METHOD`] gives the sessions: each with `id` (the address of an
//!   order), `parent_session_id` (a worker is a full session; this, or a
//!   nested `orca_workers` array, shapes the tree), `agent`, `state` and
//!   `title`. Any state not named in [`state`] is `Idle`.
//! - `send` with the session id and the text: one order
//!   ([`Cindy::order_body`]).
//!
//! **Pairing.** The client token comes from `connect/start` and a person
//! approving it in Cindy's UI. This adapter never pairs: with no token
//! ([`TOKEN_ENV`]) `status` and `send_order` fail with `not_paired`, and a
//! token Cindy refuses is `not_paired` too. The token travels in a header
//! only, never in a body, a title or `--json` output.
//!
//! **Approvals.** Cindy's native permission prompts are not forwarded by the
//! API: a worker that needs one surfaces as a `RESUME_UNAVAILABLE`-class
//! failure. That is `Blocked(Failed)` with the raw reason word as the
//! activity; there is no approval card, because nothing here could answer it.
//!
//! **Transport.** Hand-written HTTP/1.1 over a blocking
//! [`std::net::TcpStream`] to `127.0.0.1` only, as `crate::cli::info_probe`
//! does: no HTTP client crate.
use super::{json_payload, redact, safe_key, title, Blocked, Orchestrator, Task, TaskState};
use crate::config::Config;
use serde_json::{json, Value};
use std::io::{Read, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The protocol a discovery file must name.
pub const PROTOCOL: u64 = 1;
/// The `version` of every request.
pub const VERSION: u64 = 1;
/// The method that lists sessions and their workers.
pub const LIST_METHOD: &str = "session/list";
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
/// A session list gets this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// An order gets this long.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of a reply that is read; a fleet's list is far smaller.
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

/// The adapter for `status --from-file` on a machine without Cindy: `parse`
/// works, every request says the client is not paired.
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

/// A request id, unique within this process.
fn next_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("deckd-{prefix}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

impl Cindy {
    /// The adapter for this discovery file: `None` when there is no file.
    /// A file that is there but unusable still gives an adapter, whose
    /// `status` says why.
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

    /// The `send` request for one order: `text` is one field, whatever it
    /// holds, and nothing else carries it.
    #[must_use]
    pub fn order_body(&self, key: &str, text: &str) -> Value {
        self.rpc_body(
            &next_id("send"),
            SEND_METHOD,
            json!({ "session_id": key, "text": text }),
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

/// A raw reason word Cindy gave (`RESUME_UNAVAILABLE`): one word of letters,
/// digits and `_`. Free text (it may hold paths or commands) is dropped.
fn reason_code(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let ok = (2..=40).contains(&raw.len())
        && raw.starts_with(|c: char| c.is_ascii_alphabetic())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(raw)
}

/// The code of an `error` that is a word or `{"code": …}`.
fn error_code(error: &Value) -> Option<&str> {
    error
        .as_str()
        .or_else(|| error["code"].as_str())
        .and_then(reason_code)
}

/// The reply's `error`, as an adapter `Err`; `Ok` when there is none. The
/// message is never quoted (it may hold paths).
fn reply_error(reply: &Value) -> Result<(), String> {
    let Some(error) = reply.get("error").filter(|e| !e.is_null()) else {
        return Ok(());
    };
    let code = error_code(error).unwrap_or("UNKNOWN");
    let upper = code.to_ascii_uppercase();
    let word = if [
        "NOT_PAIRED",
        "UNPAIRED",
        "UNAUTHORIZED",
        "UNAUTHENTICATED",
        "FORBIDDEN",
        "INVALID_TOKEN",
        "TOKEN_REVOKED",
    ]
    .contains(&upper.as_str())
    {
        "not_paired"
    } else if upper.ends_with("_UNAVAILABLE") {
        "unavailable"
    } else if upper.contains("NOT_FOUND") || upper.starts_with("UNKNOWN_SESSION") {
        "no_such_task"
    } else {
        "backend_refused"
    };
    Err(format!("{word}: Cindy answered {code}"))
}

/// One session's state, and the raw reason when it failed.
///
/// A session carrying an error code, or whose state is itself a code
/// (`RESUME_UNAVAILABLE`), has failed: that is how a native permission
/// prompt surfaces, and it is never an approval card.
#[must_use]
pub fn state(session: &Value) -> (TaskState, Option<String>) {
    let word = session["state"].as_str().unwrap_or_default().trim();
    let code = session
        .get("error")
        .and_then(error_code)
        .or_else(|| session["reason"].as_str().and_then(reason_code))
        .or_else(|| {
            // An upper-case state is a reason code, not a state word.
            reason_code(word).filter(|w| w.chars().any(|c| c.is_ascii_uppercase()))
        });
    if let Some(code) = code {
        return (TaskState::Blocked(Blocked::Failed), Some(code.to_string()));
    }
    let state = match word {
        "running" | "working" | "busy" | "streaming" => TaskState::Working,
        "completed" | "complete" | "done" | "finished" => TaskState::Done,
        "failed" | "error" | "errored" => TaskState::Blocked(Blocked::Failed),
        // `idle`, `ready`, and whatever a later Cindy adds.
        _ => TaskState::Idle,
    };
    let reason = state.is_blocked().then(|| word.to_string());
    (state, reason)
}

/// The agent's name, when it is a plain word.
fn agent(session: &Value) -> Option<&str> {
    session["agent"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty() && safe_key(a) && !a.contains(' '))
}

/// A session's own title, unless it says nothing once paths and secrets
/// are masked (then the name stands in, see [`title`]).
fn own_title(session: &Value) -> &str {
    let raw = session["title"].as_str().unwrap_or_default();
    let says_something = redact(raw)
        .split(' ')
        .any(|w| !w.is_empty() && w != "[path]" && w != "[redacted]");
    if says_something {
        raw
    } else {
        ""
    }
}

/// The sessions of a reply: the envelope's `result`, a `sessions` array, or
/// the bare array. An error envelope is an `Err`.
fn sessions(value: &Value) -> Result<&Vec<Value>, String> {
    reply_error(value)?;
    let value = value.get("result").unwrap_or(value);
    match value {
        Value::Array(list) => Ok(list),
        Value::Object(map) => map
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| "malformed: no `sessions` array".to_string()),
        _ => Err("malformed: not a list of sessions".into()),
    }
}

/// `session` and its nested `orca_workers`, depth first.
fn flatten<'a>(session: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(session);
    if let Some(workers) = session["orca_workers"].as_array() {
        for worker in workers {
            flatten(worker, out);
        }
    }
}

/// A [`LIST_METHOD`] reply as tasks: one per session, workers included. A
/// session with no usable `id` cannot be addressed and is left out.
pub fn parse(raw: &str) -> Result<Vec<Task>, String> {
    let value = json_payload(raw)?;
    let mut all = Vec::new();
    for session in sessions(&value)? {
        flatten(session, &mut all);
    }
    let mut tasks = Vec::new();
    for session in all {
        let Some(id) = session["id"]
            .as_str()
            .map(str::trim)
            .filter(|i| safe_key(i))
        else {
            continue;
        };
        if session["state"].is_null() && session.get("error").is_none() {
            continue;
        }
        let (state, reason) = state(session);
        let name = match agent(session) {
            Some(agent) => format!("{agent} · {id}"),
            None => id.to_string(),
        };
        tasks.push(Task {
            key: id.to_string(),
            title: title(own_title(session), &name),
            state,
            activity: reason,
        });
    }
    Ok(tasks)
}

impl Orchestrator for Cindy {
    fn name(&self) -> &'static str {
        "cindy"
    }

    fn label(&self) -> &'static str {
        "Cindy"
    }

    fn status(&self) -> Result<Vec<Task>, String> {
        let body = self.rpc_body(&next_id("list"), LIST_METHOD, json!({}));
        parse(&self.call(&body, LIST_TIMEOUT)?)
    }

    fn parse(&self, raw: &str) -> Result<Vec<Task>, String> {
        parse(raw)
    }

    fn send_order(&self, key: &str, text: &str) -> Result<(), String> {
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
    use crate::orchestrators::{budget, plan, status_json, CARDS_MAX, DECK_SLOTS};
    use deck_model::AgentState;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Written from the documented shape, not captured from a live Cindy.
    const FIXTURE: &str = include_str!("../../tests/orchestrators/cindy.json");
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
    fn the_fixture_parses_into_the_documented_states() {
        let tasks = parse(FIXTURE).expect("fixture");
        let got: Vec<(&str, TaskState)> = tasks.iter().map(|t| (t.key.as_str(), t.state)).collect();
        assert_eq!(
            got,
            [
                ("s-01", TaskState::Working),
                ("s-02", TaskState::Working),
                ("s-03", TaskState::Blocked(Blocked::Failed)),
                ("s-04", TaskState::Done),
                ("s-05", TaskState::Idle),
                ("s-06", TaskState::Idle),
            ]
        );
        assert_eq!(tasks[0].title, "Refactor the session store");
        assert_eq!(
            tasks[1].title, "codex · s-02",
            "a command line is not a title"
        );
        assert_eq!(tasks[2].title, "Migrate [path]");
        assert_eq!(tasks[2].activity.as_deref(), Some("RESUME_UNAVAILABLE"));
        assert_eq!(tasks[5].title, "gemini · s-06", "a bare path says nothing");
    }

    #[test]
    fn a_permission_prompt_is_a_failure_with_its_reason_never_an_approval() {
        let tasks = parse(
            r#"[{"id":"a","state":"RESUME_UNAVAILABLE"},
                {"id":"b","state":"running","error":"PERMISSION_UNAVAILABLE"},
                {"id":"c","state":"idle","reason":"RESUME_UNAVAILABLE"},
                {"id":"d","state":"failed","reason":"could not open /etc/x"}]"#,
        )
        .expect("list");
        for task in &tasks {
            assert_eq!(task.state, TaskState::Blocked(Blocked::Failed), "{task:?}");
        }
        let reasons: Vec<_> = tasks.iter().map(|t| t.activity.as_deref()).collect();
        assert_eq!(
            reasons,
            [
                Some("RESUME_UNAVAILABLE"),
                Some("PERMISSION_UNAVAILABLE"),
                Some("RESUME_UNAVAILABLE"),
                Some("failed"),
            ]
        );
        let deck = plan(&paired(1), &tasks, budget(1));
        assert!(deck
            .iter()
            .all(|e| e.state == AgentState::Error && e.card.is_none()));
    }

    #[test]
    fn nothing_from_a_title_path_or_token_reaches_the_json() {
        let tasks = parse(FIXTURE).expect("fixture");
        let cindy = paired(4100);
        let out = status_json(&cindy, &tasks, budget(1)).to_string();
        for leak in [
            "cargo", "rm -rf", "/tmp", "~/", "/home", "sk-live", "OPENAI", "db.rs", TOKEN,
        ] {
            assert!(!out.contains(leak), "{leak} leaked into {out}");
        }
        let deck = plan(&cindy, &tasks, budget(1));
        assert_eq!(deck.len(), 4, "idle sessions are absent");
        assert_eq!(deck[0].key, "s-03");
        assert_eq!(deck[0].state, AgentState::Error);
        assert_eq!(deck[0].card, None, "no approval card Cindy cannot answer");
    }

    #[test]
    fn workers_nested_or_flat_are_tasks_of_their_own() {
        let raw = json!({"result": {"sessions": [{
            "id": "root", "state": "running", "title": "Plan",
            "orca_workers": [
                {"id": "w1", "parent_session_id": "root", "state": "completed", "agent": "codex"},
                {"id": "w2", "parent_session_id": "root", "state": "running",
                 "orca_workers": [{"id": "w3", "state": "failed"}]},
            ],
        }]}})
        .to_string();
        let keys: Vec<String> = parse(&raw)
            .expect("tree")
            .into_iter()
            .map(|t| t.key)
            .collect();
        assert_eq!(keys, ["root", "w1", "w2", "w3"]);
    }

    #[test]
    fn an_empty_fleet_is_no_tasks() {
        assert_eq!(parse("[]").expect("array"), []);
        assert_eq!(parse(r#"{"sessions":[]}"#).expect("object"), []);
        assert_eq!(
            parse(r#"{"version":1,"id":"x","result":{"sessions":[]}}"#).expect("envelope"),
            []
        );
        assert!(plan(&paired(1), &[], budget(1)).is_empty());
    }

    #[test]
    fn a_banner_before_the_json_is_skipped() {
        let raw = format!("cindy local agent v1\n{FIXTURE}");
        assert_eq!(parse(&raw).expect("banner").len(), 6);
    }

    #[test]
    fn a_forty_session_fleet_fits_the_deck() {
        let sessions: Vec<Value> = (0..40)
            .map(|i| {
                json!({
                    "id": format!("s-{i}"),
                    "parent_session_id": if i == 0 { Value::Null } else { json!("s-0") },
                    "agent": "claude",
                    "state": if i % 4 == 0 { "running" } else { "RESUME_UNAVAILABLE" },
                    "title": format!("npm test --shard {i}"),
                })
            })
            .collect();
        let raw = json!({ "result": { "sessions": sessions } }).to_string();
        let tasks = parse(&raw).expect("fleet");
        assert_eq!(tasks.len(), 40);
        for configured in [1, 2, 5] {
            let deck = plan(&paired(1), &tasks, budget(configured));
            assert!(deck.len() <= CARDS_MAX && deck.len() < DECK_SLOTS);
            let summary = deck.last().expect("summary");
            assert_eq!(summary.state, AgentState::Question);
            assert!(
                summary.title.ends_with("waiting: Cindy"),
                "{}",
                summary.title
            );
        }
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        for raw in [
            "",
            "cindy: not running",
            "{\"sessions\": 3}",
            "{\"result\": 3}",
            "\"text\"",
            "{\"sessions\":[",
            "42",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} parsed");
        }
        // Sessions Cindy may grow: missing fields are skipped, not fatal.
        let tasks = parse(
            r#"[{"state":"running"},{"id":"../x","state":"done"},{"id":"ok"},{"id":"a","state":"thinking"}]"#,
        )
        .expect("partial");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Idle);
    }

    #[test]
    fn an_error_envelope_is_a_reason_word_without_the_message() {
        let err = parse(r#"{"version":1,"id":"x","error":{"code":"NOT_PAIRED","message":"see ~/.cindy/token"}}"#)
            .expect_err("error");
        assert!(err.starts_with("not_paired"), "{err}");
        assert!(!err.contains("~/"), "{err}");
        let err = parse(r#"{"error":{"code":"RESUME_UNAVAILABLE"}}"#).expect_err("error");
        assert!(err.starts_with("unavailable"), "{err}");
        let err = parse(r#"{"error":"rm -rf / please"}"#).expect_err("error");
        assert_eq!(err, "backend_refused: Cindy answered UNKNOWN");
    }

    #[test]
    fn with_no_token_status_and_orders_fail_as_not_paired() {
        for token in [None, Some(String::new()), Some("   ".to_string())] {
            // Port 1: were a request made, it would fail as not_running.
            let cindy = Cindy::with(Ok(endpoint(1)), token);
            let err = cindy.status().expect_err("unpaired");
            assert!(err.starts_with("not_paired"), "{err}");
            let err = cindy.send_order("s-01", "hi").expect_err("unpaired");
            assert!(err.starts_with("not_paired"), "{err}");
        }
    }

    #[test]
    fn the_order_text_lands_in_exactly_one_field_and_never_the_token() {
        let text = "ship it\"; rm -rf / && echo $(id) {\"method\":\"x\"}";
        let body = paired(4100).order_body("s-03", text);
        assert_eq!(body["version"], 1);
        assert_eq!(body["instance"], "inst-7");
        assert_eq!(body["method"], SEND_METHOD);
        assert_eq!(body["params"]["session_id"], "s-03");
        assert_eq!(body["params"]["text"], text);
        assert!(body.get("jsonrpc").is_none(), "not JSON-RPC");
        fn count(value: &Value, text: &str) -> usize {
            match value {
                Value::String(s) => usize::from(s.contains(text)),
                Value::Array(a) => a.iter().map(|v| count(v, text)).sum(),
                Value::Object(m) => m.values().map(|v| count(v, text)).sum(),
                _ => 0,
            }
        }
        assert_eq!(count(&body, "rm -rf"), 1);
        assert!(!body.to_string().contains(TOKEN));
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
        assert!(cindy.status().is_err_and(|e| e.starts_with("not_paired")));
        std::fs::write(&file, r#"{"protocol":9}"#).expect("write");
        let cindy = Cindy::from_discovery_file(&file, Some(TOKEN.into())).expect("adapter");
        assert!(cindy.status().is_err_and(|e| e.starts_with("unsupported")));
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
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"result\":[]}";
        assert_eq!(http_response(plain), Ok((200, "{\"result\":[]}".into())));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"res\r\n8\r\nult\":[]}\r\n0\r\n\r\n";
        assert_eq!(http_response(chunked), Ok((200, "{\"result\":[]}".into())));
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
    fn status_and_send_go_over_local_http() {
        let (port, server) = serve(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"version\":1,\"id\":\"x\",\"result\":{\"sessions\":[{\"id\":\"s-1\",\"state\":\"running\",\"title\":\"Plan\"}]}}",
        );
        let tasks = paired(port).status().expect("status");
        assert_eq!(tasks.len(), 1);
        let got = server.join().expect("server");
        assert!(got.starts_with("POST /rpc HTTP/1.1\r\n"));
        assert!(!got.to_ascii_lowercase().contains("origin:"));
        let (_, body) = got.split_once("\r\n\r\n").expect("body");
        let body: Value = serde_json::from_str(body).expect("json");
        assert_eq!(body["method"], LIST_METHOD);
        assert_eq!(body["instance"], "inst-7");

        let (port, server) =
            serve("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"version\":1,\"id\":\"x\",\"result\":{}}");
        paired(port).send_order("s-1", "go on").expect("send");
        let got = server.join().expect("server");
        assert!(got.contains("\"text\":\"go on\""), "{got}");

        let (port, server) = serve("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n{}");
        let err = paired(port).status().expect_err("refused");
        assert!(err.starts_with("not_paired"), "{err}");
        let _ = server.join();
    }
}
