//! `deckd::integrations` — the hook installer (design §5.7, API §4.11).
//!
//! Four verbs (`status`, `install`, `uninstall`, `repair`) over five targets
//! (`claude`, `codex`, `codex-desktop`, `gemini`, `claude-desktop`). Every verb goes through the same three steps, and the order is the whole safety
//! argument:
//!
//! 1. **Plan.** Each target module reads the files it owns, merges its own
//!    entries into the user's structure, and returns a [`Plan`]: the bytes
//!    each file should end up as, the human lines of "What this writes", the
//!    conflicts it found and the steps the operator still has to take. A plan
//!    is pure — `--dry-run` prints it and writes nothing.
//! 2. **Apply.** [`backup::apply`] stages every file, re-checks every
//!    snapshot, and publishes one file at a time, moving each live file into
//!    `<data>/backups/<file>.<ts>.bak` first. A no-op plan changes nothing,
//!    which is what makes `install` idempotent.
//! 3. **Verify.** The Claude target re-reads the file it has just written and
//!    runs Claude's own fatal-settings predicate over it (C24a). A file that
//!    trips it is silently ignored *in its entirety* by Claude — model,
//!    permissions, plugins and every hook — so a failure here restores the
//!    backup rather than reporting success.
//!
//! Three rules this module exists to keep, each measured rather than assumed:
//!
//! - **Never edit a key we do not own.** Entries are recognised by `url` and
//!   `command`, never by a marker: Claude deletes `"_managed"` on its next
//!   write (C24, S2). A foreign hook on one of our events is reported, never
//!   replaced.
//! - **Never write a shape that voids the file.** C24a for Claude's settings
//!   inventory; C12 for Codex, where one array-form `command` makes 0.153.4
//!   discard *every* hook in `hooks.json` with nothing but a warning.
//! - **Never bypass a review the user is meant to give.** Codex hook trust is
//!   not written by default (C14); `install codex --trust` is the explicit
//!   opt-in and it reads the hash from `hooks/list` rather than forging one.
pub mod backup;
pub mod claude;
pub mod claude_legacy;
pub mod codex;
pub mod codex_desktop;
pub mod markers;
pub mod testdir;

use crate::config::{Config, Paths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One party deckd integrates with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Claude,
    Codex,
    CodexDesktop,
    Gemini,
    ClaudeDesktop,
}

impl Target {
    pub const ALL: [Target; 5] = [
        Target::Claude,
        Target::Codex,
        Target::CodexDesktop,
        Target::Gemini,
        Target::ClaudeDesktop,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Codex => "codex",
            Target::CodexDesktop => "codex-desktop",
            Target::Gemini => "gemini",
            Target::ClaudeDesktop => "claude-desktop",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Target::Claude => "Claude Code",
            Target::Codex => "Codex CLI",
            Target::CodexDesktop => "Codex Desktop",
            Target::Gemini => "Gemini CLI",
            Target::ClaudeDesktop => "Claude Desktop",
        }
    }

    /// Does installing this target write a file that makes a third party run
    /// `deckd hook …`? Claude does twice (the exec-form `SessionStart` hook
    /// and the chained `statusLine`), Codex once per event group.
    pub const fn writes_hook_commands(self) -> bool {
        matches!(self, Target::Claude | Target::Codex)
    }

    pub fn parse(name: &str) -> Result<Target, String> {
        Target::ALL
            .into_iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = Target::ALL.iter().map(|t| t.name()).collect();
                format!("unknown target {name:?}; one of {}", names.join(", "))
            })
    }
}

/// `integrations.status`'s vocabulary: design §4.11 and §5.7, plus C14's
/// `untrusted`/`modified`, which are Codex's first-class trust states.
///
/// **A word is in here only when some input produces it.** Four of §4.11's
/// once were not, and two of them have their paths now: `status` asks
/// codex's `hooks/list` whenever it may run the CLIs, so `hooks_not_firing`
/// is what it says when the file names deckd's hooks and codex reports none
/// of them (C18 — the absence of a `SessionStart` still decides nothing),
/// and `paused` is codex's own `enabled: false`. The other two are still
/// unreachable and still out: `hooks_unreachable` and `hooks_hijack_risk`
/// are the port-degrade states of §4.1, which reach `/v1/info.degraded` and
/// the tray but which `integrations::api_call` cannot see (it runs on a
/// blocking pool with a `Paths`/`Config` of its own and no handle on the
/// daemon's service state). Publishing a word no answer can carry is worse
/// than not publishing it — a client writes a branch for it and the branch
/// is dead — so they land with their paths, and
/// [`the_status_vocabulary_is_reachable_and_spelled_as_the_wire_spells_it`]
/// fails if a third one ever arrives without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Installed,
    Outdated,
    Absent,
    Conflict,
    PathDrift,
    /// Codex: `hooks/list` reports none of deckd's hooks, so they are on
    /// disk and dead.
    HooksNotFiring,
    /// Codex: trusted, but switched off in `/hooks`.
    Paused,
    /// Codex: the hook is written but `[hooks.state]` has no hash for it, so
    /// Codex **skips it silently** until the user reviews it in `/hooks`.
    Untrusted,
    /// Codex: a hash is recorded but the definition changed under it (a path,
    /// timeout or matcher change), so the hooks stopped firing.
    Modified,
}

impl Status {
    /// The whole vocabulary, in the order §5.7 lists it.
    pub const ALL: [Status; 9] = [
        Status::Installed,
        Status::Outdated,
        Status::Absent,
        Status::Conflict,
        Status::PathDrift,
        Status::HooksNotFiring,
        Status::Paused,
        Status::Untrusted,
        Status::Modified,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Installed => "installed",
            Status::Outdated => "outdated",
            Status::Absent => "absent",
            Status::Conflict => "conflict",
            Status::PathDrift => "path_drift",
            Status::HooksNotFiring => "hooks_not_firing",
            Status::Paused => "paused",
            Status::Untrusted => "untrusted",
            Status::Modified => "modified",
        }
    }
}

