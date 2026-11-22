//! The two API keys of context-aware dictation: the scene model's and the
//! edit model's, each with its own endpoint and model in `[context.scene]`
//! and `[context.edit]`.
//!
//! **Where a key lives.** In the OS's own store when this service has one:
//! the login Keychain on a Mac, Credential Manager on Windows, the Secret
//! Service on Linux (libsecret, through its `secret-tool`). One item per
//! key, service `ai.cohdl.openwalkietalkie`, account `context.scene.key` or
//! `context.edit.key`. The rule for "has one" is the cloud voice key's
//! (`crate::secrets::home_for`): only the account's own install, on its
//! own data directory — never a test service, a `--data-dir` service or
//! `DECKD_DATA_DIR` pointing elsewhere. On Linux the Secret Service must
//! also answer: `secret-tool` installed and a keyring daemon on the session
//! bus. Where there is no store, the key is kept in `context-keys.toml` in
//! the data directory, mode 0600, and **never in `config.toml`**, which is
//! 0644 and is what `config.get` publishes.
//!
//! **What is shown of a key** is its hint (`crate::secrets::hint`): the
//! length and at most its last four characters. `context-keys.toml` records
//! the hint and the store of each key, whichever store holds the key, so
//! that `config.get` can publish both by reading that one small file and
//! never asks a store (a store call can wait on an unlock dialog). The
//! hint describes the last key written through `deckd`; a key removed by
//! hand from Keychain Access or Seahorse keeps its hint until the next
//! `deckd context key set` or `clear`.
//!
//! **A key never reaches a log, a reply, an error or `{:?}`.** Errors carry
//! the store's own words (which never quote the secret) or a path.
//! A store that refuses a write is an error: a key is never moved to the
//! file because the Keychain said no.
use crate::config::Paths;
use crate::secrets::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The file in the data directory: hints, and the keys where no OS store is.
pub const FILE: &str = "context-keys.toml";

/// Which of the two models a key is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// The model that names the scene from the focused app and its title.
    Scene,
    /// The model that rewrites the dictated text.
    Edit,
}

impl Role {
    pub const ALL: [Role; 2] = [Role::Scene, Role::Edit];

    /// `scene` or `edit`: the table under `[context]`, and the CLI's word.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Role::Scene => "scene",
            Role::Edit => "edit",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.name() == word)
    }

    /// The store item's account, which is also the dotted name `config.get`
    /// publishes the key's hint under: `context.scene.key`.
    #[must_use]
    pub fn account(self) -> String {
        format!("context.{}.key", self.name())
    }
}

/// An OS store that keeps one secret per account (`context.edit.key`,
/// `providers.openrouter.key`).
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    /// The key, `None` when there is none. A blank one is none.
    fn get(&self, account: &str) -> Result<Option<String>, StoreError>;
    /// Keep `key` (already trimmed and not blank) in place of any other.
    fn set(&self, account: &str, key: &str) -> Result<(), StoreError>;
    /// Remove the key; `false` when there was none.
    fn delete(&self, account: &str) -> Result<bool, StoreError>;
    /// How status names it: `keychain`, `credential_manager`,
    /// `secret_service`, `memory`.
    fn word(&self) -> &'static str;
}

/// What a person reads for a store's word, the file being `context-keys.toml`
/// in `data`.
#[must_use]
pub fn place(word: &str, data: &Path) -> String {
    place_file(word, &data.join(FILE))
}

/// What a person reads for a store's word, the record file being `file`.
#[must_use]
pub fn place_file(word: &str, file: &Path) -> String {
    match word {
        "keychain" => "the Keychain".into(),
        "credential_manager" => "Credential Manager".into(),
        "secret_service" => "the Secret Service keyring (libsecret)".into(),
        "memory" => "memory".into(),
        _ => format!("{} (mode 600)", file.display()),
    }
}

/// A stored value as a key: UTF-8, trimmed, blank meaning none.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn cleaned(bytes: &[u8]) -> Result<Option<String>, StoreError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| StoreError::Refused("the stored key is not text".into()))?;
    let key = text.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}

// ---------------------------------------------------------------------------
// The OS stores
// ---------------------------------------------------------------------------

/// The login Keychain: one generic password per role.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct Keychain;

#[cfg(target_os = "macos")]
mod keychain {
    /// `errSecItemNotFound`.
    pub const NOT_FOUND: i32 = -25300;

