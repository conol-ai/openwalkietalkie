//! `deckd hook` as a CLI runs it (contract §6 rows 9–10): the parent walk
//! through both spawn shapes, the unknown-ancestry answer, a dead daemon,
//! `codex-notify`'s argv payload and the `statusLine` passthrough.
//!
//! The helper is exercised as the **real binary** wherever the thing under
//! test is a process fact (exit code, stdout bytes, who the parent is):
//! those are the parts a unit test cannot speak for.
//!
//! Nothing here touches the user's `~/.claude`, `~/.codex` or the real
//! data directory: every run gets `--data-dir` inside `$TMPDIR`.
mod common;

use common::Daemon;
use deck_model::PidKind;
use deckd::hook::walk::{self, Proc, Procs};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The helper binary under test.
fn deckd() -> &'static str {
    env!("CARGO_BIN_EXE_deckd")
}

/// A temp directory that removes itself.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("dkh{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Temp(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One `deckd hook …` run: its exit code, stdout and stderr.
struct Run {
    code: i32,
    out: Vec<u8>,
    err: String,
}

fn hook(data_dir: &Path, args: &[&str], stdin: &[u8]) -> Run {
    let mut child = Command::new(deckd())
        .arg("--data-dir")
        .arg(data_dir)
        .arg("hook")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the deckd binary runs");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(stdin)
        .expect("stdin is written");
    let done = child.wait_with_output().expect("the helper exits");
    Run {
        code: done.status.code().unwrap_or(-1),
        out: done.stdout,
        err: String::from_utf8_lossy(&done.stderr).into_owned(),
    }
}

/// A real process the walk must recognise as a CLI: the `deckd` binary
/// itself, **copied** to `<tmp>/codex`, so `proc_pidpath` ends in
/// `/codex`. A copy rather than a symlink because `proc_pidpath` resolves
/// a symlink back to its target, and not `/bin/sh` because macOS launch
/// constraints SIGKILL a system binary run from anywhere but its own path
/// (measured: a copy of `/bin/sh` dies with signal 9).
#[cfg(target_os = "macos")]
fn fake_cli(dir: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    std::fs::copy(deckd(), &path).expect("the deckd binary can be copied");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode 0755");
    path
}

/// Read a pid a spawned shell wrote, or fail after five seconds. Only the
/// macOS parent-walk row reads one.
#[cfg(target_os = "macos")]
fn pid_in(file: &Path) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(file) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pid in {}",
            file.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(target_os = "macos")]
fn kill(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status();
}

/// Contract §6 row 9. Claude installs its hooks in **exec form**, so the
/// hook's parent is the CLI itself; Codex's `hooks.json` holds a shell
/// **string** (C12/C13), so the hook's parent is a shell and the CLI is
/// its grandparent. `$PPID` is right for one and wrong for the other,
/// which is the whole reason the walk exists — and the shell is the
/// dangerous half, because it exits the moment the hook returns, so a
/// `kill(ppid, 0)` liveness poll on it would flap the roster within 2 s.
///
/// Both shapes are real processes under a real process called `codex`, so
/// this exercises `proc_pidinfo`/`proc_pidpath` and not a table.
#[test]
#[cfg(target_os = "macos")]
fn the_parent_walk_finds_the_cli_through_both_spawn_forms() {
    let temp = Temp::new("spawn");
    let exec_file = temp.path().join("exec.pid");
    let shell_file = temp.path().join("shell.pid");
    // `codex` runs one shell (the exec-form hook's stand-in, whose parent
    // *is* the CLI) which in turn backgrounds a second one (the
    // shell-form hook, two hops up). Both stay alive while the walk runs.
    let script = format!(
        // Both shells `exec` their sleep, so each keeps the pid it wrote
        // and killing that pid leaves nothing behind.
        "sh -c 'echo $$ > {shell}; exec sleep 30' & echo $$ > {exec}; exec sleep 30",
        shell = shell_file.display(),
        exec = exec_file.display()
    );
    let cli = fake_cli(temp.path(), "codex");
    let mut top = Command::new(&cli)
        .arg("--data-dir")
        .arg(temp.path())
        .args(["hook", "claude-statusline"])
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the fake CLI runs");

    let exec_pid = pid_in(&exec_file);
    let walk = walk::resolve(exec_pid, &walk::MacProcs, walk::MAX_HOPS);
    assert_eq!(walk.pid_kind, PidKind::Cli, "{:?}", walk.chain);
    assert_eq!(
        walk.cli_pid,
        Some(top.id()),
        "exec form: the parent is the CLI: {:?}",
        walk.chain
    );
    assert_eq!(walk.hops, 1, "{:?}", walk.chain);

    let shell_pid = pid_in(&shell_file);
    let walk = walk::resolve(shell_pid, &walk::MacProcs, walk::MAX_HOPS);
    assert_eq!(walk.pid_kind, PidKind::Cli, "{:?}", walk.chain);
    assert_eq!(
        walk.cli_pid,
        Some(top.id()),
        "shell form: the CLI is the grandparent: {:?}",
        walk.chain
    );
    assert_eq!(
        walk.hops, 2,
        "sh -c is the hop $PPID stops at: {:?}",
        walk.chain
    );

    kill(shell_pid);
    kill(exec_pid);
    let _ = top.kill();
    let _ = top.wait();
}

