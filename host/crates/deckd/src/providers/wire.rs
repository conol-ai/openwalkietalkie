//! The providers' protocols, behind one interface: a completion of a
//! system prompt and some turns ([`complete`]), the provider's own model
//! list ([`models`]), and a check that a provider answers ([`test`]).
//!
//! * **OpenAI shape** (`openai`, `openrouter`, `ollama`, `custom`):
//!   `POST {base}/chat/completions` with `Authorization: Bearer <key>` (none
//!   when there is no key), the system prompt as the first message, the
//!   answer in `choices[0].message.content`; `GET {base}/models` →
//!   `data[].id`. OpenRouter is asked not to reason first, as the PoC did.
//! * **Anthropic's Messages API** (`anthropic`): `POST {base}/messages` with
//!   `x-api-key` and `anthropic-version: 2023-06-01`, the system prompt a
//!   top-level `system` field, `max_tokens` required, the answer the `text`
//!   blocks of `content`; `GET {base}/models` → `data[].id`.
//!
//! Every call goes through the same curl transport
//! (`crate::context::http::send`): the key and the body on curl's stdin,
//! never on its command line; a hard timeout; nothing of the text or the key
//! in a log line or an error.
use super::{Kind, Target, Wire};
use crate::context::http::{self, Failure, Request};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

/// The Messages API version this speaks.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One completion: the target, its key, a system prompt and the turns that
/// follow it (`("user" | "assistant", text)`).
pub struct Ask<'a> {
    pub target: &'a Target,
    pub key: Option<&'a str>,
    pub system: &'a str,
    pub turns: &'a [(&'static str, String)],
    pub max_tokens: u32,
}

/// Why a call has no answer, beyond the transport's own [`Failure`]s.
fn no_key(target: &Target, key: Option<&str>) -> bool {
    target.kind.needs_key() && key.is_none_or(|key| key.trim().is_empty())
}

/// The request [`complete`] sends.
#[must_use]
pub fn completion_request(ask: &Ask<'_>) -> Request {
    let key = ask.key.map(str::trim).filter(|key| !key.is_empty());
    match ask.target.kind.wire() {
        Wire::OpenAi => {
            let mut messages = vec![json!({"role": "system", "content": ask.system})];
            messages.extend(ask.turns.iter().map(|(role, text)| json!({"role": role, "content": text})));
            let mut body = json!({
                "model": ask.target.model,
                "messages": messages,
                "temperature": 0,
                "max_tokens": ask.max_tokens,
                "stream": false,
            });
            if ask.target.kind == Kind::OpenRouter {
                body["reasoning"] = json!({"enabled": false});
            }
            Request {
                method: "POST",
                url: format!("{}/chat/completions", ask.target.base_url),
                headers: key.map(|key| vec![format!("Authorization: Bearer {key}")]).unwrap_or_default(),
                body: Some(body.to_string()),
            }
        }
        Wire::Anthropic => {
            let messages: Vec<Value> = ask
                .turns
                .iter()
                .map(|(role, text)| json!({"role": role, "content": text}))
                .collect();
            let body = json!({
                "model": ask.target.model,
                "system": ask.system,
                "messages": messages,
                "max_tokens": ask.max_tokens,
                "temperature": 0,
            });
            let mut headers = vec![format!("anthropic-version: {ANTHROPIC_VERSION}")];
            if let Some(key) = key {
                headers.push(format!("x-api-key: {key}"));
            }
            Request {
                method: "POST",
                url: format!("{}/messages", ask.target.base_url),
                headers,
                body: Some(body.to_string()),
            }
        }
    }
}

/// The answer's text, from the shape `wire` answers in.
#[must_use]
pub fn answer_text(wire: Wire, body: &str) -> Option<String> {
    match wire {
        Wire::OpenAi => http::content(body),
        Wire::Anthropic => {
            let value: Value = serde_json::from_str(body).ok()?;
            let text: String = value
                .get("content")?
                .as_array()?
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            Some(text.trim().to_string()).filter(|text| !text.is_empty())
        }
    }
}

