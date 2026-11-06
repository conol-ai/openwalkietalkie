//! The installer's exit tests (HOST-CONTRACT-W3 §6, rows 11–16), plus the two
//! that talk to the real CLIs.
//!
//! Every test runs against a throwaway `CLAUDE_CONFIG_DIR` / `CODEX_HOME` /
//! `HOME` under `$TMPDIR` and removes it afterwards. **Nothing here ever
//! reads or writes the user's own `~/.claude`, `~/.codex` or
//! `~/Library/LaunchAgents`**, and `launchctl` is disabled in the test
//! environment so no test can boot the user's running deckd out.
use deckd::integrations::testdir::TestDir;
use deckd::integrations::{self, test_env, Env, Status, Target, Verb};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const INSTALL: Verb = Verb::Install { trust: false };

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn json_at(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&read(path)).expect("valid JSON")
}

fn claude_settings(env: &Env) -> std::path::PathBuf {
    integrations::claude::settings_path(env)
}

// ---------------------------------------------------------------------------
// §6 row 11
// ---------------------------------------------------------------------------

/// Installing twice writes once. The Claude target writes its own plugin
/// files (under deckd's data dir, nothing of the user's) and the Codex target
/// merges into `hooks.json`; the second run changes nothing because
/// `backup::apply` filters a write whose bytes already match, and the one
/// user file that *is* touched was moved into `<data>/backups` first, so it
/// is recoverable byte for byte. `config.toml` is no longer written at all.
#[test]
fn install_is_idempotent_and_backs_up_every_file_it_touches() {
    let dir = TestDir::new("x-idempotent");
    let env = test_env(&dir);
    let settings = claude_settings(&env);
    let hooks = integrations::codex::hooks_path(&env);
    let config = integrations::codex::config_path(&env);
    let settings_before = b"{\n  \"model\": \"claude-haiku-4-5\"\n}\n".to_vec();
    let hooks_before = b"{\n  \"hooks\": {}\n}\n".to_vec();
    let config_before = b"# mine\nmodel = \"gpt-6\"\n".to_vec();
    std::fs::write(&settings, &settings_before).unwrap();
    std::fs::write(&hooks, &hooks_before).unwrap();
    std::fs::write(&config, &config_before).unwrap();

    // Claude: four files of ours, created, none of the user's touched.
    let first = integrations::run(&env, Target::Claude, INSTALL).expect("first install");
    assert_eq!(
        first.changed,
        vec![
            integrations::claude::marketplace_json_path(&env),
            integrations::claude::plugin_json_path(&env),
            integrations::claude::hooks_json_path(&env),
            integrations::claude::mcp_json_path(&env),
        ]
    );
    assert!(
        first.backups.is_empty(),
        "nothing pre-existing was replaced"
    );
    assert_eq!(
        read(&settings),
        settings_before,
        "settings.json is not ours to write"
    );
    assert!(first.changed.iter().all(|p| p.starts_with(&env.data)));

    // Codex: hooks.json merged and backed up; config.toml left alone.
    let first = integrations::run(&env, Target::Codex, INSTALL).expect("first install");
    assert_eq!(first.changed, vec![hooks.clone()]);
    assert_eq!(first.backups.len(), 1);
    assert_eq!(
        read(&first.backups[0]),
        hooks_before,
        "the backup is the original"
    );
    assert!(
        first.backups[0].starts_with(env.backups()),
        "backups live in the daemon's data dir, not beside the user's config"
    );
    assert_eq!(
        read(&config),
        config_before,
        "config.toml is no longer written"
    );

    for target in [Target::Claude, Target::Codex] {
        let second = integrations::run(&env, target, INSTALL).expect("second install");
        assert!(
            second.changed.is_empty(),
            "{target:?} rewrote {:?}",
            second.changed
        );
        assert!(second.backups.is_empty());
    }
    assert_eq!(
        integrations::status(&env, Some(Target::Claude))[0].status,
        Status::Installed
    );
    // Codex: written, and untrusted until the `/hooks` review (C14).
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Untrusted
    );
}

// ---------------------------------------------------------------------------
// §6 row 12
// ---------------------------------------------------------------------------

/// Somebody else's hooks are never edited. For Claude that is now trivially
/// true — the plugin puts nothing into `settings.json` — and the test pins
/// it; for Codex the group is appended after the stranger's and reported.
#[test]
fn a_foreign_hook_on_the_same_event_is_reported_not_overwritten() {
    let dir = TestDir::new("x-foreign");
    let env = test_env(&dir);
    let foreign_claude = serde_json::json!({
        "hooks": {
            "Stop": [{"hooks": [
                {"type": "command", "command": "/usr/local/bin/notify", "args": ["stopped"]}
            ]}],
            "PermissionRequest": [{"matcher": "Bash", "hooks": [
                {"type": "http", "url": "https://audit.example/hook", "timeout": 5}
            ]}],
        }
    });
    let claude_bytes = serde_json::to_vec_pretty(&foreign_claude).unwrap();
    std::fs::write(claude_settings(&env), &claude_bytes).unwrap();
    let foreign_codex = serde_json::json!({
        "hooks": {"PermissionRequest": [{"hooks": [
            {"type": "command", "command": "/opt/audit.sh", "timeout": 2}
        ]}]}
    });
    std::fs::write(
        integrations::codex::hooks_path(&env),
        serde_json::to_vec_pretty(&foreign_codex).unwrap(),
    )
    .unwrap();

    let receipt = integrations::run(&env, Target::Claude, INSTALL).unwrap();
    assert_eq!(
        read(&claude_settings(&env)),
        claude_bytes,
        "settings.json untouched, byte for byte"
    );
    assert!(receipt.conflicts.is_empty(), "{:?}", receipt.conflicts);
    assert_eq!(
        integrations::status(&env, Some(Target::Claude))[0].status,
        Status::Installed
    );

    let receipt = integrations::run(&env, Target::Codex, INSTALL).unwrap();
    let hooks = json_at(&integrations::codex::hooks_path(&env));
    assert_eq!(
        hooks["hooks"]["PermissionRequest"][0],
        foreign_codex["hooks"]["PermissionRequest"][0]
    );
    assert!(
        receipt
            .conflicts
            .iter()
            .any(|c| c.starts_with("PermissionRequest")),
        "{:?}",
        receipt.conflicts
    );

    // And an uninstall leaves the strangers exactly where they were.
    integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
    assert_eq!(read(&claude_settings(&env)), claude_bytes);
    integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
    assert_eq!(
        json_at(&integrations::codex::hooks_path(&env))["hooks"]["PermissionRequest"],
        foreign_codex["hooks"]["PermissionRequest"]
    );
}

