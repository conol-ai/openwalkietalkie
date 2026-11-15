//! The cloud voice's key, kept in the Keychain on a Mac: one generic
//! password, service `ai.cohdl.openwalkietalkie`, account
//! `voice.tts.api_key`, for the provider in force (one key, as before).
//!
//! **Where the key lives** is decided once per process ([`home_for`]): the
//! Keychain ([`KeyHome::Store`]) only for a macOS service with a deck on the
//! account's own data directory, and `config.toml`'s `[voice.tts] api_key`
//! ([`KeyHome::File`]) everywhere else. That covers Linux, which has no cloud
//! voice, and every daemon on a data directory of its own (`--data-dir`,
//! `DECKD_DATA_DIR`, a redirected `HOME`) or started `--no-device`: every test
//! service, the API gate and the screenshot harness's demo daemon. A
//! redirected `HOME` does not redirect the login Keychain, so the rule
//! compares the data directory with the account's real home, never with
//! `HOME`.
//!
//! **A key can still come in through the file.** A key found in
//! `config.toml` (an install from before the Keychain, an operator's paste,
//! `deckd tts key set` with no service to ask) moves into the store and out
//! of the file. Every other byte of the file stays as it was. Until it has
//! moved, the key in the file is the one in force ([`VoiceKey::overlay`]).
//!
//! **No Keychain call runs on the core thread.** A call can wait on a
//! dialog, and a core waiting on one would stop the deck. The service reads
//! the store once at start, on the main thread before the core exists
//! ([`VoiceKey::open`], [`VoiceKey::adopt`]). Every write while it runs goes
//! to the `deckd-keychain` thread ([`Keeper`]), and the core takes the
//! answer as a message. A store that refuses at start — the read, or the
//! move of a key found in the file — is not asked again in that run: the key
//! is read from and saved to `config.toml` until the next start.
//!
//! Tests use [`MemoryKeyStore`]. No test constructs [`Keychain`], and no
//! test service can reach it ([`home_for`]).
use crate::config::{Config, Paths};
use crossbeam_channel::{Receiver, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The Keychain item's service: the app's bundle identifier. The item has
/// no label of its own, so Keychain Access lists it under this name.
pub const SERVICE: &str = "ai.cohdl.openwalkietalkie";
/// The item's account: the dotted `config.toml` key it stands in for, which
/// is also the name `config.set` and `config.get` give it.
pub const ACCOUNT: &str = "voice.tts.api_key";
/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

/// The store as messages name it: the Keychain on a Mac, Credential
/// Manager on Windows (`CredentialStore`).
pub const STORE: &str = if cfg!(windows) { "Credential Manager" } else { "the Keychain" };
/// One item in it, as messages name it.
pub const STORE_ITEM: &str = if cfg!(windows) {
    "the Credential Manager entry"
} else {
    "the Keychain item"
};
/// The program an owner removes an item with by hand.
pub const STORE_APP: &str = if cfg!(windows) { "Credential Manager" } else { "Keychain Access" };

/// The store would not do what it was asked: its own words and code.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Refused(String),
}

/// Where the key is kept, behind a trait so that every test can use
/// [`MemoryKeyStore`].
pub trait KeyStore: Send + Sync + std::fmt::Debug {
    /// The key, or `None` when there is none. A blank one is none.
    fn get(&self) -> Result<Option<String>, StoreError>;
    /// Keep `key`, trimmed, in place of any other. A blank key deletes.
    fn set(&self, key: &str) -> Result<(), StoreError>;
    /// Remove the key. `false` when there was none.
    fn delete(&self) -> Result<bool, StoreError>;
    /// Whether there is a key, read from its attributes only, never the
    /// secret.
    fn exists(&self) -> Result<bool, StoreError>;
}

/// A stored value as a key: UTF-8, trimmed, and blank meaning none. Nothing
/// here writes anything else, so anything else is the store's problem.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn cleaned(bytes: &[u8]) -> Result<Option<String>, StoreError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| StoreError::Refused("the stored key is not text".into()))?;
    let key = text.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}

/// The key as it may be shown: its length and last four characters
/// (`Tts::api_key_hint`), never the key.
#[must_use]
pub fn hint(key: &str) -> String {
    crate::config::Tts {
        api_key: key.to_string(),
        ..Default::default()
    }
    .api_key_hint()
}

/// The login Keychain, through `security-framework`'s generic passwords.
/// The item it makes trusts the program that made it. The service and its
/// bundled command line are one signed program, so neither of them is asked
/// to allow the other.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct Keychain;

#[cfg(target_os = "macos")]
fn refused(e: &security_framework::base::Error) -> StoreError {
    StoreError::Refused(format!("{e} ({})", e.code()))
}

