//! The TCP door is off by default (the owner's answer to decision 12:
//! nobody outside this repository and Conol uses it), against the **real
//! process**: what `deckd run` decides at start and the line it logs, then
//! what the port file, `config.get` and `care.checks` say and whether
//! anything answers on the port.
//!
//! One case keeps the door open for a file that leaves `[api] tcp` out: the
//! HTTP hooks deckd gave Claude Code before its integration became a
//! plugin, which post to the door — found in Claude's settings, or named by
//! deckd's own pre-plugin record of the Claude target. Only `deckd run`
//! looks for them, in its own `CLAUDE_CONFIG_DIR` and data dir, so that rule
//! can only be held here. Every daemon is harmless: its own data
//! dir with `[api] port = 0` (never 47201), `HOME`, `CLAUDE_CONFIG_DIR` and
//! `CODEX_HOME` inside it, `DECKD_DATA_DIR` naming it for the methods that
//! resolve the data dir from the environment, no usage probe, no liveness
//! poller, no Codex client, and `--no-device`.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The binary under test.
fn deckd() -> &'static str {
    env!("CARGO_BIN_EXE_deckd")
}

/// How long the daemon gets to decide its door, and a reply to arrive.
const PATIENCE: Duration = Duration::from_secs(10);

static NEXT: AtomicU32 = AtomicU32::new(0);

/// Claude's settings on a machine that installed the integration before it
/// was a plugin and never repaired it: the user's own key, one of the old
/// http hooks on deckd's hook URL, and the allowlist pattern beside it.
const LEGACY_SETTINGS: &str = r#"{
  "model": "claude-haiku-4-5",
  "hooks": {
    "PermissionRequest": [
      {"hooks": [{"type": "http", "url": "http://127.0.0.1:47201/v1/hooks/claude", "timeout": 130}]}
    ]
  },
  "allowedHttpHookUrls": ["http://127.0.0.1:47201/v1/hooks/*"]
}
"#;

/// The same file once Repair has migrated it: the user's own key, nothing
/// of deckd's.
const MIGRATED_SETTINGS: &str = "{\n  \"model\": \"claude-haiku-4-5\"\n}\n";

/// A leftover of the old install that posts nothing: the allowlist pattern
/// alone, its hooks gone. The Claude row still asks for Repair; the door
/// has no one to keep it open for.
const PATTERN_ONLY_SETTINGS: &str =
    "{\n  \"allowedHttpHookUrls\": [\"http://127.0.0.1:47201/v1/hooks/*\"]\n}\n";

/// deckd's own `integrations.json` on a machine where the old installer
/// wrote the Claude hooks and nothing has installed, repaired or removed
/// the integration since: the pre-plugin inventory.
const PRE_PLUGIN_RECEIPT: &str = r#"{
  "claude": {
    "schema": 1,
    "deckd": "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
    "port": 47201,
    "url": "http://127.0.0.1:47201/v1/hooks/claude",
    "entries": ["SessionStart", "PermissionRequest", "Stop"],
    "allowlist": {"appended": true, "added": false, "patterns": ["http://127.0.0.1:47201/v1/hooks/*"]}
  }
}
"#;

/// The same record once the plugin was installed: the plugin era's.
const PLUGIN_RECEIPT: &str = r#"{
  "claude": {
    "schema": 2,
    "deckd": "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
    "plugin": "openwalkietalkie@openwalkietalkie",
    "events": ["Stop"]
  }
}
"#;

/// A daemon on a data dir of its own, killed and removed on drop however
/// the test ends. `sun_path` holds 103 bytes and `$TMPDIR` is already ~50
/// of them on macOS, so the name is short.
struct Daemon {
    child: Option<Child>,
    dir: PathBuf,
}

impl Daemon {
    /// `api` is what the `[api]` table holds beside `port = 0` (the switch,
    /// or nothing); `settings` is Claude's `settings.json`, or no file.
    fn start(tag: &str, api: &str, settings: Option<&str>) -> Daemon {
        let mut daemon = Daemon::prepare(api, settings);
        daemon.spawn(tag);
        daemon
    }

