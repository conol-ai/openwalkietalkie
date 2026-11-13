//! The Claude usage probe: `GET https://api.anthropic.com/api/oauth/usage`
//! with the Claude Code login, the call Zeron and the usage menu-bar apps
//! make. It is not a documented API, so the answer is read defensively
//! (`super::claude_oauth_windows`) and every failure is a log line and a
//! longer wait, never a crash or a hot loop.
//!
//! **The login** is Claude Code's own `claudeAiOauth` record: the
//! `~/.claude/.credentials.json` file where Claude writes one, else the
//! macOS Keychain item `Claude Code-credentials`, read through
//! `/usr/bin/security` (the tool Claude Code writes it with, so already on
//! the item's access list: no dialog, whichever build of deckd asks). A
//! refusal is honoured for an hour, so a dialog does not come back every
//! poll. The token is used for the usage and account-profile requests and
//! is never logged or written.
//!
//! **The request** goes through `/usr/bin/curl` rather than a TLS stack
//! of our own: the daemon has none, and curl is on every Mac. The token
//! travels in a config file on curl's stdin (`-K -`), never on its
//! command line where `ps` would show it.
//!
//! **The cadence**: `[usage] poll_s` (5 min by default, 60 s at least —
//! the endpoint rate-limits eager clients with 429s), at once on the
//! deck's ENTER (`usage.refresh_requested`) when the last probe is older
//! than 30 s, doubling from 1 min to 15 min after a failure, and 10 min
//! between looks for a login that is missing or expired.
use std::io::Write;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deck_model::EventKind;
use tokio::sync::{broadcast, watch};

use super::{claude_oauth_windows, unix_now, Provider, Window};
use crate::api::Api;
use crate::core::handle::CoreMsg;

pub const URL: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// The beta header the OAuth endpoints require.
const BETA: &str = "oauth-2025-04-20";
/// The Keychain item Claude Code writes its login into on macOS.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// Below this `[usage] poll_s` is raised to it.
const POLL_MIN: Duration = Duration::from_secs(60);
/// ENTER probes again only when the last probe is older than this.
const REFRESH_GAP: Duration = Duration::from_secs(30);
const BACKOFF_FIRST: Duration = Duration::from_secs(60);
const BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
/// Between looks when there is no login to use, or the one there is has
/// expired and Claude Code has not refreshed it yet.
const NO_LOGIN_RETRY: Duration = Duration::from_secs(10 * 60);
/// After the operator denied the Keychain dialog.
const DENIED_RETRY: Duration = Duration::from_secs(60 * 60);
/// How long `security` may wait on a dialog before it counts as denied.
#[cfg(target_os = "macos")]
const SECURITY_TIMEOUT: Duration = Duration::from_secs(120);
/// `curl --max-time`, and the wait on the child on top of it.
const HTTP_TIMEOUT_S: u64 = 20;
const CHILD_TIMEOUT: Duration = Duration::from_secs(HTTP_TIMEOUT_S + 10);

/// What one probe came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Windows were handed to the core.
    Fed(usize),
    /// No `claudeAiOauth` record anywhere this daemon looks.
    NoLogin,
    /// The record is there but past its `expiresAt`: Claude Code refreshes
    /// it the next time it runs.
    Expired,
    /// The operator denied the Keychain dialog.
    Denied,
    /// The endpoint refused the token (401/403).
    Refused(u16),
    /// 429.
    Throttled,
    /// Anything else: curl missing, no network, an answer with no windows.
    Failed(String),
}

/// The probe's clock: when it last ran and how long it is waiting now.
#[derive(Debug)]
struct Pace {
    last: Option<Instant>,
    next: Instant,
    backoff: Duration,
    /// The last outcome's kind, so a repeated failure is one log line.
    told: Option<&'static str>,
}

impl Pace {
    fn new(now: Instant) -> Self {
        Pace {
            last: None,
            next: now,
            backoff: BACKOFF_FIRST,
            told: None,
        }
    }

