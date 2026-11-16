//! Which app a CLI session runs in, for the hand-off
//! (`deck_model::handoff`): the helper reports the macOS bundle identifier
//! of the app that owns the session, and a press on the session's `Open app`
//! card brings that app forward (`crate::desktop::raise`).
//!
//! Two sources, in this order:
//!
//! 1. **Ancestry.** Up from the CLI's parent, the first process whose
//!    executable sits inside an app bundle names the app: Zed, Ghostty,
//!    Terminal, iTerm2, VS Code, Cursor, Claude Desktop. Login-shell
//!    terminals put a root-owned `/usr/bin/login` between the shell and the
//!    app, which the CLI walk's `proc_pidinfo` call cannot read, so this
//!    walk reads processes through [`walk::Procs::get_any`]. It starts
//!    *above* the CLI, so a CLI whose own binary lives inside an app (the
//!    Codex CLI inside ChatGPT.app) never names itself, and it skips this
//!    daemon's own bundle.
//! 2. **`__CFBundleIdentifier`**, which LaunchServices sets for an app and
//!    every child inherits: what a session under `tmux` or `screen`, whose
//!    server `launchd` parents, still knows about the app that started it.
//!
//! Either answer must look like a bundle identifier ([`valid_bundle_id`]):
//! the daemon checks it again when the helper's call arrives, and only a
//! value that passes ever reaches `/usr/bin/open -b`.
//!
//! The cost is a few more process-table reads and one `Info.plist` read on
//! every hook, about a millisecond beside the `ps` the tty already takes.
use super::walk::{self, Procs};

/// The variable LaunchServices sets to the bundle identifier of the app it
/// launched, inherited by every process below it.
pub const ENV: &str = "__CFBundleIdentifier";

/// How far up from the CLI the walk looks for an app. A terminal app is
/// two or three processes above a CLI (`claude ← -zsh ← login ← Zed`); the
/// bound leaves room for wrappers and still stops long before `launchd`.
pub const MAX_APP_HOPS: usize = 12;

/// Whether `id` looks like a bundle identifier: 1 to 255 bytes of ASCII
/// letters, digits, `.`, `-` and `_`, starting with a letter or digit, with
/// at least two non-empty labels (`dev.zed.Zed`). Everything else is
/// dropped: the value ends up in another process's argv.
#[must_use]
pub fn valid_bundle_id(id: &str) -> bool {
    (1..=255).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && id.contains('.')
        && id.split('.').all(|label| !label.is_empty())
}

/// The outermost app bundle an executable path sits in: the prefix up to
/// and including the first component that ends in `.app` and has more path
/// after it. A helper nested inside an app's own frameworks maps to the app
/// (`…/Visual Studio Code.app/…/Code Helper.app/…` → `…/Visual Studio
/// Code.app`); a plain binary maps to nothing.
#[must_use]
pub fn outermost_bundle(exe: &str) -> Option<&str> {
    let mut start = 0;
    for (at, _) in exe.match_indices('/') {
        let component = &exe[start..at];
        if component.len() > ".app".len() && component.ends_with(".app") {
            return Some(&exe[..at]);
        }
        start = at + 1;
    }
    None
}

/// Up from `start`, the first process whose executable sits in an app
/// bundle that is not `own` and whose identifier `resolve` reads and
/// [`valid_bundle_id`] accepts. A bundle that cannot be read is passed
/// over, not an answer. Stops at `launchd`, at a process that is its own
/// parent, at a pid the table does not know, and after [`MAX_APP_HOPS`].
#[must_use]
pub fn by_ancestry(
    start: u32,
    procs: &impl Procs,
    own: Option<&str>,
    resolve: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut cur = start;
    for _ in 0..MAX_APP_HOPS {
        let proc = procs.get_any(cur)?;
        let found = proc
            .names
            .iter()
            .filter_map(|name| outermost_bundle(name))
            .find(|bundle| Some(*bundle) != own)
            .and_then(&resolve)
            .filter(|id| valid_bundle_id(id));
        if found.is_some() {
            return found;
        }
        if proc.pid <= 1 || proc.ppid == proc.pid || proc.ppid == 0 {
            return None;
        }
        cur = proc.ppid;
    }
    None
}

