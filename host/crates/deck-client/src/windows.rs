//! The local door on Windows: a named pipe instead of the Unix socket.
//!
//! Windows has no `AF_UNIX` in tokio, and a pipe is what the platform gives
//! a same-machine, same-user API anyway. The rules this module holds are the
//! ones both ends must agree on, which is why they live in the client crate
//! the daemon already depends on:
//!
//! * **The name.** `\\.\pipe\openwalkietalkie-deckd-<16 hex>`, the hex being
//!   a hash of the data directory, so `DECKD_DATA_DIR` / `--data-dir` move
//!   the door exactly as they move the socket file on Unix (a test daemon
//!   never answers on the user's pipe). The pipe namespace is machine-wide,
//!   and two users' data directories differ, so their names do too.
//! * **Who may connect.** The daemon creates every instance with a DACL that
//!   grants its own user SID and nobody else (not even `Everyone: read`,
//!   which a pipe's default DACL hands out), refuses remote clients, and
//!   checks each client's token user on accept: the Unix door's "same uid,
//!   socket 0600" in Windows terms.
//! * **Who answers.** The first instance is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a squatter that took the name first
//!   makes the daemon's bind fail loudly rather than share it, and a client
//!   checks that the process serving the pipe runs as its own user before it
//!   says anything.
use std::io;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    CopySid, GetLengthSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
    TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The prefix every deckd pipe name starts with.
pub const PIPE_PREFIX: &str = r"\\.\pipe\openwalkietalkie-deckd-";

/// The pipe the daemon whose data directory is `data` listens on.
///
/// The directory is compared the way Windows compares paths: case folded,
/// either slash, no trailing separator. It does not have to exist, and it is
/// not canonicalised (the daemon and its clients name it the same way: from
/// `DECKD_DATA_DIR`, `--data-dir` or `%LOCALAPPDATA%`).
pub fn pipe_name_for(data: &Path) -> PathBuf {
    let text = data.to_string_lossy().replace('/', "\\").to_lowercase();
    let text = text.trim_end_matches('\\');
    // FNV-1a, 64 bit: stable across builds and toolchains, which `std`'s
    // hasher is not promised to be, and no dependency for sixteen hex digits.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    PathBuf::from(format!("{PIPE_PREFIX}{hash:016x}"))
}

/// Whether `path` names a deckd pipe rather than a file.
pub fn is_pipe(path: &Path) -> bool {
    path.to_string_lossy().starts_with(r"\\.\pipe\")
}

/// A copied SID, owned bytes (`CopySid`), comparable with `==`.
#[derive(Clone, PartialEq, Eq)]
pub struct Sid(Vec<u8>);

impl std::fmt::Debug for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sid({})", self.string())
    }
}

impl Sid {
    fn ptr(&self) -> *mut core::ffi::c_void {
        self.0.as_ptr() as *mut core::ffi::c_void
    }

    /// The SID in `S-1-5-21-…` form.
    pub fn string(&self) -> String {
        let mut out: *mut u16 = std::ptr::null_mut();
        // SAFETY: `self.0` is a valid SID (it came out of `CopySid`); the
        // returned string is LocalAlloc'd and freed below.
        if unsafe { ConvertSidToStringSidW(self.ptr(), &mut out) } == 0 || out.is_null() {
            return String::from("S-?");
        }
        let text = unsafe { wide_to_string(out) };
        unsafe { LocalFree(out.cast()) };
        text
    }

    /// The relative id, the SID's last sub-authority (1001 for the first
    /// local account): what the API prints as `uid:<n>` on Windows.
    pub fn rid(&self) -> u32 {
        // SAFETY: valid SID; the count and the sub-authority pointers point
        // into `self.0`.
        unsafe {
            let count = *GetSidSubAuthorityCount(self.ptr());
            if count == 0 {
                return 0;
            }
            *GetSidSubAuthority(self.ptr(), u32::from(count) - 1)
        }
    }
}

