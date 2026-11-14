//! The parent walk (contract §4.3, design §5): which process is the CLI
//! that ran this hook, so the daemon has a pid it may poll with
//! `kill(pid, 0)` and a tty to show. The walk on from the CLI to the app
//! that owns the session is `super::app`'s.
//!
//! `$PPID` is the wrong answer, and wrong in different ways per CLI:
//!
//! - **exec form** (Claude's `command` + `args`): the parent *is* `claude`.
//! - **shell form** (Codex — `hooks.json`'s `command` is a shell **string**,
//!   C12/C13): the parent is the `sh -c` the CLI spawned. That shell exits
//!   the moment the hook returns, so a `kill(ppid, 0)` liveness poll would
//!   fail within 2 s and flap the whole roster. (The single-simple-command
//!   `exec` optimisation often collapses the shell away, which is why the
//!   design believed Codex was exec form — but it only *often* does, and a
//!   quoted path with arguments is exactly the case where it may not.)
//! - **no CLI at all**: someone ran `deckd hook codex` by hand, or a
//!   wrapper script sits in between with a chain we do not recognise. The
//!   honest answer is `unknown`, and the daemon then polls nothing: a pid
//!   we cannot vouch for is a pid that retires somebody else's agent when
//!   it is recycled.
//!
//! The walk is a pure function of a process table (`Procs`), so both spawn
//! shapes and the unknown case are fixture-tested without spawning
//! anything. `MacProcs` is the one implementation that touches the kernel
//! (`proc_pidinfo(PROC_PIDTBSDINFO)` and `proc_pidpath`), and the peer-trust
//! ancestry cap (`crate::trust`) reads every hop of its own walk through it,
//! so the two recognise a CLI the same way.
use deck_model::PidKind;

/// How far up the chain the walk looks (contract §4.3). Eight hops covers
/// `hook → sh -c → codex` with room for a wrapper or two and still stops
/// long before `launchd`: a CLI that is eight processes above its own hook
/// is not a shape we can claim to understand.
pub const MAX_HOPS: usize = 8;

/// One process as the walk needs to see it: who its parent is and what it
/// calls itself. Two names because they differ — `/bin/sh` reached through
/// a symlink named `node` reports `pbi_comm = "node"` (the path component
/// used at `exec`) and `pbi_name = "bash"` — and either one identifying a
/// CLI is enough.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub names: Vec<String>,
}

impl Proc {
    /// A table entry for the tests and for `MacProcs`.
    pub fn new(pid: u32, ppid: u32, names: &[&str]) -> Proc {
        Proc {
            pid,
            ppid,
            names: names.iter().map(|n| (*n).to_string()).collect(),
        }
    }

    fn is_cli(&self) -> bool {
        self.names
            .iter()
            .any(|n| crate::trust::is_agent_process_name(n))
    }
}

/// Where the walk reads processes from.
pub trait Procs {
    fn get(&self, pid: u32) -> Option<Proc>;

    /// `get`, and also a process another user owns. The app walk
    /// (`super::app`) crosses the root-owned `/usr/bin/login` that
    /// login-shell terminals run between the shell and the app, which `get`
    /// cannot read; the CLI walk and liveness keep `get`'s answer.
    fn get_any(&self, pid: u32) -> Option<Proc> {
        self.get(pid)
    }
}

/// What the walk resolved, as the helper reports it to `hooks.<cli>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walk {
    /// The CLI's pid, or `None` when no ancestor was one.
    pub cli_pid: Option<u32>,
    /// `cli` with a pid, `unknown` without. The daemon polls `cli` only.
    pub pid_kind: PidKind,
    /// How many parents were looked at (0 = the hook process itself was
    /// the CLI, which happens when a CLI execs the helper in place).
    pub hops: usize,
    /// The chain the walk saw, newest first, as `(pid, first name)` — for
    /// `deckd doctor` and for a test's assertion message. Process names
    /// only, never content.
    pub chain: Vec<(u32, String)>,
}

