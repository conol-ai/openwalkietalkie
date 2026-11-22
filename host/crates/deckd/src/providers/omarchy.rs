//! The `omarchy` provider: this machine's own local models, discovered from
//! Omarchy's tooling and never configured.
//!
//! **What is read.** `omarchy-models status --json` (Omarchy's `omarchy
//! models status`, from the `omarchy-local-ai` package; Omarchy 4.6, on its
//! `quattro` branch when this was written and not released, so the shapes
//! below are from its documentation, not from a running machine). Its
//! documented shape is the engine's `status.json`: `{models: {<recipe>:
//! <model>}, …}`, each model with an `endpoint` including `/v1` (the first is
//! `http://127.0.0.1:11435/v1`, one loopback port per model), the recipe name
//! being the model id, and the key in `~/.config/omarchy/agent-local/key`.
//!
//! **What this depends on, and nothing more:**
//! * the output is one JSON object with a `models` object;
//! * each entry's key is the model id (a `recipe` field, when present, is
//!   preferred);
//! * each entry has an `endpoint` string, an `http://` or `https://` URL.
//!
//! Read when present, never required: `kind` (an entry that says it is not
//! `text` is left out: image models answer no chat), `unit` (shown as
//! running or not), `name` (the display name). Every other field is
//! ignored. An entry missing `endpoint` is left out; nothing missing fails
//! the whole read.
//!
//! **Fail quiet.** No `omarchy-models`, a command that fails, times out,
//! prints nothing, prints something that is not that shape, or lists no
//! text model: the provider is simply not there. No error, no log line
//! above debug. Only the `omarchy-models` binary is run: `omarchy agent`
//! launches a coding agent when it does not know the words it is given.
//!
//! **The wire** is OpenAI's: the engine documents its text runtimes as
//! OpenAI-compatible (`api = "openai"`). Whether they also answer
//! Anthropic's Messages API is not documented there and is not used.
use crate::config::Paths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The provider's id: it is discovered, never an entry of `[providers]`.
pub const ID: &str = "omarchy";
/// The program run.
pub const PROGRAM: &str = "omarchy-models";
/// How long discovery may take.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// One local model.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct LocalModel {
    /// The recipe: the model id.
    pub id: String,
    /// Its base URL, `…/v1`.
    pub endpoint: String,
    /// The display name, when the status gave one.
    #[serde(default)]
    pub name: Option<String>,
    /// `active`, `inactive`, … when the status gave one.
    #[serde(default)]
    pub unit: Option<String>,
}

impl LocalModel {
    /// Whether it is running now (`unit = "active"`); unknown counts as
    /// not known to be running.
    #[must_use]
    pub fn running(&self) -> bool {
        self.unit.as_deref() == Some("active")
    }
}

/// What a discovery found, cached with when.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Discovery {
    pub at_ms: u64,
    pub models: Vec<LocalModel>,
}

impl Discovery {
    /// Model `id`'s base URL.
    #[must_use]
    pub fn endpoint(&self, id: &str) -> Option<&str> {
        self.models
            .iter()
            .find(|model| model.id == id)
            .map(|model| model.endpoint.as_str())
    }
}

/// The text models in a `status --json` answer, or `None` when it is not
/// that shape or lists none.
#[must_use]
pub fn parse(output: &str) -> Option<Vec<LocalModel>> {
    let value: Value = serde_json::from_str(output.trim()).ok()?;
    let models = value.get("models")?.as_object()?;
    let mut found: Vec<LocalModel> = models
        .iter()
        .filter_map(|(key, model)| {
            let model = model.as_object()?;
            let text = |field: &str| {
                model
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            };
            if text("kind").is_some_and(|kind| kind != "text") {
                return None;
            }
            let endpoint = text("endpoint")?.trim_end_matches('/').to_string();
            if crate::context::http::url_problem(&endpoint).is_some() {
                return None;
            }
            let id = text("recipe").unwrap_or_else(|| key.trim().to_string());
            if id.is_empty() {
                return None;
            }
            Some(LocalModel {
                id,
                endpoint,
                name: text("name"),
                unit: text("unit"),
            })
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    (!found.is_empty()).then_some(found)
}

/// `program` on `path` (a `PATH` value), if it is there.
#[must_use]
pub fn find(path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(PROGRAM))
        .find(|candidate| candidate.is_file())
}

/// Run `program status --json` within `timeout`: its output, or `None`.
#[must_use]
pub fn status_output(program: &Path, timeout: Duration) -> Option<String> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(4 * 1024 * 1024).read_to_end(&mut out);
            out
        })
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let out = reader.and_then(|r| r.join().ok())?;
    String::from_utf8(out).ok()
}