#[cfg(target_os = "macos")]
impl KeyStore for Keychain {
    fn get(&self) -> Result<Option<String>, StoreError> {
        match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) => cleaned(&bytes),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(refused(&e)),
        }
    }

    fn set(&self, key: &str) -> Result<(), StoreError> {
        let key = key.trim();
        if key.is_empty() {
            return self.delete().map(drop);
        }
        security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key.as_bytes())
            .map_err(|e| refused(&e))
    }

    fn delete(&self) -> Result<bool, StoreError> {
        match security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT) {
            Ok(()) => Ok(true),
            Err(e) if e.code() == NOT_FOUND => Ok(false),
            Err(e) => Err(refused(&e)),
        }
    }

    fn exists(&self) -> Result<bool, StoreError> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(ACCOUNT)
            .load_attributes(true)
            .limit(Limit::Max(1))
            .search();
        match found {
            Ok(items) => Ok(!items.is_empty()),
            Err(e) if e.code() == NOT_FOUND => Ok(false),
            Err(e) => Err(refused(&e)),
        }
    }
}

/// Windows Credential Manager: one generic credential, target
/// `ai.cohdl.openwalkietalkie/voice.tts.api_key`, kept for this user on
/// this computer (`CRED_PERSIST_LOCAL_MACHINE`, which never roams). Windows
/// encrypts it with the user's logon (DPAPI); Control Panel › Credential
/// Manager › Windows Credentials lists it.
#[cfg(windows)]
#[derive(Debug)]
pub struct CredentialStore {
    target: String,
}

#[cfg(windows)]
impl Default for CredentialStore {
    fn default() -> Self {
        CredentialStore {
            target: format!("{SERVICE}/{ACCOUNT}"),
        }
    }
}

#[cfg(windows)]
mod credential {
    use std::ffi::c_void;

    pub const CRED_TYPE_GENERIC: u32 = 1;
    pub const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    pub const ERROR_NOT_FOUND: i32 = 1168;

    /// `CREDENTIALW`.
    #[repr(C)]
    pub struct Credential {
        pub flags: u32,
        pub kind: u32,
        pub target_name: *mut u16,
        pub comment: *mut u16,
        pub last_written: [u32; 2],
        pub blob_size: u32,
        pub blob: *mut u8,
        pub persist: u32,
        pub attribute_count: u32,
        pub attributes: *mut c_void,
        pub target_alias: *mut u16,
        pub user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn CredReadW(target: *const u16, kind: u32, flags: u32, credential: *mut *mut Credential) -> i32;
        pub fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        pub fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        pub fn CredFree(buffer: *const c_void);
    }

    pub fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn last_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }
}

#[cfg(windows)]
fn refused(e: &std::io::Error) -> StoreError {
    StoreError::Refused(e.to_string())
}

#[cfg(windows)]
impl CredentialStore {
    /// A store under another target name: a test's own, never the key's.
    #[cfg(test)]
    fn named(target: &str) -> CredentialStore {
        CredentialStore { target: target.to_string() }
    }

    /// The target name, wide and NUL-terminated.
    fn target(&self) -> Vec<u16> {
        credential::wide(&self.target)
    }

    /// The stored bytes, `None` when there is no such credential.
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
        use credential::*;
        let target = self.target();
        let mut found: *mut Credential = std::ptr::null_mut();
        // SAFETY: a NUL-terminated target; on success `found` points at a
        // buffer Windows allocated, freed below with `CredFree`.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) } == 0 {
            let e = last_error();
            return if e.raw_os_error() == Some(ERROR_NOT_FOUND) { Ok(None) } else { Err(refused(&e)) };
        }
        // SAFETY: `found` is the credential CredReadW returned; its blob is
        // `blob_size` bytes long (or null when empty).
        let bytes = unsafe {
            let credential = &*found;
            let bytes = if credential.blob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(credential.blob, credential.blob_size as usize).to_vec()
            };
            CredFree(found.cast());
            bytes
        };
        Ok(Some(bytes))
    }
}

#[cfg(windows)]
impl KeyStore for CredentialStore {
    fn get(&self) -> Result<Option<String>, StoreError> {
        match self.read()? {
            Some(bytes) => cleaned(&bytes),
            None => Ok(None),
        }
    }

    fn set(&self, key: &str) -> Result<(), StoreError> {
        use credential::*;
        let key = key.trim();
        if key.is_empty() {
            return self.delete().map(drop);
        }
        let mut target = self.target();
        let mut user = wide(ACCOUNT);
        let mut blob = key.as_bytes().to_vec();
        let credential = Credential {
            flags: 0,
            kind: CRED_TYPE_GENERIC,
            target_name: target.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            last_written: [0, 0],
            blob_size: u32::try_from(blob.len()).map_err(|_| StoreError::Refused("the key is too long".into()))?,
            blob: blob.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: user.as_mut_ptr(),
        };
        // SAFETY: every pointer is to a live buffer of this frame; Windows
        // copies the credential before returning.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(refused(&last_error()));
        }
        Ok(())
    }

    fn delete(&self) -> Result<bool, StoreError> {
        use credential::*;
        let target = self.target();
        // SAFETY: a NUL-terminated target.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let e = last_error();
            return if e.raw_os_error() == Some(ERROR_NOT_FOUND) { Ok(false) } else { Err(refused(&e)) };
        }
        Ok(true)
    }

    /// Credential Manager has no read of the attributes alone: the secret is
    /// read and dropped at once.
    fn exists(&self) -> Result<bool, StoreError> {
        Ok(self.read()?.is_some_and(|bytes| !bytes.iter().all(u8::is_ascii_whitespace)))
    }
}

