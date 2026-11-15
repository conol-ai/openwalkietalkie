//! Peer verification on the UDS (design §4.2, §11; contract §3.4): who is
//! the trusted principal (the code-signed GUI) and whose ancestry caps its
//! scopes. Nothing but a `PeerVerifier` ever sets `trusted`; over HTTP it
//! is always false. `MacVerifier` is the Security.framework path;
//! `FakeVerifier` is for tests.
//!
//! The macOS path is the design's v1 one and its pid-reuse window is the
//! documented gap (Q26): `LOCAL_PEERPID` → `SecCodeCopyGuestWithAttributes
//! (kSecGuestAttributePid)` → `SecRequirementCreateWithString([trust]
//! gui_requirement)` → `SecCodeCheckValidity` → `proc_pidpath`, re-read
//! **after** the validity call, must end in `/Contents/MacOS/OpenWalkieTalkie`.
//! It uses the `security-framework` crate's `os::macos::code_signing`
//! module (3.7.0 exposes `GuestAttributes::set_pid`,
//! `SecCode::copy_guest_with_attribues` and `SecCode::check_validity`), so
//! no hand-written FFI is needed for the signature half; the process
//! queries (`proc_pidpath`, `proc_pidinfo(PROC_PIDTBSDINFO)`) are `libc`.
use std::path::{Path, PathBuf};

/// What the verifier concluded about one connection's peer process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerVerdict {
    /// The peer is the verified GUI (`SecCode` requirement + path).
    pub trusted: bool,
    /// An agent CLI (`claude`/`codex`/`gemini`/`node`) is in its ancestry:
    /// scopes are capped to `Scope::AGENT_DESCENDED_CAP`.
    pub ancestry_capped: bool,
    /// The peer's executable, when it could be read (for the log).
    pub exe: Option<PathBuf>,
}

pub trait PeerVerifier: Send + Sync {
    /// Verify the peer with pid `pid`, once per connection, before the hello reply.
    fn verify(&self, pid: u32) -> PeerVerdict;

    /// A hot `[trust]` reload reached the daemon (§3.7 lists `trust.*` as
    /// hot). The verifier lives behind `Arc<dyn PeerVerifier>` in
    /// `api::Api`, so it re-reads its requirements through interior
    /// mutability rather than being rebuilt. The default is a no-op: a
    /// verifier with no requirements has nothing to re-read.
    fn reconfigure(&self, _gui_requirement: &str, _dev_requirement: Option<&str>) {}
}

/// The names in a ppid chain that cap a peer (§4.2), compared by basename,
/// case-insensitively.
pub const AGENT_PROCESS_NAMES: [&str; 4] = ["claude", "codex", "gemini", "node"];
/// The ancestry walk stops here.
pub const ANCESTRY_MAX_HOPS: usize = 64;
/// The trusted executable's path suffix inside the bundle.
pub const GUI_EXE_SUFFIX: &str = "/Contents/MacOS/OpenWalkieTalkie";

/// The Security.framework verifier: `SecCodeCopyGuestWithAttributes(pid)`
/// → `SecRequirementCreateWithString(requirement)` → `SecCodeCheckValidity`
/// → `proc_pidpath` ends with `GUI_EXE_SUFFIX`; `dev_requirement` (a
/// `cdhash H"…"` string) accepted the same way and logged at `warn` on
/// every verification.
#[derive(Debug)]
pub struct MacVerifier {
    /// Swapped whole on a `[trust]` hot reload; `verify` reads the version
    /// in force when it runs.
    requirements: arc_swap::ArcSwap<TrustRequirements>,
}

/// The `[trust]` requirement strings a `MacVerifier` checks against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustRequirements {
    pub gui: String,
    pub dev: Option<String>,
}

impl MacVerifier {
    pub fn new(requirement: String, dev_requirement: Option<String>) -> Self {
        MacVerifier {
            requirements: arc_swap::ArcSwap::from_pointee(TrustRequirements {
                gui: requirement,
                dev: dev_requirement,
            }),
        }
    }

    /// The requirements in force right now.
    pub fn requirements(&self) -> std::sync::Arc<TrustRequirements> {
        self.requirements.load_full()
    }