    /// [`Daemon::start`] without the start: the data dir and its files,
    /// for a test to add to before [`Daemon::spawn`].
    fn prepare(api: &str, settings: Option<&str>) -> Daemon {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dkp{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for home in ["home/claude", "home/codex"] {
            std::fs::create_dir_all(dir.join(home)).expect("a temp data dir and its homes");
        }
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[api]\nport = 0\n{api}\n[log]\nlevel = \"info\"\n\n[codex]\ndaemon = false\n\n\
                 [liveness]\nenabled = false\n\n[usage]\nproviders = []\n"
            ),
        )
        .expect("the daemon's config");
        let daemon = Daemon { child: None, dir };
        if let Some(settings) = settings {
            daemon.claude_settings(settings);
        }
        daemon
    }

    /// Write Claude's `settings.json`, the file the installer reads.
    fn claude_settings(&self, text: &str) {
        std::fs::write(self.dir.join("home/claude/settings.json"), text)
            .expect("Claude's settings");
    }

    /// Write deckd's own record of what it installed, `integrations.json`
    /// in the data dir.
    fn receipts(&self, text: &str) {
        std::fs::write(self.dir.join("integrations.json"), text).expect("deckd's receipts");
    }

    /// Start `deckd run` and wait until it has decided its door: the port
    /// file is written either way, with the port the door holds or empty.
    fn spawn(&mut self, tag: &str) {
        let log = std::fs::File::create(self.log_path()).expect("the daemon's log");
        let child = Command::new(deckd())
            .arg("--data-dir")
            .arg(&self.dir)
            .args(["run", "--foreground", "--no-device"])
            // `integrations.*` over the API resolves the data dir from the
            // environment, not from `--data-dir`.
            .env("DECKD_DATA_DIR", &self.dir)
            .env("HOME", self.dir.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.dir.join("home/claude"))
            .env("CODEX_HOME", self.dir.join("home/codex"))
            // Never mistaken for the service launchd runs.
            .env_remove("XPC_SERVICE_NAME")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap_or_else(|e| panic!("{tag}: the deckd binary runs: {e}"));
        self.child = Some(child);
        let deadline = Instant::now() + PATIENCE;
        while !(self.socket().exists() && self.dir.join("port").exists()) {
            assert!(
                Instant::now() < deadline,
                "{tag}: no socket and port file in {PATIENCE:?}: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stop the daemon and start it again, what launchd does after an exit.
    /// A killed daemon leaves its port file behind, so it goes first.
    fn restart(&mut self, tag: &str) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for file in ["deckd.sock", "port"] {
            let _ = std::fs::remove_file(self.dir.join(file));
        }
        self.spawn(tag);
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("deckd.sock")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("stderr.log")
    }

    /// What the daemon logged so far.
    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// The port the TCP door holds, as the port file says it.
    fn port(&self) -> Option<u16> {
        std::fs::read_to_string(self.dir.join("port"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
    }

    /// What the app reads about the door, over one session: `config.get`'s
    /// `api.tcp` (the Local API row's switch) and `api.tcp_rule` (why), the
    /// doctor's `tcp` row as `care.checks` answers it, and the Integrations
    /// page's Claude row, read from the files alone.
    fn said(&self) -> Said {
        let replies = calls(
            &self.socket(),
            &[
                ("config.get", serde_json::json!({})),
                ("care.checks", serde_json::json!({})),
                (
                    "integrations.status",
                    serde_json::json!({"target": "claude", "detect": false}),
                ),
            ],
        );
        for reply in &replies {
            assert!(reply["error"].is_null(), "{reply}");
        }
        let [config, checks, status] = &replies[..] else {
            panic!("three replies: {replies:?}");
        };
        Said {
            switch: config["result"]["api"]["tcp"].clone(),
            rule: config["result"]["api"]["tcp_rule"].clone(),
            tcp_row: checks["result"]["checks"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["name"] == "tcp"))
                .cloned()
                .unwrap_or_else(|| panic!("no tcp row: {checks}")),
            claude_row: status["result"]["targets"][0]["status"].clone(),
        }
    }
}

/// [`Daemon::said`]'s answers.
struct Said {
    switch: serde_json::Value,
    rule: serde_json::Value,
    tcp_row: serde_json::Value,
    claude_row: serde_json::Value,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A failed assertion must not leave a daemon (and its flock) behind.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One `session.hello`, with the scopes the app asks for, then each call in
/// turn on the same connection. Returns each call's whole JSON-RPC
/// response. One session, because every new peer of the socket is put
/// through the trust check, which is the slow part of a call from a test.
fn calls(socket: &Path, requests: &[(&str, serde_json::Value)]) -> Vec<serde_json::Value> {
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
        "jsonrpc": "2.0", "id": 0, "method": "session.hello",
        "params": {"client": {"name": "gate", "version": "1"}, "scopes": ["admin", "observe"]},
    }));
    assert!(hello["error"].is_null(), "session.hello: {hello}");
    requests
        .iter()
        .zip(1..)
        .map(|((method, params), id)| {
            line(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params,
            }))
        })
        .collect()
}

