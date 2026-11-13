//! `service.restart` / `service.stop` against the **real process**
//! (contract §4.11, week 6).
//!
//! The thing under test is a process fact — "`deckd run` exits 75, and it
//! answered first" — and no in-process rig can speak for it: `main.rs` is
//! `std::process::exit(deckd::cli::main())`, and what the core does with
//! `service::Exit` only matters because that number reaches `exit(2)`. So
//! this spawns the binary the way launchd does, calls the method over its
//! own Unix socket, reads the reply **while the process is still alive**,
//! and then waits for the code.
//!
//! The ordering is the point. A supervisor that came down the instant the
//! core decided would race the one answer the call exists to produce, and a
//! client that never receives its reply cannot tell "restarted" from
//! "crashed" — the tray would have to guess. `service::Exit` holds the code
//! back for `Exit::GRACE`; here that is observed from the outside, as the
//! reply arriving before the exit rather than as a cell's value.
//!
//! Nothing here touches the real data dir, port 47201, the user's
//! LaunchAgent or their `~/.claude`: `--data-dir` is a fresh `$TMPDIR`
//! directory with `[api] port = 0` and `HOME` and the CLI homes inside it,
//! and the daemon runs `--no-device`.
//!
//! The daemon checks its peers with the real verifier, so where this test
//! runs inside Claude Code or Codex (an agent running `cargo test`), §4.2's
//! ancestry cap refuses it `admin`: the two exits are then checked as that
//! refusal and a service that stays up, and CI checks the exits themselves.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The binary under test.
fn deckd() -> &'static str {
    env!("CARGO_BIN_EXE_deckd")
}

/// How long the daemon gets to bind its socket, and to exit afterwards.
const PATIENCE: Duration = Duration::from_secs(10);

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A daemon on a data dir of its own, killed and removed on drop however
/// the test ends. `sun_path` holds 103 bytes and `$TMPDIR` is already ~50
/// of them on macOS, so the name is short.
struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dkx{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The CLI homes inside it: a daemon whose file leaves `[api] tcp`
        // out reads Claude's settings at start (the pre-plugin hooks keep
        // the TCP door open), and the operator's are not this test's.
        for home in ["home/claude", "home/codex"] {
            std::fs::create_dir_all(dir.join(home)).expect("a temp data dir and its homes");
        }
        // Port 0: an ephemeral port, so a gate run never collides with the
        // operator's own daemon on 47201. No roster seeding and no Codex
        // daemon client: this daemon must not reach the operator's own live
        // sessions. No usage probe either: it would read the operator's
        // Claude login from the Keychain and ask api.anthropic.com.
        std::fs::write(
            dir.join("config.toml"),
            "[api]\nport = 0\n\n[log]\nlevel = \"warn\"\n\n[liveness]\nseed = false\n\n[codex]\ndaemon = false\n\n[usage]\nproviders = []\n",
        )
        .expect("the daemon's config");
        let child = Command::new(deckd())
            .arg("--data-dir")
            .arg(&dir)
            .args(["run", "--foreground", "--no-device"])
            .env("HOME", dir.join("home"))
            .env("CLAUDE_CONFIG_DIR", dir.join("home/claude"))
            .env("CODEX_HOME", dir.join("home/codex"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{tag}: the deckd binary runs: {e}"));
        let daemon = Daemon { child, dir };
        daemon.wait_for_socket(tag);
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("deckd.sock")
    }

    fn wait_for_socket(&self, tag: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "{tag}: the uds door never came up in {PATIENCE:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Still running?
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The exit code, or a failure naming how long it waited.
    fn code(&mut self, tag: &str) -> i32 {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code().unwrap_or(-1),
                Ok(None) => {
                    assert!(
                        Instant::now() < deadline,
                        "{tag}: still running {PATIENCE:?} after the call"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{tag}: {e}"),
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A failed assertion must not leave a daemon (and its flock) behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether this test process runs inside Claude Code or Codex: the macOS
/// service then refuses it `admin` (§4.2's ancestry cap, `reason:
/// "agent_ancestry"`).
fn capped() -> bool {
    deckd::trust::ancestry_capped(std::process::id())
}

/// Where [`capped`]: a session asking for `admin` is refused by the cap,
/// and the daemon, asked for nothing, is still up well past the exit's
/// grace. The caller then stops. Elsewhere, false.
fn refused_by_the_cap(daemon: &mut Daemon) -> bool {
    if !capped() {
        return false;
    }
    let stream = UnixStream::connect(daemon.socket()).expect("the uds door");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    let mut writer = stream.try_clone().expect("a write half");
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "session.hello",
        "params": {"client": {"name": "gate", "version": "1"}, "scopes": ["admin"]},
    });
    writer
        .write_all(format!("{request}\n").as_bytes())
        .expect("the request is written");
    let mut text = String::new();
    BufReader::new(stream).read_line(&mut text).expect("a line");
    let hello: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not json: {text:?}"));
    assert_eq!(
        hello["error"]["code"],
        deck_model::ApiError::FORBIDDEN,
        "{hello}"
    );
    assert_eq!(
        hello["error"]["data"]["reason"], "agent_ancestry",
        "{hello}"
    );
    std::thread::sleep(deckd::service::Exit::GRACE * 4);
    assert!(daemon.alive(), "a refused session stops nothing");
    true
}

/// One `session.hello` + one call, over the socket, as `deckd ctl` does.
/// Returns the call's whole JSON-RPC response.
fn call(socket: &Path, method: &str, scopes: &[&str]) -> serde_json::Value {
    let stream = UnixStream::connect(socket).expect("the uds door");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    let mut writer = stream.try_clone().expect("a write half");
    let mut reader = BufReader::new(stream);
    let mut line = |request: serde_json::Value| -> serde_json::Value {
        writer
            .write_all(format!("{request}\n").as_bytes())
            .expect("the request is written");
        let id = request["id"].clone();
        loop {
            let mut text = String::new();
            let n = reader.read_line(&mut text).expect("a line");
            assert!(n > 0, "the door closed before answering {request}");
            let value: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| panic!("not json: {text:?}"));
            // Skip the events a subscribed session would see; these ask for
            // none, but `session.hello` still replays.
            if value["id"] == id {
                return value;
            }
        }
    };
    let hello = line(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "session.hello",
        "params": {"client": {"name": "gate", "version": "1"}, "scopes": scopes},
    }));
    assert!(hello["error"].is_null(), "session.hello: {hello}");
    line(serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": method, "params": {},
    }))
}

