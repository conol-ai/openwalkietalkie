//! What the binary exits with when its command line is not understood
//! (§10.7's codes: 0 done, 1 refused or failed, 2 the service did not
//! answer, 3 it wants a trusted principal). A script that follows them must
//! never read a mistyped argument as a service that is down: clap's own
//! code for a usage error is 2, so `deckd` turns it into 1, and help and
//! the version stay 0. A hook is fail-open whatever it is handed.
//!
//! Nothing here reaches a service: every command line but the hook's is
//! refused before the data directory is touched, and the hook's socket is
//! in a directory that does not exist.
use std::io::Write;
use std::process::{Command, Stdio};

fn deckd(args: &[&str]) -> (i32, String, String) {
    let nowhere = std::env::temp_dir().join(format!("dku{}-absent", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_deckd"))
        .arg("--data-dir")
        .arg(&nowhere)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the deckd binary runs");
    // A hook reads its payload from stdin; an empty one is still an answer.
    drop(child.stdin.take().map(|mut stdin| stdin.write_all(b"")));
    let output = child.wait_with_output().expect("deckd exits");
    assert!(!nowhere.exists(), "nothing was created for {args:?}");
    (
        output.status.code().expect("an exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_command_line_that_is_not_understood_exits_one_not_two() {
    for args in [
        &["firmware", "install", "--board", "lilygo"][..],
        &["care", "source", "claude", "maybe"],
        &["care", "clear", "stt"],
        &["device", "set", "brightness", "70000"],
        &["no-such-verb"],
    ] {
        let (code, _, err) = deckd(args);
        assert_eq!(code, 1, "{args:?}: {err}");
        assert!(err.starts_with("error: "), "{args:?}: {err}");
    }
    let (_, _, err) = deckd(&["firmware", "install", "--board", "lilygo"]);
    assert!(err.contains("invalid value 'lilygo'"), "{err}");
}

#[test]
fn help_and_the_version_are_answers_and_a_hook_stays_fail_open() {
    let (code, out, _) = deckd(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage:"), "{out}");
    let (code, out, _) = deckd(&["--version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("deckd "), "{out}");
    // No service anywhere: the hook answers `{}` and lets the call through.
    let (code, out, err) = deckd(&["hook", "claude"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "{}");
}
