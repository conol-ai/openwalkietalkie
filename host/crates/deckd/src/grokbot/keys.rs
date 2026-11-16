//! Where each Grok Bot routine's webhook key is kept.
//!
//! The rule is the cloud voice key's (`crate::secrets::home_for`): the
//! Keychain or Windows Credential Manager only for a service on the account's own data directory
//! with a deck, a file everywhere else — every test service, the API gate,
//! a daemon on a data directory of its own. One generic password per bot,
//! service `ai.cohdl.openwalkietalkie`, account `grokbot.bots.<id>.key`; the
//! file is `grokbot-keys.toml` in the data directory, mode 0600, never
//! `config.toml`, so `config.get` has no key of a bot to publish.
//!
//! **A key never reaches a log, a reply or the bus.** What the service says
//! about one is whether it is there ([`BotKeys::exists`]). Every call here
//! may block (a Keychain call can wait on a dialog), so the module and the
//! doors call them on the blocking pool, never on the core thread.
use crate::config::Paths;
use crate::secrets::StoreError;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The file a daemon with no Keychain keeps the keys in.
pub const FILE: &str = "grokbot-keys.toml";

/// The Keychain account of bot `id`'s key.
#[must_use]
pub fn account(id: &str) -> String {
    format!("grokbot.bots.{id}.key")
}

/// Where the keys are kept, behind a trait so that every test can use
/// [`MemoryKeys`].
pub trait BotKeys: Send + Sync + std::fmt::Debug {
    /// Bot `id`'s key, or `None` when it has none. A blank one is none.
    fn get(&self, id: &str) -> Result<Option<String>, StoreError>;
    /// Keep `key`, trimmed, for bot `id`. A blank key deletes.
    fn set(&self, id: &str, key: &str) -> Result<(), StoreError>;
    /// Remove bot `id`'s key; `false` when there was none.
    fn delete(&self, id: &str) -> Result<bool, StoreError>;
    /// Whether bot `id` has a key, read without the secret where the
    /// store allows it.
    fn exists(&self, id: &str) -> Result<bool, StoreError>;
    /// How status names the store: `keychain`, `credential_manager`, `file`, `memory`.
    fn word(&self) -> &'static str;
}

/// The keys of this service: the OS credential store on a Mac or Windows
/// own install, the data directory's file elsewhere (the voice key's rule).
#[must_use]
pub fn home(paths: &Paths, no_device: bool) -> Arc<dyn BotKeys> {
    #[cfg(windows)]
    if !no_device && crate::secrets::credential_store_applies(
        paths,
        std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
    ) {
        return Arc::new(CredentialKeys);
    }
    #[cfg(target_os = "macos")]
    if !no_device && crate::secrets::keychain_applies(paths, crate::secrets::real_home().as_deref())
    {
        return Arc::new(KeychainKeys);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = no_device;
    Arc::new(FileKeys::new(&paths.data))
}

/// One Windows generic credential per bot; isolated services still use files.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct CredentialKeys;

#[cfg(windows)]
fn credential(id: &str) -> crate::secrets::CredentialStore {
    crate::secrets::CredentialStore::named(&format!("{}/{}", crate::secrets::SERVICE, account(id)))
}

#[cfg(windows)]
impl BotKeys for CredentialKeys {
    fn get(&self, id: &str) -> Result<Option<String>, StoreError> {
        use crate::secrets::KeyStore as _;
        credential(id).get()
    }
    fn set(&self, id: &str, key: &str) -> Result<(), StoreError> {
        use crate::secrets::KeyStore as _;
        credential(id).set(key)
    }
    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        use crate::secrets::KeyStore as _;
        credential(id).delete()
    }
    fn exists(&self, id: &str) -> Result<bool, StoreError> {
        use crate::secrets::KeyStore as _;
        credential(id).exists()
    }
    fn word(&self) -> &'static str { "credential_manager" }
}

/// The login Keychain: one generic password per bot.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct KeychainKeys;

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
fn refused(e: &security_framework::base::Error) -> StoreError {
    StoreError::Refused(format!("{e} ({})", e.code()))
}

#[cfg(target_os = "macos")]
impl BotKeys for KeychainKeys {
    fn get(&self, id: &str) -> Result<Option<String>, StoreError> {
        match security_framework::passwords::get_generic_password(
            crate::secrets::SERVICE,
            &account(id),
        ) {
            Ok(bytes) => cleaned(&bytes),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(refused(&e)),
        }
    }

    fn set(&self, id: &str, key: &str) -> Result<(), StoreError> {
        let key = key.trim();
        if key.is_empty() {
            return self.delete(id).map(drop);
        }
        security_framework::passwords::set_generic_password(
            crate::secrets::SERVICE,
            &account(id),
            key.as_bytes(),
        )
        .map_err(|e| refused(&e))
    }

    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        match security_framework::passwords::delete_generic_password(
            crate::secrets::SERVICE,
            &account(id),
        ) {
            Ok(()) => Ok(true),
            Err(e) if e.code() == NOT_FOUND => Ok(false),
            Err(e) => Err(refused(&e)),
        }
    }

    fn exists(&self, id: &str) -> Result<bool, StoreError> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        let account = account(id);
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(crate::secrets::SERVICE)
            .account(&account)
            .load_attributes(true)
            .limit(Limit::Max(1))
            .search();
        match found {
            Ok(items) => Ok(!items.is_empty()),
            Err(e) if e.code() == NOT_FOUND => Ok(false),
            Err(e) => Err(refused(&e)),
        }
    }

    fn word(&self) -> &'static str {
        "keychain"
    }
}

/// A stored value as a key: UTF-8, trimmed, blank meaning none.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn cleaned(bytes: &[u8]) -> Result<Option<String>, StoreError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| StoreError::Refused("the stored key is not text".into()))?;
    let key = text.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}

