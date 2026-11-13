//! `deckd device get|set|faces|set-faces|set-macro`: the Deck page's
//! settings and the Faces & Keys page's strip and Macropad keys.
//!
//! A setting's value is checked against the range the deck takes, and a
//! strip against the deck's rules (`deck_proto::strip_problem`), before the
//! service is asked: the service refuses both the same way, and this way the
//! refusal is a sentence. The settings are the ones `device.set` names
//! (`textsize`, where the deck's own line says `text`).
use super::{ask, finish, print_json, read_json, text, Service, Stop, DEVICE, OBSERVE};
use clap::Subcommand;
use deck_model::{FaceEntry, MacroKey, SetKey};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum DeckCmd {
    /// The deck as the service knows it (`device.get`): its link, the bus it
    /// is on, its battery, firmware and board, its settings.
    Get {
        #[arg(long)]
        json: bool,
    },
    /// Change one of the deck's settings (`device.set`): language, speaker,
    /// mic, announce, chime, autojump, brightness, idlehome, demo, textsize
    /// or arcadesound.
    Set { key: String, value: u16 },
    /// The deck's face strip as it last reported it, the face it shows
    /// marked.
    Faces {
        #[arg(long)]
        json: bool,
    },
    /// Send the deck a whole face strip (`device.faces.set`), from a JSON
    /// file or `-` for stdin: `[{"f":1,"t":"agents","n":"Agents"}, …]`.
    SetFaces { file: PathBuf },
    /// Send one Macropad face its keys (`device.macro.set`), from a JSON
    /// file or `-` for stdin: `[{"label":"Build","binding":"left",
    /// "keys":"cmd+b"}, …]`.
    SetMacro {
        /// The Macropad face's id on the strip.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        face: u8,
        /// The layout's name.
        #[arg(long)]
        layout: String,
        /// The tag the deck shows in the face's header.
        #[arg(long)]
        src: Option<String>,
        file: PathBuf,
    },
}

/// `deckd device get|set|faces|set-faces|set-macro`.
pub fn run(
    service: &dyn Service,
    cmd: DeckCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(deck(service, cmd, out, err))
}

fn deck(
    service: &dyn Service,
    cmd: DeckCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        DeckCmd::Get { json } => {
            let device = ask(service, OBSERVE, "device.get", json!({}), err)?;
            if json {
                print_json(out, &device)?;
            } else {
                write!(out, "{}", device_text(&device))?;
            }
            Ok(0)
        }
        DeckCmd::Set { key, value } => {
            let key = set_key(&key, value).map_err(Stop::Refused)?;
            ask(
                service,
                DEVICE,
                "device.set",
                json!({"k": key, "v": value}),
                err,
            )?;
            writeln!(out, "{} set to {value}", api_name(key))?;
            Ok(0)
        }
        DeckCmd::Faces { json } => {
            let device = ask(service, OBSERVE, "device.get", json!({}), err)?;
            if json {
                print_json(out, &device["faces"])?;
            } else {
                write!(out, "{}", faces_text(&device))?;
            }
            Ok(0)
        }
        DeckCmd::SetFaces { file } => {
            let list = strip(&read_json(&file)?).map_err(Stop::Refused)?;
            ask(
                service,
                DEVICE,
                "device.faces.set",
                json!({ "list": list }),
                err,
            )?;
            writeln!(
                out,
                "sent a strip of {} faces; the deck shows it once it takes it (`deckd device \
                 faces`)",
                list.len()
            )?;
            Ok(0)
        }
        DeckCmd::SetMacro {
            face,
            layout,
            src,
            file,
        } => {
            let keys = macro_keys(&read_json(&file)?).map_err(Stop::Refused)?;
            let params = json!({"f": face, "layout": layout, "src": src, "keys": keys});
            ask(service, DEVICE, "device.macro.set", params, err)?;
            let count = match keys.len() {
                1 => "1 key".to_string(),
                n => format!("{n} keys"),
            };
            writeln!(out, "sent {count} to face {face} ({layout})")?;
            Ok(0)
        }
    }
}