// ---------------------------------------------------------------------------
// §6 row 13 — the pre-plugin install, taken back out
// ---------------------------------------------------------------------------

/// A `settings.json` exactly as the pre-plugin installer left one, beside a
/// user's own hook, allowlist pattern and status line.
fn legacy_settings(env: &Env) -> serde_json::Value {
    let exe = env.exe.display().to_string();
    let url = "http://127.0.0.1:47201/v1/hooks/claude";
    let http = |timeout: u64| {
        serde_json::json!({"hooks": [{
            "type": "http", "url": url, "timeout": timeout,
            "headers": {"Authorization": "Bearer dk_legacy_secret"}
        }]})
    };
    serde_json::json!({
        "model": "claude-haiku-4-5",
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": exe, "args": ["hook", "claude"]}]}],
            "PermissionRequest": [http(130)],
            "PreToolUse": [{"matcher": "AskUserQuestion", "hooks": http(130)["hooks"]}],
            "UserPromptSubmit": [http(2)],
            "Stop": [
                {"hooks": [{"type": "command", "command": "/usr/local/bin/notify", "args": ["stopped"]}]},
                http(2)
            ],
            "SessionEnd": [http(1)],
        },
        "allowedHttpHookUrls": ["https://audit.example/*", "http://127.0.0.1:47201/v1/hooks/*"],
        "statusLine": {
            "type": "command",
            "command": format!(
                "{} hook claude-statusline -- 'npx ccstatusline@latest'",
                deckd::integrations::markers::shell_word(&exe)
            ),
        },
        "_openwalkietalkie": {
            "schema": 1, "deckd": exe, "port": 47201, "url": url,
            "entries": ["SessionStart", "PermissionRequest", "PreToolUse(AskUserQuestion)", "UserPromptSubmit", "Stop", "SessionEnd"],
            "allowlist": {"appended": true, "added": false, "patterns": ["http://127.0.0.1:47201/v1/hooks/*"]},
            "previousStatusLine": {"type": "command", "command": "npx ccstatusline@latest"},
        },
    })
}

/// A machine that installed the http-hook shape gets it taken back out by the
/// plugin install — only deckd's own entries, by fingerprint (C24), only its
/// own `allowedHttpHookUrls` pattern (C3), the user's `statusLine` restored
/// (C23) — and `status` says `outdated` until that has happened.
#[test]
fn a_pre_plugin_install_is_migrated_and_only_its_own_entries_are_taken_back() {
    let dir = TestDir::new("x-migrate");
    let env = test_env(&dir);
    let settings = claude_settings(&env);
    std::fs::write(
        &settings,
        serde_json::to_vec_pretty(&legacy_settings(&env)).unwrap(),
    )
    .unwrap();
    let before = &integrations::status(&env, Some(Target::Claude))[0];
    assert_eq!(before.status, Status::Outdated);
    assert!(
        before.detail.as_deref().unwrap().contains("pre-plugin"),
        "{:?}",
        before.detail
    );

    let plan = integrations::plan(&env, Target::Claude, INSTALL).unwrap();
    assert!(
        plan.notes.iter().any(|n| n.contains("pre-plugin")),
        "the plan says it migrates: {:?}",
        plan.notes
    );
    let receipt = integrations::run(&env, Target::Claude, INSTALL).unwrap();
    assert!(receipt.changed.contains(&settings), "{:?}", receipt.changed);
    assert_eq!(
        receipt.backups.len(),
        1,
        "the old settings file is backed up first"
    );

    let after = json_at(&settings);
    assert_eq!(after["model"], "claude-haiku-4-5");
    let stop = after["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1, "the stranger's Stop group stays, ours goes");
    assert_eq!(stop[0]["hooks"][0]["command"], "/usr/local/bin/notify");
    for ours in [
        "SessionStart",
        "PermissionRequest",
        "PreToolUse",
        "UserPromptSubmit",
        "SessionEnd",
    ] {
        assert!(
            after["hooks"].get(ours).is_none(),
            "{ours} survived the migration"
        );
    }
    assert_eq!(
        after["allowedHttpHookUrls"],
        serde_json::json!(["https://audit.example/*"])
    );
    assert_eq!(after["statusLine"]["command"], "npx ccstatusline@latest");
    assert!(after.get("_openwalkietalkie").is_none());
    assert!(!serde_json::to_string(&after)
        .unwrap()
        .contains("dk_legacy_secret"));
    assert_eq!(
        integrations::status(&env, Some(Target::Claude))[0].status,
        Status::Installed
    );

    // The second run has nothing left to migrate and writes nothing.
    let again = integrations::run(&env, Target::Claude, INSTALL).unwrap();
    assert!(again.changed.is_empty(), "{:?}", again.changed);
}

// ---------------------------------------------------------------------------
// §6 row 14 (C12)
// ---------------------------------------------------------------------------

/// The documented `hooks.json` schema, with `deny_unknown_fields` so a field
/// Codex does not know is a test failure rather than a silent drop.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HooksFile {
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default)]
    hooks: BTreeMap<String, Vec<HookGroup>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookGroup {
    #[serde(default)]
    #[allow(dead_code)]
    matcher: Option<String>,
    hooks: Vec<HookHandler>,
}

