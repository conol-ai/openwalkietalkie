//! What `launchctl print gui/<uid>/<label>` says, and what it means
//! (design §9, §10.5; HOST-CONTRACT-W7 §4).
//!
//! launchd is the only place two facts are recorded, and neither reaches any
//! log file we own:
//!
//! * **the last exit status.** It is the daemon's own vocabulary read back:
//!   `service.restart` exits 75 and `service.stop` exits 0 (`service::EXIT_*`,
//!   week 6). Those numbers only *mean* anything because week 6 also changed
//!   the installed plist to `KeepAlive = {SuccessfulExit: false}`. Under the
//!   `KeepAlive = true` it had before, launchd restarted the job on **every**
//!   exit, so "75" and "0" were indistinguishable in effect and the operator
//!   could not tell an update restart from a deliberate stop from a crash
//!   loop. [`Agent::exit_meaning`] therefore reads the semaphore too and says
//!   so when it is not the one the codes were written for.
//! * **a spawn that never happened.** A LaunchAgent whose program is gone —
//!   the app trashed, moved, or still translocated out of a quarantined DMG —
//!   sits in `state = spawn scheduled` with `last exit code = 78: EX_CONFIG`
//!   for ever. The service looks installed (`launchctl print` succeeds, the
//!   plist is there, `SMAppService.status` is `enabled`) and never starts, and
//!   nothing in the daemon's own logs can say so because the daemon never ran.
//!   [`Agent::dangling`] is that check, and it works from **any** copy of the
//!   binary — a fresh download in `~/Downloads` can diagnose the registration
//!   left behind by the copy that was trashed.
//!
//! Everything here parses text and looks at the filesystem; running
//! `launchctl` at all is [`print`], which the tests never call.
//!
//! The shapes below were measured on this machine on 2026-09-22 against a
//! throwaway `ai.cohdl.openwalkietalkie.doctorprobe` agent:
//!
//! ```text
//! state = spawn scheduled       program missing   last exit code = 78: EX_CONFIG
//! state = running               after exit 75     last exit code = 75: EX_TEMPFAIL, runs = 2
//! state = not running           after exit 0      last exit code = 0,               runs = 3
//! state = spawn scheduled       after SIGKILL     last terminating signal = Killed: 9
//! (never started)                                 last exit code = (never exited)
//! ```
//!
//! and `KeepAlive = {SuccessfulExit: false}` prints
//! `semaphores = { successful exit => 0 }`, where a plain `KeepAlive = true`
//! prints no `semaphores` block at all (its `properties` line says
//! `keepalive` instead).
use std::path::{Path, PathBuf};

/// The daemon's LaunchAgent (design D16).
pub const DECKD_LABEL: &str = "ai.cohdl.openwalkietalkie.deckd";
/// The same agent as installed before the project was renamed from
/// OpenMicroDial; `deckd uninstall` takes it out too.
pub const LEGACY_DECKD_LABEL: &str = "com.conol.openmicrodial.deckd";

/// This process's uid, the domain a LaunchAgent lives in.
pub fn uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` takes no arguments, cannot fail and never writes.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// `launchctl print gui/<uid>/<label>`: `Some(output)` when the agent is
/// loaded, `None` when it is not (or `launchctl` is unavailable).
pub fn print(label: &str) -> Option<String> {
    let out = std::process::Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{}/{label}", uid())])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A field of `launchctl print` output (`key = value`), first match wins —
/// which is the top-level one, because the nested blocks (`resource
/// coalition`, `jetsam coalition`) repeat `state` further down.
pub fn field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(key)
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .map(str::trim)
    })
}

/// How the job last ended, as launchd recorded it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LastExit {
    /// `last exit code = (never exited)`, or no record at all.
    #[default]
    Never,
    /// `last exit code = 75: EX_TEMPFAIL` → `75`.
    Code(i32),
    /// `last terminating signal = Killed: 9` → `"Killed: 9"`.
    Signal(String),
}

impl LastExit {
    /// The two lines launchd may print, in the order it prefers them: a job
    /// that died on a signal has no exit code line at all.
    fn parse(output: &str) -> LastExit {
        if let Some(raw) = field(output, "last exit code") {
            let digits: String = raw.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(code) = digits.parse::<i32>() {
                return LastExit::Code(code);
            }
        }
        match field(output, "last terminating signal") {
            Some(signal) if !signal.is_empty() => LastExit::Signal(signal.to_string()),
            _ => LastExit::Never,
        }
    }
}