/// One completion within `timeout`: the answer's text. A provider that needs
/// a key and has none fails at once, quietly, with no call made.
pub fn complete(curl: &Path, ask: &Ask<'_>, timeout: Duration) -> Result<String, Failure> {
    if no_key(ask.target, ask.key) {
        return Err(Failure::NoKey);
    }
    let (status, body) = http::send(curl, &completion_request(ask), timeout)?;
    match status {
        code if (200..300).contains(&code) => answer_text(ask.target.kind.wire(), &body).ok_or(Failure::Malformed),
        code => Err(Failure::Status(code)),
    }
}

/// The request [`models`] sends.
#[must_use]
pub fn models_request(kind: Kind, base_url: &str, key: Option<&str>) -> Request {
    let key = key.map(str::trim).filter(|key| !key.is_empty());
    let headers = match kind.wire() {
        Wire::OpenAi => key.map(|key| vec![format!("Authorization: Bearer {key}")]).unwrap_or_default(),
        Wire::Anthropic => {
            let mut headers = vec![format!("anthropic-version: {ANTHROPIC_VERSION}")];
            if let Some(key) = key {
                headers.push(format!("x-api-key: {key}"));
            }
            headers
        }
    };
    let url = if kind == Kind::Anthropic {
        format!("{base_url}/models?limit=1000")
    } else {
        format!("{base_url}/models")
    };
    Request {
        method: "GET",
        url,
        headers,
        body: None,
    }
}

/// The model ids in a `/models` answer (`data[].id`), in its order.
#[must_use]
pub fn model_ids(body: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(body).ok()?;
    let ids = value
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    Some(ids)
}

/// The provider's own model list, within `timeout`.
pub fn models(
    curl: &Path,
    kind: Kind,
    base_url: &str,
    key: Option<&str>,
    timeout: Duration,
) -> Result<Vec<String>, Failure> {
    let target = Target {
        key_of: String::new(),
        kind,
        base_url: base_url.to_string(),
        model: String::new(),
        local: false,
        timeout: None,
    };
    if no_key(&target, key) {
        return Err(Failure::NoKey);
    }
    let (status, body) = http::send(curl, &models_request(kind, base_url, key), timeout)?;
    match status {
        code if (200..300).contains(&code) => model_ids(&body).ok_or(Failure::Malformed),
        code => Err(Failure::Status(code)),
    }
}

/// What a provider check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// It answers, and knows the model when one was named.
    Ok,
    /// It needs a key and has none.
    NoKey,
    /// It refused the key (401, 403).
    BadKey,
    /// No answer: DNS, TLS, a refused connection, a timeout.
    Unreachable(String),
    /// It answers, and does not have the model named.
    WrongModel,
    /// It answered in a way that is none of the above (a 5xx, a body that
    /// is not a list).
    Failed(String),
}

impl Check {
    /// The word the CLI and the API say.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Check::Ok => "ok",
            Check::NoKey => "no_key",
            Check::BadKey => "bad_key",
            Check::Unreachable(_) => "unreachable",
            Check::WrongModel => "wrong_model",
            Check::Failed(_) => "failed",
        }
    }
}

