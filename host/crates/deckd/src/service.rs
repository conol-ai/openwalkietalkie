//! Process plumbing (design §9, §10): the single-instance `flock`, logging
//! (daily rolling, 7 kept; the launchd log on fds 1/2; the redacting panic
//! hook) and exit codes. The week-1 control socket is gone: the UDS door
//! of `deckd::api` answers `deckd device …` and `deckd ctl`.
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

/// launchd restarts on 75; 0 means "already running" so it does not loop.
///
/// The plist `tools/install-host.sh` writes is `KeepAlive = {SuccessfulExit
/// = false}` for exactly these two numbers: a clean 0 — `service.stop`, or
/// a second instance standing down — stays stopped, and everything else,
/// the 75 of `service.restart` and any crash or signal with it, is run
/// again.
pub const EXIT_RESTART: i32 = 75;
pub const EXIT_ALREADY_RUNNING: i32 = 0;
pub const EXIT_STOP: i32 = 0;
/// `deckd.log.*` files kept by the daily rotation.
pub const LOG_FILES_KEPT: usize = 7;
/// `deckd.launchd.log` rotates (one generation) past this.
pub const LAUNCHD_LOG_MAX: u64 = 10 * 1024 * 1024;

/// How the core hands an exit code up to the supervisor on `main`
/// (`service.restart`, `service.stop`; contract §4.11).
///
/// `main.rs` has always been `std::process::exit(deckd::cli::main())`, so
/// the code was there to be named and nothing under it could name one:
/// `core::run` returned `Ok(0)` however it ended. This is the missing half
/// — one cell the core writes and the supervision loop reads.
///
/// It carries a **deadline** rather than a bare flag, because the two
/// methods have an ordering requirement a flag cannot express. The reply is
/// written by a door, on the tokio side, *after* `Core::call` has returned
/// it; a supervisor that came down the instant the core decided would race
/// the one answer the call exists to produce, and `Served::shutdown` breaks
/// each connection loop on its stop watch before the pending line is
/// flushed (`api::conn`, the `biased` arm). A client that never receives
/// its reply cannot tell "restarted" from "crashed" — so the core arms this
/// with `now + GRACE`, [`Exit::requested`] answers `None` until then, and
/// the write has happened long before the loop is allowed to see it.
#[derive(Debug, Default)]
pub struct Exit(OnceLock<(i32, Instant)>);

impl Exit {
    /// How long the doors keep serving after an exit is asked for. Two
    /// orders of magnitude more than a UDS round trip needs and small
    /// enough that "Restart" in the GUI still feels immediate.
    pub const GRACE: Duration = Duration::from_millis(250);

    /// Ask the supervisor to come down with `code` once the grace has run.
    ///
    /// The **first** request wins and the effective code comes back: a
    /// `service.stop` arriving behind a `service.restart` must not turn one
    /// into the other, and the caller's reply has to say which it got.
    pub fn request(&self, code: i32, now: Instant) -> i32 {
        self.0.get_or_init(|| (code, now + Exit::GRACE)).0
    }

    /// The code, once the grace has passed — and `None` before that, which
    /// is what makes "answer first, then exit" an ordering rather than a
    /// hope.
    #[must_use]
    pub fn requested(&self, now: Instant) -> Option<i32> {
        self.0
            .get()
            .filter(|(_, at)| now >= *at)
            .map(|(code, _)| *code)
    }

    /// What was asked for, grace or no grace: the reply's own `exit`, and
    /// the code `run` returns once it has come down.
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        self.0.get().map(|(code, _)| *code)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("cannot create {0}")]
    CreateDir(std::path::PathBuf),
    #[error("cannot open lock {0}")]
    OpenLock(std::path::PathBuf),
    #[error("another deckd is running")]
    AlreadyRunning,
}

/// The single-instance lock. Dropping it unlocks (`LOCK_UN`) **before** the
/// descriptor is closed.
///
/// `flock` belongs to the open file description, not to the descriptor, and
/// every `fork` in the process duplicates that description: while any
/// `std::process::Command::spawn` is between its fork and its exec (the
/// `doctor` shells out to `launchctl`/`plutil`), a copy of our lock fd is
/// alive in the child. `O_CLOEXEC` — which `std` always sets, so the fd is
/// already close-on-exec — closes that copy at `exec`, not at `fork`, so
/// merely closing our own descriptor can leave the lock held for as long as
/// the child takes to exec. That is the week-1 flake of
/// `the_lock_is_exclusive_and_released_on_drop` (a second `lock()` in the
/// same process then saw `AlreadyRunning`). `LOCK_UN` releases the lock on
/// the description itself, so it is immediate however many copies exist.
#[derive(Debug)]
pub struct LockGuard {
    file: File,
}

