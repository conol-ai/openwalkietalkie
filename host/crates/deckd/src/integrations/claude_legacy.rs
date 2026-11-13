//! The pre-plugin Claude integration, kept only to **take it back out**.
//!
//! Until 2026-10 deckd merged its own entries into
//! `$CLAUDE_CONFIG_DIR/settings.json`: one exec-form `SessionStart` command
//! hook, `type:"http"` hooks for every other event carrying a literal
//! bearer, a pattern appended to `allowedHttpHookUrls`, a chained
//! `statusLine` and a top-level `_openwalkietalkie` inventory. The Claude
//! target is a **plugin** now ([`super::claude`]) and writes nothing into
//! that file — but a machine that installed the old shape still carries it,
//! and both `install` (as a migration) and `uninstall` must remove exactly
//! those entries and nothing else.
//!
//! Everything here is the un-merge half of the old installer, verbatim in
//! its rules (corrections C3, C23, C24, C24a of the week-3 contract):
//!
//! - **Ownership is decided by `url` and `command`**, never by a marker:
//!   Claude deletes the `_managed` hint on its next write (C24).
//! - **A mixed group is refused**, never dissected: deckd removes only the
//!   groups it wrote whole.
//! - **`allowedHttpHookUrls` loses only the exact patterns the inventory
//!   records deckd appending.** A defined-but-empty list gates every http
//!   hook on the machine against an empty union (C3), so the key is removed
//!   only when deckd added it and nothing else is left in it.
//! - **The chained `statusLine` gives the user's command back**, from the
//!   inventory's copy or decoded out of our own line (C23).
//! - **Never write a fatal shape** (C24a): the result is re-checked with
//!   Claude's own predicate before and after it is written.
use super::backup::{self, FileSnapshot};
use super::markers::{self, LEGACY_MANAGED_KEY, MANAGED_KEY};
use super::{Env, Plan};
use std::path::PathBuf;

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
pub fn settings_path(env: &Env) -> PathBuf {
    env.claude_dir().join("settings.json")
}

/// Does this settings object still carry anything the old installer wrote?
#[must_use]
pub fn present(root: &serde_json::Value, env: &Env) -> bool {
    !our_handlers(root).is_empty()
        || inventory(root).is_some()
        || root
            .get("statusLine")
            .is_some_and(|line| chained_command(line, env).is_some())
        || root
            .get("allowedHttpHookUrls")
            .and_then(|a| a.as_array())
            .is_some_and(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str())
                    .any(is_our_hook_url_pattern)
            })
}

/// Does this settings object still carry one of the old installer's
/// `type:"http"` hooks — the only entries that reach deckd through its TCP
/// door (`http://127.0.0.1:<port>/v1/hooks/claude`)? Narrower than
/// [`present`], which the Claude row asks: the exec-form `SessionStart`
/// hook reaches the daemon over its socket, and the inventory, the chained
/// `statusLine` and the allowlist pattern post nothing at all, so none of
/// them is a reason to keep the door open, though each still asks for
/// Repair.
#[must_use]
pub fn http_hooks_present(root: &serde_json::Value) -> bool {
    our_handlers(root)
        .iter()
        .any(|handler| handler.get("type").and_then(|t| t.as_str()) == Some("http"))
}

/// [`http_hooks_present`] over the settings file itself ([`settings_path`]),
/// read as `status` and `install` read it. A missing file, one that cannot
/// be read and one that is not a JSON object carry nothing of ours that
/// Claude would run. `deckd run` asks this at start: those hooks post to
/// the TCP door, which stays open for them while `[api] tcp` is left out
/// (`config::Api::tcp_door`).
pub fn http_hooks_on_disk(env: &Env) -> bool {
    match FileSnapshot::read(&settings_path(env)) {
        Ok(snapshot) if snapshot.exists() => {
            super::parse_json_object(&snapshot).is_ok_and(|root| http_hooks_present(&root))
        }
        _ => false,
    }
}

