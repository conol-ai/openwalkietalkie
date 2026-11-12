//! `deckd run --no-device` answers the firmware methods without opening the
//! deck or fetching anything (B2), against the **real process**.
//!
//! The firmware check is the request that used to do both. It probes USB
//! from a thread of its own, beside the hid thread rather than through it,
//! so the hid thread's `hardware: false` never covered it, and then it
//! downloads the release manifest from GitHub into `caches/firmware`. The
//! app sends one on every connection, so a throwaway service the app was
//! pointed at opened the operator's own deck and kept a manifest it had
//! fetched. What is asserted here is what a client and a reviewer can see:
//! the answer is the idle status, the status stays idle, nothing appears
//! under `caches/firmware`, and an install is refused with its reason.
//!
//! The daemon is harmless even if that regressed: its own data dir with
//! `[api] port = 0`, `HOME`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME` inside
//! it, no usage probe, no liveness poller, no Codex client, and every proxy
//! variable pointed at a closed local port, so a curl has nowhere to go.
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

/// How long the daemon gets to bind its socket, and a reply to arrive.
const PATIENCE: Duration = Duration::from_secs(10);

/// A proxy nobody answers: the discard port on loopback.
const DEAD_PROXY: &str = "http://127.0.0.1:9";

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
        let dir = std::env::temp_dir().join(format!("dkn{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for home in ["home/claude", "home/codex"] {
            std::fs::create_dir_all(dir.join(home)).expect("a temp data dir and its homes");
        }
        std::fs::write(
            dir.join("config.toml"),
            "[api]\nport = 0\n\n[log]\nlevel = \"warn\"\n\n[codex]\ndaemon = false\n\n\
             [liveness]\nenabled = false\n\n[usage]\nproviders = []\n",
        )
        .expect("the daemon's config");
        let mut command = Command::new(deckd());
        command
            .arg("--data-dir")
            .arg(&dir)
            .args(["run", "--foreground", "--no-device"])
            .env("HOME", dir.join("home"))
            .env("CLAUDE_CONFIG_DIR", dir.join("home/claude"))
            .env("CODEX_HOME", dir.join("home/codex"))
            // Never mistaken for the service launchd runs, which is the
            // one `care.checks` asks launchd about.
            .env_remove("XPC_SERVICE_NAME")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for proxy in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
            command.env(proxy, DEAD_PROXY);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{tag}: the deckd binary runs: {e}"));
        let daemon = Daemon { child, dir };
        let deadline = Instant::now() + PATIENCE;
        while !daemon.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "{tag}: the uds door never came up in {PATIENCE:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("deckd.sock")
    }

    /// Where a check keeps the release manifest it fetched.
    fn firmware_cache(&self) -> PathBuf {
        self.dir.join("caches").join("firmware")
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

/// One `session.hello` + one call, over the socket, as `deckd ctl` does.
/// Returns the call's whole JSON-RPC response.
fn call(
    socket: &Path,
    scopes: &[&str],
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
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
        "jsonrpc": "2.0", "id": 2, "method": method, "params": params,
    }))
}

/// B2: the check the app sends on every connection is answered with the
/// idle status, and nothing is fetched: no manifest, not even its directory.
#[test]
fn a_no_device_daemon_answers_a_firmware_check_with_its_idle_status_and_fetches_nothing() {
    let daemon = Daemon::start("check");
    let check = call(
        &daemon.socket(),
        &["admin"],
        "firmware.check",
        serde_json::json!({"force": true}),
    );
    assert!(check["error"].is_null(), "{check}");
    let status = &check["result"];
    assert_eq!(
        status["phase"], "idle",
        "a started check says `checking`: {check}"
    );
    assert!(status["device"].is_null(), "{check}");
    assert!(status["release"].is_null(), "{check}");
    assert_eq!(status["available"], false, "{check}");
    // A check that had started would have moved on by now, and a fetched
    // manifest would be on its way to the cache.
    std::thread::sleep(Duration::from_millis(300));
    let later = call(
        &daemon.socket(),
        &["observe"],
        "firmware.status",
        serde_json::json!({}),
    );
    assert_eq!(later["result"]["phase"], "idle", "{later}");
    let cache = daemon.firmware_cache();
    assert!(
        !cache.join("release-manifest.json").exists(),
        "a release manifest was fetched into {}",
        cache.display()
    );
    assert!(
        !cache.exists(),
        "nothing is written under {}",
        cache.display()
    );
}

