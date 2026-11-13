//! `deckd firmware status|check|install`: the Service page's firmware row
//! and its review dialog, from a terminal.
//!
//! **An install never guesses.** It reads the status first and refuses, before
//! the service is asked, what the dialog would not offer: nothing to install,
//! a service already busy with the deck, a deck it has not read on the cable,
//! a board neither the deck nor the owner names (a deck in recovery names
//! none, and a wrong board's image leaves the deck dark), a release with no
//! build for the board, and a release that may replace the bootloader without
//! `--allow-bootloader-update`. The service checks all of it again, and a deck
//! that can show a card still asks for ENTER before anything is written.
use super::{ask, finish, follow, print_json, text, Service, Stop, ADMIN, OBSERVE};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::Write;

/// The boards a release builds for, as the firmware names them, and as they
/// are printed on the board (the app's `updates::BOARDS`).
pub const BOARDS: [(&str, &str); 2] = [("hw-a", "Rev A"), ("hw-b", "Rev B")];

/// How many seconds a `--wait` follows a check, and an install (a press on
/// the deck, a download, the write and the deck's return).
pub const CHECK_READS: u32 = 120;
pub const INSTALL_READS: u32 = 900;

pub const NO_RELEASE: &str =
    "no release is known yet: run `deckd firmware check` with the deck on the cable first";
pub const NO_DECK: &str =
    "no deck has been read on the cable: plug it in by USB and run `deckd firmware check`";
pub const NO_BOARD: &str =
    "the deck in recovery does not say which board it is: pass --board hw-a \
                            or --board hw-b, the board printed on it";
pub const BOOTLOADER: &str =
    "this update may replace the bootloader, the software that starts the \
                              deck: keep USB-C firmly connected and the Mac awake until it \
                              finishes, because losing power then can leave the deck unable to \
                              start or update over USB. Pass --allow-bootloader-update to accept \
                              it";

