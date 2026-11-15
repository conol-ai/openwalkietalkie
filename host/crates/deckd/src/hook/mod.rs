//! `deckd hook <claude|codex|codex-notify|claude-statusline|gemini>`
//! (contract §4, B11): the tiny process a CLI runs for every hook event.
//!
//! It is not `ctl`. `ctl`'s conventions are actively wrong here:
//!
//! - **It always exits 0.** For Claude and Codex a non-zero exit from a
//!   hook is a *block* signal — exit 2 with something on stderr denies the
//!   tool call (facts-codex §1.7: the 0.153.4 binary carries
//!   `"PermissionRequest hook exited with code 2 but did not write a
//!   denial reason to stderr"`). `ctl` exits 1/2/3 for an RPC error, an
//!   unreachable daemon and a refused guarded op; any of those coming out
//!   of a hook would turn a deckd bug into a denied command in the user's
//!   session. So every failure path here prints `{}` and exits 0.
//! - **Its 30 s timeout is too short.** A held `PermissionRequest` parks
//!   for the whole hold budget (120 s by default).
//! - **Its connect has no budget at all.** `deckd hook` gives the socket
//!   500 ms: if the daemon is not there, the operator loses the deck for
//!   that one prompt and the terminal dialog appears immediately, which is
//!   the correct fail-open.
//!
//! Content discipline (§11, contract §9): the payload is a prompt, a tool
//! input or an assistant message — the most sensitive data on the box.
//! Nothing here logs it. The stderr line a failure prints names the
//! failure and nothing else.
pub mod app;
pub mod env;
pub mod walk;

use deck_client::{Client, ClientConfig, Transport};
use deck_model::Scope;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The stdin cap (contract §4.1, as openmicrokbd). A bigger body is not
/// truncated and sent — a half payload is worse than none — it is dropped
/// and the hook answers `{}`.
pub const STDIN_MAX: usize = 16 * 1024 * 1024;
/// The socket must answer within this, or the daemon is treated as down.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// The backstop on a held call. The CLI's own hook timeout (130 s as the
/// installer writes it) fires first; this exists so a wedged daemon can
/// never hang a terminal for longer than the CLI would have.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(150);
/// What the helper calls itself in `session.hello`.
pub const CLIENT: &str = "deckd-hook";
/// What every failure path prints.
pub const EMPTY: &str = "{}";

/// Which door this invocation speaks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Claude,
    Codex,
    Gemini,
    /// Codex's `notify` program: the payload is the **last argv entry**,
    /// not stdin (facts-codex §2.2 — the JSON is appended after the
    /// configured command, so `notify = [deckd, hook, codex-notify]` calls
    /// us with the object as `argv[3]`).
    CodexNotify,
    /// Claude's `statusLine`, which is a config object and not a hook: it
    /// chains the user's own previous command.
    ClaudeStatusline,
}

impl Which {
    /// The subcommand word, as the installer writes it into the CLIs' config.
    #[must_use]
    pub fn parse(name: &str) -> Option<Which> {
        match name {
            "claude" => Some(Which::Claude),
            "codex" => Some(Which::Codex),
            "gemini" => Some(Which::Gemini),
            "codex-notify" => Some(Which::CodexNotify),
            "claude-statusline" => Some(Which::ClaudeStatusline),
            _ => None,
        }
    }

    /// The JSON-RPC method this invocation calls.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Which::Claude => "hooks.claude",
            Which::Codex => "hooks.codex",
            Which::Gemini => "hooks.gemini",
            Which::CodexNotify => "hooks.codex_notify",
            Which::ClaudeStatusline => "hooks.claude_statusline",
        }
    }

    /// Whether the payload comes from stdin (everything but `notify`).
    #[must_use]
    pub fn reads_stdin(self) -> bool {
        self != Which::CodexNotify
    }
}

/// The subcommand. **Always returns 0** — the exit code is the one thing
/// a CLI reads before it reads stdout, and any non-zero value is a
/// decision we did not make.
#[must_use]
pub fn main(which: &str, rest: &[String], socket: Option<PathBuf>) -> i32 {
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    run(
        which,
        rest,
        socket.as_deref(),
        &mut input,
        &mut out,
        &mut err,
    );
    let _ = out.flush();
    0
}