/// The CLI this target integrates with, as found on `PATH`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detected {
    pub path: PathBuf,
    pub version: String,
}

/// One row of `integrations.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub target: Target,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected: Option<Detected>,
    pub files: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What this status cannot tell you. The honest limits of §4.11: C4
    /// (settings precedence), C11 (a missing hook may be a proxy skip), C18
    /// (`hooks/list`, not the absence of a `SessionStart`), C24b (`claude
    /// doctor` exits 0 even on a rejected file).
    pub limits: Vec<String>,
}

impl Report {
    pub fn new(target: Target, status: Status) -> Report {
        Report {
            target,
            status,
            detected: None,
            files: Vec::new(),
            backups: Vec::new(),
            last_event_at: None,
            detail: None,
            limits: Vec::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Report {
        self.detail = Some(detail.into());
        self
    }
}

/// What an installer run is trying to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Install {
        trust: bool,
    },
    Uninstall,
    /// Re-install with the current path, port and token. It is `install` that
    /// goes *through* a `path_drift` rather than around it, which the merge
    /// gets for free: entries are recognised by shape, not by the path or
    /// port they embed, so a moved binary's entries are rewritten in place
    /// instead of being left behind as a stranger's.
    Repair {
        trust: bool,
    },
}

impl Verb {
    pub fn is_uninstall(self) -> bool {
        matches!(self, Verb::Uninstall)
    }

    /// Codex only: may this run write `[hooks.state]` (C14)?
    pub fn trusts(self) -> bool {
        matches!(
            self,
            Verb::Install { trust: true } | Verb::Repair { trust: true }
        )
    }
}

/// "What this writes", before anything is written.
#[derive(Debug, Default)]
pub struct Plan {
    pub writes: Vec<backup::Planned>,
    /// One line per user-visible effect.
    pub notes: Vec<String>,
    /// Things the installer refused to touch because they are not ours.
    pub conflicts: Vec<String>,
    /// What the operator still has to do (the Codex `/hooks` review).
    pub next_steps: Vec<String>,
    /// What to record in `<data>/integrations.json` when the plan applies.
    pub receipt: Option<serde_json::Value>,
    /// The CLI's own commands this plan runs ("What this runs"): the Claude
    /// target registers its plugin through `claude plugin …` rather than by
    /// writing into Claude's files itself.
    pub commands: Vec<CliCommand>,
}

/// One `claude …` (or, one day, `codex …`) invocation an install or
/// uninstall runs, after or before its writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliCommand {
    /// The binary, resolved on `PATH` at run time.
    pub program: &'static str,
    pub args: Vec<String>,
    /// Failure codes or message fragments that mean "already done": an
    /// uninstall of a plugin that is not installed is not a failure.
    pub tolerated: Vec<&'static str>,
    /// Run before the plan's writes rather than after them (an uninstall
    /// reads the plugin's manifest from the folder the writes remove).
    pub before_writes: bool,
}

impl std::fmt::Display for CliCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

impl Plan {
    pub fn note(&mut self, line: impl Into<String>) {
        self.notes.push(line.into());
    }

    pub fn conflict(&mut self, line: impl Into<String>) {
        self.conflicts.push(line.into());
    }

    pub fn step(&mut self, line: impl Into<String>) {
        self.next_steps.push(line.into());
    }

    /// The paths this plan would change, for `--dry-run`.
    pub fn changes(&self) -> Vec<serde_json::Value> {
        self.writes
            .iter()
            .filter(|planned| !planned.is_noop())
            .map(|planned| {
                serde_json::json!({
                    "path": planned.snapshot.path(),
                    "action": match planned.action {
                        backup::Action::Write(_) if planned.snapshot.exists() => "update",
                        backup::Action::Write(_) => "create",
                        backup::Action::Remove => "remove",
                    },
                })
            })
            .collect()
    }
}

/// What one applied run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub target: String,
    pub changed: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub conflicts: Vec<String>,
    pub next_steps: Vec<String>,
}

/// Whether the installer may talk to `launchctl`. Tests set `Disabled`: a
/// `launchctl bootout` in a unit test would stop the user's own running
/// deckd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launchctl {
    Real,
    Disabled,
}

/// Everything the installer needs that comes from outside itself.
#[derive(Clone, Debug)]
pub struct Env {
    /// The absolute path hook files embed: `current_exe()`, validated.
    pub exe: PathBuf,
    /// What `.mcp.json` launches the Claude channel helper from: `exe`,
    /// unless that is a transient path and pinning the installed bundle is
    /// safe (see `claude::channel_exe`).
    pub channel_exe: PathBuf,
    /// deckd's data directory (`backups/`, `integrations.json`).
    pub data: PathBuf,
    /// `tokens.toml` — where the `hooks` token's hash lives.
    pub tokens: PathBuf,
    /// `[api] port`, the port the hook URL names.
    pub port: u16,
    /// `[hooks.claude] hold_s` — the held hook `timeout` must exceed it.
    pub hold_s: u32,
    /// `[hooks.claude] tool_titles`: install the `PostToolUse` hook at all.
    pub tool_titles: bool,
    /// The home the target paths default to (`HOME`, or a temp one in tests).
    pub home: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, when set.
    pub claude_config_dir: Option<PathBuf>,
    /// `CODEX_HOME`, when set.
    pub codex_home: Option<PathBuf>,
    pub launchctl: Launchctl,
    /// May `status` run the CLIs? `claude --version` / `codex --version` for
    /// `detected`, and `codex app-server`'s `hooks/list` for Codex's real
    /// trust state (C14a, C18: deckd cannot compute a hook's hash, so the
    /// files alone cannot tell `trusted` from `modified`). Off in tests so
    /// `status` is deterministic and spawns nothing — a row that answered
    /// from the files alone says so in its `limits`.
    pub probe_clis: bool,
    /// May `install`/`uninstall` run the CLIs' own commands (`claude plugin
    /// marketplace add`, `claude plugin install`, …)? Off in tests, which
    /// assert on the files and never register anything with a real Claude.
    /// With it off, the commands are printed as next steps instead.
    pub run_clis: bool,
    /// Check that this binary answers `hook --help` before writing files
    /// that make a CLI run `deckd hook <cli>`. See
    /// [`hook_subcommand_check`]; only a build whose `hook` subcommand is
    /// deliberately absent turns it off.
    pub verify_hook_subcommand: bool,
    /// Non-fatal findings from [`Env::detect`] (C22: outside `/Applications`
    /// is a warning, translocation is a refusal).
    pub warnings: Vec<String>,
}