/// An install is refused with a reason that names the mode, rather than
/// "check with your deck connected first", which no check on this daemon
/// could ever satisfy, and nothing is written for it.
#[test]
fn a_no_device_daemon_refuses_a_firmware_install_and_says_why() {
    let daemon = Daemon::start("install");
    let install = call(
        &daemon.socket(),
        &["admin"],
        "firmware.install",
        serde_json::json!({
            "board": "hw-b", "serial": "X", "version": "9.9.9", "allow_bootloader_update": true,
        }),
    );
    assert_eq!(
        install["error"]["code"],
        deck_model::ApiError::INVALID,
        "{install}"
    );
    assert_eq!(
        install["error"]["data"]["why"],
        deckd::firmware::NO_DEVICE,
        "{install}"
    );
    assert!(!daemon.firmware_cache().exists());
}

/// O16 against the real process: `care.checks` on a `--no-device` service
/// does not enumerate HID for its `device` row, and says so; the doctor's
/// lock and socket rows reach this very daemon from its blocking pool (the
/// lock is held, the socket answers); the two rows the doctor lacks are
/// there, the Codex one not checked while its client is off; and launchd's
/// rows are not checked either, because launchd did not start it.
#[test]
fn a_no_device_daemon_checks_its_setup_without_looking_for_the_deck() {
    let daemon = Daemon::start("checks");
    let reply = call(
        &daemon.socket(),
        &["admin"],
        "care.checks",
        serde_json::json!({}),
    );
    assert!(reply["error"].is_null(), "{reply}");
    let rows = reply["result"]["checks"].as_array().expect("the rows");
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("no {name:?} row: {reply}"))
    };
    assert_eq!(row("device")["ok"], serde_json::Value::Null, "{reply}");
    assert_eq!(
        row("device")["detail"],
        "not looked for: this service runs with --no-device"
    );
    assert_eq!(row("service")["detail"], "running", "{reply}");
    assert_eq!(row("socket")["ok"], true, "{reply}");
    assert_eq!(row("data dir")["detail"], daemon.dir.display().to_string());
    assert_eq!(row("codex daemon")["ok"], serde_json::Value::Null, "{reply}");
    // launchd did not start this service, so launchd is not asked about
    // the installed one and `~/Library/LaunchAgents` is not read.
    for name in ["launchd deckd", "legacy launch agent"] {
        assert_eq!(row(name)["ok"], serde_json::Value::Null, "{reply}");
        assert_eq!(
            row(name)["detail"],
            "not looked at: launchd did not start this service, so its agent is not the one \
             answering",
            "{reply}"
        );
    }
}

/// `care.footprint` and `care.cache.clear` from the real process: the ten
/// items in the service's order, the service being this very binary, the
/// data its own `--data-dir` and the Codex Desktop launcher absent from its
/// own `HOME`; then the firmware downloads go and the bootloader backups
/// stay.
#[test]
fn a_no_device_daemon_measures_and_clears_in_its_own_dirs() {
    let daemon = Daemon::start("footprint");
    let cache = daemon.firmware_cache();
    std::fs::create_dir_all(cache.join("bootloader-backups")).expect("the cache");
    std::fs::write(cache.join("release-manifest.json"), b"{\"a\":1}").expect("a download");
    std::fs::write(cache.join("bootloader-backups/F657.bin"), b"boot").expect("a backup");
    let reply = call(
        &daemon.socket(),
        &["admin"],
        "care.footprint",
        serde_json::json!({}),
    );
    assert!(reply["error"].is_null(), "{reply}");
    let items = reply["result"]["items"].as_array().expect("the items");
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "service",
            "claude_plugin",
            "codex_hooks",
            "stt",
            "kokoro",
            "firmware",
            "data",
            "prefs",
            "logs",
            "codex_desktop"
        ]
    );
    let item = |id: &str| items.iter().find(|i| i["id"] == id).unwrap();
    let real = |path: &str| std::fs::canonicalize(path).expect("a path on disk");
    assert_eq!(item("service")["present"], true, "{reply}");
    assert_eq!(
        real(item("service")["path"].as_str().unwrap()),
        real(deckd())
    );
    assert_eq!(item("data")["path"], daemon.dir.display().to_string());
    assert_eq!(item("firmware")["bytes"], 7, "the backup is not counted");
    assert_eq!(item("prefs")["present"], false, "this home has none");
    assert_eq!(item("codex_desktop")["present"], false, "{reply}");
    assert_eq!(
        item("codex_desktop")["path"],
        daemon
            .dir
            .join("home/Applications/Codex with OpenWalkieTalkie.app")
            .display()
            .to_string()
    );
    assert!(reply["result"]["total_bytes"].is_u64(), "{reply}");
    let cleared = call(
        &daemon.socket(),
        &["admin"],
        "care.cache.clear",
        serde_json::json!({"what": "firmware"}),
    );
    assert_eq!(cleared["result"], serde_json::json!({"removed_bytes": 7}), "{cleared}");
    assert!(!cache.join("release-manifest.json").exists());
    assert!(cache.join("bootloader-backups/F657.bin").exists());
}
