//! `deckd config get|set|reload`: the settings the service manages, as
//! `config.get` publishes them; changed together or not at all
//! (`config.set`); and `config.toml` read again (`config.reload`).
//!
//! A key is the dotted name `config.get` gives it (`voice.stt.locale`,
//! `usage.providers`, `api.tcp`). The service's own list of managed keys
//! decides what may be written and refuses the rest. The one key refused
//! here is the cloud voice's key: on a command line it would stay in the
//! shell's history and show in `ps`, and `deckd tts key set` reads it
//! without echo.
use super::{ask, finish, print_json, Service, Stop, ADMIN};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::Write;

/// Why `deckd config set` does not take the cloud voice's key.
pub const KEY_REFUSED: &str =
    "voice.tts.api_key is set with `deckd tts key set`, which reads it without echo";

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// The settings the service manages (`config.get`), or one dotted key's
    /// value.
    Get {
        /// A dotted key, `voice.stt.locale`.
        key: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change settings together, all or none (`config.set`): each
    /// `KEY=VALUE`, VALUE read as JSON when it is JSON (`true`, `47202`,
    /// `["codex"]`, `null`) and as a string otherwise; `--unset KEY` puts a
    /// key back to its default.
    Set {
        #[arg(value_name = "KEY=VALUE")]
        pairs: Vec<String>,
        #[arg(long, value_name = "KEY")]
        unset: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read config.toml again now (`config.reload`), as the service does by
    /// itself a moment after the file is saved.
    Reload {
        #[arg(long)]
        json: bool,
    },
}

/// `deckd config …`.
pub fn run(
    service: &dyn Service,
    cmd: ConfigCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(config(service, cmd, out, err))
}

fn config(
    service: &dyn Service,
    cmd: ConfigCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        ConfigCmd::Get { key: None, json } => {
            let config = ask(service, ADMIN, "config.get", json!({}), err)?;
            if json {
                print_json(out, &config)?;
            } else {
                write!(out, "{}", config_text(&config))?;
            }
            Ok(0)
        }
        ConfigCmd::Get {
            key: Some(key),
            json,
        } => {
            let config = ask(service, ADMIN, "config.get", json!({}), err)?;
            let value = value_of(&config, &key).ok_or_else(|| {
                Stop::Refused(format!(
                    "{key} is not a key config.get publishes (`deckd config get` lists them)"
                ))
            })?;
            if json {
                print_json(out, value)?;
            } else {
                writeln!(out, "{}", value_text(value))?;
            }
            Ok(0)
        }
        ConfigCmd::Set { pairs, unset, json } => {
            let patch = patch(&pairs, &unset).map_err(Stop::Refused)?;
            let reply = ask(service, ADMIN, "config.set", patch, err)?;
            changes(out, &reply, json)
        }
        ConfigCmd::Reload { json } => {
            let reply = ask(service, ADMIN, "config.reload", json!({}), err)?;
            changes(out, &reply, json)
        }
    }
}

fn changes(out: &mut dyn Write, reply: &Value, json: bool) -> Result<i32, Stop> {
    if json {
        print_json(out, reply)?;
    } else {
        write!(out, "{}", changes_text(reply))?;
    }
    Ok(0)
}

/// `config.set`'s `{patch}` from `KEY=VALUE` arguments and `--unset` keys.
/// A key named twice, an argument with no `=`, the cloud voice's key and
/// nothing at all are refused.
pub fn patch(pairs: &[String], unset: &[String]) -> Result<Value, String> {
    let mut patch = serde_json::Map::new();
    let mut put = |key: &str, value: Value, given: &str| -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{given} is not KEY=VALUE"));
        }
        if key == crate::secrets::ACCOUNT {
            return Err(KEY_REFUSED.into());
        }
        if patch.insert(key.to_string(), value).is_some() {
            return Err(format!("{key} is given twice"));
        }
        Ok(())
    };
    for pair in pairs {
        let (key, raw) = pair
            .split_once('=')
            .ok_or_else(|| format!("{pair} is not KEY=VALUE"))?;
        let value = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
        put(key, value, pair)?;
    }
    for key in unset {
        put(key, Value::Null, key)?;
    }
    if patch.is_empty() {
        return Err("nothing to set: give KEY=VALUE or --unset KEY".into());
    }
    Ok(json!({ "patch": patch }))
}

