//! **Guard 3** (HOST-CONTRACT-W5 §1 M9, M10; design §5.5): is the agent's
//! recorded CLI pid the foreground process group of the tty we recorded for
//! it?
//!
//! What this stops: a `claude` that has exited leaves a bare `zsh` on the
//! same tty. The tab is still there, the pin is still on it, the resolution
//! is still cached — and `input text "restart the build"` + `send key
//! "enter"` is then a **command the shell runs**. Guard 3 is the difference
//! between a dictation that is ignored and a dictation that is executed.
//!
//! **`tcgetpgrp` is not viable and must not come back** (M9). Opening
//! another session's `/dev/ttysNNN` and calling `tcgetpgrp` on it answers
//! **ENOTTY (errno 25)** on macOS, because the tty is not *our* controlling
//! terminal. It is the obvious call, it is in the design, and it returns an
//! error every single time this daemon would make it.
//!
//! **Three parts, not two** (M10). A pgid-only test — "is this pid the
//! foreground pgrp of *a* terminal?" — passes for a pid that is the
//! foreground of a **different** terminal, which is precisely the aim-at-
//! the-wrong-tab case guard 3 exists for. So the check is:
//!
//! 1. the pid is **alive**,
//! 2. `pgid == tpgid` — it is the foreground process group of its own
//!    controlling terminal,
//! 3. `tdev == st_rdev` of the recorded tty — and that terminal is the one
//!    we recorded.
//!
//! `scratchpad/w5/probes/rt/fgcheck.c` is the reference, proven on four
//! cases (foreground CLI → allow; alive but not foreground → refuse; dead →
//! refuse; right pid, wrong tty → refuse).
//!
//! **One deliberate deviation from §1, and it is a narrowing.** §1 says
//! `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, pid)` → `kinfo_proc` and
//! `kp_eproc.{e_pgid, e_tpgid, e_tdev}`. **`libc` 0.2.189 declares no
//! `kinfo_proc`, no `eproc` and no `extern_proc` for any Apple target**
//! (checked in the vendored source), so that route would mean hand-rolling
//! a large, union-bearing, layout-sensitive `#[repr(C)]` struct and being
//! silently wrong if one padding byte moved. `proc_pidinfo(pid,
//! PROC_PIDTBSDINFO, …)` → [`libc::proc_bsdinfo`] **is** declared, and it
//! carries `pbi_pgid`, `e_tpgid` and `e_tdev` — the same three kernel
//! fields, read from the same `p_pgrp`/`t_pgrp`/`t_dev` the `ps` `+` flag
//! comes from, with a layout `libc` maintains instead of one this file
//! guesses. No fork either way; that was the point of the rule.
use std::fmt;

/// What guard 3 decided, and why.
///
/// A verdict and not a `bool`, because the *reason* reaches the operator:
/// "the CLI exited" and "that pid belongs to another tab" call for
/// different things, and `order.failed{reason:"not_foreground"}` is the one
/// word both share on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Alive, foreground, and on the recorded tty. The only value that
    /// permits a keystroke.
    Foreground,
    /// The pid is gone (`ESRCH`): the CLI exited, crashed or was killed.
    Dead,
    /// Alive, but its controlling terminal's foreground group is somebody
    /// else's — §15's "the CLI exited to a shell" row, from the *other*
    /// side: the shell is foreground and the CLI is not.
    Background,
    /// Alive and foreground, **of another terminal** (M10). The pgid-only
    /// test the design would have shipped says yes to this one.
    OtherTty,
    /// The pid has no controlling terminal at all (a daemon, a `setsid`
    /// child).
    NoTty,
    /// The recorded tty could not be `stat`ed: the device node is gone
    /// because the tab was closed.
    TtyGone,
    /// The kernel would not answer. Inconclusive — and treated as a
    /// refusal, because guard 3 is the one check whose failure mode has to
    /// be "send nothing".
    Unknown,
}

impl Verdict {
    /// The one question the injector asks.
    #[must_use]
    pub const fn allows(self) -> bool {
        matches!(self, Verdict::Foreground)
    }