impl Walk {
    /// The answer when nothing up the chain was a CLI.
    fn unknown(chain: Vec<(u32, String)>, hops: usize) -> Walk {
        Walk {
            cli_pid: None,
            pid_kind: PidKind::Unknown,
            hops,
            chain,
        }
    }
}

/// Walk up from `start` to the first `claude`/`codex`/`gemini`/`node`.
///
/// `start` itself is examined: a CLI that `exec`s the helper (rather than
/// forking it) is its own parent chain's first entry, and answering
/// `unknown` there would be a lie we can check. The walk stops at pid 1, at
/// a process that is its own parent, at a pid the table does not know, and
/// after `max_hops` parents — a bounded loop is the whole point, because
/// this runs on the CLI's critical path.
#[must_use]
pub fn resolve(start: u32, procs: &impl Procs, max_hops: usize) -> Walk {
    let mut chain = Vec::new();
    let mut cur = start;
    for hops in 0..=max_hops {
        let Some(proc) = procs.get(cur) else {
            return Walk::unknown(chain, hops);
        };
        chain.push((proc.pid, proc.names.first().cloned().unwrap_or_default()));
        if proc.is_cli() {
            return Walk {
                cli_pid: Some(proc.pid),
                pid_kind: PidKind::Cli,
                hops,
                chain,
            };
        }
        // pid 1 is launchd; a process that is its own parent is not a
        // shape that exists, but the loop must always terminate.
        if proc.pid <= 1 || proc.ppid == proc.pid || proc.ppid == 0 {
            return Walk::unknown(chain, hops);
        }
        cur = proc.ppid;
    }
    Walk::unknown(chain, max_hops)
}

/// The kernel's process table.
#[derive(Debug, Default)]
pub struct MacProcs;

impl Procs for MacProcs {
    #[cfg(target_os = "macos")]
    fn get(&self, pid: u32) -> Option<Proc> {
        let pid_t = libc::pid_t::try_from(pid).ok()?;
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        // SAFETY: the buffer is exactly `size` bytes of `proc_bsdinfo`,
        // which is what PROC_PIDTBSDINFO fills; a short return is rejected.
        let n = unsafe {
            libc::proc_pidinfo(
                pid_t,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as libc::c_int,
            )
        };
        if n != size as libc::c_int {
            return None;
        }
        // SAFETY: `proc_pidinfo` filled the whole struct (checked above).
        let info = unsafe { info.assume_init() };
        let mut names = vec![c_name(&info.pbi_comm), c_name(&info.pbi_name)];
        // `pbi_comm` is cut to 16 bytes, so a CLI launched through a long
        // path can only be recognised from the executable itself.
        if let Some(exe) = crate::trust::exe_path(pid) {
            names.push(exe.to_string_lossy().into_owned());
        }
        names.retain(|n| !n.is_empty());
        names.dedup();
        Some(Proc {
            pid,
            ppid: info.pbi_ppid,
            names,
        })
    }

