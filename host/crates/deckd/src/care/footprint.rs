//! `care.footprint`: everything the app has put on this Mac, item by item,
//! with its size where a size means something (the Footprint page); and
//! `care.cache.clear`, the one delete that had no method yet (Firmware
//! downloads' Delete…; the plugin, the Codex setup, the Codex Desktop
//! launcher and the two voice models go through `integrations.uninstall`,
//! `stt.remove` and `tts.remove`).
//!
//! A size is the bytes of the regular files under a directory, walked
//! without following a symlink: a link into somebody's dotfiles is not the
//! app's to weigh. Nothing is weighed twice. The Claude Code plugin is not
//! part of `data`, and neither are the logs and the caches when a
//! `DECKD_DATA_DIR` puts them inside it. The bootloader backups are part of
//! nothing: Delete… keeps them, so `firmware` is what it would free.
//!
//! One item is not on disk: `keychain`, the Keychain item that holds the
//! cloud voice's key on a Mac (`crate::secrets`). It has no size, its path
//! is the item's service name, and it is there when the service last knew
//! the store to hold a key. The Keychain is not asked here. Uninstall's
//! `--purge` deletes it.
use super::{Context, Homes};
use crate::firmware::{BOOTLOADER_BACKUPS, CACHE_DIR};
use deck_model::ApiError;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The items, in the order the service lists them: the nine it always had,
/// then `codex_desktop` and `keychain`, each added after the ones before.
/// The app finds each item by its id and lists them in an order of its own.
pub const IDS: [&str; 11] = [
    "service",
    "claude_plugin",
    "codex_hooks",
    "stt",
    "kokoro",
    "firmware",
    "data",
    "prefs",
    "logs",
    "codex_desktop",
    "keychain",
];

/// One thing on disk. `bytes` and `count` are `None` where they do not
/// apply, and for anything that is not there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: &'static str,
    pub path: String,
    pub bytes: Option<u64>,
    /// The plugin's hook events; the Codex setup's hook groups.
    pub count: Option<u64>,
    /// On disk now: a file, or a directory holding at least one file.
    pub present: bool,
}

impl Item {
    /// A directory, weighed without the directories in `skip`.
    fn dir(id: &'static str, root: &Path, skip: &[PathBuf]) -> Item {
        let weight = weigh(root, skip).filter(|weight| weight.files > 0);
        Item {
            id,
            path: root.to_string_lossy().into_owned(),
            bytes: weight.map(|weight| weight.bytes),
            count: None,
            present: weight.is_some(),
        }
    }

    /// One file, its size as it reads (a symlinked file is its target's).
    fn file(id: &'static str, path: &Path) -> Item {
        let bytes = std::fs::metadata(path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|meta| meta.len());
        Item {
            id,
            path: path.to_string_lossy().into_owned(),
            bytes,
            count: None,
            present: bytes.is_some(),
        }
    }

    /// The count of a thing that is there; nothing for one that is not.
    fn counting(mut self, count: Option<usize>) -> Item {
        self.count = count
            .filter(|_| self.present)
            .and_then(|n| u64::try_from(n).ok());
        self
    }
}

/// `{"items": [...], "total_bytes": n}`.
pub fn measure(context: &Context, homes: &Homes) -> serde_json::Value {
    let items = items(context, homes);
    let total: u64 = items.iter().filter_map(|item| item.bytes).sum();
    serde_json::json!({ "items": items, "total_bytes": total })
}

