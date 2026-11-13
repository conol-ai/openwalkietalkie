//! §15's uninstall exit test: **uninstall leaves the CLIs' configs as they
//! were** (design §10.5, HOST-CONTRACT-W7 §4, §6).
//!
//! Week 3 proved the same claim for `deckd integrations uninstall <target>`.
//! This proves it for the command an operator actually types when they are
//! done — `deckd uninstall` — which is a different claim, because that
//! command also deletes things, and with `--purge` it deletes the directory
//! the backups live in. The ordering is the whole risk and it is what these
//! rows hold.
//!
//! Every test runs against a throwaway `CLAUDE_CONFIG_DIR` / `CODEX_HOME` /
//! `HOME` under `$TMPDIR` and removes it afterwards, and `launchctl` is
//! disabled in the test environment: **nothing here reads or writes the
//! operator's own `~/.claude`, `~/.codex` or `~/Library/LaunchAgents`, and
//! no test can boot their running service out.**
use deckd::config::Paths;
use deckd::integrations::testdir::TestDir;
use deckd::integrations::{self, Env, Launchctl, Status, Target, Verb};
use deckd::secrets::KeyHome;
use deckd::uninstall::{self, Options};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const INSTALL: Verb = Verb::Install { trust: false };

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn paths_for(env: &Env, dir: &TestDir) -> Paths {
    Paths::from_data_dir(env.data.clone(), dir.sub("logs"), dir.sub("caches"))
}

/// The three installer-owned files, with contents that are deliberately
/// **not** deckd's own formatting: four-space indent, the user's key order,
/// comments the installer has no reason to preserve unless it really is
/// putting the original bytes back rather than re-rendering them.
fn originals(env: &Env) -> Vec<(PathBuf, Vec<u8>)> {
    vec![
        (
            integrations::claude::settings_path(env),
            b"{\n    \"statusLine\": {\n        \"type\": \"command\",\n        \"command\": \"npx ccstatusline@latest\"\n    },\n    \"model\": \"claude-haiku-4-5\"\n}\n".to_vec(),
        ),
        (
            integrations::codex::hooks_path(env),
            b"{\n    \"description\": \"mine\",\n    \"hooks\": {\n        \"Stop\": [\n            {\n                \"hooks\": [\n                    {\n                        \"type\": \"command\",\n                        \"command\": \"/opt/mine.sh\"\n                    }\n                ]\n            }\n        ]\n    }\n}\n".to_vec(),
        ),
        (
            integrations::codex::config_path(env),
            b"# tony's codex\nmodel = \"gpt-6\"\n\n[features]\njs_repl = false\n".to_vec(),
        ),
    ]
}

/// Every file under `root`, by path, for a whole-tree before/after diff.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(path.display().to_string(), bytes);
            }
        }
    }
    out
}

#[test]
fn uninstall_leaves_the_clis_own_files_byte_for_byte_as_they_were() {
    let dir = TestDir::new("x-uninstall-cmd");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    let files = originals(&env);
    for (path, bytes) in &files {
        std::fs::write(path, bytes).unwrap();
    }
    // The whole of both CLI trees, before anything of ours touched them.
    let claude_before = snapshot_tree(&env.claude_dir());
    let codex_before = snapshot_tree(&env.codex_dir());

    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL).unwrap();
    }
    // Only hooks.json is merged into; the Claude plugin lives in deckd's own
    // data dir and config.toml is never written.
    let (hooks, hooks_before) = &files[1];
    assert_ne!(&read(hooks), hooks_before, "hooks.json was not changed");
    assert_eq!(
        read(&files[0].0),
        files[0].1,
        "settings.json is never written"
    );
    assert_eq!(
        read(&files[2].0),
        files[2].1,
        "config.toml is never written"
    );

    let outcome = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);

    for (path, before) in &files {
        assert_eq!(
            String::from_utf8_lossy(&read(path)),
            String::from_utf8_lossy(before),
            "{} did not come back byte for byte",
            path.display()
        );
    }
    // Not only the three files: nothing else in either tree moved either —
    // no stray backup, no leftover marker file, no new directory.
    assert_eq!(
        snapshot_tree(&env.claude_dir()),
        claude_before,
        "the Claude config dir is not as it was"
    );
    assert_eq!(
        snapshot_tree(&env.codex_dir()),
        codex_before,
        "the Codex home is not as it was"
    );
    for target in [Target::Claude, Target::Codex] {
        assert_eq!(
            integrations::status(&env, Some(target))[0].status,
            Status::Absent,
            "{target:?} still reports itself installed"
        );
    }
}