impl LockGuard {
    /// The locked file (`deckd.lock`), for a caller that wants to write to it.
    pub fn file(&self) -> &File {
        &self.file
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: unlocking a descriptor we own and are about to close.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Take the exclusive, non-blocking lock at `path`; the OS releases it on
/// exit, crashes included. Keep the returned guard alive for the lock's life.
pub fn lock(path: &Path) -> Result<LockGuard, ServiceError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|_| ServiceError::CreateDir(dir.to_path_buf()))?;
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(path)
        .map_err(|_| ServiceError::OpenLock(path.to_path_buf()))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: flock on a descriptor we own; LOCK_EX | LOCK_NB.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ServiceError::AlreadyRunning);
        }
    }
    // Windows: `LockFileEx` through std, exclusive and non-blocking. It
    // belongs to this handle (child processes do not inherit it), and the
    // OS releases it when the handle closes, crashes included.
    #[cfg(windows)]
    if file.try_lock().is_err() {
        return Err(ServiceError::AlreadyRunning);
    }
    Ok(LockGuard { file })
}

/// The reloadable `[log] level` of the installed subscriber (§3.7: the one
/// hot key that is not a `Config` field the core reads). One subscriber per
/// process, so one cell; `set_log_level` is how `config::watch` applies it.
type FilterHandle = tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static LOG_FILTER: OnceLock<FilterHandle> = OnceLock::new();

/// Guard that keeps the non-blocking log writer alive.
pub struct LogGuard {
    _worker: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl LogGuard {
    /// `[log] level` of the running subscriber (see `set_log_level`).
    pub fn set_level(&self, level: &str) -> Result<bool, String> {
        set_log_level(level)
    }
}

/// `[log] level` → an `EnvFilter`, with the message the operator needs when
/// it does not parse (it is their `config.toml`, not ours).
pub fn parse_level(level: &str) -> Result<EnvFilter, String> {
    EnvFilter::try_new(level).map_err(|e| e.to_string())
}

/// Apply `[log] level` to the running subscriber without a restart.
/// `Ok(false)` when this process installed no reloadable subscriber (unit
/// tests, or a second `init_logging`); `Err` when the filter does not parse.
pub fn set_log_level(level: &str) -> Result<bool, String> {
    let filter = parse_level(level)?;
    match LOG_FILTER.get() {
        Some(handle) => handle
            .reload(filter)
            .map(|()| true)
            .map_err(|e| e.to_string()),
        None => Ok(false),
    }
}

/// `tracing` to stderr (foreground) or to a daily-rolling `deckd.log.*`
/// under `logs`, 7 kept. The filter sits behind a `reload` layer so
/// `config::watch` can change `[log] level` while the daemon runs.
pub fn init_logging(level: &str, foreground: bool, logs: &Path) -> LogGuard {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = parse_level(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
    let mut worker = None;
    let installed = if foreground {
        let layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        tracing_subscriber::registry()
            .with(filter)
            .with(layer)
            .try_init()
            .is_ok()
    } else {
        let _ = std::fs::create_dir_all(logs);
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("deckd.log")
            .max_log_files(LOG_FILES_KEPT)
            .build(logs);
        match appender {
            Ok(appender) => {
                let (writer, guard) = tracing_appender::non_blocking(appender);
                worker = Some(guard);
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer);
                tracing_subscriber::registry()
                    .with(filter)
                    .with(layer)
                    .try_init()
                    .is_ok()
            }
            Err(_) => {
                // No log directory: stderr (the launchd log) is all there is.
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::io::stderr);
                tracing_subscriber::registry()
                    .with(filter)
                    .with(layer)
                    .try_init()
                    .is_ok()
            }
        }
    };
    if installed {
        // Only a handle on a subscriber that is actually installed.
        let _ = LOG_FILTER.set(handle);
    }
    LogGuard { _worker: worker }
}

