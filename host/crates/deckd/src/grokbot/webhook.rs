//! The POST to a Grok Bot routine's webhook.
//!
//! A routine's **Webhook** section gives a URL ("POST to") and a key that
//! senders put in `Authorization: Bearer <key>`. A `200` means Grok Bot
//! accepted the call and started a run, not that the bot has finished; any
//! other answer means no run started (a paused routine, a key that changed).
//!
//! **The request goes through curl**, like every other request the daemon
//! makes (`audio::cloud`, `usage::claude`): the daemon has no TLS stack of
//! its own. The URL, the key and the body travel in a config file on curl's
//! stdin (`-K -`), never on its command line, so `ps` shows none of them.
//! Only HTTPS is spoken, and plain HTTP to this machine's own loopback (the
//! tests' stand-in); curl follows no redirect, so a key is only ever sent to
//! the host the owner pasted.
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// curl gives up on a webhook after this long, connecting included.
pub const MAX_TIME_S: u32 = 15;
const CONNECT_TIMEOUT_S: u32 = 5;
/// How much of an answer is read: the status is on its last line, and a
/// webhook's body is a line of JSON.
const OUTPUT_MAX: usize = 64 * 1024;

/// One call to a webhook.
#[derive(Clone, PartialEq, Eq)]
pub struct Post {
    pub url: String,
    pub key: String,
    /// The JSON body.
    pub body: String,
}

impl std::fmt::Debug for Post {
    /// The host only: the key is a secret and the body is content (§11).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Post")
            .field("host", &host(&self.url))
            .finish_non_exhaustive()
    }
}

/// What became of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A 2xx: Grok Bot started a run.
    Accepted(u16),
    /// Any other status: no run started.
    Refused(u16),
    /// No answer at all: the network, a timeout, a host that does not exist.
    Failed {
        /// curl's exit code, when it ran.
        exit: Option<i32>,
        /// curl's own words (never the key: it is not on any line curl
        /// prints).
        why: String,
    },
}

impl Outcome {
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(self, Outcome::Accepted(_))
    }

    /// The status, when there was one.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Outcome::Accepted(status) | Outcome::Refused(status) => Some(*status),
            Outcome::Failed { .. } => None,
        }
    }

    /// The word an undelivered order is reported under
    /// (`agent.order.result`'s `reason`, `^[a-z0-9_]{1,32}$`): `None` when
    /// the call was accepted.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Outcome::Accepted(_) => None,
            Outcome::Refused(status) => Some(format!("webhook_{status}")),
            // curl's 28 is "operation timed out".
            Outcome::Failed { exit: Some(28), .. } => Some("webhook_timeout".into()),
            Outcome::Failed { .. } => Some("webhook_unreachable".into()),
        }
    }
}

/// The URL's host, for a log line or a status row: no path, no query (a
/// routine's URL carries its identity in its path).
#[must_use]
pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // No user info, should a pasted URL carry one.
    authority.rsplit('@').next().unwrap_or("").to_string()
}

/// Why `url` cannot be a webhook, or `None` when it can: HTTPS, or plain
/// HTTP to this machine's loopback; a host; nothing curl's config syntax or
/// a log line could trip on.
#[must_use]
pub fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("the webhook URL is empty");
    }
    if url.len() > 2048 {
        return Some("the webhook URL is longer than 2048 characters");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\') {
        return Some("the webhook URL has a space, a quote or a control character in it");
    }
    let lower = url.to_ascii_lowercase();
    let host = host(url);
    if host.is_empty() {
        return Some("the webhook URL names no host");
    }
    if lower.starts_with("https://") {
        return None;
    }
    let bare = host.rsplit_once(':').map_or(host.as_str(), |(name, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            name
        } else {
            host.as_str()
        }
    });
    if lower.starts_with("http://") && matches!(bare, "127.0.0.1" | "localhost" | "[::1]") {
        return None;
    }
    Some("the webhook URL must start with https://")
}

/// A value inside the double quotes of a curl config line.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The config file on curl's stdin: the request, a status line written
/// after the body (`write-out`), and limits that end a dead call.
#[must_use]
pub fn curl_config(post: &Post) -> String {
    let mut cfg = String::new();
    cfg.push_str(&format!("url = {}\n", quoted(&post.url)));
    cfg.push_str("request = \"POST\"\n");
    cfg.push_str(&format!(
        "header = {}\n",
        quoted(&format!("Authorization: Bearer {}", post.key.trim()))
    ));
    cfg.push_str("header = \"Content-Type: application/json\"\n");
    cfg.push_str("header = \"Expect:\"\n");
    cfg.push_str(&format!("data-binary = {}\n", quoted(&post.body)));
    cfg.push_str(&format!(
        "user-agent = \"openwalkietalkie-deckd/{}\"\n",
        env!("CARGO_PKG_VERSION")
    ));
    cfg.push_str(&format!(
        "proto = \"=https,http\"\n\
         silent\n\
         show-error\n\
         connect-timeout = {CONNECT_TIMEOUT_S}\n\
         max-time = {MAX_TIME_S}\n\
         write-out = \"\\n%{{http_code}}\"\n"
    ));
    cfg
}

/// The system curl on macOS and Windows, the one on `PATH` elsewhere.
#[must_use]
pub fn curl() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
            .join("System32/curl.exe")
    }
    #[cfg(not(windows))]
    {
        let system = Path::new("/usr/bin/curl");
        if cfg!(target_os = "macos") && system.exists() {
            system.to_path_buf()
        } else {
            PathBuf::from("curl")
        }
    }
}