    pub fn refused(e: &security_framework::base::Error) -> crate::secrets::StoreError {
        crate::secrets::StoreError::Refused(format!("{e} ({})", e.code()))
    }
}

#[cfg(target_os = "macos")]
impl SecretStore for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(crate::secrets::SERVICE, account) {
            Ok(bytes) => cleaned(&bytes),
            Err(e) if e.code() == keychain::NOT_FOUND => Ok(None),
            Err(e) => Err(keychain::refused(&e)),
        }
    }

    fn set(&self, account: &str, key: &str) -> Result<(), StoreError> {
        security_framework::passwords::set_generic_password(
            crate::secrets::SERVICE,
            account,
            key.as_bytes(),
        )
        .map_err(|e| keychain::refused(&e))
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        use security_framework::passwords::delete_generic_password;
        match delete_generic_password(crate::secrets::SERVICE, account) {
            Ok(()) => Ok(true),
            Err(e) if e.code() == keychain::NOT_FOUND => Ok(false),
            Err(e) => Err(keychain::refused(&e)),
        }
    }

    fn word(&self) -> &'static str {
        "keychain"
    }
}

/// Windows Credential Manager: one generic credential per role, target
/// `ai.cohdl.openwalkietalkie/context.<role>.key`, through the cloud voice
/// key's own [`crate::secrets::CredentialStore`].
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct CredentialManager;

#[cfg(windows)]
fn credential(account: &str) -> crate::secrets::CredentialStore {
    crate::secrets::CredentialStore::named(&format!(
        "{}/{}",
        crate::secrets::SERVICE,
        account
    ))
}

#[cfg(windows)]
impl SecretStore for CredentialManager {
    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        use crate::secrets::KeyStore as _;
        credential(account).get()
    }

    fn set(&self, account: &str, key: &str) -> Result<(), StoreError> {
        use crate::secrets::KeyStore as _;
        credential(account).set(key)
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        use crate::secrets::KeyStore as _;
        credential(account).delete()
    }

    fn word(&self) -> &'static str {
        "credential_manager"
    }
}

/// The Secret Service (GNOME Keyring, KWallet, KeePassXC…) through
/// libsecret's own command line, `secret-tool`: no new crate in the
/// signed bundle's tree, and libsecret answers the unlock prompts itself.
/// The key goes to `secret-tool store` on stdin, never on its command line,
/// and comes back on its stdout. Every call is bounded by `timeout`, after
/// which the child is killed and the call refused.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct SecretTool {
    program: PathBuf,
    timeout: std::time::Duration,
    /// The items' `service` attribute: [`crate::secrets::SERVICE`], or a
    /// test's own.
    service: String,
}

#[cfg(target_os = "linux")]
impl SecretTool {
    /// How long one call may take. A keyring that asks to be unlocked waits
    /// for the owner, which is the one slow answer worth waiting for.
    pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    /// The account the availability probe looks up. Nothing is stored
    /// under it.
    const PROBE: &'static str = "context.probe";

    /// `secret-tool` on `PATH`, if it is there.
    #[must_use]
    pub fn find() -> Option<SecretTool> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("secret-tool"))
            .find(|candidate| candidate.is_file())
            .map(|program| SecretTool {
                program,
                timeout: SecretTool::TIMEOUT,
                service: crate::secrets::SERVICE.to_string(),
            })
    }

    /// The same tool, its items under `service` instead (a test's own).
    #[must_use]
    pub fn under(self, service: &str) -> SecretTool {
        SecretTool {
            service: service.to_string(),
            ..self
        }
    }

    /// Whether a Secret Service answers on the session bus: a lookup of an
    /// account nothing is stored under says "not found" (exit 1, nothing on
    /// stderr) rather than why it could not ask.
    #[must_use]
    pub fn answers(&self) -> bool {
        self.lookup(SecretTool::PROBE).is_ok()
    }

    /// One run: `args`, `input` on stdin, bounded by the timeout. The exit
    /// code, stdout and stderr.
    fn run(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<(Option<i32>, Vec<u8>, String), StoreError> {
        use std::io::{Read as _, Write as _};
        use std::process::{Command, Stdio};
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| StoreError::Refused(format!("secret-tool: {e}")))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input)
                .map_err(|e| StoreError::Refused(format!("secret-tool: {e}")))?;
        }
        let deadline = std::time::Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(StoreError::Refused(format!(
                        "secret-tool did not answer within {} s",
                        self.timeout.as_secs()
                    )));
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(e) => return Err(StoreError::Refused(format!("secret-tool: {e}"))),
            }
        };
        let mut out = Vec::new();
        let mut err = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut out);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        Ok((status.code(), out, err.trim().to_string()))
    }

    fn lookup(&self, account: &str) -> Result<Option<String>, StoreError> {
        let (code, out, err) = self.run(
            &["lookup", "service", &self.service, "account", account],
            None,
        )?;
        match code {
            Some(0) => cleaned(&out),
            // Not found: libsecret says nothing.
            Some(1) if err.is_empty() => Ok(None),
            _ => Err(SecretTool::refused(&err)),
        }
    }

    fn refused(err: &str) -> StoreError {
        let first = err.lines().next().unwrap_or("").trim();
        StoreError::Refused(if first.is_empty() {
            "secret-tool failed".into()
        } else {
            format!("secret-tool: {first}")
        })
    }
}