/// `service.restart` answers, and **then** the process exits 75 — which is
/// what launchd's `KeepAlive = {SuccessfulExit = false}` runs again.
#[test]
fn service_restart_answers_its_caller_and_then_the_process_exits_seventy_five() {
    let mut daemon = Daemon::start("restart");
    if refused_by_the_cap(&mut daemon) {
        return;
    }
    let response = call(&daemon.socket(), "service.restart", &["admin"]);

    // The reply is in hand, and the daemon is still there to have sent it.
    // This is the ordering: had the supervisor acted on the code when the
    // core set it, the door's stop watch would have broken this connection
    // with the line unwritten and the caller could not tell a restart from
    // a crash.
    assert!(response["error"].is_null(), "{response}");
    let result = &response["result"];
    assert_eq!(result["action"], "restart", "{response}");
    assert_eq!(result["exit"], deckd::service::EXIT_RESTART);
    assert_eq!(
        result["pid"].as_u64(),
        Some(u64::from(daemon.child.id())),
        "the daemon that answered is the one that is going"
    );
    assert!(
        daemon.alive(),
        "the reply was written by a process that had not exited"
    );

    assert_eq!(
        daemon.code("restart"),
        deckd::service::EXIT_RESTART,
        "launchd's `run me again`"
    );
    // And it left nothing behind for the next instance to trip over.
    assert!(!daemon.socket().exists(), "the socket is removed");
}

/// `service.stop` is the same path with a 0, which is the code launchd does
/// **not** run again — the tray's "Stop service".
#[test]
fn service_stop_answers_its_caller_and_then_the_process_exits_zero() {
    let mut daemon = Daemon::start("stop");
    if refused_by_the_cap(&mut daemon) {
        return;
    }
    let response = call(&daemon.socket(), "service.stop", &["admin"]);
    assert!(response["error"].is_null(), "{response}");
    assert_eq!(response["result"]["action"], "stop");
    assert_eq!(response["result"]["exit"], deckd::service::EXIT_STOP);
    assert!(daemon.alive(), "answered before it went");
    assert_eq!(daemon.code("stop"), deckd::service::EXIT_STOP);
}

/// Both are `admin`: a session that asked for `observe` is refused, and the
/// daemon stays up. A "Stop service" any observer could press would be a
/// denial of service with a menu item.
#[test]
fn a_session_without_admin_cannot_stop_the_service() {
    let mut daemon = Daemon::start("scope");
    let response = call(&daemon.socket(), "service.stop", &["observe"]);
    assert_eq!(
        response["error"]["code"],
        deck_model::ApiError::FORBIDDEN,
        "{response}"
    );
    assert_eq!(response["error"]["data"]["need"], "admin");
    // Well past the grace, and still serving.
    std::thread::sleep(deckd::service::Exit::GRACE * 4);
    assert!(daemon.alive(), "a refused call armed nothing");
    let info = call(&daemon.socket(), "service.info", &["observe"]);
    assert!(info["error"].is_null(), "{info}");
}
