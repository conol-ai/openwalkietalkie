//! **`NSAppleScript`, on the only thread where it is real.**
//!
//! `harness = false`, so this file's `fn main` *is* the process's main
//! thread — the same reason `voice_mainloop` exists. `NSAppleScript` is not
//! thread-safe, §2.3 puts it on `main` beside the TCC prompts and the
//! AVSpeech callbacks, and [`deckd::desktop::script::AppleScriptRunner`]
//! reaches it through B10's hop. None of that can be asserted from a
//! libtest worker: the hop's queue drains on `main`, and under `#[test]`
//! `main` is libtest's own loop, which never turns a run loop. The rows
//! here would pass vacuously at best.
//!
//! Why it matters enough to want its own binary. M12 measured **~118 ms per
//! `osascript` round trip**, and a resolve-plus-pin is two of them while an
//! order is two more; the in-process runner removes the fork and the
//! dynamic-linker warm-up from all four. `DesktopConfig::from_config` picks
//! it for the daemon (`in_process: true`) and every other test in the tree
//! gets [`deckd::desktop::script::OsaRunner`] instead — so without this
//! file the path the shipped daemon actually uses would be compiled and
//! never run.
//!
//! What it does **not** claim: which of the two runners a TCC grant
//! follows. A child `osascript` is a different process and TCC attributes
//! an Apple Event to the *responsible* one (W4 V9). `-1743` was not
//! reproducible on this machine at all, so the answer needs week 7's signed
//! bundle and a measurement. `script.rs` says so where a reader will find
//! it.
//!
//! It prints its own results: a `harness = false` target has no libtest
//! summary, and forging one would inflate the gate's count.
//!
//! Every item is macOS's alone, one by one, as in `voice_mainloop`: a
//! file-wide `#![cfg]` would leave this binary with no `fn main` on any
//! other platform, which does not build. Off macOS only the `main` at the
//! end is left, and it says it skipped.

#[cfg(target_os = "macos")]
use deckd::audio::mainloop::{is_main_thread, pump};
#[cfg(target_os = "macos")]
use deckd::desktop::errors::{ScriptError, ERR_NOT_PERMITTED};
#[cfg(target_os = "macos")]
use deckd::desktop::ghostty;
#[cfg(target_os = "macos")]
use deckd::desktop::script::{AppleScriptRunner, Scripting};
#[cfg(target_os = "macos")]
use std::sync::mpsc;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
fn row(name: &str, ok: bool, failures: &mut usize) {
    if ok {
        println!("ok    desktop {name}");
    } else {
        println!("FAIL  desktop {name}");
        *failures += 1;
    }
}

/// Turn the main run loop until `rx` answers, the way `deckd run` pumps it
/// once per supervision period.
#[cfg(target_os = "macos")]
fn pump_for<T>(rx: &mpsc::Receiver<T>, budget: Duration) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(value) = rx.try_recv() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        pump(Duration::from_millis(50));
    }
}

