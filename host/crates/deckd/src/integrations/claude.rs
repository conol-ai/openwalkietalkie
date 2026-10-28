//! The Claude Code target: a **plugin**, not an edit to the user's settings.
//!
//! deckd writes a one-plugin marketplace under its own data directory —
//!
//! ```text
//! <data>/claude-marketplace/
//!   .claude-plugin/marketplace.json
//!   plugins/openwalkietalkie/.claude-plugin/plugin.json
//!   plugins/openwalkietalkie/hooks/hooks.json
//!   plugins/openwalkietalkie/.mcp.json
//! ```
//!
//! — and asks Claude Code itself to register it: `claude plugin marketplace
//! add <dir>` then `claude plugin install openwalkietalkie@openwalkietalkie
//! --scope user`. Claude records the marketplace and the `enabledPlugins`
//! flag in its own files; deckd never opens `settings.json` to put a hook
//! in it again. A local-path marketplace loads **in place**, so a daemon
//! upgrade rewrites the plugin directory and the next session start (or
//! `/reload-plugins`) picks the new hooks up; both CLI commands are
//! idempotent (measured against `claude` 2.1.296: a second `add` and a
//! second `install` answer `ok` and change nothing).
//!
//! What this buys over the pre-plugin shape ([`super::claude_legacy`]):
//!
//! - **Every hook is a `command` hook** that runs `deckd hook claude`, so
//!   every event carries the helper's environment — the terminal identity of
//!   §5.5 and the session's message inbox (`crate::inbox`) are learned on
//!   *any* event, not only `SessionStart`. No `type:"http"` hooks, so no
//!   bearer in a settings file and no `allowedHttpHookUrls` switch.
//! - **No key of the user's is touched.** The fatal-settings predicate
//!   (C24a), the ownership-by-fingerprint rules and the 800-line staged
//!   backup argument all existed to merge into a file we did not own. The
//!   plugin's files are ours alone.
//! - **One mechanism for the terminal and the Desktop app's Code tab**: both
//!   read the same user-scope plugin.
//!
//! The migration is part of `install`: a `settings.json` still carrying the
//! old entries has them taken back out in the same run
//! ([`super::claude_legacy::plan_removal`]), under the old rules.
//!
//! `status` reads Claude's own records (`plugins/installed_plugins.json`,
//! `plugins/known_marketplaces.json`, `enabledPlugins`) and never runs the
//! CLI; `claude plugin list --json` remains the authority on what a session
//! actually loads, and the row's `limits` say so.
use super::backup::{self, FileSnapshot};
use super::claude_legacy;
use super::{CliCommand, Env, Plan, Report, Status, Target, Verb};
use std::path::{Path, PathBuf};

/// The marketplace deckd declares, the plugin inside it, and the id Claude
/// Code knows the pair by.
pub const MARKETPLACE: &str = "openwalkietalkie";
pub const PLUGIN: &str = "openwalkietalkie";
pub const PLUGIN_ID: &str = "openwalkietalkie@openwalkietalkie";

/// The held events' `timeout`, in seconds (C6: seconds, the binary
/// multiplies by 1000). `hold_s` must stay at least ten seconds below it: a
/// hook that Claude gives up on leaves the terminal dialog live, so the hold
/// must always end first.
pub const HELD_TIMEOUT_S: u64 = 130;
/// Every report-only hook: the helper answers within its 500 ms connect
/// budget or not at all.
pub const REPORT_TIMEOUT_S: u64 = 2;
/// `SessionStart` blocks the session's first prompt; the helper's one
/// round trip is well inside this.
pub const SESSION_START_TIMEOUT_S: u64 = 5;
/// `SessionEnd` shares Claude's own 1.5 s budget.
pub const SESSION_END_TIMEOUT_S: u64 = 1;
/// The receipt's schema version.
pub const RECEIPT_SCHEMA: u64 = 2;

/// One hook the plugin declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Managed {
    event: &'static str,
    matcher: Option<&'static str>,
    timeout: u64,
    /// Installed only with `[hooks.claude] tool_titles = true`.
    optional: bool,
}