/// The settings file with every legacy entry taken back out, as a planned
/// write — or `None` when the file carries nothing of ours (including when
/// it does not exist). `receipt` is the inventory `integrations.json` kept
/// for this target, read when the file's own copy is gone.
pub fn plan_removal(
    env: &Env,
    receipt: Option<&serde_json::Value>,
    plan: &mut Plan,
) -> Result<Option<backup::Planned>, String> {
    let path = settings_path(env);
    let snapshot = FileSnapshot::read(&path)?;
    if !snapshot.exists() {
        return Ok(None);
    }
    let root = super::parse_json_object(&snapshot)?;
    if !present(&root, env) {
        return Ok(None);
    }
    let inherited: Option<serde_json::Value> =
        inventory(&root).cloned().or_else(|| receipt.cloned());
    let next = unmerge(env, root, inherited.as_ref(), plan)?;
    // C24a, on the value about to be serialised. The same predicate runs
    // again over the file after it is written (`verify_written`).
    if let Some(key) = markers::fatal_settings_key(&next) {
        return Err(format!(
            "refusing to write {}: the key {key} is a fatal settings diagnostic and Claude would \
             silently ignore the entire file (C24a)",
            path.display()
        ));
    }
    if next.as_object().is_some_and(|o| o.is_empty()) {
        // The old installer created the file and nothing of the user's is
        // left in it.
        plan.note(format!("removes {} (deckd created it)", path.display()));
        return Ok(Some(backup::Planned::remove(snapshot)));
    }
    // Prefer the exact bytes of the pre-install backup, so the file comes
    // back byte for byte rather than merely equivalently.
    let bytes = match super::bytes_of_equivalent_backup(&env.backups(), "settings.json", &next) {
        Some(bytes) => {
            plan.note("restores settings.json byte for byte from its pre-install backup");
            bytes
        }
        None => super::render_json(&next)?,
    };
    Ok(Some(backup::Planned::write(snapshot, bytes)))
}

/// Take deckd's entries back out, leaving everything else exactly as it is.
fn unmerge(
    env: &Env,
    mut root: serde_json::Value,
    inherited: Option<&serde_json::Value>,
    plan: &mut Plan,
) -> Result<serde_json::Value, String> {
    let hooks = hooks_object(&mut root)?;
    let events: Vec<String> = hooks.keys().cloned().collect();
    let mut removed = 0usize;
    for event in events {
        let Some(array) = hooks.get_mut(&event).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let before = array.len();
        if let Some(why) = strip_ours(array, env) {
            return Err(why);
        }
        removed += before - array.len();
    }
    prune_empty(hooks);
    let empty = hooks.is_empty();
    if let Some(object) = root.as_object_mut() {
        if empty {
            object.remove("hooks");
        }
    }
    plan.note(format!(
        "removes {removed} pre-plugin deckd hook entries from settings.json (the Claude \
         integration is a plugin now)"
    ));
    unmerge_allowlist(&mut root, inherited, plan);
    unmerge_status_line(env, &mut root, inherited, plan);
    if let Some(object) = root.as_object_mut() {
        object.remove(MANAGED_KEY);
        object.remove(LEGACY_MANAGED_KEY);
    }
    Ok(root)
}

/// `root["hooks"]`, created if absent, refused if it is not an object.
fn hooks_object(
    root: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "settings.json does not hold a JSON object".to_string())?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        return Err("the existing `hooks` value must be an object".into());
    }
    Ok(hooks.as_object_mut().expect("checked above"))
}

/// Drop every group that is ours. Returns a refusal when a group *mixes* one
/// of our handlers with someone else's: deckd does not dissect a group it did
/// not write whole.
fn strip_ours(array: &mut Vec<serde_json::Value>, env: &Env) -> Option<String> {
    let mut refusal = None;
    array.retain(|group| {
        let handlers = group
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();
        let ours = handlers.iter().filter(|h| is_ours(h, env)).count();
        if ours == 0 {
            return true;
        }
        if ours != handlers.len() {
            refusal = Some(format!(
                "a hook group holds a deckd handler next to {} other handler(s); remove it by \
                 hand before installing",
                handlers.len() - ours
            ));
        }
        false
    });
    refusal
}

/// Ownership, by `url` and `command` only (C24 — the `_managed` hint is
/// deleted by Claude and can never be relied on).
fn is_ours(handler: &serde_json::Value, env: &Env) -> bool {
    match handler.get("type").and_then(|t| t.as_str()) {
        Some("http") => handler
            .get("url")
            .and_then(|u| u.as_str())
            .is_some_and(is_our_hook_url),
        Some("command") => {
            let args: Vec<&str> = handler
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            args.first() == Some(&"hook")
                && args.get(1) == Some(&"claude")
                && handler
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| markers::is_managed_binary(c, &env.exe))
        }
        _ => false,
    }
}

