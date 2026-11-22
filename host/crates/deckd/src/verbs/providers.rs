//! `deckd providers list|add|remove|set-key|clear-key|models|refresh|test|migrate`:
//! the provider registry from the command line.
//!
//! The reads, checks and keys go through the same code the API answers
//! with (`crate::providers::api::Call`), straight against this install's
//! files and key store, so no key crosses the socket and none is printed:
//! what is shown of a key is its hint. `add` and `remove` edit
//! `config.toml`'s `[providers]` table the way `config.set` edits a file
//! (`config::edit`: every other byte kept, the result validated whole), and
//! the running service re-reads it by itself.
use crate::config::{Config, Paths, ProviderEntry};
use crate::providers::api::Call;
use crate::providers::keys::ProviderKeys;
use crate::providers::{id_ok, Kind, Provider};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::Write;

#[derive(Subcommand, Debug)]
pub enum ProvidersCmd {
    /// Every provider: its kind, where it is reached, and its key's hint.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Set one up: `deckd providers add openrouter --kind openrouter`, or a
    /// server of your own: `--kind custom --base-url http://host:11434/v1`.
    Add {
        id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Take one out, and its key with it. Refused while a feature uses it,
    /// unless `--force`.
    Remove {
        id: String,
        #[arg(long)]
        force: bool,
    },
    /// Store its key: read without echo, or from stdin when piped.
    SetKey { id: String },
    /// Remove its key.
    ClearKey { id: String },
    /// Its models: the built-in list, and its own once fetched.
    Models {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Fetch its own model list with its key, and cache it.
    Refresh { id: String },
    /// Check it, cheaply and with no dictated text: ok, no_key, bad_key,
    /// unreachable, wrong_model or failed.
    Test {
        id: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Move Milestone 1's context-aware dictation endpoints and keys into
    /// the registry now (the service does it when it starts).
    Migrate {
        #[arg(long)]
        dry_run: bool,
    },
}

/// `deckd providers …`. `read_key` reads a key the way `deckd tts key set`
/// does.
pub fn run(
    paths: &Paths,
    keys: &dyn Fn() -> ProviderKeys,
    read_key: &dyn Fn(&str) -> Result<String, String>,
    cmd: ProvidersCmd,
    out: &mut dyn Write,
) -> Result<i32, String> {
    let config = || Config::load(&paths.config).map_err(|e| e.to_string());
    let curl = crate::grokbot::webhook::curl();
    let call = |verb: &str, params: Value| -> Result<Value, String> {
        let keys = keys();
        Call::parse(verb, &params)
            .and_then(|call| call.answer(paths, &config().map_err(|e| deck_model::ApiError::internal(&e))?, &keys, &curl))
            .map_err(|e| e.data.get("why").and_then(Value::as_str).map_or_else(|| e.to_string(), String::from))
    };
    let w = |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(|e| e.to_string());
    match cmd {
        ProvidersCmd::List { json } => {
            let listed = call("list", json!({}))?;
            if json {
                w(out, serde_json::to_string_pretty(&listed).unwrap_or_default())?;
                return Ok(0);
            }
            let providers = listed["providers"].as_array().cloned().unwrap_or_default();
            if providers.is_empty() {
                w(out, "no providers yet: `deckd providers add <id> --kind openrouter`".into())?;
            }
            for p in providers {
                let key = match p["key"].as_str() {
                    Some(hint) if !hint.is_empty() => format!("key {hint} in {}", p["key_store"].as_str().unwrap_or("")),
                    _ if p["needs_key"] == true => "no key".into(),
                    _ => "no key needed".into(),
                };
                match p.get("problem").and_then(Value::as_str) {
                    Some(problem) => w(out, format!("{:<14} {problem}", p["id"].as_str().unwrap_or("")))?,
                    None => w(
                        out,
                        format!(
                            "{:<14} {:<11} {}  ({key})",
                            p["id"].as_str().unwrap_or(""),
                            p["kind"].as_str().unwrap_or(""),
                            p["base_url"].as_str().unwrap_or(""),
                        ),
                    )?,
                }
            }
            Ok(0)
        }
        ProvidersCmd::Add { id, kind, label, base_url } => {
            if !id_ok(&id) {
                return Err(format!("{id:?} cannot be an id: 1 to 32 lowercase letters, digits, - and _, starting with a letter"));
            }
            let entry = ProviderEntry {
                kind: kind.trim().to_ascii_lowercase(),
                label: label.filter(|l| !l.trim().is_empty()),
                base_url: base_url.unwrap_or_default().trim().trim_end_matches('/').to_string(),
                timeout_ms: None,
            };
            if id == crate::providers::omarchy::ID || entry.kind == "omarchy" {
                return Err("Omarchy's local models are found by themselves (`deckd providers list`): nothing to add".into());
            }
            let provider = Provider::from_entry(&id, &entry).map_err(|why| why.to_string())?;
            if provider.kind != Kind::Custom && !entry.base_url.is_empty() && provider.kind != Kind::Ollama {
                return Err(format!("a {} provider's address is built in: leave --base-url out", provider.kind.name()));
            }
            write_entry(paths, &id, Some(&entry))?;
            w(out, format!("providers: {id} ({}) at {}", provider.kind.name(), provider.base_url))?;
            if provider.kind.needs_key() {
                w(out, format!("providers: now its key: `deckd providers set-key {id}`"))?;
            }
            Ok(0)
        }
        ProvidersCmd::Remove { id, force } => {
            let config = config()?;
            if !config.providers.contains_key(&id) {
                return Err(format!("there is no provider {id:?}"));
            }
            let users = users_of(&config, &id);
            if !users.is_empty() && !force {
                return Err(format!("{} uses {id}: change that first, or --force", users.join(" and ")));
            }
            write_entry(paths, &id, None)?;
            let removed = keys().clear(&id).map_err(|e| e.to_string())?;
            w(out, format!("providers: {id} removed{}", if removed { ", and its key" } else { "" }))?;
            Ok(0)
        }
        ProvidersCmd::SetKey { id } => {
            config()?.provider(&id).map_err(|why| format!("{why:?}"))?;
            let key = read_key(&format!("{id} API key: "))?;
            let set = call("key.set", json!({"provider": id, "key": key}))?;
            w(out, format!("providers: {id} key {} stored in {}", set["key"].as_str().unwrap_or(""), keys().place()))?;
            Ok(0)
        }
        ProvidersCmd::ClearKey { id } => {
            let cleared = call("key.clear", json!({"provider": id}))?;
            w(out, format!("providers: {id} key {}", if cleared["removed"] == true { "removed" } else { "was not set" }))?;
            Ok(0)
        }
        ProvidersCmd::Models { id, json } => {
            let models = call("models", json!({"provider": id}))?;
            print_models(out, &models, json)
        }
        ProvidersCmd::Refresh { id } => {
            let models = call("refresh", json!({"provider": id}))?;
            match models["refresh"]["ok"].as_bool() {
                Some(true) => w(out, format!("providers: {id}: {} models fetched", models["refresh"]["count"]))?,
                _ => w(out, format!("providers: {id}: the fetch failed ({}); the list is what it was", models["refresh"]["why"].as_str().unwrap_or("?")))?,
            }
            print_models(out, &models, false)
        }
        ProvidersCmd::Test { id, model } => {
            let result = call("test", json!({"provider": id, "model": model}))?;
            let word = result["result"].as_str().unwrap_or("failed").to_string();
            w(out, format!("providers: {id}: {word}"))?;
            Ok(i32::from(word != "ok"))
        }
        ProvidersCmd::Migrate { dry_run } => {
            let old = crate::context::keys::ContextKeys::home(paths);
            let (report, _) = crate::providers::migrate::migrate(paths, &old, &keys(), dry_run)?;
            if report.moves.is_empty() && report.left.is_empty() {
                w(out, "providers: nothing to move".into())?;
            }
            for m in &report.moves {
                w(out, format!(
                    "{} context.{} → provider {}{}",
                    if dry_run { "would move" } else { "moved" },
                    m.role.name(),
                    m.provider,
                    if m.added.is_some() { " (new)" } else { "" }
                ))?;
            }
            for (role, id) in &report.keys_carried {
                w(out, format!("{} {}'s key → {id}'s", if dry_run { "would carry" } else { "carried" }, role.name()))?;
            }
            for role in &report.old_keys_removed {
                w(out, format!("removed the old {} key item", role.name()))?;
            }
            for (role, why) in &report.left {
                w(out, format!("left context.{} as it was: {why}", role.name()))?;
            }
            if report.voice_key_in_file {
                w(out, "the cloud voice's key is still in config.toml: the service moves it into its provider's item when it starts".into())?;
            }
            Ok(0)
        }
    }
}

fn print_models(out: &mut dyn Write, models: &Value, json: bool) -> Result<i32, String> {
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(models).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let source = match (models["source"].as_str(), models["updated_at_ms"].as_u64()) {
        (Some("updated"), Some(at)) => format!(
            "built-in + fetched {}",
            i64::try_from(at)
                .ok()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map_or_else(|| "?".into(), |at| at.format("%Y-%m-%d %H:%M UTC").to_string())
        ),
        _ => "built-in".into(),
    };
    writeln!(out, "{} ({source}):", models["provider"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
    for model in models["models"].as_array().into_iter().flatten() {
        writeln!(out, "  {}", model.as_str().unwrap_or("")).map_err(|e| e.to_string())?;
    }
    Ok(0)
}

/// The features that name provider `id`.
#[must_use]
pub fn users_of(config: &Config, id: &str) -> Vec<String> {
    let mut users = Vec::new();
    if config.context.scene.provider == id {
        users.push("context.scene".to_string());
    }
    if config.context.edit.provider == id {
        users.push("context.edit".to_string());
    }
    if config.voice.tts.provider == id && config.providers.contains_key(id) {
        users.push("voice.tts".to_string());
    }
    users
}

/// Write `[providers.<id>]` (or take it out, `None`) into `config.toml`,
/// every other byte kept, the result validated whole.
pub fn write_entry(paths: &Paths, id: &str, entry: Option<&ProviderEntry>) -> Result<(), String> {
    let text = match std::fs::read_to_string(&paths.config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", paths.config.display())),
    };
    let key = format!("providers.{id}");
    let mut edited = crate::config::edit(&text, &key, &Value::Null).map_err(|e| e.to_string())?;
    if let Some(entry) = entry {
        edited = crate::config::edit(&edited, &format!("{key}.kind"), &entry.kind.clone().into()).map_err(|e| e.to_string())?;
        if let Some(label) = &entry.label {
            edited = crate::config::edit(&edited, &format!("{key}.label"), &label.clone().into()).map_err(|e| e.to_string())?;
        }
        if !entry.base_url.is_empty() {
            edited = crate::config::edit(&edited, &format!("{key}.base_url"), &entry.base_url.clone().into()).map_err(|e| e.to_string())?;
        }
    }
    Config::parse(&edited).map_err(|e| format!("the result would not load: {e}"))?;
    crate::config::write(&paths.config, &edited).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::{MemoryStore, Vault};
    use crate::integrations::testdir::TestDir;
    use std::sync::Arc;

    const KEY: &str = "sk-cli-0123456789-abcdef-WXYZ";

    fn rig(name: &str) -> (TestDir, Paths, Arc<MemoryStore>) {
        let dir = TestDir::new(name);
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        std::fs::write(&paths.config, "# mine\n[api]\nport = 47201\n").unwrap();
        (dir, paths, Arc::new(MemoryStore::default()))
    }

    #[test]
    fn add_set_key_list_and_remove_never_print_a_key() {
        let (_dir, paths, store) = rig("providers-cli");
        let keys = || ProviderKeys::with(Vault::with(&paths.data.join(crate::providers::keys::FILE), Some(store.clone())));
        let read_key = |_: &str| Ok(KEY.to_string());
        let mut out = Vec::new();
        let mut run = |cmd| run(&paths, &keys, &read_key, cmd, &mut out);
        run(ProvidersCmd::Add { id: "openrouter".into(), kind: "openrouter".into(), label: None, base_url: None }).unwrap();
        assert!(run(ProvidersCmd::Add { id: "or2".into(), kind: "openrouter".into(), label: None, base_url: Some("https://x/v1".into()) }).is_err(), "no URL for a built-in kind");
        assert!(run(ProvidersCmd::Add { id: "box".into(), kind: "custom".into(), label: None, base_url: None }).is_err(), "custom needs one");
        run(ProvidersCmd::Add { id: "box".into(), kind: "custom".into(), label: Some("My box".into()), base_url: Some("http://192.168.1.10:11434/v1/".into()) }).unwrap();
        run(ProvidersCmd::SetKey { id: "openrouter".into() }).unwrap();
        run(ProvidersCmd::List { json: false }).unwrap();
        let text = std::fs::read_to_string(&paths.config).unwrap();
        assert!(text.starts_with("# mine\n[api]\nport = 47201\n"), "{text}");
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.providers["box"].base_url, "http://192.168.1.10:11434/v1");
        assert!(!text.contains(KEY));
        // In use: refused without --force.
        std::fs::write(&paths.config, format!("{text}\n[context.edit]\nprovider = \"openrouter\"\nmodel = \"m\"\n")).unwrap();
        assert!(run(ProvidersCmd::Remove { id: "openrouter".into(), force: false }).is_err());
        run(ProvidersCmd::Remove { id: "openrouter".into(), force: true }).unwrap();
        assert!(!Config::load(&paths.config).unwrap().providers.contains_key("openrouter"));
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("openrouter key …WXYZ (29 chars) stored in memory"), "{printed}");
        assert!(printed.contains("box            custom      http://192.168.1.10:11434/v1  (no key needed)"), "{printed}");
        assert!(printed.contains("openrouter removed, and its key"), "{printed}");
        for start in 0..KEY.len() - 8 {
            assert!(!printed.contains(&KEY[start..start + 8]));
        }
    }
}
