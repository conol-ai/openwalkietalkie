//! Windows' stand-in for the macOS launch agent: this app, run as
//! `OpenWalkieTalkie.exe --service`, keeps `deckd run` going.
//!
//! launchd runs the agent with `KeepAlive = {SuccessfulExit = false}` (see
//! `deckd::service`): a clean exit (0: `service.stop`, or a second copy
//! standing down because the service already runs) stays stopped, and every
//! other exit, the 75 of `service.restart` and any crash among them, is run
//! again. The host does the same, so Service › Restart, a configuration that
//! needs a restart and an update all work as they do on a Mac.
//!
//! The host is a GUI-subsystem process (this app's), so it opens no window
//! of its own, and it starts `deckd` with `CREATE_NO_WINDOW`, so neither does
//! the service. Its sign-in start is the Run value `login_item` writes.
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The argument that makes this app the service's host instead of a window.
pub const FLAG: &str = "--service";

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// A run this long was a working service, not a crash loop: the wait before
/// the next start goes back to its shortest.
const HEALTHY: Duration = Duration::from_secs(60);
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(30);

/// Whether this process was started as the service's host.
#[must_use]
pub fn requested() -> bool {
    std::env::args_os().nth(1).is_some_and(|arg| arg == FLAG)
}

/// Run `deckd run` until it exits cleanly; returns the exit code for this
/// process. Without a `deckd.exe` beside this app there is nothing to host.
#[must_use]
pub fn supervise() -> i32 {
    let Some(deckd) = crate::app::deckd_path() else {
        return 1;
    };
    let mut wait = FIRST_WAIT;
    loop {
        let started = Instant::now();
        match run_once(&deckd) {
            // A clean exit: stopped on purpose, or another copy already runs.
            Ok(Some(0)) => return 0,
            Ok(_) => {}
            // deckd.exe cannot be started at all: no amount of retrying helps.
            Err(_) => return 1,
        }
        if started.elapsed() >= HEALTHY {
            wait = FIRST_WAIT;
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}

/// One `deckd run`, to its exit code (`None`: ended without one).
fn run_once(deckd: &Path) -> std::io::Result<Option<i32>> {
    Command::new(deckd)
        .arg("run")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.code())
}

/// Start the host (and so the service) in the background, as the Start
/// button and a new sign-in registration do. A service that already runs is
/// left alone: its lock makes the new copy stand down, and the host with it.
pub fn start() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if crate::app::deckd_path().is_none() {
        return Err("there is no deckd beside this app".into());
    }
    Command::new(exe)
        .arg(FLAG)
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Stop the service: `service.stop` over its own door, a clean exit the host
/// does not restart. A service that is not running is already stopped.
pub fn stop() {
    let Some(deckd) = crate::app::deckd_path() else {
        return;
    };
    let _ = Command::new(deckd)
        .args(["ctl", "--scopes", "admin", "--timeout", "5", "service.stop"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
