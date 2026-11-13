//! The care verbs (decision 10) against a daemon: `deckd care`, `deckd
//! firmware`, `deckd config` and `deckd device get|set` over the real
//! socket, through the same `deck-client` session `deckd ctl` opens.
//!
//! The daemon is the in-process one the API tests use (`--no-device`, a temp
//! data dir, a `FakeVerifier`), so the ancestry cap can be switched on. The
//! verbs build a runtime of their own for each call, so they run on a
//! blocking thread beside the test's.
//!
//! `care footprint` is not run here: it reads `HOME` and `CODEX_HOME`, which
//! are this test process's own. `tests/no_device.rs` runs it against a real
//! service whose homes are inside its data dir.
mod common;

use common::{Daemon, Options};
use deckd::config::Paths;
use deckd::hid::FromHid;
use deckd::trust::FakeVerifier;
use serde_json::{json, Value};
use std::io::Write;

/// What one verb did: its exit code (or its refusal), stdout and stderr.
#[derive(Debug)]
struct Ran {
    result: Result<i32, String>,
    out: String,
    err: String,
}

impl Ran {
    fn code(&self) -> i32 {
        self.result
            .clone()
            .unwrap_or_else(|why| panic!("refused: {why}"))
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("{e}: {:?}", self.out))
    }
}

type Verb = Box<dyn FnOnce(&Paths, &mut dyn Write, &mut dyn Write) -> Result<i32, String> + Send>;

fn paths_of(daemon: &Daemon) -> Paths {
    Paths::from_data_dir(
        daemon.dir.clone(),
        daemon.dir.join("logs"),
        daemon.dir.join("caches"),
    )
}

/// `deckd <args>` as the binary parses it, run against `paths`.
fn verb(args: &[&str]) -> Verb {
    use clap::Parser;
    let mut argv = vec!["deckd"];
    argv.extend_from_slice(args);
    let cli = deckd::cli::Cli::try_parse_from(argv).expect("the verb parses");
    Box::new(move |paths, out, err| {
        let service = deckd::verbs::Socket::new(&paths.socket);
        match cli.cmd {
            deckd::cli::Cmd::Care { cmd } => deckd::verbs::care::run(&service, cmd, out, err),
            deckd::cli::Cmd::Firmware { cmd } => {
                deckd::verbs::firmware::run(&service, cmd, out, err)
            }
            deckd::cli::Cmd::Config { cmd } => deckd::verbs::config::run(&service, cmd, out, err),
            deckd::cli::Cmd::Device {
                cmd: deckd::cli::DeviceCmd::Deck(cmd),
            } => deckd::verbs::device::run(&service, cmd, out, err),
            other => panic!("not a care verb: {other:?}"),
        }
    })
}

/// Run `args` against `paths` on a blocking thread.
async fn run(paths: &Paths, args: &[&str]) -> Ran {
    let paths = paths.clone();
    let verb = verb(args);
    tokio::task::spawn_blocking(move || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = verb(&paths, &mut out, &mut err);
        Ran {
            result,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    })
    .await
    .expect("the verb's thread")
}

