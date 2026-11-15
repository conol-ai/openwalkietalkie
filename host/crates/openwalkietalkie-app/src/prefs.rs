//! The window's own preferences: four things this Mac remembers for the app
//! and never tells the service — the colour of the deck's case, so the
//! picture matches the deck in the owner's hand; whether first run is done;
//! a language picked before any service had answered; and whether a deck
//! has ever been seen here, which nothing is added before (owner decision 7:
//! first run's **Add These** and the Service page's Start at login wait for
//! it).
//!
//! **Where they are kept depends on the run** ([`Store::choose`]), because
//! keeping one is a write to the owner's Mac like any other:
//!
//! * `OPENWALKIETALKIE_PREFS=<file>`: a JSON object in that file, whatever
//!   the run — a window on a test service keeps its preferences in the
//!   test's own scratch directory;
//! * the installed app on macOS: `NSUserDefaults.standardUserDefaults`, the
//!   app's own domain (`~/Library/Preferences/ai.cohdl.openwalkietalkie.plist`,
//!   the file the Footprint page names and `deckd uninstall --purge` removes);
//! * an installed copy anywhere else:
//!   `$XDG_CONFIG_HOME/openwalkietalkie/app-prefs.json`, or under
//!   `~/.config` when that is unset (on Windows, under `%LOCALAPPDATA%`);
//! * any other run — a `target/` build, a test, a harness walk — memory only:
//!   nothing is written anywhere, and the values last as long as the process.
//!
//! The values are read once, when the store is opened, and a change is
//! written through at once. There is no "save": a window that is killed has
//! lost nothing it showed as chosen.
use crate::app::Launch;
use deck_gui_core::i18n::Language;
use gpui::{App, Global};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// `OPENWALKIETALKIE_PREFS`: keep the preferences in this file.
pub const PREFS_ENV: &str = "OPENWALKIETALKIE_PREFS";

/// Where an installed copy outside macOS keeps them, under the user's
/// configuration directory.
pub const CONFIG_FILE: &str = "openwalkietalkie/app-prefs.json";

// The keys, the same in every store.
const CASE: &str = "case";
const FIRST_RUN_DONE: &str = "first_run_done";
const LANGUAGE: &str = "language";
const DECK_SEEN: &str = "deck_seen";

/// The colour of the deck's case. The deck does not report it, so the owner
/// says it once, in first run or on the Deck page, and the picture follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Case {
    #[default]
    White,
    Black,
}

impl Case {
    pub const ALL: [Case; 2] = [Case::White, Case::Black];

    /// The word it is kept under.
    pub const fn word(self) -> &'static str {
        match self {
            Case::White => "white",
            Case::Black => "black",
        }
    }

    /// A kept word, back. Anything else names no case this app draws.
    pub fn parse(word: &str) -> Option<Case> {
        Case::ALL.into_iter().find(|case| case.word() == word)
    }
}

/// Where the preferences are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Store {
    /// `NSUserDefaults.standardUserDefaults`: the installed app on macOS.
    Defaults,
    /// A JSON object in this file.
    File(PathBuf),
    /// Nowhere: they last as long as the process.
    Memory,
}

impl Store {
    /// The rule, with everything it depends on passed in: `file` is
    /// [`PREFS_ENV`], `macos` the platform, `config_dir` the user's
    /// configuration directory. The file the environment names wins in every
    /// run; then only the installed app, run for real, keeps them in the
    /// Mac's own store (or, outside macOS, in the configuration directory);
    /// everything else keeps them in memory.
    pub fn choose(
        file: Option<PathBuf>,
        launch: Launch,
        macos: bool,
        config_dir: Option<PathBuf>,
    ) -> Store {
        if let Some(file) = file {
            return Store::File(file);
        }
        if !launch.installed() {
            return Store::Memory;
        }
        if macos {
            return Store::Defaults;
        }
        config_dir.map_or(Store::Memory, |dir| Store::File(dir.join(CONFIG_FILE)))
    }
}