/// A store in memory, which every test uses in place of the Keychain. It
/// counts its reads and writes, and it can be made to refuse, at once or
/// later.
#[derive(Default)]
pub struct MemoryKeyStore {
    key: Mutex<Option<String>>,
    refusal: Mutex<Option<String>>,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

impl std::fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryKeyStore")
            .field("key", &self.current().as_deref().map(hint))
            .field("refusal", &*lock(&self.refusal))
            .finish()
    }
}

/// A poisoned lock is a test that already failed; its value is still the
/// one to read.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl MemoryKeyStore {
    /// A store that already holds `key`.
    #[must_use]
    pub fn with(key: &str) -> MemoryKeyStore {
        let store = MemoryKeyStore::default();
        *lock(&store.key) = Some(key.to_string());
        store
    }

    /// A store that refuses everything with `why`, the way a locked
    /// Keychain or a dialog answered "Deny" would.
    #[must_use]
    pub fn failing(why: &str) -> MemoryKeyStore {
        let store = MemoryKeyStore::default();
        store.refuse(Some(why));
        store
    }

    /// Refuse from now on with `why`, or answer again with `None`.
    pub fn refuse(&self, why: Option<&str>) {
        *lock(&self.refusal) = why.map(String::from);
    }

    /// What it holds, without counting as a read.
    #[must_use]
    pub fn current(&self) -> Option<String> {
        lock(&self.key).clone()
    }

    #[must_use]
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn writes(&self) -> usize {
        self.writes.load(Ordering::Relaxed)
    }

    fn answer(&self) -> Result<(), StoreError> {
        match &*lock(&self.refusal) {
            Some(why) => Err(StoreError::Refused(why.clone())),
            None => Ok(()),
        }
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self) -> Result<Option<String>, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.answer()?;
        Ok(self.current())
    }

    fn set(&self, key: &str) -> Result<(), StoreError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.answer()?;
        let key = key.trim();
        *lock(&self.key) = (!key.is_empty()).then(|| key.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<bool, StoreError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.answer()?;
        Ok(lock(&self.key).take().is_some())
    }

    fn exists(&self) -> Result<bool, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.answer()?;
        Ok(self.current().is_some())
    }
}

/// Where a process keeps the key.
#[derive(Clone, Debug)]
pub enum KeyHome {
    /// `config.toml`'s `[voice.tts] api_key`.
    File,
    /// A store: the Keychain on a Mac, [`MemoryKeyStore`] in a test.
    Store(Arc<dyn KeyStore>),
}

impl KeyHome {
    /// How `config.get` names it (`voice."tts.key_store"`): `"keychain"`
    /// or `"config"`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            KeyHome::File => "config",
            KeyHome::Store(_) => "keychain",
        }
    }

    /// The store, for a home that has one.
    #[must_use]
    pub fn store(&self) -> Option<&Arc<dyn KeyStore>> {
        match self {
            KeyHome::File => None,
            KeyHome::Store(store) => Some(store),
        }
    }
}

/// The account's own home directory, from the user database. Never `HOME`,
/// which a test or a harness redirects while the login Keychain stays the
/// account's.
#[cfg(unix)]
#[must_use]
pub fn real_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: `getpwuid` returns a pointer to static storage or null; the
    // directory is copied out before anything else could call it.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

#[cfg(not(unix))]
#[must_use]
pub fn real_home() -> Option<PathBuf> {
    None
}

/// Whether the key belongs in the Keychain for a service on `paths`: on a
/// Mac, for the data directory of the account's own install,
/// `<real home>/Library/Application Support/OpenWalkieTalkie`, and for no
/// other.
#[must_use]
pub fn keychain_applies(paths: &Paths, real_home: Option<&Path>) -> bool {
    cfg!(target_os = "macos")
        && real_home.is_some_and(|home| {
            paths.data == home.join("Library/Application Support/OpenWalkieTalkie")
        })
}

/// Where a process on `paths` keeps the key: the Keychain only for a Mac's
/// own install with a deck ([`keychain_applies`]), `config.toml` otherwise,
/// and always for `--no-device`.
#[must_use]
pub fn home_for(paths: &Paths, no_device: bool) -> KeyHome {
    #[cfg(target_os = "macos")]
    if !no_device && keychain_applies(paths, real_home().as_deref()) {
        return KeyHome::Store(Arc::new(Keychain));
    }
    #[cfg(windows)]
    if !no_device && credential_store_applies(paths, std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new)) {
        return KeyHome::Store(Arc::new(CredentialStore::default()));
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (paths, no_device);
    KeyHome::File
}