    /// Whether the peer's code satisfies `requirement` (or, second,
    /// `dev_requirement`), and which one it was.
    #[cfg(target_os = "macos")]
    fn satisfies(&self, pid: u32) -> Option<Requirement> {
        use security_framework::os::macos::code_signing::{
            Flags, GuestAttributes, SecCode, SecRequirement,
        };
        use std::str::FromStr;

        let pid = libc::pid_t::try_from(pid).ok()?;
        let mut attrs = GuestAttributes::new();
        attrs.set_pid(pid);
        let code = match SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE) {
            Ok(code) => code,
            Err(e) => {
                tracing::debug!(pid, error = %e, "trust: no SecCode for the peer");
                return None;
            }
        };
        let check = |text: &str| -> bool {
            match SecRequirement::from_str(text) {
                Ok(req) => code.check_validity(Flags::NONE, &req).is_ok(),
                Err(e) => {
                    tracing::warn!(error = %e, "trust: requirement string does not parse");
                    false
                }
            }
        };
        let requirements = self.requirements();
        if check(&requirements.gui) {
            return Some(Requirement::Gui);
        }
        match requirements.dev.as_deref() {
            Some(dev) if check(dev) => Some(Requirement::Dev),
            _ => None,
        }
    }

    /// Windows builds are not signed, so there is no signature to check:
    /// the peer stands for the GUI when it is the app in this service's own
    /// folder ([`is_gui_path`], checked by `verify`) and no agent CLI is in
    /// its ancestry. A program running as the same user could still replace
    /// that file; on a Mac the signature is what stops that, and the install
    /// guide says Windows has no such guard.
    #[cfg(windows)]
    fn satisfies(&self, pid: u32) -> Option<Requirement> {
        (!ancestry_capped(pid)).then_some(Requirement::Gui)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn satisfies(&self, _pid: u32) -> Option<Requirement> {
        None
    }
}

/// Which `[trust]` requirement the peer's signature satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requirement {
    Gui,
    Dev,
}

impl PeerVerifier for MacVerifier {
    fn verify(&self, pid: u32) -> PeerVerdict {
        let matched = self.satisfies(pid);
        // §3.4: the path is read again *after* the validity check, so a
        // peer that re-execs between the two calls fails the gate.
        let exe = exe_path(pid);
        let path_ok = exe.as_deref().is_some_and(is_gui_path);
        if matched == Some(Requirement::Dev) {
            tracing::warn!(
                pid,
                path_ok,
                "trust: peer accepted by [trust] dev_requirement (ad-hoc signature)"
            );
        }
        let trusted = matched.is_some() && path_ok;
        let verdict = PeerVerdict {
            trusted,
            ancestry_capped: ancestry_capped(pid),
            exe,
        };
        tracing::debug!(
            pid,
            trusted = verdict.trusted,
            capped = verdict.ancestry_capped,
            "trust: peer verified"
        );
        verdict
    }

    /// §3.7 lists `trust.*` as hot: a reloaded `gui_requirement` /
    /// `dev_requirement` takes effect on the next peer, with no restart.
    fn reconfigure(&self, gui_requirement: &str, dev_requirement: Option<&str>) {
        let next = TrustRequirements {
            gui: gui_requirement.to_string(),
            dev: dev_requirement.map(String::from),
        };
        if *self.requirements() == next {
            return;
        }
        tracing::info!(
            dev_requirement = next.dev.is_some(),
            "trust: requirements reloaded"
        );
        self.requirements.store(std::sync::Arc::new(next));
    }
}

/// A verifier that says the same thing about every peer (tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeVerifier {
    pub trusted: bool,
    pub capped: bool,
}

impl PeerVerifier for FakeVerifier {
    fn verify(&self, _pid: u32) -> PeerVerdict {
        PeerVerdict {
            trusted: self.trusted,
            ancestry_capped: self.capped,
            exe: None,
        }
    }
}