/// The app the session runs in: the ancestry above the CLI the walk found
/// (above the helper itself when it found none), then the environment.
#[must_use]
pub fn owning_app(
    walk: &walk::Walk,
    procs: &impl Procs,
    helper_pid: u32,
    own: Option<&str>,
    resolve: impl Fn(&str) -> Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let below = walk.cli_pid.unwrap_or(helper_pid);
    procs
        .get_any(below)
        .map(|proc| proc.ppid)
        .filter(|&ppid| ppid > 0)
        .and_then(|start| by_ancestry(start, procs, own, resolve))
        .or_else(|| env(ENV).filter(|id| valid_bundle_id(id)))
}

/// The bundle's own identifier, read from its `Info.plist` by Foundation.
#[cfg(target_os = "macos")]
#[must_use]
pub fn bundle_id_of(bundle: &str) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};
    objc2::rc::autoreleasepool(|_| {
        NSBundle::bundleWithPath(&NSString::from_str(bundle))
            .and_then(|b| b.bundleIdentifier())
            .map(|id| id.to_string())
    })
}

/// No app bundles off macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn bundle_id_of(_bundle: &str) -> Option<String> {
    None
}

/// This helper's own answer on Windows, where there are no bundles: up from
/// the CLI's parent (the helper's, when the walk found no CLI), the first
/// process that owns an app window names the app by its executable's file
/// name (`WindowsTerminal.exe`, `Code.exe`), which [`valid_bundle_id`]
/// accepts as it accepts an identifier. This app's own window is passed
/// over, as macOS passes over its own bundle.
#[cfg(windows)]
#[must_use]
pub fn from_process(walk: &walk::Walk) -> Option<String> {
    let procs = walk::MacProcs;
    let windowed = crate::winwin::windowed_pids();
    let below = walk.cli_pid.unwrap_or_else(std::process::id);
    let mut cur = procs.get_any(below).map(|proc| proc.ppid).filter(|&ppid| ppid > 0)?;
    for _ in 0..MAX_APP_HOPS {
        let proc = procs.get_any(cur)?;
        if windowed.contains(&proc.pid) {
            let name = proc.names.first()?;
            let own = name.eq_ignore_ascii_case("OpenWalkieTalkie.exe")
                || name.eq_ignore_ascii_case("openwalkietalkie-app.exe");
            if !own && valid_bundle_id(name) {
                return Some(name.clone());
            }
        }
        if proc.ppid == 0 || proc.ppid == proc.pid {
            return None;
        }
        cur = proc.ppid;
    }
    None
}

