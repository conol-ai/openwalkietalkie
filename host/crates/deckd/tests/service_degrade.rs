//! §4.1 D8's port-degrade flow, end to end, against the **real process**
//! (HOST-CONTRACT-W7 §4, §6's "port collision keeps the deck alive" row).
//!
//! The unit half of this is already in `api::http`: a squatter makes the
//! bind fail, the daemon degrades, a foreign `/v1/info` becomes
//! `hooks_hijack_risk`. What that cannot say is the part the design actually
//! promises the operator:
//!
//! * the daemon **stays up and keeps the deck**. D8's whole reason to exist
//!   is the operator who must not lose the deck because something else
//!   took 47201. Here that is a real `deckd run`
//!   whose UDS door and hid thread go on answering with the TCP door down.
//! * the banner has something to name. `service.degraded{reason, port,
//!   listener}` and `service.info.listener` carry the squatter's **pid and
//!   command**, and this test is the squatter, so the pid it asserts is its
//!   own — not a shape, an identity.
//! * the repair works. §4.1 says the fix is `config.set{api.port}` then
//!   `integrations.repair claude`, and the test runs exactly those two,
//!   through the CLI, against a throwaway `CLAUDE_CONFIG_DIR`, and then
//!   restarts the daemon the way launchd would and watches the degradation
//!   clear.
//!
//! Nothing here touches the real data dir, port 47201, the operator's
//! `~/.claude`/`~/.codex` or their LaunchAgents: the daemon runs
//! `--no-device` on a fresh `$TMPDIR` directory, and the port under test is
//! an ephemeral one this process holds first.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn deckd() -> &'static str {
    env!("CARGO_BIN_EXE_deckd")
}

const PATIENCE: Duration = Duration::from_secs(10);
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A daemon on a data dir of its own, on a named port, killed and removed on
/// drop however the test ends. The name is short: `sun_path` holds 103 bytes
/// and `$TMPDIR` is already ~50 of them on macOS.
struct Daemon {
    child: Option<Child>,
    dir: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
    home: PathBuf,
}