    /// The wait after `outcome`, and whether it deserves a log line.
    fn settle(&mut self, outcome: &Outcome, poll: Duration, now: Instant) -> bool {
        self.last = Some(now);
        let (wait, kind) = match outcome {
            Outcome::Fed(_) => {
                self.backoff = BACKOFF_FIRST;
                (poll.max(POLL_MIN), "fed")
            }
            Outcome::NoLogin => (NO_LOGIN_RETRY, "no_login"),
            Outcome::Expired => (NO_LOGIN_RETRY, "expired"),
            Outcome::Denied => (DENIED_RETRY, "denied"),
            Outcome::Refused(_) => (NO_LOGIN_RETRY, "refused"),
            Outcome::Throttled | Outcome::Failed(_) => {
                let wait = self.backoff;
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                (wait, "failed")
            }
        };
        self.next = now + wait;
        let fresh = self.told != Some(kind);
        self.told = Some(kind);
        fresh
    }

    /// ENTER on the deck: probe now unless one just ran.
    fn nudge(&mut self, now: Instant) {
        if self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= REFRESH_GAP)
        {
            self.next = now;
        }
    }
}

/// The probe task: one per daemon, from start to `stop`.
pub async fn run(api: Arc<Api>, mut stop: watch::Receiver<bool>) {
    let mut events = api.bus.subscribe();
    let mut pace = Pace::new(Instant::now());
    loop {
        let now = Instant::now();
        let cfg = api.config.load();
        let enabled = Provider::Claude.enabled(&cfg.usage);
        if enabled && pace.next <= now {
            let outcome = probe(&api).await;
            if matches!(outcome, Outcome::NoLogin | Outcome::Expired | Outcome::Denied | Outcome::Refused(_)) {
                let _ = feed(&api, Vec::new(), None, Vec::new()).await;
            }
            let poll = Duration::from_secs(u64::from(cfg.usage.poll_s));
            let fresh = pace.settle(&outcome, poll, Instant::now());
            report(&outcome, fresh, pace.next.saturating_duration_since(Instant::now()));
            continue;
        }
        let wait = if enabled {
            pace.next.saturating_duration_since(now)
        } else {
            // Off in the config: look again in a while, a reload may have
            // switched it on.
            POLL_MIN
        };
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(wait) => {}
            published = events.recv() => match published {
                Ok(p) if p.event.kind == EventKind::UsageRefreshRequested => {
                    pace.nudge(Instant::now());
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => events = events.resubscribe(),
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

fn report(outcome: &Outcome, fresh: bool, next_in: Duration) {
    let next_s = next_in.as_secs();
    match outcome {
        Outcome::Fed(n) => tracing::debug!(windows = n, next_s, "usage: Claude windows fed"),
        Outcome::NoLogin if fresh => tracing::info!(
            next_s,
            "usage: no Claude Code login to probe with (sign in with `claude`, or drop \"claude\" from [usage] providers)"
        ),
        Outcome::Expired if fresh => tracing::info!(
            next_s,
            "usage: the Claude Code login has expired; it is refreshed the next time `claude` runs"
        ),
        Outcome::Denied if fresh => tracing::warn!(
            next_s,
            "usage: Keychain access to the Claude Code login was denied; the Claude bars stay empty (drop \"claude\" from [usage] providers to stop asking)"
        ),
        Outcome::Refused(status) if fresh => {
            tracing::info!(status, next_s, "usage: the Claude usage endpoint refused the login");
        }
        Outcome::Throttled if fresh => {
            tracing::info!(next_s, "usage: the Claude usage endpoint is rate limiting this daemon");
        }
        Outcome::Failed(why) if fresh => tracing::warn!(why, next_s, "usage: the Claude probe failed"),
        _ => tracing::debug!(?outcome, next_s, "usage: Claude probe"),
    }
}

/// One probe: the login, the request, the windows to the core.
async fn probe(api: &Api) -> Outcome {
    // Visible in the log while the Keychain dialog waits for an answer.
    tracing::debug!("usage: reading the Claude Code login");
    let login = match tokio::task::spawn_blocking(credentials::login).await {
        Ok(login) => login,
        Err(e) => return Outcome::Failed(format!("credentials task: {e}")),
    };
    let token = match login {
        Login::Token(token) => token,
        Login::None => return Outcome::NoLogin,
        Login::Expired => return Outcome::Expired,
        Login::Denied => return Outcome::Denied,
    };
    let (status, body) = match fetch(&token, URL).await {
        Ok(answer) => answer,
        Err(why) => return Outcome::Failed(why),
    };
    match status {
        200 => {}
        401 | 403 => return Outcome::Refused(status),
        429 => return Outcome::Throttled,
        other => return Outcome::Failed(format!("HTTP {other}")),
    }
    let value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(format!("not JSON: {e}")),
    };
    let windows = claude_oauth_windows(&value);
    if windows.is_empty() {
        return Outcome::Failed("an answer with no usage windows in it".into());
    }
    // Use this exact token for the profile as well: another profile or a
    // newly switched login must not label these percentages.
    let account = match fetch(&token, PROFILE_URL).await {
        Ok((200, body)) => serde_json::from_str(&body).ok().and_then(|v| profile_account(&v)),
        _ => None,
    };
    let resets = super::claude_reset_grants(&value);
    feed(api, windows, account, resets).await
}

async fn feed(api: &Api, windows: Vec<Window>, account: Option<String>, resets: Vec<super::ResetGrant>) -> Outcome {
    let n = windows.len();
    match api
        .core
        .send(CoreMsg::Usage {
            partial: false,
            resets: Some(resets),
            provider: Provider::Claude,
            windows,
            account,
        })
        .await
    {
        Ok(()) => Outcome::Fed(n),
        Err(e) => Outcome::Failed(format!("core: {e}")),
    }
}

/// `GET` the endpoint through curl: the status code and the body. The
/// child runs on a blocking thread (the runtime has no `process`
/// feature); curl's own `--max-time` bounds it, and the wait here is
/// bounded again on top.
async fn fetch(token: &str, url: &str) -> Result<(u16, String), String> {
    let config = curl_config(token, url);
    let run = tokio::task::spawn_blocking(move || -> Result<std::process::Output, String> {
        let mut child = std::process::Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                &HTTP_TIMEOUT_S.to_string(),
                "--config",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.as_bytes())
                .map_err(|e| format!("curl stdin: {e}"))?;
            drop(stdin);
        }
        child
            .wait_with_output()
            .map_err(|e| format!("curl: {e}"))
    });
    let out = tokio::time::timeout(CHILD_TIMEOUT, run)
        .await
        .map_err(|_| "curl did not finish".to_string())?
        .map_err(|e| format!("curl task: {e}"))??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "curl exit {}: {}",
            out.status.code().unwrap_or(-1),
            err.trim().lines().last().unwrap_or_default()
        ));
    }
    split_answer(&String::from_utf8_lossy(&out.stdout))
}