/// `main` with its four sides injected, so the tests can read what it
/// wrote without spawning a process.
pub fn run(
    which: &str,
    rest: &[String],
    socket: Option<&Path>,
    input: &mut impl Read,
    out: &mut impl Write,
    err: &mut impl Write,
) {
    let Some(which) = Which::parse(which) else {
        // clap refuses an unknown word before this, but a hook that
        // answers a usage error with a non-zero exit blocks a tool call.
        empty(out, err, "unknown hook");
        return;
    };
    let body = if which.reads_stdin() {
        read_stdin(input)
    } else {
        Body::Argv(rest.last().cloned().unwrap_or_default())
    };
    // The statusline's own stdout is the user's command's, byte for byte,
    // and it is produced whether or not the daemon is reachable.
    if which == Which::ClaudeStatusline {
        passthrough(rest, body.bytes(), out, err);
    }
    let Some(socket) = socket else {
        finish(which, out, err, None, "no data directory");
        return;
    };
    let mut payload = match body.json() {
        Some(payload) => payload,
        None => {
            finish(which, out, err, None, body.why());
            return;
        }
    };
    if which == Which::ClaudeStatusline && payload.get("rate_limits").is_some() {
        if let Some(object) = payload.as_object_mut() {
            object.insert("_openwalkietalkie_account".into(),
                serde_json::json!(crate::usage::claude::statusline_account()));
        }
    }
    let walk = walk::from_process();
    // Every form carries the app, the status line included: its register
    // replaces the stored terminal whole, so leaving `app` out there would
    // make it come and go.
    let params = params(
        payload,
        env::from_process(),
        walk.cli_pid.and_then(walk::tty_of),
        &walk,
        app::from_process(&walk),
    );
    match call(socket, which.method(), params) {
        Ok(value) => finish(which, out, err, Some(value), ""),
        Err(why) => finish(which, out, err, None, why),
    }
}

/// The `hooks.<cli>` call's params: the payload and what the helper found
/// out about the session around it — the environment subset, the CLI's
/// tty, pid and pid kind, and the app the session runs in (`app`, the
/// hand-off's; `null` when none was found).
fn params(
    payload: serde_json::Value,
    env: std::collections::BTreeMap<String, String>,
    tty: Option<String>,
    walk: &walk::Walk,
    app: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "payload": payload,
        "env": env,
        "tty": tty,
        "cli_pid": walk.cli_pid,
        "pid_kind": walk.pid_kind,
        "app": app,
    })
}

/// What the helper read, and why it may be unusable.
enum Body {
    Stdin(Vec<u8>),
    Argv(String),
    /// Over `STDIN_MAX`: the bytes are kept for the statusline
    /// passthrough, but nothing is sent to the daemon.
    Oversize(Vec<u8>),
    Unreadable,
}

impl Body {
    fn bytes(&self) -> &[u8] {
        match self {
            Body::Stdin(b) | Body::Oversize(b) => b,
            Body::Argv(s) => s.as_bytes(),
            Body::Unreadable => &[],
        }
    }

    /// The payload as JSON, or `None` when there is nothing to send.
    fn json(&self) -> Option<serde_json::Value> {
        match self {
            Body::Stdin(b) => serde_json::from_slice(b).ok(),
            Body::Argv(s) => serde_json::from_str(s).ok(),
            Body::Oversize(_) | Body::Unreadable => None,
        }
    }

    /// Why there is nothing to send — never the body itself.
    fn why(&self) -> &'static str {
        match self {
            Body::Oversize(_) => "payload over 16 MiB",
            Body::Unreadable => "stdin unreadable",
            _ => "payload is not JSON",
        }
    }
}

/// Read at most `STDIN_MAX` bytes; one byte more and the body is refused.
fn read_stdin(input: &mut impl Read) -> Body {
    let mut buf = Vec::new();
    match input.take(STDIN_MAX as u64 + 1).read_to_end(&mut buf) {
        Ok(_) if buf.len() > STDIN_MAX => Body::Oversize(buf),
        Ok(_) => Body::Stdin(buf),
        Err(_) => Body::Unreadable,
    }
}

/// Write the answer. The statusline has already written the user's own
/// output and must not have JSON appended to it; every other form prints
/// the daemon's object, or `{}`.
fn finish(
    which: Which,
    out: &mut impl Write,
    err: &mut impl Write,
    value: Option<serde_json::Value>,
    why: &str,
) {
    if !why.is_empty() {
        let _ = writeln!(err, "deckd hook: {why}");
    }
    if which == Which::ClaudeStatusline {
        return;
    }
    match value {
        // A non-object reply (a `null`, a bare number) is not something a
        // CLI can parse as hook output: answer as if there were none.
        Some(value) if value.is_object() => {
            let _ = writeln!(out, "{value}");
        }
        _ => {
            let _ = writeln!(out, "{EMPTY}");
        }
    }
}

fn empty(out: &mut impl Write, err: &mut impl Write, why: &str) {
    let _ = writeln!(err, "deckd hook: {why}");
    let _ = writeln!(out, "{EMPTY}");
}

