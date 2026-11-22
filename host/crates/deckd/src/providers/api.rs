//! `providers.*` from a connection (the app's page, next round; `deckd
//! providers` uses the same functions directly):
//!
//! * `providers.list` → every provider, checked, with its key's hint;
//! * `providers.models {provider}` → its models and where they came from;
//! * `providers.refresh {provider}` → its own list fetched with its key;
//! * `providers.test {provider, model?}` → `ok`, `no_key`, `bad_key`,
//!   `unreachable`, `wrong_model` or `failed`;
//! * `providers.key.set {provider, key}` / `providers.key.clear {provider}`.
//!
//! **No call returns a key**: what leaves is its hint (length and at most
//! the last four characters) and where it is kept. Answered on the blocking
//! pool: a key store can wait on a dialog, a fetch on the network.
use super::catalogue::{self, Refreshed, Source};
use super::keys::ProviderKeys;
use super::{omarchy, wire, Provider};
use crate::config::{Config, Paths};
use deck_model::ApiError;
use serde_json::{json, Value};
use std::time::Duration;

/// How long a fetch or a check may take.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);

/// `providers.<verb>` from a connection.
pub async fn api_dispatch(
    core: &crate::core::handle::CoreHandle,
    verb: &str,
    params: Value,
) -> Result<Value, ApiError> {
    let call = Call::parse(verb, &params)?;
    let paths = core.care_context().await?.paths;
    match tokio::task::spawn_blocking(move || {
        let config = Config::load(&paths.config).map_err(|e| ApiError::invalid("config.toml", &e.to_string()))?;
        call.answer(&paths, &config, &ProviderKeys::home(&paths), &crate::grokbot::webhook::curl())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(ApiError::internal(&format!("the providers task failed: {e}"))),
    }
}

/// One checked call.
pub enum Call {
    List,
    Models { id: String },
    Refresh { id: String },
    Test { id: String, model: Option<String> },
    KeySet { id: String, key: String },
    KeyClear { id: String },
}

/// The key never shows in `{:?}`.
impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Call::KeySet { id, .. } => write!(f, "KeySet {{ id: {id:?}, key: … }}"),
            Call::List => f.write_str("List"),
            Call::Models { id } => write!(f, "Models {{ id: {id:?} }}"),
            Call::Refresh { id } => write!(f, "Refresh {{ id: {id:?} }}"),
            Call::Test { id, model } => write!(f, "Test {{ id: {id:?}, model: {model:?} }}"),
            Call::KeyClear { id } => write!(f, "KeyClear {{ id: {id:?} }}"),
        }
    }
}

fn id_param(params: &Value) -> Result<String, ApiError> {
    params
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| super::id_ok(id))
        .map(String::from)
        .ok_or_else(|| ApiError::invalid("provider", "a provider id"))
}