/// The preferences, as read when the store was opened and changed since.
///
/// A gpui [`Global`]: the boot opens it before the window exists, pages read
/// it with [`prefs`], and a change goes through `cx.update_global` (or
/// [`crate::shell::Shell::update_prefs`], which also redraws).
pub struct Prefs {
    store: Store,
    /// Everything the store held, keys this build does not know included, so
    /// writing one key never drops another.
    values: Map<String, Value>,
}

impl Global for Prefs {}

/// This app's preferences: the [`Prefs`] the boot opened.
pub fn prefs(cx: &App) -> &Prefs {
    cx.global::<Prefs>()
}

impl Prefs {
    /// Open this run's store ([`Store::choose`] over this process's
    /// environment) and read it.
    pub fn open(launch: Launch) -> Prefs {
        let file = std::env::var_os(PREFS_ENV)
            .filter(|file| !file.is_empty())
            .map(PathBuf::from);
        Prefs::load(Store::choose(
            file,
            launch,
            cfg!(target_os = "macos"),
            config_dir(),
        ))
    }

    /// Read `store`. A file that is missing, unreadable or not a JSON object
    /// reads as no preferences at all, and is replaced by the first change.
    pub fn load(store: Store) -> Prefs {
        let values = match &store {
            Store::Defaults => defaults::read(),
            Store::File(path) => read_file(path),
            Store::Memory => Map::new(),
        };
        Prefs { store, values }
    }

    /// Preferences that are kept nowhere.
    pub fn memory() -> Prefs {
        Prefs::load(Store::Memory)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The deck's case colour; white until the owner says otherwise.
    pub fn case(&self) -> Case {
        self.values
            .get(CASE)
            .and_then(Value::as_str)
            .and_then(Case::parse)
            .unwrap_or_default()
    }

    /// Whether first run has been finished, or found to have nothing left to
    /// set up.
    pub fn first_run_done(&self) -> bool {
        self.values
            .get(FIRST_RUN_DONE)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The language picked on this Mac that no service has taken yet. It
    /// exists only between first run's pick and the first connection that
    /// hands it to the service.
    pub fn language(&self) -> Option<Language> {
        let id = self.values.get(LANGUAGE)?.as_u64()?;
        Language::ALL
            .into_iter()
            .find(|language| u64::from(*language as u8) == id)
    }

    /// Whether a deck has ever been seen on this Mac: first run found one,
    /// or the service linked one. Set once, and never taken back.
    pub fn deck_seen(&self) -> bool {
        self.values
            .get(DECK_SEEN)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn set_case(&mut self, case: Case) {
        self.set(CASE, Some(Value::from(case.word())));
    }

    pub fn set_first_run_done(&mut self, done: bool) {
        self.set(FIRST_RUN_DONE, Some(Value::Bool(done)));
    }

    /// Remember that a deck has been seen on this Mac. Saying so again
    /// writes nothing.
    pub fn set_deck_seen(&mut self) {
        self.set(DECK_SEEN, Some(Value::Bool(true)));
    }

    /// Keep a language picked before any service answered, or (`None`)
    /// forget it once a service has taken it. The shell is what calls this
    /// (`Shell::choose_language` and the hand-over on connecting), because it
    /// speaks the pick until the service has it.
    pub fn set_language(&mut self, language: Option<Language>) {
        self.set(
            LANGUAGE,
            language.map(|language| Value::from(language as u8)),
        );
    }

    /// Change one key, and write it through. Setting what is already there
    /// writes nothing.
    fn set(&mut self, key: &'static str, value: Option<Value>) {
        if self.values.get(key) == value.as_ref() {
            return;
        }
        match value {
            Some(value) => self.values.insert(key.to_string(), value),
            None => self.values.remove(key),
        };
        let written = match &self.store {
            Store::Defaults => {
                defaults::write(key, self.values.get(key));
                Ok(())
            }
            Store::File(path) => write_file(path, &self.values),
            Store::Memory => Ok(()),
        };
        // The window keeps the value whatever the store said: a preference
        // that could not be kept is lost at the next start, not now.
        if let Err(e) = written {
            eprintln!("openwalkietalkie-app: could not keep the preference {key}: {e}");
        }
    }
}

/// The user's configuration directory as XDG names it: `$XDG_CONFIG_HOME`
/// when that is an absolute path, else `~/.config`.
#[cfg(not(windows))]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

/// Windows: `%LOCALAPPDATA%`, where the service keeps its own folder too
/// (`%LOCALAPPDATA%\OpenWalkieTalkie`, which the file names case-blind), so
/// the install guide's "delete it to remove everything" holds. Windows sets
/// no `HOME`: read as on Unix, the installed app kept nothing between runs.
#[cfg(windows)]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// A file store's object. Missing is the ordinary first start and says
/// nothing; anything else unreadable is said once, on stderr.
fn read_file(path: &Path) -> Map<String, Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(values)) => values,
            _ => {
                eprintln!(
                    "openwalkietalkie-app: {} holds no preferences; the first change replaces it",
                    path.display()
                );
                Map::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => {
            eprintln!(
                "openwalkietalkie-app: could not read {}: {e}",
                path.display()
            );
            Map::new()
        }
    }
}

/// The whole object, written beside the file and renamed over it, so a crash
/// part-way leaves the old file whole rather than half of the new one.
fn write_file(path: &Path, values: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(values).map_err(std::io::Error::other)?;
    text.push('\n');
    let mut beside = path.as_os_str().to_owned();
    beside.push(".tmp");
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, path)
}

