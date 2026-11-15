//! Windows' process table, for the walks macOS does with `proc_pidinfo`,
//! `proc_pidpath`, `proc_listchildpids` and `KERN_PROCARGS2`: the hook
//! helper's parent walk (`hook::walk`), the ancestry cap (`trust`) and a
//! CLI's running Bash call (`procs`).
//!
//! A Toolhelp snapshot names every process with its parent and executable
//! name; the full image path and the command line come from the process
//! itself, which this user may query for its own processes without admin
//! rights. Read-only: nothing here signals or changes a process.
//!
//! Windows reuses pids, and a snapshot's parent pid is only what the parent
//! was when the child started: once that parent exits, an unrelated later
//! process can hold the same pid. So a parent counts only when it was
//! created no later than its child; otherwise the chain ends there.
//!
//! The calls are declared here rather than through windows-sys features: a
//! feature added to windows-sys rebuilds every crate that depends on it.
use std::ffi::c_void;
use std::path::PathBuf;

type Handle = *mut c_void;
const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

/// `PROCESSENTRY32W`, as Toolhelp fills it.
#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    pid: u32,
    default_heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    exe_file: [u16; 260],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

/// `UNICODE_STRING`, the head of a command-line answer.
#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum: u16,
    buffer: *const u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn QueryFullProcessImageNameW(process: Handle, flags: u32, name: *mut u16, size: *mut u32) -> i32;
    fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(
        process: Handle,
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// Closes a kernel handle on drop.
struct Owned(Handle);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: opened by us, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// One process in a snapshot.
#[derive(Clone, Debug)]
pub struct Entry {
    pub pid: u32,
    pub ppid: u32,
    /// The executable's file name (`claude.exe`).
    pub name: String,
}

/// Every process, as one Toolhelp snapshot saw it.
#[must_use]
pub fn snapshot() -> Vec<Entry> {
    // SAFETY: a process snapshot; the handle is closed by `Owned`.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return Vec::new();
    }
    let snap = Owned(snap);
    let mut entries = Vec::new();
    // SAFETY: zeroed is a valid PROCESSENTRY32W once its size is set.
    let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ProcessEntry>() as u32;
    // SAFETY: `entry` is sized; the walk fills it one process at a time.
    let mut more = unsafe { Process32FirstW(snap.0, &mut entry) } != 0;
    while more {
        let len = entry.exe_file.iter().position(|c| *c == 0).unwrap_or(entry.exe_file.len());
        entries.push(Entry {
            pid: entry.pid,
            ppid: entry.parent,
            name: String::from_utf16_lossy(&entry.exe_file[..len]),
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap.0, &mut entry) } != 0;
    }
    entries
}

fn open(pid: u32) -> Option<Owned> {
    // SAFETY: query-only access; closed by `Owned`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!process.is_null()).then_some(Owned(process))
}

/// When `pid` was created, in 100 ns ticks; `None` when it cannot be read.
fn created(pid: u32) -> Option<u64> {
    let process = open(pid)?;
    let mut times = [FileTime::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: our handle and four out-parameters.
    if unsafe { GetProcessTimes(process.0, created, exited, kernel, user) } == 0 {
        return None;
    }
    Some((u64::from(times[0].high) << 32) | u64::from(times[0].low))
}

/// Whether `parent` is really `child`'s parent rather than a later process
/// that reused the pid: it was created no later than the child. Unreadable
/// times (another user's process) are taken as they are reported.
fn really_parent(parent: u32, child: u32) -> bool {
    match (created(parent), created(child)) {
        (Some(parent), Some(child)) => parent <= child,
        _ => true,
    }
}

/// The process's executable, `C:\…\claude.exe`.
#[must_use]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    let process = open(pid)?;
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    // SAFETY: `buf` holds `size` characters; the call writes the length back.
    if unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &mut size) } == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..size as usize])))
}

/// `pid`'s parent and names (its executable's file name, then its full
/// path when that can be read), or `None` when no such process exists. A
/// parent that is not really one (see the module) is reported as 0, which
/// ends a walk.
#[must_use]
pub fn describe(pid: u32) -> Option<(u32, Vec<String>)> {
    let entry = snapshot().into_iter().find(|e| e.pid == pid)?;
    let ppid = if entry.ppid != 0 && entry.ppid != pid && really_parent(entry.ppid, pid) {
        entry.ppid
    } else {
        0
    };
    let mut names = vec![entry.name];
    if let Some(exe) = exe_path(pid) {
        names.push(exe.to_string_lossy().into_owned());
    }
    names.retain(|n| !n.is_empty());
    names.dedup();
    Some((ppid, names))
}

/// `pid`'s children (that really are: created after it).
#[must_use]
pub fn children(pid: u32) -> Vec<u32> {
    let Some(born) = created(pid) else {
        return Vec::new();
    };
    snapshot()
        .into_iter()
        .filter(|e| e.ppid == pid && e.pid != pid)
        .filter(|e| created(e.pid).is_none_or(|child| child >= born))
        .map(|e| e.pid)
        .collect()
}

/// `pid`'s command line, as the process was started.
#[must_use]
pub fn command_line(pid: u32) -> Option<String> {
    let process = open(pid)?;
    let mut needed = 0u32;
    // A size query: too small a buffer answers with the size it needs.
    let mut probe = [0u64; 4];
    // SAFETY: `probe` is writable for its length; the call reports `needed`.
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            PROCESS_COMMAND_LINE_INFORMATION,
            probe.as_mut_ptr().cast(),
            std::mem::size_of_val(&probe) as u32,
            &mut needed,
        )
    };
    if status != STATUS_INFO_LENGTH_MISMATCH && status != 0 {
        return None;
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(4)];
    // SAFETY: `buf` holds at least `needed` bytes.
    if unsafe {
        NtQueryInformationProcess(
            process.0,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut needed,
        )
    } != 0
    {
        return None;
    }
    // SAFETY: the answer is a UNICODE_STRING whose buffer points into `buf`.
    unsafe {
        let head = &*(buf.as_ptr() as *const UnicodeString);
        if head.buffer.is_null() {
            return Some(String::new());
        }
        let chars = std::slice::from_raw_parts(head.buffer, usize::from(head.length) / 2);
        Some(String::from_utf16_lossy(chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_in_the_table_with_its_parent_path_and_command_line() {
        let me = std::process::id();
        let (_, names) = describe(me).expect("this process is in the snapshot");
        assert!(names.iter().any(|n| n.ends_with(".exe")), "{names:?}");
        assert!(exe_path(me).is_some_and(|p| p.is_absolute()));
        let line = command_line(me).expect("our own command line");
        assert!(!line.is_empty());
    }

    #[test]
    fn a_child_is_found_and_its_command_line_read() {
        // `cmd /k` reads its stdin, a pipe held open here, so the child
        // lives until it is killed whatever the machine (a ping can end at
        // once on a CI runner).
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/q", "/k", "rem deck-marker"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let since = std::time::Instant::now();
        let mut found = children(std::process::id());
        while !found.contains(&child.id()) && since.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            found = children(std::process::id());
        }
        assert!(found.contains(&child.id()), "{found:?}");
        let line = command_line(child.id()).unwrap_or_default();
        assert!(line.contains("deck-marker"), "{line}");
        let (ppid, _) = describe(child.id()).unwrap();
        assert_eq!(ppid, std::process::id());
        let _ = child.kill();
        let _ = child.wait();
    }
}
