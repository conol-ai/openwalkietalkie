//! One key per provider, shared by every feature that uses it.
//!
//! Kept the way context-aware dictation's keys were
//! (`crate::context::keys::Vault`): in the OS key store where the install
//! has one — the Keychain, Credential Manager, the Secret Service — under
//! the account `providers.<id>.key`, and in `provider-keys.toml` (0600) in
//! the data directory where it has none. That file always records each
//! key's hint and store, so `config.get` can publish them without asking a
//! store. **No call returns a key**: the reads here are for the code that
//! makes a provider call, and what leaves the process is the hint.
use crate::config::Paths;
use crate::context::keys::{ContextKeys, Recorded, Role, Vault};
use crate::secrets::StoreError;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The record file in the data directory.
pub const FILE: &str = "provider-keys.toml";

/// The OS store account of provider `id`'s key.
#[must_use]
pub fn account(id: &str) -> String {
    format!("providers.{id}.key")
}

/// The keys of every provider.
#[derive(Debug)]
pub struct ProviderKeys {
    vault: Vault,
}

impl ProviderKeys {
    /// The keys of a service on `paths`.
    #[must_use]
    pub fn home(paths: &Paths) -> ProviderKeys {
        ProviderKeys {
            vault: Vault::home(paths, FILE),
        }
    }

    /// Over `vault` (a test's).
    #[must_use]
    pub fn with(vault: Vault) -> ProviderKeys {
        ProviderKeys { vault }
    }

    #[must_use]
    pub fn word(&self) -> &'static str {
        self.vault.word()
    }

    #[must_use]
    pub fn place(&self) -> String {
        self.vault.place()
    }

    pub fn get(&self, id: &str) -> Result<Option<String>, StoreError> {
        self.vault.get(id, &account(id))
    }

    pub fn set(&self, id: &str, key: &str) -> Result<Recorded, StoreError> {
        self.vault.set(id, &account(id), key)
    }

    pub fn clear(&self, id: &str) -> Result<bool, StoreError> {
        self.vault.clear(id, &account(id))
    }

    #[must_use]
    pub fn recorded(&self, id: &str) -> Option<Recorded> {
        self.vault.recorded(id)
    }
}

/// What `provider-keys.toml` in `data` records of provider `id`'s key: its
/// hint and store, from that file alone.
#[must_use]
pub fn recorded(data: &Path, id: &str) -> Option<Recorded> {
    crate::context::keys::recorded_at(&data.join(FILE), id)
}

/// The keys a running service's calls are made with, read from the stores
/// once and again only when a record file says they changed (`deckd
/// providers set-key`, the app): a keyring read per dictation would cost a
/// process (Linux) or a dialog risk (macOS) every time.
pub struct KeyCache {
    paths: Paths,
    providers: OnceLock<ProviderKeys>,
    legacy: OnceLock<ContextKeys>,
    cached: Mutex<HashMap<String, (Recorded, Option<String>)>>,
}

impl KeyCache {
    #[must_use]
    pub fn new(paths: Paths) -> KeyCache {
        KeyCache {
            paths,
            providers: OnceLock::new(),
            legacy: OnceLock::new(),
            cached: Mutex::new(HashMap::new()),
        }
    }

    /// The key a [`super::Target`]'s `key_of` names: provider `id`'s, or for
    /// `legacy:<role>` Milestone 1's role key. `None` when there is none.
    pub fn key(&self, key_of: &str) -> Option<String> {
        // Omarchy's local models have a key of their own, in Omarchy's file.
        if key_of == super::omarchy::ID {
            return super::omarchy::key();
        }
        let legacy = key_of.strip_prefix("legacy:").and_then(Role::parse);
        let now = match legacy {
            Some(role) => crate::context::keys::recorded(&self.paths.data, role),
            None => recorded(&self.paths.data, key_of),
        }?;
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((seen, key)) = cached.get(key_of) {
            if *seen == now {
                return key.clone();
            }
        }
        let read = match legacy {
            Some(role) => self
                .legacy
                .get_or_init(|| ContextKeys::home(&self.paths))
                .get(role),
            None => self
                .providers
                .get_or_init(|| ProviderKeys::home(&self.paths))
                .get(key_of),
        };
        let key = read.unwrap_or_else(|why| {
            tracing::warn!(key = key_of, "providers: the key store refused: {why}");
            None
        });
        cached.insert(key_of.to_string(), (now, key.clone()));
        key
    }
}

/// The cloud voice's key as one provider's key: what `crate::secrets`'
/// [`KeyStore`](crate::secrets::KeyStore) asks for, over the registry. With
/// it as the voice's key home, the voice's own machinery — a key found in
/// `config.toml` moved out of it at start (`VoiceKey::adopt`), Save Key
/// written on the keychain thread — keeps the key in the provider's item,
/// never in the file.
#[derive(Debug)]
pub struct VoiceProviderKey {
    keys: ProviderKeys,
    id: String,
}

impl VoiceProviderKey {
    #[must_use]
    pub fn new(keys: ProviderKeys, id: &str) -> VoiceProviderKey {
        VoiceProviderKey {
            keys,
            id: id.to_string(),
        }
    }
}

impl crate::secrets::KeyStore for VoiceProviderKey {
    fn get(&self) -> Result<Option<String>, StoreError> {
        self.keys.get(&self.id)
    }

    fn set(&self, key: &str) -> Result<(), StoreError> {
        if key.trim().is_empty() {
            return self.keys.clear(&self.id).map(drop);
        }
        self.keys.set(&self.id, key).map(drop)
    }

