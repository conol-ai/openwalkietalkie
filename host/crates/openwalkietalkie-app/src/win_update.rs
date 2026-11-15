//! Windows: the app's own update, which Sparkle does on a Mac.
//!
//! The service's look at the release feed names the app's version
//! (`firmware.status`'s `app`); when it is newer, **Update** here:
//!
//! 1. fetches the release's preview zip and its `SHA256SUMS` with Windows'
//!    own `curl.exe`, and checks the zip against the sum the release lists;
//! 2. unpacks it with Windows' own `tar.exe` into the service's caches;
//! 3. puts its `OpenWalkieTalkie.exe` and `deckd.exe` in place of the
//!    running ones. Windows lets a running program be renamed but not
//!    overwritten, so each is renamed aside (`*.old-<n>`) first, and put
//!    back if the copy fails;
//! 4. stops the service (a clean exit, which its host does not restart),
//!    waits for it to let go, and starts the host and the window again from
//!    the new files. The window that asked then quits.
//!
//! The renamed copies are deleted the next time the app starts ([`clean`]),
//! when nothing runs from them any more. The zip is unsigned, as the
//! preview is: the checksum says the file is the one the release published,
//! nothing more.
use sha2::{Digest, Sha256};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const REPOSITORY: &str = "conol-ai/openwalkietalkie";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// How long the old service may take to let go before the new one starts.
const LET_GO: Duration = Duration::from_secs(20);

static BUSY: AtomicBool = AtomicBool::new(false);

/// Whether an update is under way in this process.
pub fn busy() -> bool {
    BUSY.load(Ordering::Acquire)
}

/// The preview zip's name for `version`, as `tools/release.py` names it.
pub fn zip_name(version: &str) -> String {
    format!("OpenWalkieTalkie-{version}-windows-x86_64.zip")
}

/// Where the release for `version` keeps its files: GitHub's, or the
/// folder or URL [`RELEASES_ENV`] names (a mirror, a test's `file://`
/// release), under which each version is `v<version>`.
fn release_files(version: &str) -> String {
    match std::env::var(RELEASES_ENV) {
        Ok(base) if !base.trim().is_empty() => format!("{}/v{version}", base.trim_end_matches('/')),
        _ => format!("https://github.com/{REPOSITORY}/releases/download/v{version}"),
    }
}

/// Names another place the releases are fetched from.
pub const RELEASES_ENV: &str = "OPENWALKIETALKIE_RELEASES";

/// The argument that updates this copy without a window.
pub const FLAG: &str = "--update-to";

/// `--update-to <version>`, when this process was started with it.
pub fn requested() -> Option<String> {
    let mut args = std::env::args().skip(1);
    (args.next()? == FLAG).then(|| args.next()).flatten()
}

/// [`run`] for the command line: the outcome is written to the update log
/// (the app is a GUI program, with no console to print to), and the exit
/// code is 0 when the new copy is starting.
pub fn run_from_command_line(version: &str) -> i32 {
    let outcome = run(version);
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let log = PathBuf::from(local).join(r"OpenWalkieTalkie\logs\app-update.log");
        let line = match &outcome {
            Ok(()) => format!("updated to {version}\n"),
            Err(why) => format!("update to {version} failed: {why}\n"),
        };
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
    }
    i32::from(outcome.is_err())
}

/// A program of Windows' own, by its name in System32.
fn system32(program: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
        .join(program)
}

/// The folder the update is unpacked in, under the service's caches.
fn staging(version: &str) -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local)
        .join(r"OpenWalkieTalkie\caches\app-update")
        .join(version))
}

fn hidden(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null());
    command
}