/// `HookHandlerConfig::Command` carries exactly six fields besides the tag,
/// and `command` is a **string**. An array there makes codex-cli 0.153.4
/// discard every hook in the file, reported only as a `warnings` string.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum HookHandler {
    Command {
        command: String,
        #[serde(default, rename = "commandWindows")]
        #[allow(dead_code)]
        command_windows: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        #[allow(dead_code)]
        r#async: Option<bool>,
        #[serde(default, rename = "statusMessage")]
        #[allow(dead_code)]
        status_message: Option<String>,
        #[serde(default, rename = "additionalContextLimit")]
        #[allow(dead_code)]
        additional_context_limit: Option<u64>,
    },
}

#[test]
fn codex_command_is_a_shell_quoted_string_and_the_file_still_parses() {
    let dir = TestDir::new("x-codex-command");
    // A path with a space and an apostrophe: the case `shell_word` exists for.
    let env = {
        let mut env = test_env(&dir);
        let awkward = dir.path().join("Tony's deck/bin");
        std::fs::create_dir_all(&awkward).unwrap();
        let exe = awkward.join("deckd");
        std::fs::copy(&env.exe, &exe).unwrap();
        env.exe = exe;
        env
    };
    integrations::run(&env, Target::Codex, INSTALL).unwrap();
    let path = integrations::codex::hooks_path(&env);
    let bytes = read(&path);

    // 1. It parses against the documented schema, with `command` a string.
    let file: HooksFile = serde_json::from_slice(&bytes).expect("hooks.json parses");
    assert_eq!(file.hooks.len(), 7, "{:?}", file.hooks.keys());
    let mut seen = 0;
    for (event, groups) in &file.hooks {
        for group in groups {
            for handler in &group.hooks {
                let HookHandler::Command {
                    command, timeout, ..
                } = handler;
                seen += 1;
                // 2. It is one shell word plus `hook codex`, and it decodes
                //    back to the exact path — apostrophe, space and all.
                let (binary, rest) =
                    deckd::integrations::markers::split_command(command).expect("shell-quoted");
                assert_eq!(Path::new(&binary), env.exe, "{event}");
                assert_eq!(rest, "hook codex", "{event}");
                assert!(command.contains('\''), "an awkward path must be quoted");
                assert!(timeout.is_some(), "{event} has no timeout");
            }
        }
    }
    assert_eq!(seen, 7);

    // 3. The raw JSON really holds strings: the array form is the landmine.
    let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for groups in raw["hooks"].as_object().unwrap().values() {
        for group in groups.as_array().unwrap() {
            for handler in group["hooks"].as_array().unwrap() {
                assert!(
                    handler["command"].is_string(),
                    "an array-form command discards the WHOLE file (C12)"
                );
            }
        }
    }

    // 4. And the array form really is rejected by the schema, so the check
    //    above is not vacuous.
    let mut poisoned = raw.clone();
    poisoned["hooks"]["Stop"][0]["hooks"][0]["command"] =
        serde_json::json!([env.exe.display().to_string(), "hook", "codex"]);
    assert!(
        serde_json::from_value::<HooksFile>(poisoned).is_err(),
        "the schema must reject the array form"
    );

    // 5. If `codex` is here, ask it: only `hooks/list` shows the whole-file
    //    discard, and it shows up as a warning with `hooks: []`.
    match integrations::codex::hooks_list(&env) {
        Ok(rows) => {
            assert!(!rows.is_empty(), "codex reported no hooks at all");
            assert!(
                rows.iter().any(|row| row.trust_status == "untrusted"),
                "a freshly written hook is untrusted until reviewed: {rows:?}"
            );
            println!("live codex hooks/list: {} hook(s), no warnings", rows.len());
        }
        Err(why) => println!("skipping the live codex check: {why}"),
    }
}

// ---------------------------------------------------------------------------
// §6 row 15
// ---------------------------------------------------------------------------