/// Check a provider, cheaply and with no dictated text: its model list,
/// and whether `model` is in it. A server with no list (`404` on
/// `/models`) is asked for a one-token completion of the fixed word
/// "ping" instead, when a model is named.
pub fn test(
    curl: &Path,
    kind: Kind,
    base_url: &str,
    key: Option<&str>,
    model: Option<&str>,
    timeout: Duration,
) -> Check {
    let classify = |failure: Failure| match failure {
        Failure::NoKey => Check::NoKey,
        Failure::Status(401 | 403) => Check::BadKey,
        Failure::Status(code) if code >= 500 => Check::Failed(format!("http_{code}")),
        Failure::Status(code) => Check::Failed(format!("http_{code}")),
        Failure::Timeout | Failure::Network(_) | Failure::Spawn | Failure::Endpoint => {
            Check::Unreachable(failure.word())
        }
        Failure::Malformed => Check::Failed("malformed".into()),
    };
    match models(curl, kind, base_url, key, timeout) {
        Ok(ids) => match model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(model) if !ids.is_empty() && !ids.iter().any(|id| id == model) => Check::WrongModel,
            _ => Check::Ok,
        },
        Err(Failure::Status(404)) if model.is_some_and(|m| !m.trim().is_empty()) => {
            let target = Target {
                key_of: String::new(),
                kind,
                base_url: base_url.to_string(),
                model: model.unwrap_or_default().trim().to_string(),
                local: false,
                timeout: None,
            };
            let turns = [("user", "ping".to_string())];
            let ask = Ask {
                target: &target,
                key,
                system: "Reply with one word.",
                turns: &turns,
                max_tokens: 1,
            };
            match complete(curl, &ask, timeout) {
                Ok(_) | Err(Failure::Malformed) => Check::Ok,
                Err(Failure::Status(404)) => Check::WrongModel,
                Err(failure) => classify(failure),
            }
        }
        Err(failure) => classify(failure),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// What the stub saw of one request.
    #[derive(Clone, Debug, Default)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub headers: Vec<String>,
        pub body: String,
    }

    /// A loopback stub that answers every request with `answer(method,
    /// path, headers, body) -> (status, body)` and records what it saw.
    pub fn stub(
        answer: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let log = log.clone();
                let answer = answer.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let (mut length, mut headers) = (0usize, Vec::new());
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                        let lower = header.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        headers.push(header.trim().to_string());
                    }
                    let mut body = vec![0u8; length];
                    let _ = reader.read_exact(&mut body);
                    let request = Seen { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() };
                    let (status, reply) = answer(&request);
                    log.lock().unwrap().push(request);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                });
            }
        });
        (base, seen)
    }

    /// A stub speaking OpenAI's shape: a model list, and a completion that
    /// answers `text`; 401 without the right key.
    pub fn openai_stub(text: &'static str, key: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
        stub(move |seen| {
            if !seen.headers.iter().any(|h| h == &format!("Authorization: Bearer {key}")) {
                return (401, "{\"error\":{\"message\":\"no\"}}".into());
            }
            match (seen.method.as_str(), seen.path.as_str()) {
                ("GET", "/v1/models") => (200, json!({"object": "list", "data": [{"id": "small"}, {"id": "large"}]}).to_string()),
                ("POST", "/v1/chat/completions") => (200, json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string()),
                _ => (404, "{}".into()),
            }
        })
    }

    /// A stub speaking Anthropic's Messages API.
    pub fn anthropic_stub(text: &'static str, key: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
        stub(move |seen| {
            let has = |want: &str| seen.headers.iter().any(|h| h.eq_ignore_ascii_case(want));
            if !has(&format!("x-api-key: {key}")) {
                return (401, json!({"type": "error", "error": {"type": "authentication_error"}}).to_string());
            }
            if !has(&format!("anthropic-version: {ANTHROPIC_VERSION}")) {
                return (400, "{}".into());
            }
            match (seen.method.as_str(), seen.path.split('?').next().unwrap_or("")) {
                ("GET", "/v1/models") => (200, json!({"data": [{"id": "claude-haiku-5-5", "type": "model"}], "has_more": false}).to_string()),
                ("POST", "/v1/messages") => (200, json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "text", "text": text}],
                    "stop_reason": "end_turn"
                }).to_string()),
                _ => (404, "{}".into()),
            }
        })
    }

    fn target(kind: Kind, base: &str, model: &str) -> Target {
        Target { key_of: "p".into(), kind, base_url: base.into(), model: model.into(), local: false, timeout: None }
    }

    const CURL: &str = "curl";

    #[test]
    fn the_openai_shape_sends_a_bearer_and_the_system_prompt_first() {
        let (base, seen) = openai_stub("git status", "sk-openai");
        let target = target(Kind::Custom, &base, "small");
        let turns = [("user", "Transcript:\nget status".to_string())];
        let ask = Ask { target: &target, key: Some("sk-openai"), system: "SYS", turns: &turns, max_tokens: 64 };
        assert_eq!(complete(Path::new(CURL), &ask, Duration::from_secs(3)).unwrap(), "git status");
        let seen = seen.lock().unwrap();
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["messages"][0], json!({"role": "system", "content": "SYS"}));
        assert_eq!(body["messages"][1]["content"], "Transcript:\nget status");
        assert!(body.get("system").is_none());
    }

    #[test]
    fn the_anthropic_shape_sends_its_headers_and_a_top_level_system() {
        let (base, seen) = anthropic_stub("git status", "sk-ant");
        let target = target(Kind::Anthropic, &base, "claude-haiku-5-5");
        let turns = [("user", "Transcript:\nget status".to_string())];
        let ask = Ask { target: &target, key: Some("sk-ant"), system: "SYS", turns: &turns, max_tokens: 64 };
        assert_eq!(complete(Path::new(CURL), &ask, Duration::from_secs(3)).unwrap(), "git status");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].path, "/v1/messages");
        assert!(!seen[0].headers.iter().any(|h| h.to_ascii_lowercase().starts_with("authorization")));
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["system"], "SYS");
        assert_eq!(body["messages"], json!([{"role": "user", "content": "Transcript:\nget status"}]));
        assert_eq!(body["max_tokens"], 64);
        // The answer is its text blocks.
        assert_eq!(answer_text(Wire::Anthropic, r#"{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":" ok "}]}"#).as_deref(), Some("ok"));
        assert_eq!(answer_text(Wire::Anthropic, r#"{"content":[]}"#), None);
    }

    #[test]
    fn no_key_fails_at_once_and_quietly_with_no_call_made() {
        let (base, seen) = openai_stub("x", "k");
        let target = target(Kind::OpenAi, &base, "m");
        let turns = [("user", "hello".to_string())];
        for key in [None, Some("  ")] {
            let ask = Ask { target: &target, key, system: "S", turns: &turns, max_tokens: 8 };
            assert_eq!(complete(Path::new(CURL), &ask, Duration::from_secs(3)), Err(Failure::NoKey));
        }
        assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
        // A local server needs none.
        let local = target_kind(Kind::Ollama, &base);
        let turns = [("user", "hello".to_string())];
        let ask = Ask { target: &local, key: None, system: "S", turns: &turns, max_tokens: 8 };
        assert_eq!(complete(Path::new(CURL), &ask, Duration::from_secs(3)), Err(Failure::Status(401)), "sent, and the stub wants a key");
    }

    fn target_kind(kind: Kind, base: &str) -> Target {
        target(kind, base, "m")
    }

    #[test]
    fn a_check_tells_a_bad_key_a_wrong_model_and_an_unreachable_provider_apart() {
        let curl = Path::new(CURL);
        let t = Duration::from_secs(3);
        let (base, seen) = openai_stub("x", "good");
        assert_eq!(test(curl, Kind::Custom, &base, Some("good"), Some("small"), t), Check::Ok);
        assert_eq!(test(curl, Kind::Custom, &base, Some("good"), Some("huge"), t), Check::WrongModel);
        assert_eq!(test(curl, Kind::Custom, &base, Some("bad"), None, t), Check::BadKey);
        assert_eq!(test(curl, Kind::OpenRouter, &base, None, None, t), Check::NoKey);
        assert!(seen.lock().unwrap().iter().all(|s| s.method == "GET"), "a check sends no text");
        let (ant, _) = anthropic_stub("x", "good");
        assert_eq!(test(curl, Kind::Anthropic, &ant, Some("good"), Some("claude-haiku-5-5"), t), Check::Ok);
        assert_eq!(test(curl, Kind::Anthropic, &ant, Some("bad"), None, t), Check::BadKey);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dead = format!("http://127.0.0.1:{port}/v1");
        assert!(matches!(test(curl, Kind::Custom, &dead, None, None, t), Check::Unreachable(_)));
        // A server with no /models: one token of "ping".
        let (bare, seen) = stub(|seen| match seen.path.as_str() {
            "/v1/chat/completions" => (200, json!({"choices": [{"message": {"content": "pong"}}]}).to_string()),
            _ => (404, "{}".into()),
        });
        assert_eq!(test(curl, Kind::Custom, &bare, None, Some("m"), t), Check::Ok);
        assert!(seen.lock().unwrap()[1].body.contains("\"ping\""));
    }
}