/// `NSUserDefaults.standardUserDefaults`, the installed app's own domain.
/// Each key is kept as its own kind: the case as a string, first run and a
/// deck seen as booleans, the language as an integer.
#[cfg(target_os = "macos")]
mod defaults {
    use super::{CASE, DECK_SEEN, FIRST_RUN_DONE, LANGUAGE};
    use objc2_foundation::{NSString, NSUserDefaults};
    use serde_json::{Map, Value};

    /// The four keys, as far as the domain holds them.
    pub(super) fn read() -> Map<String, Value> {
        let defaults = NSUserDefaults::standardUserDefaults();
        let mut values = Map::new();
        if let Some(case) = defaults.stringForKey(&NSString::from_str(CASE)) {
            values.insert(CASE.to_string(), Value::String(case.to_string()));
        }
        // `boolForKey` and `integerForKey` answer false and 0 for a key that
        // is not there, so presence is asked first.
        for key in [FIRST_RUN_DONE, DECK_SEEN] {
            let name = NSString::from_str(key);
            if defaults.objectForKey(&name).is_some() {
                values.insert(key.to_string(), Value::Bool(defaults.boolForKey(&name)));
            }
        }
        let language = NSString::from_str(LANGUAGE);
        if defaults.objectForKey(&language).is_some() {
            values.insert(
                LANGUAGE.to_string(),
                Value::from(defaults.integerForKey(&language)),
            );
        }
        values
    }

    /// One key: set to `value`, or removed.
    pub(super) fn write(key: &str, value: Option<&Value>) {
        let defaults = NSUserDefaults::standardUserDefaults();
        let key = NSString::from_str(key);
        match value {
            Some(Value::String(word)) => {
                let word = NSString::from_str(word);
                // SAFETY: an NSString is a property-list object, the one kind
                // of value the defaults database keeps.
                unsafe { defaults.setObject_forKey(Some(&word), &key) };
            }
            Some(Value::Bool(on)) => defaults.setBool_forKey(*on, &key),
            Some(Value::Number(number)) => {
                if let Some(number) = number.as_i64().and_then(|n| isize::try_from(n).ok()) {
                    defaults.setInteger_forKey(number, &key);
                }
            }
            None | Some(Value::Null) => defaults.removeObjectForKey(&key),
            // Nothing this app keeps is a list or a map.
            Some(Value::Array(_) | Value::Object(_)) => {}
        }
    }
}