/// The settings `device.set` takes: the language, then the rest in the
/// order the deck is sent them (`SetKey::ALL`).
fn settings() -> impl Iterator<Item = SetKey> {
    std::iter::once(SetKey::Language).chain(SetKey::ALL)
}

/// A setting as `device.set` and `device.get` name it (`textsize`).
fn api_name(key: SetKey) -> String {
    serde_json::to_value(key)
        .ok()
        .and_then(|name| name.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// What a setting takes, as a refusal says it.
fn range(key: SetKey) -> &'static str {
    match key {
        SetKey::Language => "0 to 3 (English, 简体中文, 繁體中文, 日本語)",
        SetKey::Speaker | SetKey::Mic => "0 to 10",
        SetKey::Announce | SetKey::Chime | SetKey::Autojump | SetKey::Demo => "0 or 1",
        SetKey::Brightness => "1 to 5",
        SetKey::Idlehome => "any number of seconds",
        SetKey::Textsize | SetKey::Arcadesound => "0 to 2",
    }
}

/// `name` as a setting, and `value` in its range, or why not.
pub fn set_key(name: &str, value: u16) -> Result<SetKey, String> {
    let key = settings()
        .find(|key| api_name(*key) == name)
        .ok_or_else(|| {
            let names: Vec<String> = settings().map(api_name).collect();
            format!("{name} is not a deck setting: {}", names.join(", "))
        })?;
    if !key.accepts(value) {
        return Err(format!("{name} takes {}", range(key)));
    }
    Ok(key)
}

/// A strip from a file: a list of faces, or `device.faces.set`'s own
/// `{list}`, and only one the deck would take.
pub fn strip(document: &Value) -> Result<Vec<FaceEntry>, String> {
    let list = document.get("list").unwrap_or(document);
    let faces: Vec<FaceEntry> = serde_json::from_value(list.clone())
        .map_err(|e| format!("not a strip, a list of {{f, t, n}}: {e}"))?;
    match deck_proto::strip_problem(&faces) {
        Some(why) => Err(format!("the deck would not take this strip: {why}")),
        None => Ok(faces),
    }
}

/// A Macropad face's keys from a file: a list of keys, or
/// `device.macro.set`'s own `{keys}`.
pub fn macro_keys(document: &Value) -> Result<Vec<MacroKey>, String> {
    let keys = document.get("keys").unwrap_or(document);
    serde_json::from_value(keys.clone())
        .map_err(|e| format!("not a list of keys, each {{label, binding, keys}}: {e}"))
}

/// `device.get` as a person reads it: the link, then the deck while it is
/// linked (or when it last was), then its settings.
#[must_use]
pub fn device_text(device: &Value) -> String {
    let mut words = format!(
        "link: {}, session {}\n",
        text(device, "link"),
        text(device, "session")
    );
    let mut deck = Vec::new();
    if let Some(label) = device["deck"].as_str() {
        deck.push(format!("deck {label}"));
    }
    if let Some(transport) = device["transport"].as_str() {
        deck.push(format!("over {transport}"));
    }
    if let Some(board) = device["board"].as_str() {
        deck.push(board.to_string());
    }
    if let Some(fw) = device["fw"].as_str() {
        deck.push(format!("firmware {fw}"));
    }
    if let Some(battery) = device["battery"].as_u64() {
        deck.push(format!("battery {battery}%"));
    }
    match device["charging"].as_bool() {
        Some(true) => deck.push("charging".into()),
        Some(false) => deck.push("not charging".into()),
        None => {}
    }
    if !deck.is_empty() {
        words.push_str(&format!("{}\n", deck.join(", ")));
    }
    if device["fw"].is_null() {
        if let Some(at) = device["last"]["at"].as_str() {
            words.push_str(&format!("last linked {at}\n"));
        }
    }
    let reported = &device["settings"];
    let values: Vec<String> = settings()
        .map(api_name)
        .filter_map(|name| {
            reported[name.as_str()]
                .as_u64()
                .map(|value| format!("{name} {value}"))
        })
        .collect();
    if !values.is_empty() {
        words.push_str(&format!("settings: {}\n", values.join(", ")));
    }
    words
}

/// The strip a face to a line, `f`, template and name, the face the deck
/// shows marked.
#[must_use]
pub fn faces_text(device: &Value) -> String {
    let faces = device["faces"].as_array().cloned().unwrap_or_default();
    if faces.is_empty() {
        return "the deck has not reported its strip\n".into();
    }
    let shown = device["face"].as_u64();
    faces
        .iter()
        .map(|face| {
            let mark = if face["f"].as_u64() == shown && shown.is_some() {
                "  ← shown now"
            } else {
                ""
            };
            format!(
                "{} {:<8} {}{mark}\n",
                face["f"].as_u64().unwrap_or(0),
                text(face, "t"),
                text(face, "n")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{fixture, run, Script};
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<DeckCmd, clap::Error> {
        let mut argv = vec!["deckd", "device"];
        argv.extend_from_slice(args);
        crate::cli::Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            crate::cli::Cmd::Device {
                cmd: crate::cli::DeviceCmd::Deck(cmd),
            } => cmd,
            other => panic!("{other:?}"),
        })
    }

    /// The new verbs sit beside the debugging ones, which still parse.
    #[test]
    fn the_deck_verbs_parse_beside_the_debugging_ones() {
        for args in [
            &["get"][..],
            &["get", "--json"],
            &["set", "brightness", "3"],
            &["faces", "--json"],
            &["set-faces", "-"],
            &["set-macro", "--face", "3", "--layout", "Build", "keys.json"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        assert!(parse(&["set-macro", "--face", "9", "--layout", "Build", "k.json"]).is_err());
        assert!(parse(&["set", "brightness", "-1"]).is_err());
        assert!(matches!(
            crate::cli::Cli::try_parse_from(["deckd", "device", "sync"])
                .unwrap()
                .cmd,
            crate::cli::Cmd::Device {
                cmd: crate::cli::DeviceCmd::Sync
            }
        ));
    }

    /// A setting is named as `device.set` names it and refused outside the
    /// deck's range before anything is sent.
    #[test]
    fn a_setting_is_checked_against_the_decks_range_before_it_is_sent() {
        assert_eq!(set_key("brightness", 3), Ok(SetKey::Brightness));
        assert_eq!(set_key("textsize", 2), Ok(SetKey::Textsize));
        assert_eq!(set_key("idlehome", 600), Ok(SetKey::Idlehome));
        assert_eq!(
            set_key("brightness", 9),
            Err("brightness takes 1 to 5".into())
        );
        assert_eq!(set_key("announce", 2), Err("announce takes 0 or 1".into()));
        assert!(set_key("text", 1)
            .unwrap_err()
            .starts_with("text is not a deck setting: language, speaker"));
        let script = Script::new(Vec::new());
        let ran = run(|out, err| {
            super::run(
                &script,
                parse(&["set", "brightness", "9"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.result, Err("brightness takes 1 to 5".into()));
        assert!(script.methods().is_empty());
        let script = Script::new(vec![Ok(json!(12))]);
        let ran =
            run(|out, err| super::run(&script, parse(&["set", "speaker", "4"]).unwrap(), out, err));
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (Ok(0), "speaker set to 4\n")
        );
        let asked = script.call(0);
        assert_eq!(
            (asked.method.as_str(), asked.scopes),
            ("device.set", vec!["device".to_string()])
        );
        assert_eq!(asked.params, json!({"k": "speaker", "v": 4}));
    }

    /// A strip comes as a list or as `{list}`, and one the deck would not
    /// take is refused with its rule before anything is sent.
    #[test]
    fn a_strip_is_read_as_a_list_or_its_params_and_checked_by_the_decks_rules() {
        let faces = json!([
            {"f": 1, "t": "agents", "n": "Agents"},
            {"f": 3, "t": "macropad", "n": "Build"},
            {"f": 5, "t": "settings", "n": "Settings"},
        ]);
        assert_eq!(strip(&faces).unwrap().len(), 3);
        assert_eq!(strip(&json!({ "list": faces })).unwrap().len(), 3);
        assert_eq!(
            strip(&json!([{"f": 3, "t": "macropad", "n": "Build"}])),
            Err("the deck would not take this strip: agents first".into())
        );
        assert!(strip(&json!([{"f": 1}]))
            .unwrap_err()
            .starts_with("not a strip"));
        let dir = crate::integrations::testdir::TestDir::new("verbs-set-faces");
        let file = dir.write("strip.json", &faces.to_string());
        let script = Script::new(vec![Ok(json!({"rev": 3, "sent": true}))]);
        let cmd = parse(&["set-faces", file.to_str().unwrap()]).unwrap();
        let ran = run(|out, err| super::run(&script, cmd, out, err));
        assert_eq!(ran.result, Ok(0));
        assert_eq!(script.call(0).params, json!({ "list": faces }));
        let keys = json!([{"label": "Build", "binding": "left", "keys": "cmd+b"}]);
        assert_eq!(macro_keys(&json!({ "keys": keys })).unwrap().len(), 1);
        let file = dir.write("keys.json", &keys.to_string());
        let script = Script::new(vec![Ok(json!({}))]);
        let cmd = parse(&[
            "set-macro",
            "--face",
            "3",
            "--layout",
            "Build",
            file.to_str().unwrap(),
        ])
        .unwrap();
        let ran = run(|out, err| super::run(&script, cmd, out, err));
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (Ok(0), "sent 1 key to face 3 (Build)\n")
        );
        assert_eq!(
            script.call(0).params,
            json!({"f": 3, "layout": "Build", "src": null, "keys": keys})
        );
    }

    /// The deck as the Deck page shows it, from the recorded reply of a
    /// service with no deck and from a linked one.
    #[test]
    fn the_deck_reads_as_its_link_its_state_and_its_settings() {
        let recorded = device_text(&fixture("recorded/device.get.json"));
        assert!(
            recorded.starts_with("link: offline, session absent\n"),
            "{recorded}"
        );
        assert!(recorded.contains("\nsettings: language "), "{recorded}");
        let offline = json!({
            "link": "offline", "session": "absent", "fw": null, "faces": [],
            "settings": {"language": 0, "language_revision": 0, "speaker": 6, "mic": 6,
                         "announce": 1, "chime": 1, "autojump": 0, "brightness": 5,
                         "idlehome": 120, "demo": 1, "textsize": 1, "arcadesound": 0},
            "last": {"at": "2026-11-12T10:03:00.000Z"},
        });
        assert_eq!(
            device_text(&offline),
            "link: offline, session absent\nlast linked 2026-11-12T10:03:00.000Z\nsettings: \
             language 0, speaker 6, mic 6, announce 1, chime 1, autojump 0, brightness 5, \
             idlehome 120, demo 1, textsize 1, arcadesound 0\n"
        );
        let linked = json!({
            "link": "online", "session": "synced", "deck": "3f2a9c1d", "transport": "usb",
            "board": "Rev B", "fw": "0.13.0-openwalkietalkie", "battery": 87, "charging": true,
            "face": 3, "faces": [
                {"f": 1, "t": "agents", "n": "Agents"}, {"f": 3, "t": "macropad", "n": "Build"},
            ],
            "settings": {"speaker": 4},
        });
        assert_eq!(
            device_text(&linked),
            "link: online, session synced\ndeck 3f2a9c1d, over usb, Rev B, firmware \
             0.13.0-openwalkietalkie, battery 87%, charging\nsettings: speaker 4\n"
        );
        assert_eq!(
            faces_text(&linked),
            "1 agents   Agents\n3 macropad Build  ← shown now\n"
        );
        assert_eq!(
            faces_text(&fixture("recorded/device.get.json")),
            "the deck has not reported its strip\n"
        );
    }
}