/// One LaunchAgent as launchd sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    pub label: String,
    /// `path = …`: the plist launchd loaded. An SMAppService registration
    /// has no file here — launchd prints `(submitted by smd.N)` — so that
    /// is `None`, with [`Agent::submitted`] set instead.
    pub plist: Option<PathBuf>,
    /// Registered by the app through SMAppService (`path = (submitted by
    /// smd.N)`), not from a plist file.
    pub submitted: bool,
    /// `state = running` / `not running` / `spawn scheduled`.
    pub state: String,
    pub pid: Option<u32>,
    /// `program = …`, absolute — launchd resolves `BundleProgram` for us.
    pub program: Option<PathBuf>,
    pub runs: Option<u64>,
    pub last_exit: LastExit,
    /// `semaphores = { successful exit => 0 }`, i.e. `KeepAlive =
    /// {SuccessfulExit: false}`: the mode the exit codes are written for.
    pub keepalive_on_failure_only: bool,
}

impl Agent {
    /// Parse one `launchctl print` output.
    pub fn parse(label: &str, output: &str) -> Agent {
        Agent {
            label: label.to_string(),
            plist: field(output, "path")
                .filter(|p| !p.starts_with('('))
                .map(PathBuf::from),
            submitted: field(output, "path").is_some_and(|p| p.starts_with("(submitted by")),
            state: field(output, "state").unwrap_or("?").to_string(),
            pid: field(output, "pid").and_then(|p| p.parse().ok()),
            program: field(output, "program").map(PathBuf::from),
            runs: field(output, "runs").and_then(|r| r.parse().ok()),
            last_exit: LastExit::parse(output),
            keepalive_on_failure_only: successful_exit_semaphore(output),
        }
    }

    /// Read the agent launchd currently holds under `label`.
    pub fn load(label: &str) -> Option<Agent> {
        let mut agent = print(label).map(|out| Agent::parse(label, &out))?;
        // An SMAppService job names only its `program identifier`, relative
        // to a bundle launchd does not print: the running pid says which
        // copy of deckd it actually is.
        if agent.program.is_none() {
            agent.program = agent.pid.and_then(crate::trust::exe_path);
        }
        Some(agent)
    }

    /// The last exit **and what it means**, in the daemon's own vocabulary.
    ///
    /// The `KeepAlive` mode is half the sentence: 75 and 0 are a restart
    /// request and a deliberate stop only under `{SuccessfulExit: false}`.
    /// Under a plain `KeepAlive = true` — what `tools/install-host.sh` wrote
    /// before week 6 — launchd runs the job again whatever it exited with, so
    /// the same numbers describe nothing the operator can act on, and this
    /// says which plist they are looking at rather than reading a meaning
    /// into a number that does not carry one.
    pub fn exit_meaning(&self) -> String {
        let honoured = self.keepalive_on_failure_only;
        match &self.last_exit {
            LastExit::Never => "has not exited since it was loaded".into(),
            LastExit::Code(code) if *code == crate::service::EXIT_RESTART => format!(
                "exit {code} (EX_TEMPFAIL): it asked to be restarted — `service.restart`, which is \
                 what a Sparkle update calls so the new bundle's daemon takes over{}",
                if honoured {
                    ""
                } else {
                    ", but this plist has no `KeepAlive = {SuccessfulExit: false}`, so launchd \
                     would have restarted it on any code at all"
                }
            ),
            LastExit::Code(0) if honoured => "exit 0: it was stopped on purpose — `service.stop`, \
                 or a second instance finding the lock held — and `KeepAlive = {SuccessfulExit: \
                 false}` leaves it stopped until something starts it again"
                .into(),
            LastExit::Code(0) => "exit 0: a clean exit, but this plist restarts the job on any \
                 exit (`KeepAlive` is not `{SuccessfulExit: false}`), so the code says nothing \
                 about whether it meant to stop — re-run tools/install-host.sh"
                .into(),
            // 78 is launchd's own, not ours: it is what it reports when it
            // could not execute the program at all, which is the dangling
            // registration below.
            LastExit::Code(78) => "exit 78 (EX_CONFIG): launchd could not spawn the program at \
                 all — see the dangling-agent check below"
                .into(),
            LastExit::Code(code) => format!(
                "exit {code}: a crash — it neither asked to restart (75) nor stopped on purpose \
                 (0), so the last thing it did is in the logs, and `deckd diagnostics` collects \
                 them"
            ),
            LastExit::Signal(signal) => format!(
                "killed by a signal ({signal}): a crash — nothing wrote an exit code, so the \
                 launchd log (`deckd.launchd.log`) is the only record"
            ),
        }
    }