/// The port the hooks must name. `[api] port` is what the daemon *asks*
/// for, and `0` means "any free port", so an installer that copied it would
/// write `http://127.0.0.1:0/…` into the user's settings — a URL nothing can
/// ever answer. The running daemon writes what it actually bound to the
/// `port` file (§9), so that is the truth when it exists.
fn bound_port(port_file: &std::path::Path, configured: u16) -> Result<u16, String> {
    let bound = std::fs::read_to_string(port_file)
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .filter(|port| *port != 0);
    match (bound, configured) {
        (Some(port), _) => Ok(port),
        (None, 0) => Err(
            "deckd has not bound a port yet and `[api] port` is 0 (any free port), so the hooks \
             would name a port nothing answers: start the service first, or set `[api] port`"
                .into(),
        ),
        (None, configured) => Ok(configured),
    }
}

impl Env {
    /// The installer's view of this machine. Refuses to run translocated
    /// (C22) and warns — but does not refuse — outside `/Applications`,
    /// because this machine's own bundle lives in `~/Applications`.
    pub fn detect(paths: &Paths, config: &Config) -> Result<Env, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate the deckd executable: {e}"))?;
        Env::with_exe(exe, paths, config)
    }

    pub fn with_exe(exe: PathBuf, paths: &Paths, config: &Config) -> Result<Env, String> {
        validate_executable(&exe)?;
        let text = exe
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", exe.display()))?
            .to_string();
        if cfg!(target_os = "macos") && text.contains("/AppTranslocation/") {
            return Err(
                "deckd is running from macOS App Translocation; move OpenWalkieTalkie.app to \
                 Applications and reopen it before installing hooks"
                    .into(),
            );
        }
        let mut warnings = Vec::new();
        if cfg!(target_os = "macos") && !text.contains("/Applications/") {
            warnings.push(format!(
                "{text} is outside /Applications: the hooks will name this path, and moving the \
                 binary later shows up as path_drift (and, for Codex, breaks hook trust)"
            ));
        }
        let home = home_dir()?;
        let channel_exe = claude::resolve_channel_exe(&exe, &paths.data, true);
        Ok(Env {
            exe,
            channel_exe,
            data: paths.data.clone(),
            tokens: paths.tokens.clone(),
            port: bound_port(&paths.port, config.api.port)?,
            hold_s: config.hooks.claude.hold_s,
            tool_titles: config.hooks.claude.tool_titles,
            claude_config_dir: nonempty_env_path("CLAUDE_CONFIG_DIR")?,
            codex_home: nonempty_env_path("CODEX_HOME")?,
            home,
            launchctl: Launchctl::Real,
            probe_clis: true,
            run_clis: true,
            verify_hook_subcommand: true,
            warnings,
        })
    }

    pub fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }

    pub fn receipts_path(&self) -> PathBuf {
        self.data.join("integrations.json")
    }

    /// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub fn claude_dir(&self) -> PathBuf {
        self.claude_config_dir
            .clone()
            .unwrap_or_else(|| self.home.join(".claude"))
    }

    /// `$CODEX_HOME`, else `~/.codex`.
    pub fn codex_dir(&self) -> PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    /// The URL the Claude http hooks POST to.
    pub fn hook_url(&self, cli: &str) -> String {
        format!("http://127.0.0.1:{}/v1/hooks/{cli}", self.port)
    }

    /// The `allowedHttpHookUrls` pattern that covers [`Env::hook_url`].
    pub fn hook_url_pattern(&self) -> String {
        format!("http://127.0.0.1:{}/v1/hooks/*", self.port)
    }
}

/// `$NAME` as an absolute path; an empty value is "unset" and a relative one
/// is a hard error rather than a surprise write under the cwd.
fn nonempty_env_path(name: &str) -> Result<Option<PathBuf>, String> {
    env_path(name, std::env::var_os(name).as_deref())
}

/// The pure half of [`nonempty_env_path`], so the rule can be tested without
/// a test mutating the process environment out from under its neighbours.
fn env_path(name: &str, value: Option<&std::ffi::OsStr>) -> Result<Option<PathBuf>, String> {
    match value {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(format!("{name} must be an absolute path"));
            }
            Ok(Some(path))
        }
    }
}

fn home_dir() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    directories::BaseDirs::new()
        .map(|base| base.home_dir().to_path_buf())
        .ok_or_else(|| "no home directory".to_string())
}

fn validate_executable(path: &Path) -> Result<(), String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "{} is not a regular executable file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
    }
    Ok(())
}