    /// The stable word for the log and for `integrations.status`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Verdict::Foreground => "foreground",
            Verdict::Dead => "dead",
            Verdict::Background => "background",
            Verdict::OtherTty => "other_tty",
            Verdict::NoTty => "no_tty",
            Verdict::TtyGone => "tty_gone",
            Verdict::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The seam guard 3 is asked through.
///
/// It exists so [`super::inject`] can be tested against every verdict
/// without arranging a live tty in the right state for each one — and so
/// the *ordering* claim ("checked again immediately before `send key`") can
/// be asserted by counting calls, which is the only way to prove it.
pub trait Foreground: Send + Sync + std::fmt::Debug {
    fn check(&self, pid: u32, tty: &str) -> Verdict;
}

/// The real check.
#[derive(Clone, Copy, Debug, Default)]
pub struct Procs;

impl Foreground for Procs {
    fn check(&self, pid: u32, tty: &str) -> Verdict {
        check(pid, tty)
    }
}

/// `proc_bsdinfo::e_tdev` for a process with no controlling terminal.
pub const NODEV: u32 = u32::MAX;

/// The decision of M9/M10, as a pure function of the four kernel numbers.
///
/// It is separate from [`check`] because the row that matters most —
/// M10's, the pid that **is** the foreground group of a **different**
/// terminal — cannot be arranged from a test process that has no
/// controlling terminal of its own, and the gate, CI and launchd all run
/// without one. Behind `proc_pidinfo` that clause is unfalsifiable there:
/// `e_tdev` is `NODEV`, the `NoTty` arm answers first, and the comparison
/// the whole narrowing exists for is never reached. Deleting
/// `tdev == recorded_rdev` from the old inlined form left every test in
/// this file green. Out here every row is one call.
#[must_use]
pub const fn verdict(recorded_rdev: u32, tdev: u32, pgid: u32, tpgid: u32) -> Verdict {
    // Checked before the comparison, so "no tty" is never reported as
    // "other tty".
    if tdev == NODEV {
        return Verdict::NoTty;
    }
    match (tdev == recorded_rdev, pgid == tpgid) {
        (true, true) => Verdict::Foreground,
        (true, false) => Verdict::Background,
        (false, _) => Verdict::OtherTty,
    }
}

/// The three-part check of M9/M10, as one call and no fork.
#[cfg(target_os = "macos")]
#[must_use]
pub fn check(pid: u32, tty: &str) -> Verdict {
    let Some(rdev) = tty_rdev(tty) else {
        return Verdict::TtyGone;
    };
    let Some(info) = bsdinfo(pid) else {
        // `proc_pidinfo` answers 0 bytes for a pid that is not there. It
        // also answers 0 for a pid we may not look at, which on this
        // daemon's own uid does not happen — and if it ever did, refusing
        // is the right side to be wrong on.
        return Verdict::Dead;
    };
    verdict(rdev, info.e_tdev, info.pbi_pgid, info.e_tpgid)
}

/// §13's Linux port needs `/proc/<pid>/stat` field 8 instead; until then
/// the honest answer is that this build cannot tell, which refuses.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn check(_pid: u32, _tty: &str) -> Verdict {
    Verdict::Unknown
}

/// `st_rdev` of a tty device node, or `None` when it is gone.
#[cfg(target_os = "macos")]
fn tty_rdev(tty: &str) -> Option<u32> {
    // `/dev/ttys004`, which is what the hook helper records, and a bare
    // `ttys004`, which is what `ps -t` takes — both resolve, because a
    // `Terminal.tty` that lost its prefix somewhere is not a reason to send
    // a keystroke nobody checked.
    let path = if tty.starts_with('/') {
        std::path::PathBuf::from(tty)
    } else {
        std::path::Path::new("/dev").join(tty)
    };
    let meta = std::fs::metadata(path).ok()?;
    use std::os::unix::fs::MetadataExt;
    Some(meta.rdev() as u32)
}