/// The items of [`IDS`], in that order.
pub fn items(context: &Context, homes: &Homes) -> Vec<Item> {
    let paths = &context.paths;
    let marketplace = crate::integrations::claude::marketplace_in(&paths.data);
    let firmware = paths.caches.join(CACHE_DIR);
    let exe = context.executable.clone().unwrap_or_default();
    let codex_hooks = homes.codex.join("hooks.json");
    // Codex's own file is the setup only while deckd's hooks are in it.
    let groups =
        crate::integrations::codex::managed_groups(&codex_hooks, &exe).filter(|groups| *groups > 0);
    let mut codex = Item::file("codex_hooks", &codex_hooks);
    if groups.is_none() {
        codex.bytes = None;
        codex.present = false;
    }
    vec![
        Item {
            id: "service",
            path: exe.to_string_lossy().into_owned(),
            bytes: None,
            count: None,
            present: exe.is_file(),
        },
        Item::dir("claude_plugin", &marketplace, &[])
            .counting(crate::integrations::claude::installed_hooks(&paths.data)),
        codex.counting(groups),
        Item::dir("stt", &paths.stt, &[]),
        Item::dir("kokoro", &paths.kokoro, &[]),
        Item::dir("firmware", &firmware, &[firmware.join(BOOTLOADER_BACKUPS)]),
        Item::dir(
            "data",
            &paths.data,
            &[
                marketplace.clone(),
                paths.logs.clone(),
                paths.caches.clone(),
            ],
        ),
        Item::file("prefs", &homes.prefs),
        Item::dir("logs", &paths.logs, &[]),
        // The Codex Desktop launcher the Integrations page's Set Up writes
        // (`integrations::codex_desktop`): the app bundle, its files
        // weighed; `integrations.uninstall codex-desktop` removes it.
        Item::dir(
            "codex_desktop",
            &crate::integrations::codex_desktop::launcher_in(&homes.home),
            &[],
        ),
        // The cloud voice's key, as the service knows its store: never a
        // Keychain read here.
        Item {
            id: "keychain",
            path: crate::secrets::SERVICE.into(),
            bytes: None,
            count: None,
            present: context.keychain == Some(true),
        },
    ]
}

/// What a walk found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Weight {
    bytes: u64,
    files: u64,
}

/// The regular files under `root`, not following a symlink and not
/// entering a directory of `skip`. `None` when `root` is not a directory.
/// What cannot be read is passed over: a size is never an error.
fn weigh(root: &Path, skip: &[PathBuf]) -> Option<Weight> {
    if !root.is_dir() {
        return None;
    }
    let mut weight = Weight::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let path = entry.path();
                if !skip.contains(&path) {
                    pending.push(path);
                }
            } else if kind.is_file() {
                if let Ok(meta) = entry.metadata() {
                    weight.bytes += meta.len();
                    weight.files += 1;
                }
            }
        }
    }
    Some(weight)
}

/// What `care.cache.clear` clears: the one cache it may.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// The firmware updater's downloads, its bootloader backups kept.
    Firmware,
}