/// Contract §6 row 9, second half. No CLI anywhere up the chain → no pid
/// is reported and the daemon polls nothing for that agent (a pid we
/// cannot vouch for retires somebody else's agent when it is recycled).
///
/// This one is a fixture table rather than real processes on purpose: the
/// test binary's *own* ancestry depends on what ran `cargo test`, and
/// under an agent CLI it legitimately contains one. A real-process
/// "unknown" case would assert something about the developer's terminal.
#[test]
fn the_parent_walk_reports_unknown_when_there_is_no_cli_ancestor() {
    struct Table(Vec<Proc>);
    impl Procs for Table {
        fn get(&self, pid: u32) -> Option<Proc> {
            self.0.iter().find(|p| p.pid == pid).cloned()
        }
    }
    let table = Table(vec![
        Proc::new(4402, 4401, &["deckd", "deckd"]),
        Proc::new(4401, 4400, &["sh", "sh"]),
        Proc::new(4400, 4300, &["python3.13", "python3.13"]),
        Proc::new(4300, 1, &["login", "login"]),
        Proc::new(1, 0, &["launchd", "launchd"]),
    ]);
    let walk = walk::resolve(4402, &table, walk::MAX_HOPS);
    assert_eq!(walk.cli_pid, None, "{:?}", walk.chain);
    assert_eq!(walk.pid_kind, PidKind::Unknown);
    assert_eq!(walk.chain.len(), 5, "the walk stops at launchd");
}

/// Contract §6 row 10. Fail-open is the whole contract of a hook helper:
/// no daemon, no socket, nothing listening — the CLI still gets a parsable
/// answer and a zero exit, and its own dialog appears.
#[test]
fn a_dead_daemon_prints_empty_and_exits_zero() {
    let temp = Temp::new("dead");
    assert!(
        !temp.path().join("deckd.sock").exists(),
        "no daemon in this data dir"
    );
    let payload = br#"{"session_id":"01a0bca9-1344-7d20-a793-10903639be67",
        "hook_event_name":"PermissionRequest","cwd":"/tmp","tool_name":"Bash",
        "turn_id":"01a0bca9-e001-7042-8668-78526755dd61",
        "tool_input":{"command":"rm -rf build"}}"#;
    for cli in ["claude", "codex", "gemini"] {
        let run = hook(temp.path(), &[cli], payload);
        assert_eq!(run.code, 0, "{cli}: {}", run.err);
        assert_eq!(
            String::from_utf8_lossy(&run.out),
            "{}\n",
            "{cli} must answer an empty hook output"
        );
    }
    // `codex-notify` takes its payload from argv and must fail open too.
    let run = hook(temp.path(), &["codex-notify", "{\"type\":\"x\"}"], b"");
    assert_eq!(
        (run.code, String::from_utf8_lossy(&run.out)),
        (0, "{}\n".into())
    );
}