/// The config file on curl's stdin: the URL, the headers, and the status
/// code written after the body on its own line.
fn curl_config(token: &str, url: &str) -> String {
    format!(
        "url = \"{url}\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: {BETA}\"\n\
         header = \"Accept: application/json\"\n\
         user-agent = \"openwalkietalkie-deckd/{}\"\n\
         write-out = \"\\n%{{http_code}}\"\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// `<body>\n<status>` → `(status, body)`.
fn split_answer(text: &str) -> Result<(u16, String), String> {
    let text = text.trim_end_matches(['\r', '\n']);
    let (body, code) = text
        .rsplit_once('\n')
        .map_or(("", text), |(body, code)| (body, code));
    let status = code
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("no status code in curl's output ({} bytes)", text.len()))?;
    Ok((status, body.to_string()))
}

/// What the credentials lookup found.
#[derive(Debug, PartialEq, Eq)]
pub enum Login {
    Token(String),
    None,
    Expired,
    Denied,
}

/// The authenticated profile and Claude Code's cached profile spell the
/// email differently. Only these identity fields become a display label.
pub fn profile_account(value: &serde_json::Value) -> Option<String> {
    super::account_label(value["account"]["email"].as_str()
        .or_else(|| value["account"]["email_address"].as_str())
        .or_else(|| value["oauthAccount"]["emailAddress"].as_str()))
}

/// The status-line helper runs in the CLI's own profile environment.
/// Read its cached identity there, rather than guessing from the daemon's
/// default profile. Never fall back to another profile when unavailable.
pub fn statusline_account() -> Option<String> {
    let path = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir).join(".claude.json"),
        None => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".claude.json"),
    };
    let bytes = std::fs::read(path).ok()?;
    profile_account(&serde_json::from_slice(&bytes).ok()?)
}

/// Where Claude Code's login is read from.
pub mod credentials {
    use super::Login;

