//! `context.key.set {role, key}` and `context.key.clear {role}`: the two
//! models' keys, from the companion app.
//!
//! `config.set` refuses the keys by design (a key is no setting of
//! `config.toml`), and the CLI writes them straight into the key store
//! (`deckd context key set`). The app cannot do that itself: on a Mac a
//! Keychain item trusts the program that wrote it, and the service is the
//! one that reads it. So the app hands the key to the service, which writes
//! it through the same [`ContextKeys`] the CLI uses — the OS key store where
//! there is one, `context-keys.toml` (0600) where there is not.
//!
//! Answered on the blocking pool, never on the core thread: a Keychain
//! write can wait on a dialog, a Secret Service one on an unlock. The reply
//! carries the key's hint (its length and at most its last four characters)
//! and where it is kept, never the key; so do the errors.
use super::keys::{ContextKeys, Role};
use deck_model::ApiError;
use serde_json::{json, Value};

/// The longest key taken: no provider's key comes near it, and anything
/// longer is a paste of something else.
pub const KEY_MAX: usize = 4096;

/// `context.<verb>` from a connection.
pub async fn api_dispatch(
    core: &crate::core::handle::CoreHandle,
    verb: &str,
    params: Value,
) -> Result<Value, ApiError> {
    let call = Call::parse(verb, &params)?;
    let paths = core.care_context().await?.paths;
    match tokio::task::spawn_blocking(move || {
        // Since the provider registry, a role's key is its provider's: the
        // call writes that one, and only a role still on Milestone 1's own
        // endpoint keeps a key of its own.
        let config = crate::config::Config::load(&paths.config).unwrap_or_default();
        let provider = match call.role() {
            Role::Scene => config.context.scene.provider.trim().to_string(),
            Role::Edit => config.context.edit.provider.trim().to_string(),
        };
        if provider.is_empty() {
            call.answer(&ContextKeys::home(&paths))
        } else {
            call.answer_for(&crate::providers::keys::ProviderKeys::home(&paths), &provider)
        }
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(ApiError::internal(&format!("the key task failed: {e}"))),
    }
}

/// One checked call.
pub enum Call {
    Set { role: Role, key: String },
    Clear { role: Role },
}

/// The key never shows in `{:?}`.
impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Call::Set { role, .. } => write!(f, "Set {{ role: {role:?}, key: … }}"),
            Call::Clear { role } => write!(f, "Clear {{ role: {role:?} }}"),
        }
    }
}