/// Discover with `program` (none: the provider is not there).
#[must_use]
pub fn discover_with(program: Option<&Path>, now_ms: u64) -> Option<Discovery> {
    let output = status_output(program?, TIMEOUT)?;
    let models = parse(&output)?;
    Some(Discovery { at_ms: now_ms, models })
}

/// Discover on this machine: `omarchy-models` on `PATH`.
#[must_use]
pub fn discover(now_ms: u64) -> Option<Discovery> {
    let program = std::env::var_os("PATH").and_then(|path| find(&path));
    discover_with(program.as_deref(), now_ms)
}

/// The discovery cache: `caches/providers/omarchy.json`.
#[must_use]
pub fn cache_path(paths: &Paths) -> PathBuf {
    paths.caches.join("providers").join("omarchy.json")
}

/// The last discovery, if one found anything.
#[must_use]
pub fn cached(paths: &Paths) -> Option<Discovery> {
    std::fs::read_to_string(cache_path(paths))
        .ok()
        .and_then(|text| serde_json::from_str::<Discovery>(&text).ok())
        .filter(|d| !d.models.is_empty())
}

/// Discover again and cache what was found; a discovery that finds
/// nothing removes the cache, so the provider vanishes with the models.
pub fn refresh(paths: &Paths, found: Option<Discovery>) -> Option<Discovery> {
    let path = cache_path(paths);
    match &found {
        Some(discovery) => {
            let write = || -> Result<(), String> {
                let dir = path.parent().ok_or("no directory")?;
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                let text = serde_json::to_string_pretty(discovery).map_err(|e| e.to_string())?;
                let tmp = dir.join(format!(".omarchy.json.{}.tmp", std::process::id()));
                {
                    use std::io::Write as _;
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt as _;
                        options.mode(0o600);
                    }
                    let mut out = options.open(&tmp).map_err(|e| e.to_string())?;
                    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
            };
            if let Err(why) = write() {
                tracing::debug!("providers: the omarchy discovery was not cached: {why}");
            }
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    found
}

/// The key Omarchy's local models take: `~/.config/omarchy/agent-local/key`
/// (`$XDG_CONFIG_HOME` when set), trimmed. `None` when it is not there.
#[must_use]
pub fn key() -> Option<String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    key_at(&config.join("omarchy/agent-local/key"))
}

/// The key in the file at `path`, trimmed.
#[must_use]
pub fn key_at(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|key| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;

    /// What the engine's documentation says `status --json` prints, with
    /// fields this does not read (and one it does not know yet) around the
    /// three it depends on.
    const STATUS: &str = r#"{
      "models": {
        "qwen3.8-27b-gufo": {
          "recipe": "qwen3.8-27b-gufo", "kind": "text", "name": "Qwen 3.8 27B", "vendor": "qwen",
          "runtime": "llamacpp", "ctx": 131072, "port": 11435, "endpoint": "http://127.0.0.1:11435/v1",
          "unit": "active", "sub_state": "running", "fits": true, "update": null, "observed": {"ram_used_gib": 40.1},
          "something_new_in_4_7": {"x": 1}
        },
        "qwen3.6-35b-a3b-llamacpp": { "endpoint": "http://127.0.0.1:11436/v1/" },
        "flux-schnell": { "kind": "image", "endpoint": "http://127.0.0.1:11437/v1", "unit": "active" },
        "installing-one": { "kind": "text", "endpoint": "", "unit": "installing" },
        "broken": "not an object"
      },
      "last": "qwen3.8-27b-gufo", "ram_total_gib": 128, "best": null, "link": {"state": "none"}
    }"#;

    #[test]
    fn the_status_gives_the_text_models_and_their_endpoints_and_tolerates_the_rest() {
        let models = parse(STATUS).unwrap();
        assert_eq!(
            models.iter().map(|m| (m.id.as_str(), m.endpoint.as_str())).collect::<Vec<_>>(),
            [
                ("qwen3.6-35b-a3b-llamacpp", "http://127.0.0.1:11436/v1"),
                ("qwen3.8-27b-gufo", "http://127.0.0.1:11435/v1"),
            ],
            "image models and entries with no endpoint are left out"
        );
        assert!(models[1].running() && !models[0].running());
        assert_eq!(models[1].name.as_deref(), Some("Qwen 3.8 27B"));
        for nothing in ["", "   ", "{}", "[]", "{\"models\": {}}", "{\"models\": []}", "not json", "{\"models\": {\"a\": {\"endpoint\": \"ftp://x\"}}}"] {
            assert_eq!(parse(nothing), None, "{nothing:?}");
        }
    }

    /// A fake `omarchy-models` on a PATH of its own: a realistic status, one
    /// that prints nothing, one that fails, and one that prints garbage.
    #[cfg(unix)]
    #[test]
    fn a_fake_command_on_path_is_discovered_and_every_failure_makes_the_provider_vanish() {
        use std::os::unix::fs::PermissionsExt as _;
        let script = |dir: &TestDir, body: &str| {
            let bin = dir.sub("bin");
            let program = bin.join(PROGRAM);
            std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        };
        let found = |bin: &Path| discover_with(find(bin.as_os_str()).as_deref(), 7);
        let dir = TestDir::new("omarchy-status");
        std::fs::write(dir.path().join("status.json"), STATUS).unwrap();
        let bin = script(&dir, &format!("[ \"$1 $2\" = \"status --json\" ] || exit 2\ncat '{}'", dir.path().join("status.json").display()));
        let discovery = found(&bin).expect("discovered");
        assert_eq!((discovery.at_ms, discovery.models.len()), (7, 2));
        assert_eq!(discovery.endpoint("qwen3.8-27b-gufo"), Some("http://127.0.0.1:11435/v1"));
        for (name, body) in [
            ("omarchy-silent", "exit 0"),
            ("omarchy-fails", "echo '{\"models\":{\"a\":{\"endpoint\":\"http://127.0.0.1:1/v1\"}}}'; exit 1"),
            ("omarchy-garbage", "echo 'Models: qwen3 (running) at 127.0.0.1:11435'"),
            ("omarchy-slow", "sleep 10"),
        ] {
            let dir = TestDir::new(name);
            let bin = script(&dir, body);
            assert_eq!(found(&bin), None, "{name}: the provider vanishes");
        }
        // No such program at all.
        let empty = TestDir::new("omarchy-absent");
        assert_eq!(found(empty.path()), None);
    }

    #[test]
    fn the_cache_follows_the_discovery_and_the_key_is_read_from_its_file() {
        let dir = TestDir::new("omarchy-cache");
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        let discovery = Discovery { at_ms: 9, models: parse(STATUS).unwrap() };
        refresh(&paths, Some(discovery.clone()));
        assert_eq!(cached(&paths), Some(discovery));
        refresh(&paths, None);
        assert_eq!(cached(&paths), None, "nothing found: the provider is gone");
        std::fs::write(dir.path().join("key"), "  local-key-1234 \n").unwrap();
        assert_eq!(key_at(&dir.path().join("key")).as_deref(), Some("local-key-1234"));
        assert_eq!(key_at(&dir.path().join("absent")), None);
    }
}