/// The status line of `GET /v1/info` on `port`, the one route that needs no
/// token.
fn info_over_tcp(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("the tcp door");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    write!(
        stream,
        "GET /v1/info HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .expect("the request is written");
    let mut reply = String::new();
    stream.read_to_string(&mut reply).expect("a reply");
    reply.lines().next().unwrap_or_default().to_string()
}

/// A file that leaves the switch out, on a machine with no old Claude
/// hooks: no door. Nothing is bound and the port file names nothing,
/// `config.get` says off, the doctor's row calls it the default rather than
/// a degrade, and the start-up line says which rule applied. Claude's own
/// settings are no reason to open it: only deckd's old entries are.
#[test]
fn a_file_that_leaves_the_switch_out_gets_no_tcp_door() {
    for (tag, settings) in [
        ("default", None),
        ("default-claude", Some(MIGRATED_SETTINGS)),
    ] {
        let daemon = Daemon::start(tag, "", settings);
        assert_eq!(daemon.port(), None, "{tag}: {}", daemon.log());
        let said = daemon.said();
        assert_eq!(said.switch, false, "{tag}");
        assert_eq!(said.rule, "default", "{tag}");
        assert_eq!(said.tcp_row["ok"], true, "{tag}: {}", said.tcp_row);
        assert_eq!(
            said.tcp_row["detail"],
            "off ([api] tcp is left out, and the door is off by default): the socket is the \
             only door",
            "{tag}"
        );
        assert_eq!(said.claude_row, "absent", "{tag}: nothing to repair");
        let log = daemon.log();
        assert!(
            log.contains("tcp door: [api] tcp is left out, and the door is off by default"),
            "{tag}: {log}"
        );
        assert!(log.contains("no tcp door"), "{tag}: {log}");
    }
}

/// What the start-up line says when the old Claude hooks keep the door
/// open.
const LEGACY_LINE: &str = "tcp door: [api] tcp is left out, but the pre-plugin deckd hooks of \
                           Claude Code, which post to this door, may still be installed";

/// The safeguard: while Claude's settings still carry the pre-plugin HTTP
/// hooks, which post to the door, a file that leaves the switch out keeps
/// it open, `config.get` says so and why (`legacy_hooks`), and the
/// Integrations page's Claude row asks for Repair (`outdated`). Once Repair
/// has taken them out, the next start closes the door and the row no
/// longer asks.
#[test]
fn the_old_claude_hooks_keep_the_door_open_until_they_are_migrated() {
    let mut daemon = Daemon::start("legacy", "", Some(LEGACY_SETTINGS));
    let port = daemon
        .port()
        .unwrap_or_else(|| panic!("the door is bound: {}", daemon.log()));
    assert_ne!(port, 47201, "an ephemeral port, never the operator's");
    assert_eq!(info_over_tcp(port), "HTTP/1.1 200 OK");
    let said = daemon.said();
    assert_eq!(said.switch, true);
    assert_eq!(said.rule, "legacy_hooks", "and why");
    assert_eq!(said.claude_row, "outdated", "the row asks for Repair");
    assert_eq!(said.tcp_row["ok"], true, "{}", said.tcp_row);
    assert_eq!(
        said.tcp_row["detail"],
        format!("127.0.0.1:{port}/v1/info answers and is ours")
    );
    let log = daemon.log();
    assert!(log.contains(LEGACY_LINE), "{log}");

    // What Repair leaves behind; the door goes with the next start.
    daemon.claude_settings(MIGRATED_SETTINGS);
    daemon.restart("legacy-migrated");
    assert_eq!(daemon.port(), None, "{}", daemon.log());
    let said = daemon.said();
    assert_eq!(said.switch, false);
    assert_eq!(said.rule, "default");
    assert_eq!(said.claude_row, "absent", "nothing left to repair");
}

/// Only the old HTTP hooks keep the door open. A leftover of the old
/// install that posts nothing — the allowlist pattern alone — still makes
/// the Claude row ask for Repair, but the door stays shut. And deckd's own
/// record of a pre-plugin Claude install keeps it open on its own, even
/// with Claude's settings clean, as they are when the old hooks went into
/// a `CLAUDE_CONFIG_DIR` the daemon does not see; once that record is the
/// plugin era's, the next start closes it.
#[test]
fn only_hooks_that_post_to_the_door_keep_it_open() {
    let leftover = Daemon::start("leftover", "", Some(PATTERN_ONLY_SETTINGS));
    assert_eq!(leftover.port(), None, "{}", leftover.log());
    let said = leftover.said();
    assert_eq!(
        (said.switch.clone(), said.rule.clone()),
        (false.into(), "default".into())
    );
    assert_eq!(said.claude_row, "outdated", "the row still asks for Repair");

    let mut recorded = Daemon::prepare("", Some(MIGRATED_SETTINGS));
    recorded.receipts(PRE_PLUGIN_RECEIPT);
    recorded.spawn("receipt");
    let port = recorded
        .port()
        .unwrap_or_else(|| panic!("the door is bound: {}", recorded.log()));
    assert_eq!(info_over_tcp(port), "HTTP/1.1 200 OK");
    let said = recorded.said();
    assert_eq!(
        (said.switch, said.rule),
        (true.into(), "legacy_hooks".into())
    );
    assert!(recorded.log().contains(LEGACY_LINE), "{}", recorded.log());

    recorded.receipts(PLUGIN_RECEIPT);
    recorded.restart("receipt-plugin");
    assert_eq!(recorded.port(), None, "{}", recorded.log());
    let said = recorded.said();
    assert_eq!((said.switch, said.rule), (false.into(), "default".into()));
}

/// An explicit switch is always honoured: `tcp = false` closes the door
/// even for the old Claude hooks (whose row still asks for Repair), and
/// `tcp = true` opens it with nothing asking for it. Each says so at start.
#[test]
fn an_explicit_switch_is_always_honoured() {
    let off = Daemon::start("explicit-off", "tcp = false\n", Some(LEGACY_SETTINGS));
    assert_eq!(off.port(), None, "{}", off.log());
    let said = off.said();
    assert_eq!(said.switch, false);
    assert_eq!(said.rule, "explicit");
    assert_eq!(said.claude_row, "outdated");
    assert_eq!(
        said.tcp_row["detail"],
        "off ([api] tcp = false): the socket is the only door"
    );
    assert!(
        off.log().contains("tcp door: [api] tcp = false"),
        "{}",
        off.log()
    );

    let on = Daemon::start("explicit-on", "tcp = true\n", None);
    let port = on
        .port()
        .unwrap_or_else(|| panic!("the door is bound: {}", on.log()));
    assert_eq!(info_over_tcp(port), "HTTP/1.1 200 OK");
    let said = on.said();
    assert_eq!(said.switch, true);
    assert_eq!(said.rule, "explicit");
    assert_eq!(
        said.tcp_row["detail"],
        format!("127.0.0.1:{port}/v1/info answers and is ours")
    );
    assert!(
        on.log().contains("tcp door: [api] tcp = true"),
        "{}",
        on.log()
    );
}