impl Call {
    /// `verb` and its params, checked before any store is touched.
    pub fn parse(verb: &str, params: &Value) -> Result<Call, ApiError> {
        let role = params
            .get("role")
            .and_then(Value::as_str)
            .and_then(Role::parse)
            .ok_or_else(|| ApiError::invalid("role", "scene or edit"))?;
        match verb {
            "key.set" => {
                let key = params
                    .get("key")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| ApiError::invalid("key", "the key, as text"))?;
                if key.chars().count() > KEY_MAX || key.chars().any(char::is_control) {
                    return Err(ApiError::invalid(
                        "key",
                        "one line of text, at most 4096 characters",
                    ));
                }
                Ok(Call::Set {
                    role,
                    key: key.to_string(),
                })
            }
            "key.clear" => Ok(Call::Clear { role }),
            other => Err(ApiError::method_not_found(&format!("context.{other}"))),
        }
    }

    /// The role it is about.
    #[must_use]
    pub fn role(&self) -> Role {
        match self {
            Call::Set { role, .. } | Call::Clear { role } => *role,
        }
    }

    /// The call, against provider `id`'s key. Blocking.
    pub fn answer_for(
        &self,
        keys: &crate::providers::keys::ProviderKeys,
        id: &str,
    ) -> Result<Value, ApiError> {
        let refused = |why: crate::secrets::StoreError| {
            ApiError::invalid("keychain", &format!("{} did not take it: {why}", keys.place()))
        };
        match self {
            Call::Set { role, key } => {
                let recorded = keys.set(id, key).map_err(refused)?;
                Ok(json!({"role": role.name(), "provider": id, "key": recorded.hint, "key_store": recorded.store}))
            }
            Call::Clear { role } => {
                let removed = keys.clear(id).map_err(refused)?;
                Ok(json!({"role": role.name(), "provider": id, "removed": removed}))
            }
        }
    }

    /// The call, against `keys`. Blocking.
    pub fn answer(&self, keys: &ContextKeys) -> Result<Value, ApiError> {
        let refused = |why: crate::secrets::StoreError| {
            ApiError::invalid(
                "keychain",
                &format!("{} did not take it: {why}", keys.place()),
            )
        };
        match self {
            Call::Set { role, key } => {
                let recorded = keys.set(*role, key).map_err(refused)?;
                tracing::info!(
                    role = role.name(),
                    store = %recorded.store,
                    "context: a key was saved from the app"
                );
                Ok(json!({
                    "role": role.name(),
                    "key": recorded.hint,
                    "key_store": recorded.store,
                }))
            }
            Call::Clear { role } => {
                let removed = keys.clear(*role).map_err(refused)?;
                tracing::info!(role = role.name(), removed, "context: a key was removed from the app");
                Ok(json!({ "role": role.name(), "removed": removed }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::MemoryStore;
    use crate::integrations::testdir::TestDir;
    use std::sync::Arc;

    const KEY: &str = "sk-app-0123456789-abcdefgh-WXYZ";

    #[test]
    fn a_key_from_the_app_is_kept_and_only_its_hint_comes_back() {
        let dir = TestDir::new("context-api");
        let store = Arc::new(MemoryStore::default());
        let keys = ContextKeys::with(dir.path(), Some(store.clone()));
        let set = Call::parse("key.set", &json!({"role": "edit", "key": format!(" {KEY} ")})).unwrap();
        assert!(!format!("{set:?}").contains("WXYZ"));
        let reply = set.answer(&keys).unwrap();
        assert_eq!(reply, json!({"role": "edit", "key": "…WXYZ (31 chars)", "key_store": "memory"}));
        assert!(!reply.to_string().contains(KEY));
        use crate::context::keys::SecretStore as _;
        assert_eq!(store.get(&Role::Edit.account()).unwrap().as_deref(), Some(KEY));
        let clear = Call::parse("key.clear", &json!({"role": "edit"})).unwrap();
        assert_eq!(clear.answer(&keys).unwrap()["removed"], true);
        assert_eq!(store.get(&Role::Edit.account()).unwrap(), None);
    }

    #[test]
    fn a_bad_call_is_refused_before_any_store_is_touched() {
        for (verb, params, field) in [
            ("key.set", json!({"role": "tts", "key": KEY}), "role"),
            ("key.set", json!({"key": KEY}), "role"),
            ("key.set", json!({"role": "scene"}), "key"),
            ("key.set", json!({"role": "scene", "key": "   "}), "key"),
            ("key.set", json!({"role": "scene", "key": "a\nb"}), "key"),
            ("key.set", json!({"role": "scene", "key": "k".repeat(KEY_MAX + 1)}), "key"),
        ] {
            let refused = Call::parse(verb, &params).unwrap_err();
            assert_eq!(refused.data["field"], field, "{params}");
            assert!(!refused.to_string().contains(KEY));
        }
        assert!(Call::parse("key.get", &json!({"role": "edit"})).is_err(), "no call reads a key");
    }

    #[test]
    fn a_store_that_refuses_says_so_without_the_key() {
        let dir = TestDir::new("context-api-refused");
        let store = Arc::new(MemoryStore::default());
        store.refuse(Some("User canceled the operation. (-128)"));
        let keys = ContextKeys::with(dir.path(), Some(store));
        let refused = Call::parse("key.set", &json!({"role": "scene", "key": KEY}))
            .unwrap()
            .answer(&keys)
            .unwrap_err();
        assert!(!format!("{refused:?}").contains(KEY));
        assert_eq!(refused.data["field"], "keychain");
    }
}