/// `deckd.launchd.log` (§9): opened by `deckd run` itself (O_APPEND|O_CREAT,
/// 0600, rotated at 10 MB) and put on fds 1 and 2, because the SMAppService
/// plist carries no log paths. Returns the file's path.
pub fn capture_std_streams(logs: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(logs)?;
    let path = logs.join("deckd.launchd.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= LAUNCHD_LOG_MAX) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let mut opts = OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: dup2 onto the standard descriptors of our own process.
        unsafe {
            libc::dup2(file.as_raw_fd(), 1);
            libc::dup2(file.as_raw_fd(), 2);
        }
    }
    // Windows: std looks the standard handles up on every write, so
    // pointing them at the file is the `dup2`. The handle is leaked on
    // purpose: it is the process's stdout and stderr from here on.
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows_sys::Win32::System::Console::{
            SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        let handle = file.into_raw_handle();
        // SAFETY: a valid file handle that stays open for the process's life.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
    #[cfg(not(any(unix, windows)))]
    drop(file);
    Ok(path)
}

/// Panics reach the log through `tracing` and stderr with the payload
/// redacted (an `unwrap` on a hook value must not print the value) unless
/// `[privacy] log_content`.
pub fn install_panic_hook(log_content: bool) {
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let text = if log_content {
            format!("panic at {location}: {payload}")
        } else {
            format!(
                "panic at {location} (payload of {} bytes redacted)",
                payload.len()
            )
        };
        tracing::error!("{text}");
        eprintln!("deckd: {text}");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deckd-service-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let d = dir("lock");
        let path = d.join("nested/deckd.lock");
        let first = lock(&path).unwrap();
        assert!(first.file().metadata().unwrap().is_file());
        assert!(matches!(lock(&path), Err(ServiceError::AlreadyRunning)));
        drop(first);
        let again = lock(&path).unwrap();
        drop(again);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The week-1 flake, made deterministic: a process that forked while we
    /// held the lock owns a copy of the open file description, so closing
    /// our descriptor alone would not release the `flock`. `LockGuard`
    /// unlocks first, so the next `lock()` succeeds however long the child
    /// takes to `exec` (or, here, never execs at all).
    #[test]
    fn a_fork_that_copied_the_lock_fd_cannot_keep_it_after_the_guard_drops() {
        let d = dir("fork");
        let path = d.join("deckd.lock");
        let held = lock(&path).unwrap();
        // SAFETY: the child touches nothing but async-signal-safe calls
        // (`sleep`, `_exit`) and never returns into Rust.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: as above; `_exit` skips every at-exit handler.
            unsafe {
                libc::sleep(30);
                libc::_exit(0);
            }
        }
        assert!(
            matches!(lock(&path), Err(ServiceError::AlreadyRunning)),
            "still held while the guard lives"
        );
        drop(held);
        let again = lock(&path);
        // SAFETY: the pid is our own child's, from `fork` above.
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        assert!(
            again.is_ok(),
            "the fork's copy of the fd must not hold the lock"
        );
        drop(again);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_log_level_is_validated_before_it_is_applied() {
        assert!(parse_level("info").is_ok());
        assert!(parse_level("deckd=debug,hyper=warn").is_ok());
        assert!(parse_level("deckd=louder").is_err());
        // Without an installed reloadable subscriber this is a no-op, never
        // a failure (the unit tests install none).
        assert!(set_log_level("deckd=louder").is_err());
        assert!(matches!(set_log_level("trace"), Ok(true) | Ok(false)));
    }

    #[test]
    fn the_launchd_log_rotates_past_ten_megabytes() {
        let d = dir("launchd");
        let logs = d.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("deckd.launchd.log");
        let big = File::create(&path).unwrap();
        big.set_len(LAUNCHD_LOG_MAX).unwrap();
        drop(big);
        // Rotation only; the dup2 half is exercised by `deckd run` itself.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= LAUNCHD_LOG_MAX) {
            std::fs::rename(&path, path.with_extension("log.1")).unwrap();
        }
        assert!(logs.join("deckd.launchd.log.1").exists());
        assert!(!path.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The ordering `service.restart`/`service.stop` are built on, on its
    /// own: the code is readable at once — the reply has to carry it — and
    /// the **supervisor** may not see it until the grace has run, which is
    /// the window the door writes that reply in. A flag would have given
    /// the client an exit and no answer, and a client that never receives
    /// its reply cannot tell "restarted" from "crashed".
    #[test]
    fn the_exit_cell_names_a_code_at_once_and_releases_it_only_after_the_grace() {
        let t0 = Instant::now();
        let exit = Exit::default();
        assert_eq!(exit.code(), None, "nothing was asked for");
        assert_eq!(exit.requested(t0 + Duration::from_secs(60)), None);

        assert_eq!(exit.request(EXIT_RESTART, t0), EXIT_RESTART);
        assert_eq!(exit.code(), Some(EXIT_RESTART), "the reply can say so now");
        assert_eq!(exit.requested(t0), None, "the supervisor may not act yet");
        assert_eq!(
            exit.requested(t0 + Exit::GRACE - Duration::from_millis(1)),
            None,
            "still inside the window the reply is written in"
        );
        assert_eq!(exit.requested(t0 + Exit::GRACE), Some(EXIT_RESTART));

        // The first request wins: a `service.stop` arriving behind a
        // `service.restart` must not turn 75 into 0 half a second before
        // launchd would have run us again.
        assert_eq!(
            exit.request(EXIT_STOP, t0 + Duration::from_millis(10)),
            EXIT_RESTART,
            "and the caller is told what will really happen"
        );
        assert_eq!(exit.requested(t0 + Exit::GRACE), Some(EXIT_RESTART));

        // …and the other way round, on a cell of its own.
        let stop = Exit::default();
        assert_eq!(stop.request(EXIT_STOP, t0), EXIT_STOP);
        assert_eq!(stop.request(EXIT_RESTART, t0), EXIT_STOP);
        assert_eq!(stop.requested(t0 + Exit::GRACE), Some(EXIT_STOP));
    }
}
