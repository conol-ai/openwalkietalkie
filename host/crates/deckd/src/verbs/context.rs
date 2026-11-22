//! `deckd context status|key set|key clear`: context-aware dictation from
//! the command line.
//!
//! The settings — the switch, and each model's endpoint and name — are
//! managed keys of `config.toml`, read and written like every other one:
//! `deckd config get context.enabled`, `deckd config set
//! context.enabled=true context.scene.endpoint=https://…`. This verb adds
//! what `config.set` must not carry, the two API keys: read without echo
//! (or from stdin when piped), never from the command line, and written
//! straight into this install's key store (`crate::context::keys`), so no
//! key crosses the socket. What is ever printed of a key is its hint: the
//! length and at most its last four characters.
use super::{finish, print_json, Service, Stop, ADMIN};
use crate::config::Config;
use crate::context::keys::{place, recorded, ContextKeys, Recorded, Role};
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

/// `scene` or `edit`, as the command line names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Which {
    /// The model that names the scene from the focused app and its title.
    Scene,
    /// The model that rewrites the dictated text.
    Edit,
}

impl From<Which> for Role {
    fn from(which: Which) -> Role {
        match which {
            Which::Scene => Role::Scene,
            Which::Edit => Role::Edit,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum ContextCmd {
    /// Whether it is on, each model's endpoint and name, and each key's hint
    /// and where it is kept. Asks the running service when there is one,
    /// else reads config.toml. Never reads a key.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// The two models' API keys.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum KeyCmd {
    /// Store a model's key: in the Keychain on a Mac, Credential Manager on
    /// Windows, the Secret Service keyring on Linux, or where none is
    /// available `context-keys.toml` in the data directory (mode 600). It
    /// is read from the terminal without echo, or from stdin when piped.
    Set { which: Which },
    /// Remove a model's key from wherever it is kept.
    Clear { which: Which },
}

/// `deckd context …`. `data` is the data directory; `keys` opens the key
/// store, and is only called by the two commands that write a key (on Linux
/// it asks the Secret Service whether it answers).
pub fn run(
    service: &dyn Service,
    data: &Path,
    keys: &dyn Fn() -> ContextKeys,
    cmd: ContextCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(context(service, data, keys, cmd, out, err))
}

fn context(
    service: &dyn Service,
    data: &Path,
    keys: &dyn Fn() -> ContextKeys,
    cmd: ContextCmd,
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        ContextCmd::Status { json } => {
            let status = status(service, data);
            if json {
                print_json(out, &status)?;
            } else {
                write!(out, "{}", status_text(&status, data))?;
            }
            Ok(0)
        }
        ContextCmd::Key {
            cmd: KeyCmd::Set { which },
        } => {
            let role = Role::from(which);
            on_its_provider(data, role)?;
            let key = service
                .read_key(&format!("{} model API key: ", role.name()))
                .map_err(Stop::Refused)?;
            let keys = keys();
            let Recorded { store, hint } = keys.set(role, &key).map_err(|why| {
                Stop::Refused(format!("{} did not take the key: {why}", keys.place()))
            })?;
            writeln!(
                out,
                "context: {} key {hint} stored in {}",
                role.name(),
                place(&store, data)
            )?;
            Ok(0)
        }
        ContextCmd::Key {
            cmd: KeyCmd::Clear { which },
        } => {
            let role = Role::from(which);
            on_its_provider(data, role)?;
            let keys = keys();
            let removed = keys.clear(role).map_err(|why| {
                Stop::Refused(format!("{} did not remove the key: {why}", keys.place()))
            })?;
            if removed {
                writeln!(
                    out,
                    "context: {} key removed from {}",
                    role.name(),
                    keys.place()
                )?;
            } else {
                writeln!(out, "context: there was no {} key", role.name())?;
            }
            Ok(0)
        }
    }
}

/// `{enabled, scene: {endpoint, model, key, key_store}, edit: {…}, source}`:
/// the settings from the running service's `config.get` when it answers
/// (`source: "service"`), else from config.toml (`source: "config.toml"`);
/// the keys' hints from `context-keys.toml` either way.
#[must_use]
pub fn status(service: &dyn Service, data: &Path) -> Value {
    let config = Config::load(&data.join("config.toml")).unwrap_or_default();
    let (enabled, source) = match service.call(ADMIN, "config.get", json!({})) {
        Ok(published) if published.get("context").is_some() => (published["context"]["enabled"].clone(), "service"),
        _ => (json!(config.context.enabled), "config.toml"),
    };
    // Context-aware dictation is experimental: what that rests on is
    // docs/host-service.md's "Experimental" section.
    let mut status = json!({ "enabled": enabled, "experimental": true, "source": source });
    for role in Role::ALL {
        let name = role.name();
        let model = match role {
            Role::Scene => &config.context.scene,
            Role::Edit => &config.context.edit,
        };
        let (resolves, key_of) = match config.context_target(role) {
            Ok(target) => (
                format!("{} at {}", target.kind.name(), target.base_url),
                Some(target.key_of),
            ),
            Err(crate::providers::Unresolved::NotSetUp) => ("not set up".into(), None),
            Err(why) => (format!("does not resolve: {why:?}"), None),
        };
        let key = key_of
            .as_deref()
            .and_then(|key_of| match key_of.strip_prefix("legacy:") {
                Some(_) => recorded(data, role),
                None => crate::providers::keys::recorded(data, key_of),
            })
            .unwrap_or_default();
        status[name] = json!({
            "provider": model.provider,
            "model": model.model,
            "resolves": resolves,
            "key": key.hint,
            "key_store": key.store,
        });
    }
    status
}

/// A role that names a provider has that provider's key, set with `deckd
/// providers set-key`: the role's own key would be read by nothing.
fn on_its_provider(data: &Path, role: Role) -> Result<(), Stop> {
    let config = Config::load(&data.join("config.toml")).unwrap_or_default();
    let provider = match role {
        Role::Scene => config.context.scene.provider,
        Role::Edit => config.context.edit.provider,
    };
    match provider.trim() {
        "" => Ok(()),
        id => Err(Stop::Refused(format!(
            "context.{} uses provider {id}, whose key is shared: `deckd providers set-key {id}`",
            role.name()
        ))),
    }
}

/// [`status`] for a person.
#[must_use]
pub fn status_text(status: &Value, data: &Path) -> String {
    let on = status["enabled"].as_bool().unwrap_or(false);
    let mut words = format!(
        "context-aware dictation: {}{}\nexperimental: true\n",
        if on { "on" } else { "off" },
        if on { "" } else { " (`deckd config set context.enabled=true` turns it on)" }
    );
    for role in Role::ALL {
        let row = &status[role.name()];
        let text = |key: &str| -> String {
            match row[key].as_str() {
                Some(text) if !text.is_empty() => text.to_string(),
                _ => "(unset)".into(),
            }
        };
        let key = match row["key"].as_str() {
            Some(hint) if !hint.is_empty() => format!(
                "{hint}, in {}",
                place(row["key_store"].as_str().unwrap_or("file"), data)
            ),
            _ => "(none)".into(),
        };
        words.push_str(&format!(
            "{:<6} provider {}\n       model    {}\n       uses     {}\n       key      {key}\n",
            format!("{}:", role.name()),
            text("provider"),
            text("model"),
            text("resolves"),
        ));
    }
    if status["source"] == "config.toml" {
        words.push_str("(the service did not answer: read from config.toml)\n");
    }
    words
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{run, Script};
    use super::*;
    use crate::integrations::testdir::TestDir;
    use clap::Parser;

    const KEY: &str = "sk-context-test-0123456789-LAST";

    fn parse(args: &[&str]) -> Result<ContextCmd, clap::Error> {
        let mut argv = vec!["deckd", "context"];
        argv.extend_from_slice(args);
        crate::cli::Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            crate::cli::Cmd::Context { cmd } => cmd,
            other => panic!("{other:?}"),
        })
    }

