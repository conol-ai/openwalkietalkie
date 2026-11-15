//! `deckd uninstall [--purge]` (design §10.5, §15's exit test).
//!
//! The exit test is one sentence — **uninstall leaves the CLIs' configs as
//! they were** — and week 3 already built the machinery that makes it true:
//! every installer write goes through `integrations::backup`, which *moves*
//! the original into `<data>/backups/` before publishing, and
//! `Verb::Uninstall` takes back exactly the entries this installer appended
//! and puts the displaced bytes back. So this module does not re-implement
//! any of that; it decides *what to run and in what order*, and the order is
//! the part that can go wrong:
//!
//! 1. **the CLIs first.** `--purge` deletes the data directory, and
//!    `<data>/backups/` is inside it. Purging before uninstalling would throw
//!    away the only copy of the operator's original `settings.json`, which is
//!    the exact failure the exit test exists to catch.
//! 2. **the LaunchAgent next**, so nothing restarts the daemon while its
//!    files are being taken apart.
//! 3. **the data directory last**, and only with `--purge`, and with it
//!    the Keychain item that holds the cloud voice's key on a Mac
//!    (`crate::secrets`), when one is there.
//!
//! Two things this deliberately does **not** do, and says so instead:
//! `SMAppService.unregister()` for the agent and the `mainApp` login item
//! is the GUI's call (there is no Rust binding for it here, and a
//! `launchctl bootout` of an SMAppService registration is not the same
//! thing); and `tccutil reset All ai.cohdl.openwalkietalkie` is **printed,
//! not run** — a CLI that silently revokes microphone grants is a CLI
//! nobody can undo.
//!
//! Logs are kept, always. They are the record of what ran, they contain
//! nothing content-bearing unless the operator turned `[privacy]
//! log_content` on, and an uninstall that deletes the evidence of why the
//! operator is uninstalling is the wrong shape.
use crate::config::Paths;
use crate::integrations::{self, Launchctl, Status, Target, Verb};
use crate::secrets::{KeyHome, SERVICE};
use std::path::{Path, PathBuf};

/// Targets with managed files to remove before deleting receipts and backups.
pub const CLI_TARGETS: [Target; 3] = [Target::Claude, Target::Codex, Target::CodexDesktop];

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Also delete the data directory, the caches and the GUI's preferences.
    pub purge: bool,
}

/// What the run did, in the order it did it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Outcome {
    /// One line per thing that happened.
    pub steps: Vec<String>,
    /// Files put back, for the byte-for-byte claim.
    pub restored: Vec<PathBuf>,
    /// What was deliberately left behind, and why.
    pub kept: Vec<String>,
    /// What the operator still has to do themselves.
    pub next_steps: Vec<String>,
    /// What did not work. A non-empty list is a non-zero exit.
    pub problems: Vec<String>,
}

impl Outcome {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.steps {
            out.push_str(&format!("  {line}\n"));
        }
        if !self.kept.is_empty() {
            out.push_str("\nkept:\n");
            for line in &self.kept {
                out.push_str(&format!("  {line}\n"));
            }
        }
        if !self.next_steps.is_empty() {
            out.push_str("\nstill yours to do:\n");
            for line in &self.next_steps {
                out.push_str(&format!("  {line}\n"));
            }
        }
        if !self.problems.is_empty() {
            out.push_str("\nPROBLEMS:\n");
            for line in &self.problems {
                out.push_str(&format!("  {line}\n"));
            }
        }
        out
    }
}