#[derive(Subcommand, Debug)]
pub enum FirmwareCmd {
    /// What the service knows of the deck's firmware and the latest release
    /// (`firmware.status`).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Have the service look for a release and read the deck on the cable
    /// (`firmware.check`). It looks at most every six hours unless
    /// `--force`; `--wait` follows the check to its end.
    Check {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install the release the last check found on the deck on the cable
    /// (`firmware.install`). A deck that can show a card asks for ENTER
    /// first; `--wait` follows the install to its end.
    Install {
        /// The board printed on the deck, for a deck in recovery that does
        /// not name its own.
        #[arg(long, value_parser = ["hw-a", "hw-b"])]
        board: Option<String>,
        /// Accept a release that may replace the bootloader.
        #[arg(long)]
        allow_bootloader_update: bool,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
}

/// `deckd firmware …`.
pub fn run(
    service: &dyn Service,
    cmd: FirmwareCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(firmware(service, cmd, out, err))
}

fn firmware(
    service: &dyn Service,
    cmd: FirmwareCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        FirmwareCmd::Status { json } => {
            let status = ask(service, OBSERVE, "firmware.status", json!({}), err)?;
            show(out, &status, json)?;
            Ok(0)
        }
        FirmwareCmd::Check { force, wait, json } => {
            let mut status = ask(
                service,
                ADMIN,
                "firmware.check",
                json!({ "force": force }),
                err,
            )?;
            if wait && busy(&status) {
                status = follow_status(service, status, CHECK_READS, json, out, err)?;
            }
            show(out, &status, json)?;
            // Started and not followed is done; followed, it has to end.
            Ok(i32::from(failed(&status) || (wait && busy(&status))))
        }
        FirmwareCmd::Install {
            board,
            allow_bootloader_update,
            wait,
            json,
        } => {
            let status = ask(service, OBSERVE, "firmware.status", json!({}), err)?;
            let params = install_params(&status, board.as_deref(), allow_bootloader_update)
                .map_err(Stop::Refused)?;
            let version = text(&params, "version").to_string();
            let mut status = ask(service, ADMIN, "firmware.install", params, err)?;
            if confirming(&status) && !json {
                writeln!(out, "press ENTER on the deck to install {version}")?;
            }
            if !wait {
                if !(confirming(&status) && !json) {
                    show(out, &status, json)?;
                }
                return Ok(i32::from(failed(&status)));
            }
            status = follow_status(service, status, INSTALL_READS, json, out, err)?;
            show(out, &status, json)?;
            Ok(i32::from(text(&status, "phase") != "complete"))
        }
    }
}

/// The status as JSON, or as [`status_text`] says it.
fn show(out: &mut dyn Write, status: &Value, json: bool) -> Result<(), Stop> {
    if json {
        print_json(out, status)
    } else {
        write!(out, "{}", status_text(status))?;
        Ok(())
    }
}

/// Read the status every second until the service is no longer busy with
/// the deck, at most `reads` times, saying each new step in text.
pub(crate) fn follow_status(
    service: &dyn Service,
    first: Value,
    reads: u32,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Value, Stop> {
    let mut said = step_line(&first);
    let mut last = first;
    let read = || ask(service, OBSERVE, "firmware.status", json!({}), &mut *err);
    let settled = |status: &Value| {
        let line = step_line(status);
        if !json && busy(status) && line != said {
            let _ = writeln!(out, "{line}");
            said = line;
        }
        last = status.clone();
        (!busy(status)).then_some(())
    };
    follow(service, read, settled, reads)?;
    Ok(last)
}

/// Whether the service is working on the deck: checking, waiting for its
/// press, downloading, installing, waiting for it to come back.
#[must_use]
pub fn busy(status: &Value) -> bool {
    matches!(
        text(status, "phase"),
        "checking" | "confirming" | "downloading" | "installing" | "reconnecting" | "bootloader"
    )
}

/// Whether the install waits for ENTER on the deck.
#[must_use]
pub fn confirming(status: &Value) -> bool {
    text(status, "phase") == "confirming"
}

/// Whether the last check or install ended in an error.
#[must_use]
pub fn failed(status: &Value) -> bool {
    text(status, "phase") == "error" || status["error"].is_string()
}

/// The release a check found newer than the deck's firmware, by its version.
#[must_use]
pub fn available(status: &Value) -> Option<String> {
    let version = text(&status["release"]["firmware"], "version");
    (status["available"] == true && !version.is_empty()).then(|| version.to_string())
}

/// Whether the install may replace the bootloader: the service says so, or
/// a release of the second protocol meets a deck whose recovery is older, or
/// one already in maintenance (the app's rule).
#[must_use]
pub fn bootloader_warning(status: &Value) -> bool {
    status["bootloader_warning"] == true
        || (status["release"]["firmware"]["protocol"] == 2
            && (status["device"]["recovery_protocol"] != 2
                || status["device"]["maintenance"] == true))
}

/// What `firmware.install` is sent for `status`, `board` (`--board`) and
/// the owner's consent to a bootloader update, or why nothing is: the
/// dialog's own conditions, and never a guessed board.
pub fn install_params(
    status: &Value,
    board: Option<&str>,
    allow_bootloader_update: bool,
) -> Result<Value, String> {
    if busy(status) {
        return Err(format!(
            "the service is busy with the firmware ({}): `deckd firmware status` says when it is \
             done",
            text(status, "phase")
        ));
    }
    let device = &status["device"];
    let release = text(&status["release"]["firmware"], "version");
    if release.is_empty() {
        return Err(NO_RELEASE.into());
    }
    let serial = text(device, "serial");
    if device.is_null() || serial.is_empty() {
        return Err(NO_DECK.into());
    }
    let Some(version) = available(status) else {
        return Err(format!(
            "the deck already runs {}, and the release is {release}: there is nothing newer to \
             install",
            device["version"].as_str().unwrap_or("its firmware")
        ));
    };
    let named = text(device, "board");
    let board = match (board, named) {
        (Some(picked), "") => picked,
        (Some(picked), named) if picked != named => {
            return Err(format!(
                "the deck says it is {named}; --board {picked} does not match it"
            ))
        }
        (_, "") => return Err(NO_BOARD.into()),
        (_, named) => named,
    };
    if status["release"]["firmware"]["variants"]
        .get(board)
        .is_none()
    {
        return Err(format!("release {version} has no firmware for {board}"));
    }
    let warning = bootloader_warning(status);
    if warning && !allow_bootloader_update {
        return Err(BOOTLOADER.into());
    }
    Ok(json!({
        "board": board,
        "serial": serial,
        "version": version,
        "allow_bootloader_update": warning,
    }))
}

/// A board as it is printed on it, else as the firmware names it.
fn board_name(board: &str) -> &str {
    BOARDS
        .iter()
        .find(|(id, _)| *id == board)
        .map_or(board, |(_, name)| name)
}

/// The step a status is at: its phase and message, with the progress while
/// the service works on the deck.
fn step_line(status: &Value) -> String {
    let (phase, message) = (text(status, "phase"), text(status, "message"));
    if busy(status) && !confirming(status) {
        format!(
            "{phase}: {message} {}%",
            status["progress"].as_u64().unwrap_or(0)
        )
    } else {
        format!("{phase}: {message}")
    }
}

/// `firmware.status` as a person reads it: the step, the deck (never its
/// serial number, which is its Bluetooth address), the release, the linked
/// deck's firmware against it when no deck is on the cable, an error and the
/// bootloader's warning.
#[must_use]
pub fn status_text(status: &Value) -> String {
    let mut words = format!("{}\n", step_line(status));
    let device = &status["device"];
    if device.is_object() {
        let mut deck = format!(
            "deck: {}",
            match text(device, "board") {
                "" => "board not named".to_string(),
                board => board_name(board).to_string(),
            }
        );
        if let Some(version) = device["version"].as_str() {
            deck.push_str(&format!(", firmware {version}"));
        }
        if device["recovery"] == true {
            deck.push_str(", in recovery");
        }
        if device["maintenance"] == true {
            deck.push_str(", in maintenance");
        }
        words.push_str(&format!("{deck}\n"));
    }
    let release = text(&status["release"]["firmware"], "version");
    if !release.is_empty() {
        let compared = if available(status).is_some() {
            ", newer than the deck's: `deckd firmware install`"
        } else if device.is_object() {
            ", not newer than the deck's"
        } else {
            ""
        };
        words.push_str(&format!("release: {release}{compared}\n"));
    }
    // A deck on Bluetooth alone is never read on the cable: the firmware its
    // hello named (`linked`, R8) is what says whether it is behind, as the
    // window's "Deck firmware … is available" does. On the cable, the deck
    // line above says it.
    let linked = &status["linked"];
    if !device.is_object() && linked.is_object() {
        let fw = text(linked, "fw");
        let compared = if linked["available"] == true {
            format!(", older than {release}: plug it in by USB and run `deckd firmware check`")
        } else if release.is_empty() {
            String::new()
        } else {
            format!(", not older than {release}")
        };
        words.push_str(&format!("linked deck: firmware {fw}{compared}\n"));
    }
    if let Some(error) = status["error"].as_str() {
        words.push_str(&format!("error: {error}\n"));
    }
    if bootloader_warning(status) && !release.is_empty() {
        words.push_str("note: this update may replace the bootloader\n");
    }
    words
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{fixture, run, Script};
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<FirmwareCmd, clap::Error> {
        let mut argv = vec!["deckd", "firmware"];
        argv.extend_from_slice(args);
        crate::cli::Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            crate::cli::Cmd::Firmware { cmd } => cmd,
            other => panic!("{other:?}"),
        })
    }