/// Windows' counterpart of [`keychain_applies`]: Credential Manager holds the
/// key for the account's own install, `<%LOCALAPPDATA%>\OpenWalkieTalkie`
/// (compared case-blind, as Windows names folders), and for no data folder
/// of a test's or a `--data-dir` service's own.
#[must_use]
pub fn credential_store_applies(paths: &Paths, local_app_data: Option<&Path>) -> bool {
    cfg!(windows)
        && local_app_data.is_some_and(|local| {
            paths.data.to_string_lossy().to_lowercase()
                == local.join("OpenWalkieTalkie").to_string_lossy().to_lowercase()
        })
}

/// The key as a running service holds it: where it lives and, for a store,
/// what the store holds as this run last read or wrote it, so that the core
/// never has to ask the store.
pub struct VoiceKey {
    home: KeyHome,
    stored: Option<String>,
    /// The key from the file that is on its way to the store, so that a
    /// second read of the same file does not send it twice.
    moving: Option<String>,
    /// The store refused a key from the file in this run. The key stays in
    /// the file until the next start, so the store is not asked again.
    refused: bool,
}

impl std::fmt::Debug for VoiceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceKey")
            .field("home", &self.word())
            .field("stored", &self.stored.as_deref().map(hint))
            .field("moving", &self.moving.is_some())
            .field("refused", &self.refused)
            .finish()
    }
}

impl VoiceKey {
    /// The key in `config.toml`: every test core, every `--no-device`
    /// daemon, Linux.
    #[must_use]
    pub fn file() -> VoiceKey {
        VoiceKey {
            home: KeyHome::File,
            stored: None,
            moving: None,
            refused: false,
        }
    }

    /// `home`, its store read once. Call it off the core thread: the
    /// service calls it on the main thread before the core exists. A store
    /// that refuses is not asked again in this run, and the key is read
    /// from `config.toml` until the next start.
    #[must_use]
    pub fn open(home: KeyHome) -> VoiceKey {
        let stored = match home.store().map(|store| store.get()) {
            None => return VoiceKey::file(),
            Some(Ok(stored)) => stored,
            Some(Err(why)) => {
                tracing::warn!(
                    "voice: the Keychain refused this service ({why}): the cloud voice key is \
                     read from config.toml until the next start"
                );
                return VoiceKey::file();
            }
        };
        VoiceKey {
            home,
            stored,
            ..VoiceKey::file()
        }
    }

    /// Whether a store keeps the key.
    #[must_use]
    pub fn is_store(&self) -> bool {
        self.home.store().is_some()
    }

    /// `config.get`'s `voice."tts.key_store"`: `"keychain"` or `"config"`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        self.home.word()
    }

    /// The store, for the thread that writes it.
    #[must_use]
    pub fn store(&self) -> Option<Arc<dyn KeyStore>> {
        self.home.store().cloned()
    }

    /// What the store holds, as last read or written.
    #[must_use]
    pub fn stored(&self) -> Option<&str> {
        self.stored.as_deref()
    }

    /// Whether the store holds a key (the Footprint's Keychain row). `None`
    /// for the file, whose key is part of the data directory.
    #[must_use]
    pub fn present(&self) -> Option<bool> {
        self.is_store().then_some(self.stored.is_some())
    }

    /// The store holds `key` now (`None`: no key), after a write the keeper
    /// made.
    pub fn took(&mut self, key: Option<String>) {
        if self.is_store() {
            self.stored = key;
        }
    }

    /// `config` as the service runs with it: the stored key in `[voice.tts]
    /// api_key` when the file has none. A key in the file always wins,
    /// because it has not moved yet or the store refused it.
    #[must_use]
    pub fn overlay(&self, config: Arc<Config>) -> Arc<Config> {
        match &self.stored {
            Some(key) if self.is_store() && !config.voice.tts.has_key() => {
                let mut next = (*config).clone();
                next.voice.tts.api_key.clone_from(key);
                Arc::new(next)
            }
            _ => config,
        }
    }

    /// Move a key found in `file`, the config just read from `config.toml`,
    /// into the store and out of the file, and give back the config now on
    /// disk. This is for the service's start, on the main thread. A store
    /// that refuses leaves the file as it was, and the key in it is used: the
    /// run is then the file's, as after a read the store refused. A file
    /// that cannot be rewritten keeps its copy of a key the store now holds
    /// too.
    pub fn adopt(&mut self, paths: &Paths, file: Config) -> Config {
        let Some(store) = self.store() else {
            return file;
        };
        if !file.voice.tts.has_key() {
            return file;
        }
        let key = file.voice.tts.api_key.trim().to_string();
        if let Err(why) = store.set(&key) {
            tracing::warn!(
                "voice: the Keychain refused the cloud voice key ({why}); it stays in config.toml, \
                 where the key is kept until the next start"
            );
            // As after a read the store refused: the run is the file's. The
            // service says so (`tts.key_store`), Save Key writes the file,
            // and the store that just refused is not asked again.
            self.home = KeyHome::File;
            self.stored = None;
            self.refused = true;
            return file;
        }
        self.stored = Some(key.clone());
        match remove_from_file(paths, &key) {
            Ok(Some(next)) => {
                tracing::info!(
                    "voice: the cloud voice key moved from config.toml into the Keychain ({})",
                    hint(&key)
                );
                next
            }
            Ok(None) => file,
            Err(why) => {
                tracing::warn!(
                    "voice: the cloud voice key is in the Keychain, and config.toml still holds \
                     it: {why}"
                );
                file
            }
        }
    }

    /// The key in `file` a running service should move into the store now:
    /// a store home, a key in the file, no refusal in this run and not
    /// already on its way. Marks it on its way.
    pub fn to_move(&mut self, file: &Config) -> Option<String> {
        if !self.is_store() || self.refused || !file.voice.tts.has_key() {
            return None;
        }
        let key = file.voice.tts.api_key.trim().to_string();
        if self.moving.as_deref() == Some(key.as_str()) {
            return None;
        }
        self.moving = Some(key.clone());
        Some(key)
    }

    /// The move of `key` has been answered. Taken: the store holds it.
    /// Refused: the key stays in the file, and the store is not asked again
    /// in this run.
    pub fn moved(&mut self, key: &str, taken: bool) {
        if self.moving.as_deref() == Some(key) {
            self.moving = None;
        }
        if taken {
            self.took(Some(key.to_string()));
        } else {
            self.refused = true;
        }
    }
}

