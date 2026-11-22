//! One OpenAI-compatible `chat/completions` call, through curl.
//!
//! **Why curl and not a crate.** deckd has no TLS stack of its own: the
//! cloud voice, the Claude usage probe and the Grok Bot webhook all go
//! through the system curl, which is `/usr/bin/curl` on a Mac (part of the
//! OS, outside the signed bundle, nothing to sign or notarise), `curl.exe`
//! in `System32` on Windows 10 and later, and the one on `PATH` on Linux.
//! Measured on the Omarchy box (2026-11-20): a fresh curl costs ~6 ms to
//! spawn and initialise against loopback (6.8 ms per call against 0.6 ms
//! per request inside one curl process), and against a real HTTPS API the
//! handshake it cannot reuse is done 16 ms in, against ~260 ms for the
//! answer. Two calls per dictation therefore pay ~45 ms at most over a
//! pooled in-process client, against model answers in the hundreds of
//! milliseconds, and the bundle gains no TLS code.
//!
//! **What travels where.** The URL, the key and the body go to curl in a
//! config file on its stdin (`-K -`), never on its command line, so `ps`
//! shows none of them. HTTPS and plain HTTP are spoken, to any host
//! ([`url_problem`]): the owner chose to allow a model server on the LAN
//! over plain HTTP (2026-11-20), knowing that the key and the dictated
//! text then cross that network readable by anyone on it. curl follows no
//! redirect, so a key only ever goes to the host the owner configured.
//! Every call is bounded twice: curl's own `max-time`, and a kill from
//! here a little after it.
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How much of an answer is read. A rewrite is a line; a model that writes
/// pages has not answered the question.
const OUTPUT_MAX: u64 = 256 * 1024;
/// How much of a list is read (`GET /models`): OpenRouter's was 925 KB on
/// 2026-11-21, and a list cut short loses the status line curl writes
/// after it, which reads as no answer at all.
const LIST_MAX: u64 = 16 * 1024 * 1024;

/// One request.
pub struct Chat<'a> {
    /// The provider's base URL (`https://api.openai.com/v1`), or the full
    /// `…/chat/completions` URL.
    pub endpoint: &'a str,
    pub model: &'a str,
    /// `None` for a local server that takes none.
    pub key: Option<&'a str>,
    /// `(role, content)`.
    pub messages: Vec<(&'static str, String)>,
    pub max_tokens: u32,
}

/// Why a call gave no text. Words only: never the body, the key or the
/// answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The endpoint is not an `http://` or `https://` URL we can parse.
    Endpoint,
    /// curl could not be started.
    Spawn,
    /// The time ran out.
    Timeout,
    /// The server answered with this status.
    Status(u16),
    /// No answer at all: DNS, TLS, a refused connection (curl's exit code).
    Network(Option<i32>),
    /// A 2xx whose body is not a chat completion with text in it.
    Malformed,
    /// The provider needs a key and has none: no call was made.
    NoKey,
}

impl Failure {
    /// The word a log line carries.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Failure::Endpoint => "endpoint".into(),
            Failure::Spawn => "no_curl".into(),
            Failure::Timeout => "timeout".into(),
            Failure::Status(code) => format!("http_{code}"),
            Failure::Network(code) => format!("network_{}", code.unwrap_or(-1)),
            Failure::Malformed => "malformed".into(),
            Failure::NoKey => "no_key".into(),
        }
    }
}