#[cfg(target_os = "linux")]
impl SecretStore for SecretTool {
    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        self.lookup(account)
    }

    fn set(&self, account: &str, key: &str) -> Result<(), StoreError> {
        let label = format!("OpenWalkieTalkie {account}");
        let (code, _, err) = self.run(
            &[
                "store",
                "--label",
                &label,
                "service",
                &self.service,
                "account",
                account,
            ],
            Some(key.as_bytes()),
        )?;
        match code {
            Some(0) => Ok(()),
            _ => Err(SecretTool::refused(&err)),
        }
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        let had = self.lookup(account)?.is_some();
        if !had {
            return Ok(false);
        }
        let (code, _, err) = self.run(
            &["clear", "service", &self.service, "account", account],
            None,
        )?;
        match code {
            Some(0) => Ok(true),
            _ => Err(SecretTool::refused(&err)),
        }
    }

    fn word(&self) -> &'static str {
        "secret_service"
    }
}

/// A store in memory, for the tests. It can be made to refuse.
#[derive(Debug, Default)]
pub struct MemoryStore {
    keys: Mutex<BTreeMap<String, String>>,
    refusal: Mutex<Option<String>>,
}

impl MemoryStore {
    /// Refuse every call from now on with `why`, or answer again with `None`.
    pub fn refuse(&self, why: Option<&str>) {
        *self
            .refusal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = why.map(String::from);
    }

    fn answer(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, String>>, StoreError> {
        if let Some(why) = &*self
            .refusal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(StoreError::Refused(why.clone()));
        }
        Ok(self
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        Ok(self.answer()?.get(account).cloned())
    }

    fn set(&self, account: &str, key: &str) -> Result<(), StoreError> {
        self.answer()?.insert(account.to_string(), key.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        Ok(self.answer()?.remove(account).is_some())
    }

    fn word(&self) -> &'static str {
        "memory"
    }
}

// ---------------------------------------------------------------------------
// The record file: `context-keys.toml`, `provider-keys.toml`
// ---------------------------------------------------------------------------

/// One key as the record file has it.
#[derive(Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
struct Entry {
    /// The store's word ([`SecretStore::word`], or `file`).
    #[serde(default)]
    store: String,
    /// The key's hint: its length and last four characters at most.
    #[serde(default)]
    hint: String,
    /// The key itself, only when no OS store keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

/// The key stays out of every `{:?}`.
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("store", &self.store)
            .field("hint", &self.hint)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The whole file: one table per name (`[scene]`, `[openrouter]`).
#[derive(Debug, Default, Deserialize, Serialize)]
struct Entries {
    #[serde(flatten)]
    roles: BTreeMap<String, Entry>,
}

/// What `config.get` publishes about a key: its hint and where it is kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    /// [`SecretStore::word`], or `file`.
    pub store: String,
    /// `…abcd (51 chars)`, or `N chars` for a key of eight or fewer.
    pub hint: String,
}

/// A record file: written whole through a temp file and a rename, mode 0600
/// from the moment it exists.
#[derive(Debug)]
struct KeyFile {
    path: PathBuf,
    /// One writer at a time within this process.
    lock: Mutex<()>,
}

const HEADER: &str = "# Written by deckd. The hints and the store of its API keys, and the\n\
# keys themselves only where no OS key store is available.\n\
# Mode 0600; never edit by hand.\n";

impl KeyFile {
    fn new(path: PathBuf) -> KeyFile {
        KeyFile {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Entries, StoreError> {
        read_entries(&self.path)
    }

    fn write(&self, entries: &Entries) -> Result<(), StoreError> {
        let body = toml::to_string(entries).map_err(|e| StoreError::Refused(e.to_string()))?;
        let text = format!("{HEADER}\n{body}");
        let dir = self
            .path
            .parent()
            .ok_or_else(|| StoreError::Refused("the key file has no directory".into()))?;
        let failed =
            |at: &Path, e: std::io::Error| StoreError::Refused(format!("{}: {e}", at.display()));
        std::fs::create_dir_all(dir).map_err(|e| failed(dir, e))?;
        let name = self.path.file_name().map_or_else(|| "keys".into(), |n| n.to_string_lossy());
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut out = options.open(&tmp).map_err(|e| failed(&tmp, e))?;
            // A temp file left by a crash may have been created looser.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                out.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| failed(&tmp, e))?;
            }
            out.write_all(text.as_bytes())
                .and_then(|()| out.sync_all())
                .map_err(|e| failed(&tmp, e))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| failed(&self.path, e))
    }