    fn firmware_run(script: &Script, args: &[&str]) -> super::super::testkit::Ran {
        let cmd = parse(args).unwrap();
        run(|out, err| super::run(script, cmd, out, err))
    }

    /// A deck on Bluetooth alone is never read on the cable, so the status
    /// names no `device`; the firmware its hello named (`linked`) is what
    /// says whether it is behind the release, and the line says how to
    /// install it. On the cable the deck line says it already.
    #[test]
    fn a_deck_on_bluetooth_alone_is_compared_by_the_firmware_its_hello_named() {
        let status = json!({
            "phase": "ready",
            "message": "Firmware 0.13.0 is available.",
            "release": {"firmware": {"version": "0.13.0"}},
            "linked": {"fw": "0.12.0-openwalkietalkie", "available": true},
        });
        assert_eq!(
            status_text(&status),
            "ready: Firmware 0.13.0 is available.\nrelease: 0.13.0\nlinked deck: firmware \
             0.12.0-openwalkietalkie, older than 0.13.0: plug it in by USB and run `deckd \
             firmware check`\n"
        );
        let mut current = status.clone();
        current["linked"] = json!({"fw": "0.13.0-openwalkietalkie", "available": false});
        assert!(
            status_text(&current).ends_with(
                "linked deck: firmware 0.13.0-openwalkietalkie, not older than 0.13.0\n"
            ),
            "{}",
            status_text(&current)
        );
        let mut cabled = status.clone();
        cabled["device"] = json!({"board": "hw-b", "version": "0.12.0"});
        assert!(
            !status_text(&cabled).contains("linked deck"),
            "{}",
            status_text(&cabled)
        );
    }