/// Run the user's previous `statusLine` command with the same stdin and
/// copy its stdout through **byte for byte** (C23: `statusLine` has no
/// `args` and runs in a shell, so the installer shell-quotes the whole
/// previous command into one word — which is why a single argument is
/// handed back to `/bin/sh -c`). A failure of the user's command is never
/// swallowed: its stderr is copied too, and a non-zero status is named on
/// ours.
fn passthrough(rest: &[String], stdin: &[u8], out: &mut impl Write, err: &mut impl Write) {
    let Some((program, args)) = split_command(rest) else {
        return;
    };
    #[cfg(windows)]
    let (program, args) = windows_shell(program, args);
    let mut child = match std::process::Command::new(&*program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            let _ = writeln!(err, "deckd hook: statusline command did not start: {e}");
            return;
        }
    };
    if let Some(mut sink) = child.stdin.take() {
        let _ = sink.write_all(stdin);
    }
    match child.wait_with_output() {
        Ok(done) => {
            let _ = out.write_all(&done.stdout);
            let _ = err.write_all(&done.stderr);
            if !done.status.success() {
                let _ = writeln!(err, "deckd hook: statusline command {}", done.status);
            }
        }
        Err(e) => {
            let _ = writeln!(err, "deckd hook: statusline command failed: {e}");
        }
    }
}

/// The passthrough's program and arguments. One word is a shell command
/// (the installer's `shell_word` form); several are an argv.
fn split_command(rest: &[String]) -> Option<(&str, Vec<&str>)> {
    match rest {
        [] => None,
        [one] => Some(("/bin/sh", vec!["-c", one.as_str()])),
        [program, args @ ..] => Some((program.as_str(), args.iter().map(String::as_str).collect())),
    }
}

/// Windows: Claude Code runs a `statusLine` command in Git Bash, so the
/// user's command is bash's to run here too: the bash Claude Code names in
/// `CLAUDE_CODE_GIT_BASH_PATH`, else Git for Windows' own, else the first
/// `bash.exe` on `PATH` that is not System32's WSL launcher. Without one,
/// `cmd.exe /c` runs it as Windows would. An argv (several words) runs as
/// it is.
#[cfg(windows)]
fn windows_shell<'a>(program: &'a str, args: Vec<&'a str>) -> (std::borrow::Cow<'a, str>, Vec<&'a str>) {
    if program != "/bin/sh" {
        return (program.into(), args);
    }
    match git_bash() {
        Some(bash) => (bash.to_string_lossy().into_owned().into(), args),
        None => {
            let command = args.get(1).copied().unwrap_or_default();
            ("cmd.exe".into(), vec!["/d", "/s", "/c", command])
        }
    }
}

#[cfg(windows)]
fn git_bash() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let file = |p: PathBuf| p.is_file().then_some(p);
    if let Some(named) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from).and_then(file) {
        return Some(named);
    }
    for root in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(found) = std::env::var_os(root).and_then(|r| file(PathBuf::from(r).join(r"Git\bin\bash.exe"))) {
            return Some(found);
        }
    }
    if let Some(found) = std::env::var_os("LOCALAPPDATA")
        .and_then(|r| file(PathBuf::from(r).join(r"Programs\Git\bin\bash.exe")))
    {
        return Some(found);
    }
    let system = std::env::var_os("SystemRoot").map(|s| PathBuf::from(s).to_string_lossy().to_lowercase());
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| system.as_deref().is_none_or(|s| !dir.to_string_lossy().to_lowercase().starts_with(s)))
        .find_map(|dir| file(dir.join("bash.exe")))
}