    /// Windows: the Toolhelp snapshot's parent and executable name, and the
    /// image path (`crate::winproc`), with a reused parent pid ending the
    /// chain rather than continuing it through an unrelated process.
    #[cfg(windows)]
    fn get(&self, pid: u32) -> Option<Proc> {
        let (ppid, names) = crate::winproc::describe(pid)?;
        Some(Proc { pid, ppid, names })
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn get(&self, _pid: u32) -> Option<Proc> {
        None
    }

    /// `PROC_PIDT_SHORTBSDINFO` and `proc_pidpath` answer for a process of
    /// any user (the kernel exempts them from the same-user check that
    /// `PROC_PIDTBSDINFO` makes), which is all the app walk needs: a parent
    /// and the names.
    #[cfg(target_os = "macos")]
    fn get_any(&self, pid: u32) -> Option<Proc> {
        if let Some(proc) = self.get(pid) {
            return Some(proc);
        }
        let pid_t = libc::pid_t::try_from(pid).ok()?;
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
        // SAFETY: the buffer is exactly `size` bytes of `proc_bsdshortinfo`,
        // which is what PROC_PIDT_SHORTBSDINFO fills; a short return is
        // rejected.
        let n = unsafe {
            libc::proc_pidinfo(
                pid_t,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as libc::c_int,
            )
        };
        if n != size as libc::c_int {
            return None;
        }
        // SAFETY: `proc_pidinfo` filled the whole struct (checked above).
        let info = unsafe { info.assume_init() };
        let mut names = vec![c_name(&info.pbsi_comm)];
        if let Some(exe) = crate::trust::exe_path(pid) {
            names.push(exe.to_string_lossy().into_owned());
        }
        names.retain(|n| !n.is_empty());
        names.dedup();
        Some(Proc {
            pid,
            ppid: info.pbsi_ppid,
            names,
        })
    }
}

/// A NUL-terminated `c_char` array as a `String` (lossy; process names,
/// never content).
#[cfg(target_os = "macos")]
fn c_name(bytes: &[libc::c_char]) -> String {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    let raw: Vec<u8> = bytes[..end].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&raw).into_owned()
}

/// This process's own walk, through the kernel.
#[must_use]
pub fn from_process() -> Walk {
    resolve(std::process::id(), &MacProcs, MAX_HOPS)
}

/// `ps -o tty= -p <pid>`: the CLI's terminal, `ttys004`-style, or `None`
/// when the process has none (a Desktop-hosted session) or is gone.
///
/// `ps` rather than `devname(3)`: the value is only ever compared and
/// displayed, a fork is affordable once per `SessionStart`, and the
/// `?`/`??` a pty-less process reports is exactly the "no terminal" answer.
#[must_use]
#[cfg(windows)]
pub fn tty_of(_pid: u32) -> Option<String> {
    // Windows consoles have no tty device; a session is never told apart by one.
    None
}