/// A payload over the 16 MiB cap is dropped whole — never truncated and
/// sent — and the hook still answers `{}` and exits 0.
#[test]
fn an_oversize_payload_is_dropped_and_the_hook_still_answers() {
    let temp = Temp::new("big");
    let mut payload = Vec::with_capacity(deckd::hook::STDIN_MAX + 64);
    payload.extend_from_slice(br#"{"hook_event_name":"UserPromptSubmit","prompt":""#);
    payload.resize(deckd::hook::STDIN_MAX + 8, b'x');
    payload.extend_from_slice(br#""}"#);
    let run = hook(temp.path(), &["codex"], &payload);
    assert_eq!(run.code, 0, "{}", run.err);
    assert_eq!(String::from_utf8_lossy(&run.out), "{}\n");
    assert!(run.err.contains("16 MiB"), "{}", run.err);
    assert!(
        !run.err.contains("xxxx"),
        "a failure line never carries the payload"
    );
}

/// Contract §4.6: the chained `statusLine` prints the user's own command's
/// stdout **verbatim**. Not "trimmed", not "with our JSON after it" — the
/// bytes, because a status line is drawn as given and a stray newline or
/// an appended `{}` is a visible defect in every prompt the user sees.
#[test]
fn the_statusline_passthrough_preserves_the_users_output_byte_for_byte() {
    let temp = Temp::new("status");
    // No trailing newline, ANSI colour, a multi-byte character and a tab:
    // everything a real status line contains and nothing that survives a
    // careless `println!` of a trimmed `String`.
    let wanted = "\u{1b}[32m◆\u{1b}[0m main\tcohdl-api  42%";
    let script = format!("printf %s '{wanted}'");
    let run = hook(
        temp.path(),
        &["claude-statusline", &script],
        br#"{"session_id":"s"}"#,
    );
    assert_eq!(run.code, 0, "{}", run.err);
    assert_eq!(
        String::from_utf8_lossy(&run.out),
        wanted,
        "the user's line is the whole of our stdout"
    );
}

/// The user's `statusLine` command failing is the user's business: deckd
/// copies its stderr through and names the status, and still exits 0 so
/// Claude keeps drawing the (empty) line rather than disabling it.
#[test]
fn a_failing_statusline_command_is_reported_and_never_swallowed() {
    let temp = Temp::new("statusfail");
    let run = hook(
        temp.path(),
        &[
            "claude-statusline",
            "echo 'the user script broke' >&2; exit 3",
        ],
        br#"{"session_id":"s"}"#,
    );
    assert_eq!(run.code, 0, "a statusline failure is not ours to signal");
    assert!(run.out.is_empty(), "{:?}", run.out);
    assert!(run.err.contains("the user script broke"), "{}", run.err);
    assert!(run.err.contains("exit status: 3"), "{}", run.err);
}

/// The helper asks for `hooks:ingest` and nothing else: a hook process is
/// agent-descended by construction, and the one thing it may do is hand
/// over a payload. A daemon that granted it `observe` or `agent` would be
/// handing the CLI's own subprocess the roster.
#[tokio::test]
async fn the_helper_session_holds_only_hooks_ingest() {
    let daemon = Daemon::start().await;
    let mut client = daemon.connect().await;
    let hello = client
        .hello(serde_json::json!({
            "client": {"name": deckd::hook::CLIENT, "version": "0"},
            "scopes": ["hooks:ingest"]
        }))
        .await;
    assert_eq!(
        hello["result"]["principal"]["scopes"],
        serde_json::json!(["hooks:ingest"]),
        "{hello}"
    );
    // With that one scope the helper cannot read anything.
    let refused = client.call_err("agent.list", serde_json::json!({})).await;
    assert_eq!(refused["message"], "forbidden", "{refused}");
    daemon.shutdown().await;
}

/// Codex's `notify` is **not** a hook: it is a program Codex runs with the
/// event JSON appended as the **last** argv entry, after whatever fixed
/// arguments the user configured (facts §2.2 — the user's own machine has
/// `notify = ["…/SkyComputerUseClient", "turn-ended"]`, a subcommand
/// before the payload). A helper that read stdin here would see nothing at
/// all, and one that read `argv[1]` would parse the subcommand.
///
/// Run against a live daemon so the proof is the roster moving, not a
/// string we passed to ourselves — and with **empty stdin**, so nothing
/// but argv can be the source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notify_payload_comes_from_the_last_argument() {
    let daemon = Daemon::start().await;
    let dir = daemon.dir.clone();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks/codex");
    let start = std::fs::read(fixtures.join("session-start.json")).expect("the fixture");
    let notify = std::fs::read_to_string(fixtures.join("notify-agent-turn-complete.json"))
        .expect("the fixture");

    // The session joins the roster through the ordinary stdin path.
    let started = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || hook(&dir, &["codex"], &start))
            .await
            .expect("the helper runs")
    };
    assert_eq!(started.code, 0, "{}", started.err);
    assert_eq!(String::from_utf8_lossy(&started.out), "{}\n");
    daemon
        .until("the codex session is on the roster", |s| {
            s.agents.values().any(|a| a.kind == deck_model::Kind::Codex)
        })
        .await;

    // `deckd hook codex-notify turn-ended '<json>'`, stdin empty.
    let reported = {
        let dir = dir.clone();
        let notify = notify.clone();
        tokio::task::spawn_blocking(move || {
            hook(&dir, &["codex-notify", "turn-ended", &notify], b"")
        })
        .await
        .expect("the helper runs")
    };
    assert_eq!(reported.code, 0, "{}", reported.err);
    assert_eq!(String::from_utf8_lossy(&reported.out), "{}\n");
    daemon
        .until("the turn was reported from argv", |s| {
            s.agents.values().any(|a| {
                a.state == deck_model::AgentState::Ready
                    && a.reply.as_ref().and_then(|r| r.text.as_deref())
                        == Some("REDACTED assistant message")
            })
        })
        .await;
    daemon.shutdown().await;
}
