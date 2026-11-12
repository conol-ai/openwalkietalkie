//! Codex Desktop's opt-in shared app-server transport, inspected in
//! Codex.app 26.930.51102 (bundled codex-cli 0.160.0).
//!
//! Desktop normally owns a private stdio server. An explicit WebSocket URL
//! over the shared daemon's Unix socket lets `codexd` track the same threads.
//! The implicit local-daemon switch is insufficient: Desktop's default plugin
//! config overrides make that path fall back to a private server.
//! Install a separate launcher: no app-bundle patch, global launchctl
//! environment, hook trust, or second owner of the CLI's hooks.json.
use super::backup::{self, FileSnapshot, Planned};
use super::{markers, Detected, Env, Plan, Report, Status, Target, Verb};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NAME: &str = "Codex with OpenWalkieTalkie.app";
const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>ai.cohdl.openwalkietalkie.codex-desktop</string>
<key>CFBundleName</key><string>Codex with OpenWalkieTalkie</string>
<key>CFBundleExecutable</key><string>launch-codex</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSUIElement</key><true/>
</dict></plist>
"#;

pub fn launcher(env: &Env) -> PathBuf {
    launcher_in(&env.home)
}

/// The launcher under `home`, without an installer environment: where the
/// Footprint page looks (`care.footprint`).
pub fn launcher_in(home: &Path) -> PathBuf {
    home.join("Applications").join(NAME)
}

// Per-user installations take precedence. Within each location prefer the
// renamed desktop app; an older Codex.app can remain after the rename.
fn desktop_apps(env: &Env) -> [PathBuf; 4] {
    [
        env.home.join("Applications/ChatGPT.app"),
        env.home.join("Applications/Codex.app"),
        PathBuf::from("/Applications/ChatGPT.app"),
        PathBuf::from("/Applications/Codex.app"),
    ]
}