/// The same with `--purge`. This is the row the ordering exists for: the
/// backups live in `<data>/backups`, `--purge` deletes `<data>`, and an
/// uninstall that purged first would have thrown away the only copy of the
/// operator's original `settings.json` before it could put it back.
#[test]
fn purge_still_puts_the_clis_files_back_before_it_deletes_the_backups() {
    let dir = TestDir::new("x-uninstall-purge");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    let files = originals(&env);
    for (path, bytes) in &files {
        std::fs::write(path, bytes).unwrap();
    }
    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL).unwrap();
    }
    assert!(
        env.backups().starts_with(&paths.data),
        "the backups are inside the data dir; that is what makes the order load-bearing"
    );

    let outcome = uninstall::run(&env, &paths, Options { purge: true }, &KeyHome::File);
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    for (path, before) in &files {
        assert_eq!(
            String::from_utf8_lossy(&read(path)),
            String::from_utf8_lossy(before),
            "{} did not come back",
            path.display()
        );
    }
    assert!(!paths.data.exists(), "and then the data dir went");
}

/// A file deckd created from nothing is removed again rather than left as an
/// empty husk — the same rule `integrations uninstall` follows, reached
/// through the operator's command.
#[test]
fn a_file_deckd_created_is_removed_again_rather_than_left_empty() {
    let dir = TestDir::new("x-uninstall-fresh");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL).unwrap();
    }
    assert!(integrations::claude::hooks_json_path(&env).exists());
    assert!(
        !integrations::claude::settings_path(&env).exists(),
        "the plugin invents no settings.json"
    );

    let outcome = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    assert!(
        !integrations::claude::hooks_json_path(&env).exists(),
        "deckd created the plugin directory, so it removes it again"
    );
    assert!(!integrations::codex::hooks_path(&env).exists());
}

/// Uninstalling twice is not an error, and the second run says there was
/// nothing to do rather than reporting a failure the operator cannot act on.
#[test]
fn uninstalling_twice_is_a_no_op_and_not_a_failure() {
    let dir = TestDir::new("x-uninstall-twice");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    std::fs::write(integrations::claude::settings_path(&env), b"{}\n").unwrap();
    integrations::run(&env, Target::Claude, INSTALL).unwrap();
    let first = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(first.problems.is_empty(), "{:?}", first.problems);
    let second = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(second.problems.is_empty(), "{:?}", second.problems);
    assert!(
        second.steps.iter().any(|s| s.contains("nothing installed")),
        "{:?}",
        second.steps
    );
}

#[cfg(target_os = "macos")]
#[test]
fn full_uninstall_removes_the_desktop_launcher_before_purging_its_receipt() {
    let dir = TestDir::new("x-uninstall-desktop");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    integrations::run(&env, Target::CodexDesktop, INSTALL).unwrap();
    let app = integrations::codex_desktop::launcher(&env);
    assert!(app.exists());
    let outcome = uninstall::run(&env, &paths, Options { purge: true }, &KeyHome::File);
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    assert!(!app.exists());
    assert!(!paths.data.exists());
}

/// The constraint, as a test rather than as a comment in the module header:
/// a test environment never talks to `launchctl`, so nothing here can stop
/// the operator's own running service.
#[test]
fn no_test_environment_may_reach_launchctl() {
    let dir = TestDir::new("x-uninstall-launchctl");
    let env = integrations::test_env(&dir);
    assert_eq!(env.launchctl, Launchctl::Disabled);
    let paths = paths_for(&env, &dir);
    let outcome = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(
        outcome.steps.iter().any(|s| s.contains("was not loaded")),
        "{:?}",
        outcome.steps
    );
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    // And the app itself is the operator's to trash: the CLI says so rather
    // than pretending it unregistered the SMAppService job.
    assert!(
        outcome
            .next_steps
            .iter()
            .any(|s| s.contains("SMAppService")),
        "{:?}",
        outcome.next_steps
    );
}

/// The LaunchAgent plist left by an install from before the project was
/// renamed from OpenMicroDial goes with the current one.
#[test]
fn the_agent_under_the_old_project_name_is_removed_too() {
    let dir = TestDir::new("x-uninstall-legacy-agent");
    let env = integrations::test_env(&dir);
    let paths = paths_for(&env, &dir);
    let agents = env.home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let plists = [
        deckd::launchd::DECKD_LABEL,
        deckd::launchd::LEGACY_DECKD_LABEL,
    ]
    .map(|label| agents.join(format!("{label}.plist")));
    for plist in &plists {
        std::fs::write(plist, b"<plist/>").unwrap();
    }
    let outcome = uninstall::run(&env, &paths, Options { purge: false }, &KeyHome::File);
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    for plist in &plists {
        assert!(!plist.exists(), "{} survived", plist.display());
    }
}