/// Uninstall gives the files back exactly as they were — not merely
/// equivalently. `hooks.json` is re-serialised whole by `serde_json`, so the
/// un-merged value is compared against the pre-install backup and the backup's
/// own bytes are restored when they agree. `settings.json` and `config.toml`
/// are never written by an install any more, so they are trivially intact.
#[test]
fn uninstall_restores_the_files_byte_for_byte() {
    let dir = TestDir::new("x-uninstall");
    let env = test_env(&dir);
    // Deliberately not deckd's own formatting: four-space indent, the keys in
    // a user's order, a trailing comment-free newline.
    let settings_before = b"{\n    \"statusLine\": {\n        \"type\": \"command\",\n        \"command\": \"npx ccstatusline@latest\"\n    },\n    \"model\": \"claude-haiku-4-5\",\n    \"permissions\": {\n        \"ask\": [\n            \"Bash(date:*)\"\n        ]\n    }\n}\n".to_vec();
    let hooks_before =
        b"{\n    \"description\": \"mine\",\n    \"hooks\": {\n        \"Stop\": [\n            {\n                \"hooks\": [\n                    {\n                        \"type\": \"command\",\n                        \"command\": \"/opt/mine.sh\",\n                        \"timeout\": 2\n                    }\n                ]\n            }\n        ]\n    }\n}\n"
            .to_vec();
    let config_before = b"# tony's codex\nmodel = \"gpt-6\"\n\n[features]\njs_repl = false\n\n[tui.model_availability_nux]\nseen = true\n".to_vec();
    let files = [
        (claude_settings(&env), settings_before),
        (integrations::codex::hooks_path(&env), hooks_before),
        (integrations::codex::config_path(&env), config_before),
    ];
    for (path, bytes) in &files {
        std::fs::write(path, bytes).unwrap();
    }

    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL).unwrap();
    }
    let (hooks, hooks_before) = &files[1];
    assert_ne!(&read(hooks), hooks_before, "hooks.json was not merged into");
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
    for target in [Target::Codex, Target::Claude] {
        integrations::run(&env, target, Verb::Uninstall).unwrap();
    }
    for (path, before) in &files {
        assert_eq!(
            String::from_utf8_lossy(&read(path)),
            String::from_utf8_lossy(before),
            "{} did not come back byte for byte",
            path.display()
        );
    }

    // What deckd created from nothing is removed again rather than left as
    // an empty husk — its plugin directory, and a hooks.json it invented.
    let dir = TestDir::new("x-uninstall-fresh");
    let env = test_env(&dir);
    integrations::run(&env, Target::Claude, INSTALL).unwrap();
    integrations::run(&env, Target::Codex, INSTALL).unwrap();
    assert!(integrations::claude::hooks_json_path(&env).exists());
    assert!(
        !claude_settings(&env).exists(),
        "the plugin needs no settings.json"
    );
    integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
    integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
    assert!(!integrations::claude::hooks_json_path(&env).exists());
    assert!(!integrations::claude::marketplace_json_path(&env).exists());
    assert!(!integrations::codex::hooks_path(&env).exists());
    for target in [Target::Claude, Target::Codex] {
        assert_eq!(
            integrations::status(&env, Some(target))[0].status,
            Status::Absent
        );
    }
}

/// Row 15's other half, and the shape the gate's `adapters` step starts from:
/// a `hooks.json` that is already there but **empty** — `{"hooks": {}}`, the
/// file `codex` itself leaves behind — and one that carries a `description`
/// of the user's own.
///
/// Install has to invent a `description` for the first and a `hooks` map for
/// the second, and uninstall may take back only the key this install
/// invented. Before the receipt recorded which, uninstall removed an empty
/// `hooks` map the user had written and left deckd's `description` sitting in
/// a file that never had one: JSON-equivalent to nothing that was backed up,
/// so `bytes_of_equivalent_backup` could not give the original bytes back and
/// the file came back rewritten.
///
/// The re-install is part of the test, not decoration: the second run sees
/// the file the first one wrote, so it computes "the description was already
/// there" — and unless the receipt's flags survive that, uninstall leaves the
/// description behind.
#[test]
fn uninstall_gives_back_the_keys_install_invented_and_keeps_the_users() {
    // The empty map `codex` writes on its own: no `description`, a `hooks`
    // key the installer must not take away.
    let empty_map = b"{\n  \"hooks\": {}\n}\n".to_vec();
    // The mirror image: a `description` of the user's, no `hooks` key at all.
    let described = b"{\n  \"description\": \"tony's hooks\"\n}\n".to_vec();
    for (name, before, created_description, created_hooks) in [
        ("x-empty-map", empty_map, true, false),
        ("x-described", described, false, true),
    ] {
        let dir = TestDir::new(name);
        let env = test_env(&dir);
        let hooks = integrations::codex::hooks_path(&env);
        std::fs::write(&hooks, &before).unwrap();

        integrations::run(&env, Target::Codex, INSTALL).unwrap();
        assert_ne!(read(&hooks), before, "{name}: install changed nothing");
        let receipt = json_at(&env.receipts_path())["codex"].clone();
        assert_eq!(
            receipt["created_description"].as_bool(),
            Some(created_description),
            "{name}: created_description"
        );
        assert_eq!(
            receipt["created_hooks_key"].as_bool(),
            Some(created_hooks),
            "{name}: created_hooks_key"
        );
        // The file deckd wrote is not one it created, so the flags have to
        // come back off the receipt rather than be recomputed here.
        assert_eq!(receipt["created_hooks_file"].as_bool(), Some(false));

        // A second run reads the file the first one wrote. What install
        // invented stays install's.
        integrations::run(&env, Target::Codex, INSTALL).unwrap();
        let receipt = json_at(&env.receipts_path())["codex"].clone();
        assert_eq!(
            receipt["created_description"].as_bool(),
            Some(created_description),
            "{name}: the re-install re-read its own description"
        );
        assert_eq!(
            receipt["created_hooks_key"].as_bool(),
            Some(created_hooks),
            "{name}: the re-install re-read its own hooks map"
        );

        integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&read(&hooks)),
            String::from_utf8_lossy(&before),
            "{name}: hooks.json did not come back byte for byte"
        );
        assert!(
            hooks.exists(),
            "{name}: deckd did not create hooks.json, so it does not remove it"
        );
    }
}

