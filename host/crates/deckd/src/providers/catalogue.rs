//! What models a provider offers: a curated list per kind that is always
//! there, and the provider's own list fetched with its key and cached.
//!
//! The curated list is the floor: a fetch that fails, a provider with no
//! key yet, a first start with no network — the picker still has models to
//! offer. The fetched list is added after it, and the cache says when it
//! was fetched, so a picker can say "built-in" or "updated <when>".
//!
//! The cache is `caches/providers/models.json` in the service's own caches
//! directory, written whole through a temp file and a rename, mode 0600. It
//! holds model ids and times: no key, nothing dictated.
use super::Kind;
use crate::config::Paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The models a kind offers before anything is fetched. Kept short and
/// conservative: a model a provider has retired is the fetched list's to
/// drop, and a wrong id here is a model the check (`wire::test`) reports.
#[must_use]
pub const fn builtin(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::OpenAi => &["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano", "gpt-4o-mini-tts", "tts-1"],
        Kind::OpenRouter => &[
            "deepseek/deepseek-v4.1-flash-1017",
            "openai/gpt-4o-mini",
            "google/gemini-2.5-flash",
        ],
        Kind::Anthropic => &["claude-haiku-5-5", "claude-sonnet-5-5"],
        Kind::ElevenLabs => &["eleven_flash_v2_5", "eleven_multilingual_v2"],
        Kind::Ollama | Kind::Omarchy | Kind::Custom => &[],
    }
}

/// One provider's fetched list.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Fetched {
    /// When, in milliseconds since the epoch.
    pub at_ms: u64,
    /// The provider's kind and base URL then: a list fetched from another
    /// place is not this provider's any more.
    pub kind: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// The cache file: provider id → its last fetched list.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Cache {
    #[serde(default)]
    pub providers: BTreeMap<String, Fetched>,
}

/// The cache's path for a service on `paths`.
#[must_use]
pub fn cache_path(paths: &Paths) -> PathBuf {
    paths.caches.join("providers").join("models.json")
}

impl Cache {
    /// The cache at `path`, empty when it is missing or unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Cache {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write the cache whole, 0600.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let dir = path.parent().ok_or("the cache has no directory")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!(".models.json.{}.tmp", std::process::id()));
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut out = options.open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            out.write_all(text.as_bytes())
                .and_then(|()| out.sync_all())
                .map_err(|e| format!("{}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Where a list came from, for a picker to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The curated list alone.
    BuiltIn,
    /// The curated list and the provider's own, fetched then.
    Updated { at_ms: u64 },
}

/// A provider's models: the curated ones first, in their order, then the
/// fetched ones the curated list does not have, sorted. A fetched list from
/// another kind or URL than the provider's now is ignored.
#[must_use]
pub fn merged(kind: Kind, base_url: &str, fetched: Option<&Fetched>) -> (Vec<String>, Source) {
    let mut models: Vec<String> = builtin(kind).iter().map(|m| (*m).to_string()).collect();
    let fetched = fetched.filter(|f| f.kind == kind.word() && f.base_url == base_url);
    let Some(fetched) = fetched else {
        return (models, Source::BuiltIn);
    };
    let mut more: Vec<String> = fetched
        .models
        .iter()
        .filter(|m| !models.contains(m))
        .cloned()
        .collect();
    more.sort();
    more.dedup();
    models.extend(more);
    (models, Source::Updated { at_ms: fetched.at_ms })
}

/// What a refresh did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refreshed {
    /// The provider's list was fetched and cached: how many models.
    Fetched(usize),
    /// The fetch failed (its word); the cache was left as it was.
    Failed(String),
}

/// Fetch provider `id`'s list with `fetch` and cache it under the cache at
/// `path`. A failure leaves the previous list, so the picker keeps what it
/// had.
pub fn refresh(
    path: &Path,
    id: &str,
    kind: Kind,
    base_url: &str,
    now_ms: u64,
    fetch: impl FnOnce() -> Result<Vec<String>, crate::context::http::Failure>,
) -> Refreshed {
    match fetch() {
        Ok(models) => {
            let mut cache = Cache::load(path);
            let count = models.len();
            cache.providers.insert(
                id.to_string(),
                Fetched {
                    at_ms: now_ms,
                    kind: kind.word().into(),
                    base_url: base_url.into(),
                    models,
                },
            );
            match cache.save(path) {
                Ok(()) => Refreshed::Fetched(count),
                Err(why) => Refreshed::Failed(why),
            }
        }
        Err(failure) => Refreshed::Failed(failure.word()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::http::Failure;
    use crate::integrations::testdir::TestDir;

    #[test]
    fn the_built_in_list_is_the_floor_and_a_fetch_is_added_after_it() {
        let base = "https://openrouter.ai/api/v1";
        let (models, source) = merged(Kind::OpenRouter, base, None);
        assert_eq!(source, Source::BuiltIn);
        assert_eq!(models[0], "deepseek/deepseek-v4.1-flash-1017");
        let fetched = Fetched {
            at_ms: 1_795_000_000_000,
            kind: "openrouter".into(),
            base_url: base.into(),
            models: vec!["z/last".into(), "openai/gpt-4o-mini".into(), "a/first".into(), "a/first".into()],
        };
        let (models, source) = merged(Kind::OpenRouter, base, Some(&fetched));
        assert_eq!(source, Source::Updated { at_ms: 1_795_000_000_000 });
        assert_eq!(&models[..3], builtin(Kind::OpenRouter).iter().map(|m| m.to_string()).collect::<Vec<_>>().as_slice());
        assert_eq!(&models[3..], ["a/first", "z/last"], "the curated first, then the rest sorted, once each");
        // A list from another place is not this provider's.
        let (models, source) = merged(Kind::OpenRouter, "https://elsewhere/v1", Some(&fetched));
        assert_eq!((models.len(), source), (3, Source::BuiltIn));
        // A custom server has no curated list: only what it says.
        assert!(merged(Kind::Custom, "http://x/v1", None).0.is_empty());
    }

    #[test]
    fn a_refresh_caches_the_list_and_a_failure_keeps_the_last_one() {
        let dir = TestDir::new("provider-catalogue");
        let path = dir.path().join("caches/providers/models.json");
        let base = "http://127.0.0.1:1/v1";
        assert_eq!(refresh(&path, "local", Kind::Custom, base, 10, || Ok(vec!["m1".into(), "m2".into()])), Refreshed::Fetched(2));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(refresh(&path, "local", Kind::Custom, base, 20, || Err(Failure::Timeout)), Refreshed::Failed("timeout".into()));
        let cache = Cache::load(&path);
        let (models, source) = merged(Kind::Custom, base, cache.providers.get("local"));
        assert_eq!((models, source), (vec!["m1".to_string(), "m2".to_string()], Source::Updated { at_ms: 10 }));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("key"));
    }
}