/// Whether a process name (as `pbi_comm`/`pbi_name` or a path) is one of
/// the agent CLIs, by basename, case-insensitively — or by the native
/// installer's layout, where the basename is a version.
///
/// Measured on this machine (2026-09-21): `~/.local/bin/claude` is a
/// symlink to `~/.local/share/claude/versions/2.1.278`, so the running
/// process reports `pbi_comm = "2.1.278"` and an executable path whose
/// basename is a version string. A basename-only test therefore misses
/// every natively installed CLI, which costs twice: the hook helper's
/// parent walk answers `pid_kind: unknown` (so the daemon never polls that
/// session's liveness, §5.1) and the ancestry cap of §4.2 does not fire for
/// a peer the CLI spawned.
pub fn is_agent_process_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    // Windows: `C:\…\claude.exe` — backslashes, and the `.exe` the
    // executable's name carries.
    #[cfg(windows)]
    let base = {
        let base = base.rsplit('\\').next().unwrap_or(base);
        match base.len().checked_sub(4) {
            Some(cut) if base.is_char_boundary(cut) && base[cut..].eq_ignore_ascii_case(".exe") => &base[..cut],
            _ => base,
        }
    };
    if AGENT_PROCESS_NAMES
        .iter()
        .any(|agent| base.eq_ignore_ascii_case(agent))
    {
        return true;
    }
    #[cfg(windows)]
    {
        is_versioned_agent_path(&name.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    is_versioned_agent_path(name)
}

/// `…/<cli>/versions/<something>`: the native installer's layout. The `cli`
/// component must be one we know and `versions` must sit directly between
/// it and the leaf, so an unrelated path that merely contains the word
/// (`/home/claude/bin/rsync`) is not a match.
fn is_versioned_agent_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // The leaf is the version, so the layout needs at least `cli/versions/leaf`.
    parts.len() >= 3
        && parts.windows(3).any(|w| {
            AGENT_PROCESS_NAMES
                .iter()
                .any(|agent| w[0].eq_ignore_ascii_case(agent))
                && w[1].eq_ignore_ascii_case("versions")
                && w[2] == parts[parts.len() - 1]
        })
}

/// The executable of a trusted peer lives at `…app/Contents/MacOS/OpenWalkieTalkie`.
#[cfg(not(windows))]
pub fn is_gui_path(path: &Path) -> bool {
    path.to_str().is_some_and(|p| p.ends_with(GUI_EXE_SUFFIX))
}

/// Windows: the app beside this service, `OpenWalkieTalkie.exe` as the
/// preview zip names it (or the build's own `openwalkietalkie-app.exe`), in
/// the folder `deckd.exe` runs from, compared case-blind as Windows does.
#[cfg(windows)]
pub fn is_gui_path(path: &Path) -> bool {
    let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else {
        return false;
    };
    is_gui_path_beside(path, &dir)
}

/// [`is_gui_path`] against a given service folder.
#[cfg(windows)]
pub fn is_gui_path_beside(path: &Path, service_dir: &Path) -> bool {
    let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
        name.eq_ignore_ascii_case("OpenWalkieTalkie.exe") || name.eq_ignore_ascii_case("openwalkietalkie-app.exe")
    });
    // Both resolved, so `..`, 8.3 short names and links compare by the
    // folder they are.
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let same_folder = path.parent().is_some_and(|parent| {
        resolve(parent).to_string_lossy().to_lowercase() == resolve(service_dir).to_string_lossy().to_lowercase()
    });
    named && same_folder
}

/// `proc_pidpath`: the peer's executable, or `None` when the pid is gone
/// (or the platform has no such call).
#[cfg(target_os = "macos")]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for exactly the length passed, and
    // `proc_pidpath` writes at most that many bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(OsString::from_vec(buf)))
}

/// `QueryFullProcessImageNameW` (see `crate::winproc`).
#[cfg(windows)]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    crate::winproc::exe_path(pid)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn exe_path(_pid: u32) -> Option<PathBuf> {
    None
}

/// True when the peer, or any process up its ppid chain, is an agent CLI
/// (§4.2). The peer itself counts: a connection *from* `claude` is exactly
/// the case the cap exists for.
pub fn ancestry_capped(pid: u32) -> bool {
    ancestry_capped_within(pid, ANCESTRY_MAX_HOPS)
}