    /// A release newer than the deck on the cable: Rev B, protocol 2 on
    /// both, nothing in recovery.
    fn found() -> Value {
        json!({
            "phase": "ready", "progress": 0, "message": "Firmware 0.13.0 is available.",
            "available": true, "bootloader_warning": false, "error": null,
            "device": {"serial": "A1B2C3D4E5F6", "board": "hw-b", "version": "0.12.0",
                       "recovery": false, "recovery_protocol": 2, "maintenance": false},
            "release": {"firmware": {"version": "0.13.0", "protocol": 2,
                        "variants": {"hw-a": {}, "hw-b": {}}}},
        })
    }

    #[test]
    fn the_firmware_verbs_parse_and_a_board_the_firmware_does_not_name_is_refused() {
        for args in [
            &["status"][..],
            &["status", "--json"],
            &["check"],
            &["check", "--force", "--wait"],
            &["install"],
            &[
                "install",
                "--board",
                "hw-a",
                "--allow-bootloader-update",
                "--wait",
            ],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        for args in [
            &["install", "--board", "lilygo"][..],
            &["install", "--board", "Rev B"],
            &[],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    /// The install the dialog would offer is the one sent: the deck's own
    /// board, its serial, the release, and consent only where it is asked.
    #[test]
    fn an_install_is_sent_only_for_what_the_dialog_would_offer() {
        assert_eq!(
            install_params(&found(), None, false),
            Ok(
                json!({"board": "hw-b", "serial": "A1B2C3D4E5F6", "version": "0.13.0",
                      "allow_bootloader_update": false})
            )
        );
        // Busy, nothing found, no deck, nothing newer.
        let mut busy = found();
        busy["phase"] = json!("downloading");
        assert!(install_params(&busy, None, false)
            .unwrap_err()
            .contains("busy"));
        let idle = fixture("recorded/firmware.status.json");
        assert_eq!(install_params(&idle, None, false), Err(NO_RELEASE.into()));
        let mut no_deck = found();
        no_deck["device"] = Value::Null;
        no_deck["available"] = json!(false);
        assert_eq!(install_params(&no_deck, None, false), Err(NO_DECK.into()));
        let mut current = found();
        current["available"] = json!(false);
        assert!(install_params(&current, None, false)
            .unwrap_err()
            .starts_with("the deck already runs 0.12.0"));
        // A deck in recovery names no board: never guessed, always asked.
        let mut recovery = found();
        recovery["device"]["board"] = Value::Null;
        recovery["device"]["recovery"] = json!(true);
        assert_eq!(install_params(&recovery, None, false), Err(NO_BOARD.into()));
        assert_eq!(
            install_params(&recovery, Some("hw-a"), false).unwrap()["board"],
            "hw-a"
        );
        // A board that contradicts the deck's own is refused, not obeyed.
        assert_eq!(
            install_params(&found(), Some("hw-a"), false),
            Err("the deck says it is hw-b; --board hw-a does not match it".into())
        );
        assert_eq!(
            install_params(&found(), Some("hw-b"), false).unwrap()["board"],
            "hw-b"
        );
        let mut one_board = found();
        one_board["release"]["firmware"]["variants"] = json!({"hw-a": {}});
        assert_eq!(
            install_params(&one_board, None, false),
            Err("release 0.13.0 has no firmware for hw-b".into())
        );
        // A bootloader update needs the owner's word, and only then is sent.
        let mut old_recovery = found();
        old_recovery["device"]["recovery_protocol"] = json!(1);
        assert_eq!(
            install_params(&old_recovery, None, false),
            Err(BOOTLOADER.into())
        );
        assert_eq!(
            install_params(&old_recovery, None, true).unwrap()["allow_bootloader_update"],
            true
        );
        let mut flagged = found();
        flagged["bootloader_warning"] = json!(true);
        assert_eq!(
            install_params(&flagged, None, false),
            Err(BOOTLOADER.into())
        );
    }

    /// A refused install asks the service nothing past the status.
    #[test]
    fn a_refused_install_asks_nothing_past_the_status() {
        let script = Script::new(vec![Ok(fixture("recorded/firmware.status.json"))]);
        let ran = firmware_run(&script, &["install"]);
        assert_eq!(ran.result, Err(NO_RELEASE.into()));
        assert_eq!(script.methods(), ["firmware.status"]);
        assert_eq!(script.call(0).scopes, Vec::<String>::new());
    }

    /// On a deck that can show a card the install waits for ENTER there;
    /// with `--wait` it is followed to its end, each new step said once.
    #[test]
    fn an_install_says_to_press_enter_and_is_followed_to_its_end() {
        let at = |phase: &str, message: &str, progress: u64| {
            let mut status = found();
            status["phase"] = json!(phase);
            status["message"] = json!(message);
            status["progress"] = json!(progress);
            status
        };
        let confirming = at(
            "confirming",
            "Press ENTER on the deck to start the install.",
            0,
        );
        let script = Script::new(vec![Ok(found()), Ok(confirming.clone())]);
        let ran = firmware_run(&script, &["install"]);
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (Ok(0), "press ENTER on the deck to install 0.13.0\n")
        );
        assert_eq!(script.methods(), ["firmware.status", "firmware.install"]);
        assert_eq!(script.call(1).scopes, ["admin"]);
        let script = Script::new(vec![
            Ok(found()),
            Ok(confirming.clone()),
            Ok(confirming),
            Ok(at("downloading", "Downloading and verifying firmware…", 40)),
            Ok(at("downloading", "Downloading and verifying firmware…", 40)),
            Ok(at("installing", "Installing…", 70)),
            Ok(at("complete", "Firmware updated. Your deck is ready.", 100)),
        ]);
        let ran = firmware_run(&script, &["install", "--wait"]);
        assert_eq!(ran.result, Ok(0));
        let lines: Vec<&str> = ran.out.lines().collect();
        assert_eq!(
            lines[..4],
            [
                "press ENTER on the deck to install 0.13.0",
                "downloading: Downloading and verifying firmware… 40%",
                "installing: Installing… 70%",
                "complete: Firmware updated. Your deck is ready.",
            ]
        );
        // Not confirmed on the deck: idle again, nothing installed, exit 1.
        let script = Script::new(vec![
            Ok(found()),
            Ok(at(
                "confirming",
                "Press ENTER on the deck to start the install.",
                0,
            )),
            Ok(at(
                "idle",
                "Not confirmed on the deck. Nothing was installed.",
                0,
            )),
        ]);
        let ran = firmware_run(&script, &["install", "--wait"]);
        assert_eq!(ran.result, Ok(1));
        assert!(ran
            .out
            .contains("idle: Not confirmed on the deck. Nothing was installed.\n"));
    }

    /// The status as a person reads it, with the deck's board by the name
    /// printed on it and never its serial number.
    #[test]
    fn the_status_names_the_step_the_deck_and_the_release_never_the_serial() {
        let words = status_text(&found());
        assert_eq!(
            words,
            "ready: Firmware 0.13.0 is available.\ndeck: Rev B, firmware 0.12.0\nrelease: 0.13.0, \
             newer than the deck's: `deckd firmware install`\n"
        );
        assert!(!words.contains("A1B2C3D4E5F6"));
        assert_eq!(
            status_text(&fixture("recorded/firmware.status.json")),
            "idle: Connect your deck with USB to update its firmware.\n"
        );
        let mut broken = found();
        broken["phase"] = json!("error");
        broken["error"] = json!("the deck did not come back");
        broken["available"] = json!(false);
        broken["device"]["recovery"] = json!(true);
        broken["device"]["board"] = Value::Null;
        let words = status_text(&broken);
        assert!(
            words.contains("deck: board not named, firmware 0.12.0, in recovery\n"),
            "{words}"
        );
        assert!(
            words.contains("release: 0.13.0, not newer than the deck's\n"),
            "{words}"
        );
        assert!(
            words.ends_with("error: the deck did not come back\n"),
            "{words}"
        );
        assert!(failed(&broken) && !failed(&found()));
        // A check that ends in an error exits 1.
        let script = Script::new(vec![Ok(broken)]);
        assert_eq!(firmware_run(&script, &["check", "--force"]).result, Ok(1));
        assert_eq!(script.call(0).params, json!({"force": true}));
    }
}