/// # Safety
/// `ptr` must point at a NUL-terminated UTF-16 string.
unsafe fn wide_to_string(ptr: *const u16) -> String {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Closes a kernel handle on drop.
struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The user SID of `process`'s primary token.
fn token_user(process: HANDLE) -> io::Result<Sid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a valid process handle; `token` is written once.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    let mut needed = 0u32;
    // SAFETY: a size query (null buffer, zero length) only writes `needed`.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 words keep the TOKEN_USER header aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` holds `needed` bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation(TokenUser) filled a TOKEN_USER whose SID
    // points into `buf`, which outlives the copy.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = user.User.Sid;
        let len = GetLengthSid(sid);
        let mut copy = vec![0u8; len as usize];
        if CopySid(len, copy.as_mut_ptr().cast(), sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Sid(copy))
    }
}

/// This process's user.
pub fn current_user() -> io::Result<Sid> {
    // SAFETY: the pseudo handle needs no closing.
    token_user(unsafe { GetCurrentProcess() })
}

/// The user process `pid` runs as.
pub fn process_user(pid: u32) -> io::Result<Sid> {
    // SAFETY: plain query access; the handle is closed by `Owned`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Owned(process);
    token_user(process.0)
}

/// A self-relative security descriptor from SDDL, `LocalFree`d on drop.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable once built and only ever read by the
// kernel when an instance is created; it is freed exactly once, on drop.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    /// Owner `user`, a protected DACL granting `user` full access and nobody
    /// else anything.
    pub fn only(user: &Sid) -> io::Result<SecurityDescriptor> {
        let sid = user.string();
        let sddl = wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `sd` is LocalAlloc'd by the call.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1, // SDDL_REVISION_1
                &mut sd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityDescriptor(sd))
    }

    /// `SECURITY_ATTRIBUTES` pointing at this descriptor, not inheritable.
    /// Valid for as long as `self` is.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor…; freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// The process on the other end of a connected pipe: the client's when
/// `server_side`, the server's otherwise.
pub fn pipe_peer_pid(
    pipe: &impl std::os::windows::io::AsRawHandle,
    server_side: bool,
) -> io::Result<u32> {
    use windows_sys::Win32::System::Pipes::{
        GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    };
    let handle = pipe.as_raw_handle() as HANDLE;
    let mut pid = 0u32;
    // SAFETY: a connected pipe handle; `pid` is written once.
    let ok = unsafe {
        if server_side {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// Open the daemon's pipe and check that it is served by this user.
///
/// A pipe whose every instance is busy is retried for up to a second (the
/// daemon creates the next instance right after each accept); a pipe that
/// does not exist is `NotFound`, the same `Io` error a missing socket file
/// is on Unix, so callers' "daemon not running" paths stay the same.
pub async fn connect(path: &Path) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let client = loop {
        match ClientOptions::new().open(path) {
            Ok(client) => break client,
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e),
        }
    };
    let server = pipe_peer_pid(&client, false)?;
    let theirs = process_user(server)?;
    let ours = current_user()?;
    if theirs != ours {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is served by another user ({}), not deckd",
                path.display(),
                theirs.string()
            ),
        ));
    }
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_name_follows_the_data_dir_as_windows_compares_paths() {
        let a = pipe_name_for(Path::new(r"C:\Users\u\AppData\Local\OpenWalkieTalkie"));
        assert!(a.to_string_lossy().starts_with(PIPE_PREFIX), "{a:?}");
        assert_eq!(a.to_string_lossy().len(), PIPE_PREFIX.len() + 16);
        for same in [
            r"c:\users\U\appdata\local\openwalkietalkie",
            "C:/Users/u/AppData/Local/OpenWalkieTalkie/",
            r"C:\Users\u\AppData\Local\OpenWalkieTalkie\",
        ] {
            assert_eq!(pipe_name_for(Path::new(same)), a, "{same}");
        }
        assert_ne!(pipe_name_for(Path::new(r"C:\other")), a);
        assert!(is_pipe(&a));
        assert!(!is_pipe(Path::new(r"C:\data\deckd.sock")));
    }

    #[test]
    fn this_process_has_a_user_with_a_rid_and_a_descriptor_for_it() {
        let me = current_user().unwrap();
        assert!(me.string().starts_with("S-1-"), "{me:?}");
        assert_eq!(process_user(std::process::id()).unwrap(), me);
        let sd = SecurityDescriptor::only(&me).unwrap();
        assert!(!sd.attributes().lpSecurityDescriptor.is_null());
    }
}