    /// Change the file under the lock: `edit` gets the entries and says
    /// whether anything changed, which is then written.
    fn update(&self, edit: impl FnOnce(&mut Entries) -> bool) -> Result<(), StoreError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut entries = self.read()?;
        if edit(&mut entries) {
            self.write(&entries)?;
        }
        Ok(())
    }
}

fn read_entries(path: &Path) -> Result<Entries, StoreError> {
    match std::fs::read_to_string(path) {
        // `toml`'s message alone, never its rendering, which quotes the
        // offending line — and that line may hold a key.
        Ok(text) => toml::from_str(&text).map_err(|e| {
            StoreError::Refused(format!("{} is not valid: {}", path.display(), e.message()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Entries::default()),
        Err(e) => Err(StoreError::Refused(format!("{}: {e}", path.display()))),
    }
}

/// What the record file at `path` has of `name`'s key: its hint and its
/// store, read from that file alone and never from a store. `None` when no
/// key is recorded, and when the file cannot be read (logged by path,
/// without its contents).
#[must_use]
pub fn recorded_at(path: &Path, name: &str) -> Option<Recorded> {
    let entries = match read_entries(path) {
        Ok(entries) => entries,
        Err(why) => {
            tracing::warn!("keys: {why}");
            return None;
        }
    };
    entries
        .roles
        .get(name)
        .filter(|entry| !entry.hint.is_empty())
        .map(|entry| Recorded {
            store: entry.store.clone(),
            hint: entry.hint.clone(),
        })
}

/// What `context-keys.toml` in `data` records of `role`'s key.
#[must_use]
pub fn recorded(data: &Path, role: Role) -> Option<Recorded> {
    recorded_at(&data.join(FILE), role.name())
}

// ---------------------------------------------------------------------------
// The one interface
// ---------------------------------------------------------------------------

/// Keys by name, each with its OS store account: an OS store when the
/// install has one, else the record file. Every call may block (a Keychain
/// dialog, a keyring unlock), so a running service makes them off the core
/// thread.
#[derive(Debug)]
pub struct Vault {
    store: Option<Arc<dyn SecretStore>>,
    file: KeyFile,
}

impl Vault {
    /// The vault of a service on `paths`, its records in `file` in the data
    /// directory: the OS store for the account's own install when it
    /// answers, the file everywhere else.
    #[must_use]
    pub fn home(paths: &Paths, file: &str) -> Vault {
        Vault::with(&paths.data.join(file), os_store(paths))
    }

    /// `store` (none: the file alone), with its records at `path`.
    #[must_use]
    pub fn with(path: &Path, store: Option<Arc<dyn SecretStore>>) -> Vault {
        Vault {
            store,
            file: KeyFile::new(path.to_path_buf()),
        }
    }

    /// Where new keys go: the store's word, or `file`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        self.store.as_ref().map_or("file", |store| store.word())
    }

    /// Where new keys go, for a person.
    #[must_use]
    pub fn place(&self) -> String {
        place_file(self.word(), &self.file.path)
    }

    /// The record file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file.path
    }

    /// `name`'s key: the store's (`account`), else one the file holds
    /// (written while no store answered). `None` when there is neither.
    pub fn get(&self, name: &str, account: &str) -> Result<Option<String>, StoreError> {
        if let Some(store) = &self.store {
            if let Some(key) = store.get(account)? {
                return Ok(Some(key));
            }
        }
        Ok(self
            .file
            .read()?
            .roles
            .get(name)
            .and_then(|entry| entry.key.as_deref())
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(String::from))
    }

    /// Keep `key` for `name`, trimmed, where new keys go, and record its
    /// hint. A blank key is refused (clearing is [`Vault::clear`]). With a
    /// store, a copy the file still held from before is removed. The answer
    /// is what was recorded.
    pub fn set(&self, name: &str, account: &str, key: &str) -> Result<Recorded, StoreError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(StoreError::Refused("no key given".into()));
        }
        if let Some(store) = &self.store {
            store.set(account, key)?;
        }
        let entry = Entry {
            store: self.word().to_string(),
            hint: crate::secrets::hint(key),
            key: self.store.is_none().then(|| key.to_string()),
        };
        let recorded = Recorded {
            store: entry.store.clone(),
            hint: entry.hint.clone(),
        };
        self.file.update(|entries| {
            entries.roles.insert(name.to_string(), entry);
            true
        })?;
        Ok(recorded)
    }

    /// Remove `name`'s key from the store and the file. `false` when there
    /// was none in either.
    pub fn clear(&self, name: &str, account: &str) -> Result<bool, StoreError> {
        let in_store = match &self.store {
            Some(store) => store.delete(account)?,
            None => false,
        };
        let mut in_file = false;
        self.file.update(|entries| {
            let removed = entries.roles.remove(name);
            in_file = removed.as_ref().is_some_and(|entry| entry.key.is_some());
            removed.is_some()
        })?;
        Ok(in_store || in_file)
    }

    /// What the file records of `name`'s key ([`recorded_at`]).
    #[must_use]
    pub fn recorded(&self, name: &str) -> Option<Recorded> {
        recorded_at(&self.file.path, name)
    }
}