impl Call {
    pub fn parse(verb: &str, params: &Value) -> Result<Call, ApiError> {
        Ok(match verb {
            "list" => Call::List,
            "models" => Call::Models { id: id_param(params)? },
            "refresh" => Call::Refresh { id: id_param(params)? },
            "test" => Call::Test {
                id: id_param(params)?,
                model: params
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(String::from),
            },
            "key.set" => {
                let id = id_param(params)?;
                let key = params
                    .get("key")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| ApiError::invalid("key", "the key, as text"))?;
                if key.chars().count() > crate::context::api::KEY_MAX || key.chars().any(char::is_control) {
                    return Err(ApiError::invalid("key", "one line of text, at most 4096 characters"));
                }
                Call::KeySet { id, key: key.to_string() }
            }
            "key.clear" => Call::KeyClear { id: id_param(params)? },
            other => return Err(ApiError::method_not_found(&format!("providers.{other}"))),
        })
    }

    /// The call. Blocking.
    pub fn answer(
        &self,
        paths: &Paths,
        config: &Config,
        keys: &ProviderKeys,
        curl: &std::path::Path,
    ) -> Result<Value, ApiError> {
        let provider = |id: &str| -> Result<Provider, ApiError> {
            config
                .provider(id)
                .map_err(|why| ApiError::invalid("provider", &format!("{why:?}")))
        };
        let refused = |why: crate::secrets::StoreError| {
            ApiError::invalid("keychain", &format!("{} did not take it: {why}", keys.place()))
        };
        match self {
            Call::List => {
                let mut providers = list(config, keys);
                // Each provider's models, so a picker needs one call.
                let cache = catalogue::Cache::load(&catalogue::cache_path(paths));
                for entry in &mut providers {
                    let id = entry["id"].as_str().unwrap_or("").to_string();
                    if let Ok(provider) = config.provider(&id) {
                        let models = models_json(&provider, &cache);
                        entry["models"] = models["models"].clone();
                        entry["source"] = models["source"].clone();
                        entry["updated_at_ms"] = models["updated_at_ms"].clone();
                    }
                }
                // This machine's own Omarchy local models: discovered, shown
                // only when there are some (`omarchy`), never an error.
                let found = omarchy::cached(paths)
                    .or_else(|| omarchy::refresh(paths, omarchy::discover(now_ms())));
                if let Some(found) = found {
                    providers.push(omarchy_json(&found));
                }
                Ok(json!({ "providers": providers }))
            }
            Call::Models { id } if id == omarchy::ID => match omarchy::cached(paths) {
                Some(found) => Ok(omarchy_models(&found)),
                None => Err(ApiError::invalid("provider", "Omarchy's local models are not on this machine")),
            },
            Call::Refresh { id } if id == omarchy::ID => {
                match omarchy::refresh(paths, omarchy::discover(now_ms())) {
                    Some(found) => {
                        let mut answer = omarchy_models(&found);
                        answer["refresh"] = json!({"ok": true, "count": found.models.len()});
                        Ok(answer)
                    }
                    None => Err(ApiError::invalid("provider", "Omarchy's local models are not on this machine")),
                }
            }
            Call::Test { id, model } if id == omarchy::ID => {
                let found = omarchy::cached(paths)
                    .ok_or_else(|| ApiError::invalid("provider", "Omarchy's local models are not on this machine"))?;
                let model = model.clone().or_else(|| found.models.first().map(|m| m.id.clone()));
                let endpoint = model
                    .as_deref()
                    .and_then(|m| found.endpoint(m))
                    .unwrap_or(super::Kind::Omarchy.base_url().unwrap_or_default())
                    .to_string();
                let key = omarchy::key();
                let check = wire::test(curl, super::Kind::Omarchy, &endpoint, key.as_deref(), model.as_deref(), NETWORK_TIMEOUT);
                Ok(json!({ "provider": id, "result": check.word() }))
            }
            Call::KeySet { id, .. } | Call::KeyClear { id } if id == omarchy::ID => Err(ApiError::invalid(
                "provider",
                "Omarchy's local models use Omarchy's own key (~/.config/omarchy/agent-local/key)",
            )),
            Call::Models { id } => {
                let provider = provider(id)?;
                Ok(models_json(&provider, &catalogue::Cache::load(&catalogue::cache_path(paths))))
            }
            Call::Refresh { id } => {
                let provider = provider(id)?;
                let key = keys.get(id).ok().flatten();
                let path = catalogue::cache_path(paths);
                let refreshed = catalogue::refresh(&path, id, provider.kind, &provider.base_url, now_ms(), || {
                    wire::models(curl, provider.kind, &provider.base_url, key.as_deref(), NETWORK_TIMEOUT)
                });
                let mut answer = models_json(&provider, &catalogue::Cache::load(&path));
                answer["refresh"] = match refreshed {
                    Refreshed::Fetched(count) => json!({"ok": true, "count": count}),
                    Refreshed::Failed(why) => json!({"ok": false, "why": why}),
                };
                Ok(answer)
            }
            Call::Test { id, model } => {
                let provider = provider(id)?;
                let key = keys.get(id).ok().flatten();
                let check = wire::test(curl, provider.kind, &provider.base_url, key.as_deref(), model.as_deref(), NETWORK_TIMEOUT);
                Ok(json!({ "provider": id, "result": check.word() }))
            }
            Call::KeySet { id, key } => {
                provider(id)?;
                let recorded = keys.set(id, key).map_err(refused)?;
                tracing::info!(provider = %id, store = %recorded.store, "providers: a key was saved");
                Ok(json!({ "provider": id, "key": recorded.hint, "key_store": recorded.store }))
            }
            Call::KeyClear { id } => {
                let removed = keys.clear(id).map_err(refused)?;
                Ok(json!({ "provider": id, "removed": removed }))
            }
        }
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(0)
}

/// Every provider as `providers.list` gives it: never a key.
#[must_use]
pub fn list(config: &Config, keys: &ProviderKeys) -> Vec<Value> {
    config
        .providers
        .iter()
        .map(|(id, entry)| match Provider::from_entry(id, entry) {
            Ok(p) => {
                let key = keys.recorded(id).unwrap_or_default();
                json!({
                    "id": id, "kind": p.kind.word(), "label": p.label, "base_url": p.base_url,
                    "custom": p.kind == super::Kind::Custom, "local": p.local,
                    "needs_key": p.kind.needs_key(), "chat": p.kind.chat(), "speech": p.kind.speech(),
                    "key": key.hint, "key_store": key.store,
                })
            }
            Err(problem) => json!({"id": id, "kind": entry.kind, "problem": problem.to_string()}),
        })
        .collect()
}