/// Take deckd back out. See the module docs for why the order is the order.
/// `keys` is where this install keeps the cloud voice's key
/// (`crate::secrets::home_for`): a store's item goes with `--purge`, and a
/// key in `config.toml` goes with the data directory.
pub fn run(env: &integrations::Env, paths: &Paths, opts: Options, keys: &KeyHome) -> Outcome {
    let mut outcome = Outcome::default();

    // 1. The CLIs' own files, before anything can delete `<data>/backups`.
    for target in CLI_TARGETS {
        let installed = integrations::status(env, Some(target))
            .first()
            .map(|report| report.status != Status::Absent)
            .unwrap_or(false);
        if !installed {
            outcome
                .steps
                .push(format!("{}: nothing installed", target.display_name()));
            continue;
        }
        match integrations::run(env, target, Verb::Uninstall) {
            Ok(receipt) => {
                outcome.steps.push(format!(
                    "{}: {} file(s) put back as they were",
                    target.display_name(),
                    receipt.changed.len()
                ));
                outcome.restored.extend(receipt.changed);
                outcome.next_steps.extend(receipt.next_steps);
                for conflict in receipt.conflicts {
                    outcome.problems.push(conflict);
                }
            }
            Err(why) => outcome
                .problems
                .push(format!("{}: {why}", target.display_name())),
        }
    }

    // 2. The LaunchAgent: the loaded job, then a plist on disk. Both halves
    //    are needed — `deckd install --dev` writes a plist into
    //    `~/Library/LaunchAgents` and bootstraps it, and either can outlive
    //    the other.
    //    The agent under the project's old name goes the same way, quietly
    //    when it was never there.
    //    Windows has no launchd: the service stops through its own door and
    //    its sign-in starts are Run values (`windows_service`).
    #[cfg(windows)]
    windows_service(env, paths, &mut outcome);
    #[cfg(not(windows))]
    for label in [
        crate::launchd::DECKD_LABEL,
        crate::launchd::LEGACY_DECKD_LABEL,
    ] {
        let legacy = label == crate::launchd::LEGACY_DECKD_LABEL;
        match bootout(env, label) {
            Ok(true) => outcome.steps.push(format!(
                "launchd: booted {label} out of gui/{}",
                crate::launchd::uid()
            )),
            Ok(false) if !legacy => outcome
                .steps
                .push(format!("launchd: {label} was not loaded")),
            Ok(false) => {}
            Err(why) => outcome.problems.push(why),
        }
        let plist = env
            .home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        if plist.exists() {
            match std::fs::remove_file(&plist) {
                Ok(()) => outcome
                    .steps
                    .push(format!("launchd: removed {}", plist.display())),
                Err(e) => outcome
                    .problems
                    .push(format!("cannot remove {}: {e}", plist.display())),
            }
        }
    }
    #[cfg(not(windows))]
    outcome.next_steps.push(
        "the app itself: quit OpenWalkieTalkie and drag it to the Trash. Its SMAppService \
         registration and the `mainApp` login item are unregistered by the app (Settings → \
         Uninstall…), not by this command."
            .into(),
    );
    #[cfg(windows)]
    outcome.next_steps.push(
        "the app itself: quit OpenWalkieTalkie (its tray icon's Quit) and delete the folder it \
         was unzipped to."
            .into(),
    );

    // 3. The data, last, and only when asked. On Windows the logs live in
    //    the data directory, and they are kept there.
    #[cfg(windows)]
    if opts.purge {
        match remove_dir_except(&paths.data, &paths.logs) {
            Ok(true) => outcome
                .steps
                .push(format!("purged {} (its logs kept)", paths.data.display())),
            Ok(false) => outcome
                .steps
                .push(format!("purged {} (was absent)", paths.data.display())),
            Err(e) => outcome.problems.push(e),
        }
        // The app's preferences are in that folder too
        // (`%LOCALAPPDATA%\openwalkietalkie\app-prefs.json`), and went with it.
    }
    if opts.purge && cfg!(not(windows)) {
        for dir in [&paths.data, &paths.caches] {
            match remove_dir(dir) {
                Ok(true) => outcome.steps.push(format!("purged {}", dir.display())),
                Ok(false) => outcome
                    .steps
                    .push(format!("purged {} (was absent)", dir.display())),
                Err(e) => outcome.problems.push(e),
            }
        }
        let prefs = env
            .home
            .join("Library/Preferences/ai.cohdl.openwalkietalkie.plist");
        if prefs.exists() {
            match std::fs::remove_file(&prefs) {
                Ok(()) => outcome.steps.push(format!("purged {}", prefs.display())),
                Err(e) => outcome
                    .problems
                    .push(format!("cannot remove {}: {e}", prefs.display())),
            }
        }
        if let Some(store) = keys.store() {
            match store.delete() {
                Ok(true) => outcome.steps.push(format!(
                    "removed the cloud voice key from the Keychain ({SERVICE})"
                )),
                Ok(false) => {}
                Err(why) => outcome.problems.push(format!(
                    "cannot remove the Keychain item {SERVICE}: {why}; remove it in Keychain \
                     Access"
                )),
            }
        }
        if cfg!(target_os = "macos") {
            outcome.next_steps.push(
                "to give back the TCC grants as well, run `tccutil reset All \
                 ai.cohdl.openwalkietalkie` — our own bundle id and nothing else. This command \
                 does not run it for you."
                    .into(),
            );
        }
    } else if !opts.purge {
        outcome.kept.push(format!(
            "{} — settings, tokens and the deck's remembered slots. `deckd uninstall --purge` \
             deletes them.",
            paths.data.display()
        ));
        if keys.store().is_some_and(|store| store.exists() == Ok(true)) {
            outcome.kept.push(format!(
                "the cloud voice key in the Keychain item {SERVICE} — `deckd uninstall --purge` \
                 deletes it."
            ));
        }
    }
    outcome.kept.push(format!(
        "{} — the logs are the record of what ran and are never deleted, purge or not.",
        paths.logs.display()
    ));
    outcome
}

