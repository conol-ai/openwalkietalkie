//! A command for everything the companion app does for the deck (decision
//! 10): `deckd care …`, `deckd firmware …`, `deckd config …`, `deckd service
//! …` and `deckd device get|set|faces|set-faces|set-macro`. Each verb calls
//! the method the window calls for the same control (§10.7 of
//! `docs/host-service.md` has the table), so the service decides for a
//! terminal exactly as it does for the window, and refuses the same things.
//!
//! - **`--json` is the stable form**: the method's reply as the service
//!   gave it, for a script. The text is for a person, in English like the
//!   rest of the CLI, and may change.
//! - **Exit codes** are `deckd ctl`'s: 0 done; 1 refused, failed, or a
//!   check that did not pass (a command line that is not understood
//!   included: clap's own 2 is not used); 2 the service did not answer; 3
//!   it wants a trusted principal.
//! - **On macOS a terminal that Claude Code or Codex started** gets no
//!   `admin` or `device` session (the ancestry cap, which stays): those
//!   verbs say so and exit 1, while the ones that only read (`care
//!   sources`, `care permissions`, `firmware status`, `device get`, `device
//!   faces`) still answer.
//!
//! A verb reaches the service through [`Service`]: one call on a fresh
//! session, as `deckd ctl` makes one ([`Socket`]). The tests drive the verbs
//! through a scripted one, with no socket.
pub mod care;
pub mod config;
pub mod device;
pub mod firmware;
pub mod service;

use crate::cli::CtlFailure;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The scopes a verb asks for. No scope is the socket's default, `observe`,
/// which a terminal an agent started is still given.
pub(crate) const OBSERVE: &[&str] = &[];
pub(crate) const VOICE: &[&str] = &["voice"];
pub(crate) const DEVICE: &[&str] = &["device"];
pub(crate) const ADMIN: &[&str] = &["admin"];

/// How often a verb that follows a status reads it again.
pub(crate) const EVERY: Duration = Duration::from_secs(1);

/// How a verb reaches the running service.
pub trait Service {
    /// One call on a fresh session that asks for `scopes`.
    fn call(&self, scopes: &[&str], method: &str, params: Value) -> Result<Value, CtlFailure>;

    /// The wait between two reads of a status a verb follows (`--wait`).
    fn pause(&self, every: Duration) {
        std::thread::sleep(every);
    }
}

/// The service at its socket, reached the way `deckd ctl` reaches it.
pub struct Socket(PathBuf);

impl Socket {
    #[must_use]
    pub fn new(socket: &Path) -> Socket {
        Socket(socket.to_path_buf())
    }
}

impl Service for Socket {
    fn call(&self, scopes: &[&str], method: &str, params: Value) -> Result<Value, CtlFailure> {
        crate::cli::uds_call(&self.0, scopes, method, params)
    }
}

/// A family's entry point (`care::run`, `firmware::run`, …): the service,
/// the parsed command, stdout and stderr; the exit code, or the refusal
/// `cli::main` prints.
pub type Run<C> = fn(&dyn Service, C, &mut dyn Write, &mut dyn Write) -> Result<i32, String>;

/// Why a verb stopped short.
#[derive(Debug)]
pub(crate) enum Stop {
    /// A refusal of the verb's own, before a call or after one it read:
    /// `deckd: …` on stderr, exit 1.
    Refused(String),
    /// A call that failed and was reported on stderr: its exit code.
    Failed(i32),
}

impl From<std::io::Error> for Stop {
    fn from(error: std::io::Error) -> Stop {
        Stop::Refused(error.to_string())
    }
}

/// A verb's end as `cli::run` takes it: the exit code, or the refusal it
/// prints.
pub(crate) fn finish(result: Result<i32, Stop>) -> Result<i32, String> {
    match result {
        Ok(code) | Err(Stop::Failed(code)) => Ok(code),
        Err(Stop::Refused(why)) => Err(why),
    }
}

/// One call. A failure is reported on `err` as `deckd ctl` reports one,
/// with its hints (`cli::report`), and stops the verb with that exit code.
pub(crate) fn ask(
    service: &dyn Service,
    scopes: &[&str],
    method: &str,
    params: Value,
    err: &mut dyn Write,
) -> Result<Value, Stop> {
    service
        .call(scopes, method, params)
        .map_err(|failure| Stop::Failed(crate::cli::report(failure, err)))
}