/// The discovered Omarchy provider as `providers.list` gives it.
#[must_use]
pub fn omarchy_json(found: &omarchy::Discovery) -> Value {
    json!({
        "id": omarchy::ID, "kind": "omarchy", "label": super::Kind::Omarchy.name(),
        "base_url": super::Kind::Omarchy.base_url(), "custom": false, "discovered": true, "local": true,
        "needs_key": true, "chat": true, "speech": false,
        "key": if omarchy::key().is_some() { "Omarchy's own" } else { "" }, "key_store": "omarchy",
        "models": found.models.iter().map(|m| json!({"id": m.id, "name": m.name, "running": m.running()})).collect::<Vec<_>>(),
    })
}

/// The discovered models as `providers.models` gives them.
#[must_use]
pub fn omarchy_models(found: &omarchy::Discovery) -> Value {
    json!({
        "provider": omarchy::ID,
        "models": found.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        "source": "discovered", "updated_at_ms": found.at_ms,
    })
}

/// A provider's models as `providers.models` gives them.
#[must_use]
pub fn models_json(provider: &Provider, cache: &catalogue::Cache) -> Value {
    let (models, source) = catalogue::merged(provider.kind, &provider.base_url, cache.providers.get(&provider.id));
    let (word, at) = match source {
        Source::BuiltIn => ("built-in", None),
        Source::Updated { at_ms } => ("updated", Some(at_ms)),
    };
    json!({ "provider": provider.id, "models": models, "source": word, "updated_at_ms": at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::{MemoryStore, Vault};
    use crate::integrations::testdir::TestDir;
    use crate::providers::wire::tests::openai_stub;
    use std::sync::Arc;

    const KEY: &str = "sk-qz7Hj2Lm9Pn4Rt8-0123456789-WXYZ";

    #[test]
    fn no_call_returns_a_key_and_a_check_and_a_refresh_use_it() {
        let dir = TestDir::new("providers-api");
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        let (base, _) = openai_stub("x", KEY);
        let config = Config::parse(&format!("[providers.local]\nkind = \"custom\"\nbase_url = \"{base}\"\n[providers.or]\nkind = \"openrouter\"\n")).unwrap();
        let keys = ProviderKeys::with(Vault::with(&data.join(super::super::keys::FILE), Some(Arc::new(MemoryStore::default()))));
        let curl = std::path::Path::new("curl");
        let run = |verb: &str, params: Value| Call::parse(verb, &params).unwrap().answer(&paths, &config, &keys, curl).unwrap();
        let set = run("key.set", json!({"provider": "local", "key": KEY}));
        assert_eq!(set["key"], "…WXYZ (34 chars)");
        assert_eq!(run("test", json!({"provider": "local", "model": "small"}))["result"], "ok");
        assert_eq!(run("test", json!({"provider": "local", "model": "nope"}))["result"], "wrong_model");
        assert_eq!(run("test", json!({"provider": "or"}))["result"], "no_key");
        let before = run("models", json!({"provider": "local"}));
        assert_eq!((before["source"].as_str(), before["models"].as_array().unwrap().len()), (Some("built-in"), 0));
        let refreshed = run("refresh", json!({"provider": "local"}));
        assert_eq!(refreshed["refresh"]["ok"], true);
        assert_eq!(refreshed["models"], json!(["large", "small"]));
        assert_eq!(refreshed["source"], "updated");
        let listed = run("list", json!({}));
        // The grep: no call, of any kind, carries the key or any 8 characters of it.
        let everything = format!("{set}{listed}{before}{refreshed}");
        for start in 0..KEY.len() - 8 {
            assert!(!everything.contains(&KEY[start..start + 8]), "{}", &KEY[start..start + 8]);
        }
        assert_eq!(listed["providers"][0]["key"], "…WXYZ (34 chars)");
        assert_eq!(run("key.clear", json!({"provider": "local"}))["removed"], true);
        assert!(Call::parse("key.get", &json!({"provider": "local"})).is_err(), "no call reads a key");
        assert!(!format!("{:?}", Call::parse("key.set", &json!({"provider": "local", "key": KEY})).unwrap()).contains("WXYZ"));
    }
}