/// The two context-aware dictation keys of Milestone 1, one per [`Role`],
/// in `context-keys.toml`. Kept for the migration into the provider registry
/// (`crate::providers::migrate`) and for a role still on its own endpoint.
#[derive(Debug)]
pub struct ContextKeys {
    vault: Vault,
    data: PathBuf,
}

impl ContextKeys {
    #[must_use]
    pub fn home(paths: &Paths) -> ContextKeys {
        ContextKeys::with(&paths.data, os_store(paths))
    }

    #[must_use]
    pub fn file_only(data: &Path) -> ContextKeys {
        ContextKeys::with(data, None)
    }

    #[must_use]
    pub fn with(data: &Path, store: Option<Arc<dyn SecretStore>>) -> ContextKeys {
        ContextKeys {
            vault: Vault::with(&data.join(FILE), store),
            data: data.to_path_buf(),
        }
    }

    #[must_use]
    pub fn word(&self) -> &'static str {
        self.vault.word()
    }

    #[must_use]
    pub fn place(&self) -> String {
        self.vault.place()
    }

    pub fn get(&self, role: Role) -> Result<Option<String>, StoreError> {
        self.vault.get(role.name(), &role.account())
    }

    pub fn set(&self, role: Role, key: &str) -> Result<Recorded, StoreError> {
        self.vault.set(role.name(), &role.account(), key)
    }

    pub fn clear(&self, role: Role) -> Result<bool, StoreError> {
        self.vault.clear(role.name(), &role.account())
    }

    #[must_use]
    pub fn recorded(&self, role: Role) -> Option<Recorded> {
        recorded(&self.data, role)
    }
}