/// deckd's own record of what it wrote, per target. It is *not* the source
/// of truth for ownership (that is the `url`/`command` fingerprint) — it
/// carries the things the third-party file has nowhere to keep, such as the
/// Codex `config.toml` keys we may remove again on uninstall.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Receipts {
    #[serde(flatten)]
    targets: BTreeMap<String, serde_json::Value>,
}

impl Receipts {
    pub fn load(path: &Path) -> Receipts {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, target: Target) -> Option<&serde_json::Value> {
        self.targets.get(target.name())
    }

    pub fn set(&mut self, target: Target, value: Option<serde_json::Value>) {
        match value {
            Some(value) => {
                self.targets.insert(target.name().to_string(), value);
            }
            None => {
                self.targets.remove(target.name());
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|e| format!("cannot render receipts: {e}"))?;
        bytes.push(b'\n');
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temp, &bytes)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// How long `deckd hook --help` may take before the guard gives up. It is
/// argument parsing and a usage string; a second is already generous.
const HOOK_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Does this binary carry the `hook` subcommand the files it writes invoke?
///
/// Every hook this installer writes runs `deckd hook <claude|codex|
/// claude-statusline>`. A build without that subcommand does not fail
/// quietly: clap exits **2** with a usage error on stderr, and a Codex
/// `PermissionRequest` reads exit 2 + stderr as a **deny** (C21), so such an
/// install denies every permission request on the machine. Claude's
/// exec-form `SessionStart` hook and the chained `statusLine` fail on every
/// session for the same reason. That is the one thing the week-3 rule "a
/// hook must never make a CLI wait or see a non-zero exit" forbids outright,
/// so `install` and `repair` refuse rather than write it.
///
/// It is a self-check, not a CLI probe: the binary asked is `env.exe`, the
/// very path the hook files will name.
pub fn hook_subcommand_check(exe: &Path) -> Result<(), String> {
    let refusal = |why: String| {
        format!(
            "{} does not answer `hook --help` ({why}), so the hooks this would write could not \
             run: a CLI that invokes a missing subcommand gets clap's exit 2 and a usage error on \
             stderr, which Codex reads as a DENY of every permission request (C21) and Claude as \
             a failed SessionStart hook. Install a deckd build that carries `deckd hook` first.",
            exe.display()
        )
    };
    let mut child = std::process::Command::new(exe)
        .arg("hook")
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| refusal(format!("it could not be run: {e}")))?;
    // `wait()` has no timeout, so wait on a thread and kill what outlives it:
    // a hung binary must not hang `integrations install`.
    let (tx, rx) = std::sync::mpsc::channel();
    let waited = std::thread::spawn(move || {
        let status = child.wait();
        if tx.send(status.as_ref().ok().map(|s| s.success())).is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        status
    });
    match rx.recv_timeout(HOOK_CHECK_TIMEOUT) {
        Ok(Some(true)) => {
            let _ = waited.join();
            Ok(())
        }
        Ok(Some(false)) | Ok(None) => {
            let _ = waited.join();
            Err(refusal("it exited non-zero".to_string()))
        }
        Err(_) => Err(refusal(format!(
            "it did not exit within {} s",
            HOOK_CHECK_TIMEOUT.as_secs()
        ))),
    }
}

/// How long one `claude plugin …` command may take. Marketplace and plugin
/// registration are local file operations for a path source, but the CLI
/// starts a Node runtime first.
const CLI_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

/// Run one of the plan's CLI commands.
///
/// `Ok(Some(line))` is a success worth a note; `Ok(None)` means this
/// environment may not run CLIs (`run_clis` off) or the binary is not on
/// `PATH`, so the caller prints the command as a next step instead;
/// `Err(why)` is a real refusal. The command's `--json` result line, when
/// there is one, decides: `outcome: "ok"` is success, and a `failureCode` or
/// message naming one of the command's `tolerated` fragments is "already
/// done". `CLAUDE_CONFIG_DIR` is passed through when the environment names
/// one, so the CLI registers into the same config dir the files describe.
pub fn run_cli(env: &Env, command: &CliCommand) -> Result<Option<String>, String> {
    if !env.run_clis {
        return Ok(None);
    }
    let Some(binary) = which(command.program) else {
        return Ok(None);
    };
    let mut process = std::process::Command::new(&binary);
    process
        .args(&command.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if command.program == "claude" {
        if let Some(dir) = &env.claude_config_dir {
            process.env("CLAUDE_CONFIG_DIR", dir);
        }
    }
    let mut child = process
        .spawn()
        .map_err(|e| format!("{} could not be run: {e}", binary.display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let mut err = String::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_string(&mut out);
        }
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut err);
        }
        let status = child.wait();
        let _ = tx.send((status, out, err));
    });
    let (status, out, err) = rx.recv_timeout(CLI_COMMAND_TIMEOUT).map_err(|_| {
        format!(
            "`{command}` did not finish within {} s",
            CLI_COMMAND_TIMEOUT.as_secs()
        )
    })?;
    let result: Option<serde_json::Value> = out
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .and_then(|line| serde_json::from_str(line.trim()).ok());
    let message = result
        .as_ref()
        .and_then(|r| r["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let text = if err.trim().is_empty() { &out } else { &err };
            text.lines().last().unwrap_or_default().trim().to_string()
        });
    let failure_code = result
        .as_ref()
        .and_then(|r| r["failureCode"].as_str())
        .unwrap_or_default()
        .to_string();
    let ok = match &result {
        Some(r) => r["outcome"].as_str() == Some("ok"),
        None => status.as_ref().is_ok_and(|s| s.success()),
    };
    if ok {
        return Ok(Some(format!("claude: {message}")));
    }
    let tolerated = command
        .tolerated
        .iter()
        .any(|t| failure_code == *t || message.contains(t));
    if tolerated {
        return Ok(Some(format!("claude: {message} (already so)")));
    }
    Err(if message.is_empty() {
        format!("exit {:?}", status.ok().and_then(|s| s.code()))
    } else {
        message
    })
}