/// `grokbot-keys.toml`: `[keys]` with one string per bot id, written whole
/// through a temp file and a rename, mode 0600.
#[derive(Debug)]
pub struct FileKeys {
    path: PathBuf,
    /// One writer at a time within this process.
    lock: Mutex<()>,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct KeyFile {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

impl FileKeys {
    #[must_use]
    pub fn new(data: &Path) -> FileKeys {
        FileKeys {
            path: data.join(FILE),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<KeyFile, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                StoreError::Refused(format!("{} is not valid: {}", self.path.display(), e.message()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyFile::default()),
            Err(e) => Err(StoreError::Refused(format!("{}: {e}", self.path.display()))),
        }
    }

    fn write(&self, file: &KeyFile) -> Result<(), StoreError> {
        let text = toml::to_string(file).map_err(|e| StoreError::Refused(e.to_string()))?;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| StoreError::Refused("the key file has no directory".into()))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| StoreError::Refused(format!("{}: {e}", dir.display())))?;
        let tmp = dir.join(format!(".{FILE}.tmp"));
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut out = options
                .open(&tmp)
                .map_err(|e| StoreError::Refused(format!("{}: {e}", tmp.display())))?;
            out.write_all(text.as_bytes())
                .and_then(|()| out.sync_all())
                .map_err(|e| StoreError::Refused(format!("{}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::Refused(format!("{}: {e}", self.path.display())))
    }
}

impl BotKeys for FileKeys {
    fn get(&self, id: &str) -> Result<Option<String>, StoreError> {
        let file = self.read()?;
        Ok(file
            .keys
            .get(id)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty()))
    }

    fn set(&self, id: &str, key: &str) -> Result<(), StoreError> {
        let key = key.trim();
        if key.is_empty() {
            return self.delete(id).map(drop);
        }
        let _guard = self.lock.lock().map_err(|_| StoreError::Refused("poisoned".into()))?;
        let mut file = self.read()?;
        file.keys.insert(id.to_string(), key.to_string());
        self.write(&file)
    }

    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        let _guard = self.lock.lock().map_err(|_| StoreError::Refused("poisoned".into()))?;
        let mut file = self.read()?;
        let had = file.keys.remove(id).is_some();
        if had {
            self.write(&file)?;
        }
        Ok(had)
    }

    fn exists(&self, id: &str) -> Result<bool, StoreError> {
        self.get(id).map(|key| key.is_some())
    }

    fn word(&self) -> &'static str {
        "file"
    }
}

/// The keys of a test.
#[derive(Debug, Default)]
pub struct MemoryKeys(Mutex<BTreeMap<String, String>>);

impl BotKeys for MemoryKeys {
    fn get(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self.0.lock().ok().and_then(|keys| keys.get(id).cloned()))
    }

    fn set(&self, id: &str, key: &str) -> Result<(), StoreError> {
        let key = key.trim();
        if let Ok(mut keys) = self.0.lock() {
            if key.is_empty() {
                keys.remove(id);
            } else {
                keys.insert(id.to_string(), key.to_string());
            }
        }
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.0.lock().is_ok_and(|mut keys| keys.remove(id).is_some()))
    }

    fn exists(&self, id: &str) -> Result<bool, StoreError> {
        self.get(id).map(|key| key.is_some())
    }

    fn word(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn credentials_keep_each_bots_key_separate() {
        let first = format!("test-{}-grok-first", std::process::id());
        let second = format!("test-{}-grok-second", std::process::id());
        let keys = CredentialKeys;
        keys.set(&first, " key-one ").unwrap();
        keys.set(&second, "key-two").unwrap();
        let result = (keys.get(&first), keys.get(&second), keys.exists(&first));
        keys.delete(&first).unwrap();
        keys.delete(&second).unwrap();
        assert_eq!(result.0.unwrap().as_deref(), Some("key-one"));
        assert_eq!(result.1.unwrap().as_deref(), Some("key-two"));
        assert!(result.2.unwrap());
        assert!(!keys.exists(&first).unwrap());
    }

    use crate::integrations::testdir::TestDir;

    #[test]
    fn the_file_keeps_one_key_per_bot_and_only_its_owner_reads_it() {
        let dir = TestDir::new("grokbot-keys");
        let keys = FileKeys::new(dir.path());
        assert_eq!(keys.get("research").unwrap(), None);
        assert!(!keys.exists("research").unwrap());
        keys.set("research", "  key-one \n").unwrap();
        keys.set("inbox", "key-two").unwrap();
        assert_eq!(keys.get("research").unwrap().as_deref(), Some("key-one"));
        assert!(keys.exists("inbox").unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A blank key deletes; deleting twice says there was nothing.
        keys.set("research", " ").unwrap();
        assert_eq!(keys.get("research").unwrap(), None);
        assert!(keys.delete("inbox").unwrap());
        assert!(!keys.delete("inbox").unwrap());
        assert_eq!(keys.word(), "file");
    }

    #[test]
    fn a_test_daemon_never_reaches_the_keychain() {
        let dir = TestDir::new("grokbot-home");
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        assert_eq!(home(&paths, false).word(), "file");
        assert_eq!(home(&paths, true).word(), "file");
        assert_eq!(account("research"), "grokbot.bots.research.key");
    }

    #[cfg(windows)]
    #[test]
    fn only_the_normal_windows_service_uses_credentials() {
        let local = std::env::var_os("LOCALAPPDATA").expect("Windows local app data");
        let data = PathBuf::from(local).join("OpenWalkieTalkie");
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        assert_eq!(home(&paths, false).word(), "credential_manager");
        assert_eq!(home(&paths, true).word(), "file");
    }
}