    fn delete(&self) -> Result<bool, StoreError> {
        self.keys.clear(&self.id)
    }

    fn exists(&self) -> Result<bool, StoreError> {
        Ok(self.keys.recorded(&self.id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::MemoryStore;
    use crate::integrations::testdir::TestDir;
    use std::sync::Arc;

    const KEY: &str = "sk-or-v1-0123456789abcdef-WXYZ";

    #[test]
    fn one_key_per_provider_in_the_store_and_only_its_hint_in_the_file() {
        let dir = TestDir::new("provider-keys");
        let store = Arc::new(MemoryStore::default());
        let keys = ProviderKeys::with(Vault::with(&dir.path().join(FILE), Some(store.clone())));
        keys.set("openrouter", KEY).unwrap();
        keys.set("openai", "sk-other-key-1234").unwrap();
        use crate::context::keys::SecretStore as _;
        assert_eq!(store.get("providers.openrouter.key").unwrap().as_deref(), Some(KEY));
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!text.contains(KEY) && text.contains("…WXYZ (30 chars)"), "{text}");
        assert_eq!(recorded(dir.path(), "openrouter").unwrap().store, "memory");
        assert!(keys.clear("openrouter").unwrap());
        assert_eq!(keys.get("openrouter").unwrap(), None);
        assert_eq!(keys.get("openai").unwrap().as_deref(), Some("sk-other-key-1234"));
    }

    #[test]
    fn the_voice_key_store_is_the_providers_item() {
        use crate::secrets::KeyStore as _;
        let dir = TestDir::new("provider-voice-key");
        let store = Arc::new(MemoryStore::default());
        let voice = VoiceProviderKey::new(
            ProviderKeys::with(Vault::with(&dir.path().join(FILE), Some(store.clone()))),
            "openai",
        );
        assert!(!voice.exists().unwrap());
        voice.set("sk-tts-0123456789-ABCD").unwrap();
        use crate::context::keys::SecretStore as _;
        assert_eq!(store.get("providers.openai.key").unwrap().as_deref(), Some("sk-tts-0123456789-ABCD"));
        assert!(voice.exists().unwrap());
        voice.set("  ").unwrap();
        assert_eq!(voice.get().unwrap(), None, "a blank key clears");
    }

    /// The security fix: the cloud voice's key, in clear in `config.toml`
    /// on Linux, is moved into its provider's item by the voice's own
    /// start-up path and is gone from the file; a store that refuses leaves
    /// the voice as it was, still working, and nothing is guessed.
    #[test]
    fn the_cloud_voice_key_moves_out_of_config_toml_into_its_providers_item() {
        use crate::secrets::{KeyHome, VoiceKey};
        const TTS_KEY: &str = "sk-proj-tts-0123456789abcdef-MNOP";
        let dir = TestDir::new("provider-voice-move");
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        let text = format!("# mine\n[voice.tts]\nengine = \"cloud\"\nprovider = \"openai\"\napi_key = \"{TTS_KEY}\"\n\n[api]\nport = 47201\n");
        std::fs::write(&paths.config, &text).unwrap();
        let store = Arc::new(MemoryStore::default());
        let home = || {
            KeyHome::Store(Arc::new(VoiceProviderKey::new(
                ProviderKeys::with(Vault::with(&data.join(FILE), Some(store.clone()))),
                "openai",
            )))
        };
        let mut voice = VoiceKey::open(home());
        let file = crate::config::Config::parse(&text).unwrap();
        let now = voice.adopt(&paths, file);
        let on_disk = std::fs::read_to_string(&paths.config).unwrap();
        assert!(!on_disk.contains(TTS_KEY) && !on_disk.contains("api_key"), "{on_disk}");
        assert!(on_disk.starts_with("# mine\n[voice.tts]\nengine = \"cloud\"\nprovider = \"openai\"\n"), "{on_disk}");
        assert!(!now.voice.tts.has_key());
        use crate::context::keys::SecretStore as _;
        assert_eq!(store.get("providers.openai.key").unwrap().as_deref(), Some(TTS_KEY));
        // The config in force still speaks with it.
        let in_force = voice.overlay(Arc::new(now));
        assert_eq!(in_force.voice.tts.api_key, TTS_KEY);
        assert_eq!(in_force.voice.tts.effective_engine(), "openai");
        // Idempotent: a second start finds nothing to move.
        let mut again = VoiceKey::open(home());
        let reread = crate::config::Config::load(&paths.config).unwrap();
        let _ = again.adopt(&paths, reread);
        assert_eq!(std::fs::read_to_string(&paths.config).unwrap(), on_disk);
        assert_eq!(again.stored(), Some(TTS_KEY));

        // A store that refuses: the key stays where it was, and works.
        let locked = TestDir::new("provider-voice-locked");
        let ldata = locked.path().to_path_buf();
        let lpaths = Paths::from_data_dir(ldata.clone(), ldata.join("logs"), ldata.join("caches"));
        std::fs::write(&lpaths.config, &text).unwrap();
        let refusing = Arc::new(MemoryStore::default());
        refusing.refuse(Some("the keyring is locked"));
        let mut voice = VoiceKey::open(KeyHome::Store(Arc::new(VoiceProviderKey::new(
            ProviderKeys::with(Vault::with(&ldata.join(FILE), Some(refusing))),
            "openai",
        ))));
        let kept = voice.adopt(&lpaths, crate::config::Config::parse(&text).unwrap());
        assert_eq!(std::fs::read_to_string(&lpaths.config).unwrap(), text, "untouched");
        assert_eq!(kept.voice.tts.api_key, TTS_KEY);
    }
}