/// [`Store::Defaults`] is only ever chosen on macOS; anywhere else it holds
/// nothing and keeps nothing.
#[cfg(not(target_os = "macos"))]
mod defaults {
    use serde_json::{Map, Value};

    pub(super) fn read() -> Map<String, Value> {
        Map::new()
    }

    pub(super) fn write(_key: &str, _value: Option<&Value>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An empty directory of the test's own under the system's temporary
    /// one, removed again when the test is done with it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "openwalkietalkie-prefs-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn saved(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_store_is_the_named_file_then_the_installed_apps_own_then_memory() {
        let installed = Launch {
            bundled: true,
            ..Launch::default()
        };
        let file = PathBuf::from("/tmp/owt-run/prefs.json");
        let config = PathBuf::from("/home/owner/.config");
        let harness = Launch {
            harness: true,
            ..installed
        };
        let test_service = Launch {
            data_dir: true,
            ..installed
        };
        // The file the environment names wins in every run.
        for launch in [installed, harness, test_service, Launch::default()] {
            for macos in [true, false] {
                assert_eq!(
                    Store::choose(Some(file.clone()), launch, macos, Some(config.clone())),
                    Store::File(file.clone())
                );
            }
        }
        // The installed app keeps them in the Mac's own store, or outside
        // macOS in the configuration directory.
        assert_eq!(
            Store::choose(None, installed, true, Some(config.clone())),
            Store::Defaults
        );
        assert_eq!(
            Store::choose(None, installed, false, Some(config.clone())),
            Store::File(config.join("openwalkietalkie/app-prefs.json"))
        );
        assert_eq!(Store::choose(None, installed, false, None), Store::Memory);
        // A build, a harness walk and a window on a test service write
        // nowhere.
        for launch in [Launch::default(), harness, test_service] {
            for macos in [true, false] {
                assert_eq!(
                    Store::choose(None, launch, macos, Some(config.clone())),
                    Store::Memory,
                    "{launch:?}"
                );
            }
        }
    }

    #[test]
    fn a_file_keeps_every_preference_across_a_reopen() {
        let scratch = Scratch::new("reopen");
        // The directory does not exist yet: a first change makes it.
        let path = scratch.0.join("openwalkietalkie/app-prefs.json");
        let mut prefs = Prefs::load(Store::File(path.clone()));
        assert_eq!(
            (prefs.case(), prefs.first_run_done(), prefs.language(), prefs.deck_seen()),
            (Case::White, false, None, false)
        );
        assert!(!path.exists(), "reading writes nothing");
        prefs.set_case(Case::Black);
        prefs.set_first_run_done(true);
        prefs.set_language(Some(Language::Japanese));
        prefs.set_deck_seen();
        let again = Prefs::load(Store::File(path.clone()));
        assert_eq!(
            (again.case(), again.first_run_done(), again.language(), again.deck_seen()),
            (Case::Black, true, Some(Language::Japanese), true)
        );
        assert_eq!(
            saved(&path),
            json!({"case": "black", "first_run_done": true, "language": 3, "deck_seen": true})
        );
        let beside = scratch.0.join("openwalkietalkie/app-prefs.json.tmp");
        assert!(
            !beside.exists(),
            "the file is renamed into place, not left beside it"
        );
    }

    #[test]
    fn a_language_the_service_has_taken_leaves_no_key_behind() {
        let scratch = Scratch::new("language");
        let path = scratch.0.join("prefs.json");
        let mut prefs = Prefs::load(Store::File(path.clone()));
        prefs.set_language(Some(Language::SimplifiedChinese));
        assert_eq!(saved(&path), json!({"language": 1}));
        prefs.set_language(None);
        assert_eq!(prefs.language(), None);
        assert_eq!(saved(&path), json!({}));
        assert_eq!(Prefs::load(Store::File(path)).language(), None);
    }