/// `config.toml` is the user's. An install writes nothing into it — not
/// `notify`, not `tui.terminal_title`, not `features.hooks` — and a machine
/// where an older deckd set the first two gets them taken back out (the
/// `notify` by the binary it names, the title only on the receipt's word).
#[test]
fn codex_install_leaves_config_toml_alone_and_takes_back_what_an_older_install_wrote() {
    // (a) A user's own notify and title: never touched, never a conflict.
    let dir = TestDir::new("x-config-theirs");
    let env = test_env(&dir);
    let config = integrations::codex::config_path(&env);
    let theirs = "notify = [\"/usr/local/bin/ping-me\"]\n\n[tui]\nterminal_title = [\"activity\", \"project-name\", \"thread-id\"]\n";
    std::fs::write(&config, theirs).unwrap();
    let receipt = integrations::run(&env, Target::Codex, INSTALL).unwrap();
    assert!(receipt.conflicts.is_empty(), "{:?}", receipt.conflicts);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), theirs);
    assert!(!receipt.changed.contains(&config));
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert!(
        !report.limits.iter().any(|l| l.contains("notify")),
        "{:?}",
        report.limits
    );

    // (b) No config.toml at all: none is invented.
    let dir = TestDir::new("x-config-none");
    let env = test_env(&dir);
    integrations::run(&env, Target::Codex, INSTALL).unwrap();
    assert!(!integrations::codex::config_path(&env).exists());

    // (c) An older deckd's keys, with the receipt it left: both come out,
    //     the user's other keys and comments stay, and uninstall has
    //     nothing of ours left to remove.
    let dir = TestDir::new("x-config-ours");
    let env = test_env(&dir);
    let config = integrations::codex::config_path(&env);
    let exe = deckd::integrations::markers::shell_word(&env.exe.display().to_string());
    let old = format!(
        "# tony's codex\nmodel = \"gpt-6\"\nnotify = [\"{}\", \"hook\", \"codex-notify\"]\n\n[features]\njs_repl = false\n\n[tui]\nterminal_title = [\"activity\", \"project-name\", \"thread-id\"]\n",
        env.exe.display()
    );
    let _ = exe;
    std::fs::write(&config, &old).unwrap();
    std::fs::create_dir_all(&env.data).unwrap();
    std::fs::write(
        env.receipts_path(),
        serde_json::to_vec_pretty(&serde_json::json!({"codex": {
            "schema": 1, "deckd": env.exe.display().to_string(),
            "wrote_terminal_title": true, "created_tui_table": true, "wrote_notify": true,
        }}))
        .unwrap(),
    )
    .unwrap();
    let receipt = integrations::run(&env, Target::Codex, INSTALL).unwrap();
    assert!(receipt.changed.contains(&config), "{:?}", receipt.changed);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("notify"), "{text}");
    assert!(
        !text.contains("terminal_title") && !text.contains("[tui]"),
        "{text}"
    );
    assert!(
        text.contains("# tony's codex") && text.contains("js_repl = false"),
        "{text}"
    );
    let recorded = json_at(&env.receipts_path())["codex"].clone();
    assert_eq!(recorded["wrote_notify"], false);
    assert_eq!(recorded["wrote_terminal_title"], false);
    integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        text,
        "nothing of ours was left to remove"
    );
}

// ---------------------------------------------------------------------------
// §6 row 16 (C14)
// ---------------------------------------------------------------------------

/// The realpath-keyed `[hooks.state]` sub-table the `/hooks` review writes:
/// one `trusted_hash` key per hook, no `enabled`.
fn write_trust_table(config: &Path, base: &str, entries: &[(String, String)]) {
    let mut text = base.to_string();
    text.push_str("\n[hooks.state]\n");
    for (key, hash) in entries {
        text.push_str(&format!(
            "\n[hooks.state.\"{key}\"]\ntrusted_hash = \"{hash}\"\n"
        ));
    }
    toml::from_str::<toml::Table>(&text).expect("the hand-written trust table is valid TOML");
    std::fs::write(config, &text).unwrap();
}

