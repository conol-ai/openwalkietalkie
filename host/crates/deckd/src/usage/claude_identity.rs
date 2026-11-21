//! Which Claude account the status line's rate limits belong to.
//!
//! `deckd hook claude-statusline` labels the `rate_limits` Claude Code
//! hands its status line with the account's email. The numbers come from
//! the login the CLI is using, so the label must come from **that same
//! login**, never from a file another process keeps.
//!
//! Claude Code's `~/.claude.json` is such a file: a cache of the profile,
//! rewritten some time after `/login` replaces the credential. Reading the
//! email from it alone put the previous account's address on the new
//! account's percentages for a while after a switch.
//!
//! So the identity is keyed by the credential itself. Its `claudeAiOauth`
//! record carries `accountUuid` (read with the token, no network); the
//! email for that uuid is kept in `<caches>/claude-accounts.json`, which
//! only deckd writes. A render whose uuid is known costs a file read. A
//! uuid not seen before (a switched account) gets **no label** on that
//! render, and a detached `deckd hook claude-account` asks the profile
//! endpoint with that credential's own token, under a short timeout, and
//! records the answer for the renders after it. The status line never
//! waits on the network.
//!
//! `~/.claude.json` is still read, never written, as a fallback that costs
//! no request: only when its `oauthAccount.accountUuid` is the credential's,
//! so a stale copy cannot label another account.
//!
//! When the lookup fails (offline, 401, 429, timeout) the label stays off,
//! and the lookup is not tried again for [`RETRY_AFTER_S`]. A missing label
//! is honest; a wrong one is worse than none.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{account_label, unix_now};

/// The cache, in deckd's caches directory.
pub const CACHE_FILE: &str = "claude-accounts.json";
/// The hidden `deckd hook` word the status line starts the lookup with.
pub const LOOKUP_HOOK: &str = "claude-account";
/// curl's `--max-time` for the profile request.
pub const LOOKUP_TIMEOUT_S: u64 = 5;
/// A lookup started, or failed, this recently is not started again.
pub const RETRY_AFTER_S: u64 = 60;
/// How long the status line may wait on `security` for the Keychain item
/// (macOS, only where there is no credentials file).
const KEYCHAIN_WAIT: Duration = Duration::from_millis(1500);
/// Entries kept per list: a few profiles, never a growing file.
const KEEP: usize = 16;

/// The live credential: its token and the key its identity is cached by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    pub token: String,
    /// `accountUuid` when the record has one.
    pub uuid: Option<String>,
}

impl Credential {
    /// `{"claudeAiOauth": {"accessToken", "accountUuid", …}}`. Expiry is not
    /// checked: an expired token names its account all the same, and the
    /// lookup with it fails into "no label".
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Credential> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        let oauth = &value["claudeAiOauth"];
        let token = oauth["accessToken"].as_str().filter(|t| !t.is_empty())?;
        let uuid = oauth["accountUuid"]
            .as_str()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        Some(Credential { token: token.to_string(), uuid })
    }

    /// The one this process can see: `$CLAUDE_CONFIG_DIR` (the CLI's own,
    /// for the status-line helper) or `~/.claude`, then the Keychain.
    #[must_use]
    pub fn live() -> Option<Credential> {
        Credential::from_bytes(&super::claude::credentials::record(KEYCHAIN_WAIT)?)
    }

    /// The cache key: the account's uuid, or for a record too old to carry
    /// one, a digest of the token (a new token is a new lookup, never a
    /// guess). The token itself is never written.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.uuid {
            Some(uuid) => format!("uuid:{uuid}"),
            None => {
                let digest = Sha256::digest(self.token.as_bytes());
                let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
                format!("token:{hex}")
            }
        }
    }
}