impl Cache {
    pub const NAMES: [&'static str; 1] = ["firmware"];

    pub fn parse(what: &str) -> Result<Cache, ApiError> {
        match what {
            "firmware" => Ok(Cache::Firmware),
            other => Err(ApiError::invalid_params(&format!(
                "what: {other:?} is not a cache this service clears; one of {:?}",
                Cache::NAMES
            ))),
        }
    }
}

/// `care.cache.clear` → `{"removed_bytes": n}`. Refused while a firmware
/// check or install runs: it is reading those files.
pub fn clear(context: &Context, cache: Cache) -> Result<serde_json::Value, ApiError> {
    match cache {
        Cache::Firmware => {
            if context.firmware_busy {
                return Err(ApiError::invalid(
                    "firmware",
                    "A firmware check or install is running. Clear the downloads when it has \
                     finished.",
                ));
            }
            let removed = clear_firmware(&context.paths.caches.join(CACHE_DIR))
                .map_err(|why| ApiError::internal(&why))?;
            Ok(serde_json::json!({ "removed_bytes": removed }))
        }
    }
}

/// Remove everything in `dir` but [`BOOTLOADER_BACKUPS`], without following
/// a symlink out of it, and say how many bytes of files went. A `dir` that
/// is not there has nothing to clear.
fn clear_firmware(dir: &Path) -> Result<u64, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if entry.file_name() == BOOTLOADER_BACKUPS {
            continue;
        }
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (bytes, gone) = if kind.is_dir() {
            let bytes = weigh(&path, &[]).unwrap_or_default().bytes;
            (bytes, std::fs::remove_dir_all(&path))
        } else {
            let bytes = if kind.is_file() {
                entry.metadata().map_or(0, |meta| meta.len())
            } else {
                0
            };
            (bytes, std::fs::remove_file(&path))
        };
        match gone {
            Ok(()) => removed += bytes,
            // Another clear got there first: nothing of it is left to free.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", path.display())),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::care::testkit::{context, homes};
    use crate::integrations::testdir::TestDir;
    use serde_json::Value;
    use std::collections::BTreeSet;

    /// A file of `len` bytes at `name` under the test dir.
    fn sized(dir: &TestDir, name: &str, len: usize) -> PathBuf {
        dir.write(name, &"x".repeat(len))
    }

    /// Every item there, with sizes known to the byte: the data dir apart
    /// from the logs and the caches, as on macOS, and a cloud voice key in
    /// the store.
    fn every_item(dir: &TestDir) -> (Context, Homes) {
        let mut context = context(dir);
        context.keychain = Some(true);
        let homes = homes(dir);
        sized(dir, "bin/deckd", 300);
        sized(dir, "data/config.toml", 40);
        sized(dir, "data/state.json", 60);
        dir.write(
            "data/claude-marketplace/plugins/openwalkietalkie/hooks/hooks.json",
            r#"{"hooks":{"Stop":[],"SessionStart":[],"Notification":[]}}"#,
        );
        sized(
            dir,
            "data/claude-marketplace/.claude-plugin/marketplace.json",
            25,
        );
        // A deckd at an absolute path as this platform writes one.
        let deckd = if cfg!(windows) { "C:/opt/deckd.exe" } else { "/opt/deckd" };
        dir.write(
            "home/.codex/hooks.json",
            &r#"{"hooks":{
                "Stop":[{"hooks":[{"type":"command","command":"DECKD hook codex"}]},
                        {"hooks":[{"type":"command","command":"echo mine"}]}],
                "SessionEnd":[{"hooks":[{"type":"command","command":"DECKD hook codex"}]}]
            }}"#
                .replace("DECKD", deckd),
        );
        sized(dir, "caches/stt/ggml-large-v3-turbo.bin", 1000);
        sized(dir, "caches/tts/kokoro/model.onnx", 700);
        sized(dir, "caches/tts/kokoro/voices/af.bin", 50);
        sized(dir, "caches/tts/venv/python", 999);
        sized(dir, "caches/firmware/release-manifest.json", 30);
        sized(dir, "caches/firmware/images/0.13.1/app.bin", 200);
        sized(dir, "caches/firmware/bootloader-backups/F657-abc.bin", 4000);
        sized(
            dir,
            "home/Library/Preferences/ai.cohdl.openwalkietalkie.plist",
            90,
        );
        sized(dir, "logs/deckd.log", 120);
        sized(dir, "logs/deckd.launchd.log", 8);
        (context, homes)
    }

    /// The Codex Desktop launcher, its plist and its script: 210 bytes.
    /// Not one of [`every_item`]'s, because the app's fixture shows it
    /// absent.
    fn launcher(dir: &TestDir) {
        let app = "home/Applications/Codex with OpenWalkieTalkie.app/Contents";
        sized(dir, &format!("{app}/Info.plist"), 150);
        sized(dir, &format!("{app}/MacOS/launch-codex"), 60);
    }

    fn item<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
    }

    fn json_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    fn keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// B3.3: with every item there but the Codex Desktop launcher (absent in
    /// the fixture too), the reply has the shape of
    /// `GC/tests/fixtures/daemon/care_footprint.json`: every item the app
    /// reads, by id, each key with the same JSON type, `null` where the
    /// fixture has `null`. The app lists the items in its own order and
    /// finds each by id, so the service keeps its own ([`IDS`], a new item
    /// last); an item the fixture does not know yet is shaped like the
    /// directory items it does.
    #[test]
    fn the_reply_has_every_item_of_the_apps_fixture_with_the_same_types() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/care_footprint.json"
        ))
        .unwrap();
        let dir = TestDir::new("care-footprint-fixture");
        let (context, homes) = every_item(&dir);
        let reply = measure(&context, &homes);
        assert_eq!(keys(&reply), keys(&fixture));
        assert_eq!(json_type(&reply["total_bytes"]), "number");
        let (got, want) = (
            reply["items"].as_array().unwrap(),
            fixture["items"].as_array().unwrap(),
        );
        let ids = |items: &[Value]| -> Vec<String> {
            items
                .iter()
                .map(|i| i["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(got), IDS);
        let same_shape = |got: &Value, want: &Value| {
            assert_eq!(keys(got), keys(want), "{got}");
            for key in keys(want) {
                assert_eq!(
                    json_type(&got[key]),
                    json_type(&want[key]),
                    "{}.{key}: {} where the app reads {}",
                    got["id"],
                    got[key],
                    want[key]
                );
            }
        };
        let by_id = |items: &[Value], id: &Value| items.iter().find(|i| i["id"] == *id).cloned();
        for want in want {
            let got = by_id(got, &want["id"])
                .unwrap_or_else(|| panic!("{}: an item the app reads is not served", want["id"]));
            same_shape(&got, want);
        }
        let directory = by_id(want, &"logs".into()).expect("the fixture's logs");
        for newer in got.iter().filter(|got| by_id(want, &got["id"]).is_none()) {
            same_shape(newer, &directory);
        }
    }

    /// Each item weighs what its tree holds, to the byte. The plugin is not
    /// part of `data`, the bootloader backups are part of nothing, the
    /// counts are the plugin's hook events and deckd's own Codex groups,
    /// and the total is the sum.
    #[test]
    fn every_item_weighs_its_own_tree_and_nothing_twice() {
        let dir = TestDir::new("care-footprint-bytes");
        let (context, homes) = every_item(&dir);
        launcher(&dir);
        let items = items(&context, &homes);
        assert!(items.iter().all(|item| item.present), "{items:?}");
        let service = item(&items, "service");
        assert_eq!((service.bytes, service.count), (None, None));
        assert_eq!(service.path, dir.path().join("bin/deckd").to_string_lossy());
        let plugin = item(&items, "claude_plugin");
        let plugin_json = r#"{"hooks":{"Stop":[],"SessionStart":[],"Notification":[]}}"#;
        assert_eq!(plugin.bytes, Some(plugin_json.len() as u64 + 25));
        assert_eq!(plugin.count, Some(3), "the events in its hooks.json");
        let codex = item(&items, "codex_hooks");
        assert_eq!(
            codex.count,
            Some(2),
            "deckd's two groups, not the user's echo"
        );
        assert_eq!(
            codex.bytes,
            Some(
                std::fs::metadata(homes.codex.join("hooks.json"))
                    .unwrap()
                    .len()
            )
        );
        assert_eq!(item(&items, "stt").bytes, Some(1000));
        assert_eq!(item(&items, "kokoro").bytes, Some(750), "not the MLX venv");
        assert_eq!(
            item(&items, "firmware").bytes,
            Some(230),
            "backups excluded"
        );
        assert_eq!(item(&items, "data").bytes, Some(100), "the plugin excluded");
        assert_eq!(item(&items, "prefs").bytes, Some(90));
        assert_eq!(item(&items, "logs").bytes, Some(128));
        // The Codex Desktop launcher, where its installer writes it.
        let launcher = item(&items, "codex_desktop");
        assert_eq!(launcher.bytes, Some(210), "its plist and its script");
        assert_eq!(
            launcher.path,
            crate::integrations::codex_desktop::launcher_in(&homes.home).to_string_lossy()
        );
        assert!(launcher
            .path
            .replace('\\', "/")
            .ends_with("/Applications/Codex with OpenWalkieTalkie.app"));
        for id in [
            "stt",
            "kokoro",
            "firmware",
            "data",
            "prefs",
            "logs",
            "codex_desktop",
            "keychain",
        ] {
            assert_eq!(item(&items, id).count, None, "{id}");
        }
        // The Keychain item weighs nothing the service could say.
        assert_eq!(item(&items, "keychain").bytes, None);
        let total = measure(&context, &homes)["total_bytes"].as_u64().unwrap();
        assert_eq!(
            total,
            plugin.bytes.unwrap() + codex.bytes.unwrap() + 1000 + 750 + 230 + 100 + 90 + 128 + 210
        );
    }

    /// A data dir that holds the logs and the caches (`DECKD_DATA_DIR`)
    /// does not weigh them again, and a fresh install's items are absent
    /// with no size and no count: a directory with no file in it, a
    /// Codex file with none of deckd's hooks, no preferences yet.
    #[test]
    fn nested_directories_are_weighed_once_and_absent_items_say_nothing() {
        let dir = TestDir::new("care-footprint-nested");
        let data = dir.sub("data");
        let mut context = context(&dir);
        context.paths = crate::config::Paths::from_data_dir(
            data.clone(),
            data.join("logs"),
            data.join("caches"),
        );
        let homes = homes(&dir);
        sized(&dir, "data/config.toml", 40);
        sized(&dir, "data/logs/deckd.log", 120);
        sized(&dir, "data/caches/firmware/release-manifest.json", 30);
        std::fs::create_dir_all(data.join("caches/stt")).unwrap();
        dir.write(
            "home/.codex/hooks.json",
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        );
        let items = items(&context, &homes);
        assert_eq!(item(&items, "data").bytes, Some(40));
        assert_eq!(item(&items, "logs").bytes, Some(120));
        assert_eq!(item(&items, "firmware").bytes, Some(30));
        for id in [
            "service",
            "claude_plugin",
            "codex_hooks",
            "stt",
            "kokoro",
            "prefs",
            "codex_desktop",
            "keychain",
        ] {
            let absent = item(&items, id);
            assert!(!absent.present, "{absent:?}");
            assert_eq!((absent.bytes, absent.count), (None, None), "{id}");
        }
    }

    /// The Keychain item is there only while the service knows its store to
    /// hold the cloud voice's key: not for a key kept in `config.toml`
    /// (which is part of the data), not for an empty store. It is named by
    /// the item's service, and has no size.
    #[test]
    fn the_keychain_item_is_there_only_while_the_store_holds_a_key() {
        let dir = TestDir::new("care-footprint-keychain");
        let mut context = context(&dir);
        let homes = homes(&dir);
        for (keychain, present) in [(None, false), (Some(false), false), (Some(true), true)] {
            context.keychain = keychain;
            let items = items(&context, &homes);
            let row = item(&items, "keychain");
            assert_eq!(row.present, present, "{keychain:?}");
            assert_eq!(row.path, "ai.cohdl.openwalkietalkie");
            assert_eq!((row.bytes, row.count), (None, None));
        }
        assert_eq!(IDS.last(), Some(&"keychain"), "a new item goes last");
    }

    /// `care.cache.clear{what:"firmware"}` takes every download and keeps
    /// the bootloader backups, says how much it freed, has nothing to do a
    /// second time, and waits for a firmware operation that is running.
    #[test]
    #[cfg(unix)] // a symlink out of the cache
    fn clearing_the_firmware_cache_keeps_the_bootloader_backups() {
        let dir = TestDir::new("care-cache-clear");
        let (mut context, _) = every_item(&dir);
        let backup = dir
            .path()
            .join("caches/firmware/bootloader-backups/F657-abc.bin");
        let outside = sized(&dir, "elsewhere/keep.bin", 70);
        std::os::unix::fs::symlink(&outside, dir.path().join("caches/firmware/link.bin")).unwrap();
        context.firmware_busy = true;
        let busy = clear(&context, Cache::Firmware).unwrap_err();
        assert_eq!(busy.code, ApiError::INVALID);
        assert!(dir
            .path()
            .join("caches/firmware/release-manifest.json")
            .exists());
        context.firmware_busy = false;
        let cleared = clear(&context, Cache::Firmware).unwrap();
        assert_eq!(cleared, serde_json::json!({"removed_bytes": 230}));
        assert!(backup.exists(), "a bootloader backup is never a download");
        assert!(outside.exists(), "a symlink goes, not what it points at");
        let left: Vec<_> = std::fs::read_dir(dir.path().join("caches/firmware"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [BOOTLOADER_BACKUPS]);
        assert_eq!(
            clear(&context, Cache::Firmware).unwrap(),
            serde_json::json!({"removed_bytes": 0})
        );
        assert_eq!(
            weigh(&dir.path().join("caches/firmware"), &[])
                .unwrap()
                .bytes,
            4000
        );
        // No firmware cache at all is nothing to clear.
        std::fs::remove_dir_all(dir.path().join("caches/firmware")).unwrap();
        assert_eq!(
            clear(&context, Cache::Firmware).unwrap()["removed_bytes"],
            0
        );
        assert_eq!(Cache::parse("firmware"), Ok(Cache::Firmware));
        assert_eq!(
            Cache::parse("stt").unwrap_err().code,
            ApiError::INVALID_PARAMS
        );
    }
}