/// Take `key` out of `config.toml`'s `[voice.tts]` and give back the config
/// now on disk. Everything else in the file stays byte for byte
/// (`config::edit`). `None` when the file no longer holds that key: another
/// write got there first, and nothing newer is undone.
pub fn remove_from_file(paths: &Paths, key: &str) -> Result<Option<Config>, String> {
    let text = match std::fs::read_to_string(&paths.config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", paths.config.display())),
    };
    let current = Config::parse(&text).map_err(|e| e.to_string())?;
    if current.voice.tts.api_key.trim() != key {
        return Ok(None);
    }
    let edited =
        crate::config::edit(&text, ACCOUNT, &serde_json::Value::Null).map_err(|e| e.to_string())?;
    let next = Config::parse(&edited).map_err(|e| format!("the result would not load: {e}"))?;
    crate::config::write(&paths.config, &edited).map_err(|e| e.to_string())?;
    Ok(Some(next))
}

/// One write the keeper made: what it wrote (`None` for a delete), how it
/// went, and the note it came with.
pub struct Done<T> {
    pub key: Option<String>,
    pub result: Result<(), StoreError>,
    pub then: T,
}

/// The `deckd-keychain` thread: every store write of a running service, in
/// the order asked, each answered with the note it came with (`T`, the
/// core's own) so that the core can carry on when the answer lands. The core
/// never waits for it.
pub struct Keeper<T> {
    jobs: Option<Sender<(Option<String>, T)>>,
    /// One answer per write. The core selects on it beside its other
    /// workers.
    pub events: Receiver<Done<T>>,
    /// Kept so that `events` never disconnects: a disconnected `select!` arm
    /// is always ready (the trap `DesktopHandle` documents).
    _idle: Sender<Done<T>>,
}