    #[test]
    fn a_damaged_or_foreign_file_reads_as_nothing_chosen_and_keeps_what_it_does_not_know() {
        let scratch = Scratch::new("foreign");
        let path = scratch.0.join("prefs.json");
        // Words this build never writes, and a key it does not know.
        std::fs::write(
            &path,
            r#"{"case":"teal","first_run_done":"yes","language":9,"window":{"x":1}}"#,
        )
        .unwrap();
        let mut prefs = Prefs::load(Store::File(path.clone()));
        assert_eq!(
            (prefs.case(), prefs.first_run_done(), prefs.language()),
            (Case::White, false, None)
        );
        prefs.set_case(Case::Black);
        let after = saved(&path);
        assert_eq!(after["case"], "black");
        assert_eq!(after["window"], json!({"x": 1}), "a key it does not know");
        // Not JSON at all: nothing chosen, and the first change replaces it.
        std::fs::write(&path, "case = black").unwrap();
        let mut prefs = Prefs::load(Store::File(path.clone()));
        assert_eq!(prefs.case(), Case::White);
        prefs.set_first_run_done(true);
        assert_eq!(saved(&path), json!({"first_run_done": true}));
    }

    #[test]
    fn setting_what_is_already_there_writes_nothing() {
        let scratch = Scratch::new("unchanged");
        let path = scratch.0.join("prefs.json");
        let mut prefs = Prefs::load(Store::File(path.clone()));
        prefs.set_case(Case::Black);
        std::fs::remove_file(&path).unwrap();
        prefs.set_case(Case::Black);
        assert!(!path.exists(), "the same value again is no change");
        prefs.set_case(Case::White);
        assert_eq!(saved(&path), json!({"case": "white"}));
    }

    #[test]
    fn memory_keeps_a_change_for_the_process_alone() {
        let mut prefs = Prefs::memory();
        assert_eq!(prefs.store(), &Store::Memory);
        prefs.set_case(Case::Black);
        prefs.set_first_run_done(true);
        prefs.set_language(Some(Language::TraditionalChinese));
        prefs.set_deck_seen();
        assert_eq!(
            (prefs.case(), prefs.first_run_done(), prefs.language(), prefs.deck_seen()),
            (Case::Black, true, Some(Language::TraditionalChinese), true)
        );
        // Another opening of memory has nothing: nothing was kept anywhere.
        let fresh = Prefs::memory();
        assert_eq!(
            (fresh.case(), fresh.first_run_done(), fresh.language(), fresh.deck_seen()),
            (Case::White, false, None, false)
        );
    }

    #[test]
    fn a_deck_seen_is_kept_once_and_a_word_that_is_not_true_is_not_seen() {
        let scratch = Scratch::new("deck-seen");
        let path = scratch.0.join("prefs.json");
        // Only a true is a deck seen: a string or a number is not.
        for foreign in [r#"{"deck_seen":"yes"}"#, r#"{"deck_seen":1}"#, r#"{"deck_seen":false}"#] {
            std::fs::write(&path, foreign).unwrap();
            assert!(!Prefs::load(Store::File(path.clone())).deck_seen(), "{foreign}");
        }
        std::fs::remove_file(&path).unwrap();
        let mut prefs = Prefs::load(Store::File(path.clone()));
        prefs.set_deck_seen();
        assert_eq!(saved(&path), json!({"deck_seen": true}));
        // Seen again: the same value, and nothing is written.
        std::fs::remove_file(&path).unwrap();
        prefs.set_deck_seen();
        assert!(!path.exists());
        assert!(prefs.deck_seen());
    }

    #[test]
    fn a_case_is_kept_by_its_word_and_every_language_by_its_id() {
        for case in Case::ALL {
            assert_eq!(Case::parse(case.word()), Some(case));
        }
        assert_eq!(Case::parse("teal"), None);
        assert_eq!(Case::default(), Case::White);
        let mut prefs = Prefs::memory();
        for language in Language::ALL {
            prefs.set_language(Some(language));
            assert_eq!(prefs.language(), Some(language));
        }
    }
}