    /// Why this registration can never start, if it cannot: the state that
    /// makes a service look installed and do nothing (§10.5).
    ///
    /// Three shapes, all of them a missing file in the end, but each worth
    /// its own sentence because the operator's next move differs: the program
    /// is gone (re-install), the program is inside App Translocation (move
    /// the app to /Applications and reopen it), or the plist launchd loaded
    /// is itself gone (unregister).
    pub fn dangling(&self) -> Option<String> {
        if let Some(program) = &self.program {
            if let Some(why) = program_dangling(program) {
                return Some(format!(
                    "{}: {why} — launchd keeps retrying it, so the service looks installed and \
                     never starts. Re-install from the app (Service → Start service), or run \
                     `launchctl bootout gui/{}/{}` to clear it.",
                    program.display(),
                    uid(),
                    self.label
                ));
            }
        }
        if let Some(plist) = &self.plist {
            if !plist.exists() {
                return Some(format!(
                    "the plist launchd loaded ({}) is gone — the registration outlived the bundle \
                     that carried it. `launchctl bootout gui/{}/{}` clears it.",
                    plist.display(),
                    uid(),
                    self.label
                ));
            }
        }
        None
    }
}

/// Why `program` cannot be executed, if it cannot.
///
/// Translocation is checked before existence because a translocated path
/// *does* exist while the read-only mount is up: the operator's problem is
/// not a missing file but a path that disappears the moment the mount goes,
/// and "move it to /Applications" is a different instruction from
/// "re-install".
pub fn program_dangling(program: &Path) -> Option<String> {
    let text = program.to_string_lossy();
    if text.contains("/AppTranslocation/") {
        return Some(
            "it is running from macOS App Translocation, a read-only copy that vanishes when the \
             quarantined image is unmounted"
                .into(),
        );
    }
    if !program.exists() {
        // An app the operator trashed takes its whole `Contents/` with it,
        // and naming the bundle is more useful than naming the Mach-O
        // three levels inside it.
        if let Some(app) = app_bundle_of(program) {
            if !app.exists() {
                return Some(format!(
                    "the app that contained it ({}) is gone",
                    app.display()
                ));
            }
        }
        return Some("the program it runs no longer exists".into());
    }
    None
}