/// `endpoint` as the URL to POST to: `…/chat/completions` appended unless it
/// is already there.
#[must_use]
pub fn url(endpoint: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// Why `url` cannot be called, or `None` when it can: `http://` or
/// `https://` (any case), a host, an optional numeric port, and nothing
/// that curl's config syntax or a log line could trip on.
#[must_use]
pub fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() || url.len() > 2048 {
        return Some("empty or longer than 2048 characters");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\') {
        return Some("a space, a quote or a control character");
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or("not http:// or https://");
    let rest = match rest {
        Ok(rest) => rest,
        Err(why) => return Some(why),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = match authority.strip_prefix('[') {
        // [v6]:port
        Some(v6) => match v6.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => return Some("an unclosed [ in the host"),
        },
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty()
        || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
    {
        return Some("no host, or a host with characters a host cannot have");
    }
    if port.is_some_and(|port| port.is_empty() || port.parse::<u16>().is_err()) {
        return Some("a port that is not a number from 0 to 65535");
    }
    None
}

/// The JSON body.
#[must_use]
pub fn body(chat: &Chat<'_>) -> String {
    let messages: Vec<serde_json::Value> = chat
        .messages
        .iter()
        .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
        .collect();
    let mut body = serde_json::json!({
        "model": chat.model,
        "messages": messages,
        "temperature": 0,
        "max_tokens": chat.max_tokens,
        "stream": false,
    });
    // As the PoC does (`omarchy_polish.Polisher.ask`): a rewrite is not a
    // thinking task, so no reasoning tokens before the answer — half the
    // wait on a reasoning model. OpenRouter's own field; OpenAI rejects it,
    // so it goes to OpenRouter only.
    if chat.endpoint.to_ascii_lowercase().contains("openrouter.ai") {
        body["reasoning"] = serde_json::json!({"enabled": false});
    }
    body.to_string()
}

/// One request, as the providers' adapters build it
/// (`crate::providers::wire`). `headers` are whole header lines
/// (`x-api-key: …`); they go to curl on stdin with everything else.
pub struct Request {
    /// `GET` or `POST`.
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<String>,
    /// A JSON body, for a `POST`.
    pub body: Option<String>,
}

/// The key never shows in `{:?}`: only the method and the URL's host.
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("host", &crate::grokbot::webhook::host(&self.url))
            .finish_non_exhaustive()
    }
}

/// The config file on curl's stdin for `request`.
fn config_for(request: &Request, timeout: Duration) -> String {
    use crate::grokbot::webhook::quoted;
    let secs = timeout.as_secs_f64().max(0.05);
    let mut cfg = String::new();
    cfg.push_str(&format!("url = {}\n", quoted(&request.url)));
    cfg.push_str(&format!("request = {}\n", quoted(request.method)));
    for header in &request.headers {
        cfg.push_str(&format!("header = {}\n", quoted(header)));
    }
    if let Some(body) = &request.body {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
        cfg.push_str("header = \"Expect:\"\n");
        cfg.push_str(&format!("data-binary = {}\n", quoted(body)));
    }
    cfg.push_str(&format!(
        "user-agent = \"openwalkietalkie-deckd/{}\"\n",
        env!("CARGO_PKG_VERSION")
    ));
    cfg.push_str(&format!(
        "proto = \"=https,http\"\nsilent\nshow-error\nconnect-timeout = {:.3}\nmax-time = {secs:.3}\nwrite-out = \"\\n%{{http_code}}\"\n",
        secs.min(1.5)
    ));
    cfg
}

/// The config file on curl's stdin for an OpenAI-style chat completion.
#[cfg(test)]
fn config(chat: &Chat<'_>, timeout: Duration) -> String {
    config_for(&chat_request(chat), timeout)
}

/// An OpenAI-style chat completion as a [`Request`].
fn chat_request(chat: &Chat<'_>) -> Request {
    let mut headers = Vec::new();
    if let Some(key) = chat.key.map(str::trim).filter(|key| !key.is_empty()) {
        headers.push(format!("Authorization: Bearer {key}"));
    }
    Request {
        method: "POST",
        url: url(chat.endpoint),
        headers,
        body: Some(body(chat)),
    }
}

/// Make an OpenAI-style chat completion, within `timeout`: its text.
pub fn chat(curl: &Path, chat: &Chat<'_>, timeout: Duration) -> Result<String, Failure> {
    let (status, body) = send(curl, &chat_request(chat), timeout)?;
    match status {
        code if (200..300).contains(&code) => content(&body).ok_or(Failure::Malformed),
        code => Err(Failure::Status(code)),
    }
}

/// Send `request` within `timeout`: the status and the body (at most
/// [`OUTPUT_MAX`] of it), whatever the status. Blocking.
pub fn send(curl: &Path, request: &Request, timeout: Duration) -> Result<(u16, String), Failure> {
    if url_problem(&request.url).is_some() {
        return Err(Failure::Endpoint);
    }
    let deadline = Instant::now() + timeout + Duration::from_millis(150);
    let mut command = Command::new(curl);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .args(["--disable", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure::Spawn)?;
    if let Some(mut stdin) = child.stdin.take() {
        // A write error is curl having gone already; its exit says why.
        let _ = stdin.write_all(config_for(request, timeout).as_bytes());
    }
    // Read on a thread of its own so that a full pipe never holds curl up
    // while this one watches the clock.
    let max = if request.method == "GET" { LIST_MAX } else { OUTPUT_MAX };
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(max).read_to_end(&mut out);
            out
        })
    });
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err(Failure::Network(None)),
        }
    };
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    // curl's own "operation timed out".
    if exit == Some(28) {
        return Err(Failure::Timeout);
    }
    let text = String::from_utf8_lossy(&out);
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    match status.trim().parse::<u16>() {
        Ok(code) if code >= 100 => Ok((code, body.to_string())),
        _ => Err(Failure::Network(exit)),
    }
}