/// `proc_pidinfo(pid, PROC_PIDTBSDINFO, …)`.
#[cfg(target_os = "macos")]
fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    // SAFETY: a zeroed `proc_bsdinfo` is a valid value of a plain-old-data
    // struct, and `proc_pidinfo` is told exactly its size. The call only
    // writes into the buffer and returns how many bytes it wrote.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
            size,
        )
    };
    (written == size).then_some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live child process in a **session of its own**, so it has no
    /// controlling terminal at all.
    ///
    /// `setsid(2)` in the child between `fork` and `exec` is the only way
    /// to get one: a plain child inherits our session and therefore our
    /// `e_tdev`, and macOS ships no `setsid(1)` to do it for us. The
    /// obvious candidate — pid 1 — does **not** work, and finding that out
    /// is worth a line: `proc_pidinfo` answers 0 bytes for `launchd`
    /// because it belongs to root, so this check reports `Dead` for it.
    /// That is safe (it refuses) and it is not the case we want to prove.
    #[cfg(target_os = "macos")]
    fn ttyless_child() -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30");
        // SAFETY: `pre_exec` runs in the forked child before `exec`, and
        // `setsid` is async-signal-safe. Nothing else is touched there.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sleep")
    }

    /// This process's own controlling tty, or `None` under a test runner
    /// that has none (CI, launchd).
    #[cfg(target_os = "macos")]
    fn own_tty() -> Option<String> {
        // `ttyname` on stdin: the one call that answers "what is my
        // terminal called" without a fork.
        // SAFETY: `ttyname` returns a pointer into a static buffer or
        // NULL, and it is read before anything else can call it again.
        let name = unsafe { libc::ttyname(0) };
        if name.is_null() {
            return None;
        }
        // SAFETY: non-null, and `ttyname`'s result is a NUL-terminated C
        // string.
        Some(
            unsafe { std::ffi::CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// The four cases `fgcheck.c` was proven on, against **real pids** —
    /// three of them unconditionally, and the "allow" one only when the
    /// test really has a terminal.
    ///
    /// The refusals are the rows that matter, and they need no tty of their
    /// own: a dead pid, a pid with no controlling terminal at all, and a
    /// tty that does not exist are each a refusal on this machine, in CI,
    /// and under launchd. These rows read the macOS check; elsewhere it
    /// cannot tell (`off_macos_guard_three_cannot_tell_and_refuses`).
    #[cfg(target_os = "macos")]
    #[test]
    fn guard_three_refuses_a_dead_pid_a_ttyless_pid_and_a_vanished_tty() {
        // A pid that cannot be alive: `pid_max` is 99999 on macOS, so
        // 999999 is not a pid at all. (The `fgcheck.c` run used the same
        // number.)
        assert_eq!(check(999_999, "/dev/null"), Verdict::Dead);

        // Alive with no controlling terminal at all: the "there is no tty
        // to be the foreground of" case. `/dev/null` is a character device
        // with a real `st_rdev`, so the tty `stat` succeeds and the verdict
        // comes from the process.
        let mut orphan = ttyless_child();
        assert_eq!(check(orphan.id(), "/dev/null"), Verdict::NoTty);
        let ttyless_pid = orphan.id();
        let _ = orphan.kill();
        let _ = orphan.wait();
        // …and once it is reaped, the same pid is `Dead`.
        assert_eq!(check(ttyless_pid, "/dev/null"), Verdict::Dead);

        // A root-owned process is refused too, for a different reason:
        // `proc_pidinfo` will not describe `launchd` to us, and an answer
        // we cannot get is an answer that refuses.
        assert!(!check(1, "/dev/null").allows());

        // A tty device node that is not there: the tab was closed.
        assert_eq!(
            check(std::process::id(), "/dev/ttys99999"),
            Verdict::TtyGone
        );
        assert_eq!(
            check(std::process::id(), "ttys99999"),
            Verdict::TtyGone,
            "a recorded tty with no /dev prefix resolves the same way"
        );

        // None of those allow anything.
        for v in [
            Verdict::Dead,
            Verdict::NoTty,
            Verdict::TtyGone,
            Verdict::Background,
            Verdict::OtherTty,
            Verdict::Unknown,
        ] {
            assert!(!v.allows(), "{v}");
        }
        assert!(Verdict::Foreground.allows());
    }

    /// **M9 and M10's four cases as a decision table**, with no
    /// controlling terminal required.
    ///
    /// This is the row the environment cannot take away. The live checks
    /// below arrange real pids, and three of their four cases survive a
    /// runner with no tty — but M10's, the one the whole `e_tdev`
    /// narrowing exists for, does not: with `e_tdev == NODEV` the `NoTty`
    /// arm answers first and the comparison is never reached. Deleting
    /// `tdev == recorded_rdev` from the real check leaves every other test
    /// in this file green under the gate. Here it does not.
    #[test]
    fn the_three_part_decision_refuses_the_right_pid_on_the_wrong_terminal() {
        // `st_rdev` of the tty the hook helper recorded, and the same
        // number as the process's `e_tdev` when it is really that terminal.
        const OURS: u32 = 0x1000_0004;
        const THEIRS: u32 = 0x1000_0009;

        // 1. Alive, foreground, on the recorded tty: the only allow.
        assert_eq!(verdict(OURS, OURS, 700, 700), Verdict::Foreground);
        // 2. Alive on the recorded tty, but the shell holds the
        //    foreground — §15's "the CLI exited to a shell".
        assert_eq!(verdict(OURS, OURS, 700, 701), Verdict::Background);
        // 3. **M10.** Alive and the foreground group of its own terminal,
        //    which is *not* the one we recorded. A pgid-only check says
        //    `Foreground` here; this is the case that would aim a
        //    dictation at another tab.
        assert_eq!(verdict(OURS, THEIRS, 700, 700), Verdict::OtherTty);
        assert_eq!(verdict(OURS, THEIRS, 700, 701), Verdict::OtherTty);
        // 4. No controlling terminal at all, answered before the
        //    comparison so it is never mistaken for case 3.
        assert_eq!(verdict(OURS, NODEV, 700, 700), Verdict::NoTty);
        assert_eq!(verdict(NODEV, NODEV, 700, 700), Verdict::NoTty);
        // Exactly one of the six allows anything.
        for (r, t, p, tp) in [
            (OURS, OURS, 700u32, 701u32),
            (OURS, THEIRS, 700, 700),
            (OURS, THEIRS, 700, 701),
            (OURS, NODEV, 700, 700),
            (NODEV, NODEV, 700, 700),
        ] {
            assert!(!verdict(r, t, p, tp).allows(), "{t:x} {p} {tp}");
        }
    }

    /// M10's case, which a pgid-only check gets wrong: this process is
    /// alive and (under a terminal) the foreground group of *its own* tty,
    /// and it must still be refused when it is aimed at another one.
    ///
    /// `/dev/console` is the other terminal: a real character device with
    /// its own `st_rdev` that this process is certainly not the foreground
    /// of, present on every macOS install.
    #[cfg(target_os = "macos")]
    #[test]
    fn guard_three_refuses_the_right_pid_aimed_at_the_wrong_tty() {
        let me = std::process::id();
        let verdict = check(me, "/dev/console");
        assert!(!verdict.allows(), "aimed at another terminal: {verdict}");
        assert!(
            matches!(verdict, Verdict::OtherTty | Verdict::NoTty),
            "either this process has no tty, or it has one and it is not \
             /dev/console — never `foreground`: {verdict}"
        );
    }

    /// The allow row, when the test runner really has a terminal. Under
    /// `cargo test` from a shell it does; in CI it does not, and the row
    /// says so rather than failing.
    ///
    /// Its value is that it is the *only* assertion in this file that
    /// `Foreground` is reachable at all — a check that could only ever
    /// refuse would pass every other test here. On macOS, where the check
    /// can allow at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn guard_three_allows_this_process_on_its_own_tty_when_it_has_one() {
        let Some(tty) = own_tty() else {
            println!("skip  guard 3 allow row: this test has no controlling tty");
            return;
        };
        let me = std::process::id();
        let verdict = check(me, &tty);
        // libtest's threads share the process's pgrp, and `cargo test`
        // itself is what holds the foreground — so this process's pgid is
        // the foreground pgid of the tty it was started from. If a future
        // runner backgrounds us, `Background` is the correct answer and the
        // row still proves the mechanism ran.
        assert!(
            matches!(verdict, Verdict::Foreground | Verdict::Background),
            "own pid on own tty: {verdict}"
        );
        if verdict == Verdict::Foreground {
            println!("note  guard 3 allowed pid {me} on {tty}");
        } else {
            println!("note  guard 3 saw pid {me} backgrounded on {tty} (still a refusal)");
        }
        // And the same pid on a *different* device is refused, which is the
        // pair that makes the tty clause load-bearing rather than decorative.
        assert!(!check(me, "/dev/null").allows());
    }

    /// Off macOS this build cannot tell whose foreground a pid is (§13's
    /// Linux port needs `/proc/<pid>/stat`), and a guard that cannot tell
    /// refuses: every pid on every tty is `Unknown`, which allows nothing,
    /// so no dictation is ever typed there on a guess.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_guard_three_cannot_tell_and_refuses() {
        for (pid, tty) in [
            (std::process::id(), "/dev/null"),
            (std::process::id(), "/dev/tty"),
            (999_999, "/dev/null"),
            (1, "/dev/console"),
        ] {
            let verdict = check(pid, tty);
            assert_eq!(verdict, Verdict::Unknown, "{pid} on {tty}");
            assert!(!verdict.allows(), "{pid} on {tty}");
        }
    }

    /// The trait and the free function are one check, so a test that fakes
    /// the seam is faking the same thing the daemon runs.
    #[test]
    fn the_foreground_seam_forwards_to_the_real_check() {
        let procs = Procs;
        assert_eq!(
            procs.check(999_999, "/dev/null"),
            check(999_999, "/dev/null")
        );
        assert!(!procs.check(1, "/dev/null").allows());
        assert_eq!(Verdict::Foreground.code(), "foreground");
        assert_eq!(Verdict::Background.to_string(), "background");
    }
}
