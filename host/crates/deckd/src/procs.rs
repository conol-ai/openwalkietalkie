//! What a CLI process is running right now: the sign that an approved Bash
//! call has started, long before its `PostToolUse` (which comes only when
//! the command finishes).
//!
//! Claude Code runs every Bash call as a fresh child shell of its own
//! process — `zsh -c source <snapshot> … && eval '<command>' …` — and only
//! once the call is allowed. A host approver (Zeron's, auto mode's
//! classifier) decides in milliseconds and leaves no hook behind, so a
//! child of the CLI whose arguments carry the asked command is the earliest
//! proof that the ask was decided without the deck (`adapters::settle`).
//!
//! Read-only `proc_listchildpids` and `sysctl(KERN_PROCARGS2)`, same uid;
//! nothing here signals or changes a process. Arguments are compared in
//! memory and never logged (§11).

/// Whether `cli` has a child whose arguments carry `command`.
#[must_use]
pub fn runs_command(cli: u32, command: &str) -> bool {
    let needle = squash(command);
    if needle.len() < 3 {
        // Too short to tell one call from another.
        return false;
    }
    let needle: String = needle.chars().take(160).collect();
    children(cli)
        .into_iter()
        .filter_map(args)
        .any(|argv| squash(&argv).contains(&needle))
}

/// The command as it survives any quoting: Claude wraps it in `eval '…'`
/// and escapes its quotes, so quotes, backslashes and whitespace are
/// dropped on both sides before they are compared.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\'' | '"' | '\\'))
        .collect()
}

#[cfg(target_os = "macos")]
fn children(pid: u32) -> Vec<u32> {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return Vec::new();
    };
    let mut buf = vec![0 as libc::pid_t; 256];
    let size = libc::c_int::try_from(buf.len() * std::mem::size_of::<libc::pid_t>()).unwrap_or(0);
    // SAFETY: `buf` is writable for exactly `size` bytes; the call writes at
    // most that many. Unlike `proc_listpids` it returns a pid **count**.
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr().cast(), size) };
    if n <= 0 {
        return Vec::new();
    }
    let count = (n as usize).min(buf.len());
    buf.truncate(count);
    buf.into_iter().filter_map(|p| u32::try_from(p).ok()).filter(|p| *p > 0).collect()
}

#[cfg(not(target_os = "macos"))]
fn children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// `pid`'s argv, joined by spaces.
#[cfg(target_os = "macos")]
fn args(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query (null buffer) on a fixed three-element MIB.
    let ok = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0)
    };
    if ok != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is writable for `size` bytes, and the kernel writes at
    // most `size` and updates it to what it wrote.
    let ok = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0)
    };
    if ok != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs(&buf)
}

#[cfg(not(target_os = "macos"))]
fn args(_pid: u32) -> Option<String> {
    None
}

/// `KERN_PROCARGS2`: `argc` (a native `int`), the executable path, NUL
/// padding, then `argc` NUL-terminated arguments (the environment follows
/// and is never read).
fn parse_procargs(buf: &[u8]) -> Option<String> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let mut rest = buf.get(4..)?;
    // The executable path, then its padding.
    let end = rest.iter().position(|b| *b == 0)?;
    rest = &rest[end..];
    let start = rest.iter().position(|b| *b != 0)?;
    rest = &rest[start..];
    let argv: Vec<String> = rest
        .split(|b| *b == 0)
        .take(usize::try_from(argc).ok()?)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some(argv.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procargs_yield_the_arguments_and_never_the_environment() {
        let mut raw = 3i32.to_ne_bytes().to_vec();
        raw.extend_from_slice(b"/bin/zsh\0\0\0zsh\0-c\0eval 'ls -la'\0SECRET=1\0");
        assert_eq!(parse_procargs(&raw).as_deref(), Some("zsh -c eval 'ls -la'"));
    }

    #[test]
    fn a_quoted_command_still_matches_its_shell() {
        let shell = r#"zsh -c source /x.sh && eval 'git commit -m '\''fix it'\''' < /dev/null"#;
        assert!(squash(shell).contains(&squash("git commit -m 'fix it'")));
        assert!(!squash(shell).contains(&squash("git commit -m 'other'")));
    }

    /// The real thing: this test's own child shell is found by its command.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_child_is_found_by_its_command() {
        let mut child = std::process::Command::new("/bin/sh")
            // Compound, as Claude's are: `sh -c` would exec a lone command
            // and its argv would no longer carry the line.
            .args(["-c", "sleep 5; true # owt-procs-probe-7731"])
            .spawn()
            .expect("a child shell");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let me = std::process::id();
        assert!(runs_command(me, "sleep 5; true # owt-procs-probe-7731"));
        assert!(!runs_command(me, "sleep 5; true # some other command"));
        child.kill().ok();
        child.wait().ok();
    }
}