/// Connect, hello, one call, close. Errors are kinds, never contents.
fn call(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, &'static str> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "no runtime")?;
    let config = ClientConfig {
        transport: Transport::Uds(socket.to_path_buf()),
        client_name: CLIENT.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        scopes: vec![Scope::HooksIngest],
        subscribe: Vec::new(),
        snapshot: false,
        reconnect: false,
        content: None,
    };
    rt.block_on(async move {
        let client = match tokio::time::timeout(CONNECT_TIMEOUT, Client::connect(config)).await {
            Ok(Ok(client)) => client,
            Ok(Err(_)) => return Err("daemon unreachable"),
            Err(_) => return Err("daemon did not answer in 500 ms"),
        };
        let answer = tokio::time::timeout(CALL_TIMEOUT, client.call(method, params)).await;
        client.close().await;
        match answer {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err("the daemon refused the hook"),
            Err(_) => Err("the daemon did not answer the hook"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn every_installed_word_maps_to_its_method() {
        let table = [
            ("claude", "hooks.claude"),
            ("codex", "hooks.codex"),
            ("gemini", "hooks.gemini"),
            ("codex-notify", "hooks.codex_notify"),
            ("claude-statusline", "hooks.claude_statusline"),
        ];
        for (word, method) in table {
            let which = Which::parse(word).unwrap_or_else(|| panic!("{word}"));
            assert_eq!(which.method(), method);
            // Every one of them is in the daemon's method table, or the
            // helper would be calling a name nothing answers.
            assert!(
                crate::api::dispatch::method(method).is_some(),
                "{method} is not an advertised method"
            );
        }
        assert_eq!(Which::parse("cursor"), None);
    }

    /// The call carries the app beside everything it carried before, and
    /// says `null` when the helper found none.
    #[test]
    fn the_params_carry_the_app_beside_the_walk() {
        let walk = walk::Walk {
            cli_pid: Some(4100),
            pid_kind: deck_model::PidKind::Cli,
            hops: 1,
            chain: Vec::new(),
        };
        let env: std::collections::BTreeMap<String, String> =
            [("TERM_PROGRAM".to_string(), "zed".to_string())].into();
        let got = params(
            serde_json::json!({"hook_event_name": "Stop"}),
            env.clone(),
            Some("/dev/ttys004".into()),
            &walk,
            Some("dev.zed.Zed".into()),
        );
        assert_eq!(
            got,
            serde_json::json!({
                "payload": {"hook_event_name": "Stop"},
                "env": {"TERM_PROGRAM": "zed"},
                "tty": "/dev/ttys004",
                "cli_pid": 4100,
                "pid_kind": "cli",
                "app": "dev.zed.Zed",
            })
        );
        let none = params(serde_json::json!({}), env, None, &walk, None);
        assert_eq!(none["app"], serde_json::Value::Null);
        assert!(none.as_object().unwrap().contains_key("app"));
    }

    #[test]
    fn only_notify_takes_its_payload_from_argv() {
        assert!(!Which::CodexNotify.reads_stdin());
        for which in [
            Which::Claude,
            Which::Codex,
            Which::Gemini,
            Which::ClaudeStatusline,
        ] {
            assert!(which.reads_stdin(), "{which:?}");
        }
    }

    #[test]
    fn stdin_is_capped_at_sixteen_mebibytes() {
        let ok = vec![b'x'; STDIN_MAX];
        assert!(matches!(read_stdin(&mut ok.as_slice()), Body::Stdin(_)));
        let over = vec![b'x'; STDIN_MAX + 1];
        let body = read_stdin(&mut over.as_slice());
        assert!(matches!(body, Body::Oversize(_)));
        assert_eq!(body.json(), None, "an oversize body is never sent");
        assert_eq!(body.why(), "payload over 16 MiB");
    }

    #[test]
    fn a_body_that_is_not_json_is_refused_without_quoting_it() {
        let body = read_stdin(&mut b"{\"prompt\": \"my secret plan".as_slice());
        assert_eq!(body.json(), None);
        assert!(
            !body.why().contains("secret"),
            "a failure message never carries the payload"
        );
    }

    #[test]
    fn the_passthrough_is_a_shell_command_when_it_is_one_word() {
        assert_eq!(split_command(&[]), None);
        let one = strings(&["echo hi | tr a-z A-Z"]);
        assert_eq!(
            split_command(&one),
            Some(("/bin/sh", vec!["-c", "echo hi | tr a-z A-Z"]))
        );
        let many = strings(&["/usr/bin/env", "python3", "line.py"]);
        assert_eq!(
            split_command(&many),
            Some(("/usr/bin/env", vec!["python3", "line.py"]))
        );
    }

    #[test]
    fn a_missing_socket_still_answers_empty() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        run(
            "codex",
            &[],
            None,
            &mut b"{}".as_slice(),
            &mut out,
            &mut err,
        );
        assert_eq!(String::from_utf8_lossy(&out), "{}\n");
        assert!(String::from_utf8_lossy(&err).contains("no data directory"));
    }

    #[test]
    fn a_notify_payload_that_is_not_json_answers_empty() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let socket = std::path::PathBuf::from("/nonexistent/deckd.sock");
        run(
            "codex-notify",
            &strings(&["turn-ended", "not json at all"]),
            Some(&socket),
            &mut b"".as_slice(),
            &mut out,
            &mut err,
        );
        assert_eq!(String::from_utf8_lossy(&out), "{}\n");
        assert!(String::from_utf8_lossy(&err).contains("not JSON"));
    }

    #[test]
    fn the_statusline_writes_the_users_output_and_no_json() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let socket = std::path::PathBuf::from("/nonexistent/deckd.sock");
        run(
            "claude-statusline",
            &strings(&["printf 'the user line'"]),
            Some(&socket),
            &mut b"{\"session_id\":\"x\"}".as_slice(),
            &mut out,
            &mut err,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "the user line",
            "no trailing newline, no {{}} appended"
        );
    }
}