/// A freshly written Codex hook does not run. It reports `trustStatus:
/// "untrusted"` and is skipped in silence until the user reviews it in
/// `/hooks` or its hash is recorded in `[hooks.state]`, so `untrusted` is a
/// first-class status and not a footnote.
///
/// And a recorded hash is not itself trust. deckd cannot compute a hook's
/// hash (C14a(iii)), so the presence of the table says nothing about whether
/// the hash still matches the command: only `hooks/list` does. The live half
/// of this test records an arbitrary hash — which is exactly the shape of a
/// stale one left behind by a deckd upgrade — and asserts codex's own verdict
/// reaches the status row, rather than that a table is enough.
#[test]
fn status_reports_untrusted_until_the_hash_is_recorded() {
    let dir = TestDir::new("x-untrusted");
    let mut env = test_env(&dir);
    integrations::run(&env, Target::Codex, INSTALL).unwrap();

    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::Untrusted);
    assert!(
        report.detail.as_ref().unwrap().contains("/hooks"),
        "{:?}",
        report.detail
    );
    assert!(
        report.limits.iter().any(|l| l.contains("first turn")),
        "C18's limit must be stated: {:?}",
        report.limits
    );
    // An install writes no config.toml; the trust table the review writes
    // is appended to whatever the user has, here an empty file.
    let config = integrations::codex::config_path(&env);
    if !config.exists() {
        std::fs::write(&config, "").unwrap();
    }
    let base = std::fs::read_to_string(&config).unwrap();
    assert!(
        !base.contains("hooks.state"),
        "the default install never writes trust"
    );

    // Record a hash the way `/hooks` does, but an arbitrary one.
    let hooks_file = integrations::codex::hooks_path(&env);
    let events = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "Stop",
        "Interrupt",
        "SessionEnd",
    ];
    let made_up: Vec<(String, String)> = events
        .iter()
        .map(|event| {
            (
                integrations::codex::trust_key(&hooks_file, event, 0, 0),
                format!("sha256:{:064x}", 0xdecdu32),
            )
        })
        .collect();
    write_trust_table(&config, &base, &made_up);

    // Without codex (`probe_clis: false`), a table is all deckd can read —
    // and the row says so in `limits` rather than passing the guess off as a
    // check.
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::Installed, "{:?}", report.detail);
    assert!(
        report
            .limits
            .iter()
            .any(|l| l.contains("fell back") && l.contains("[hooks.state]")),
        "the file-only fallback must be stated: {:?}",
        report.limits
    );

    // Take one hash away again: some of our entries trusted, some not, is
    // `modified`. It is NOT what a deckd upgrade that moved the binary leaves
    // behind — see below.
    write_trust_table(&config, &base, &made_up[..made_up.len() - 1]);
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Modified
    );

    // The moved binary, with every hash back in place: `path_drift` is
    // decided before trust is even looked at, so that — not `modified` — is
    // the status a user sees after a deckd upgrade that moved the binary.
    // This is the sentence `host/README.md` prints; the two must not drift
    // apart again.
    write_trust_table(&config, &base, &made_up);
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Installed,
        "the trust table alone must put it back to installed"
    );
    let intact = std::fs::read(&hooks_file).unwrap();
    let mut file = json_at(&hooks_file);
    file["hooks"]["Stop"][0]["hooks"][0]["command"] =
        "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd hook codex".into();
    std::fs::write(&hooks_file, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::PathDrift, "{:?}", report.detail);

    // And the limit the docs lean on: with no `codex app-server` in the loop
    // trust comes out of `[hooks.state]` alone, so a `trusted_hash` that went
    // stale in place is invisible here and only `hooks/list` can see it.
    assert!(
        report
            .limits
            .iter()
            .any(|l| l.contains("[hooks.state]") && l.contains("still reads as `installed`")),
        "status must own up to what it cannot see without the app-server: {:?}",
        report.limits
    );
    // Put the moved binary back: the live half below is about trust, and
    // `path_drift` is decided ahead of it.
    std::fs::write(&hooks_file, &intact).unwrap();

    // ---- the live half: what codex says it will actually run (C14a, C18).
    if integrations::which("codex").is_none() {
        println!("skipping the live half: `codex` is not on PATH");
        return;
    }
    env.probe_clis = true;
    write_trust_table(&config, &base, &made_up);
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_ne!(
        report.status,
        Status::Installed,
        "a made-up hash is not trust: codex skips the hook and the row must not say `installed` \
         ({:?})",
        report.detail
    );
    assert_eq!(report.status, Status::Modified, "{:?}", report.detail);
    assert!(
        !report.limits.iter().any(|l| l.contains("fell back")),
        "codex answered, so nothing fell back: {:?}",
        report.limits
    );

    // Now record what codex itself reports, which is the only hash that can
    // ever match, and the row turns green.
    let live: Vec<(String, String)> = integrations::codex::hooks_list(&env)
        .expect("hooks/list")
        .into_iter()
        .map(|hook| (hook.key, hook.current_hash))
        .collect();
    assert_eq!(live.len(), events.len(), "{live:?}");
    // `hooks/list` reports the events in its own order, so the keys are
    // matched by name: every key deckd computed is one codex reports, and no
    // hash deckd made up was ever the right one (C14a(i), C14a(iii)).
    let by_key: BTreeMap<&str, &str> = live
        .iter()
        .map(|(key, hash)| (key.as_str(), hash.as_str()))
        .collect();
    for (key, made_up_hash) in &made_up {
        let live_hash = by_key
            .get(key.as_str())
            .unwrap_or_else(|| panic!("codex reports no hook for {key}: {live:?}"));
        assert_ne!(
            made_up_hash, live_hash,
            "the made-up hash was never the right one"
        );
    }
    write_trust_table(&config, &base, &live);
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::Installed, "{:?}", report.detail);

    // And one stale hash among six good ones is still an outage.
    let mut stale = live.clone();
    stale[4].1 = format!("sha256:{:064x}", 0xdecdu32);
    write_trust_table(&config, &base, &stale);
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::Modified, "{:?}", report.detail);
    assert!(
        report.detail.as_ref().unwrap().contains("modified"),
        "{:?}",
        report.detail
    );
}

/// C12 from the status side. One array-form `command` in a *user's* hook
/// group makes codex discard the whole of `hooks.json` — deckd's groups with
/// it — and the only trace is a `warnings` entry in `hooks/list`. A status
/// row that reads the file alone sees seven healthy deckd hooks and says
/// `installed`; the live check says `hooks_not_firing`, which is what C18
/// requires.
#[test]
fn a_discarded_hooks_file_is_reported_not_firing() {
    if integrations::which("codex").is_none() {
        println!("skipping: `codex` is not on PATH");
        return;
    }
    let dir = TestDir::new("x-discarded");
    let mut env = test_env(&dir);
    integrations::run(&env, Target::Codex, INSTALL).unwrap();
    let hooks_file = integrations::codex::hooks_path(&env);
    let mut file = json_at(&hooks_file);
    // The user's own hook, in the shape C12 measured as fatal.
    file["hooks"]["Stop"].as_array_mut().unwrap().insert(
        0,
        serde_json::json!({
            "hooks": [{"type": "command", "command": ["/bin/echo", "mine"]}],
        }),
    );
    std::fs::write(&hooks_file, serde_json::to_vec_pretty(&file).unwrap()).unwrap();

    // deckd's own hooks are still in the file, so the file-only read is happy.
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Untrusted,
        "the file still names deckd's hooks"
    );
    env.probe_clis = true;
    let report = &integrations::status(&env, Some(Target::Codex))[0];
    assert_eq!(report.status, Status::HooksNotFiring, "{:?}", report.detail);
    assert!(
        report.detail.as_ref().unwrap().contains("discard"),
        "{:?}",
        report.detail
    );
}