/// The `.app` a path sits inside, if any: `…/Foo.app/Contents/MacOS/deckd`
/// → `…/Foo.app`.
pub fn app_bundle_of(program: &Path) -> Option<PathBuf> {
    program
        .ancestors()
        .find(|a| a.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// Is there a `semaphores = { successful exit => 0 }` block, i.e. `KeepAlive
/// = {SuccessfulExit: false}`? A plain `KeepAlive = true` prints no
/// `semaphores` block at all.
fn successful_exit_semaphore(output: &str) -> bool {
    let mut inside = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("semaphores") && line.ends_with('{') {
            inside = true;
            continue;
        }
        if inside {
            if line == "}" {
                return false;
            }
            if let Some(value) = line.strip_prefix("successful exit") {
                return value.trim_start().strip_prefix("=>").is_some_and(|v| {
                    // `=> 0` is "the job may run only when the last exit was
                    // **not** successful".
                    v.trim() == "0"
                });
            }
        }
    }
    false
}

/// A plist on disk that launchd has **not** loaded: the other half of the
/// dangling check, because a bundle the operator trashed also takes the
/// registration's `launchctl print` entry with it on the next login and
/// leaves only `~/Library/LaunchAgents/<label>.plist` behind (the `--dev`
/// install's shape).
///
/// `BundleProgram` is relative to the enclosing app bundle, which is
/// resolvable exactly when the plist itself lives inside one
/// (`<app>/Contents/Library/LaunchAgents/…`, §10.1) — for a copy in
/// `~/Library/LaunchAgents` there is no bundle to resolve against and this
/// says so rather than guessing.
pub fn program_of_plist(plist: &Path) -> Result<PathBuf, String> {
    for key in ["Program", "ProgramArguments.0", "BundleProgram"] {
        let out = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-", &plist.to_string_lossy()])
            .output()
            .map_err(|e| format!("cannot run plutil: {e}"))?;
        if !out.status.success() {
            continue;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if value.is_empty() {
            continue;
        }
        let path = PathBuf::from(&value);
        if path.is_absolute() {
            return Ok(path);
        }
        // `BundleProgram`: `<app>/Contents/Library/LaunchAgents/x.plist` is
        // three directories under `Contents`, so the bundle root is four up.
        return match plist
            .ancestors()
            .nth(4)
            .filter(|a| a.extension().is_some_and(|e| e == "app"))
        {
            Some(app) => Ok(app.join(&value)),
            None => Err(format!(
                "{} names a relative {key} ({value}) and does not sit inside an app bundle, so \
                 there is nothing to resolve it against",
                plist.display()
            )),
        };
    }
    Err(format!(
        "{} names no Program, ProgramArguments or BundleProgram",
        plist.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `launchctl print` output, in the layout the real one uses (tabs,
    /// nested blocks that repeat `state`).
    fn print_output(body: &str) -> String {
        format!("gui/501/ai.cohdl.openwalkietalkie.deckd = {{\n{body}\n\tresource coalition = {{\n\t\tstate = active\n\t}}\n}}\n")
    }

    #[test]
    fn the_top_level_field_wins_over_the_same_name_in_a_nested_block() {
        let out = print_output("\tstate = running\n\tpid = 4242\n\tprogram = /Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
        let agent = Agent::parse(DECKD_LABEL, &out);
        assert_eq!(agent.state, "running");
        assert_eq!(agent.pid, Some(4242));
        assert_eq!(
            agent.program.as_deref(),
            Some(Path::new(
                "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"
            ))
        );
    }

    #[test]
    fn seventy_five_is_a_restart_request_and_zero_is_a_deliberate_stop() {
        let keepalive = "\tsemaphores = {\n\t\tsuccessful exit => 0\n\t}";
        let restart = Agent::parse(
            DECKD_LABEL,
            &print_output(&format!(
                "\tstate = running\n\truns = 2\n\tlast exit code = 75: EX_TEMPFAIL\n{keepalive}"
            )),
        );
        assert_eq!(restart.last_exit, LastExit::Code(75));
        assert!(restart.keepalive_on_failure_only);
        let meaning = restart.exit_meaning();
        assert!(meaning.contains("asked to be restarted"), "{meaning}");
        assert!(meaning.contains("service.restart"), "{meaning}");

        let stopped = Agent::parse(
            DECKD_LABEL,
            &print_output(&format!(
                "\tstate = not running\n\truns = 3\n\tlast exit code = 0\n{keepalive}"
            )),
        );
        assert_eq!(stopped.last_exit, LastExit::Code(0));
        let meaning = stopped.exit_meaning();
        assert!(meaning.contains("stopped on purpose"), "{meaning}");
        assert!(meaning.contains("leaves it stopped"), "{meaning}");
    }

    /// The whole reason week 6 changed the plist: under `KeepAlive = true`
    /// launchd restarts on every exit, so a 0 means nothing the operator can
    /// act on and the check has to say that instead of reading a meaning in.
    #[test]
    fn a_plain_keepalive_makes_the_same_codes_meaningless_and_doctor_says_so() {
        let agent = Agent::parse(
            DECKD_LABEL,
            &print_output(
                "\tstate = running\n\tlast exit code = 0\n\tproperties = keepalive | runatload",
            ),
        );
        assert!(!agent.keepalive_on_failure_only);
        let meaning = agent.exit_meaning();
        assert!(meaning.contains("says nothing"), "{meaning}");
        assert!(meaning.contains("install-host.sh"), "{meaning}");
    }

    #[test]
    fn a_crash_is_anything_else_and_a_signal_leaves_no_code_at_all() {
        let crashed = Agent::parse(
            DECKD_LABEL,
            &print_output("\tstate = not running\n\tlast exit code = 2"),
        );
        assert_eq!(crashed.last_exit, LastExit::Code(2));
        assert!(crashed.exit_meaning().contains("a crash"));

        let killed = Agent::parse(
            DECKD_LABEL,
            &print_output("\tstate = spawn scheduled\n\tlast terminating signal = Killed: 9"),
        );
        assert_eq!(killed.last_exit, LastExit::Signal("Killed: 9".into()));
        let meaning = killed.exit_meaning();
        assert!(meaning.contains("Killed: 9"), "{meaning}");
        assert!(meaning.contains("deckd.launchd.log"), "{meaning}");

        let fresh = Agent::parse(
            DECKD_LABEL,
            &print_output("\tstate = running\n\tlast exit code = (never exited)"),
        );
        assert_eq!(fresh.last_exit, LastExit::Never);
        assert!(fresh.exit_meaning().contains("has not exited"));
    }

    #[test]
    fn a_program_that_is_gone_is_dangling_and_names_the_app_that_carried_it() {
        let dir = crate::integrations::testdir::TestDir::new("launchd-dangling");
        let app = dir.path().join("Trashed.app");
        let program = app.join("Contents/MacOS/deckd");
        let plist = dir.write("loaded.plist", "<plist/>");
        let agent = Agent::parse(
            DECKD_LABEL,
            &print_output(&format!(
                "\tpath = {}\n\tstate = spawn scheduled\n\tlast exit code = 78: EX_CONFIG\n\tprogram = {}",
                plist.display(),
                program.display()
            )),
        );
        let why = agent.dangling().expect("the program is gone");
        assert!(why.contains("Trashed.app"), "{why}");
        assert!(why.contains("is gone"), "{why}");
        assert!(why.contains("launchctl bootout"), "{why}");
        // 78 is launchd's own code for it, and the exit line points at the
        // dangling check rather than calling it a crash.
        assert!(agent.exit_meaning().contains("could not spawn"));

        // The same program, present: nothing dangles.
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        let agent = Agent::parse(
            DECKD_LABEL,
            &print_output(&format!(
                "\tpath = {}\n\tstate = running\n\tprogram = {}",
                plist.display(),
                program.display()
            )),
        );
        assert_eq!(agent.dangling(), None);
    }

    #[test]
    fn a_translocated_program_is_dangling_even_though_the_path_exists() {
        let why = program_dangling(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/OpenWalkieTalkie.app/Contents/MacOS/deckd",
        ))
        .expect("translocated");
        assert!(why.contains("App Translocation"), "{why}");
    }

    /// The registration can outlive the plist: the app is trashed, the job
    /// stays in the domain, and `path` names a file that is no longer there.
    #[test]
    fn a_loaded_job_whose_plist_is_gone_is_dangling_too() {
        let dir = crate::integrations::testdir::TestDir::new("launchd-plist");
        let program = dir.write("deckd", "#!/bin/sh\n");
        let agent = Agent::parse(
            DECKD_LABEL,
            &print_output(&format!(
                "\tpath = {}\n\tstate = spawn scheduled\n\tprogram = {}",
                dir.path().join("gone.plist").display(),
                program.display()
            )),
        );
        let why = agent.dangling().expect("the plist is gone");
        assert!(why.contains("gone.plist"), "{why}");
        assert!(why.contains("outlived the bundle"), "{why}");
    }

    /// An app-registered job has no plist file: `(submitted by smd.N)` is
    /// launchd's marker for one, not a path that went missing.
    #[test]
    fn an_app_registered_job_is_not_a_dangling_plist() {
        let agent = Agent::parse(
            DECKD_LABEL,
            &print_output(
                "\tpath = (submitted by smd.40195)\n\tstate = running\n\tprogram identifier = Contents/MacOS/deckd (mode: 2)\n\tpid = 50557",
            ),
        );
        assert!(agent.submitted);
        assert_eq!(agent.plist, None);
        assert_eq!(agent.program, None, "parse names no program; `load` reads the pid's");
        assert_eq!(agent.dangling(), None);
    }

    #[test]
    fn a_bundle_program_resolves_against_the_app_the_plist_sits_in() {
        let dir = crate::integrations::testdir::TestDir::new("launchd-bundleprogram");
        let plist = dir.path().join(
            "OpenWalkieTalkie.app/Contents/Library/LaunchAgents/ai.cohdl.openwalkietalkie.deckd.plist",
        );
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(
            &plist,
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
              <key>Label</key><string>ai.cohdl.openwalkietalkie.deckd</string>\
              <key>BundleProgram</key><string>Contents/MacOS/deckd</string>\
              </dict></plist>\n",
        )
        .unwrap();
        assert_eq!(
            program_of_plist(&plist).unwrap(),
            dir.path()
                .join("OpenWalkieTalkie.app/Contents/MacOS/deckd")
                .to_path_buf()
        );
        // The same key in a plist that is not inside a bundle has nothing to
        // resolve against, and says so rather than inventing a root.
        let loose = dir.path().join("loose.plist");
        std::fs::copy(&plist, &loose).unwrap();
        let err = program_of_plist(&loose).unwrap_err();
        assert!(err.contains("nothing to resolve it against"), "{err}");
    }

    #[test]
    fn an_absolute_program_argument_is_taken_as_written() {
        let dir = crate::integrations::testdir::TestDir::new("launchd-programargs");
        let plist = dir.path().join("dev.plist");
        std::fs::write(
            &plist,
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
              <key>ProgramArguments</key><array><string>/opt/deckd</string><string>run</string></array>\
              </dict></plist>\n",
        )
        .unwrap();
        assert_eq!(
            program_of_plist(&plist).unwrap(),
            PathBuf::from("/opt/deckd")
        );
    }
}