/// The walk with an explicit hop budget (the cap is tested with a small one).
///
/// Each hop is read as the hook helper's own walk reads it
/// (`hook::walk::MacProcs`, `proc_pidinfo(PROC_PIDTBSDINFO)` and
/// `proc_pidpath`), so the two cannot drift apart: **both** process names —
/// they differ, `/bin/sh` reached through a symlink named `node` reports
/// `pbi_comm = "node"` and `pbi_name = "bash"` — and the executable's path.
/// The path is what recognises a natively installed CLI, whose names are a
/// bare version (`2.1.322`) and whose path is `…/claude/versions/2.1.322`;
/// a walk of names alone left every process such a Claude Code started
/// uncapped. A hop the kernel will not describe (another user's, such as the
/// root `login` between a terminal and its shell) ends the walk uncapped.
/// Windows reads the same hops through `crate::winproc`.
#[cfg(any(target_os = "macos", windows))]
pub fn ancestry_capped_within(pid: u32, max_hops: usize) -> bool {
    use crate::hook::walk::{MacProcs, Procs};

    let mut cur = pid;
    for _ in 0..max_hops {
        if cur == 0 {
            return false;
        }
        let Some(hop) = MacProcs.get(cur) else {
            return false;
        };
        if hop.names.iter().any(|name| is_agent_process_name(name)) {
            return true;
        }
        // pid 1 is launchd: the chain ends there, and so does a pid whose
        // parent is itself (never seen, but the walk must always stop).
        if cur == 1 || hop.ppid == cur {
            return false;
        }
        cur = hop.ppid;
    }
    false
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn ancestry_capped_within(_pid: u32, _max_hops: usize) -> bool {
    false
}

/// The ppid chain as `(pid, names)`, newest first, where `names` are the
/// hop's `pbi_comm`, `pbi_name` and executable path (empty ones dropped,
/// repeats folded) — what the cap looked at, for the tests' assertion
/// messages. Process names and paths only, never content.
#[cfg(any(target_os = "macos", windows))]
pub fn ancestry_chain(pid: u32) -> Vec<(u32, Vec<String>)> {
    use crate::hook::walk::{MacProcs, Procs};

    let mut chain = Vec::new();
    let mut cur = pid;
    for _ in 0..ANCESTRY_MAX_HOPS {
        if cur == 0 {
            break;
        }
        let Some(hop) = MacProcs.get(cur) else {
            break;
        };
        let parent = hop.ppid;
        chain.push((cur, hop.names));
        if cur == 1 || parent == cur {
            break;
        }
        cur = parent;
    }
    chain
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn ancestry_chain(_pid: u32) -> Vec<(u32, Vec<String>)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::time::Duration;

    /// The default `[trust] gui_requirement` of `config.rs`.
    fn verifier() -> MacVerifier {
        MacVerifier::new(crate::config::Trust::default().gui_requirement, None)
    }

    #[test]
    fn the_fake_verifier_reports_what_it_was_told() {
        let fake = FakeVerifier {
            trusted: true,
            capped: false,
        };
        let v = fake.verify(1);
        assert!(v.trusted && !v.ancestry_capped && v.exe.is_none());
        let fake = FakeVerifier {
            trusted: false,
            capped: true,
        };
        assert!(fake.verify(1).ancestry_capped);
        let boxed: Box<dyn PeerVerifier> = Box::new(FakeVerifier::default());
        assert!(!boxed.verify(2).trusted);
    }

    /// The native installer puts the real binary at
    /// `~/.local/share/claude/versions/<v>` and symlinks `claude` to it, so
    /// the running process's name IS a version string. Measured on this
    /// machine 2026-09-21 against `claude 2.1.278`: a basename-only match
    /// left every such session `pid_kind: unknown` (no liveness polling)
    /// and un-capped by the §4.2 ancestry rule.
    #[test]
    fn a_natively_installed_cli_is_recognised_by_its_versioned_path() {
        for path in [
            "/Users/tony/.local/share/claude/versions/2.1.278",
            "/Users/tony/.local/share/codex/versions/0.153.4",
            "/opt/gemini/versions/1.0.0-rc.3",
        ] {
            assert!(is_agent_process_name(path), "{path}");
        }
        // The plain shapes still work, and nothing else starts matching.
        assert!(is_agent_process_name("claude"));
        assert!(is_agent_process_name("/usr/local/bin/node"));
        for path in [
            "/home/claude/bin/rsync",
            "/Users/tony/claude/notes.txt",
            "/opt/versions/claude-ish/1.0.0",
            "/Users/tony/.local/share/claudette/versions/2.1.278",
            "versions/2.1.278",
        ] {
            assert!(!is_agent_process_name(path), "{path} should not match");
        }
    }

    #[test]
    fn agent_process_names_match_by_basename_case_insensitively() {
        assert!(is_agent_process_name("node"));
        assert!(is_agent_process_name("/usr/local/bin/Claude"));
        assert!(is_agent_process_name("CODEX"));
        assert!(!is_agent_process_name("nodejs"));
        assert!(!is_agent_process_name("deckd"));
        assert!(!is_agent_process_name(""));
    }

    #[test]
    #[cfg(not(windows))] // a Mac's bundle path; Windows has its own test
    fn only_the_bundle_path_is_the_gui() {
        assert!(is_gui_path(Path::new(
            "/Applications/OpenWalkieTalkie.app/Contents/MacOS/OpenWalkieTalkie"
        )));
        assert!(!is_gui_path(Path::new("/usr/local/bin/OpenWalkieTalkie")));
        assert!(!is_gui_path(Path::new(
            "/Applications/Evil.app/Contents/MacOS/OpenWalkieTalkie2"
        )));
        assert!(!is_gui_path(Path::new("/usr/local/bin/deckd")));
    }

    /// The test binary is signed (ad hoc) but lives nowhere near the
    /// bundle path, so it is untrusted — and asking about it must not
    /// panic or block. Its ancestry is whatever ran `cargo test`, so the
    /// cap is not asserted here (`launchd_is_never_agent_descended` is the
    /// deterministic negative). `proc_pidpath` is macOS's: elsewhere the
    /// verdict names no executable
    /// (`off_macos_no_executable_is_read_and_nothing_is_capped`).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_test_process_itself_is_untrusted_with_a_clean_verdict() {
        use std::os::unix::fs::MetadataExt;

        let me = std::process::id();
        let verdict = verifier().verify(me);
        assert!(!verdict.trusted, "{verdict:?}");
        let exe = verdict.exe.expect("proc_pidpath reads our own path");
        assert!(exe.is_absolute(), "{exe:?}");
        // `proc_pidpath` answers from the executable's **vnode**, and any
        // hard link to it is an equally valid name — the `node` fixture
        // below makes one, so comparing file names would race it. The
        // inode is what "the same executable" actually means.
        let inode = |path: &Path| std::fs::metadata(path).map(|m| m.ino()).ok();
        assert_eq!(
            inode(&exe),
            inode(&std::env::current_exe().unwrap()),
            "{exe:?} is not this test binary"
        );
        assert!(!is_gui_path(&exe));
        // A dev_requirement that parses but matches nothing changes nothing.
        let dev = MacVerifier::new(
            verifier().requirements().gui.clone(),
            Some("cdhash H\"0000000000000000000000000000000000000000\"".into()),
        );
        assert!(!dev.verify(me).trusted);
        // A requirement string that does not even parse is refused, not fatal.
        let broken = MacVerifier::new("this is not a requirement (".into(), None);
        assert!(!broken.verify(me).trusted);
    }

    #[test]
    fn a_pid_that_does_not_exist_is_untrusted_and_uncapped() {
        // Above PID_MAX (99999) on macOS, and past `pid_t` entirely.
        for pid in [999_999u32, u32::MAX] {
            let verdict = verifier().verify(pid);
            assert_eq!(verdict, PeerVerdict::default(), "pid {pid}");
        }
        // pid 0 is the kernel: never trusted, never an agent, never a panic.
        let kernel = verifier().verify(0);
        assert!(!kernel.trusted && !kernel.ancestry_capped, "{kernel:?}");
    }

    #[test]
    fn launchd_is_never_agent_descended() {
        assert!(!ancestry_capped(1), "pid 1 is launchd");
        // The hop budget is honoured: no hops, no verdict.
        assert!(!ancestry_capped_within(std::process::id(), 0));
        assert_eq!(ANCESTRY_MAX_HOPS, 64);
    }

    /// Off macOS nothing reads a peer's process: there is no `proc_pidpath`,
    /// so the verdict names no executable, and no ppid walk, so nothing is
    /// ever capped — whoever the peer is, this test binary included, under
    /// whatever ran it. The §4.2 cap is the macOS one and nothing more
    /// (the owner's decision 3 keeps it as it is), and `docs/host-service.md`
    /// §13 says so for Linux.
    /// Windows: the GUI is the app in the service's own folder, by either of
    /// its names, and nothing else is.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_gui_is_the_app_beside_the_service() {
        let dir = std::env::temp_dir().join(format!("owt-trust-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let bin = dir.join("bin");
        assert!(is_gui_path_beside(&bin.join("OpenWalkieTalkie.exe"), &bin));
        assert!(is_gui_path_beside(&bin.join("openwalkietalkie-app.exe"), &bin));
        assert!(is_gui_path_beside(&bin.join("OPENWALKIETALKIE.EXE"), &bin), "case-blind");
        assert!(!is_gui_path_beside(&dir.join("OpenWalkieTalkie.exe"), &bin), "another folder");
        assert!(!is_gui_path_beside(&bin.join("powershell.exe"), &bin), "another program");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn off_macos_no_executable_is_read_and_nothing_is_capped() {
        let me = std::process::id();
        assert_eq!(exe_path(me), None);
        assert!(!ancestry_capped(me));
        assert!(!ancestry_capped_within(me, ANCESTRY_MAX_HOPS));
        assert!(ancestry_chain(me).is_empty());
        let verdict = verifier().verify(me);
        assert_eq!(verdict, PeerVerdict::default(), "untrusted, uncapped, no exe");
    }

    /// Set on the fixture half of `a_grandchild_of_a_process_named_node_is_
    /// ancestry_capped` to the file the fixture reports its child's pid in:
    /// the same binary, re-run under the name `node`. A file, not stdout —
    /// libtest owns the child's stdout and does not hand it over mid-run.
    #[cfg(target_os = "macos")]
    const FIXTURE_ENV: &str = "DECKD_TRUST_FIXTURE_PID_FILE";
    /// How long the fixture lives if nobody kills it, and how long the test
    /// waits for its pid file.
    #[cfg(target_os = "macos")]
    const FIXTURE_LIFE: Duration = Duration::from_secs(20);

    /// The fixture: spawn a child, report its pid, then wait to be killed.
    #[cfg(target_os = "macos")]
    fn node_fixture(pid_file: std::path::PathBuf) -> ! {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("/bin/sleep");
        // Rename, so the test never reads a half-written pid.
        let tmp = pid_file.with_extension("tmp");
        std::fs::write(&tmp, child.id().to_string()).expect("write the pid");
        std::fs::rename(&tmp, &pid_file).expect("publish the pid");
        std::thread::sleep(FIXTURE_LIFE);
        std::process::exit(0);
    }

    /// Set on the fixture half of `a_natively_installed_cli_is_capped_by_its_executable_path`:
    /// the copy only waits to be looked at, then killed.
    #[cfg(target_os = "macos")]
    const NATIVE_FIXTURE_ENV: &str = "DECKD_TRUST_NATIVE_FIXTURE";

    /// Claude Code's native installer runs it from `~/.local/share/claude/versions/<v>`, so
    /// both of the process's names are a bare version (`2.1.322`, measured on this Mac on
    /// 2026-11-13) and only the executable's path says what it is. The walk reads every hop's
    /// path too, so a process such a CLI started is capped; a look-alike directory is not.
    ///
    /// The fixture is a **copy** of this test binary under the versioned name: a hard link can
    /// make `proc_pidpath` answer the other link's path, and a copy of a system binary is
    /// killed at `exec` (the platform-binary check). With the environment variable set the
    /// copy only sleeps. One hop: the copy alone, so whatever ran `cargo test` does not count.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_natively_installed_cli_is_capped_by_its_executable_path() {
        use std::process::{Command, Stdio};

        if std::env::var_os(NATIVE_FIXTURE_ENV).is_some() {
            std::thread::sleep(FIXTURE_LIFE);
            std::process::exit(0);
        }
        let dir = std::env::temp_dir().join(format!("deckd-trust-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = std::env::current_exe().expect("the test binary's path");
        let look = |cli: &str| {
            let versions = dir.join(cli).join("versions");
            std::fs::create_dir_all(&versions).unwrap();
            let copy = versions.join("9.9.9");
            std::fs::copy(&me, &copy).expect("copy this binary under a version's name");
            let mut child = Command::new(&copy)
                .args([
                    "--exact",
                    "trust::tests::a_natively_installed_cli_is_capped_by_its_executable_path",
                    "--test-threads=1",
                ])
                .env(NATIVE_FIXTURE_ENV, "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("run the copy");
            let measured = (
                ancestry_capped_within(child.id(), 1),
                ancestry_chain(child.id()),
            );
            let _ = child.kill();
            let _ = child.wait();
            measured
        };
        let (native, chain) = look("claude");
        let (lookalike, other) = look("claudette");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            native,
            "a CLI named by its versioned path caps what it starts: {chain:?}"
        );
        let names = &chain.first().expect("the copy itself").1;
        assert!(
            names.iter().any(|n| n == "9.9.9"),
            "its names are the bare version: {chain:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("/claude/versions/9.9.9")),
            "and its path is the versioned one: {chain:?}"
        );
        assert!(
            !lookalike,
            "`claudette/versions/9.9.9` is no agent: {other:?}"
        );
    }

    /// A grandchild of a process whose `pbi_comm` is `node` is capped.
    ///
    /// macOS takes `p_comm`/`p_name` from the executable **file**, not from
    /// `argv[0]`, so neither `exec -a node …` nor a symlink named `node`
    /// pointing at `/bin/sh` renames anything (the symlink case reports
    /// `bash`, measured on macOS 27); copying a system binary is refused by
    /// the platform-binary check. What does work is a copy — or a hard link
    /// — of a binary of ours under the name `node`, so the test re-runs
    /// **this** test binary that way: with `DECKD_TRUST_FIXTURE` set the
    /// same test function plays the fixture half (spawn `/bin/sleep`, print
    /// its pid, wait). The tree is therefore test → `node` → `sleep`, and
    /// `ancestry_capped_within(sleep, 2)` pins the cap on the `node` hop —
    /// never on whatever ran `cargo test`. The walk is macOS's
    /// (`proc_pidinfo`), and so is this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_grandchild_of_a_process_named_node_is_ancestry_capped() {
        use std::process::{Command, Stdio};

        // The fixture half runs in the copy, whose pid (and so whose own
        // `dir`) is a different one: the path comes from the environment.
        if let Some(pid_file) = std::env::var_os(FIXTURE_ENV) {
            node_fixture(std::path::PathBuf::from(pid_file));
        }

        let dir = std::env::temp_dir().join(format!("deckd-trust-{}", std::process::id()));
        let pid_file = dir.join("child.pid");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let node = dir.join("node");
        let me = std::env::current_exe().expect("the test binary's path");
        // A hard link is instant; a copy is the fallback across devices.
        // Either way the name at `exec` is `node`, which is what `p_comm` follows.
        if std::fs::hard_link(&me, &node).is_err() {
            std::fs::copy(&me, &node).expect("copy this binary as `node`");
        }

        let mut child = Command::new(&node)
            .args([
                "--exact",
                "trust::tests::a_grandchild_of_a_process_named_node_is_ancestry_capped",
                "--test-threads=1",
            ])
            .env(FIXTURE_ENV, &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("re-run this binary as `node`");
        let deadline = std::time::Instant::now() + FIXTURE_LIFE;
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                break Some(pid);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        let measured = grandchild.map(|pid| {
            (
                verifier().verify(pid),
                ancestry_capped_within(pid, 1),
                ancestry_capped_within(pid, 2),
                ancestry_chain(pid),
            )
        });

        // Tear the tree down before asserting, so a failure leaves nothing.
        if let Some(pid) = grandchild {
            // SAFETY: the pid was just read from our own child's output and
            // the signal is only a request to exit.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);

        let (verdict, one_hop, two_hops, chain) =
            measured.expect("the `node` fixture published its child's pid");
        assert!(verdict.ancestry_capped, "{verdict:?} {chain:?}");
        assert!(!verdict.trusted, "{verdict:?}");
        assert_eq!(
            verdict.exe.as_deref().and_then(Path::to_str),
            Some("/bin/sleep"),
            "the walk started at the sleep grandchild"
        );
        assert!(!one_hop, "the sleep itself is not an agent: {chain:?}");
        assert!(two_hops, "`node` is exactly one hop up: {chain:?}");
        assert_eq!(chain.len().min(2), 2);
        assert!(
            chain[1].1.iter().any(|n| is_agent_process_name(n)),
            "the second hop is the `node` copy: {chain:?}"
        );
    }
}