// ---------------------------------------------------------------------------
// The two tests that ask the installed CLIs
// ---------------------------------------------------------------------------

/// The plugin, registered with the real `claude` in a throwaway
/// `CLAUDE_CONFIG_DIR`: Claude lists it enabled at user scope, reads it in
/// place from deckd's marketplace directory, sees its hooks, and `status`
/// reads the same records back. Skipped with a printed reason when `claude`
/// is not on `PATH`.
#[test]
fn the_plugin_registers_with_a_real_claude_in_a_throwaway_config_dir() {
    let Some(claude) = integrations::which("claude") else {
        println!("skipping: `claude` is not on PATH");
        return;
    };
    let dir = TestDir::new("x-real-plugin");
    let mut env = test_env(&dir);
    env.run_clis = true;
    let config_dir = env.claude_dir();
    // A realistic settings file: the install must leave every key of it alone.
    let mine = serde_json::json!({
        "model": "claude-haiku-4-5",
        "permissions": {"ask": ["Bash(date:*)"]},
        "statusLine": {"type": "command", "command": "npx ccstatusline@latest"},
    });
    std::fs::write(
        claude_settings(&env),
        serde_json::to_vec_pretty(&mine).unwrap(),
    )
    .unwrap();

    let receipt = match integrations::run(&env, Target::Claude, INSTALL) {
        Ok(receipt) => receipt,
        Err(why) if why.contains("did not finish") => {
            println!("skipping: the claude CLI did not answer in time ({why})");
            return;
        }
        Err(why) => panic!("{why}"),
    };
    assert!(
        receipt.notes.iter().any(|n| n.starts_with("claude: ")),
        "the CLI's own answers are reported: {:?}",
        receipt.notes
    );
    let listed = run_briefly(
        std::process::Command::new(&claude)
            .args(["plugin", "list", "--json"])
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .current_dir(dir.path()),
    )
    .expect("claude plugin list");
    let rows: serde_json::Value = serde_json::from_str(listed.trim()).expect("a JSON array");
    let ours = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == integrations::claude::PLUGIN_ID)
        .unwrap_or_else(|| panic!("plugin not listed: {listed}"));
    assert_eq!(ours["scope"], "user");
    assert_eq!(ours["enabled"], true);
    assert_eq!(
        std::fs::canonicalize(ours["readFromFolder"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(integrations::claude::plugin_dir(&env)).unwrap(),
        "loaded in place from deckd's marketplace"
    );
    if let Some(details) = run_briefly(
        std::process::Command::new(&claude)
            .args(["plugin", "details", integrations::claude::PLUGIN_ID])
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .current_dir(dir.path()),
    ) {
        assert!(
            details.contains("Hooks (10)") && details.contains("SubagentStart") && details.contains("SubagentStop"),
            "claude does not see the hooks:\n{details}"
        );
    }
    // Claude wrote its own records; deckd put nothing into the user's keys.
    let settings = json_at(&claude_settings(&env));
    assert_eq!(settings["model"], mine["model"]);
    assert_eq!(settings["statusLine"], mine["statusLine"]);
    assert!(
        settings.get("hooks").is_none(),
        "no hook of ours in settings.json"
    );
    assert_eq!(
        settings["enabledPlugins"][integrations::claude::PLUGIN_ID],
        true
    );
    assert_eq!(
        integrations::status(&env, Some(Target::Claude))[0].status,
        Status::Installed
    );
    println!("live claude: plugin registered and listed with its hooks");

    // And the mirror image.
    integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
    let listed = run_briefly(
        std::process::Command::new(&claude)
            .args(["plugin", "list", "--json"])
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .current_dir(dir.path()),
    )
    .expect("claude plugin list");
    assert!(
        !listed.contains(integrations::claude::PLUGIN_ID),
        "{listed}"
    );
    assert_eq!(
        integrations::status(&env, Some(Target::Claude))[0].status,
        Status::Absent
    );
    let settings = json_at(&claude_settings(&env));
    assert_eq!(settings["model"], mine["model"]);
}

/// `--trust` is the explicit opt-in, and it only works *after* the new
/// `hooks.json` is on disk: the hash covers the file Codex can see, so asking
/// `hooks/list` while the old file was still there (or before a first install
/// created one) records a hash that never matches and the hooks stay dead.
///
/// Needs the real `codex`, because deckd cannot derive the hash (C14a) —
/// skipped with a printed reason otherwise.
#[test]
fn codex_trust_is_recorded_only_with_the_flag_and_only_after_the_file_lands() {
    if integrations::which("codex").is_none() {
        println!("skipping: `codex` is not on PATH");
        return;
    }
    let dir = TestDir::new("x-trust");
    let env = test_env(&dir);
    let config = integrations::codex::config_path(&env);
    std::fs::write(&config, "# tony\nmodel = \"gpt-6\"\n").unwrap();

    // Without the flag: installed, untrusted, no `[hooks.state]` at all.
    integrations::run(&env, Target::Codex, INSTALL).unwrap();
    assert!(!std::fs::read_to_string(&config)
        .unwrap()
        .contains("hooks.state"));
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Untrusted
    );

    // With it: the hashes Codex itself reported, keyed on the realpath.
    let receipt = integrations::run(&env, Target::Codex, Verb::Repair { trust: true }).unwrap();
    assert!(
        receipt.next_steps.iter().all(|s| !s.contains("WARNING")),
        "--trust failed: {:?}",
        receipt.next_steps
    );
    let text = std::fs::read_to_string(&config).unwrap();
    toml::from_str::<toml::Table>(&text).expect("config.toml is still valid TOML");
    let real = std::fs::canonicalize(integrations::codex::hooks_path(&env)).unwrap();
    assert!(
        text.contains(&format!(
            "[hooks.state.\"{}:session_start:0:0\"]",
            real.display()
        )),
        "the trust key must use the realpath of hooks.json:\n{text}"
    );
    assert!(
        !text.contains("enabled ="),
        "the review writes only the hash"
    );
    assert_eq!(
        integrations::status(&env, Some(Target::Codex))[0].status,
        Status::Installed
    );

    // And Codex agrees: every one of deckd's hooks now runs.
    let rows = integrations::codex::hooks_list(&env).expect("hooks/list");
    assert_eq!(rows.len(), 7, "{rows:?}");
    for row in &rows {
        assert_eq!(row.trust_status, "trusted", "{row:?}");
        assert!(row.enabled, "{row:?}");
    }
    println!("live codex hooks/list: 7/7 trusted");

    // Uninstall takes the trust table back out with everything else.
    integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "# tony\nmodel = \"gpt-6\"\n"
    );
}