/// `ps -o tty= -p <pid>` (see the Windows twin).
#[must_use]
#[cfg(not(windows))]
pub fn tty_of(pid: u32) -> Option<String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tty.is_empty() || tty.starts_with('?') {
        return None;
    }
    Some(format!("/dev/{tty}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture process table, newest pid first.
    struct Table(Vec<Proc>);

    impl Procs for Table {
        fn get(&self, pid: u32) -> Option<Proc> {
            self.0.iter().find(|p| p.pid == pid).cloned()
        }
    }

    /// Claude's exec form: the CLI spawns the helper directly.
    fn exec_form() -> Table {
        Table(vec![
            Proc::new(4101, 4100, &["deckd", "deckd"]),
            Proc::new(4100, 900, &["claude", "node"]),
            Proc::new(900, 1, &["zsh", "zsh"]),
        ])
    }

    /// Codex's shell form (C12/C13): `hooks.json` holds a command string,
    /// so the CLI runs it through `sh -c` and the helper's parent is the
    /// shell, not `codex`.
    fn shell_form() -> Table {
        Table(vec![
            Proc::new(5202, 5201, &["deckd", "deckd"]),
            Proc::new(5201, 5200, &["sh", "sh"]),
            Proc::new(5200, 900, &["codex", "codex"]),
            Proc::new(900, 1, &["zsh", "zsh"]),
        ])
    }

    #[test]
    fn the_exec_form_finds_the_cli_one_hop_up() {
        let walk = resolve(4101, &exec_form(), MAX_HOPS);
        assert_eq!(walk.cli_pid, Some(4100), "{:?}", walk.chain);
        assert_eq!(walk.pid_kind, PidKind::Cli);
        assert_eq!(walk.hops, 1);
    }

    #[test]
    fn the_shell_form_finds_the_cli_past_the_shell() {
        let walk = resolve(5202, &shell_form(), MAX_HOPS);
        assert_eq!(walk.cli_pid, Some(5200), "{:?}", walk.chain);
        assert_eq!(walk.pid_kind, PidKind::Cli);
        assert_eq!(walk.hops, 2, "sh -c is the hop the naive $PPID stops at");
    }

    #[test]
    fn a_cli_that_execs_the_helper_is_its_own_answer() {
        let table = Table(vec![
            Proc::new(7000, 900, &["claude", "node"]),
            Proc::new(900, 1, &["zsh", "zsh"]),
        ]);
        let walk = resolve(7000, &table, MAX_HOPS);
        assert_eq!((walk.cli_pid, walk.hops), (Some(7000), 0));
    }

    #[test]
    fn no_cli_ancestor_is_unknown_and_carries_no_pid() {
        let table = Table(vec![
            Proc::new(8002, 8001, &["deckd", "deckd"]),
            Proc::new(8001, 8000, &["sh", "sh"]),
            Proc::new(8000, 900, &["make", "make"]),
            Proc::new(900, 1, &["zsh", "zsh"]),
            Proc::new(1, 0, &["launchd", "launchd"]),
        ]);
        let walk = resolve(8002, &table, MAX_HOPS);
        assert_eq!(walk.cli_pid, None, "{:?}", walk.chain);
        assert_eq!(walk.pid_kind, PidKind::Unknown);
        assert_eq!(walk.chain.len(), 5, "the walk stops at launchd");
    }

    #[test]
    fn the_hop_budget_is_a_ceiling_not_a_suggestion() {
        // A chain of 40 shells with `codex` at the top: past eight hops the
        // walk must give up rather than keep reading the process table on
        // the CLI's critical path.
        let mut procs: Vec<Proc> = (0..40)
            .map(|i| Proc::new(9000 + i, 9001 + i, &["sh", "sh"]))
            .collect();
        procs.push(Proc::new(9040, 1, &["codex", "codex"]));
        let table = Table(procs);
        assert_eq!(resolve(9000, &table, MAX_HOPS).pid_kind, PidKind::Unknown);
        assert_eq!(resolve(9000, &table, 40).cli_pid, Some(9040));
    }

    #[test]
    fn a_pid_that_went_away_mid_walk_is_unknown_not_a_panic() {
        let table = Table(vec![Proc::new(6001, 6000, &["deckd", "deckd"])]);
        let walk = resolve(6001, &table, MAX_HOPS);
        assert_eq!(walk.pid_kind, PidKind::Unknown);
        assert_eq!(walk.chain.len(), 1);
    }

    #[test]
    fn a_process_that_is_its_own_parent_terminates_the_walk() {
        let table = Table(vec![Proc::new(3, 3, &["weird", "weird"])]);
        assert_eq!(resolve(3, &table, MAX_HOPS).pid_kind, PidKind::Unknown);
    }

    /// `launchd` is root's: `get` cannot read it from a user's process and
    /// `get_any` can, which is what crossing `/usr/bin/login` needs. A
    /// read-only kernel probe.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_walk_can_read_a_root_owned_process() {
        let launchd = MacProcs.get_any(1).expect("pid 1 is readable");
        assert_eq!((launchd.pid, launchd.ppid), (1, 0));
        assert!(
            launchd.names.iter().any(|n| n.ends_with("/launchd")),
            "{launchd:?}"
        );
    }

    #[test]
    fn the_kernel_table_answers_for_this_process() {
        // Identity is deliberately not asserted: this test runs under
        // whatever spawned cargo, which may or may not be a CLI. What must
        // hold is that the real table answers, and answers about *us*.
        let procs = MacProcs;
        let me = procs.get(std::process::id());
        if cfg!(target_os = "macos") {
            let me = me.expect("the kernel knows this process");
            assert_eq!(me.pid, std::process::id());
            assert!(me.ppid > 0, "{me:?}");
            assert!(!me.names.is_empty(), "{me:?}");
        } else {
            assert!(me.is_none());
        }
    }
}