fn contents(env: &Env) -> Vec<(PathBuf, Vec<u8>, u32)> {
    let script: String = include_str!("codex_desktop.sh")
        .lines()
        .map(|line| match line {
            "export CODEX_HOME=@CODEX_HOME@" => format!(
                "export CODEX_HOME={}\n",
                markers::shell_word(&env.codex_dir().display().to_string())
            ),
            "for candidate in @DESKTOP_APPS@; do" => format!(
                "for candidate in {}; do\n",
                desktop_apps(env)
                    .iter()
                    .map(|path| markers::shell_word(&path.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            _ => format!("{line}\n"),
        })
        .collect();
    vec![
        (
            launcher(env).join("Contents/Info.plist"),
            PLIST.as_bytes().to_vec(),
            0o644,
        ),
        (
            launcher(env).join("Contents/MacOS/launch-codex"),
            script.into_bytes(),
            0o755,
        ),
    ]
}

fn owned(snapshot: &FileSnapshot, expected: &[u8], receipt: Option<&Value>) -> bool {
    let Some(bytes) = snapshot.bytes() else {
        return true;
    };
    if bytes == expected {
        return true;
    }
    let path = snapshot.path().display().to_string();
    receipt
        .and_then(|r| r["files"][&path].as_str())
        .is_some_and(|hash| hash == blake3::hash(bytes).to_hex().as_str())
}

pub fn plan(env: &Env, verb: Verb, receipt: Option<&Value>) -> Result<Plan, String> {
    if !cfg!(target_os = "macos") && !verb.is_uninstall() {
        return Err("Codex Desktop's local-daemon launcher requires macOS".into());
    }
    let mut plan = Plan::default();
    let mut hashes = serde_json::Map::new();
    for (path, bytes, mode) in contents(env) {
        let snapshot = FileSnapshot::read(&path)?;
        if !owned(&snapshot, &bytes, receipt) {
            return Err(format!(
                "{} contains changes not owned by deckd; nothing was written",
                path.display()
            ));
        }
        if !verb.is_uninstall() && mode == 0o755 && snapshot.mode().is_some_and(|m| m & 0o111 == 0)
        {
            return Err(format!("{} is not executable; restore its execute permission or uninstall and reinstall the desktop launcher", path.display()));
        }
        if verb.is_uninstall() {
            plan.writes.push(Planned::remove(snapshot));
        } else {
            hashes.insert(
                path.display().to_string(),
                json!(blake3::hash(&bytes).to_hex().as_str()),
            );
            plan.writes
                .push(Planned::write_with_mode(snapshot, bytes, Some(mode)));
        }
    }
    if verb.is_uninstall() {
        plan.note("Removes only the desktop launcher. The shared Codex daemon and its sessions keep running.");
    } else {
        plan.receipt = Some(json!({"schema": 1, "files": hashes}));
        plan.note(format!("Creates {}. Opening it starts or reuses Codex's local daemon and connects Desktop directly to its Unix socket using CODEX_APP_SERVER_WS_URL. No TCP listener is opened.", launcher(env).display()));
        plan.note(format!(
            "The launcher uses CODEX_HOME={}; Codex CLI hooks and trust are managed separately.",
            env.codex_dir().display()
        ));
        plan.step(format!("Quit Codex Desktop, then open {} for desktop sessions on the deck. Keep deckd running with [codex] daemon = true and its default socket for this CODEX_HOME.", launcher(env).display()));
        plan.step("Requires Desktop's explicit WebSocket transport (inspected in Codex 26.930.51102 and ChatGPT 26.1007.71326). The shared daemon supplies server configuration; Desktop's private-server CLI overrides do not apply to it.");
    }
    Ok(plan)
}

pub fn status(env: &Env) -> Report {
    let mut report = Report::new(Target::CodexDesktop, Status::Absent);
    let receipt = super::Receipts::load(&env.receipts_path());
    let receipt = receipt.get(Target::CodexDesktop);
    let mut present = 0;
    let mut current = true;
    for (path, bytes, mode) in contents(env) {
        report.files.push(path.clone());
        report.backups.extend(backup::list_backups(
            &env.backups(),
            path.file_name().unwrap().to_str().unwrap(),
        ));
        match FileSnapshot::read(&path) {
            Ok(snapshot) => {
                present += usize::from(snapshot.exists());
                current &= snapshot.bytes() == Some(bytes.as_slice());
                if !owned(&snapshot, &bytes, receipt)
                    || (mode == 0o755 && snapshot.mode().is_some_and(|m| m & 0o111 == 0))
                {
                    report.status = Status::Conflict;
                    report.detail = Some(format!(
                        "{} was modified or is not executable; inspect it before repairing",
                        path.display()
                    ));
                    return report;
                }
            }
            Err(why) => {
                return Report {
                    status: Status::Conflict,
                    detail: Some(why),
                    ..report
                }
            }
        }
    }
    report.status = if present == 0 {
        Status::Absent
    } else if current {
        Status::Installed
    } else {
        Status::Outdated
    };
    if env.probe_clis {
        report.detected = desktop_apps(env)
            .into_iter()
            .find(|p| p.join("Contents/Resources/codex-cli/bin/codex").is_file())
            .map(|path| Detected {
                path,
                version: String::new(),
            });
    }
    report.detail = Some(if current {
        "Desktop launcher installed; quit Codex and open Codex with OpenWalkieTalkie to connect desktop sessions."
    } else {
        "Install the desktop launcher to connect Codex Desktop to the shared local app server."
    }.into());
    report.limits.push("Installed describes the launcher files, not a live desktop connection. Use the launcher after quitting Codex; local-daemon support is experimental and cloud/SSH sessions are not attached by this integration.".into());
    report
}

/// Remove only empty directories after the backed-up file removal. User
/// additions keep their directories; uninstall never recursively deletes an app.
pub fn cleanup(env: &Env) {
    for suffix in ["Contents/MacOS", "Contents", ""] {
        let _ = std::fs::remove_dir(launcher(env).join(suffix));
    }
}