/// `<caches>/claude-accounts.json`.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cache {
    /// Key → email, newest last.
    #[serde(default)]
    pub known: Vec<Known>,
    /// Key → when a lookup was last started for it.
    #[serde(default)]
    pub tried: Vec<Tried>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Known {
    pub key: String,
    pub email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tried {
    pub key: String,
    pub at: u64,
}

impl Cache {
    #[must_use]
    pub fn load(path: &Path) -> Cache {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write through a temporary file and a rename, so a reader in another
    /// process sees the old file or the new one, never half of either.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    #[must_use]
    pub fn email(&self, key: &str) -> Option<&str> {
        self.known.iter().find(|k| k.key == key).map(|k| k.email.as_str())
    }

    fn tried_since(&self, key: &str, now: u64) -> bool {
        self.tried
            .iter()
            .any(|t| t.key == key && now.saturating_sub(t.at) < RETRY_AFTER_S)
    }

    fn mark_tried(&mut self, key: &str, now: u64) {
        self.tried.retain(|t| t.key != key);
        self.tried.push(Tried { key: key.to_string(), at: now });
        trim(&mut self.tried);
    }

    fn remember(&mut self, key: &str, email: String) {
        self.known.retain(|k| k.key != key);
        self.known.push(Known { key: key.to_string(), email });
        trim(&mut self.known);
        self.tried.retain(|t| t.key != key);
    }
}

fn trim<T>(list: &mut Vec<T>) {
    let excess = list.len().saturating_sub(KEEP);
    list.drain(..excess);
}

/// What the status line does with a render.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The credential's account is known: label with this.
    Known(String),
    /// Unknown: no label, and start a lookup unless one is under way.
    Unknown { lookup: bool },
}

/// The decision, from what the helper can read without the network.
///
/// `claude_json` is Claude Code's `~/.claude.json`; its email counts only
/// when its `accountUuid` is the credential's.
#[must_use]
pub fn resolve(cred: Option<&Credential>, cache: &Cache, claude_json: Option<&Value>, now: u64) -> Resolution {
    let Some(cred) = cred else {
        // No credential, no account to name: whatever a cache says may be
        // anyone's.
        return Resolution::Unknown { lookup: false };
    };
    let key = cred.key();
    if let Some(email) = cache.email(&key).and_then(|e| account_label(Some(e))) {
        return Resolution::Known(email);
    }
    if let (Some(uuid), Some(json)) = (cred.uuid.as_deref(), claude_json) {
        let account = &json["oauthAccount"];
        if account["accountUuid"].as_str() == Some(uuid) {
            if let Some(email) = account_label(account["emailAddress"].as_str()) {
                return Resolution::Known(email);
            }
        }
    }
    Resolution::Unknown { lookup: !cache.tried_since(&key, now) }
}

/// The status-line helper's label for this render, from the credential it
/// can see. `caches` is deckd's caches directory (`None`: no cache, no
/// lookup). `start` launches the lookup and must return at once; it is
/// called only on a miss that has not been tried in the last minute.
pub fn statusline_label(
    cred: Option<&Credential>,
    claude_json: Option<&Value>,
    caches: Option<&Path>,
    now: u64,
    start: impl FnOnce(&Path) -> bool,
) -> Option<String> {
    let path = caches.map(|dir| dir.join(CACHE_FILE));
    let mut cache = path.as_deref().map(Cache::load).unwrap_or_default();
    match resolve(cred, &cache, claude_json, now) {
        Resolution::Known(email) => Some(email),
        Resolution::Unknown { lookup } => {
            if let (true, Some(cred), Some(dir), Some(path)) = (lookup, cred, caches, path.as_deref()) {
                // Marked before the start, so the next renders (a status
                // line redraws often) do not start a second one.
                cache.mark_tried(&cred.key(), now);
                if cache.save(path).is_ok() {
                    start(dir);
                }
            }
            None
        }
    }
}

/// The helper's whole job, with the real files: the credential in the
/// CLI's environment, Claude Code's `.claude.json` beside it (read only),
/// and the lookup as a detached `deckd hook claude-account`.
#[must_use]
pub fn statusline_account(caches: Option<&Path>) -> Option<String> {
    let cred = Credential::live();
    let claude_json = claude_json();
    statusline_label(cred.as_ref(), claude_json.as_ref(), caches, unix_now(), spawn_lookup)
}

/// `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` when set).
fn claude_json() -> Option<Value> {
    let path = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => crate::integrations::home_dir().ok()?.join(".claude.json"),
    };
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Start `deckd hook claude-account <caches>` and do not wait for it. Its
/// standard streams are closed, so the CLI reading the status line's
/// output is not held open by it either. It inherits this environment,
/// `CLAUDE_CONFIG_DIR` included, so it reads the same credential.
fn spawn_lookup(caches: &Path) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .arg("hook")
        .arg(LOOKUP_HOOK)
        .arg(caches)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// `deckd hook claude-account <caches>`: ask the profile endpoint which
/// account the live credential is, with that credential's token, and
/// record the email under its key. Any failure records nothing.
pub fn lookup_main(caches: Option<&str>) {
    let (Some(dir), Some(cred)) = (caches.filter(|c| !c.is_empty()), Credential::live()) else {
        return;
    };
    let answer = super::claude::fetch_blocking(&cred.token, super::claude::PROFILE_URL, LOOKUP_TIMEOUT_S);
    let path = Path::new(dir).join(CACHE_FILE);
    let mut cache = Cache::load(&path);
    if record_lookup(&mut cache, &cred, answer) {
        let _ = cache.save(&path);
    }
}

/// Take the profile endpoint's answer into the cache. Only a 200 that
/// names an email does, and only when the profile is the credential's own
/// account (a profile naming another uuid is a login that changed between
/// the read and the request: dropped, the next render looks again).
/// Returns whether the cache changed.
pub fn record_lookup(cache: &mut Cache, cred: &Credential, answer: Result<(u16, String), String>) -> bool {
    let Ok((200, body)) = answer else {
        return false;
    };
    let Ok(profile) = serde_json::from_str::<Value>(&body) else {
        return false;
    };
    if let (Some(ours), Some(theirs)) = (cred.uuid.as_deref(), profile["account"]["uuid"].as_str()) {
        if ours != theirs {
            return false;
        }
    }
    let Some(email) = super::claude::profile_account(&profile) else {
        return false;
    };
    cache.remember(&cred.key(), email);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    const NOW: u64 = 1_800_000_000;

    fn cred(uuid: &str) -> Credential {
        Credential { token: format!("tok-{uuid}"), uuid: Some(uuid.into()) }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deckd-claude-identity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Claude Code's cache, still naming the account signed in before.
    fn stale_claude_json() -> Value {
        serde_json::json!({"oauthAccount": {"accountUuid": "old-uuid", "emailAddress": "old@example.com"}})
    }

    #[test]
    fn the_credential_record_gives_the_token_and_the_account_uuid() {
        let record = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","expiresAt":1000,"accountUuid":"u-1"}}"#;
        assert_eq!(
            Credential::from_bytes(record),
            Some(Credential { token: "sk-ant-oat01-x".into(), uuid: Some("u-1".into()) })
        );
        let old = Credential::from_bytes(br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x"}}"#).unwrap();
        assert_eq!(old.uuid, None);
        // Keyed by a digest, never by the token itself.
        assert!(old.key().starts_with("token:") && !old.key().contains("sk-ant"));
        assert_eq!(cred("u-1").key(), "uuid:u-1");
        assert_eq!(Credential::from_bytes(br#"{"claudeAiOauth":{"accessToken":""}}"#), None);
        assert_eq!(Credential::from_bytes(b"not json"), None);
    }

    /// The bug: `/login` switched to a new account, `.claude.json` and
    /// deckd's cache still name the old one. Neither may label the numbers.
    #[test]
    fn a_switched_account_is_not_labelled_with_the_old_email() {
        let dir = temp_dir("switched");
        let mut cache = Cache::default();
        cache.remember(&cred("old-uuid").key(), "old@example.com".into());
        cache.save(&dir.join(CACHE_FILE)).unwrap();
        let new = cred("new-uuid");
        let started = AtomicUsize::new(0);
        let label = statusline_label(Some(&new), Some(&stale_claude_json()), Some(&dir), NOW, |_| {
            started.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(label, None, "the old account's email must not label the new account");
        assert_eq!(started.load(Ordering::SeqCst), 1, "the new uuid is looked up");
        // The next renders, before the lookup answers: still no label, and
        // no second lookup.
        for _ in 0..3 {
            let label = statusline_label(Some(&new), Some(&stale_claude_json()), Some(&dir), NOW + 5, |_| {
                started.fetch_add(1, Ordering::SeqCst);
                true
            });
            assert_eq!(label, None);
        }
        assert_eq!(started.load(Ordering::SeqCst), 1);
        // The lookup answers with the new account's profile: labelled.
        let mut cache = Cache::load(&dir.join(CACHE_FILE));
        let profile = r#"{"account":{"uuid":"new-uuid","email":"new@example.com"}}"#;
        assert!(record_lookup(&mut cache, &new, Ok((200, profile.into()))));
        cache.save(&dir.join(CACHE_FILE)).unwrap();
        let label = statusline_label(Some(&new), Some(&stale_claude_json()), Some(&dir), NOW + 6, |_| panic!("no lookup"));
        assert_eq!(label.as_deref(), Some("new@example.com"));
        // And switching back finds the old one without a request.
        let label = statusline_label(Some(&cred("old-uuid")), None, Some(&dir), NOW + 7, |_| panic!("no lookup"));
        assert_eq!(label.as_deref(), Some("old@example.com"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Business as usual: the credential's uuid is known, so a render costs
    /// no request at all, from deckd's cache or from a `.claude.json`
    /// that agrees with the credential.
    #[test]
    fn a_known_uuid_costs_no_lookup() {
        let dir = temp_dir("known");
        let mut cache = Cache::default();
        cache.remember(&cred("u-1").key(), "me@example.com".into());
        cache.save(&dir.join(CACHE_FILE)).unwrap();
        let label = statusline_label(Some(&cred("u-1")), None, Some(&dir), NOW, |_| panic!("no lookup on a hit"));
        assert_eq!(label.as_deref(), Some("me@example.com"));

        let empty = temp_dir("known-json");
        let json = serde_json::json!({"oauthAccount": {"accountUuid": "old-uuid", "emailAddress": "old@example.com"}});
        let label = statusline_label(Some(&cred("old-uuid")), Some(&json), Some(&empty), NOW, |_| panic!("no lookup when .claude.json agrees"));
        assert_eq!(label.as_deref(), Some("old@example.com"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// Offline, refused, throttled, timed out or nonsense: nothing is
    /// recorded, the label stays off, and the next attempt waits a minute.
    #[test]
    fn a_failed_lookup_leaves_no_label() {
        let new = cred("new-uuid");
        for answer in [
            Err("curl exit 28: Operation timed out after 5001 milliseconds".to_string()),
            Err("curl exit 6: Could not resolve host".to_string()),
            Ok((401, String::new())),
            Ok((429, String::new())),
            Ok((200, "not json".to_string())),
            Ok((200, r#"{"account":{}}"#.to_string())),
            // A profile for another account than the credential's.
            Ok((200, r#"{"account":{"uuid":"other","email":"other@example.com"}}"#.to_string())),
        ] {
            let mut cache = Cache::default();
            cache.mark_tried(&new.key(), NOW);
            assert!(!record_lookup(&mut cache, &new, answer.clone()), "{answer:?}");
            assert_eq!(resolve(Some(&new), &cache, Some(&stale_claude_json()), NOW + 1), Resolution::Unknown { lookup: false });
            assert_eq!(
                resolve(Some(&new), &cache, None, NOW + RETRY_AFTER_S),
                Resolution::Unknown { lookup: true },
                "tried again after a minute"
            );
        }
        // No credential at all: nothing to name, nothing to look up.
        assert_eq!(resolve(None, &Cache::default(), Some(&stale_claude_json()), NOW), Resolution::Unknown { lookup: false });
    }

    /// The lookup is started, not waited on: a profile request that hangs
    /// does not hold the render, which answers at once without a label.
    #[test]
    fn a_slow_lookup_does_not_block_the_status_line() {
        let dir = temp_dir("slow");
        let begun = Instant::now();
        let label = statusline_label(Some(&cred("new-uuid")), None, Some(&dir), NOW, |_| {
            // Stands in for the detached child stuck on the network.
            std::thread::spawn(|| std::thread::sleep(Duration::from_secs(30)));
            true
        });
        assert_eq!(label, None);
        assert!(begun.elapsed() < Duration::from_secs(1), "{:?}", begun.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_stays_small() {
        let mut cache = Cache::default();
        for i in 0..40 {
            cache.remember(&format!("uuid:{i}"), format!("{i}@example.com"));
            cache.mark_tried(&format!("uuid:x{i}"), NOW);
        }
        assert_eq!(cache.known.len(), KEEP);
        assert_eq!(cache.tried.len(), KEEP);
        assert_eq!(cache.email("uuid:39"), Some("39@example.com"));
        assert_eq!(cache.email("uuid:0"), None);
    }
}