/// A reply as `--json` prints it.
pub(crate) fn print_json(out: &mut dyn Write, value: &Value) -> Result<(), Stop> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Stop::Refused(e.to_string()))?;
    writeln!(out, "{text}")?;
    Ok(())
}

/// A string field, empty when it is not one.
pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The JSON document in `file`, or on stdin for `-`.
pub(crate) fn read_json(file: &Path) -> Result<Value, Stop> {
    let (name, raw) = if file == Path::new("-") {
        let mut raw = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw)?;
        ("stdin".to_string(), raw)
    } else {
        let raw = std::fs::read_to_string(file)
            .map_err(|e| Stop::Refused(format!("{}: {e}", file.display())))?;
        (file.display().to_string(), raw)
    };
    serde_json::from_str(&raw).map_err(|e| Stop::Refused(format!("{name} is not JSON: {e}")))
}

/// Read a status every [`EVERY`] until `settled` makes something of it, at
/// most `reads` times; `None` when it never did. The first read is at once.
pub(crate) fn follow<T>(
    service: &dyn Service,
    mut read: impl FnMut() -> Result<Value, Stop>,
    mut settled: impl FnMut(&Value) -> Option<T>,
    reads: u32,
) -> Result<Option<T>, Stop> {
    for n in 0..reads {
        if n > 0 {
            service.pause(EVERY);
        }
        if let Some(end) = settled(&read()?) {
            return Ok(Some(end));
        }
    }
    Ok(None)
}

/// A size as the app's pages print it (`present::size_text` there): whole
/// kilobytes and megabytes, gigabytes with one decimal, each rounded to the
/// nearest, the unit stepping up where the number would reach a thousand;
/// a file that is not empty is never `0 KB`.
#[must_use]
pub fn size_text(bytes: u64) -> String {
    if bytes < 999_500 {
        let kilobytes = (bytes + 500) / 1_000;
        format!("{} KB", kilobytes.max(u64::from(bytes > 0)))
    } else if bytes < 999_500_000 {
        format!("{} MB", (bytes + 500_000) / 1_000_000)
    } else {
        let tenths = bytes.saturating_add(50_000_000) / 100_000_000;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    }
}