const MANAGED: &[Managed] = &[
    Managed {
        event: "SessionStart",
        matcher: None,
        timeout: SESSION_START_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "PermissionRequest",
        matcher: None,
        timeout: HELD_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "PreToolUse",
        matcher: Some("AskUserQuestion"),
        timeout: HELD_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "UserPromptSubmit",
        matcher: None,
        timeout: REPORT_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "Stop",
        matcher: None,
        timeout: REPORT_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "StopFailure",
        matcher: None,
        timeout: REPORT_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "Notification",
        matcher: None,
        timeout: REPORT_TIMEOUT_S,
        optional: false,
    },
    Managed {
        event: "PostToolUse",
        matcher: None,
        timeout: REPORT_TIMEOUT_S,
        optional: true,
    },
    Managed {
        event: "SessionEnd",
        matcher: None,
        timeout: SESSION_END_TIMEOUT_S,
        optional: false,
    },
];

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`. Read
/// for the migration and for `enabledPlugins`; never written to install.
pub fn settings_path(env: &Env) -> PathBuf {
    claude_legacy::settings_path(env)
}

/// The marketplace root: deckd's data directory, so it moves with the
/// daemon's own state and never sits inside `~/.claude`.
pub fn marketplace_dir(env: &Env) -> PathBuf {
    env.data.join("claude-marketplace")
}

pub fn marketplace_json_path(env: &Env) -> PathBuf {
    marketplace_dir(env)
        .join(".claude-plugin")
        .join("marketplace.json")
}

pub fn plugin_dir(env: &Env) -> PathBuf {
    marketplace_dir(env).join("plugins").join(PLUGIN)
}

pub fn plugin_json_path(env: &Env) -> PathBuf {
    plugin_dir(env).join(".claude-plugin").join("plugin.json")
}

pub fn hooks_json_path(env: &Env) -> PathBuf {
    plugin_dir(env).join("hooks").join("hooks.json")
}

pub fn mcp_json_path(env: &Env) -> PathBuf {
    plugin_dir(env).join(".mcp.json")
}

/// The path the long-lived MCP helper is launched from. A copy running from a
/// temporary location (a Sparkle staging directory, `/var/folders`) is gone
/// after the update, and every later Claude session would name a dead path, so
/// prefer the installed bundle's executable when it exists.
fn channel_exe(exe: &Path) -> PathBuf {
    let text = exe.to_string_lossy();
    let transient = ["/var/folders/", "/private/var/", "/Sparkle", "/AppTranslocation/"]
        .iter()
        .any(|marker| text.contains(marker));
    let installed = Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    if transient && installed.is_file() {
        installed.to_path_buf()
    } else {
        exe.to_path_buf()
    }
}

pub fn mcp_json(env: &Env) -> serde_json::Value {
    serde_json::json!({"mcpServers": {"openwalkietalkie": {
        "command": channel_exe(&env.exe),
        "args": ["--data-dir", env.data.to_string_lossy(), "claude-channel"]
    }}})
}

/// The files this target owns, plus the settings file it reads.
pub fn files(env: &Env) -> Vec<PathBuf> {
    vec![
        marketplace_json_path(env),
        plugin_json_path(env),
        hooks_json_path(env),
        mcp_json_path(env),
        settings_path(env),
    ]
}

/// `plugins/installed_plugins.json` and `plugins/known_marketplaces.json`,
/// Claude Code's own records under the config dir.
fn installed_plugins_path(env: &Env) -> PathBuf {
    env.claude_dir()
        .join("plugins")
        .join("installed_plugins.json")
}

fn known_marketplaces_path(env: &Env) -> PathBuf {
    env.claude_dir()
        .join("plugins")
        .join("known_marketplaces.json")
}

/// The events the plugin declares for this configuration, in file order.
fn managed(tool_titles: bool) -> impl Iterator<Item = &'static Managed> {
    MANAGED.iter().filter(move |m| !m.optional || tool_titles)
}

/// `hooks/hooks.json`: one exec-form command hook per event, every one of
/// them `deckd hook claude`. Exec form (`command` + `args`) rather than a
/// shell string: the helper's parent walk then finds `claude` directly,
/// and no shell quoting of the binary's path is needed.
pub fn hooks_json(env: &Env) -> serde_json::Value {
    let mut hooks = serde_json::Map::new();
    for m in managed(env.tool_titles) {
        let handler = serde_json::json!({
            "type": "command",
            "command": env.exe.display().to_string(),
            "args": ["hook", "claude"],
            "timeout": m.timeout,
        });
        let mut group = serde_json::json!({"hooks": [handler]});
        if let Some(matcher) = m.matcher {
            group["matcher"] = matcher.into();
        }
        hooks.insert(m.event.to_string(), serde_json::json!([group]));
    }
    serde_json::json!({"hooks": hooks})
}

/// `.claude-plugin/plugin.json`. The version is deckd's own, so a daemon
/// upgrade reads as a plugin upgrade in `claude plugin list`.
pub fn plugin_json() -> serde_json::Value {
    serde_json::json!({
        "name": PLUGIN,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "OpenWalkieTalkie deck: Claude Code sessions as channels on the deck (managed by `deckd integrations`)",
        "author": {"name": "Cohdl"},
    })
}

/// `.claude-plugin/marketplace.json`: one plugin, sourced by relative path
/// so the marketplace loads in place from wherever the data dir is.
pub fn marketplace_json() -> serde_json::Value {
    serde_json::json!({
        "name": MARKETPLACE,
        "owner": {"name": "Cohdl"},
        "metadata": {
            "description": "The OpenWalkieTalkie deck's Claude Code plugin, written and registered by deckd",
        },
        "plugins": [{
            "name": PLUGIN,
            "source": format!("./plugins/{PLUGIN}"),
            "description": "OpenWalkieTalkie deck hooks",
            "version": env!("CARGO_PKG_VERSION"),
        }],
    })
}

/// The `claude plugin …` invocations an install or uninstall runs.
fn commands(env: &Env, verb: Verb) -> Vec<CliCommand> {
    let dir = marketplace_dir(env).display().to_string();
    if verb.is_uninstall() {
        vec![
            CliCommand {
                program: "claude",
                args: vec![
                    "plugin".into(),
                    "uninstall".into(),
                    PLUGIN_ID.into(),
                    "--scope".into(),
                    "user".into(),
                    "--json".into(),
                ],
                tolerated: vec!["not_installed", "not found"],
                before_writes: true,
            },
            CliCommand {
                program: "claude",
                args: vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "remove".into(),
                    MARKETPLACE.into(),
                ],
                tolerated: vec!["not found"],
                before_writes: true,
            },
        ]
    } else {
        vec![
            CliCommand {
                program: "claude",
                args: vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "add".into(),
                    dir,
                    "--scope".into(),
                    "user".into(),
                    "--json".into(),
                ],
                tolerated: Vec::new(),
                before_writes: false,
            },
            CliCommand {
                program: "claude",
                args: vec![
                    "plugin".into(),
                    "install".into(),
                    PLUGIN_ID.into(),
                    "--scope".into(),
                    "user".into(),
                    "--json".into(),
                ],
                tolerated: Vec::new(),
                before_writes: false,
            },
        ]
    }
}

/// "What this writes" (and runs) for one verb, without doing any of it.
pub fn plan(env: &Env, verb: Verb, receipt: Option<&serde_json::Value>) -> Result<Plan, String> {
    if !verb.is_uninstall() && env.hold_s + 10 > HELD_TIMEOUT_S as u32 {
        return Err(format!(
            "[hooks.claude] hold_s = {} leaves no headroom under the installed timeout of \
             {HELD_TIMEOUT_S} s: Claude would close the connection before the hold ends and the \
             terminal dialog would stay live. Lower it to {} or less.",
            env.hold_s,
            HELD_TIMEOUT_S - 10
        ));
    }
    let mut plan = Plan::default();
    let ours = [
        (marketplace_json_path(env), marketplace_json()),
        (plugin_json_path(env), plugin_json()),
        (hooks_json_path(env), hooks_json(env)),
        (mcp_json_path(env), mcp_json(env)),
    ];
    if verb.is_uninstall() {
        let mut removed = 0usize;
        for (path, _) in &ours {
            let snapshot = FileSnapshot::read(path)?;
            if snapshot.exists() {
                plan.writes.push(backup::Planned::remove(snapshot));
                removed += 1;
            }
        }
        if removed > 0 {
            plan.note(format!(
                "removes the plugin marketplace under {}",
                marketplace_dir(env).display()
            ));
        }
    } else {
        for (path, value) in &ours {
            let snapshot = FileSnapshot::read(path)?;
            plan.writes
                .push(backup::Planned::write(snapshot, super::render_json(value)?));
        }
        let events: Vec<String> = managed(env.tool_titles).map(entry_name).collect();
        plan.note(format!(
            "writes the `{PLUGIN}` plugin under {}: {} command hooks ({}), every one running \
             `{} hook claude`",
            marketplace_dir(env).display(),
            events.len(),
            events.join(", "),
            env.exe.display()
        ));
        plan.note(
            "registers it with Claude Code at user scope (`claude plugin marketplace add`, \
             `claude plugin install`): Claude writes its own `enabledPlugins` entry; deckd puts \
             nothing into settings.json",
        );
        plan.step(
            "a session already open loads the plugin after `/reload-plugins`; new sessions load it \
             at start",
        );
        plan.step("For voice messages through Claude Channels, follow Integrations → Claude voice messages in the app. Channels require per-session opt-in; cross-session inbox delivery remains the fallback.");
        plan.receipt = Some(serde_json::json!({
            "schema": RECEIPT_SCHEMA,
            "deckd": env.exe.display().to_string(),
            "marketplace": marketplace_dir(env).display().to_string(),
            "plugin": PLUGIN_ID,
            "events": events,
        }));
    }
    // The migration, on both verbs: a settings.json that still carries the
    // pre-plugin entries has them taken back out under the old rules.
    if let Some(planned) = claude_legacy::plan_removal(env, legacy_receipt(receipt), &mut plan)? {
        plan.writes.push(planned);
    }
    plan.commands = commands(env, verb);
    Ok(plan)
}

/// The inventory of a pre-plugin install, if that is what the receipt is.
/// A plugin-era receipt (`schema: 2`) carries no inventory and is not one.
fn legacy_receipt(receipt: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    receipt.filter(|r| r.get("entries").is_some() || r.get("allowlist").is_some())
}

fn entry_name(managed: &Managed) -> String {
    match managed.matcher {
        None => managed.event.to_string(),
        Some(matcher) => format!("{}({matcher})", managed.event),
    }
}

/// C24a's second half, kept for the migration write: the settings file is
/// re-read and run through Claude's own predicate after it is written.
pub fn verify_written(env: &Env) -> Result<(), String> {
    claude_legacy::verify_written(env)
}

/// What Claude Code's own records say about the plugin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Registered {
    /// `installed_plugins.json` lists the id at user scope.
    installed: bool,
    /// `settings.json` has `enabledPlugins[id] == true`.
    enabled: bool,
    /// `known_marketplaces.json` names the marketplace at our directory.
    marketplace: bool,
}

fn registered(env: &Env, settings: &serde_json::Value) -> Registered {
    let read = |path: &Path| -> serde_json::Value {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(serde_json::Value::Null)
    };
    let installed = read(&installed_plugins_path(env))["plugins"][PLUGIN_ID]
        .as_array()
        .is_some_and(|entries| entries.iter().any(|e| e["scope"].as_str() == Some("user")));
    let enabled = settings["enabledPlugins"][PLUGIN_ID].as_bool() == Some(true);
    let dir = marketplace_dir(env);
    let known = &read(&known_marketplaces_path(env))[MARKETPLACE];
    let marketplace = [
        known["installLocation"].as_str(),
        known["source"]["path"].as_str(),
    ]
    .into_iter()
    .flatten()
    .any(|recorded| same_dir(Path::new(recorded), &dir));
    Registered {
        installed,
        enabled,
        marketplace,
    }
}

/// Two spellings of one directory (`/private/tmp` vs `/tmp`, a symlinked
/// data dir) are the same marketplace.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

pub fn status(env: &Env) -> Report {
    let settings_path = settings_path(env);
    let mut report = Report::new(Target::Claude, Status::Absent);
    report.files = files(env);
    report.backups = backup::list_backups(&env.backups(), "settings.json");
    report.detected = super::detect_cli(env, "claude");
    report.limits = limits();
    // The settings file: read for `enabledPlugins` and for the migration,
    // and a file Claude cannot read is a conflict whatever else is true.
    let settings = match FileSnapshot::read(&settings_path) {
        Ok(snapshot) if !snapshot.exists() => serde_json::json!({}),
        Ok(snapshot) => match super::parse_json_object(&snapshot) {
            Ok(root) => root,
            Err(why) => {
                report.status = Status::Conflict;
                report.detail = Some(why);
                return report;
            }
        },
        Err(why) => {
            report.status = Status::Conflict;
            report.detail = Some(why);
            return report;
        }
    };
    if let Some(key) = super::markers::fatal_settings_key(&settings) {
        report.status = Status::Conflict;
        report.detail = Some(format!(
            "{} holds the fatal key {key}: Claude ignores the entire file and says nothing",
            settings_path.display()
        ));
        return report;
    }
    let legacy = claude_legacy::present(&settings, env);
    let hooks_path = hooks_json_path(env);
    let on_disk: Option<serde_json::Value> = std::fs::read(&hooks_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let any_file =
        on_disk.is_some() || marketplace_json_path(env).exists() || plugin_json_path(env).exists();
    let registered = registered(env, &settings);
    if !any_file && !legacy && !registered.installed && !registered.enabled {
        report.detail = Some(format!(
            "no `{PLUGIN}` plugin under {}",
            marketplace_dir(env).display()
        ));
        return report;
    }
    if legacy {
        report.status = Status::Outdated;
        report.detail = Some(format!(
            "{} still carries the pre-plugin deckd hooks; `deckd integrations install claude` \
             migrates them to the plugin",
            settings_path.display()
        ));
        return report;
    }
    // `path_drift`: the hooks name a binary that is not this one.
    let named: Vec<String> = on_disk
        .as_ref()
        .and_then(|v| v["hooks"].as_object())
        .map(|events| {
            events
                .values()
                .flat_map(|groups| groups.as_array().into_iter().flatten())
                .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
                .filter_map(|h| h["command"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if named.iter().any(|path| Path::new(path) != env.exe) {
        report.status = Status::PathDrift;
        report.detail = Some(format!(
            "{} names {}, not this binary",
            hooks_path.display(),
            named.first().cloned().unwrap_or_default()
        ));
        return report;
    }
    let desired = [
        (marketplace_json_path(env), marketplace_json()),
        (plugin_json_path(env), plugin_json()),
        (hooks_path.clone(), hooks_json(env)),
        (mcp_json_path(env), mcp_json(env)),
    ];
    let stale: Vec<String> = desired
        .iter()
        .filter(|(path, value)| {
            super::render_json(value).ok().as_deref() != std::fs::read(path).ok().as_deref()
        })
        .map(|(path, _)| path.display().to_string())
        .collect();
    if !stale.is_empty() {
        report.status = Status::Outdated;
        report.detail = Some(format!(
            "{} differ(s) from what this build writes; `deckd integrations repair claude` \
             rewrites them",
            stale.join(", ")
        ));
        return report;
    }
    // Registration is Claude's half. An environment that may not run the
    // CLI (`run_clis` off: tests, a `--no-clis` status) has no way to have
    // registered anything, so the files are the whole story there.
    if env.run_clis {
        let missing: Vec<&str> = [
            (
                !registered.marketplace,
                "the marketplace is not in known_marketplaces.json",
            ),
            (
                !registered.installed,
                "the plugin is not in installed_plugins.json at user scope",
            ),
            (!registered.enabled, "settings.json does not enable it"),
        ]
        .into_iter()
        .filter_map(|(missing, why)| missing.then_some(why))
        .collect();
        if !missing.is_empty() {
            report.status = Status::Outdated;
            report.detail = Some(format!(
                "the plugin files are in place but Claude Code has not registered them ({}); \
                 `deckd integrations install claude` runs the `claude plugin` commands again",
                missing.join("; ")
            ));
            return report;
        }
    }
    report.status = Status::Installed;
    report
}

/// What `integrations.status` cannot tell you about Claude.
fn limits() -> Vec<String> {
    vec![
        "a plugin is read at session start: a session that was already open when it was \
         installed or rewritten loads it after `/reload-plugins`, not before"
            .into(),
        "`installed` means enabled at USER scope; a project's own .claude/settings.json can set \
         `enabledPlugins[\"openwalkietalkie@openwalkietalkie\"]: false` and turn it off for that \
         repository, which this row cannot see"
            .into(),
        "this row reads Claude Code's own records (installed_plugins.json, known_marketplaces.json, \
         enabledPlugins) and never runs the CLI; `claude plugin list --json` is the authority on \
         what a session actually loads"
            .into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::test_env;
    use crate::integrations::testdir::TestDir;
    use crate::integrations::Receipts;

    fn install(env: &Env) -> crate::integrations::Receipt {
        crate::integrations::run(env, Target::Claude, Verb::Install { trust: false })
            .expect("install")
    }

    fn json_at(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn channel_exe_replaces_only_transient_paths() {
        let stable = Path::new("/Users/x/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
        assert_eq!(channel_exe(stable), stable);
        let staged = Path::new("/private/var/folders/x/T/Sparkle/OpenWalkieTalkie.app/Contents/MacOS/deckd");
        let installed =
            Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
        assert_eq!(
            channel_exe(staged),
            if installed.is_file() { installed } else { staged }
        );
    }

    #[test]
    fn channel_bridge_is_managed_without_enabling_channels_for_the_user() {
        let dir = TestDir::new("plugin-channel");
        let env = test_env(&dir);
        install(&env);
        let mcp = json_at(&mcp_json_path(&env));
        assert_eq!(
            mcp["mcpServers"]["openwalkietalkie"]["command"],
            channel_exe(&env.exe).to_string_lossy().as_ref()
        );
        assert_eq!(
            mcp["mcpServers"]["openwalkietalkie"]["args"],
            serde_json::json!(["--data-dir", env.data.to_string_lossy(), "claude-channel"])
        );
        assert!(!settings_path(&env).exists());
        std::fs::remove_file(mcp_json_path(&env)).unwrap();
        assert_eq!(status(&env).status, Status::Outdated);
        crate::integrations::run(&env, Target::Claude, Verb::Repair { trust: false }).unwrap();
        assert_eq!(status(&env).status, Status::Installed);
        crate::integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
        assert!(!mcp_json_path(&env).exists());
    }

    /// Every hook is a command hook running `deckd hook claude` in exec form,
    /// with the measured timeouts (C6: seconds), and no http hook anywhere.
    #[test]
    fn every_hook_is_an_exec_form_command_hook_with_seconds_timeouts() {
        let dir = TestDir::new("plugin-hooks");
        let env = test_env(&dir);
        install(&env);
        let hooks = json_at(&hooks_json_path(&env));
        let events = hooks["hooks"].as_object().unwrap();
        let mut names = events.keys().cloned().collect::<Vec<_>>();
        names.sort(); // serde_json's GUI-enabled preserve_order changes map iteration.
        assert_eq!(
            names,
            [
                "Notification",
                "PermissionRequest",
                "PreToolUse",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "StopFailure",
                "UserPromptSubmit",
            ],
            "PostToolUse is off without tool_titles"
        );
        for (event, groups) in events {
            let group = &groups[0];
            let handler = &group["hooks"][0];
            assert_eq!(handler["type"], "command", "{event}");
            assert_eq!(handler["command"], env.exe.display().to_string(), "{event}");
            assert_eq!(
                handler["args"],
                serde_json::json!(["hook", "claude"]),
                "{event}"
            );
            assert!(handler.get("url").is_none() && handler.get("headers").is_none());
            let expected = match event.as_str() {
                "PermissionRequest" | "PreToolUse" => HELD_TIMEOUT_S,
                "SessionStart" => SESSION_START_TIMEOUT_S,
                "SessionEnd" => SESSION_END_TIMEOUT_S,
                _ => REPORT_TIMEOUT_S,
            };
            assert_eq!(handler["timeout"], expected, "{event}");
            if event == "PreToolUse" {
                assert_eq!(group["matcher"], "AskUserQuestion");
            } else {
                assert!(group.get("matcher").is_none(), "{event}");
            }
        }
        assert!(env.hold_s + 10 <= HELD_TIMEOUT_S as u32);
    }

    #[test]
    fn post_tool_use_is_installed_only_with_tool_titles() {
        let dir = TestDir::new("plugin-titles");
        let mut env = test_env(&dir);
        install(&env);
        assert!(json_at(&hooks_json_path(&env))["hooks"]
            .get("PostToolUse")
            .is_none());
        env.tool_titles = true;
        install(&env);
        assert_eq!(
            json_at(&hooks_json_path(&env))["hooks"]["PostToolUse"][0]["hooks"][0]["timeout"],
            REPORT_TIMEOUT_S
        );
    }

    #[test]
    fn the_marketplace_and_manifest_name_the_plugin_claude_installs() {
        let dir = TestDir::new("plugin-manifest");
        let env = test_env(&dir);
        let receipt = install(&env);
        let marketplace = json_at(&marketplace_json_path(&env));
        assert_eq!(marketplace["name"], MARKETPLACE);
        assert_eq!(marketplace["plugins"][0]["name"], PLUGIN);
        assert_eq!(
            marketplace["plugins"][0]["source"],
            "./plugins/openwalkietalkie"
        );
        let manifest = json_at(&plugin_json_path(&env));
        assert_eq!(manifest["name"], PLUGIN);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert!(plugin_dir(&env).ends_with("plugins/openwalkietalkie"));
        // Nothing of ours in the user's settings, and no settings file
        // invented either.
        assert!(!settings_path(&env).exists());
        // The receipt is the plugin era's, and uninstall drops it.
        let receipts = Receipts::load(&env.receipts_path());
        assert_eq!(receipts.get(Target::Claude).unwrap()["plugin"], PLUGIN_ID);
        assert_eq!(
            receipts.get(Target::Claude).unwrap()["schema"],
            RECEIPT_SCHEMA
        );
        assert!(receipt
            .next_steps
            .iter()
            .any(|s| s.contains("/reload-plugins")));
        crate::integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
        assert!(Receipts::load(&env.receipts_path())
            .get(Target::Claude)
            .is_none());
        assert!(
            !marketplace_dir(&env).join(".claude-plugin").exists()
                || !marketplace_json_path(&env).exists()
        );
    }

    /// The two `claude plugin` commands, in order, and their uninstall
    /// mirror running **before** the files go (Claude reads the manifest
    /// from the in-place folder to uninstall).
    #[test]
    fn the_commands_register_and_unregister_in_the_right_order() {
        let dir = TestDir::new("plugin-commands");
        let env = test_env(&dir);
        let plan = crate::integrations::plan(&env, Target::Claude, Verb::Install { trust: false })
            .unwrap();
        let lines: Vec<String> = plan.commands.iter().map(|c| c.to_string()).collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("claude plugin marketplace add "),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains(&marketplace_dir(&env).display().to_string()));
        assert!(lines[0].ends_with("--scope user --json"));
        assert_eq!(
            lines[1],
            "claude plugin install openwalkietalkie@openwalkietalkie --scope user --json"
        );
        assert!(plan.commands.iter().all(|c| !c.before_writes));
        let plan = crate::integrations::plan(&env, Target::Claude, Verb::Uninstall).unwrap();
        let lines: Vec<String> = plan.commands.iter().map(|c| c.to_string()).collect();
        assert_eq!(
            lines,
            [
                "claude plugin uninstall openwalkietalkie@openwalkietalkie --scope user --json",
                "claude plugin marketplace remove openwalkietalkie",
            ]
        );
        assert!(plan.commands.iter().all(|c| c.before_writes));
    }

    /// A machine that installed the pre-plugin shape has it taken back out
    /// of settings.json in the same `install`, and `status` says so until then.
    #[test]
    fn install_migrates_a_legacy_settings_file_and_leaves_the_users_keys() {
        let dir = TestDir::new("plugin-migrate");
        let env = test_env(&dir);
        let legacy = claude_legacy::tests::legacy_settings(&env);
        std::fs::write(
            settings_path(&env),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let before = status(&env);
        assert_eq!(before.status, Status::Outdated);
        assert!(before.detail.unwrap().contains("pre-plugin"));

        let receipt = install(&env);
        assert!(receipt.changed.contains(&settings_path(&env)));
        let after = json_at(&settings_path(&env));
        assert_eq!(after["model"], "claude-haiku-4-5");
        assert_eq!(after["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/usr/local/bin/notify"
        );
        assert!(after["hooks"].get("PermissionRequest").is_none());
        assert_eq!(
            after["allowedHttpHookUrls"],
            serde_json::json!(["https://audit.example/*"])
        );
        assert_eq!(after["statusLine"]["command"], "npx ccstatusline@latest");
        assert!(after.get("_openwalkietalkie").is_none());
        assert_eq!(status(&env).status, Status::Installed);
        // Idempotent: the second run has nothing left to migrate.
        let again = install(&env);
        assert!(again.changed.is_empty(), "{:?}", again.changed);
    }

    #[test]
    fn status_walks_absent_installed_outdated_and_path_drift() {
        let dir = TestDir::new("plugin-status");
        let mut env = test_env(&dir);
        assert_eq!(status(&env).status, Status::Absent);
        install(&env);
        assert_eq!(status(&env).status, Status::Installed);
        // A different build writes different files: outdated.
        let mut hooks = json_at(&hooks_json_path(&env));
        hooks["hooks"]["Stop"][0]["hooks"][0]["timeout"] = serde_json::json!(9);
        std::fs::write(
            hooks_json_path(&env),
            serde_json::to_vec_pretty(&hooks).unwrap(),
        )
        .unwrap();
        let report = status(&env);
        assert_eq!(report.status, Status::Outdated, "{:?}", report.detail);
        install(&env);
        assert_eq!(status(&env).status, Status::Installed);
        // The binary moved: path drift, and `install` goes through it.
        let moved = dir.path().join("elsewhere/deckd");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::copy(&env.exe, &moved).unwrap();
        env.exe = moved;
        assert_eq!(status(&env).status, Status::PathDrift);
        install(&env);
        assert_eq!(status(&env).status, Status::Installed);
        // Uninstall: absent again, nothing of ours left on disk.
        crate::integrations::run(&env, Target::Claude, Verb::Uninstall).unwrap();
        assert_eq!(status(&env).status, Status::Absent);
        assert!(!hooks_json_path(&env).exists());
    }

    /// With the CLI allowed, `status` also wants Claude's own records.
    #[test]
    fn status_wants_claudes_registration_when_the_cli_is_in_play() {
        let dir = TestDir::new("plugin-registered");
        let mut env = test_env(&dir);
        install(&env);
        env.run_clis = true;
        let report = status(&env);
        assert_eq!(report.status, Status::Outdated);
        assert!(
            report.detail.as_deref().unwrap().contains("not registered"),
            "{:?}",
            report.detail
        );
        // Forge what `claude plugin marketplace add` and `install` write.
        let plugins = env.claude_dir().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(
            plugins.join("installed_plugins.json"),
            serde_json::to_vec(&serde_json::json!({"version": 2, "plugins": {
                PLUGIN_ID: [{"scope": "user", "version": env!("CARGO_PKG_VERSION")}]
            }}))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            plugins.join("known_marketplaces.json"),
            serde_json::to_vec(&serde_json::json!({MARKETPLACE: {
                "source": {"source": "directory", "path": marketplace_dir(&env)},
                "installLocation": marketplace_dir(&env),
            }}))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            settings_path(&env),
            serde_json::to_vec(&serde_json::json!({"enabledPlugins": {PLUGIN_ID: true}})).unwrap(),
        )
        .unwrap();
        assert_eq!(status(&env).status, Status::Installed);
    }
}