/// The text of a chat completion: `choices[0].message.content`, with a
/// reasoning model's `<think>…</think>` taken off. `None` for anything else.
#[must_use]
pub fn content(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = value
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?;
    let text = match (text.find("<think>"), text.find("</think>")) {
        (Some(open), Some(close)) if open < close => {
            format!("{}{}", &text[..open], &text[close + "</think>".len()..])
        }
        _ => text.to_string(),
    };
    Some(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_gets_its_path_once() {
        assert_eq!(
            url("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            url("https://api.example.com/v1/"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            url("http://127.0.0.1:11434/v1/chat/completions"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn the_key_and_the_body_go_on_stdin_and_no_key_sends_no_header() {
        let mut chat = Chat {
            endpoint: "https://api.example.com/v1",
            model: "m",
            key: Some("sk-secret"),
            messages: vec![("user", "say \"hi\"".into())],
            max_tokens: 16,
        };
        let cfg = config(&chat, Duration::from_millis(2500));
        assert!(
            cfg.contains("header = \"Authorization: Bearer sk-secret\"\n"),
            "{cfg}"
        );
        assert!(cfg.contains("max-time = 2.500\n"), "{cfg}");
        assert!(cfg.contains("\\\"temperature\\\":0"), "{cfg}");
        chat.key = None;
        assert!(!config(&chat, Duration::from_secs(1)).contains("Authorization"));
    }

    #[test]
    fn openrouter_alone_is_asked_not_to_reason() {
        let chat = |endpoint| Chat { endpoint, model: "m", key: None, messages: vec![], max_tokens: 8 };
        let routed: serde_json::Value = serde_json::from_str(&body(&chat("https://openrouter.ai/api/v1"))).unwrap();
        assert_eq!(routed["reasoning"], serde_json::json!({"enabled": false}));
        let plain: serde_json::Value = serde_json::from_str(&body(&chat("https://api.openai.com/v1"))).unwrap();
        assert!(plain.get("reasoning").is_none());
    }

    #[test]
    fn a_completion_gives_its_text_and_anything_else_none() {
        let ok = r#"{"choices":[{"message":{"role":"assistant","content":" git status \n"}}]}"#;
        assert_eq!(content(ok).as_deref(), Some("git status"));
        let think = r#"{"choices":[{"message":{"content":"<think>hm</think>ls -la"}}]}"#;
        assert_eq!(content(think).as_deref(), Some("ls -la"));
        for bad in [
            "",
            "not json",
            "{}",
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{"content":null}}]}"#,
        ] {
            assert_eq!(content(bad), None, "{bad}");
        }
    }

    #[test]
    fn any_http_or_https_url_is_called_and_nothing_else() {
        for ok in [
            "http://192.168.1.10:11434/v1/chat/completions",
            "http://127.0.0.1:11434/v1/chat/completions",
            "HTTP://ollama.lan/v1/chat/completions",
            "https://api.example.com/v1/chat/completions",
            "http://[fe80::1]:8080/v1/chat/completions",
        ] {
            assert_eq!(url_problem(ok), None, "{ok}");
        }
        for bad in [
            "",
            "ftp://x/v1",
            "htp://x/v1",
            "http//x/v1",
            "http:/x/v1",
            "file:///etc/passwd",
            "http:///v1",
            "http://x:port/v1",
            "http://x:99999/v1",
            "http://bad host/v1",
            "http://x\"y/v1",
            "javascript:alert(1)",
        ] {
            assert!(url_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn a_url_we_cannot_parse_is_never_called() {
        let chat = |endpoint| Chat {
            endpoint,
            model: "m",
            key: None,
            messages: vec![],
            max_tokens: 1,
        };
        for endpoint in ["", "ftp://x", "htp://example.com/v1", "http://"] {
            let got = super::chat(Path::new("curl"), &chat(endpoint), Duration::from_secs(1));
            assert_eq!(got, Err(Failure::Endpoint), "{endpoint}");
        }
    }
}