/// The plan for one target, without writing anything ("What this writes").
pub fn plan(env: &Env, target: Target, verb: Verb) -> Result<Plan, String> {
    // Before the plan, not after: `--dry-run` must refuse too, and a refusal
    // that arrives only at apply time has already printed "What this writes".
    if !verb.is_uninstall() && env.verify_hook_subcommand && target.writes_hook_commands() {
        hook_subcommand_check(&env.exe)?;
    }
    let receipts = Receipts::load(&env.receipts_path());
    match target {
        Target::Claude => claude::plan(env, verb, receipts.get(target)),
        Target::Codex => codex::plan(env, verb, receipts.get(target)),
        Target::CodexDesktop => codex_desktop::plan(env, verb, receipts.get(target)),
        Target::Gemini | Target::ClaudeDesktop => Err(format!(
            "{} is designed but not built in v1 (design §5.3, §4.16); integrations status reports \
             it as absent",
            target.display_name()
        )),
    }
}

/// Plan, apply, record and verify one target.
pub fn run(env: &Env, target: Target, verb: Verb) -> Result<Receipt, String> {
    let plan = plan(env, target, verb)?;
    apply(env, target, verb, plan)
}

/// Apply a plan that was already made (so `--dry-run` and the real run share
/// one code path and one set of refusals).
pub fn apply(env: &Env, target: Target, verb: Verb, plan: Plan) -> Result<Receipt, String> {
    let Plan {
        writes,
        notes,
        conflicts,
        next_steps,
        receipt,
        commands,
    } = plan;
    let mut notes = notes;
    let mut next_steps = next_steps;
    // The CLI's own commands that must run while the files are still there
    // (an uninstall reads the plugin manifest from the folder about to go).
    // A refusal here is reported, never fatal: the files still come off.
    for command in commands.iter().filter(|c| c.before_writes) {
        match run_cli(env, command) {
            Ok(Some(said)) => notes.push(said),
            Ok(None) => next_steps.push(format!("run: {command}")),
            Err(why) => next_steps.push(format!(
                "WARNING: `{command}` failed ({why}); run it by hand to finish unregistering"
            )),
        }
    }
    let applied = backup::apply(writes, &env.backups())?;
    if target == Target::CodexDesktop && verb.is_uninstall() {
        codex_desktop::cleanup(env);
    }
    // C24a: a settings file with a fatal diagnostic is discarded whole and in
    // silence. Re-read what the migration wrote and put the backup back rather
    // than report a success that voided the user's model, permissions and
    // plugins.
    if target == Target::Claude {
        if let Err(why) = claude::verify_written(env) {
            let restored = restore_all(&applied);
            return Err(format!(
                "{why}; the previous {} restored ({restored})",
                Target::Claude.display_name()
            ));
        }
    }
    // The commands that register what was just written. A failure here is
    // fatal for an install — files on disk that Claude does not load are not
    // an installed integration — and the error names the command to re-run.
    for command in commands.iter().filter(|c| !c.before_writes) {
        match run_cli(env, command) {
            Ok(Some(said)) => notes.push(said),
            Ok(None) => next_steps.push(format!("run: {command}")),
            Err(why) if verb.is_uninstall() => {
                next_steps.push(format!("WARNING: `{command}` failed ({why})"))
            }
            Err(why) => {
                return Err(format!(
                    "the plugin files are written but Claude Code did not register them: `{command}`                      failed ({why}). Fix the cause and run `deckd integrations repair {}`.",
                    target.name()
                ));
            }
        }
    }
    let mut changed = applied.changed;
    let mut backups = applied.backups;
    let mut receipt = receipt;
    // Codex `--trust` is a *second* write, made once the new `hooks.json` is
    // on disk: the hash Codex reports covers the file it can see (C14a), so a
    // hash read before the publish would be the old definition's — or, on a
    // first install, a hash for a file that does not exist yet.
    //
    // A failure here is not fatal. The hooks are installed and working in
    // every respect but trust, and rolling the install back over it would be
    // worse than saying so: the operator can still review them in `/hooks`.
    if target == Target::Codex && verb.trusts() {
        match codex::record_trust(env, receipt.as_ref()) {
            Ok(outcome) => {
                changed.extend(outcome.changed);
                backups.extend(outcome.backups);
                notes.extend(outcome.notes);
                next_steps.extend(outcome.steps);
                if let Some(receipt) = receipt.as_mut() {
                    receipt["trust_keys"] = serde_json::json!(outcome.trust_keys);
                }
            }
            Err(why) => next_steps.push(format!(
                "WARNING: --trust could not record the hashes ({why}). The hooks are installed but \
                 UNTRUSTED, so Codex skips them in silence: review them with `/hooks`, or re-run \
                 `deckd integrations repair codex --trust`."
            )),
        }
    }
    let mut receipts = Receipts::load(&env.receipts_path());
    receipts.set(target, if verb.is_uninstall() { None } else { receipt });
    receipts.save(&env.receipts_path())?;
    notes.extend(applied.notes.iter().cloned());
    notes.extend(env.warnings.iter().cloned());
    Ok(Receipt {
        target: target.name().to_string(),
        changed,
        backups,
        notes,
        conflicts,
        next_steps,
    })
}