/// One dotted key of `config.get`'s reply: the table before the first dot,
/// the leaf after it, as `config.get` spells nested tables out flat
/// (`voice` carries `stt.locale`).
#[must_use]
pub fn value_of<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    let (table, leaf) = key.split_once('.')?;
    config.get(table)?.get(leaf)
}

/// A value as `deckd config get KEY` prints it: a string as it is, anything
/// else as JSON.
#[must_use]
pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `config.get` a key to a line, `table.leaf = <JSON>`.
#[must_use]
pub fn config_text(config: &Value) -> String {
    let mut words = String::new();
    for (table, leaves) in config.as_object().into_iter().flatten() {
        match leaves.as_object() {
            Some(leaves) => {
                for (leaf, value) in leaves {
                    words.push_str(&format!("{table}.{leaf} = {value}\n"));
                }
            }
            None => words.push_str(&format!("{table} = {leaves}\n")),
        }
    }
    words
}

/// What `config.set` and `config.reload` changed: the keys, those in force
/// at once, and those that wait for a restart.
#[must_use]
pub fn changes_text(reply: &Value) -> String {
    let list = |key: &str| -> Vec<&str> {
        reply[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    };
    let changed = list("changed");
    if changed.is_empty() {
        return "nothing changed\n".into();
    }
    let mut words = format!("changed: {}\n", changed.join(", "));
    let applied = list("applied");
    if !applied.is_empty() {
        words.push_str(&format!("in force now: {}\n", applied.join(", ")));
    }
    let restart = list("restart_needed");
    if !restart.is_empty() {
        words.push_str(&format!(
            "in force after a restart: {} (`deckd service restart`)\n",
            restart.join(", ")
        ));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{fixture, run, Script};
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<ConfigCmd, clap::Error> {
        let mut argv = vec!["deckd", "config"];
        argv.extend_from_slice(args);
        crate::cli::Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            crate::cli::Cmd::Config { cmd } => cmd,
            other => panic!("{other:?}"),
        })
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_config_verbs_parse() {
        for args in [
            &["get"][..],
            &["get", "voice.stt.locale", "--json"],
            &["set", "api.tcp=true", "--unset", "voice.stt.model"],
            &["reload"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        assert!(parse(&["remove", "x"]).is_err());
    }

    /// A value is JSON when it is JSON and a string otherwise; `--unset`
    /// sends `null`. Refused before the service is asked: no `=`, a key
    /// given twice, nothing at all, and the cloud voice's key.
    #[test]
    fn a_patch_reads_each_value_as_json_or_as_a_string() {
        assert_eq!(
            patch(
                &strings(&[
                    "sources.paused=true",
                    "api.port=47202",
                    "usage.providers=[\"codex\"]",
                    "voice.stt.locale=en-GB",
                    "voice.stt.model=\"1\"",
                    "voice.tts.instruct=",
                ]),
                &strings(&["voice.stt.engine"]),
            ),
            Ok(json!({"patch": {
                "sources.paused": true,
                "api.port": 47202,
                "usage.providers": ["codex"],
                "voice.stt.locale": "en-GB",
                "voice.stt.model": "1",
                "voice.tts.instruct": "",
                "voice.stt.engine": null,
            }}))
        );
        assert_eq!(
            patch(&strings(&["api.port"]), &[]),
            Err("api.port is not KEY=VALUE".into())
        );
        assert_eq!(
            patch(&strings(&["=1"]), &[]),
            Err("=1 is not KEY=VALUE".into())
        );
        assert_eq!(
            patch(&strings(&["api.tcp=true"]), &strings(&["api.tcp"])),
            Err("api.tcp is given twice".into())
        );
        assert_eq!(
            patch(&[], &[]),
            Err("nothing to set: give KEY=VALUE or --unset KEY".into())
        );
        for (pairs, unset) in [
            (strings(&["voice.tts.api_key=sk-1"]), vec![]),
            (vec![], strings(&["voice.tts.api_key"])),
        ] {
            assert_eq!(patch(&pairs, &unset), Err(KEY_REFUSED.into()));
        }
        let script = Script::new(Vec::new());
        let ran = run(|out, err| {
            super::run(
                &script,
                parse(&["set", "voice.tts.api_key=sk-1"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.result, Err(KEY_REFUSED.into()));
        assert!(
            script.methods().is_empty(),
            "the key never left this process"
        );
    }

    /// One key's value, or every key a line, as `config.get` publishes them.
    #[test]
    fn a_key_is_read_by_its_dotted_name() {
        let config = fixture("config_get_sources.json");
        assert_eq!(value_of(&config, "voice.stt.locale"), Some(&json!("en-GB")));
        assert_eq!(
            value_of(&config, "usage.providers"),
            Some(&json!(["codex"]))
        );
        assert_eq!(value_of(&config, "hooks.claude.hold_s"), Some(&json!(120)));
        assert_eq!(value_of(&config, "voice"), None);
        assert_eq!(value_of(&config, "voice.nope"), None);
        assert_eq!(value_text(&json!("en-GB")), "en-GB");
        assert_eq!(value_text(&json!(["codex"])), "[\"codex\"]");
        let words = config_text(&config);
        assert!(words.contains("api.tcp = true\n"), "{words}");
        assert!(words.contains("voice.stt.locale = \"en-GB\"\n"), "{words}");
        assert!(words.contains("hooks.claude.hold_s = 120\n"), "{words}");
        let script = Script::new(vec![Ok(config.clone()), Ok(config)]);
        let get =
            |args: &[&str]| run(|out, err| super::run(&script, parse(args).unwrap(), out, err));
        let ran = get(&["get", "voice.stt.locale"]);
        assert_eq!((ran.result, ran.out.as_str()), (Ok(0), "en-GB\n"));
        let ran = get(&["get", "voice.stt.nope"]);
        assert_eq!(
            ran.result,
            Err(
                "voice.stt.nope is not a key config.get publishes (`deckd config get` lists them)"
                    .into()
            )
        );
        assert_eq!(script.call(0).scopes, ["admin"]);
    }

    /// What a write changed, as the service answered it.
    #[test]
    fn a_write_says_what_changed_and_what_waits_for_a_restart() {
        assert_eq!(
            changes_text(&fixture("config_set.json")),
            "changed: hooks.enabled\nin force now: hooks.enabled\n"
        );
        assert_eq!(
            changes_text(
                &json!({"changed": ["api.port"], "applied": [], "restart_needed": ["api.port"]})
            ),
            "changed: api.port\nin force after a restart: api.port (`deckd service restart`)\n"
        );
        assert_eq!(
            changes_text(&json!({"changed": [], "applied": [], "restart_needed": []})),
            "nothing changed\n"
        );
        let script = Script::new(vec![
            Ok(fixture("config_set.json")),
            Ok(json!({"changed": []})),
        ]);
        let ran = run(|out, err| {
            super::run(
                &script,
                parse(&["set", "hooks.enabled=true"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.result, Ok(0));
        assert_eq!(
            script.call(0).params,
            json!({"patch": {"hooks.enabled": true}})
        );
        let ran = run(|out, err| super::run(&script, parse(&["reload"]).unwrap(), out, err));
        assert_eq!((ran.result, ran.out.as_str()), (Ok(0), "nothing changed\n"));
        assert_eq!(script.methods(), ["config.set", "config.reload"]);
    }
}