/// The status on the last line of curl's output (`write-out`), `None` for
/// `000` (no answer) or no such line.
#[must_use]
pub fn status_of(output: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(output);
    let last = text.trim_end().rsplit('\n').next()?.trim();
    last.parse::<u16>().ok().filter(|status| (100..600).contains(status))
}

/// Make the call. Blocking: the module runs it on the blocking pool.
#[must_use]
pub fn send(curl: &Path, post: &Post) -> Outcome {
    if let Some(problem) = url_problem(&post.url) {
        return Outcome::Failed {
            exit: None,
            why: problem.to_string(),
        };
    }
    let mut command = Command::new(curl);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = command
        .args(["--disable", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            return Outcome::Failed {
                exit: None,
                why: format!("{}: {e}", curl.display()),
            }
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A write error is curl having gone already; its exit says why.
        let _ = stdin.write_all(curl_config(post).as_bytes());
    }
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(OUTPUT_MAX as u64).read_to_end(&mut out);
    }
    let mut err = String::new();
    if let Some(stderr) = child.stderr.take() {
        let _ = stderr.take(4096).read_to_string(&mut err);
    }
    let exit = child.wait().ok().and_then(|status| status.code());
    match status_of(&out) {
        Some(status) if (200..300).contains(&status) => Outcome::Accepted(status),
        Some(status) => Outcome::Refused(status),
        None => Outcome::Failed {
            exit,
            why: err.lines().next().unwrap_or("no answer").trim().to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> Post {
        Post {
            url: "https://hooks.example.com/routines/abc?x=1".into(),
            key: " sk-secret ".into(),
            body: r#"{"text":"say \"hi\"\nnow"}"#.into(),
        }
    }

    #[test]
    fn the_key_and_the_body_travel_on_stdin_quoted_for_curl() {
        let cfg = curl_config(&post());
        assert!(cfg.contains("url = \"https://hooks.example.com/routines/abc?x=1\"\n"));
        assert!(cfg.contains("header = \"Authorization: Bearer sk-secret\"\n"));
        // JSON's own escapes survive curl's quoting.
        assert!(cfg.contains(r#"data-binary = "{\"text\":\"say \\\"hi\\\"\\nnow\"}""#), "{cfg}");
        assert!(cfg.contains("proto = \"=https,http\"\n"));
        assert!(cfg.contains("write-out = \"\\n%{http_code}\"\n"));
        assert!(cfg.contains("max-time = 15\n"));
        // Debug shows the host and nothing else.
        let shown = format!("{:?}", post());
        assert!(shown.contains("hooks.example.com") && !shown.contains("secret") && !shown.contains("abc"));
    }

    #[test]
    fn only_https_or_this_machine_is_a_webhook() {
        assert_eq!(url_problem("https://cursor.com/api/hooks/xyz"), None);
        assert_eq!(url_problem("http://127.0.0.1:47555/hook"), None);
        assert_eq!(url_problem("http://localhost/hook"), None);
        assert!(url_problem("http://example.com/hook").is_some());
        assert!(url_problem("file:///etc/passwd").is_some());
        assert!(url_problem("https://").is_some());
        assert!(url_problem("https://a.example/x y").is_some());
        assert!(url_problem("https://a.example/\"x").is_some());
        assert!(url_problem("").is_some());
        assert_eq!(host("https://user:pw@hooks.example.com:8443/a/b?c"), "hooks.example.com:8443");
    }

    #[test]
    fn the_status_is_the_last_line() {
        assert_eq!(status_of(b"{\"ok\":true}\n200"), Some(200));
        assert_eq!(status_of(b"\n401"), Some(401));
        assert_eq!(status_of(b"\n000"), None);
        assert_eq!(status_of(b""), None);
        assert_eq!(Outcome::Refused(401).reason().as_deref(), Some("webhook_401"));
        assert_eq!(Outcome::Accepted(200).reason(), None);
        let timeout = Outcome::Failed { exit: Some(28), why: "timed out".into() };
        assert_eq!(timeout.reason().as_deref(), Some("webhook_timeout"));
        let gone = Outcome::Failed { exit: Some(6), why: "no host".into() };
        assert_eq!(gone.reason().as_deref(), Some("webhook_unreachable"));
    }

    /// The real curl against a listener on this machine: the method, the
    /// bearer header and the body arrive as sent, and the status comes back.
    #[test]
    fn a_post_reaches_a_listener_with_its_key_and_body() {
        use std::io::{BufRead as _, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push(line.trim_end().to_string());
            }
            let length: usize = head
                .iter()
                .find_map(|h| h.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; length];
            std::io::Read::read_exact(&mut reader, &mut body).unwrap();
            let mut stream = stream;
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno")
                .unwrap();
            (head, String::from_utf8(body).unwrap())
        });
        let outcome = send(
            &curl(),
            &Post {
                url: format!("http://127.0.0.1:{port}/routine"),
                key: "k-123".into(),
                body: r#"{"text":"hello"}"#.into(),
            },
        );
        let (head, body) = server.join().unwrap();
        assert_eq!(outcome, Outcome::Refused(401));
        assert_eq!(head[0], "POST /routine HTTP/1.1");
        assert!(head.iter().any(|h| h == "Authorization: Bearer k-123"), "{head:?}");
        assert!(head.iter().any(|h| h == "Content-Type: application/json"));
        assert_eq!(body, r#"{"text":"hello"}"#);
    }
}