/// This helper's own answer: the kernel's process table, the bundles on
/// disk and its own environment.
#[cfg(not(windows))]
#[must_use]
pub fn from_process(walk: &walk::Walk) -> Option<String> {
    let exe = std::env::current_exe().ok();
    let own = exe
        .as_deref()
        .and_then(std::path::Path::to_str)
        .and_then(outermost_bundle);
    owning_app(
        walk,
        &walk::MacProcs,
        std::process::id(),
        own,
        bundle_id_of,
        |name| std::env::var(name).ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::walk::Proc;

    /// A fixture process table.
    struct Table(Vec<Proc>);

    impl Procs for Table {
        fn get(&self, pid: u32) -> Option<Proc> {
            self.0.iter().find(|p| p.pid == pid).cloned()
        }
    }

    /// The bundles a fixture can resolve, as `bundle_id_of` would on a Mac
    /// that has them installed.
    fn installed(bundle: &str) -> Option<String> {
        match bundle {
            "/Applications/Zed.app" => Some("dev.zed.Zed".into()),
            "/Applications/Ghostty.app" => Some("com.mitchellh.ghostty".into()),
            "/Applications/ChatGPT.app" => Some("com.openai.codex".into()),
            "/Applications/OpenWalkieTalkie.app" => Some("ai.cohdl.openwalkietalkie".into()),
            "/Applications/Visual Studio Code.app" => Some("com.microsoft.VSCode".into()),
            _ => None,
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn walk(cli: Option<u32>) -> walk::Walk {
        walk::Walk {
            cli_pid: cli,
            pid_kind: if cli.is_some() {
                deck_model::PidKind::Cli
            } else {
                deck_model::PidKind::Unknown
            },
            hops: 1,
            chain: Vec::new(),
        }
    }

    #[test]
    fn a_bundle_id_is_reverse_dns_and_nothing_else() {
        for good in [
            "com.mitchellh.ghostty",
            "dev.zed.Zed",
            "com.todesktop.230313mzl4w4u92",
            "com.anthropic.claudefordesktop",
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "a.b",
        ] {
            assert!(valid_bundle_id(good), "{good}");
        }
        let long = format!("a.{}", "b".repeat(254));
        for bad in [
            "",
            "-b",
            "a",
            ".a.b",
            "a..b",
            "a.b.",
            "a b",
            "a/b",
            "a.b;c",
            "zed.Zed\n",
            "é.app",
            long.as_str(),
        ] {
            assert!(!valid_bundle_id(bad), "{bad:?}");
        }
        assert!(valid_bundle_id(&long[..255]));
    }

    #[test]
    fn the_outermost_bundle_is_the_app_and_not_its_helper() {
        assert_eq!(
            outermost_bundle(
                "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)"
            ),
            Some("/Applications/Visual Studio Code.app")
        );
        assert_eq!(
            outermost_bundle(
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"
            ),
            Some("/Applications/ChatGPT.app")
        );
        assert_eq!(
            outermost_bundle("/Applications/Zed.app/Contents/MacOS/zed"),
            Some("/Applications/Zed.app")
        );
        for none in [
            "/usr/bin/login",
            "-zsh",
            "zsh",
            "/x/my.app.bak/y",
            "/x/.app/y",
            "/x/Zed.app",
        ] {
            assert_eq!(outermost_bundle(none), None, "{none}");
        }
    }

    /// Measured on this Mac with `ps`: `claude ← -zsh ← /usr/bin/login
    /// (root) ← Zed`. The login process is what `get_any` exists for.
    #[test]
    fn a_login_shell_in_zed_names_zed() {
        let table = Table(vec![
            Proc::new(
                500,
                400,
                &[
                    "deckd",
                    "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
                ],
            ),
            Proc::new(
                400,
                300,
                &["claude", "/Users/u/.local/share/claude/versions/2.1.276"],
            ),
            Proc::new(300, 200, &["-zsh"]),
            Proc::new(200, 100, &["login", "/usr/bin/login"]),
            Proc::new(100, 1, &["zed", "/Applications/Zed.app/Contents/MacOS/zed"]),
            Proc::new(1, 0, &["launchd", "/sbin/launchd"]),
        ]);
        let own = Some("/Applications/OpenWalkieTalkie.app");
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 500, own, installed, no_env).as_deref(),
            Some("dev.zed.Zed")
        );
    }

    /// `codex ← -/bin/zsh ← login ← Ghostty`.
    #[test]
    fn a_login_shell_in_ghostty_names_ghostty() {
        let table = Table(vec![
            Proc::new(400, 300, &["codex", "/opt/homebrew/bin/codex"]),
            Proc::new(300, 200, &["-/bin/zsh"]),
            Proc::new(200, 100, &["login", "/usr/bin/login"]),
            Proc::new(
                100,
                1,
                &[
                    "ghostty",
                    "/Applications/Ghostty.app/Contents/MacOS/ghostty",
                ],
            ),
            Proc::new(1, 0, &["launchd", "/sbin/launchd"]),
        ]);
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, no_env).as_deref(),
            Some("com.mitchellh.ghostty")
        );
    }

    /// The Codex CLI ChatGPT.app ships, run in Ghostty: the walk starts
    /// above the CLI, so it is Ghostty that comes forward, never ChatGPT.
    #[test]
    fn a_cli_inside_an_app_bundle_never_names_its_own_app() {
        let table = Table(vec![
            Proc::new(
                400,
                300,
                &["codex", "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"],
            ),
            Proc::new(300, 100, &["zsh", "/bin/zsh"]),
            Proc::new(100, 1, &["ghostty", "/Applications/Ghostty.app/Contents/MacOS/ghostty"]),
        ]);
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, no_env).as_deref(),
            Some("com.mitchellh.ghostty")
        );
    }

    /// `claude ← tmux ← launchd`: no app above, so the variable the tmux
    /// server inherited decides — and only a plausible one.
    #[test]
    fn under_tmux_the_environment_decides() {
        let table = Table(vec![
            Proc::new(400, 300, &["claude", "claude"]),
            Proc::new(300, 1, &["tmux", "/opt/homebrew/bin/tmux"]),
            Proc::new(1, 0, &["launchd", "/sbin/launchd"]),
        ]);
        let env = |name: &str| (name == ENV).then(|| "com.mitchellh.ghostty".to_string());
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, env).as_deref(),
            Some("com.mitchellh.ghostty")
        );
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, no_env),
            None
        );
        let forged = |name: &str| (name == ENV).then(|| "-b com.evil".to_string());
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, forged),
            None,
            "an implausible value is ignored"
        );
    }

    /// This daemon's own app is never the answer.
    #[test]
    fn our_own_bundle_is_skipped() {
        let table = Table(vec![
            Proc::new(400, 300, &["claude", "claude"]),
            Proc::new(
                300,
                100,
                &[
                    "OpenWalkieTalkie",
                    "/Applications/OpenWalkieTalkie.app/Contents/MacOS/OpenWalkieTalkie",
                ],
            ),
            Proc::new(100, 1, &["zed", "/Applications/Zed.app/Contents/MacOS/zed"]),
        ]);
        let own = Some("/Applications/OpenWalkieTalkie.app");
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, own, installed, no_env).as_deref(),
            Some("dev.zed.Zed")
        );
    }

    /// A bundle whose identifier cannot be read is passed over, and the
    /// walk goes on upward.
    #[test]
    fn an_unreadable_bundle_lets_the_walk_continue() {
        let table = Table(vec![
            Proc::new(400, 300, &["claude", "claude"]),
            Proc::new(
                300,
                100,
                &["wrapper", "/Users/u/Broken.app/Contents/MacOS/wrapper"],
            ),
            Proc::new(100, 1, &["zed", "/Applications/Zed.app/Contents/MacOS/zed"]),
        ]);
        assert_eq!(
            owning_app(&walk(Some(400)), &table, 450, None, installed, no_env).as_deref(),
            Some("dev.zed.Zed")
        );
    }

    /// With no CLI found, the walk starts above the helper itself.
    #[test]
    fn without_a_cli_the_walk_starts_above_the_helper() {
        let table = Table(vec![
            Proc::new(500, 300, &["deckd", "deckd"]),
            Proc::new(300, 100, &["sh", "/bin/sh"]),
            Proc::new(100, 1, &["zed", "/Applications/Zed.app/Contents/MacOS/zed"]),
        ]);
        assert_eq!(
            owning_app(&walk(None), &table, 500, None, installed, no_env).as_deref(),
            Some("dev.zed.Zed")
        );
    }

    /// The hop bound is a ceiling: an app further up than it is not found.
    #[test]
    fn the_app_hop_budget_is_a_ceiling() {
        let mut procs: Vec<Proc> = (0..20u32)
            .map(|i| Proc::new(1000 + i, 1001 + i, &["sh", "/bin/sh"]))
            .collect();
        procs.push(Proc::new(
            1020,
            1,
            &["zed", "/Applications/Zed.app/Contents/MacOS/zed"],
        ));
        let table = Table(procs);
        assert_eq!(by_ancestry(1000, &table, None, installed), None);
        assert_eq!(
            by_ancestry(1020 - MAX_APP_HOPS as u32 + 1, &table, None, installed).as_deref(),
            Some("dev.zed.Zed")
        );
    }

    /// Foundation reads the identifier from the bundle's own `Info.plist`
    /// (an XML one here). Reads files only.
    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_id_of_reads_the_bundles_own_info_plist() {
        let dir = std::env::temp_dir().join(format!("deckd-app-{}", std::process::id()));
        let bundle = dir.join("Fake.app");
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.example.fake</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
"#,
        )
        .unwrap();
        assert_eq!(
            bundle_id_of(bundle.to_str().unwrap()).as_deref(),
            Some("com.example.fake")
        );
        assert_eq!(
            bundle_id_of(dir.join("Missing.app").to_str().unwrap()),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