/// The plugin carries no secret: no bearer, no `dk_` token, nothing an
/// `Authorization` header would hold — in the files, the notes or a status
/// report. (The http-hook shape embedded one; see `claude_legacy`.)
#[test]
fn nothing_secret_is_written_for_claude() {
    let dir = TestDir::new("x-secrets");
    let env = test_env(&dir);
    let receipt = integrations::run(&env, Target::Claude, INSTALL).unwrap();
    let mut spoken = format!(
        "{}\n{}",
        serde_json::to_string(&receipt).unwrap(),
        serde_json::to_string(&integrations::status(&env, None)).unwrap()
    );
    for path in receipt.changed.iter() {
        spoken.push_str(&String::from_utf8_lossy(&read(path)));
    }
    for forbidden in ["Bearer", "dk_", "Authorization", "allowedHttpHookUrls"] {
        assert!(
            !spoken.contains(forbidden),
            "{forbidden} appears in what the install wrote or said"
        );
    }
    assert!(!env.tokens.exists(), "no hooks token is minted any more");
}

/// Every target answers `status` without writing a single byte — the check a
/// GUI polling `integrations.status` depends on.
#[test]
fn status_writes_nothing_for_any_target() {
    let dir = TestDir::new("x-status-readonly");
    let env = test_env(&dir);
    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL).unwrap();
    }
    let before = snapshot_tree(dir.path());
    let reports = integrations::status(&env, None);
    assert_eq!(reports.len(), 5, "one row per target");
    assert_eq!(
        reports
            .iter()
            .map(|r| r.target.name())
            .collect::<Vec<&str>>(),
        ["claude", "codex",
            "codex-desktop", "gemini", "claude-desktop"]
    );
    assert_eq!(snapshot_tree(dir.path()), before, "status wrote something");
}

/// The cross-branch landing hazard. Every hook this installer writes runs
/// `deckd hook <claude|codex|claude-statusline>`, which is B11 and owned by
/// another builder. A build without it does not fail quietly: clap exits **2**
/// with a usage error on stderr, and a Codex `PermissionRequest` reads exit 2
/// plus stderr as a DENY (C21) — so installing from such a build and trusting
/// the hooks denies every permission request on the machine, and Claude's
/// exec-form `SessionStart` hook and the chained `statusLine` fail on every
/// session. The installer therefore asks the very binary the hook files will
/// name before it writes one.
#[test]
fn install_refuses_when_this_binary_has_no_hook_subcommand() {
    let dir = TestDir::new("x-no-hook-subcommand");
    let env = test_env(&dir);
    // Exactly what a deckd without B11 does: clap's "unrecognized subcommand"
    // exit 2. Everything else about the binary is fine.
    std::fs::write(
        &env.exe,
        b"#!/bin/sh\ncase \"$1\" in hook) exit 2;; esac\nexit 0\n",
    )
    .unwrap();
    let empty = snapshot_tree(dir.path());

    for target in [Target::Claude, Target::Codex] {
        for verb in [INSTALL, Verb::Repair { trust: false }] {
            // The refusal lands on `plan` too, so `--dry-run` refuses rather
            // than printing a "What this writes" that must not be applied.
            let error = integrations::plan(&env, target, verb).unwrap_err();
            assert!(
                error.contains("hook --help") && error.contains("exit 2"),
                "{}/{verb:?}: {error}",
                target.name()
            );
            let error = integrations::run(&env, target, verb).unwrap_err();
            assert!(error.contains("hook --help"), "{}: {error}", target.name());
        }
    }
    assert_eq!(
        snapshot_tree(dir.path()),
        empty,
        "a refused install still wrote something"
    );

    // Uninstall and status never invoke the helper, so neither may be blocked
    // by a binary that cannot run it — an operator has to be able to take a
    // bad install back off with the build they have.
    integrations::run(&env, Target::Claude, Verb::Uninstall).expect("uninstall");
    integrations::run(&env, Target::Codex, Verb::Uninstall).expect("uninstall");
    assert_eq!(integrations::status(&env, None).len(), 5);

    // And with the subcommand answering, the same install goes through.
    std::fs::write(&env.exe, b"#!/bin/sh\nexit 0\n").unwrap();
    for target in [Target::Claude, Target::Codex] {
        integrations::run(&env, target, INSTALL)
            .unwrap_or_else(|e| panic!("{}: {e}", target.name()));
    }
}

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

/// Run a command with a hard ceiling, returning its stdout. A CLI that hangs
/// must never hang the gate.
fn run_briefly(command: &mut std::process::Command) -> Option<String> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let text = rx.recv_timeout(std::time::Duration::from_secs(90)).ok();
    let _ = child.kill();
    let _ = child.wait();
    text
}