#[cfg(target_os = "macos")]
fn main() {
    let mut failures = 0usize;

    // The premise. If this is false every row below is meaningless, so it
    // is a row of its own rather than an assumption.
    row("this is the main thread", is_main_thread(), &mut failures);

    let runner = AppleScriptRunner::new();
    row(
        "the runner names itself",
        runner.name() == "nsapplescript",
        &mut failures,
    );

    // ── on main, directly ───────────────────────────────────────────────
    // `AppleScriptRunner::run` called *from* main runs `f` in place rather
    // than posting and then waiting for a queue only it could drain
    // (`audio::apple::on_main` makes the same call for the same reason). If
    // that branch were wrong this row would hang for the whole budget and
    // then answer `Unavailable`.
    let value = runner.run("return \"hello from nsapplescript\"\n");
    row(
        "a script compiles and runs in process on main",
        value.as_deref() == Ok("hello from nsapplescript"),
        &mut failures,
    );
    // A successful answer without pumping main proves the synchronous
    // branch. Posting to main would instead time out with `Unavailable`.
    // Wall time cannot distinguish those branches: cold AppleScript startup
    // can exceed the worker's queue budget even though this call succeeds.

    // A script whose last statement is not a `return` is still a success.
    // In AppleScript nearly everything has a value — `set x to 1` answers
    // `1`, and `perform action …` answers `true` — so what this row pins is
    // that the result descriptor is read rather than mistaken for an error,
    // which is the shape `pin` and `unpin` both come back in.
    let assigned = runner.run("set x to 1\n");
    println!("      `set x to 1` answered {assigned:?}");
    row(
        "a script that does not return is still a success",
        assigned.is_ok(),
        &mut failures,
    );

    // ── the error dictionary ────────────────────────────────────────────
    // AppleScript's own `error … number …` lets the row choose the number,
    // which is how the `-1743` branch is exercised in process on a machine
    // where M13 could not reproduce a real Automation denial. It proves the
    // dictionary is read (`NSAppleScriptErrorNumber`/`…ErrorMessage`) and
    // that the number reaches `ScriptError::denied`.
    match runner.run("error \"Not authorized to send Apple events\" number -1743\n") {
        Err(e) => {
            println!("      {e}");
            row(
                "an error dictionary becomes a numbered ScriptError",
                e.number() == Some(ERR_NOT_PERMITTED) && e.denied(),
                &mut failures,
            );
            row(
                "…carrying the engine's own message",
                matches!(&e, ScriptError::Osa { message, .. } if message.contains("Apple events")),
                &mut failures,
            );
        }
        Ok(unexpected) => {
            println!("      got {unexpected:?}");
            row(
                "an error dictionary becomes a numbered ScriptError",
                false,
                &mut failures,
            );
        }
    }

    // A source that will not compile is an error and never a panic: the
    // generated bindings' non-optional `Retained` return cannot represent
    // the nil `executeAndReturnError:` answers, which is why the message is
    // sent by hand with an `Option<Retained<_>>`.
    row(
        "an uncompilable source is an error and not a panic",
        runner.run("tell tell tell\n").is_err(),
        &mut failures,
    );

    // ── from a worker, with main pumping: the daemon's own topology ─────
    // `deckd-desktop` is a std thread and `main` is a run loop, so this is
    // the arrangement every real resolution and every real order uses.
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("deckd-desktop-probe".into())
        .spawn(move || {
            let runner = AppleScriptRunner::new();
            let _ = tx.send((
                runner.run("return \"hello from a worker\"\n"),
                runner.run("error \"nope\" number -1728\n"),
            ));
        })
        .expect("the worker thread");
    match pump_for(&rx, Duration::from_secs(10)) {
        Some((value, error)) => {
            row(
                "a worker's script runs on main and the answer comes back",
                value.as_deref() == Ok("hello from a worker"),
                &mut failures,
            );
            row(
                "…and so does its error",
                error.err().and_then(|e| e.number()) == Some(-1728),
                &mut failures,
            );
        }
        None => {
            // A real failure, not a skip: the hop is the daemon's only way
            // to reach `NSAppleScript`, and a hop that does not answer is a
            // daemon that cannot resolve a terminal.
            row(
                "a worker's script runs on main and the answer comes back",
                false,
                &mut failures,
            );
        }
    }

    // ── the real thing, in process ──────────────────────────────────────
    // M13's benign probe (`version & (count of windows)`) through the
    // in-process runner: the one call that is safe to make unconditionally,
    // because it reads nothing an operator would mind and writes nothing at
    // all. Absent or denied is a skip — a missing grant is never a failure.
    if !std::path::Path::new("/Applications/Ghostty.app").exists() {
        println!("skip  Ghostty is absent, so the in-process round trip has nothing to ask");
    } else {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("deckd-desktop-ghostty".into())
            .spawn(move || {
                let _ = tx.send(ghostty::probe(&AppleScriptRunner::new()));
            })
            .expect("the worker thread");
        match pump_for(&rx, Duration::from_secs(15)) {
            Some(Ok((version, windows))) => {
                println!("      ghostty {version}, {windows} window(s)");
                row(
                    "Ghostty answers the in-process runner from a worker thread",
                    !version.is_empty(),
                    &mut failures,
                );
            }
            Some(Err(e)) if e.denied() => {
                println!("skip  Automation is denied for Ghostty (-1743): {e}");
            }
            Some(Err(e)) => {
                println!("skip  Ghostty did not answer the in-process runner: {e}");
            }
            None => {
                row(
                    "Ghostty answers the in-process runner from a worker thread",
                    false,
                    &mut failures,
                );
            }
        }
    }

    println!("desktop mainloop result: {failures} failed");
    if failures > 0 {
        std::process::exit(1);
    }
}

/// Off macOS there is no `NSAppleScript` and no main run loop to hop to, so
/// there is nothing here to run: the target builds and says it skipped,
/// and no row is printed as passed.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skip  desktop mainloop: NSAppleScript and the main run loop are macOS's alone");
}