/// `url` into `to`, by curl: redirects followed, an HTTP error an error.
fn fetch(url: &str, to: &Path) -> Result<(), String> {
    let out = hidden(system32("curl.exe"))
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o"])
        .arg(to)
        .arg(url)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not download {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The sum `sums` (a `SHA256SUMS` file) lists for `name`, lowercase.
pub fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sum.trim().to_ascii_lowercase())
    })
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Fetch, check and unpack `version`'s preview; its `bin` folder.
fn stage(version: &str) -> Result<PathBuf, String> {
    let dir = staging(version)?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = zip_name(version);
    let zip = dir.join(&name);
    let sums = dir.join("SHA256SUMS");
    fetch(&format!("{}/SHA256SUMS", release_files(version)), &sums)?;
    fetch(&format!("{}/{name}", release_files(version)), &zip)?;
    let listed = std::fs::read_to_string(&sums)
        .ok()
        .and_then(|sums| listed_sum(&sums, &name))
        .ok_or_else(|| format!("the release lists no checksum for {name}"))?;
    let got = sha256_of(&zip)?;
    if got != listed {
        return Err(format!("{name} is not the file the release published (its SHA-256 is {got})"));
    }
    let unpacked = dir.join("unpacked");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    let out = hidden(system32("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&unpacked)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("could not unpack {name}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let bin = unpacked
        .join(name.trim_end_matches(".zip"))
        .join("bin");
    for file in ["OpenWalkieTalkie.exe", "deckd.exe"] {
        if !bin.join(file).is_file() {
            return Err(format!("{name} holds no bin\\{file}"));
        }
    }
    Ok(bin)
}

/// A name beside `path` nothing else uses: `<name>.old-<n>`.
fn aside(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    (0u32..)
        .map(|n| path.with_file_name(format!("{name}.old-{n}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.with_file_name(format!("{name}.old")))
}

/// `new` in place of the file at `target`, which may be running: renamed
/// aside first, and put back if the copy fails.
fn replace(new: &Path, target: &Path) -> Result<(), String> {
    let old = target.exists().then(|| aside(target));
    if let Some(old) = &old {
        std::fs::rename(target, old).map_err(|e| format!("cannot move {} aside: {e}", target.display()))?;
    }
    if let Err(e) = std::fs::copy(new, target) {
        if let Some(old) = &old {
            let _ = std::fs::rename(old, target);
        }
        return Err(format!("cannot write {}: {e}", target.display()));
    }
    Ok(())
}

/// The staged programs in place of this copy's own.
fn install(staged: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = exe.parent().ok_or("this app is in no folder")?.to_path_buf();
    replace(&staged.join("deckd.exe"), &bin.join("deckd.exe"))?;
    replace(&staged.join("OpenWalkieTalkie.exe"), &exe)?;
    Ok(exe)
}

/// Whether the service answers on its door.
fn service_answers(deckd: &Path) -> bool {
    hidden(deckd)
        .args(["ctl", "--timeout", "2", "session.ping"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Stop the old service, start the new one, and open the new window.
fn relaunch(exe: &Path) -> Result<(), String> {
    let bin = exe.parent().ok_or("this app is in no folder")?;
    let deckd = bin.join("deckd.exe");
    let was_running = service_answers(&deckd);
    if was_running {
        let _ = hidden(&deckd)
            .args(["ctl", "--scopes", "admin", "--timeout", "5", "service.stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let since = Instant::now();
        while service_answers(&deckd) && since.elapsed() < LET_GO {
            std::thread::sleep(Duration::from_millis(500));
        }
        // Its lock goes with its process, a moment after the door.
        std::thread::sleep(Duration::from_secs(1));
        Command::new(exe)
            .arg(crate::service_host::FLAG)
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start the new service: {e}"))?;
    }
    Command::new(exe)
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot open the new window: {e}"))
}

/// The whole update to `version`, blocking. `Ok` means the new copy runs
/// and this one should quit.
pub fn run(version: &str) -> Result<(), String> {
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err("an update is already under way".into());
    }
    let result = stage(version).and_then(|staged| install(&staged)).and_then(|exe| relaunch(&exe));
    BUSY.store(false, Ordering::Release);
    result
}

/// What an earlier update left: the renamed programs beside this one, and
/// the unpacked zips. Whatever is still in use stays for the next start.
pub fn clean() {
    if let Some(bin) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        if let Ok(entries) = std::fs::read_dir(&bin) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains(".exe.old") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let _ = std::fs::remove_dir_all(PathBuf::from(local).join(r"OpenWalkieTalkie\caches\app-update"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_sum_is_read_by_the_files_name() {
        let sums = "aa11  OpenWalkieTalkie-0.15.0-macos-aarch64.dmg\n\
                    BB22  OpenWalkieTalkie-0.15.0-windows-x86_64.zip\n\
                    cc33 *OpenWalkieTalkie-0.15.0-hw-a.bin\n";
        assert_eq!(listed_sum(sums, &zip_name("0.15.0")).as_deref(), Some("bb22"));
        assert_eq!(listed_sum(sums, "OpenWalkieTalkie-0.15.0-hw-a.bin").as_deref(), Some("cc33"));
        assert_eq!(listed_sum(sums, "missing.zip"), None);
    }

    /// A running program's file is renamed aside and the new one put in its
    /// place; the old copy is found and deleted by the next start's clean.
    #[test]
    fn a_file_in_use_is_moved_aside_and_replaced() {
        let dir = std::env::temp_dir().join(format!("owt-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("deckd.exe");
        let new = dir.join("new.exe");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        // Held open, as a running program's image is.
        let held = std::fs::File::open(&target).unwrap();
        replace(&new, &target).unwrap();
        drop(held);
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("deckd.exe.old-0")).unwrap(), b"old");
        // A second update finds the first one's name taken.
        assert_eq!(aside(&target), dir.join("deckd.exe.old-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