/// Undo every publish of this run: a replaced file gets its backup back, a
/// file we created is deleted again.
///
/// Every step works on the **resolved** path, the one `backup::publish` wrote
/// through, and never on the logical one. If `~/.claude/settings.json` is a
/// symlink into a dotfiles repo, rolling back against the logical path would
/// delete the symlink and leave a regular file in its place while the
/// dotfiles file kept the content deckd wrote — the opposite of a rollback.
fn restore_all(applied: &backup::Applied) -> String {
    let mut done = 0usize;
    for published in applied.published.iter().rev() {
        let _ = std::fs::remove_file(&published.resolved);
        match &published.backup {
            None => done += 1,
            Some(backup) => {
                if std::fs::rename(backup, &published.resolved).is_ok() {
                    done += 1;
                }
            }
        }
    }
    format!("{done}/{} file(s)", applied.published.len())
}

/// `integrations.status` for every target, or one of them.
pub fn status(env: &Env, only: Option<Target>) -> Vec<Report> {
    Target::ALL
        .into_iter()
        .filter(|t| only.is_none_or(|want| want == *t))
        .map(|target| match target {
            Target::Claude => claude::status(env),
            Target::Codex => codex::status(env),
            Target::CodexDesktop => codex_desktop::status(env),
            Target::Gemini | Target::ClaudeDesktop => Report::new(target, Status::Absent)
                .with_detail(
                    "designed for v1.1 (design §5.3, §4.16); deckd writes nothing for it today",
                ),
        })
        .collect()
}

/// How long a `--version` probe may take before it is killed. `status` is on
/// the request path; a CLI that hangs must not hang the daemon with it.
const DETECT_TIMEOUT: Duration = Duration::from_secs(5);

/// `claude --version` / `codex --version`, when [`Env::probe_clis`] allows
/// it. A CLI that is not on `PATH`, does not answer, or takes too long is
/// simply not detected — it is never an error.
pub fn detect_cli(env: &Env, binary: &str) -> Option<Detected> {
    if !env.probe_clis {
        return None;
    }
    let path = which(binary)?;
    let mut child = std::process::Command::new(&path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let version = rx.recv_timeout(DETECT_TIMEOUT).ok();
    let _ = child.kill();
    let _ = child.wait();
    let version = version?.trim().to_string();
    Some(Detected {
        path,
        version: if version.is_empty() {
            "unknown".into()
        } else {
            version
        },
    })
}

/// The `integrations.*` methods (§4.11), answered on the tokio side.
///
/// They are file I/O and subprocess probes, so they run on a blocking pool
/// and never on the core thread: an installer run that waited on the core
/// would stop every agent, every device line and every other request for as
/// long as the disk took.
pub async fn api_dispatch(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, deck_model::ApiError> {
    let method = method.to_string();
    match tokio::task::spawn_blocking(move || api_call(&method, params)).await {
        Ok(result) => result,
        Err(e) => Err(internal(&format!("the integrations task failed: {e}"))),
    }
}

/// `{target?, trust?, dry_run?, detect?}`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiParams {
    #[serde(default)]
    target: Option<String>,
    /// Codex only: write `[hooks.state]` instead of printing the review step.
    #[serde(default)]
    trust: bool,
    /// Render "What this writes" and change nothing.
    #[serde(default)]
    dry_run: bool,
    /// Run the CLIs: `--version` for `detected`, and Codex's `hooks/list`
    /// for its live hook trust. Off makes `status` pure file I/O.
    #[serde(default = "yes")]
    detect: bool,
}

fn yes() -> bool {
    true
}

fn internal(why: &str) -> deck_model::ApiError {
    deck_model::ApiError::internal(why)
}

fn conflict(why: &str) -> deck_model::ApiError {
    deck_model::ApiError {
        code: deck_model::ApiError::CONFLICT,
        message: "conflict".into(),
        data: serde_json::json!({"why": why}),
    }
}

fn api_call(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, deck_model::ApiError> {
    let params: ApiParams = serde_json::from_value(if params.is_null() {
        serde_json::json!({})
    } else {
        params
    })
    .map_err(|e| deck_model::ApiError::invalid_params(&e.to_string()))?;
    let target = params
        .target
        .as_deref()
        .map(Target::parse)
        .transpose()
        .map_err(|e| deck_model::ApiError::invalid_params(&e))?;
    // The daemon's own data directory and config. A daemon started with an
    // explicit `--data-dir` is not visible from here, so `DECKD_DATA_DIR` (or
    // the platform default) is what `integrations.*` over the API resolves.
    let paths = Paths::detect().map_err(|e| internal(&e.to_string()))?;
    let config = Config::load(&paths.config).map_err(|e| internal(&e.to_string()))?;
    let mut env = Env::detect(&paths, &config).map_err(|e| conflict(&e))?;
    env.probe_clis = params.detect;
    match method {
        "status" => Ok(serde_json::json!({
            "targets": status(&env, target),
            "warnings": env.warnings,
        })),
        "install" | "uninstall" | "repair" => {
            let target = target.ok_or_else(|| {
                deck_model::ApiError::invalid_params("integrations.{method} needs a target")
            })?;
            let verb = match method {
                "uninstall" => Verb::Uninstall,
                "repair" => Verb::Repair {
                    trust: params.trust,
                },
                _ => Verb::Install {
                    trust: params.trust,
                },
            };
            let plan = plan(&env, target, verb).map_err(|e| conflict(&e))?;
            if params.dry_run {
                return Ok(serde_json::json!({
                    "target": target.name(),
                    "dry_run": true,
                    "changes": plan.changes(),
                    "notes": plan.notes,
                    "conflicts": plan.conflicts,
                    "next_steps": plan.next_steps,
                }));
            }
            let receipt = apply(&env, target, verb, plan).map_err(|e| conflict(&e))?;
            serde_json::to_value(&receipt).map_err(|e| internal(&e.to_string()))
        }
        other => Err(deck_model::ApiError::method_not_found(&format!(
            "integrations.{other}"
        ))),
    }
}

/// The first `PATH` entry holding an executable called `binary`.
pub fn which(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(candidate)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                candidate.is_file()
            }
        })
}