/// A scripted service for the verbs' tests: each call takes the next reply,
/// and every call is kept so a test can read what was asked.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// One call as the verb made it.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Asked {
        pub scopes: Vec<String>,
        pub method: String,
        pub params: Value,
    }

    #[derive(Default)]
    pub struct Script {
        replies: RefCell<VecDeque<Result<Value, CtlFailure>>>,
        pub asked: RefCell<Vec<Asked>>,
        pub pauses: RefCell<u32>,
    }

    impl Script {
        pub fn new(replies: Vec<Result<Value, CtlFailure>>) -> Script {
            Script {
                replies: RefCell::new(replies.into()),
                ..Script::default()
            }
        }

        /// The methods called, in order.
        pub fn methods(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|a| a.method.clone())
                .collect()
        }

        /// The `n`th call.
        pub fn call(&self, n: usize) -> Asked {
            self.asked.borrow()[n].clone()
        }
    }

    impl Service for Script {
        fn call(&self, scopes: &[&str], method: &str, params: Value) -> Result<Value, CtlFailure> {
            self.asked.borrow_mut().push(Asked {
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                method: method.into(),
                params,
            });
            self.replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| panic!("no reply scripted for {method}"))
        }

        fn pause(&self, _every: Duration) {
            *self.pauses.borrow_mut() += 1;
        }
    }

    /// What a verb wrote, and its exit code or refusal.
    pub struct Ran {
        pub result: Result<i32, String>,
        pub out: String,
        pub err: String,
    }

    /// Run `verb` with fresh stdout and stderr.
    pub fn run(verb: impl FnOnce(&mut dyn Write, &mut dyn Write) -> Result<i32, String>) -> Ran {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = verb(&mut out, &mut err);
        Ran {
            result,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    /// A fixture of the app's recorded or hand-written daemon replies.
    pub fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../deck-gui-core/tests/fixtures/daemon")
            .join(name);
        let raw =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_str(&raw).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;
    use deck_model::ApiError;
    use serde_json::json;

    /// The app's own table (`present::size_text`'s test): the same words for
    /// the same bytes on the Footprint page and in a terminal.
    #[test]
    fn sizes_read_as_the_footprint_page_prints_them() {
        for (bytes, said) in [
            (4_096, "4 KB"),
            (180_000, "180 KB"),
            (9_400_000, "9 MB"),
            (77_000_000, "77 MB"),
            (428_000_000, "428 MB"),
            (2_100_000_000, "2.1 GB"),
            (1_624_555_275, "1.6 GB"),
            (364_816_464, "365 MB"),
            (0, "0 KB"),
            (1, "1 KB"),
            (499, "1 KB"),
            (1_499, "1 KB"),
            (1_500, "2 KB"),
            (999_499, "999 KB"),
            (999_500, "1 MB"),
            (999_499_999, "999 MB"),
            (999_500_000, "1.0 GB"),
            (12_345_000_000, "12.3 GB"),
        ] {
            assert_eq!(size_text(bytes), said, "{bytes} bytes");
        }
        assert!(size_text(u64::MAX).ends_with(" GB"));
    }

    /// A failed call is reported as `deckd ctl` reports one and becomes the
    /// verb's exit code; a refusal of the verb's own is the error `cli::main`
    /// prints.
    #[test]
    fn a_failed_call_is_reported_and_ends_the_verb_with_its_code() {
        let script = Script::new(vec![
            Err(CtlFailure::Unreachable("cannot reach deckd".into())),
            Err(CtlFailure::Rpc(ApiError::forbidden_ancestry("admin"))),
        ]);
        let mut err = Vec::new();
        let first = ask(&script, ADMIN, "care.checks", json!({}), &mut err);
        assert!(matches!(first, Err(Stop::Failed(2))), "{first:?}");
        let second = ask(&script, ADMIN, "care.checks", json!({}), &mut err);
        assert!(matches!(second, Err(Stop::Failed(1))), "{second:?}");
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains("deckd: cannot reach deckd\n"), "{err}");
        assert!(
            err.contains(&format!("deckd: {}\n", crate::cli::ANCESTRY_HINT)),
            "{err}"
        );
        assert_eq!(finish(Ok(0)), Ok(0));
        assert_eq!(finish(Err(Stop::Failed(2))), Ok(2));
        assert_eq!(finish(Err(Stop::Refused("no".into()))), Err("no".into()));
    }

    /// A followed status is read at once, then once a second, until it
    /// settles or the reads run out.
    #[test]
    fn a_followed_status_is_read_until_it_settles_or_the_reads_run_out() {
        let states =
            |states: &[&str]| Script::new(states.iter().map(|s| Ok(json!({"state": s}))).collect());
        let script = states(&["sent", "sent", "answered"]);
        let read = || {
            ask(
                &script,
                ADMIN,
                "care.test.status",
                json!({}),
                &mut Vec::new(),
            )
        };
        let settled = |status: &Value| (status["state"] != "sent").then(|| status["state"].clone());
        let ended = follow(&script, read, settled, 5).unwrap();
        assert_eq!(ended, Some(json!("answered")));
        assert_eq!((script.methods().len(), *script.pauses.borrow()), (3, 2));
        let script = states(&["sent", "sent"]);
        let read = || {
            ask(
                &script,
                ADMIN,
                "care.test.status",
                json!({}),
                &mut Vec::new(),
            )
        };
        assert_eq!(follow(&script, read, settled, 2).unwrap(), None);
        assert_eq!(*script.pauses.borrow(), 1);
    }

    /// A strip or a set of keys comes from a file, or from stdin for `-`; a
    /// file that is not JSON is refused with its name.
    #[test]
    fn a_json_argument_is_read_from_its_file_and_named_when_it_is_not_json() {
        let dir = crate::integrations::testdir::TestDir::new("verbs-read-json");
        let good = dir.path().join("strip.json");
        std::fs::write(&good, "[1, 2]").unwrap();
        assert_eq!(read_json(&good).unwrap(), json!([1, 2]));
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "[1,").unwrap();
        match read_json(&bad) {
            Err(Stop::Refused(why)) => {
                assert!(
                    why.starts_with(&format!("{} is not JSON", bad.display())),
                    "{why}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read_json(&dir.path().join("absent.json")),
            Err(Stop::Refused(_))
        ));
    }
}