impl<T: Send + 'static> Keeper<T> {
    /// No store, no thread: a file home.
    #[must_use]
    pub fn none() -> Keeper<T> {
        let (idle, events) = crossbeam_channel::unbounded();
        Keeper {
            jobs: None,
            events,
            _idle: idle,
        }
    }

    /// The thread, over `store`. It ends when the keeper is dropped.
    #[must_use]
    pub fn spawn(store: Arc<dyn KeyStore>) -> Keeper<T> {
        let (jobs, asked) = crossbeam_channel::unbounded::<(Option<String>, T)>();
        let (idle, events) = crossbeam_channel::unbounded();
        let answers = idle.clone();
        let spawned = std::thread::Builder::new()
            .name("deckd-keychain".into())
            .spawn(move || {
                for (key, then) in asked {
                    let result = match &key {
                        Some(key) => store.set(key),
                        None => store.delete().map(drop),
                    };
                    if answers.send(Done { key, result, then }).is_err() {
                        break;
                    }
                }
            });
        match spawned {
            Ok(_) => Keeper {
                jobs: Some(jobs),
                events,
                _idle: idle,
            },
            Err(e) => {
                tracing::warn!(error = %e, "voice: no Keychain thread; the key cannot be changed");
                Keeper::none()
            }
        }
    }

    /// Hand the thread a write: `key`, or `None` to delete. Gives the note
    /// back when there is no thread to take it.
    pub fn post(&self, key: Option<String>, then: T) -> Result<(), T> {
        match &self.jobs {
            Some(jobs) => jobs
                .send((key, then))
                .map_err(|refused| refused.into_inner().1),
            None => Err(then),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;
    use std::time::Duration;

    fn paths_in(dir: &TestDir) -> Paths {
        let data = dir.sub("data");
        Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"))
    }

    fn store_home(store: &Arc<MemoryKeyStore>) -> KeyHome {
        KeyHome::Store(store.clone())
    }

    /// The store's four calls: a key goes in trimmed, a blank one is none,
    /// a delete says whether there was one, and every call is counted.
    #[test]
    fn the_memory_store_keeps_one_key_and_counts_every_call() {
        let store = MemoryKeyStore::default();
        assert_eq!(store.get(), Ok(None));
        assert_eq!(store.exists(), Ok(false));
        store.set("  sk-test-0123456789abcd \n").unwrap();
        assert_eq!(store.get(), Ok(Some("sk-test-0123456789abcd".into())));
        assert_eq!(store.exists(), Ok(true));
        store.set("   ").unwrap();
        assert_eq!(store.current(), None, "a blank key deletes");
        store.set("sk-again").unwrap();
        assert_eq!(store.delete(), Ok(true));
        assert_eq!(store.delete(), Ok(false), "nothing left to delete");
        assert_eq!((store.reads(), store.writes()), (4, 5));
        // Refusing, then answering again.
        store.refuse(Some("User interaction is not allowed. (-25308)"));
        let refused = StoreError::Refused("User interaction is not allowed. (-25308)".into());
        assert_eq!(store.set("sk-x"), Err(refused.clone()));
        assert_eq!(store.get(), Err(refused));
        store.refuse(None);
        assert_eq!(store.get(), Ok(None));
        assert_eq!(
            MemoryKeyStore::with("sk-1").current().as_deref(),
            Some("sk-1")
        );
        assert!(MemoryKeyStore::failing("locked").exists().is_err());
        // The key never shows in a `{:?}`.
        let shown = format!("{:?}", MemoryKeyStore::with("sk-test-0123456789abcd"));
        assert!(!shown.contains("sk-test"), "{shown}");
    }

    #[test]
    fn a_stored_value_is_text_trimmed_and_blank_is_none() {
        assert_eq!(cleaned(b" sk-1 \n"), Ok(Some("sk-1".into())));
        assert_eq!(cleaned(b"  "), Ok(None));
        assert!(cleaned(&[0xff, 0xfe]).is_err());
    }

    /// A store that refuses at start is read once and then left alone for
    /// the run: the key is the file's, and nothing is overlaid or moved.
    #[test]
    fn a_store_that_refuses_at_start_is_not_asked_again() {
        let store = Arc::new(MemoryKeyStore::failing("locked"));
        let mut key = VoiceKey::open(store_home(&store));
        assert!(!key.is_store());
        assert_eq!((key.word(), key.present()), ("config", None));
        let mut config = Config::default();
        config.voice.tts.api_key = "sk-in-the-file".into();
        assert_eq!(key.to_move(&config), None);
        let dir = TestDir::new("secrets-refused-start");
        let paths = paths_in(&dir);
        let adopted = key.adopt(&paths, config.clone());
        assert_eq!(adopted, config);
        assert_eq!((store.reads(), store.writes()), (1, 0));
    }

    /// The overlay puts the stored key where the file has none, never over
    /// a key in the file, and a file home passes the config through as it
    /// came.
    #[test]
    fn the_overlay_fills_an_empty_file_key_and_a_file_key_wins() {
        let store = Arc::new(MemoryKeyStore::with("sk-stored-0123456789"));
        let key = VoiceKey::open(store_home(&store));
        assert_eq!((key.word(), key.present()), ("keychain", Some(true)));
        let bare = Arc::new(Config::default());
        let overlaid = key.overlay(bare.clone());
        assert_eq!(overlaid.voice.tts.api_key, "sk-stored-0123456789");
        assert!(
            bare.voice.tts.api_key.is_empty(),
            "the file's config is not touched"
        );
        let mut pasted = Config::default();
        pasted.voice.tts.api_key = "sk-pasted".into();
        let pasted = Arc::new(pasted);
        assert!(Arc::ptr_eq(&key.overlay(pasted.clone()), &pasted));
        let file = VoiceKey::file();
        assert!(Arc::ptr_eq(&file.overlay(bare.clone()), &bare));
        assert_eq!(store.writes(), 0, "an overlay writes nothing");
        // Nothing stored: nothing to fill.
        let empty = VoiceKey::open(store_home(&Arc::new(MemoryKeyStore::default())));
        assert_eq!(empty.present(), Some(false));
        assert!(Arc::ptr_eq(&empty.overlay(bare.clone()), &bare));
    }

    /// At start a key in `config.toml` moves into the store, and the file
    /// loses that one line: the comment and the provider line beside it stay
    /// byte for byte.
    #[test]
    fn adopt_moves_a_file_key_and_leaves_every_other_byte() {
        let dir = TestDir::new("secrets-adopt");
        let paths = paths_in(&dir);
        let original = "# mine\n[api]\nport = 47201\n\n[voice.tts]\n# whose voice\nprovider = \"elevenlabs\"\napi_key = \"sk-test-0123456789abcd\"\nengine = \"cloud\"\n";
        std::fs::write(&paths.config, original).unwrap();
        let file = Config::parse(original).unwrap();
        let store = Arc::new(MemoryKeyStore::default());
        let mut key = VoiceKey::open(store_home(&store));
        let next = key.adopt(&paths, file);
        assert_eq!(store.current().as_deref(), Some("sk-test-0123456789abcd"));
        assert_eq!(key.stored(), Some("sk-test-0123456789abcd"));
        assert!(!next.voice.tts.has_key());
        assert_eq!(next.voice.tts.provider, "elevenlabs");
        let text = std::fs::read_to_string(&paths.config).unwrap();
        assert_eq!(
            text,
            original.replace("api_key = \"sk-test-0123456789abcd\"\n", ""),
            "only the key's line went"
        );
        // In force, the key is the stored one.
        assert_eq!(
            key.overlay(Arc::new(next)).voice.tts.api_key,
            "sk-test-0123456789abcd"
        );
    }

    /// A store that refuses the key at start leaves the file exactly as it
    /// was, and the key in it stays in force.
    #[test]
    fn adopt_leaves_the_file_alone_when_the_store_refuses() {
        let dir = TestDir::new("secrets-adopt-refused");
        let paths = paths_in(&dir);
        let original = "[voice.tts]\napi_key = \"sk-test-0123456789abcd\"\n";
        std::fs::write(&paths.config, original).unwrap();
        let store = Arc::new(MemoryKeyStore::default());
        let mut key = VoiceKey::open(store_home(&store));
        store.refuse(Some("locked"));
        let file = Config::parse(original).unwrap();
        let next = key.adopt(&paths, file.clone());
        assert_eq!(next, file);
        assert_eq!(std::fs::read_to_string(&paths.config).unwrap(), original);
        assert_eq!(key.stored(), None);
        assert_eq!(
            key.overlay(Arc::new(next)).voice.tts.api_key,
            "sk-test-0123456789abcd"
        );
        // …and a running service does not offer it to the store again.
        assert_eq!(key.to_move(&file), None);
        // The run is the file's, as after a read the store refused: the
        // service says the key is in config.toml, and Save Key writes there.
        assert_eq!(
            (key.word(), key.is_store(), key.present()),
            ("config", false, None)
        );
        assert_eq!(
            (store.reads(), store.writes()),
            (1, 1),
            "asked once each, then left alone"
        );
    }

    /// A running service moves a key once: a second read of the same file
    /// does not send it again, an answer clears the way, and a refusal stops
    /// further moves until the next start.
    #[test]
    fn a_running_service_moves_each_key_once_and_stops_after_a_refusal() {
        let store = Arc::new(MemoryKeyStore::default());
        let mut key = VoiceKey::open(store_home(&store));
        let mut file = Config::default();
        file.voice.tts.api_key = " sk-pasted ".into();
        assert_eq!(key.to_move(&file).as_deref(), Some("sk-pasted"));
        assert_eq!(key.to_move(&file), None, "already on its way");
        key.moved("sk-pasted", true);
        assert_eq!(key.stored(), Some("sk-pasted"));
        file.voice.tts.api_key = "sk-newer".into();
        assert_eq!(key.to_move(&file).as_deref(), Some("sk-newer"));
        key.moved("sk-newer", false);
        assert_eq!(
            key.stored(),
            Some("sk-pasted"),
            "a refusal keeps what the store had"
        );
        file.voice.tts.api_key = "sk-newest".into();
        assert_eq!(
            key.to_move(&file),
            None,
            "refused once: not again in this run"
        );
        // A file home never moves anything.
        assert_eq!(VoiceKey::file().to_move(&file), None);
    }

    /// The file's copy is taken out only while it is still the key that
    /// moved: a newer paste stays.
    #[test]
    fn remove_from_file_leaves_a_newer_key_alone() {
        let dir = TestDir::new("secrets-remove");
        let paths = paths_in(&dir);
        assert_eq!(remove_from_file(&paths, "sk-1"), Ok(None), "no file");
        std::fs::write(&paths.config, "[voice.tts]\napi_key = \"sk-2\"\n").unwrap();
        assert_eq!(remove_from_file(&paths, "sk-1"), Ok(None));
        assert!(std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("sk-2"));
        let next = remove_from_file(&paths, "sk-2")
            .unwrap()
            .expect("taken out");
        assert!(!next.voice.tts.has_key());
        assert_eq!(
            std::fs::read_to_string(&paths.config).unwrap(),
            "[voice.tts]\n"
        );
    }

    /// The Keychain is the home of the account's own install on a Mac and of
    /// nothing else: not a test's data directory, not one under a redirected
    /// home, never off macOS, and never for `--no-device`.
    #[test]
    fn the_keychain_is_only_for_the_accounts_own_install_on_a_mac() {
        let home = Path::new("/Users/someone");
        let own = Paths::from_data_dir(
            home.join("Library/Application Support/OpenWalkieTalkie"),
            home.join("Library/Logs/OpenWalkieTalkie"),
            home.join("Library/Caches/OpenWalkieTalkie"),
        );
        assert_eq!(
            keychain_applies(&own, Some(home)),
            cfg!(target_os = "macos")
        );
        assert!(!keychain_applies(&own, None));
        // The same layout under a home that is not the account's own.
        assert!(!keychain_applies(&own, Some(Path::new("/Users/other"))));
        let dir = TestDir::new("secrets-home");
        let mine = paths_in(&dir);
        assert!(!keychain_applies(&mine, Some(home)));
        // A redirected HOME's default layout is still not the real home's.
        let redirected = dir.path().join("home");
        let under = Paths::from_data_dir(
            redirected.join("Library/Application Support/OpenWalkieTalkie"),
            redirected.join("Library/Logs/OpenWalkieTalkie"),
            redirected.join("Library/Caches/OpenWalkieTalkie"),
        );
        assert!(!keychain_applies(&under, Some(home)));
        // What every test service gets.
        assert_eq!(home_for(&mine, false).word(), "config");
        assert_eq!(home_for(&mine, true).word(), "config");
        assert_eq!(home_for(&under, true).word(), "config");
    }

    /// Windows: Credential Manager is the home of the account's own install
    /// (`%LOCALAPPDATA%\OpenWalkieTalkie`, case-blind) and of nothing else.
    #[test]
    fn credential_manager_is_only_for_the_accounts_own_install_on_windows() {
        let local = Path::new(r"C:\Users\someone\AppData\Local");
        let own = Paths::from_data_dir(
            local.join("OpenWalkieTalkie"),
            local.join(r"OpenWalkieTalkie\logs"),
            local.join(r"OpenWalkieTalkie\caches"),
        );
        assert_eq!(credential_store_applies(&own, Some(local)), cfg!(windows));
        let lower = Paths::from_data_dir(local.join("openwalkietalkie"), local.join("l"), local.join("c"));
        assert_eq!(credential_store_applies(&lower, Some(local)), cfg!(windows), "case-blind");
        assert!(!credential_store_applies(&own, None));
        assert!(!credential_store_applies(&own, Some(Path::new(r"C:\Users\other\AppData\Local"))));
        let dir = TestDir::new("secrets-credential-home");
        assert!(!credential_store_applies(&paths_in(&dir), Some(local)));
    }

    /// The real Credential Manager, under a target of the test's own (never
    /// the key's): written, read back trimmed, found, deleted, and a second
    /// delete finds nothing.
    #[cfg(windows)]
    #[test]
    fn credential_manager_keeps_the_key_and_lets_it_go() {
        let store = CredentialStore::named(&format!("{SERVICE}/test-{}", std::process::id()));
        let _ = store.delete();
        assert_eq!(store.get(), Ok(None));
        assert_eq!(store.exists(), Ok(false));
        store.set("  sk-test-1234  ").unwrap();
        assert_eq!(store.get(), Ok(Some("sk-test-1234".into())));
        assert_eq!(store.exists(), Ok(true));
        store.set("sk-test-5678").unwrap();
        assert_eq!(store.get(), Ok(Some("sk-test-5678".into())), "replaced");
        assert_eq!(store.delete(), Ok(true));
        assert_eq!(store.delete(), Ok(false));
        assert_eq!(store.get(), Ok(None));
    }

    /// The keeper writes in the order it was asked and hands each note back
    /// with its answer; with no store it gives the note straight back.
    #[test]
    fn the_keeper_answers_each_write_with_its_note() {
        let store = Arc::new(MemoryKeyStore::default());
        let keeper: Keeper<u32> = Keeper::spawn(store.clone());
        keeper.post(Some("sk-1".into()), 1).unwrap();
        keeper.post(None, 2).unwrap();
        keeper.post(Some("sk-3".into()), 3).unwrap();
        let mut notes = Vec::new();
        for _ in 0..3 {
            let done = keeper
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("the keeper answers");
            assert_eq!(done.result, Ok(()));
            notes.push((done.then, done.key));
        }
        assert_eq!(
            notes,
            [
                (1, Some("sk-1".into())),
                (2, None),
                (3, Some("sk-3".into()))
            ]
        );
        assert_eq!(store.current().as_deref(), Some("sk-3"));
        store.refuse(Some("locked"));
        keeper.post(Some("sk-4".into()), 4).unwrap();
        let done = keeper.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(done.result, Err(StoreError::Refused("locked".into())));
        assert_eq!(done.then, 4);
        let none: Keeper<u32> = Keeper::none();
        assert_eq!(none.post(Some("sk-5".into()), 5), Err(5));
        assert!(none.events.try_recv().is_err());
    }
}