/// Windows: stop the running service (`service.stop` through its own door:
/// a clean exit, which its host does not restart) and remove the sign-in
/// starts of the service and the app (their Run values). Skipped in a test
/// environment, as `launchctl` is: it would stop the operator's own service.
#[cfg(windows)]
fn windows_service(env: &integrations::Env, paths: &Paths, outcome: &mut Outcome) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if env.launchctl == Launchctl::Disabled {
        return;
    }
    let stopped = std::env::current_exe().ok().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("--data-dir")
            .arg(&paths.data)
            .args(["ctl", "--scopes", "admin", "--timeout", "5", "service.stop"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
    });
    match stopped {
        Some(out) if out.status.success() => outcome.steps.push("stopped the running service".into()),
        // 2: no service answered, so none was running.
        Some(out) if out.status.code() == Some(2) => outcome.steps.push("the service was not running".into()),
        Some(out) => outcome.problems.push(format!(
            "the service did not stop: {}; end deckd.exe in Task Manager",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        None => outcome
            .problems
            .push("cannot ask the service to stop; end deckd.exe in Task Manager".into()),
    }
    let reg = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join(r"System32\reg.exe");
    for value in ["OpenWalkieTalkie Service", "OpenWalkieTalkie"] {
        let removed = std::process::Command::new(&reg)
            .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", value, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match removed {
            Ok(out) if out.status.success() => {
                outcome.steps.push(format!("startup: removed {value}"));
            }
            // reg exits 1 when there was no such value.
            Ok(_) => {}
            Err(e) => outcome.problems.push(format!("cannot run reg.exe: {e}")),
        }
    }
}

/// Remove everything in `dir` but `keep` (the logs); `Ok(false)` when `dir`
/// was not there. A service that is still letting go of its files gets a
/// few moments.
#[cfg(windows)]
fn remove_dir_except(dir: &Path, keep: &Path) -> Result<bool, String> {
    if !dir.exists() {
        return Ok(false);
    }
    let mut last = None;
    for _ in 0..10 {
        last = None;
        let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path == keep {
                continue;
            }
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = removed {
                last = Some(format!("cannot remove {}: {e}", path.display()));
            }
        }
        if last.is_none() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    Err(last.unwrap_or_default())
}

/// Remove a directory tree; `Ok(false)` when it was not there.
fn remove_dir(dir: &Path) -> Result<bool, String> {
    if !dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)
        .map(|()| true)
        .map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