/// Pretty JSON with a trailing newline — the shape every JSON target is
/// written in, and the one `serde_json::to_vec_pretty` produces.
pub fn render_json(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| format!("cannot render JSON: {e}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parse a config file that must be a JSON object. A missing file is an
/// empty object (we create it); anything else is a conflict, never a
/// silent overwrite of something we failed to understand.
pub fn parse_json_object(snapshot: &backup::FileSnapshot) -> Result<serde_json::Value, String> {
    let Some(bytes) = snapshot.bytes() else {
        return Ok(serde_json::json!({}));
    };
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("{} is not valid JSON: {e}", snapshot.path().display()))?;
    if !value.is_object() {
        return Err(format!(
            "{} does not hold a JSON object",
            snapshot.path().display()
        ));
    }
    Ok(value)
}

/// The newest backup of `path` whose parsed JSON equals `want`, if any.
///
/// `uninstall` un-merges the user's file structurally, but `serde_json`
/// re-serialises the *whole* file (two-space pretty, and without the
/// `preserve_order` feature the keys come back sorted). Restoring the exact
/// bytes of the pre-install backup when the un-merged result is semantically
/// the same file is what makes an uninstall byte-for-byte rather than
/// merely equivalent.
pub fn bytes_of_equivalent_backup(
    backups: &Path,
    name: &str,
    want: &serde_json::Value,
) -> Option<Vec<u8>> {
    for candidate in backup::list_backups(backups, name) {
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if &value == want {
            return Some(bytes);
        }
    }
    None
}

/// Shared by the unit tests here and by `tests/integrations.rs`: an
/// installer environment that touches nothing outside `dir`.
#[doc(hidden)]
pub fn test_env(dir: &testdir::TestDir) -> Env {
    let exe = dir.path().join("bin/deckd");
    if let Some(parent) = exe.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write the fake deckd");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the fake deckd");
    }
    let data = dir.sub("data");
    Env {
        channel_exe: exe.clone(),
        exe,
        tokens: data.join("tokens.toml"),
        data,
        port: 47201,
        hold_s: 120,
        tool_titles: false,
        home: dir.sub("home"),
        claude_config_dir: Some(dir.sub("claude")),
        codex_home: Some(dir.sub("codex")),
        launchctl: Launchctl::Disabled,
        probe_clis: false,
        run_clis: false,
        // On: the stub written above answers every argv with exit 0, so the
        // tests run the real guard rather than a path around it.
        verify_hook_subcommand: true,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
fn fake_exe(dir: &testdir::TestDir) -> PathBuf {
    let exe = dir.path().join("bin/deckd");
    write_exe(&exe);
    exe
}

#[cfg(test)]
fn write_exe(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the bin directory");
    }
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write the fake deckd");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the fake deckd");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// `[api] port` is what the daemon asks for; `0` means "any free port".
    /// An installer that copied it would write `http://127.0.0.1:0/…` into
    /// the user's settings, which nothing can ever answer.
    #[test]
    fn the_hooks_name_the_port_the_daemon_bound_not_the_one_it_asked_for() {
        let dir = TestDir::new("bound-port");
        let port_file = dir.path().join("port");
        // The daemon is running on an ephemeral port: that is the truth.
        std::fs::write(&port_file, "52632\n").unwrap();
        assert_eq!(bound_port(&port_file, 0).unwrap(), 52632);
        assert_eq!(
            bound_port(&port_file, 47201).unwrap(),
            52632,
            "a live port beats the configured one: the config may have changed since the bind"
        );
        // No daemon yet: the configured port is the best we can do…
        std::fs::remove_file(&port_file).unwrap();
        assert_eq!(bound_port(&port_file, 47201).unwrap(), 47201);
        // …unless it is 0, which would name a port nothing answers.
        let err = bound_port(&port_file, 0).unwrap_err();
        assert!(err.contains("has not bound a port"), "{err}");
        // An empty port file is a degraded daemon (§4.1), not a port.
        std::fs::write(&port_file, "").unwrap();
        assert!(bound_port(&port_file, 0).is_err());
        assert_eq!(bound_port(&port_file, 47201).unwrap(), 47201);
    }

    use crate::integrations::testdir::TestDir;

    #[test]
    fn target_names_round_trip_and_unknown_ones_list_the_choices() {
        for target in Target::ALL {
            assert_eq!(Target::parse(target.name()), Ok(target));
        }
        let error = Target::parse("vscode").unwrap_err();
        assert!(
            error.contains("claude-desktop") && error.contains("gemini"),
            "{error}"
        );
    }

    /// Two properties, and the second is the one that was missing.
    ///
    /// The spelling: `as_str` and the serde rename are the same wire word,
    /// so a status logged by name and a status on the wire never drift.
    ///
    /// The **reachability**: every word in the vocabulary is built by some
    /// path in `integrations/`. A spelling test alone let four of §4.11's
    /// words (`hooks_not_firing`, `hooks_unreachable`, `hooks_hijack_risk`,
    /// `paused`) sit in the enum for a whole slice, published to every
    /// client through `/v1/schema` and the status rows, with nothing in the
    /// daemon able to produce one — the test passed on their spelling and
    /// stood in for coverage it never had. Two of the four came back with
    /// their path, `codex::status`'s reading of `hooks/list`; the other two
    /// are still out. This reads the three files that decide a status and
    /// fails if a word has no constructor, which is the cheapest honest
    /// check: a variant no `Status::X` names cannot be reached at runtime
    /// either.
    #[test]
    fn the_status_vocabulary_is_reachable_and_spelled_as_the_wire_spells_it() {
        let names: Vec<&str> = Status::ALL.into_iter().map(Status::as_str).collect();
        assert_eq!(
            names,
            [
                "installed",
                "outdated",
                "absent",
                "conflict",
                "path_drift",
                "hooks_not_firing",
                "paused",
                "untrusted",
                "modified",
            ]
        );
        for status in Status::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        }
        let deciders = concat!(include_str!("claude.rs"), include_str!("codex.rs"),);
        for status in Status::ALL {
            let ctor = format!("Status::{status:?}");
            assert!(
                deciders.contains(&ctor),
                "nothing in integrations/ ever builds {} — publish it with its path, not before",
                status.as_str()
            );
        }
    }

    /// Tested on the pure half: a test that set `CLAUDE_CONFIG_DIR` would
    /// change it for every other test in the binary.
    #[test]
    fn a_relative_config_dir_override_is_refused_and_an_empty_one_is_unset() {
        use std::ffi::OsStr;
        assert_eq!(
            env_path("CLAUDE_CONFIG_DIR", Some(OsStr::new("relative/claude"))),
            Err("CLAUDE_CONFIG_DIR must be an absolute path".into())
        );
        assert_eq!(env_path("CODEX_HOME", Some(OsStr::new(""))), Ok(None));
        assert_eq!(env_path("CODEX_HOME", None), Ok(None));
        assert_eq!(
            env_path("CODEX_HOME", Some(OsStr::new("/tmp/codex"))),
            Ok(Some(PathBuf::from("/tmp/codex")))
        );
    }

    #[test]
    fn a_translocated_binary_is_refused_and_one_outside_applications_only_warns() {
        let dir = TestDir::new("env-translocation");
        let paths = Paths::from_data_dir(
            dir.path().to_path_buf(),
            dir.path().join("logs"),
            dir.path().join("caches"),
        );
        let translocated = dir.sub("AppTranslocation/X/d/OpenWalkieTalkie.app/Contents/MacOS");
        let exe = translocated.join("deckd");
        write_exe(&exe);
        let error = Env::with_exe(exe, &paths, &Config::default()).unwrap_err();
        if cfg!(target_os = "macos") {
            assert!(error.contains("App Translocation"), "{error}");
        }
        let env = Env::with_exe(fake_exe(&dir), &paths, &Config::default()).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
            assert!(env.warnings[0].contains("outside /Applications"));
        }
        assert!(
            !env.exe.as_os_str().is_empty(),
            "a warning is not a refusal (C22)"
        );
    }

    #[test]
    fn a_non_executable_or_missing_binary_is_refused() {
        let dir = TestDir::new("env-exe");
        let paths = Paths::from_data_dir(
            dir.path().to_path_buf(),
            dir.path().join("logs"),
            dir.path().join("caches"),
        );
        let error = Env::with_exe(dir.path().join("nope"), &paths, &Config::default()).unwrap_err();
        assert!(error.contains("cannot inspect"), "{error}");
        let plain = dir.write("plain", "not executable");
        let error = Env::with_exe(plain, &paths, &Config::default()).unwrap_err();
        #[cfg(unix)]
        assert!(error.contains("is not executable"), "{error}");
        #[cfg(not(unix))]
        let _ = error;
    }

    /// The C24a rollback has to undo the publish where the publish happened.
    /// `backup::publish` writes through the *resolved* path — that is the
    /// documented property that editing a symlinked `settings.json` edits its
    /// target and leaves the symlink alone — so a rollback against the
    /// logical path would delete the user's symlink, drop a regular file in
    /// its place, and leave the dotfiles file holding what deckd wrote.
    #[cfg(unix)]
    #[test]
    fn a_rollback_puts_the_backup_back_behind_a_symlink_and_keeps_the_symlink() {
        use std::os::unix::fs::MetadataExt;
        let dir = TestDir::new("rollback-symlink");
        let real = dir.path().join("dotfiles/settings.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, b"{\"model\":\"mine\"}\n").unwrap();
        let inode = std::fs::metadata(&real).unwrap().ino();
        let link = dir.path().join("claude/settings.json");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // A file deckd creates outright, to cover the other arm of the undo.
        let fresh = dir.path().join("claude/hooks.json");

        let backups = dir.path().join("backups");
        let applied = backup::apply(
            vec![
                backup::Planned::write(
                    backup::FileSnapshot::read(&link).unwrap(),
                    b"{\"model\":\"deckd\"}\n".to_vec(),
                ),
                backup::Planned::write(
                    backup::FileSnapshot::read(&fresh).unwrap(),
                    b"{}\n".to_vec(),
                ),
            ],
            &backups,
        )
        .unwrap();
        assert_eq!(std::fs::read(&real).unwrap(), b"{\"model\":\"deckd\"}\n");

        assert_eq!(restore_all(&applied), "2/2 file(s)");
        assert!(
            std::fs::symlink_metadata(&link).unwrap().is_symlink(),
            "the rollback must not replace the symlink with a regular file"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"{\"model\":\"mine\"}\n",
            "the file the symlink points at is the one that comes back"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().ino(),
            inode,
            "and it is the very same file, because the backup was a rename"
        );
        assert!(!fresh.exists(), "a file deckd created is deleted again");
    }

    #[test]
    fn receipts_survive_a_round_trip_and_an_unreadable_file() {
        let dir = TestDir::new("env-receipts");
        let path = dir.path().join("integrations.json");
        let mut receipts = Receipts::load(&path);
        assert!(receipts.get(Target::Codex).is_none());
        receipts.set(Target::Codex, Some(serde_json::json!({"trust": ["k"]})));
        receipts.save(&path).unwrap();
        let reloaded = Receipts::load(&path);
        assert_eq!(reloaded.get(Target::Codex).unwrap()["trust"][0], "k");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Receipts::load(&path).get(Target::Codex).is_none());
    }
}