    /// The file, then the Keychain.
    pub fn login() -> Login {
        match file() {
            Login::None => keychain(),
            found => found,
        }
    }

    /// `~/.claude/.credentials.json` (`$CLAUDE_CONFIG_DIR` first when the
    /// daemon has one).
    pub fn file() -> Login {
        let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".claude")));
        let Some(dir) = dir else {
            return Login::None;
        };
        match std::fs::read(dir.join(".credentials.json")) {
            Ok(bytes) => from_bytes(&bytes),
            Err(_) => Login::None,
        }
    }

    /// The `Claude Code-credentials` item of the login keychain, under
    /// this user's account name, **read through `/usr/bin/security`**.
    ///
    /// Claude Code writes the item with that tool, so `security` is on the
    /// item's access list and is let in without a dialog. Asking the
    /// Security framework from this process instead makes macOS check *this*
    /// binary against the list on every read: a daemon that outlived a
    /// Sparkle update (its executable deleted, its path a staging directory)
    /// or a build that is not signed like the app fails the check, so the
    /// dialog came back at every poll and "Always Allow" never stuck.
    #[cfg(target_os = "macos")]
    pub fn keychain() -> Login {
        let Some(account) = account() else {
            return Login::None;
        };
        let child = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", super::KEYCHAIN_SERVICE, "-a", &account, "-w"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return Login::Denied;
        };
        // A dialog the operator never answers must not hold the probe forever.
        let deadline = std::time::Instant::now() + super::SECURITY_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Login::Denied;
                }
            }
        };
        let mut out = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        }
        from_security(status.code(), &out)
    }

    /// What `security find-generic-password -w` came to: its exit status and
    /// the secret it printed.
    #[cfg(any(target_os = "macos", test))]
    pub fn from_security(code: Option<i32>, stdout: &[u8]) -> Login {
        match code {
            Some(0) => from_bytes(stdout.trim_ascii()),
            // `errSecItemNotFound` (44): no login on this Mac. Anything else
            // is a refusal or a locked keychain, and is not asked again soon.
            Some(44) => Login::None,
            _ => Login::Denied,
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn keychain() -> Login {
        Login::None
    }

    #[cfg(target_os = "macos")]
    fn account() -> Option<String> {
        if let Some(user) = std::env::var_os("USER").filter(|u| !u.is_empty()) {
            return user.into_string().ok();
        }
        // SAFETY: `getpwuid` returns a pointer to static storage or null;
        // the name is copied out before anything else could call it.
        unsafe {
            let pw = libc::getpwuid(libc::getuid());
            if pw.is_null() || (*pw).pw_name.is_null() {
                return None;
            }
            Some(std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned())
        }
    }

    /// `{"claudeAiOauth": {"accessToken", "expiresAt": <ms>, …}}`.
    pub fn from_bytes(bytes: &[u8]) -> Login {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return Login::None;
        };
        let oauth = &value["claudeAiOauth"];
        let Some(token) = oauth["accessToken"].as_str().filter(|t| !t.is_empty()) else {
            return Login::None;
        };
        if let Some(expires_ms) = oauth["expiresAt"].as_f64() {
            #[allow(clippy::cast_precision_loss)]
            let now_ms = super::unix_now() as f64 * 1000.0;
            if expires_ms <= now_ms {
                return Login::Expired;
            }
        }
        Login::Token(token.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_security_tools_answer_maps_to_a_login() {
        use credentials::from_security;
        let live = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-x","expiresAt":{}}}}}"#,
            (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 3_600_000)
        );
        let with_newline = format!("{live}\n");
        assert_eq!(from_security(Some(0), with_newline.as_bytes()), Login::Token("sk-ant-oat01-x".into()));
        assert_eq!(from_security(Some(44), b""), Login::None);
        assert_eq!(from_security(Some(36), b""), Login::Denied);
        assert_eq!(from_security(None, b""), Login::Denied);
    }

    #[test]
    fn profiles_supply_only_the_email_and_missing_identity_stays_unknown() {
        for profile in [
            serde_json::json!({"account": {"email": "person@example.com"}}),
            serde_json::json!({"account": {"email_address": "person@example.com"}}),
            serde_json::json!({"oauthAccount": {"emailAddress": "person@example.com", "accountUuid": "private-id"}}),
        ] {
            assert_eq!(profile_account(&profile), Some("person@example.com".into()));
        }
        assert_eq!(profile_account(&serde_json::json!({"claudeAiOauth": {"accessToken": "secret"}})), None);
        assert_eq!(profile_account(&serde_json::Value::Null), None);
        let config = curl_config("test-token", PROFILE_URL);
        assert!(config.starts_with(&format!("url = \"{PROFILE_URL}\"\n")));
    }

    #[test]
    fn the_credentials_record_is_read_and_its_expiry_honoured() {
        let live = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-x","refreshToken":"r","expiresAt":{},"scopes":["user:inference"],"subscriptionType":"max"}}}}"#,
            (unix_now() + 3600) * 1000
        );
        assert_eq!(credentials::from_bytes(live.as_bytes()), Login::Token("sk-ant-oat01-x".into()));
        let stale = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","expiresAt":1000}}"#;
        assert_eq!(credentials::from_bytes(stale.as_bytes()), Login::Expired);
        for none in [
            r#"{"claudeAiOauth":{"accessToken":""}}"#,
            r#"{"apiKey":"sk-ant-api03-x"}"#,
            "not json",
            "",
        ] {
            assert_eq!(credentials::from_bytes(none.as_bytes()), Login::None, "{none}");
        }
    }

    #[test]
    fn curls_output_splits_into_status_and_body() {
        assert_eq!(
            split_answer("{\"five_hour\":{}}\n200\n"),
            Ok((200, "{\"five_hour\":{}}".to_string()))
        );
        assert_eq!(split_answer("\n429"), Ok((429, String::new())));
        assert_eq!(split_answer("000"), Ok((0, String::new())));
        assert!(split_answer("").is_err());
        assert!(split_answer("{\"a\":1}").is_err());
        // The token goes in the config, after the URL, never in argv.
        let config = curl_config("tok-123", URL);
        assert!(config.starts_with(&format!("url = \"{URL}\"\n")));
        assert!(config.contains("header = \"Authorization: Bearer tok-123\"\n"));
        assert!(config.contains("anthropic-beta: oauth-2025-04-20"));
        assert!(config.ends_with("write-out = \"\\n%{http_code}\"\n"));
    }

    #[test]
    fn the_pace_backs_off_on_failure_and_rests_without_a_login() {
        let t0 = Instant::now();
        let poll = Duration::from_secs(300);
        let mut pace = Pace::new(t0);
        assert!(pace.next <= t0);
        // A fed probe waits the poll; a repeat says nothing new.
        assert!(pace.settle(&Outcome::Fed(2), poll, t0));
        assert_eq!(pace.next, t0 + poll);
        assert!(!pace.settle(&Outcome::Fed(2), poll, t0));
        // A poll below the floor is raised to it.
        pace.settle(&Outcome::Fed(2), Duration::from_secs(5), t0);
        assert_eq!(pace.next, t0 + POLL_MIN);
        // Failures double from a minute to the cap, and a success resets.
        assert!(pace.settle(&Outcome::Failed("x".into()), poll, t0));
        assert_eq!(pace.next, t0 + BACKOFF_FIRST);
        assert!(!pace.settle(&Outcome::Throttled, poll, t0));
        assert_eq!(pace.next, t0 + BACKOFF_FIRST * 2);
        for _ in 0..8 {
            pace.settle(&Outcome::Failed("x".into()), poll, t0);
        }
        assert_eq!(pace.next, t0 + BACKOFF_MAX);
        assert!(pace.settle(&Outcome::Fed(1), poll, t0));
        assert!(pace.settle(&Outcome::Failed("x".into()), poll, t0));
        assert_eq!(pace.next, t0 + BACKOFF_FIRST);
        // No login: a long rest, and a denial a longer one.
        pace.settle(&Outcome::NoLogin, poll, t0);
        assert_eq!(pace.next, t0 + NO_LOGIN_RETRY);
        pace.settle(&Outcome::Denied, poll, t0);
        assert_eq!(pace.next, t0 + DENIED_RETRY);
        // ENTER brings the next probe forward, unless one just ran.
        pace.nudge(t0 + Duration::from_secs(5));
        assert_eq!(pace.next, t0 + DENIED_RETRY);
        pace.nudge(t0 + REFRESH_GAP);
        assert_eq!(pace.next, t0 + REFRESH_GAP);
    }
}