    #[test]
    fn the_context_verbs_parse() {
        for args in [
            &["status"][..],
            &["status", "--json"],
            &["key", "set", "scene"],
            &["key", "set", "edit"],
            &["key", "clear", "edit"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        // A key is never an argument.
        assert!(parse(&["key", "set", "scene", KEY]).is_err());
        assert!(parse(&["key", "set", "tts"]).is_err());
        assert!(parse(&["key", "set"]).is_err());
    }

    /// Set and clear through the file fallback: the key is read the way a
    /// key is typed, never sent to the service, and what is printed is its
    /// hint.
    #[test]
    fn a_key_is_stored_and_cleared_and_only_its_hint_is_printed() {
        let dir = TestDir::new("context-verb-keys");
        let data = dir.path().to_path_buf();
        let keys = || ContextKeys::file_only(&data);
        let script = Script::new(Vec::new());
        *script.key.borrow_mut() = Some(KEY.into());
        let ran = run(|out, err| {
            super::run(
                &script,
                &data,
                &keys,
                parse(&["key", "set", "scene"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.result, Ok(0));
        assert!(
            !ran.out.contains(KEY) && !ran.err.contains(KEY),
            "{}{}",
            ran.out,
            ran.err
        );
        assert!(
            ran.out
                .starts_with("context: scene key …LAST (31 chars) stored in "),
            "{}",
            ran.out
        );
        assert!(
            script.methods().is_empty(),
            "the key never reached the socket"
        );
        assert_eq!(keys().get(Role::Scene).unwrap().as_deref(), Some(KEY));

        let ran = run(|out, err| {
            super::run(
                &script,
                &data,
                &keys,
                parse(&["key", "clear", "scene"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.result, Ok(0));
        assert!(
            ran.out.starts_with("context: scene key removed from "),
            "{}",
            ran.out
        );
        assert_eq!(keys().get(Role::Scene).unwrap(), None);
        let ran = run(|out, err| {
            super::run(
                &script,
                &data,
                &keys,
                parse(&["key", "clear", "scene"]).unwrap(),
                out,
                err,
            )
        });
        assert_eq!(ran.out, "context: there was no scene key\n");
    }

    /// Status from the service's `config.get`, the keys' hints from the
    /// file, and never a key.
    #[test]
    fn status_shows_the_settings_and_the_hints_never_a_key() {
        let dir = TestDir::new("context-verb-status");
        let data = dir.path().to_path_buf();
        std::fs::write(
            data.join("config.toml"),
            "[providers.openrouter]\nkind = \"openrouter\"\n[context]\nenabled = true\n[context.edit]\nprovider = \"openrouter\"\nmodel = \"large\"\n",
        )
        .unwrap();
        crate::providers::keys::ProviderKeys::with(crate::context::keys::Vault::with(
            &data.join(crate::providers::keys::FILE),
            None,
        ))
        .set("openrouter", KEY)
        .unwrap();
        let published = json!({"context": {"enabled": true}});
        let script = Script::new(vec![Ok(published.clone()), Ok(published)]);
        let keys = || ContextKeys::file_only(&data);
        let ran = run(|out, err| {
            super::run(&script, &data, &keys, parse(&["status", "--json"]).unwrap(), out, err)
        });
        let status: Value = serde_json::from_str(&ran.out).unwrap();
        assert_eq!(status["enabled"], true);
        assert_eq!(status["source"], "service");
        assert_eq!(status["edit"]["provider"], "openrouter");
        assert_eq!(status["edit"]["model"], "large");
        assert_eq!(status["edit"]["resolves"], "OpenRouter at https://openrouter.ai/api/v1");
        assert_eq!(status["edit"]["key"], "\u{2026}LAST (31 chars)");
        assert_eq!(status["scene"]["resolves"], "not set up");
        assert_eq!(status["scene"]["key"], "");
        let ran = run(|out, err| super::run(&script, &data, &keys, parse(&["status"]).unwrap(), out, err));
        assert!(ran.out.starts_with("context-aware dictation: on\nexperimental: true\n"), "{}", ran.out);
        assert_eq!(status["experimental"], true);
        assert!(ran.out.contains("edit:  provider openrouter\n       model    large\n       uses     OpenRouter at https://openrouter.ai/api/v1\n"), "{}", ran.out);
        assert!(ran.out.contains("\u{2026}LAST (31 chars), in "), "{}", ran.out);
        assert!(!ran.out.contains(KEY), "{}", ran.out);
        assert_eq!(script.call(0).scopes, ["admin"]);
        // The role's own key is no longer the one to set.
        let mut set = Script::new(Vec::new());
        *set.key.get_mut() = Some(KEY.into());
        let ran = run(|out, err| super::run(&set, &data, &keys, parse(&["key", "set", "edit"]).unwrap(), out, err));
        assert_eq!(ran.result, Err("context.edit uses provider openrouter, whose key is shared: `deckd providers set-key openrouter`".into()));
    }

    #[test]
    fn status_without_a_service_reads_the_file_and_no_table_is_off() {
        let dir = TestDir::new("context-verb-offline");
        let data = dir.path().to_path_buf();
        std::fs::write(data.join("config.toml"), "[api]\nport = 47201\n").unwrap();
        let script = Script::new(vec![Err(crate::cli::CtlFailure::Unreachable("no socket".into()))]);
        let keys = || ContextKeys::file_only(&data);
        let ran = run(|out, err| super::run(&script, &data, &keys, parse(&["status"]).unwrap(), out, err));
        assert_eq!(ran.result, Ok(0));
        assert!(ran.out.starts_with("context-aware dictation: off (`deckd config set context.enabled=true` turns it on)\nexperimental: true\n"), "{}", ran.out);
        assert!(ran.out.ends_with("(the service did not answer: read from config.toml)\n"), "{}", ran.out);
    }
}