impl Daemon {
    fn start(tag: &str, port: u16) -> Daemon {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dkg{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp data dir");
        let mut daemon = Daemon {
            child: None,
            claude: sub(&dir, "claude"),
            codex: sub(&dir, "codex"),
            home: sub(&dir, "home"),
            dir,
        };
        daemon.write_config(port);
        daemon.spawn(tag);
        daemon
    }

    /// The TCP door switched on: it is off unless the file says so, and its
    /// degrade is what these tests are about. No usage probe: it would read
    /// the operator's Claude login from the Keychain (redirecting `HOME`
    /// does not move the Keychain) and ask api.anthropic.com.
    fn write_config(&self, port: u16) {
        std::fs::write(
            self.dir.join("config.toml"),
            format!(
                "[api]\nport = {port}\ntcp = true\n\n[log]\nlevel = \"warn\"\n\n[usage]\nproviders = []\n"
            ),
        )
        .expect("the daemon's config");
    }

    fn spawn(&mut self, tag: &str) {
        let child = self
            .command(&["run", "--foreground", "--no-device"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{tag}: the deckd binary runs: {e}"));
        self.child = Some(child);
        let deadline = Instant::now() + PATIENCE;
        while !self.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "{tag}: the uds door never came up in {PATIENCE:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stop the daemon and start it again — what launchd does after an
    /// exit 75, and what `[api] port` (not a hot key) needs before a new
    /// port takes effect.
    fn restart(&mut self, tag: &str) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(self.socket());
        self.spawn(tag);
    }

    /// A `deckd` invocation on this daemon's data dir, with the throwaway
    /// CLI homes. `HOME` is redirected too, so nothing can reach the
    /// operator's own `~/.claude` even if an env var were missed.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(deckd());
        cmd.arg("--data-dir")
            .arg(&self.dir)
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", &self.codex)
            .env("HOME", &self.home)
            .stdin(Stdio::null());
        cmd
    }

    /// Run a `deckd` subcommand to completion; returns its stdout.
    fn cli(&self, tag: &str, args: &[&str]) -> String {
        let out = self
            .command(args)
            .output()
            .unwrap_or_else(|e| panic!("{tag}: deckd {args:?}: {e}"));
        assert!(
            out.status.success(),
            "{tag}: deckd {args:?} exited {}: {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("deckd.sock")
    }

    fn port_file(&self) -> String {
        std::fs::read_to_string(self.dir.join("port")).unwrap_or_default()
    }

    fn settings(&self) -> String {
        std::fs::read_to_string(self.claude.join("settings.json")).unwrap_or_default()
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sub(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).expect("a sub-directory");
    path
}

/// One `session.hello` + one call over the UDS, as `deckd ctl` does.
fn call(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
    scopes: &[&str],
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

fn info(socket: &Path) -> serde_json::Value {
    let response = call(socket, "service.info", serde_json::json!({}), &["observe"]);
    assert!(response["error"].is_null(), "{response}");
    response["result"].clone()
}

/// Poll `service.info` until `degraded` is what we are waiting for.
fn wait_degraded(socket: &Path, want: Option<&str>) -> serde_json::Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let info = info(socket);
        let got = info["degraded"].as_str();
        if got == want {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "degraded is {got:?}, waited {PATIENCE:?} for {want:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A port this process holds and keeps holding: the squatter D8 is about.
fn squat() -> (TcpListener, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port to squat");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// A port nothing holds — bound, read, released.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn a_squatted_port_degrades_the_tcp_door_and_names_the_squatter_by_pid() {
    let (_squatter, port) = squat();
    let daemon = Daemon::start("degrade", port);

    let info = wait_degraded(&daemon.socket(), Some("port_in_use"));
    // The port is hidden while it is somebody else's: reporting it would
    // send a client — and Claude's hooks — to the squatter.
    assert_eq!(info["port"], serde_json::Value::Null, "{info}");
    assert_eq!(
        daemon.port_file(),
        "",
        "the port file is empty while degraded"
    );
    // And the banner has something to say: our own pid, because this test
    // process is the squatter.
    assert_eq!(
        info["listener"]["pid"].as_u64(),
        Some(u64::from(std::process::id())),
        "the listener is this test process: {info}"
    );
    assert!(
        info["listener"]["command"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "and it is named: {info}"
    );
}

/// D8's reason to exist: **never exit**. The operator keeps the deck
/// whatever is on 47201, so a bind failure leaves the UDS door and the hid
/// thread — the deck's own channel — answering.
///
/// What this proves is that both survive the failed bind and go on taking
/// work; it is a `--no-device` daemon, so no real deck was written to.
#[test]
fn a_degraded_daemon_keeps_the_uds_door_and_the_deck_channel_alive() {
    let (_squatter, port) = squat();
    let mut daemon = Daemon::start("degrade-alive", port);
    wait_degraded(&daemon.socket(), Some("port_in_use"));

    // The door that the GUI and `deckd ctl` use.
    let ping = call(&daemon.socket(), "session.ping", serde_json::json!({}), &[]);
    assert!(ping["error"].is_null(), "{ping}");
    // The hid thread: `metrics.get` is its counters and `device.sync` puts a
    // barrier on its queue. Both answer, so the deck's channel is up with
    // the TCP door down.
    let metrics = call(
        &daemon.socket(),
        "metrics.get",
        serde_json::json!({}),
        &["admin"],
    );
    assert!(metrics["error"].is_null(), "{metrics}");
    let sync = call(
        &daemon.socket(),
        "device.sync",
        serde_json::json!({}),
        &["admin"],
    );
    assert!(sync["error"].is_null(), "{sync}");
    // Well past the door's retry, and still there.
    std::thread::sleep(Duration::from_millis(500));
    assert!(daemon.alive(), "the daemon exited over a taken port");
    assert_eq!(info(&daemon.socket())["degraded"], "port_in_use");
}

/// The event the banner and the tray dot are drawn from (§4.12):
/// `service.degraded{reason, port, listener}` with the squatter in it,
/// replayable from the ring so a GUI that connects *after* the bind failed
/// still learns about it.
#[test]
fn the_service_degraded_event_carries_the_reason_the_port_and_the_listener() {
    let (_squatter, port) = squat();
    let daemon = Daemon::start("degrade-event", port);
    wait_degraded(&daemon.socket(), Some("port_in_use"));

    let replay = call(
        &daemon.socket(),
        "state.events",
        serde_json::json!({"since": 0, "types": ["service.degraded"]}),
        &["observe"],
    );
    assert!(replay["error"].is_null(), "{replay}");
    let events = replay["result"]["events"]
        .as_array()
        .expect("an events array");
    let event = events
        .iter()
        .find(|e| e["data"]["reason"] == "port_in_use")
        .unwrap_or_else(|| panic!("no service.degraded in {events:#?}"));
    assert_eq!(event["type"], "service.degraded");
    assert_eq!(event["data"]["port"], port);
    assert_eq!(
        event["data"]["listener"]["pid"].as_u64(),
        Some(u64::from(std::process::id())),
        "{event}"
    );
}

/// §4.1's repair, as the Integrations card runs it: `config.set{api.port}`
/// then `integrations.repair claude`, then the restart launchd does after
/// the `service.restart` the GUI calls next. Since the Claude integration
/// became a plugin of command hooks, nothing deckd installs names a port at
/// all — the repair has nothing to move and says so — and the restart alone
/// binds the new port and clears the degradation while the squatter keeps
/// its own.
#[test]
fn change_the_port_and_repair_the_hooks_is_the_way_out_of_a_squatted_port() {
    let (_squatter, taken) = squat();
    let mut daemon = Daemon::start("degrade-repair", taken);
    wait_degraded(&daemon.socket(), Some("port_in_use"));

    // The plugin as it stands: installed while the port is not ours, and
    // naming no port anywhere.
    daemon.cli("repair", &["integrations", "install", "claude"]);
    let status = daemon.cli("repair", &["integrations", "status", "claude", "--json"]);
    let reports: serde_json::Value = serde_json::from_str(status.trim()).expect("status json");
    let files: Vec<String> = reports[0]["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|f| f.as_str().map(String::from))
        .collect();
    let plugin_files: Vec<&String> = files
        .iter()
        .filter(|f| f.contains("claude-marketplace"))
        .collect();
    // Marketplace, plugin manifest and command hooks. A new install has no
    // stdio MCP config: the Claude Channels helper waits for the owner's
    // switch (`[sources] claude_channels`).
    assert_eq!(plugin_files.len(), 3, "{files:?}");
    for file in &plugin_files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            !text.contains(&format!(":{taken}")),
            "{file} names the squatted port: {text}"
        );
        assert!(
            !text.contains("/v1/hooks/"),
            "{file} carries an http hook: {text}"
        );
    }
    assert!(
        !daemon.settings().contains("/v1/hooks/claude"),
        "nothing of ours in settings.json: {}",
        daemon.settings()
    );

    // Step 1: `config.set{api.port}`. It is not a hot key, so the daemon
    // says so rather than claiming it took effect.
    let free = free_port();
    let set = call(
        &daemon.socket(),
        "config.set",
        serde_json::json!({"patch": {"api.port": free}}),
        &["admin"],
    );
    assert!(set["error"].is_null(), "{set}");
    assert_eq!(
        set["result"]["restart_needed"],
        serde_json::json!(["api.port"]),
        "{set}"
    );

    // Step 2: `integrations.repair claude` has nothing port-shaped to
    // rewrite and rewrites nothing.
    let out = daemon.cli("repair", &["integrations", "repair", "claude"]);
    assert!(
        !out.contains("wrote "),
        "a port change is not a plugin change any more: {out}"
    );

    // Step 3: the restart the GUI asks for next. The squatter still holds
    // its port; the daemon takes the new one and stops being degraded.
    daemon.restart("degrade-repair");
    let info = wait_degraded(&daemon.socket(), None);
    assert_eq!(info["port"], free, "{info}");
    assert_eq!(info["listener"], serde_json::Value::Null, "{info}");
    assert_eq!(daemon.port_file().trim(), free.to_string());
}