/// `config.get`'s reply, through the verb.
async fn config(paths: &Paths) -> Value {
    let ran = run(paths, &["config", "get", "--json"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    ran.json()
}

/// Pausing writes what the menu writes and the service reads it back;
/// resuming turns the older hooks switch back on in the same patch. An
/// app's switch rewrites the paused list; the Usage face's sources switch
/// by the panel's rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_switches_write_what_the_window_writes() {
    let daemon = Daemon::start().await;
    let paths = paths_of(&daemon);
    let ran = run(&paths, &["config", "set", "hooks.enabled=false"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    let ran = run(&paths, &["care", "pause"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    assert_eq!(config(&paths).await["sources"]["paused"], true);
    let ran = run(&paths, &["care", "sources", "--json"]).await;
    assert_eq!(ran.json()["paused"], true, "{ran:?}");
    let ran = run(&paths, &["care", "resume"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    let after = config(&paths).await;
    assert_eq!(
        (&after["sources"]["paused"], &after["hooks"]["enabled"]),
        (&json!(false), &json!(true))
    );

    let ran = run(&paths, &["care", "source", "app:Conol", "off"]).await;
    assert_eq!((ran.code(), ran.out.as_str()), (0, "app:Conol: off\n"));
    let ran = run(&paths, &["care", "source", "app:Omarchy", "off"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    assert_eq!(
        config(&paths).await["sources"]["apps_paused"],
        json!(["Conol", "Omarchy"])
    );
    run(&paths, &["care", "source", "app:Conol", "on"]).await;
    assert_eq!(
        config(&paths).await["sources"]["apps_paused"],
        json!(["Omarchy"])
    );
    let ran = run(&paths, &["care", "source", "codex", "off"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    assert_eq!(config(&paths).await["codex"]["daemon"], false);

    let ran = run(&paths, &["care", "usage", "claude", "on"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    assert!(
        ran.out.starts_with("Usage face: codex, claude\n"),
        "{}",
        ran.out
    );
    assert_eq!(
        config(&paths).await["usage"]["providers"],
        json!(["codex", "claude"])
    );
    run(&paths, &["care", "usage", "claude", "off"]).await;
    assert_eq!(config(&paths).await["usage"]["providers"], json!(["codex"]));
    daemon.shutdown().await;
}

/// With no deck linked a test sends nothing and says so (exit 1), and the
/// status keeps it; the checks leave the deck unjudged; the firmware is
/// idle and an install is refused before it is asked; the firmware
/// downloads are cleared and the backups kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_deck_the_tests_and_the_firmware_say_why_nothing_happened() {
    let daemon = Daemon::start().await;
    let paths = paths_of(&daemon);
    let ran = run(&paths, &["care", "test", "question", "--wait"]).await;
    assert_eq!(
        (ran.code(), ran.out.as_str()),
        (1, "no deck is linked: nothing was sent\n")
    );
    let ran = run(&paths, &["care", "test", "status", "--json"]).await;
    assert_eq!(ran.json()["question"]["state"], "no_deck", "{ran:?}");
    assert_eq!(ran.json()["voice"], Value::Null);

    let ran = run(&paths, &["care", "checks", "--json"]).await;
    let rows = ran.json()["checks"].as_array().cloned().unwrap();
    let device = rows.iter().find(|row| row["name"] == "device").unwrap();
    assert_eq!(device["ok"], Value::Null, "{device}");
    let failed = rows.iter().any(|row| row["ok"] == false);
    assert_eq!(ran.code(), i32::from(failed), "{ran:?}");

    let ran = run(&paths, &["firmware", "status"]).await;
    assert_eq!(
        (ran.code(), ran.out.as_str()),
        (
            0,
            "idle: Connect your deck with USB to update its firmware.\n"
        )
    );
    let ran = run(&paths, &["firmware", "install", "--board", "hw-b"]).await;
    assert_eq!(
        ran.result,
        Err(deckd::verbs::firmware::NO_RELEASE.to_string())
    );

    let firmware = paths.caches.join("firmware");
    std::fs::create_dir_all(firmware.join("bootloader-backups")).unwrap();
    std::fs::write(firmware.join("release-manifest.json"), [b'x'; 1500]).unwrap();
    std::fs::write(firmware.join("bootloader-backups/F657.bin"), b"boot").unwrap();
    let ran = run(&paths, &["care", "clear", "firmware"]).await;
    assert_eq!(
        (ran.code(), ran.out.as_str()),
        (
            0,
            "cleared the firmware downloads: 2 KB freed; the bootloader backups are kept\n"
        )
    );
    assert!(firmware.join("bootloader-backups/F657.bin").exists());
    daemon.shutdown().await;
}

/// A setting goes through `device.set` and reads back from `device.get`; a
/// value outside the deck's range is refused before it is sent; a strip the
/// deck would refuse is refused with its rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setting_reads_back_and_a_bad_one_never_leaves() {
    let daemon = Daemon::start().await;
    let paths = paths_of(&daemon);
    let ran = run(&paths, &["device", "set", "brightness", "3"]).await;
    assert_eq!((ran.code(), ran.out.as_str()), (0, "brightness set to 3\n"));
    let ran = run(&paths, &["device", "get", "--json"]).await;
    assert_eq!(ran.json()["settings"]["brightness"], 3, "{ran:?}");
    let ran = run(&paths, &["device", "set", "brightness", "9"]).await;
    assert_eq!(ran.result, Err("brightness takes 1 to 5".into()));
    let strip = daemon.dir.join("strip.json");
    std::fs::write(&strip, r#"[{"f":3,"t":"macropad","n":"Build"}]"#).unwrap();
    let ran = run(&paths, &["device", "set-faces", strip.to_str().unwrap()]).await;
    assert_eq!(
        ran.result,
        Err("the deck would not take this strip: agents first".into())
    );
    let ran = run(&paths, &["config", "get", "voice.stt.locale"]).await;
    assert_eq!((ran.code(), ran.out.as_str()), (0, "en-US\n"));
    let ran = run(&paths, &["config", "set", "voice.stt.locale=en-GB"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    assert!(
        ran.out.starts_with("changed: voice.stt.locale\n"),
        "{}",
        ran.out
    );
    let ran = run(&paths, &["config", "get", "voice.stt.locale"]).await;
    assert_eq!(ran.out, "en-GB\n");
    daemon.shutdown().await;
}

/// The decks two links identified are listed by label; the one not in use
/// is forgotten and the one in use is refused, as is a label no deck has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_decks_are_listed_by_label_and_one_not_in_use_is_forgotten() {
    let daemon = Daemon::start().await;
    let paths = paths_of(&daemon);
    let ran = run(&paths, &["care", "decks"]).await;
    assert!(
        ran.out.starts_with("no deck is remembered here yet"),
        "{ran:?}"
    );
    let (a, b) = ("A1B2C3D4E5F6", "C0FFEE15600D");
    daemon.hid(FromHid::Identity(Some(a.into())));
    daemon.hid(FromHid::Identity(Some(b.into())));
    let (label_a, label_b) = (
        deckd::device::identity::label(a),
        deckd::device::identity::label(b),
    );
    daemon
        .until("deck B in use", |snapshot| {
            snapshot.device.deck.as_deref() == Some(label_b.as_str())
        })
        .await;
    let ran = run(&paths, &["care", "decks", "--json"]).await;
    let decks = ran.json()["decks"].as_array().cloned().unwrap();
    assert_eq!(decks.len(), 2, "{ran:?}");
    assert!(!ran.out.contains(a) && !ran.out.contains(b), "{}", ran.out);
    let ran = run(&paths, &["care", "decks"]).await;
    assert!(
        ran.out
            .contains(&format!("{label_b} (in use): never linked here\n")),
        "{}",
        ran.out
    );
    let ran = run(&paths, &["care", "decks", "forget", &label_b]).await;
    assert_eq!(ran.code(), 1, "{ran:?}");
    assert!(ran.err.contains("in use"), "{}", ran.err);
    let ran = run(&paths, &["care", "decks", "forget", "0badc0de"]).await;
    assert_eq!(ran.code(), 1, "{ran:?}");
    let ran = run(&paths, &["care", "decks", "forget", &label_a]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    let ran = run(&paths, &["care", "decks", "--json"]).await;
    assert_eq!(
        ran.json(),
        json!({"decks": [{"deck": label_b, "current": true, "last_linked": null,
                          "faces": [], "macros": 0}]})
    );
    daemon.shutdown().await;
}

/// A terminal an agent started is refused `admin`: the verb says why and
/// exits 1, while a verb that only reads still answers. With no service at
/// all, a verb exits 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_terminal_reads_but_is_told_why_it_may_not_change() {
    let daemon = Daemon::with(Options {
        verifier: FakeVerifier {
            capped: true,
            ..FakeVerifier::default()
        },
        ..Options::default()
    })
    .await;
    let paths = paths_of(&daemon);
    let ran = run(&paths, &["care", "pause"]).await;
    assert_eq!((ran.code(), ran.out.as_str()), (1, ""));
    assert!(
        ran.err
            .ends_with(&format!("deckd: {}\n", deckd::cli::ANCESTRY_HINT)),
        "{}",
        ran.err
    );
    let ran = run(&paths, &["care", "sources"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    let ran = run(&paths, &["device", "get"]).await;
    assert_eq!(ran.code(), 0, "{ran:?}");
    let ran = run(&paths, &["device", "set", "brightness", "2"]).await;
    assert_eq!(ran.code(), 1, "{ran:?}");
    daemon.shutdown().await;

    let gone = Paths::from_data_dir(
        paths.data.join("nowhere"),
        paths.data.join("nowhere/logs"),
        paths.data.join("nowhere/caches"),
    );
    let ran = run(&gone, &["care", "checks"]).await;
    assert_eq!(ran.code(), 2, "{ran:?}");
    assert!(
        ran.err
            .ends_with(&format!("deckd: {}\n", deckd::verbs::care::DOCTOR_HINT)),
        "{}",
        ran.err
    );
}