/// `launchctl bootout gui/<uid>/<label>`, guarded by a `print` so an absent
/// job is not an error, and disabled outright in a test environment — a
/// `bootout` from a unit test would stop the operator's own running service.
#[cfg(not(windows))]
fn bootout(env: &integrations::Env, label: &str) -> Result<bool, String> {
    if env.launchctl == Launchctl::Disabled {
        return Ok(false);
    }
    let service = format!("gui/{}/{label}", crate::launchd::uid());
    if crate::launchd::print(label).is_none() {
        return Ok(false);
    }
    let out = std::process::Command::new("/bin/launchctl")
        .args(["bootout", &service])
        .output()
        .map_err(|e| format!("cannot run launchctl: {e}"))?;
    if out.status.success() {
        return Ok(true);
    }
    Err(format!(
        "`launchctl bootout {service}` failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;

    /// A `Paths` whose data directory is the test environment's, so a purge
    /// deletes the throwaway tree and nothing else.
    fn paths_for(env: &integrations::Env, dir: &TestDir) -> Paths {
        Paths::from_data_dir(env.data.clone(), dir.sub("logs"), dir.sub("caches"))
    }

    #[test]
    fn launchctl_is_never_run_from_a_test_environment() {
        let dir = TestDir::new("uninstall-launchctl");
        let env = integrations::test_env(&dir);
        assert_eq!(env.launchctl, Launchctl::Disabled);
        assert_eq!(bootout(&env, crate::launchd::DECKD_LABEL), Ok(false));
    }

    /// Without `--purge` the data directory stays, and the logs stay either
    /// way — both of them as a line the operator can read, not silently.
    #[test]
    fn data_stays_without_purge_and_the_logs_stay_with_it() {
        let dir = TestDir::new("uninstall-keep");
        let env = integrations::test_env(&dir);
        let paths = paths_for(&env, &dir);
        std::fs::write(paths.state.as_path(), b"{}").unwrap();
        let outcome = run(&env, &paths, Options { purge: false }, &KeyHome::File);
        assert!(paths.data.is_dir(), "the data dir stays");
        assert!(paths.state.exists(), "and what is in it");
        assert!(
            outcome.kept.iter().any(|k| k.contains("--purge")),
            "{:?}",
            outcome.kept
        );
        assert!(
            outcome.kept.iter().any(|k| k.contains("logs")),
            "{:?}",
            outcome.kept
        );
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    }

    #[test]
    fn purge_deletes_the_data_dir_and_the_caches_and_says_so() {
        let dir = TestDir::new("uninstall-purge");
        let env = integrations::test_env(&dir);
        let paths = paths_for(&env, &dir);
        std::fs::write(paths.state.as_path(), b"{}").unwrap();
        std::fs::write(paths.caches.join("chime.pcm"), b"x").unwrap();
        let outcome = run(&env, &paths, Options { purge: true }, &KeyHome::File);
        assert!(!paths.data.exists(), "{:?}", outcome.steps);
        assert!(!paths.caches.exists(), "{:?}", outcome.steps);
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        // The thing a purge must *not* do quietly.
        assert!(
            outcome
                .next_steps
                .iter()
                .any(|s| s.contains("tccutil reset All")),
            "{:?}",
            outcome.next_steps
        );
        // A key kept in config.toml went with the data directory: there is
        // no Keychain item to remove or to talk about.
        let all = [&outcome.steps, &outcome.next_steps, &outcome.kept];
        assert!(
            !all.iter()
                .any(|lines| lines.iter().any(|s| s.contains("Keychain"))),
            "{outcome:?}"
        );
        // And the logs are still named as kept.
        assert!(outcome.kept.iter().any(|k| k.contains("never deleted")));
    }

    /// On a Mac's own install the cloud voice's key is a Keychain item:
    /// `--purge` deletes it and says so, has nothing to say when there is
    /// none, and names a store that refuses as a problem the operator can
    /// act on. Without `--purge` the item stays, and the outcome says how to
    /// remove it.
    #[test]
    fn purge_deletes_the_keychain_item_and_a_plain_uninstall_names_it_kept() {
        use crate::secrets::{KeyStore, MemoryKeyStore};
        use std::sync::Arc;
        let removed = "removed the cloud voice key from the Keychain (ai.cohdl.openwalkietalkie)";
        let dir = TestDir::new("uninstall-keychain");
        let env = integrations::test_env(&dir);
        let paths = paths_for(&env, &dir);

        let store = Arc::new(MemoryKeyStore::with("sk-test-0123456789abcd"));
        let keep = run(
            &env,
            &paths,
            Options { purge: false },
            &KeyHome::Store(store.clone()),
        );
        assert_eq!(store.current().as_deref(), Some("sk-test-0123456789abcd"));
        assert!(
            keep.kept.contains(
                &"the cloud voice key in the Keychain item ai.cohdl.openwalkietalkie — `deckd \
                  uninstall --purge` deletes it."
                    .to_string()
            ),
            "{:?}",
            keep.kept
        );

        let purged = run(
            &env,
            &paths,
            Options { purge: true },
            &KeyHome::Store(store.clone()),
        );
        assert_eq!(store.current(), None);
        assert!(
            purged.steps.contains(&removed.to_string()),
            "{:?}",
            purged.steps
        );
        assert!(purged.problems.is_empty(), "{:?}", purged.problems);
        assert!(
            !purged.next_steps.iter().any(|s| s.contains("Keychain")),
            "{:?}",
            purged.next_steps
        );

        // Nothing stored: nothing removed, nothing kept, nothing wrong.
        let empty = Arc::new(MemoryKeyStore::default());
        let again = run(
            &env,
            &paths,
            Options { purge: true },
            &KeyHome::Store(empty.clone()),
        );
        assert!(
            !again.steps.iter().any(|s| s.contains("Keychain")),
            "{:?}",
            again.steps
        );
        assert!(again.problems.is_empty(), "{:?}", again.problems);
        let kept = run(
            &env,
            &paths,
            Options { purge: false },
            &KeyHome::Store(empty),
        );
        assert!(
            !kept.kept.iter().any(|s| s.contains("Keychain")),
            "{:?}",
            kept.kept
        );

        // A store that refuses: a problem, which is a non-zero exit.
        let locked = Arc::new(MemoryKeyStore::with("sk-1"));
        locked.refuse(Some("User interaction is not allowed. (-25308)"));
        let refused = run(
            &env,
            &paths,
            Options { purge: true },
            &KeyHome::Store(locked.clone()),
        );
        assert_eq!(
            refused.problems,
            [
                "cannot remove the Keychain item ai.cohdl.openwalkietalkie: User interaction is \
                 not allowed. (-25308); remove it in Keychain Access"
            ]
        );
        locked.refuse(None);
        assert_eq!(
            locked.get(),
            Ok(Some("sk-1".into())),
            "the item is still there"
        );
    }

    /// The ordering claim, as a test rather than as a comment: the backups
    /// live inside the data directory, so a purge that ran first would take
    /// the operator's original `settings.json` with it.
    #[test]
    fn a_purge_uninstalls_the_clis_before_it_deletes_the_backups_they_restore_from() {
        let dir = TestDir::new("uninstall-order");
        let env = integrations::test_env(&dir);
        let paths = paths_for(&env, &dir);
        // The one CLI file an install still merges into — Codex's
        // `hooks.json` (the Claude plugin lives in deckd's own data dir) —
        // is backed up on the way in and restored from that backup on the
        // way out, which is the case the order exists for.
        let settings = integrations::codex::hooks_path(&env);
        let before = b"{\n    \"description\": \"mine\",\n    \"hooks\": {}\n}\n".to_vec();
        std::fs::write(&settings, &before).unwrap();
        integrations::run(&env, Target::Codex, Verb::Install { trust: false }).unwrap();
        assert_ne!(std::fs::read(&settings).unwrap(), before, "install wrote");
        assert!(
            env.backups().is_dir() && env.backups().starts_with(&paths.data),
            "the backups are inside the data dir, which is what makes the order matter"
        );

        let outcome = run(&env, &paths, Options { purge: true }, &KeyHome::File);
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(
            std::fs::read(&settings).unwrap(),
            before,
            "the file came back before the backups were purged"
        );
        assert!(!paths.data.exists(), "and then the data dir went");
    }
}