/// The OS store for a service on `paths`, if it has one (the module's rule).
#[allow(clippy::unnecessary_wraps)]
fn os_store(paths: &Paths) -> Option<Arc<dyn SecretStore>> {
    #[cfg(target_os = "macos")]
    if crate::secrets::keychain_applies(paths, crate::secrets::real_home().as_deref()) {
        return Some(Arc::new(Keychain));
    }
    #[cfg(windows)]
    if crate::secrets::credential_store_applies(
        paths,
        std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
    ) {
        return Some(Arc::new(CredentialManager));
    }
    #[cfg(target_os = "linux")]
    if linux_own_install(paths) {
        if let Some(tool) = SecretTool::find().filter(SecretTool::answers) {
            return Some(Arc::new(tool));
        }
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    let _ = paths;
    None
}

/// Linux's [`crate::secrets::keychain_applies`]: the service on the
/// platform's own data directory (`$XDG_DATA_HOME/openwalkietalkie`), the one
/// the Omarchy units run it on.
#[cfg(target_os = "linux")]
pub(crate) fn linux_own_install(paths: &Paths) -> bool {
    Paths::platform_default().is_ok_and(|own| own.data == paths.data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;

    const SCENE_KEY: &str = "sk-scene-0123456789-abcdefgh-WXYZ";
    const EDIT_KEY: &str = "sk-edit-9876543210-zyxwvuts-QRST";

    fn file_text(dir: &TestDir) -> String {
        std::fs::read_to_string(dir.path().join(FILE)).unwrap_or_default()
    }

    #[test]
    fn a_role_has_a_name_and_an_account() {
        assert_eq!(Role::parse("scene"), Some(Role::Scene));
        assert_eq!(Role::parse("edit"), Some(Role::Edit));
        assert_eq!(Role::parse("Scene"), None);
        assert_eq!(Role::parse("tts"), None);
        assert_eq!(Role::Scene.account(), "context.scene.key");
        assert_eq!(Role::Edit.account(), "context.edit.key");
    }

    /// The fallback: no OS store, the file keeps both keys, mode 0600,
    /// each apart from the other, and a clear takes one away.
    #[test]
    fn the_file_keeps_the_keys_where_there_is_no_store() {
        let dir = TestDir::new("context-keys-file");
        let keys = ContextKeys::file_only(dir.path());
        assert_eq!(keys.word(), "file");
        assert_eq!(keys.get(Role::Scene).unwrap(), None);
        assert_eq!(keys.recorded(Role::Scene), None);
        assert!(!dir.path().join(FILE).exists(), "nothing written by a read");

        let recorded = keys.set(Role::Scene, &format!("  {SCENE_KEY}\n")).unwrap();
        assert_eq!(
            recorded,
            Recorded {
                store: "file".into(),
                hint: "…WXYZ (33 chars)".into()
            }
        );
        keys.set(Role::Edit, EDIT_KEY).unwrap();
        assert_eq!(keys.get(Role::Scene).unwrap().as_deref(), Some(SCENE_KEY));
        assert_eq!(keys.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY));
        assert_eq!(keys.recorded(Role::Edit).unwrap().hint, "…QRST (32 chars)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Another key replaces the first.
        keys.set(Role::Scene, "sk-second-key-0000-1111").unwrap();
        assert_eq!(
            keys.get(Role::Scene).unwrap().as_deref(),
            Some("sk-second-key-0000-1111")
        );
        assert!(!file_text(&dir).contains(SCENE_KEY));

        // A clear removes one key and leaves the other.
        assert!(keys.clear(Role::Scene).unwrap());
        assert_eq!(keys.get(Role::Scene).unwrap(), None);
        assert_eq!(keys.recorded(Role::Scene), None);
        assert_eq!(keys.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY));
        assert!(!keys.clear(Role::Scene).unwrap(), "nothing left to clear");

        // A blank key is refused, not taken as a clear.
        let refused = keys.set(Role::Edit, "  \n").unwrap_err();
        assert_eq!(refused.to_string(), "no key given");
        assert_eq!(keys.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY));
    }

    /// A short key's hint gives its length and nothing of the key.
    #[test]
    fn a_short_keys_hint_shows_none_of_it() {
        let dir = TestDir::new("context-keys-short");
        let keys = ContextKeys::file_only(dir.path());
        assert_eq!(keys.set(Role::Edit, "abc12345").unwrap().hint, "8 chars");
    }

    /// With a store, the key is in the store and the file holds its hint
    /// and the store's name only; a key the file held from before the store
    /// answered moves out of it at the next write.
    #[test]
    fn with_a_store_the_file_never_holds_the_key() {
        let dir = TestDir::new("context-keys-store");
        ContextKeys::file_only(dir.path())
            .set(Role::Edit, EDIT_KEY)
            .unwrap();
        let store = Arc::new(MemoryStore::default());
        let keys = ContextKeys::with(dir.path(), Some(store.clone()));
        assert_eq!(keys.word(), "memory");
        // The file's key is still read until it is replaced.
        assert_eq!(keys.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY));

        keys.set(Role::Scene, SCENE_KEY).unwrap();
        keys.set(Role::Edit, EDIT_KEY).unwrap();
        assert_eq!(store.get(&Role::Scene.account()).unwrap().as_deref(), Some(SCENE_KEY));
        assert_eq!(store.get(&Role::Edit.account()).unwrap().as_deref(), Some(EDIT_KEY));
        let text = file_text(&dir);
        assert!(
            !text.contains(SCENE_KEY) && !text.contains(EDIT_KEY),
            "{text}"
        );
        assert!(text.contains("…WXYZ (33 chars)"), "{text}");
        assert_eq!(keys.recorded(Role::Scene).unwrap().store, "memory");

        assert!(keys.clear(Role::Scene).unwrap());
        assert_eq!(store.get(&Role::Scene.account()).unwrap(), None);
        assert_eq!(keys.recorded(Role::Scene), None);
    }

    /// A store that refuses is an error, and the key is not put in the file
    /// instead; the error says nothing of the key.
    #[test]
    fn a_store_that_refuses_is_an_error_and_never_a_move_to_the_file() {
        let dir = TestDir::new("context-keys-refused");
        let store = Arc::new(MemoryStore::default());
        store.refuse(Some("User canceled the operation. (-128)"));
        let keys = ContextKeys::with(dir.path(), Some(store.clone()));
        let refused = keys.set(Role::Scene, SCENE_KEY).unwrap_err().to_string();
        assert!(
            !refused.contains(SCENE_KEY) && !refused.contains("WXYZ"),
            "{refused}"
        );
        assert!(!dir.path().join(FILE).exists());
        assert!(keys.get(Role::Scene).is_err());
        assert!(keys.clear(Role::Scene).is_err());
    }

    /// A key never shows in `{:?}`, in the file's error, or in what
    /// `recorded` gives.
    #[test]
    fn a_key_never_shows_in_debug_or_errors() {
        let dir = TestDir::new("context-keys-debug");
        let keys = ContextKeys::file_only(dir.path());
        keys.set(Role::Scene, SCENE_KEY).unwrap();
        let entries = keys.vault.file.read().unwrap();
        let shown = format!("{keys:?} {entries:?} {:?}", keys.recorded(Role::Scene));
        assert!(!shown.contains(SCENE_KEY), "{shown}");
        // A broken file is reported by its path and toml's message, never by
        // the line that holds the key.
        std::fs::write(
            dir.path().join(FILE),
            format!("[scene]\nkey = \"{SCENE_KEY}\" oops\n"),
        )
        .unwrap();
        let why = keys.get(Role::Scene).unwrap_err().to_string();
        assert!(!why.contains(SCENE_KEY), "{why}");
        assert_eq!(keys.recorded(Role::Scene), None);
    }

    /// A test daemon, a `--data-dir` daemon: never an OS store.
    #[test]
    fn a_service_on_a_data_dir_of_its_own_never_reaches_an_os_store() {
        let dir = TestDir::new("context-keys-home");
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        assert_eq!(ContextKeys::home(&paths).word(), "file");
        assert!(place("file", &data).ends_with("context-keys.toml (mode 600)"));
    }

    /// The real Secret Service, by hand only: it writes to the session's
    /// keyring, under a service of its own (never the real items'), and
    /// removes what it wrote.
    /// `cargo test -p deckd context::keys -- --ignored`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "writes to the session's real keyring"]
    fn the_secret_service_round_trip() {
        let tool = SecretTool::find()
            .expect("secret-tool on PATH")
            .under(&format!(
                "{}.test-{}",
                crate::secrets::SERVICE,
                std::process::id()
            ));
        assert!(tool.answers(), "a Secret Service on the session bus");
        let dir = TestDir::new("context-keys-secret-service");
        let keys = ContextKeys::with(dir.path(), Some(Arc::new(tool.clone())));
        assert_eq!(keys.word(), "secret_service");
        let _ = keys.clear(Role::Scene);
        keys.set(Role::Scene, SCENE_KEY).unwrap();
        assert_eq!(tool.get(&Role::Scene.account()).unwrap().as_deref(), Some(SCENE_KEY));
        assert!(!file_text(&dir).contains(SCENE_KEY));
        assert!(keys.clear(Role::Scene).unwrap());
        assert_eq!(tool.get(&Role::Scene.account()).unwrap(), None);
        assert!(!keys.clear(Role::Scene).unwrap());
    }
}