/// `http://127.0.0.1:<any port>/v1/hooks/claude`, on loopback only. The port
/// is deliberately not pinned: a port change must still be recognised as our
/// own entry, not orphaned as a stranger's.
fn is_our_hook_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => return false,
    };
    let host = authority.split(':').next().unwrap_or_default();
    (host == "127.0.0.1" || host == "localhost") && path == "v1/hooks/claude"
}

fn prune_empty(hooks: &mut serde_json::Map<String, serde_json::Value>) {
    let empty: Vec<String> = hooks
        .iter()
        .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in empty {
        hooks.remove(&key);
    }
}

/// Every allowlist pattern the inventory records deckd appending.
///
/// An inventory written before this was a list records one `pattern` string;
/// it is read too, so an upgrade still takes back what the previous version
/// wrote.
fn recorded_patterns(allowlist: Option<&serde_json::Value>) -> Vec<String> {
    let Some(allowlist) = allowlist else {
        return Vec::new();
    };
    if !allowlist
        .get("appended")
        .and_then(|a| a.as_bool())
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let mut out: Vec<String> = allowlist
        .get("patterns")
        .and_then(|p| p.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if let Some(one) = allowlist.get("pattern").and_then(|p| p.as_str()) {
        if !out.iter().any(|p| p == one) {
            out.push(one.to_string());
        }
    }
    out
}

/// Does this `allowedHttpHookUrls` entry have the exact shape the old
/// installer wrote — `http://<loopback>:<any port>/v1/hooks/*`? A user's own
/// broader pattern (`*`, `http://127.0.0.1:*/...`) is *not* this shape and
/// stays theirs.
fn is_our_hook_url_pattern(pattern: &str) -> bool {
    let Some(rest) = pattern.strip_prefix("http://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    if path != "v1/hooks/*" {
        return false;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host != "127.0.0.1" && host != "localhost" {
        return false;
    }
    port.is_none_or(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
}

/// C3, the un-merge side: take back **only** the exact pattern the inventory
/// records deckd appending, and never leave an empty array behind a key that
/// carried something before — an `allowedHttpHookUrls` that is *defined but
/// empty* gates every http hook on the machine against an empty union.
fn unmerge_allowlist(
    root: &mut serde_json::Value,
    inherited: Option<&serde_json::Value>,
    plan: &mut Plan,
) {
    let Some(object) = root.as_object_mut() else {
        return;
    };
    let allowlist = inherited.and_then(|i| i.get("allowlist"));
    let created = allowlist
        .and_then(|a| a.get("added"))
        .and_then(|a| a.as_bool())
        .unwrap_or(false);
    let mut ours = recorded_patterns(allowlist);
    let Some(serde_json::Value::Array(mut items)) = object.get("allowedHttpHookUrls").cloned()
    else {
        return;
    };
    // With no inventory left (Claude rewrote the file, or the receipt is
    // gone) the only honest removal is of a pattern that has exactly the
    // shape deckd wrote: a user's own broader pattern is never touched.
    if ours.is_empty() {
        ours = items
            .iter()
            .filter_map(|i| i.as_str())
            .filter(|p| is_our_hook_url_pattern(p))
            .map(String::from)
            .collect();
    }
    if ours.is_empty() {
        plan.note(
            "leaves allowedHttpHookUrls exactly as it is: nothing in it has the shape deckd wrote",
        );
        return;
    }
    let mut removed: Vec<String> = Vec::new();
    for pattern in &ours {
        if let Some(at) = items
            .iter()
            .rposition(|item| item.as_str() == Some(pattern.as_str()))
        {
            items.remove(at);
            removed.push(pattern.clone());
        }
    }
    if !removed.is_empty() {
        plan.note(format!(
            "removes deckd's own {} from allowedHttpHookUrls and leaves every other pattern in \
             it untouched",
            removed.join(", ")
        ));
    }
    if items.is_empty() && created {
        object.remove("allowedHttpHookUrls");
        plan.note("removes allowedHttpHookUrls, which deckd added and nothing else uses");
    } else {
        object.insert(
            "allowedHttpHookUrls".to_string(),
            serde_json::Value::Array(items),
        );
    }
}

/// Give the user's `statusLine` back if ours is chained in front of it (C23).
fn unmerge_status_line(
    env: &Env,
    root: &mut serde_json::Value,
    inherited: Option<&serde_json::Value>,
    plan: &mut Plan,
) {
    let Some(object) = root.as_object_mut() else {
        return;
    };
    let Some(current) = object.get("statusLine").cloned() else {
        return;
    };
    let Some(previous_command) = chained_command(&current, env) else {
        return;
    };
    // The inventory's copy is the whole original object; the command decoded
    // out of our own line is the fallback when Claude rewrote the file and
    // the inventory went stale.
    let restored = inherited
        .and_then(|i| i.get("previousStatusLine"))
        .cloned()
        .or_else(|| {
            previous_command
                .map(|command| serde_json::json!({"type": "command", "command": command}))
        });
    match restored {
        Some(previous) => {
            plan.note("restores the statusLine deckd chained");
            object.insert("statusLine".to_string(), previous);
        }
        None => {
            plan.note("removes the statusLine deckd added");
            object.remove("statusLine");
        }
    }
}

/// `Some(previous command)` when this `statusLine` is deckd's chained one.
/// `Some(None)` means it is ours but wraps nothing.
fn chained_command(value: &serde_json::Value, env: &Env) -> Option<Option<String>> {
    let command = value.get("command")?.as_str()?;
    let (binary, rest) = markers::split_command(command)?;
    if !markers::is_managed_binary(&binary, &env.exe) {
        return None;
    }
    let rest = rest.strip_prefix("hook claude-statusline")?;
    match rest.trim().strip_prefix("--") {
        None => Some(None),
        Some(tail) => Some(markers::decode_shell_word(tail.trim())),
    }
}

fn inventory(root: &serde_json::Value) -> Option<&serde_json::Value> {
    [MANAGED_KEY, LEGACY_MANAGED_KEY]
        .into_iter()
        .find_map(|key| root.get(key).filter(|v| v.is_object()))
}

/// Every handler in the file that the `url`/`command` fingerprint calls ours.
fn our_handlers(root: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = Vec::new();
    let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) else {
        return out;
    };
    for groups in hooks.values() {
        for group in groups.as_array().into_iter().flatten() {
            for handler in group
                .get("hooks")
                .and_then(|h| h.as_array())
                .into_iter()
                .flatten()
            {
                let http = handler.get("type").and_then(|t| t.as_str()) == Some("http")
                    && handler
                        .get("url")
                        .and_then(|u| u.as_str())
                        .is_some_and(is_our_hook_url);
                let command = handler.get("type").and_then(|t| t.as_str()) == Some("command")
                    && handler
                        .get("args")
                        .and_then(|a| a.as_array())
                        .is_some_and(|args| {
                            args.first().and_then(|a| a.as_str()) == Some("hook")
                                && args.get(1).and_then(|a| a.as_str()) == Some("claude")
                        });
                if http || command {
                    out.push(handler);
                }
            }
        }
    }
    out
}

/// C24a's second half: read back what was written and run Claude's own
/// predicate over it. A file that trips it is discarded *whole* and in
/// silence, so this failure must undo the run rather than report success.
pub fn verify_written(env: &Env) -> Result<(), String> {
    let path = settings_path(env);
    let Ok(bytes) = std::fs::read(&path) else {
        // Nothing there: a removal of the file deckd created.
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "{} is no longer valid JSON after the write: {e}",
            path.display()
        )
    })?;
    match markers::fatal_settings_key(&value) {
        None => Ok(()),
        Some(key) => Err(format!(
            "{} was written with the fatal key {key}; Claude would ignore the whole file",
            path.display()
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::integrations::test_env;
    use crate::integrations::testdir::TestDir;

    /// A `settings.json` exactly as the old installer left one on a machine
    /// that also had a user's own hook, allowlist pattern and status line.
    pub(crate) fn legacy_settings(env: &Env) -> serde_json::Value {
        let exe = env.exe.display().to_string();
        let url = "http://127.0.0.1:47201/v1/hooks/claude";
        let http = |timeout: u64| {
            serde_json::json!({"hooks": [{
                "type": "http", "url": url, "timeout": timeout,
                "headers": {"Authorization": "Bearer dk_legacy_secret"}
            }]})
        };
        serde_json::json!({
            "model": "claude-haiku-4-5",
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": exe, "args": ["hook", "claude"]}]}],
                "PermissionRequest": [http(130)],
                "PreToolUse": [{"matcher": "AskUserQuestion", "hooks": http(130)["hooks"]}],
                "UserPromptSubmit": [http(2)],
                "Stop": [
                    {"hooks": [{"type": "command", "command": "/usr/local/bin/notify", "args": ["stopped"]}]},
                    http(2)
                ],
                "SessionEnd": [http(1)],
            },
            "allowedHttpHookUrls": ["https://audit.example/*", "http://127.0.0.1:47201/v1/hooks/*"],
            "statusLine": {
                "type": "command",
                "command": format!("{} hook claude-statusline -- 'npx ccstatusline@latest'", markers::shell_word(&exe)),
            },
            "_openwalkietalkie": {
                "schema": 1, "deckd": exe, "port": 47201, "url": url,
                "entries": ["SessionStart", "PermissionRequest", "PreToolUse(AskUserQuestion)", "UserPromptSubmit", "Stop", "SessionEnd"],
                "allowlist": {"appended": true, "added": false, "patterns": ["http://127.0.0.1:47201/v1/hooks/*"]},
                "previousStatusLine": {"type": "command", "command": "npx ccstatusline@latest"},
            },
        })
    }

    #[test]
    fn a_legacy_file_is_recognised_and_a_clean_one_is_not() {
        let dir = TestDir::new("legacy-present");
        let env = test_env(&dir);
        assert!(present(&legacy_settings(&env), &env));
        let clean = serde_json::json!({
            "model": "claude-haiku-4-5",
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/usr/local/bin/notify"}]}]},
            "allowedHttpHookUrls": ["https://audit.example/*"],
            "statusLine": {"type": "command", "command": "npx ccstatusline@latest"},
            "enabledPlugins": {"openwalkietalkie@openwalkietalkie": true},
        });
        assert!(!present(&clean, &env));
        // A stray pattern of our shape with no inventory is still ours.
        let stray = serde_json::json!({"allowedHttpHookUrls": ["http://localhost:9/v1/hooks/*"]});
        assert!(present(&stray, &env));
    }

    /// What `deckd run` asks before it decides the TCP door: our old HTTP
    /// hooks, on the file the installer reads. Only those count; a file
    /// Claude could not read counts as nothing.
    #[test]
    fn the_file_on_disk_carries_the_old_http_hooks_or_it_does_not() {
        let dir = TestDir::new("legacy-on-disk");
        let env = test_env(&dir);
        let path = settings_path(&env);
        assert!(!path.exists());
        assert!(!http_hooks_on_disk(&env), "no settings file at all");
        let write = |value: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        };
        write(&legacy_settings(&env));
        assert!(http_hooks_on_disk(&env));
        let one_hook = serde_json::json!({"hooks": {"PermissionRequest": [{"hooks": [{
            "type": "http", "url": "http://127.0.0.1:47201/v1/hooks/claude",
        }]}]}});
        write(&one_hook);
        assert!(http_hooks_on_disk(&env), "one http hook of ours is enough");
        write(&serde_json::json!({
            "model": "claude-haiku-4-5",
            "enabledPlugins": {"openwalkietalkie@openwalkietalkie": true},
        }));
        assert!(
            !http_hooks_on_disk(&env),
            "the plugin's records are not ours to count"
        );
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            !http_hooks_on_disk(&env),
            "Claude ignores a file it cannot parse"
        );
    }

    /// Every other leftover of the old installer still asks the Claude row
    /// for Repair ([`present`]), but none of them reaches the TCP door, so
    /// none keeps it open ([`http_hooks_present`]): the exec-form
    /// `SessionStart` hook (which reaches the daemon over its socket), the
    /// inventory, the chained `statusLine` and the allowlist pattern, each
    /// alone. Nor does somebody else's http hook, which is not ours.
    #[test]
    fn only_the_old_http_hooks_use_the_door() {
        let dir = TestDir::new("legacy-http-only");
        let env = test_env(&dir);
        let legacy = legacy_settings(&env);
        assert!(present(&legacy, &env) && http_hooks_present(&legacy));
        let leftovers = [
            ("the SessionStart command hook", serde_json::json!({"hooks": {
                "SessionStart": legacy["hooks"]["SessionStart"].clone(),
            }})),
            ("the inventory", serde_json::json!({
                "_openwalkietalkie": legacy["_openwalkietalkie"].clone(),
            })),
            ("the chained statusLine", serde_json::json!({
                "statusLine": legacy["statusLine"].clone(),
            })),
            ("the allowlist pattern", serde_json::json!({
                "allowedHttpHookUrls": ["http://127.0.0.1:47201/v1/hooks/*"],
            })),
        ];
        for (what, root) in &leftovers {
            assert!(present(root, &env), "{what} still asks for Repair");
            assert!(!http_hooks_present(root), "{what} posts nothing to the door");
        }
        let theirs = serde_json::json!({"hooks": {"Stop": [{"hooks": [{
            "type": "http", "url": "https://audit.example/hooks",
        }]}]}});
        assert!(!present(&theirs, &env) && !http_hooks_present(&theirs));
    }

    #[test]
    fn removal_takes_back_exactly_what_the_old_installer_wrote() {
        let dir = TestDir::new("legacy-removal");
        let env = test_env(&dir);
        std::fs::write(
            settings_path(&env),
            serde_json::to_vec_pretty(&legacy_settings(&env)).unwrap(),
        )
        .unwrap();
        let mut plan = Plan::default();
        let planned = plan_removal(&env, None, &mut plan)
            .unwrap()
            .expect("something to remove");
        let backup::Action::Write(bytes) = &planned.action else {
            panic!("the user's keys remain, so the file is rewritten, not removed");
        };
        let after: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            after,
            serde_json::json!({
                "model": "claude-haiku-4-5",
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/usr/local/bin/notify", "args": ["stopped"]}]}]},
                "allowedHttpHookUrls": ["https://audit.example/*"],
                "statusLine": {"type": "command", "command": "npx ccstatusline@latest"},
            })
        );
        assert!(!present(&after, &env));
        assert!(
            plan.notes.iter().any(|n| n.contains("pre-plugin")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn a_file_the_old_installer_created_is_removed_rather_than_emptied() {
        let dir = TestDir::new("legacy-created");
        let env = test_env(&dir);
        let mut only_ours = legacy_settings(&env);
        let object = only_ours.as_object_mut().unwrap();
        object.remove("model");
        object.remove("statusLine");
        object["hooks"]["Stop"].as_array_mut().unwrap().remove(0);
        object["allowedHttpHookUrls"] = serde_json::json!(["http://127.0.0.1:47201/v1/hooks/*"]);
        object["_openwalkietalkie"]["allowlist"]["added"] = serde_json::json!(true);
        object["_openwalkietalkie"]
            .as_object_mut()
            .unwrap()
            .remove("previousStatusLine");
        std::fs::write(
            settings_path(&env),
            serde_json::to_vec_pretty(&only_ours).unwrap(),
        )
        .unwrap();
        let mut plan = Plan::default();
        let planned = plan_removal(&env, None, &mut plan).unwrap().unwrap();
        assert!(
            matches!(planned.action, backup::Action::Remove),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn a_mixed_hook_group_is_refused_rather_than_dissected() {
        let dir = TestDir::new("legacy-mixed");
        let env = test_env(&dir);
        let mixed = serde_json::json!({"hooks": {"Stop": [{"hooks": [
            {"type": "http", "url": "http://127.0.0.1:47201/v1/hooks/claude", "timeout": 2},
            {"type": "command", "command": "/usr/local/bin/notify"}
        ]}]}});
        std::fs::write(
            settings_path(&env),
            serde_json::to_vec_pretty(&mixed).unwrap(),
        )
        .unwrap();
        let mut plan = Plan::default();
        let error = plan_removal(&env, None, &mut plan).unwrap_err();
        assert!(error.contains("next to 1 other handler"), "{error}");
    }

    #[test]
    fn nothing_of_ours_means_nothing_to_write() {
        let dir = TestDir::new("legacy-none");
        let env = test_env(&dir);
        let mut plan = Plan::default();
        assert!(
            plan_removal(&env, None, &mut plan).unwrap().is_none(),
            "no file at all"
        );
        std::fs::write(settings_path(&env), b"{\"model\": \"claude-haiku-4-5\"}\n").unwrap();
        assert!(plan_removal(&env, None, &mut plan).unwrap().is_none());
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn our_own_url_is_recognised_across_a_port_change_and_nobody_elses_is() {
        assert!(is_our_hook_url("http://127.0.0.1:47201/v1/hooks/claude"));
        assert!(is_our_hook_url("http://localhost:9/v1/hooks/claude"));
        assert!(!is_our_hook_url("https://127.0.0.1:47201/v1/hooks/claude"));
        assert!(!is_our_hook_url("http://10.0.0.5:47201/v1/hooks/claude"));
        assert!(!is_our_hook_url("http://127.0.0.1:47201/v1/hooks/codex"));
        assert!(is_our_hook_url_pattern("http://127.0.0.1:47201/v1/hooks/*"));
        assert!(is_our_hook_url_pattern("http://localhost/v1/hooks/*"));
        assert!(!is_our_hook_url_pattern("http://127.0.0.1:*/v1/hooks/*"));
        assert!(!is_our_hook_url_pattern("*"));
    }
}
