//! The Codex CLI target: `$CODEX_HOME/hooks.json` (design §5.2, §5.7,
//! corrections C12–C18). `config.toml` is touched only by `--trust`, which
//! records hook hashes under `[hooks.state]`, and by the migration that takes
//! back the `notify` and `tui.terminal_title` keys the pre-2026-10 installer
//! set.
//!
//! Hooks are the fallback path for Codex. Codex 0.161+ runs the TUI through a
//! shared local app-server daemon (`codex --no-daemon` opts out), and the
//! daemon client (`crate::adapters::codex_daemon`) learns every live thread,
//! its approvals and its questions from that daemon and steers it directly —
//! including sessions that started before deckd did. The hooks still serve a
//! `--no-daemon` session and an older CLI.
//!
//! Three measured facts shape every line of this module.
//!
//! **C12 — `command` is a string.** One array-form `command` anywhere in
//! `hooks.json` makes codex-cli 0.153.4 discard *the entire file* across every
//! event, reported only as a `warnings` string with `errors: []`. So the
//! command is a single shell-quoted string built with [`markers::shell_word`],
//! and the test that guards it asserts the parsed handler's `command` is a
//! JSON string, never a sequence.
//!
//! **C14a — hook trust is positional and deckd cannot compute it.** Trust
//! lives in `config.toml` under `[hooks.state."<REALPATH>:<event_snake>:<group>:<handler>"]`
//! with a single `trusted_hash` key. The key uses the *realpath* of
//! `hooks.json` (so keying off `$CODEX_HOME` verbatim writes a hash that never
//! matches under a macOS temp dir), and it is positional, so deckd **appends
//! its group at the end of each event's array** — inserting anywhere else
//! silently re-attributes a user's existing trust to a different handler.
//! deckd cannot derive the hash, so `--trust` asks `hooks/list` for the
//! `currentHash` of its own key and writes that. Without `--trust` nothing is
//! written and the installer prints the `/hooks` review step: forging the hash
//! silently would bypass exactly the review `/hooks` exists to provide (C14).
//!
//! **C15–C17 — `config.toml` keys deckd no longer sets.** `tui.terminal_title`
//! was a terminal-identity fallback the hook environment made redundant, and
//! `notify` duplicates what the `Stop` hook already carries
//! (`last_assistant_message`, `turn_id`, C17). An install removes both when an
//! older deckd wrote them (recognised by the receipt and, for `notify`, by the
//! binary it names) and otherwise leaves `config.toml` unwritten.
//! `features.hooks` is `stable true` by default and is **not** written (C16).
//!
//! `config.toml` is edited as *text*, line by line, so comments, key order
//! and formatting survive: the workspace pins `toml = 1.1.6`, which parses
//! and serialises but has no comment-preserving document model. Text editing
//! is why [`plan`] re-parses its own result before the bytes reach the
//! staging area: Codex reads this file whole, so one syntax error costs the
//! user every setting in it, not just the key deckd touched.
use super::backup::{self, FileSnapshot};
use super::markers;
use super::{Env, Plan, Report, Status, Target, Verb};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Every report-only hook.
pub const REPORT_TIMEOUT_S: u64 = 2;
/// `SessionEnd` and `Interrupt` are clamped to 3 s by Codex anyway.
pub const SESSION_END_TIMEOUT_S: u64 = 1;
pub const RECEIPT_SCHEMA: u64 = 1;
/// `tui.terminal_title`'s items. The id item is `thread-id`: there is no
/// `session_id` item in 0.153.4 (C15).
pub const TERMINAL_TITLE: [&str; 3] = ["activity", "project-name", "thread-id"];
/// How long `hooks/list` may take before `--trust` gives up.
const APP_SERVER_TIMEOUT: Duration = Duration::from_secs(25);

/// The events deckd installs, in the order they are written.
const EVENTS: &[(&str, u64)] = &[
    ("SessionStart", REPORT_TIMEOUT_S),
    ("UserPromptSubmit", REPORT_TIMEOUT_S),
    ("PreToolUse", REPORT_TIMEOUT_S),
    ("PermissionRequest", REPORT_TIMEOUT_S),
    ("Stop", REPORT_TIMEOUT_S),
    ("Interrupt", REPORT_TIMEOUT_S),
    ("SessionEnd", SESSION_END_TIMEOUT_S),
];

pub fn hooks_path(env: &Env) -> PathBuf {
    env.codex_dir().join("hooks.json")
}

pub fn config_path(env: &Env) -> PathBuf {
    env.codex_dir().join("config.toml")
}

pub fn files(env: &Env) -> Vec<PathBuf> {
    vec![hooks_path(env), config_path(env)]
}

/// `PermissionRequest` → `permission_request`, the spelling the trust key
/// uses (the same event is PascalCase in the file and camelCase on the wire).
pub fn event_snake(event: &str) -> String {
    let mut out = String::new();
    for (at, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if at != 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The `[hooks.state]` key of one of our handlers: the **realpath** of
/// `hooks.json`, the snake-case event, and the positions in the file.
pub fn trust_key(hooks_file: &Path, event: &str, group: usize, handler: usize) -> String {
    let real = std::fs::canonicalize(hooks_file).unwrap_or_else(|_| hooks_file.to_path_buf());
    format!(
        "{}:{}:{group}:{handler}",
        real.display(),
        event_snake(event)
    )
}

/// The `description` deckd writes into a `hooks.json` that has none. It is
/// a signpost for whoever opens the file, not a marker: ownership is
/// recognised by the command (C24), and uninstall takes this back out only
/// when install put it in.
pub const DESCRIPTION: &str = "OpenWalkieTalkie deck hooks (managed by `deckd integrations`)";

/// The one command string every handler runs (C12: a string, shell-quoted).
pub fn command(env: &Env) -> String {
    format!(
        "{} hook codex",
        markers::shell_word(&env.exe.display().to_string())
    )
}

/// The `hooks.json` keys deckd had to create, so uninstall takes back
/// exactly what install added and nothing the user wrote.
///
/// Both are recorded in `integrations.json` at install time and read back
/// at uninstall, for the same reason `created_tui_table` is: a file that
/// already carried a `description` of the user's own, or an empty `hooks`
/// map waiting to be filled, must come back with it. Absent from an older
/// receipt they default to `false` — "not ours, leave it" — which is the
/// only safe guess when the record is gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CreatedKeys {
    description: bool,
    hooks: bool,
}

impl CreatedKeys {
    fn from(receipt: Option<&serde_json::Value>) -> CreatedKeys {
        let flag = |name: &str| {
            receipt
                .and_then(|r| r.get(name))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        CreatedKeys {
            description: flag("created_description"),
            hooks: flag("created_hooks_key"),
        }
    }

    /// Either run created it.
    fn or(self, prior: CreatedKeys) -> CreatedKeys {
        CreatedKeys {
            description: self.description || prior.description,
            hooks: self.hooks || prior.hooks,
        }
    }
}

pub fn plan(env: &Env, verb: Verb, receipt: Option<&serde_json::Value>) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let hooks_file = hooks_path(env);
    let config_file = config_path(env);
    let hooks_snapshot = FileSnapshot::read(&hooks_file)?;
    let config_snapshot = FileSnapshot::read(&config_file)?;
    let root = super::parse_json_object(&hooks_snapshot)?;
    let created_file = receipt
        .and_then(|r| r.get("created_hooks_file"))
        .and_then(|c| c.as_bool())
        .unwrap_or(!hooks_snapshot.exists());

    let (next, positions, created_keys) = if verb.is_uninstall() {
        (
            unmerge_hooks(env, root, &mut plan, CreatedKeys::from(receipt))?,
            Vec::new(),
            CreatedKeys::default(),
        )
    } else {
        let (next, positions, created) = merge_hooks(env, root, &mut plan)?;
        // A re-install (`repair`, `--trust`, a second `install`) sees the
        // file the first one wrote, so it would compute "the description
        // was already there" and hand uninstall a receipt saying the key is
        // the user's. What deckd created stays deckd's until uninstall
        // takes it back.
        (next, positions, created.or(CreatedKeys::from(receipt)))
    };
    let hooks_bytes = if verb.is_uninstall() {
        match super::bytes_of_equivalent_backup(&env.backups(), "hooks.json", &next) {
            Some(bytes) => {
                plan.note("restores hooks.json byte for byte from its pre-install backup");
                bytes
            }
            None => super::render_json(&next)?,
        }
    } else {
        super::render_json(&next)?
    };
    let drop_file = verb.is_uninstall()
        && created_file
        && next
            .as_object()
            .is_some_and(|o| o.keys().all(|k| k == "description"));
    let hooks_write = if drop_file {
        plan.note(format!(
            "removes {} (deckd created it)",
            hooks_file.display()
        ));
        backup::Planned::remove(hooks_snapshot)
    } else {
        backup::Planned::write(hooks_snapshot, hooks_bytes)
    };
    plan.writes.push(hooks_write);

    let mut config_receipt = ConfigReceipt::from(receipt);
    let original = config_snapshot.text()?;
    let text = if verb.is_uninstall() {
        unmerge_config(env, &config_receipt, original.clone(), &mut plan)?
    } else {
        migrate_config(env, &mut config_receipt, original.clone(), &mut plan)?
    };
    let config_changes = text != original;
    // `config.toml` is edited as text, to keep the user's comments and key
    // order, so the last word on whether the edit is sound belongs to the
    // parser. Codex reads this file whole: one syntax error and *none* of the
    // user's configuration is read, not just the key deckd touched. Refuse
    // before the bytes reach the staging area rather than publish a file
    // Codex cannot read.
    if let Err(e) = toml::from_str::<toml::Table>(&text) {
        return Err(format!(
            "deckd's edit of {} would not parse as TOML ({e}); nothing was written",
            config_file.display()
        ));
    }
    if verb.trusts() {
        // The hash covers the hooks.json Codex can *see*, so the trust table
        // cannot be part of this plan: it is written by `record_trust` after
        // the new hooks.json has been published (C14a).
        plan.note("--trust: the [hooks.state] hashes are read from hooks/list and written once the new hooks.json is in place");
    } else if !verb.is_uninstall() {
        plan.step(
            "Codex will NOT run these hooks until you review them: open `codex`, run `/hooks`, \
             press `t` to trust (or answer the \"Hooks need review\" prompt at startup). `deckd \
             integrations install codex --trust` writes the hash for you instead, which skips \
             that review.",
        );
    }
    // `config.toml` is written only when an edit is actually made: an install
    // on a machine with no pre-plugin keys to take back never creates or
    // rewrites the file (the hooks live in hooks.json; trust is `--trust`'s
    // own later write).
    if config_changes {
        plan.writes
            .push(backup::Planned::write(config_snapshot, text.into_bytes()));
    }
    if !verb.is_uninstall() {
        plan.receipt = Some(config_receipt.into_value(
            env,
            &hooks_file,
            created_file,
            created_keys,
            &positions,
        ));
    }
    Ok(plan)
}

/// Merge our groups into `hooks.json`, appending each at the end of its
/// event's array when it is new and rewriting it **in place** when it is
/// already there (C14a ii — trust is positional). Returns the file and the
/// index our group ended up at, per event.
#[allow(clippy::type_complexity)]
fn merge_hooks(
    env: &Env,
    mut root: serde_json::Value,
    plan: &mut Plan,
) -> Result<(serde_json::Value, Vec<(String, usize)>, CreatedKeys), String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "hooks.json does not hold a JSON object".to_string())?;
    let mut created = CreatedKeys {
        description: !object.contains_key("description"),
        hooks: !object.contains_key("hooks"),
    };
    if created.description {
        object.insert("description".to_string(), DESCRIPTION.into());
    }
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "the existing `hooks` value must be an object".to_string())?;
    let command = command(env);
    let mut positions = Vec::new();
    for (event, timeout) in EVENTS {
        let array = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("the existing {event} hooks value must be an array"))?;
        let mine = find_ours(array, env)?;
        let foreign = array.len() - mine.len();
        let group = serde_json::json!({
            "hooks": [{"type": "command", "command": command, "timeout": timeout}],
        });
        // A Codex trust hash is keyed on a group's **index**
        // (`<realpath>:<event>:<group>:<handler>`, C14a ii), so re-writing
        // our group must never move anyone else's: replace ours where it
        // already sits, and only append when there is none. Stripping ours
        // first and pushing at the end would shift every foreign group that
        // sat after it down one, handing it a hash that is not its own —
        // after which Codex reads it `untrusted`/`modified` and skips it in
        // silence.
        let at = match mine.first().copied() {
            Some(at) => {
                array[at] = group;
                for extra in mine.iter().skip(1).rev() {
                    array.remove(*extra);
                }
                if mine.len() > 1 {
                    plan.conflict(format!(
                        "{event} carried {} copies of deckd's own hook group; the first is kept \
                         in place and the rest removed. Any group that sat after a removed copy \
                         moves up, so re-review {event} in `/hooks`",
                        mine.len()
                    ));
                }
                at
            }
            None => {
                array.push(group);
                array.len() - 1
            }
        };
        if foreign > 0 {
            plan.conflict(format!(
                "{event} already has {foreign} hook group(s) from somewhere else; deckd {} and \
                 never moves theirs — a trust hash is keyed on a group's index, so shifting one \
                 silently re-points it and Codex then skips that hook without a word (C14a)",
                if mine.is_empty() {
                    "appends after them"
                } else {
                    "rewrites its own group where it already sits"
                }
            ));
        }
        positions.push((event.to_string(), at));
    }
    plan.note(format!(
        "adds {} Codex hook groups running `{command}` (one shell-quoted string — an array would \
         make Codex discard every hook in the file)",
        EVENTS.len()
    ));
    // A `hooks` map that was already there stays the user's, however it
    // was spelled: only a key this run had to invent is one uninstall may
    // take away again.
    created.hooks &= !positions.is_empty();
    Ok((root, positions, created))
}

fn unmerge_hooks(
    env: &Env,
    mut root: serde_json::Value,
    plan: &mut Plan,
    created: CreatedKeys,
) -> Result<serde_json::Value, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "hooks.json does not hold a JSON object".to_string())?;
    let mut removed = 0usize;
    let mut displaced: Vec<String> = Vec::new();
    if let Some(hooks) = object.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in &events {
            let Some(array) = hooks.get_mut(event).and_then(|v| v.as_array_mut()) else {
                continue;
            };
            let before = array.len();
            // Taking an element out is the one edit that cannot leave the
            // indexes after it alone, and a Codex trust hash is keyed on the
            // index (C14a ii). Uninstall has no way around that, so it names
            // the events whose later groups move and tells the user to review
            // them again.
            let mine = find_ours(array, env)?;
            if let Some(first) = mine.first().copied() {
                if (first + 1..array.len()).any(|at| !mine.contains(&at)) {
                    displaced.push(event.clone());
                }
            }
            for at in mine.iter().rev() {
                array.remove(*at);
            }
            removed += before - array.len();
        }
        let empty: Vec<String> = hooks
            .iter()
            .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
            .map(|(k, _)| k.clone())
            .collect();
        for key in empty {
            hooks.remove(&key);
        }
        if hooks.is_empty() && created.hooks {
            object.remove("hooks");
        }
    }
    // The `description` goes back out only when install put it in: a file
    // that already carried one of the user's own keeps it.
    if created.description
        && object
            .get("description")
            .and_then(serde_json::Value::as_str)
            == Some(DESCRIPTION)
    {
        object.remove("description");
    }
    plan.note(format!("removes {removed} Codex hook groups"));
    if !displaced.is_empty() {
        plan.step(format!(
            "another hook group sits after deckd's on {}: removing ours moves it up one index, \
             and a Codex trust hash is keyed on that index (C14a), so review {} in `/hooks` \
             afterwards or Codex will skip {} in silence",
            displaced.join(", "),
            if displaced.len() == 1 {
                "that event"
            } else {
                "those events"
            },
            if displaced.len() == 1 {
                "that hook"
            } else {
                "those hooks"
            },
        ));
    }
    Ok(root)
}

/// Where our own groups sit in one event's array, **without touching it**.
/// `Err` for a group that mixes one of our handlers with someone else's:
/// that one is the user's to untangle, not ours to rewrite.
fn find_ours(array: &[serde_json::Value], env: &Env) -> Result<Vec<usize>, String> {
    let mut found = Vec::new();
    for (at, group) in array.iter().enumerate() {
        let handlers = group
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let ours = handlers.iter().filter(|h| is_ours(h, env)).count();
        if ours == 0 {
            continue;
        }
        if ours != handlers.len() {
            return Err(format!(
                "a Codex hook group holds a deckd handler next to {} other handler(s); remove it \
                 by hand before installing",
                handlers.len() - ours
            ));
        }
        found.push(at);
    }
    Ok(found)
}

/// Ours when the command's first shell word is a deckd binary and the rest
/// is exactly `hook codex`. Suffix-exact, never a substring match: an `echo
/// /path/deckd hook codex` in the user's own hook must survive.
fn is_ours(handler: &serde_json::Value, env: &Env) -> bool {
    runs_deckd(handler, &env.exe)
}

/// [`is_ours`] against the binary deckd runs as, for a caller with no
/// installer environment (`managed_groups`).
fn runs_deckd(handler: &serde_json::Value, exe: &Path) -> bool {
    if handler.get("type").and_then(|t| t.as_str()) != Some("command") {
        return false;
    }
    let Some(command) = handler.get("command").and_then(|c| c.as_str()) else {
        return false;
    };
    command_runs_deckd(command, exe)
}

/// The same ownership test against a bare command string, which is all
/// `hooks/list` reports back (it has no `type` field of its own).
fn is_ours_command(command: &str, env: &Env) -> bool {
    command_runs_deckd(command, &env.exe)
}

fn command_runs_deckd(command: &str, exe: &Path) -> bool {
    markers::split_command(command).is_some_and(|(binary, rest)| {
        rest == "hook codex" && markers::is_managed_binary(&binary, exe)
    })
}

/// How many of `hooks_file`'s hook groups hold a deckd handler: the entries
/// of the leftover Codex setup the Footprint page lists (`care.footprint`),
/// counted the way [`status`] counts them. `None` when the file is absent,
/// unreadable or not a JSON object.
pub fn managed_groups(hooks_file: &Path, exe: &Path) -> Option<usize> {
    let snapshot = FileSnapshot::read(hooks_file).ok()?;
    if !snapshot.exists() {
        return None;
    }
    let root = super::parse_json_object(&snapshot).ok()?;
    Some(root["hooks"].as_object().map_or(0, |hooks| {
        hooks
            .values()
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .filter(|group| {
                group["hooks"]
                    .as_array()
                    .is_some_and(|handlers| handlers.iter().any(|h| runs_deckd(h, exe)))
            })
            .count()
    }))
}

/// What deckd wrote into `config.toml`, so uninstall can take back exactly
/// that and nothing else.
#[derive(Clone, Debug, Default)]
struct ConfigReceipt {
    terminal_title: bool,
    /// Did deckd have to create the `[tui]` header itself? Only then may
    /// uninstall take the header away again with the key.
    created_tui_table: bool,
    notify: bool,
    trust_keys: Vec<String>,
}

impl ConfigReceipt {
    fn from(receipt: Option<&serde_json::Value>) -> ConfigReceipt {
        let Some(receipt) = receipt else {
            return ConfigReceipt::default();
        };
        ConfigReceipt {
            terminal_title: receipt["wrote_terminal_title"].as_bool().unwrap_or(false),
            created_tui_table: receipt["created_tui_table"].as_bool().unwrap_or(false),
            notify: receipt["wrote_notify"].as_bool().unwrap_or(false),
            trust_keys: receipt["trust_keys"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|k| k.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn into_value(
        self,
        env: &Env,
        hooks_file: &Path,
        created_file: bool,
        created_keys: CreatedKeys,
        positions: &[(String, usize)],
    ) -> serde_json::Value {
        serde_json::json!({
            "schema": RECEIPT_SCHEMA,
            "deckd": env.exe.display().to_string(),
            "hooks_file": hooks_file.display().to_string(),
            "created_hooks_file": created_file,
            // What uninstall may take back out of a file it did not create
            // (see `CreatedKeys`).
            "created_description": created_keys.description,
            "created_hooks_key": created_keys.hooks,
            "entries": positions
                .iter()
                .map(|(event, group)| format!("{event}/{group}"))
                .collect::<Vec<String>>(),
            "wrote_terminal_title": self.terminal_title,
            "created_tui_table": self.created_tui_table,
            "wrote_notify": self.notify,
            "trust_keys": self.trust_keys,
        })
    }
}

/// Install no longer writes anything into `config.toml`. The pre-2026-10
/// installer set `tui.terminal_title` (a terminal-identity fallback the
/// hook environment made redundant) and `notify` (redundant with the `Stop`
/// hook, C17), and a machine that still carries those has them taken back
/// out here, under the same ownership rules as uninstall: `notify` by the
/// binary it names, `terminal_title` only when the receipt says deckd wrote
/// it (its value alone is too plain to claim).
fn migrate_config(
    env: &Env,
    receipt: &mut ConfigReceipt,
    text: String,
    plan: &mut Plan,
) -> Result<String, String> {
    let doc: toml::Table = toml::from_str(&text)
        .map_err(|e| format!("{} is not valid TOML: {e}", config_path(env).display()))?;
    let mut text = text;
    if receipt.terminal_title {
        if let Some(value) = toml_get(&doc, &["tui", "terminal_title"]) {
            if is_our_terminal_title(value) {
                text = toml_remove_key(&text, &["tui"], "terminal_title");
                if receipt.created_tui_table {
                    text = toml_remove_empty_table(&text, &["tui"]);
                }
                plan.note(
                    "removes tui.terminal_title, which an older deckd set: the terminal is \
                     identified from the hook environment now",
                );
            }
        }
        receipt.terminal_title = false;
        receipt.created_tui_table = false;
    }
    if toml_get(&doc, &["notify"]).is_some_and(|value| is_our_notify(value, env)) {
        text = toml_remove_key(&text, &[], "notify");
        plan.note(
            "removes notify, which an older deckd set: the Stop hook carries the same \
             last_assistant_message and turn_id (C17)",
        );
    }
    receipt.notify = false;
    // C16: `features.hooks` is `stable true` by default and is never written.
    Ok(text)
}

/// Is this `notify` deckd's own entry? Recognised by the binary and the
/// arguments, like every other ownership check here — never by a marker.
fn is_our_notify(value: &toml::Value, env: &Env) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let words: Vec<&str> = items.iter().filter_map(|i| i.as_str()).collect();
    words.len() == 3
        && markers::is_managed_binary(words[0], &env.exe)
        && words[1] == "hook"
        && words[2] == "codex-notify"
}

/// Take back the `config.toml` keys install wrote — but only the ones that
/// are *still deckd's*.
///
/// The receipt flag says "install wrote this key", not "the value on disk is
/// still the one install wrote". `merge_config` already refuses to replace an
/// occupied `notify` (C17); uninstall has to be the same shape, or a key the
/// user took back over after the install is deleted in silence. So each key
/// is re-checked against the parsed document with the same ownership test
/// that recognises it on the way in, and a key someone else has since claimed
/// is left exactly where it is.
fn unmerge_config(
    env: &Env,
    receipt: &ConfigReceipt,
    text: String,
    plan: &mut Plan,
) -> Result<String, String> {
    let doc: toml::Table = toml::from_str(&text)
        .map_err(|e| format!("{} is not valid TOML: {e}", config_path(env).display()))?;
    let mut text = text;
    if receipt.terminal_title {
        match toml_get(&doc, &["tui", "terminal_title"]) {
            Some(value) if is_our_terminal_title(value) => {
                text = toml_remove_key(&text, &["tui"], "terminal_title");
                if receipt.created_tui_table {
                    text = toml_remove_empty_table(&text, &["tui"]);
                }
                plan.note("removes tui.terminal_title, which deckd set");
            }
            Some(_) => plan.note(
                "leaves tui.terminal_title alone: it no longer holds the value deckd wrote, so \
                 something else has set it since",
            ),
            None => plan.note("tui.terminal_title is already gone; deckd removes nothing"),
        }
    }
    if receipt.notify {
        match toml_get(&doc, &["notify"]) {
            Some(value) if is_our_notify(value, env) => {
                text = toml_remove_key(&text, &[], "notify");
                plan.note("removes notify, which deckd set");
            }
            Some(_) => plan.note(
                "leaves notify alone: it no longer points at deckd, so something else has set it \
                 since deckd wrote it",
            ),
            None => plan.note("notify is already gone; deckd removes nothing"),
        }
    }
    for key in &receipt.trust_keys {
        text = toml_remove_table(&text, &format!("hooks.state.{}", toml_string(key)));
    }
    if !receipt.trust_keys.is_empty() {
        text = toml_remove_empty_table(&text, &["hooks", "state"]);
        plan.note(format!(
            "removes {} [hooks.state] entries deckd wrote with --trust",
            receipt.trust_keys.len()
        ));
    }
    Ok(text)
}

/// Is this `tui.terminal_title` the exact list deckd writes (C15)? Like every
/// other ownership check here it looks at the value, never at a marker.
fn is_our_terminal_title(value: &toml::Value) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() == TERMINAL_TITLE.len()
            && items
                .iter()
                .zip(TERMINAL_TITLE)
                .all(|(item, want)| item.as_str() == Some(want))
    })
}

/// What a post-apply `--trust` step did.
#[derive(Debug, Default)]
pub struct TrustOutcome {
    pub changed: Vec<std::path::PathBuf>,
    pub backups: Vec<std::path::PathBuf>,
    pub notes: Vec<String>,
    pub steps: Vec<String>,
    pub trust_keys: Vec<String>,
}

/// `--trust` (C14), run **after** the new `hooks.json` is published.
///
/// The hash covers the file Codex can see, so asking `hooks/list` while the
/// old file was still on disk would record a hash for the wrong definition —
/// or, on a first install, for a file that does not exist yet. So this is a
/// second, separate write of `config.toml`.
///
/// It trusts exactly the rows `hooks/list` says are ours, by their reported
/// `key` and `command`, rather than by positions this process computed: the
/// key is positional *and* realpath-based (C14a), and taking it from the
/// server is the only way to be sure the hash lands on our handler and not on
/// someone else's.
pub fn record_trust(
    env: &Env,
    receipt: Option<&serde_json::Value>,
) -> Result<TrustOutcome, String> {
    let listed = hooks_list(env).map_err(|why| why.to_string())?;
    let mine: Vec<&HookMeta> = listed
        .iter()
        .filter(|hook| is_ours_command(&hook.command, env))
        .collect();
    if mine.is_empty() {
        return Err(format!(
            "hooks/list reported {} hook(s) but none of them is deckd's; nothing was trusted",
            listed.len()
        ));
    }
    let config_file = config_path(env);
    let snapshot = FileSnapshot::read(&config_file)?;
    let mut text = snapshot.text()?;
    let mut keys = Vec::new();
    for hook in &mine {
        text = toml_set_trust(&text, &hook.key, &hook.current_hash);
        keys.push(hook.key.clone());
    }
    let applied = backup::apply(
        vec![backup::Planned::write(snapshot, text.into_bytes())],
        &env.backups(),
    )?;
    let mut outcome = TrustOutcome {
        changed: applied.changed,
        backups: applied.backups,
        trust_keys: keys,
        ..TrustOutcome::default()
    };
    outcome.notes.push(format!(
        "--trust: recorded {} [hooks.state] hash(es) reported by hooks/list; deckd never computes \
         them itself",
        outcome.trust_keys.len()
    ));
    outcome.steps.push(
        "this skipped the /hooks review. Any later change to deckd's path, timeout or matcher \
         re-breaks the hash as `modified` and the hooks stop firing silently — re-run `deckd \
         integrations repair codex --trust` after an upgrade."
            .into(),
    );
    // Carry the rest of the receipt forward so uninstall still knows which
    // config.toml keys deckd set.
    let _ = receipt;
    Ok(outcome)
}

/// One row of the app-server's `hooks/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookMeta {
    pub key: String,
    /// The shell string Codex will run. Ownership is decided from this, never
    /// from a position this process guessed.
    pub command: String,
    pub current_hash: String,
    pub trust_status: String,
    pub enabled: bool,
    pub source_path: String,
}

/// Why `hooks/list` produced no rows. The two cases mean opposite things to
/// `status`: a codex that cannot be run leaves deckd guessing from the files,
/// while a codex that *answered with warnings* has told us the hooks are dead
/// (C12's whole-file discard).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HooksListError {
    /// codex is on neither `PATH` nor [`super::cli_dirs`], did not start,
    /// or did not answer in time.
    Unavailable(String),
    /// codex answered, and its `warnings` say the hooks config was discarded.
    Discarded(String),
}

impl std::fmt::Display for HooksListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HooksListError::Unavailable(why) | HooksListError::Discarded(why) => f.write_str(why),
        }
    }
}

/// `codex app-server --listen stdio://` → `initialize`, `initialized`,
/// `hooks/list`. There is no `codex hooks list` subcommand in 0.153.4 and no
/// RPC that *writes* trust, so this is the only way to read `currentHash`.
pub fn hooks_list(env: &Env) -> Result<Vec<HookMeta>, HooksListError> {
    let codex = super::find_cli("codex").ok_or_else(|| {
        HooksListError::Unavailable(
            "codex is neither on PATH nor where its installer puts it, so deckd cannot read the \
             hook hashes it would trust"
                .to_string(),
        )
    })?;
    let cwd = std::env::temp_dir();
    let mut child = super::cli_command(&codex)
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", env.codex_dir())
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| HooksListError::Unavailable(format!("cannot run {}: {e}", codex.display())))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| HooksListError::Unavailable("no stdin on codex app-server".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HooksListError::Unavailable("no stdout on codex app-server".to_string()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let send = |stdin: &mut std::process::ChildStdin, value: serde_json::Value| {
        let _ = writeln!(stdin, "{value}");
        let _ = stdin.flush();
    };
    send(
        &mut stdin,
        serde_json::json!({
            "method": "initialize", "id": 0,
            "params": {
                "clientInfo": {"name": "deckd", "title": "OpenWalkieTalkie", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true},
            },
        }),
    );
    let deadline = Instant::now() + APP_SERVER_TIMEOUT;
    let mut answer = None;
    let mut asked = false;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(left.min(Duration::from_secs(1))) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value["id"] == 0 && !asked {
            asked = true;
            send(
                &mut stdin,
                serde_json::json!({"method": "initialized", "params": {}}),
            );
            send(
                &mut stdin,
                serde_json::json!({
                    "method": "hooks/list", "id": 1,
                    "params": {"cwds": [cwd.display().to_string()]},
                }),
            );
        }
        if value["id"] == 1 {
            answer = Some(value);
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let answer = answer.ok_or_else(|| {
        HooksListError::Unavailable(format!(
            "codex app-server did not answer hooks/list within {} s",
            APP_SERVER_TIMEOUT.as_secs()
        ))
    })?;
    parse_hooks_list(&answer)
}

/// The `hooks/list` result as [`HookMeta`] rows. A non-empty `warnings` is
/// fatal here: C12's whole-file discard shows up *only* there, with
/// `hooks: []` and `errors: []`.
pub fn parse_hooks_list(answer: &serde_json::Value) -> Result<Vec<HookMeta>, HooksListError> {
    if let Some(error) = answer.get("error") {
        return Err(HooksListError::Unavailable(format!(
            "codex hooks/list failed: {error}"
        )));
    }
    let data = answer["result"]["data"].as_array().ok_or_else(|| {
        HooksListError::Unavailable("codex hooks/list returned no data".to_string())
    })?;
    let mut rows = Vec::new();
    for entry in data {
        if let Some(warnings) = entry["warnings"].as_array() {
            if !warnings.is_empty() {
                return Err(HooksListError::Discarded(format!(
                    "codex reported {} hooks-config warning(s) — the whole file may have been \
                     discarded: {}",
                    warnings.len(),
                    warnings
                        .iter()
                        .filter_map(|w| w.as_str())
                        .collect::<Vec<&str>>()
                        .join("; ")
                )));
            }
        }
        for hook in entry["hooks"].as_array().unwrap_or(&Vec::new()) {
            rows.push(HookMeta {
                key: hook["key"].as_str().unwrap_or_default().to_string(),
                command: hook["command"].as_str().unwrap_or_default().to_string(),
                current_hash: hook["currentHash"].as_str().unwrap_or_default().to_string(),
                trust_status: hook["trustStatus"].as_str().unwrap_or_default().to_string(),
                enabled: hook["enabled"].as_bool().unwrap_or(false),
                source_path: hook["sourcePath"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    Ok(rows)
}

pub fn status(env: &Env) -> Report {
    let mut report = Report::new(Target::Codex, Status::Absent);
    report.files = files(env);
    report.backups = backup::list_backups(&env.backups(), "hooks.json");
    report
        .backups
        .extend(backup::list_backups(&env.backups(), "config.toml"));
    report.detected = super::detect_cli(env, "codex");
    report.limits = limits();
    let hooks_file = hooks_path(env);
    let snapshot = match FileSnapshot::read(&hooks_file) {
        Ok(snapshot) => snapshot,
        Err(why) => return conflict(report, why),
    };
    let root = match super::parse_json_object(&snapshot) {
        Ok(root) => root,
        Err(why) => return conflict(report, why),
    };
    let ours: Vec<(String, usize)> = root["hooks"]
        .as_object()
        .map(|hooks| {
            hooks
                .iter()
                .flat_map(|(event, groups)| {
                    groups
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                        .filter(|(_, group)| {
                            group["hooks"]
                                .as_array()
                                .is_some_and(|hs| hs.iter().any(|h| is_ours(h, env)))
                        })
                        .map(|(at, _)| (event.clone(), at))
                        .collect::<Vec<(String, usize)>>()
                })
                .collect()
        })
        .unwrap_or_default();
    if ours.is_empty() {
        report.detail = Some(format!("no deckd hooks in {}", hooks_file.display()));
        return report;
    }
    let embedded = embedded_binaries(&root);
    let config_text = FileSnapshot::read(&config_path(env))
        .and_then(|s| s.text())
        .unwrap_or_default();
    let recorded = ours
        .iter()
        .filter(|(event, group)| {
            let key = trust_key(&hooks_file, event, *group, 0);
            toml_has_table(&config_text, &format!("hooks.state.{}", toml_string(&key)))
        })
        .count();
    // C14a(iii) and C18. A `[hooks.state]` table proves only that *a* hash was
    // recorded under that key once. deckd cannot compute the hash itself, so
    // it cannot tell a current one from a stale one — and a stale one is the
    // silent outage this status exists to surface: codex reports the hook
    // `modified` and skips it, with nothing on screen. `hooks/list` is the
    // only thing that knows, and it is also the only place C12's whole-file
    // discard shows up. So ask codex whenever this call may spawn one.
    let live = env.probe_clis.then(|| hooks_list(env));
    let mut fell_back_to_files = None;
    let live_verdict = match &live {
        Some(Ok(rows)) => trust_from_codex(env, rows),
        Some(Err(HooksListError::Discarded(why))) => Some((Status::HooksNotFiring, why.clone())),
        Some(Err(HooksListError::Unavailable(why))) => {
            fell_back_to_files = Some(why.clone());
            None
        }
        None => {
            fell_back_to_files = Some("this status call may not run the CLIs".to_string());
            None
        }
    };
    // Only when codex could not answer: the presence of the table is all
    // there is to go on.
    let file_verdict = fell_back_to_files
        .as_ref()
        .and_then(|_| trust_from_files(recorded, ours.len()));
    report.status = if embedded.iter().any(|path| Path::new(path) != env.exe) {
        Status::PathDrift
    } else if let Some((status, detail)) = live_verdict {
        report.detail = Some(detail);
        status
    } else if let Some(status) = file_verdict {
        status
    } else {
        let mut plan = Plan::default();
        match merge_hooks(env, root, &mut plan) {
            Ok((desired, _, _)) => {
                match super::render_json(&desired).ok().as_deref() == snapshot.bytes() {
                    true => Status::Installed,
                    false => Status::Outdated,
                }
            }
            Err(why) => {
                report.detail = Some(why);
                Status::Conflict
            }
        }
    };
    if let Some(why) = fell_back_to_files {
        report.limits.push(format!(
            "codex was not asked for the live hook trust ({why}), so this row fell back to the \
             presence of a [hooks.state] table in config.toml: deckd cannot compute a hook's hash \
             itself (C14a), so a recorded hash that no longer matches the handler still reads as \
             `installed` here while codex skips the hook"
        ));
    }
    if report.detail.is_none() {
        report.detail = Some(match report.status {
            Status::Untrusted => format!(
                "{} deckd hook(s) written, none trusted: run `/hooks` in codex (or install with \
                 --trust). Codex skips an untrusted hook silently.",
                ours.len()
            ),
            _ => format!(
                "{} deckd hook group(s) in {}",
                ours.len(),
                hooks_file.display()
            ),
        });
    }
    report
}

/// Trust as far as the files can tell it (C14): how many of deckd's groups
/// have a `[hooks.state]` table. `None` means nothing here contradicts
/// "installed" — which is *not* the same as "codex runs them", hence the
/// extra `limits` line whenever this is what answered.
fn trust_from_files(recorded: usize, total: usize) -> Option<Status> {
    match recorded {
        // C14: written but not reviewed. Codex skips it in silence.
        0 => Some(Status::Untrusted),
        n if n < total => Some(Status::Modified),
        _ => None,
    }
}

/// `hooks/list` mapped onto the status vocabulary (C14, C14a, C18): what
/// codex itself says it will run. `None` means codex sees every deckd hook as
/// trusted and enabled, so the answer is the file comparison instead.
fn trust_from_codex(env: &Env, rows: &[HookMeta]) -> Option<(Status, String)> {
    let mine: Vec<&HookMeta> = rows
        .iter()
        .filter(|hook| is_ours_command(&hook.command, env))
        .collect();
    if mine.is_empty() {
        // The file names deckd's hooks and codex reports none of them: they
        // are on disk and dead, which is exactly what C18 says the check must
        // catch rather than infer from a missing SessionStart.
        return Some((
            Status::HooksNotFiring,
            format!(
                "hooks.json names deckd's hooks but codex hooks/list reports {} hook(s), none of \
                 them deckd's — codex is not running them",
                rows.len()
            ),
        ));
    }
    let modified = mine
        .iter()
        .filter(|hook| hook.trust_status == "modified")
        .count();
    if modified > 0 {
        return Some((
            Status::Modified,
            format!(
                "codex reports {modified} of {} deckd hook(s) as `modified`: the recorded hash no \
                 longer matches the command, so codex skips them silently. Re-review them in \
                 `/hooks`, or run `deckd integrations repair codex --trust`.",
                mine.len()
            ),
        ));
    }
    let untrusted = mine
        .iter()
        .filter(|hook| hook.trust_status != "trusted")
        .count();
    if untrusted > 0 {
        return Some((
            // All of them is the state a fresh install leaves behind; some of
            // them is the same silent outage as a stale hash.
            match untrusted == mine.len() {
                true => Status::Untrusted,
                false => Status::Modified,
            },
            format!(
                "codex reports {untrusted} of {} deckd hook(s) as not trusted: they do not run \
                 until they are reviewed in `/hooks` (or installed with --trust). Codex skips an \
                 untrusted hook silently.",
                mine.len()
            ),
        ));
    }
    let disabled = mine.iter().filter(|hook| !hook.enabled).count();
    if disabled > 0 {
        return Some((
            Status::Paused,
            format!(
                "codex reports {disabled} of {} deckd hook(s) as disabled: trusted, but switched \
                 off in `/hooks`.",
                mine.len()
            ),
        ));
    }
    None
}

/// The `notify` key when it belongs to somebody else: the note `status`
/// carries for as long as it does.
///
/// `notify` is single-valued and has no merge semantics, so deckd never
/// replaces one it did not write (`merge_config`). Ours is recognised the
/// way every other entry here is — by the binary and the arguments, never
/// by a marker — and the foreign command itself is not echoed: a status
/// row is not the place to publish a line out of somebody's config.
fn conflict(mut report: Report, why: String) -> Report {
    report.status = Status::Conflict;
    report.detail = Some(why);
    report
}

/// Every deckd binary path the file names. `status` reports `path_drift` when
/// *any* of them is not this executable: reading only the first would call a
/// half-upgraded file healthy, and on Codex a stale path is not merely
/// cosmetic — it breaks the hook's trust hash and the hooks stop firing.
fn embedded_binaries(root: &serde_json::Value) -> Vec<String> {
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
                let Some(command) = handler.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                if let Some((binary, rest)) = markers::split_command(command) {
                    if rest == "hook codex" {
                        out.push(binary);
                    }
                }
            }
        }
    }
    out
}

/// What this status cannot tell you about Codex.
fn limits() -> Vec<String> {
    vec![
        "trust and enablement come from codex's own `hooks/list` (trustStatus, enabled, \
         warnings) whenever codex can be run, because deckd cannot compute a hook's hash itself \
         (C14a) and only codex tells `trusted` from `modified` or sees the warnings that mean the \
         whole hooks.json was discarded (C12, C18)"
            .into(),
        "the absence of a SessionStart is NOT evidence that hooks are dead: SessionStart fires on \
         a session's first turn, not when it opens (C18), so an idle session legitimately \
         produces none"
            .into(),
        "trust is positional (<event>:<group>:<handler>) and keyed on the REALPATH of hooks.json; \
         any change to deckd's path, timeout, matcher or statusMessage re-breaks it as `modified` \
         and the hooks stop firing with no notice (C14a)"
            .into(),
    ]
}

// ---------------------------------------------------------------------------
// A very small TOML text editor.
//
// `toml = 1.1.6` (the workspace pin) parses and serialises but has no
// comment-preserving document model, and re-serialising `~/.codex/config.toml`
// would throw away the user's comments and key order. Every edit deckd makes
// is one key or one sub-table, so they are made as line edits against the
// original text and *checked* against the parsed document.
// ---------------------------------------------------------------------------

/// The value at a dotted path, if the document defines it.
fn toml_get<'a>(doc: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut table = doc;
    for segment in parents {
        table = table.get(*segment)?.as_table()?;
    }
    table.get(*last)
}

/// A TOML basic string, for a key that holds a path.
fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A bare key if it can be one, else a quoted key.
fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_string()
    } else {
        toml_string(key)
    }
}

fn header_of(path: &[&str]) -> String {
    format!(
        "[{}]",
        path.iter()
            .map(|s| toml_key(s))
            .collect::<Vec<String>>()
            .join(".")
    )
}

/// Is there an explicit `[header]` line? (A table created only implicitly by
/// `[a.b]` has none, and TOML allows defining the super-table afterwards.)
fn toml_has_table(text: &str, header: &str) -> bool {
    let want = format!("[{header}]");
    text.lines().any(|line| strip_comment(line).trim() == want)
}

fn strip_comment(line: &str) -> &str {
    // Good enough for a header line: a `#` inside a quoted key is rare and
    // only costs us an extra appended table, never a corrupt file.
    match line.find('#') {
        Some(at) if !line[..at].contains('"') => &line[..at],
        _ => line,
    }
}

/// Set `key = value` inside `table`, creating the table if need be. An
/// existing key of the same name is replaced in place. The flag says whether
/// a `[table]` header had to be created, which is what lets uninstall take
/// the header away again without touching one the user wrote.
fn toml_set_key(text: &str, table: &[&str], key: &str, value: &str) -> (String, bool) {
    let line = format!("{} = {value}", toml_key(key));
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (start, end) = table_span(&lines, table);
    if let Some(start) = start {
        let key_line = format!("{} =", toml_key(key));
        for at in start..end {
            if strip_comment(&lines[at])
                .trim_start()
                .starts_with(&key_line)
            {
                lines[at] = line;
                return (join(lines, text), false);
            }
        }
        // Append at the end of the table's own keys, before the blank lines
        // that separate it from the next header.
        let mut at = end;
        while at > start && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, line);
        return (join(lines, text), false);
    }
    // No such table yet: the root goes before the first header, a named
    // table is appended (TOML allows a super-table after its sub-tables).
    if table.is_empty() {
        let mut at = lines
            .iter()
            .position(|l| strip_comment(l).trim_start().starts_with('['))
            .unwrap_or(lines.len());
        while at > 0 && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, line);
        return (join(lines, text), false);
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(header_of(table));
    lines.push(line);
    (join(lines, text), true)
}

/// Drop a `[table]` header deckd created once its last key is gone, together
/// with the blank line that separated it. A table that still holds anything —
/// a key, a comment — is left alone.
fn toml_remove_empty_table(text: &str, table: &[&str]) -> String {
    let want = header_of(table);
    let lines: Vec<String> = text.lines().map(String::from).collect();
    let Some(at) = lines
        .iter()
        .position(|line| strip_comment(line).trim() == want)
    else {
        return text.to_string();
    };
    let (Some(start), end) = table_span(&lines, table) else {
        return text.to_string();
    };
    if lines[start..end].iter().any(|line| !line.trim().is_empty()) {
        return text.to_string();
    }
    let mut head = at;
    while head > 0 && lines[head - 1].trim().is_empty() {
        head -= 1;
    }
    let mut kept: Vec<String> = lines[..head].to_vec();
    kept.extend_from_slice(&lines[end..]);
    join(kept, text)
}

/// Remove `key` and **the whole of its value** from `table`.
///
/// A TOML value can run over several lines — a multi-line array is the normal
/// way a long `notify` command is written — so deleting the key's first line
/// alone leaves the rest of the value behind as syntax errors. Codex reads
/// `config.toml` whole, so that costs the user every other setting in the
/// file, not just this key. When the value cannot be delimited (which cannot
/// happen for a file that parsed, and [`plan`] re-parses the result either
/// way) nothing is removed: a file that still holds one key of deckd's beats
/// a file Codex refuses.
fn toml_remove_key(text: &str, table: &[&str], key: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (start, end) = table_span(&lines, table);
    let Some(start) = start else {
        return text.to_string();
    };
    let key_line = format!("{} =", toml_key(key));
    for at in start..end {
        if strip_comment(&lines[at])
            .trim_start()
            .starts_with(&key_line)
        {
            let Some(last) = value_end(&lines, at, end) else {
                return text.to_string();
            };
            lines.drain(at..=last);
            break;
        }
    }
    join(lines, text)
}

/// The index of the last line of the value assigned on `lines[at]`, or `None`
/// when it is not closed before `limit` (the end of its own table, which a
/// value can never run past).
fn value_end(lines: &[String], at: usize, limit: usize) -> Option<usize> {
    let mut scan = ValueScan::default();
    let head = &lines[at][lines[at].find('=')? + 1..];
    if scan.feed(head) {
        return Some(at);
    }
    (at + 1..limit).find(|next| scan.feed(&lines[*next]))
}

/// Just enough of a TOML value scanner to find where a value ends: bracket
/// and brace depth, tracked outside strings, plus the multi-line string forms
/// that can hold an unbalanced bracket of their own.
#[derive(Default)]
struct ValueScan {
    depth: i32,
    multiline_basic: bool,
    multiline_literal: bool,
}

impl ValueScan {
    /// Feed one line (the first one starting after its `=`). True once the
    /// value is complete on that line.
    fn feed(&mut self, line: &str) -> bool {
        let chars: Vec<char> = line.chars().collect();
        let mut at = 0usize;
        while at < chars.len() {
            if self.multiline_basic || self.multiline_literal {
                let quote = if self.multiline_basic { '"' } else { '\'' };
                if self.multiline_basic && chars[at] == '\\' {
                    at += 2;
                    continue;
                }
                if chars[at] == quote && chars[at + 1..].starts_with(&[quote, quote]) {
                    self.multiline_basic = false;
                    self.multiline_literal = false;
                    at += 3;
                    continue;
                }
                at += 1;
                continue;
            }
            match chars[at] {
                // Outside a string, `#` starts a comment: the rest is not
                // part of the value.
                '#' => break,
                '"' | '\'' => {
                    let quote = chars[at];
                    if chars[at + 1..].starts_with(&[quote, quote]) {
                        if quote == '"' {
                            self.multiline_basic = true;
                        } else {
                            self.multiline_literal = true;
                        }
                        at += 3;
                        continue;
                    }
                    at += 1;
                    // A single-line string. It has to close on this line;
                    // if it does not the file was never valid TOML.
                    while at < chars.len() {
                        if quote == '"' && chars[at] == '\\' {
                            at += 2;
                            continue;
                        }
                        if chars[at] == quote {
                            at += 1;
                            break;
                        }
                        at += 1;
                    }
                }
                '[' | '{' => {
                    self.depth += 1;
                    at += 1;
                }
                ']' | '}' => {
                    self.depth -= 1;
                    at += 1;
                }
                _ => at += 1,
            }
        }
        self.depth <= 0 && !self.multiline_basic && !self.multiline_literal
    }
}

/// Drop a whole `[header]` table and its keys.
fn toml_remove_table(text: &str, header: &str) -> String {
    let want = format!("[{header}]");
    let lines: Vec<String> = text.lines().map(String::from).collect();
    let Some(at) = lines
        .iter()
        .position(|line| strip_comment(line).trim() == want)
    else {
        return text.to_string();
    };
    let mut end = at + 1;
    while end < lines.len() && !strip_comment(&lines[end]).trim_start().starts_with('[') {
        end += 1;
    }
    let mut kept: Vec<String> = lines[..at].to_vec();
    kept.extend_from_slice(&lines[end..]);
    join(kept, text)
}

/// The `[hooks.state."<key>"] trusted_hash = "<hash>"` pair, written exactly
/// as Codex's own `/hooks` review writes it — one key and nothing else. It
/// must never write `enabled`: `enabled = false` is the user's pause switch,
/// and a trusted-but-disabled hook does not run.
fn toml_set_trust(text: &str, key: &str, hash: &str) -> String {
    let header = format!("hooks.state.{}", toml_string(key));
    let mut text = text.to_string();
    if !toml_has_table(&text, &header) && !toml_has_table(&text, "hooks.state") {
        // Codex emits the empty parent header first; match it byte for byte.
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("\n[hooks.state]\n");
    }
    if !toml_has_table(&text, &header) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n[{header}]\n"));
    }
    let table: Vec<&str> = vec!["hooks", "state", key];
    toml_set_key(&text, &table, "trusted_hash", &toml_string(hash)).0
}

/// The line range `[start, end)` holding a table's keys, and `None` when the
/// table has no explicit header. The root table runs to the first header.
fn table_span(lines: &[String], table: &[&str]) -> (Option<usize>, usize) {
    if table.is_empty() {
        let end = lines
            .iter()
            .position(|l| strip_comment(l).trim_start().starts_with('['))
            .unwrap_or(lines.len());
        return (Some(0), end);
    }
    let want = header_of(table);
    let Some(at) = lines
        .iter()
        .position(|line| strip_comment(line).trim() == want)
    else {
        return (None, 0);
    };
    let mut end = at + 1;
    while end < lines.len() && !strip_comment(&lines[end]).trim_start().starts_with('[') {
        end += 1;
    }
    (Some(at + 1), end)
}

fn join(lines: Vec<String>, original: &str) -> String {
    let mut text = lines.join("\n");
    if original.is_empty() || original.ends_with('\n') || !text.is_empty() {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;
    use crate::integrations::{test_env, Receipts};

    fn install(env: &Env) -> crate::integrations::Receipt {
        crate::integrations::run(env, Target::Codex, Verb::Install { trust: false })
            .expect("install")
    }

    fn hooks(env: &Env) -> serde_json::Value {
        let bytes = std::fs::read(hooks_path(env)).expect("read hooks.json");
        serde_json::from_slice(&bytes).expect("hooks.json is JSON")
    }

    /// C12. Every `command` is a JSON **string**; an array form would make
    /// Codex discard every hook in the file with only a warning.
    #[test]
    fn every_command_is_one_shell_quoted_string() {
        let dir = TestDir::new("codex-command");
        let env = test_env(&dir);
        install(&env);
        let file = hooks(&env);
        let mut seen = 0;
        for (event, groups) in file["hooks"].as_object().unwrap() {
            for group in groups.as_array().unwrap() {
                for handler in group["hooks"].as_array().unwrap() {
                    assert!(
                        handler["command"].is_string(),
                        "{event} command is not a string"
                    );
                    assert_eq!(handler["type"], "command");
                    assert!(handler["timeout"].is_u64(), "{event}");
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, EVENTS.len());
        let command = file["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let (binary, rest) = markers::split_command(command).unwrap();
        assert_eq!(Path::new(&binary), env.exe);
        assert_eq!(rest, "hook codex");
        assert_eq!(
            file["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"],
            REPORT_TIMEOUT_S
        );
        assert_eq!(file["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 1);
    }

    /// The footprint's count of the leftover Codex setup is the groups
    /// `status` counts: one per event deckd installed, never a user's own
    /// group beside them, and nothing at all for a file that is not there.
    #[test]
    fn the_managed_groups_are_deckds_and_only_deckds() {
        let dir = TestDir::new("codex-groups");
        let env = test_env(&dir);
        let file = hooks_path(&env);
        assert_eq!(managed_groups(&file, &env.exe), None, "no file, no setup");
        install(&env);
        assert_eq!(managed_groups(&file, &env.exe), Some(EVENTS.len()));
        let mut root = hooks(&env);
        root["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"hooks": [
                {"type": "command", "command": "echo /opt/deckd hook codex"}
            ]}));
        std::fs::write(&file, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
        assert_eq!(
            managed_groups(&file, &env.exe),
            Some(EVENTS.len()),
            "the user's echo is not ours"
        );
        std::fs::write(&file, b"[]").unwrap();
        assert_eq!(managed_groups(&file, &env.exe), None, "not an object");
    }

    /// C14a: appended at the end, never inserted, because the trust key is
    /// positional and an insert re-points someone else's hash.
    #[test]
    fn our_group_is_appended_after_every_existing_group() {
        let dir = TestDir::new("codex-append");
        let env = test_env(&dir);
        let foreign = serde_json::json!({
            "hooks": {"PermissionRequest": [
                {"hooks": [{"type": "command", "command": "/opt/other hook", "timeout": 2}]}
            ]}
        });
        std::fs::write(
            hooks_path(&env),
            serde_json::to_vec_pretty(&foreign).unwrap(),
        )
        .unwrap();
        let receipt = install(&env);
        let file = hooks(&env);
        let groups = file["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], "/opt/other hook");
        assert!(is_ours(&groups[1]["hooks"][0], &env), "ours is last");
        assert!(
            receipt
                .conflicts
                .iter()
                .any(|c| c.contains("PermissionRequest")),
            "{:?}",
            receipt.conflicts
        );
        // And a re-install keeps it at index 1 rather than growing the array.
        install(&env);
        assert_eq!(
            hooks(&env)["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    /// C14a ii, the case the append rule exists for: a group the user added
    /// **after** deckd's. Re-writing ours must not move theirs, because a
    /// Codex trust hash is keyed on the index
    /// (`<realpath>:<event_snake>:<group>:<handler>`) and a group that slides
    /// one place inherits a hash that is not its own — after which Codex
    /// reads it `modified` and skips it without a word.
    #[test]
    fn a_group_added_after_ours_keeps_its_index_through_repair() {
        let dir = TestDir::new("codex-inplace");
        let env = test_env(&dir);
        let before = serde_json::json!({
            "hooks": {"Stop": [
                {"hooks": [{"type": "command", "command": "mine.sh", "timeout": 2}]}
            ]}
        });
        std::fs::write(
            hooks_path(&env),
            serde_json::to_vec_pretty(&before).unwrap(),
        )
        .unwrap();
        install(&env);

        // The user appends their own group after deckd's and trusts it, so
        // its hash is now bound to `…:stop:2:0`.
        let mut file = hooks(&env);
        let stop = file["hooks"]["Stop"].as_array_mut().unwrap();
        assert!(is_ours(&stop[1]["hooks"][0], &env), "ours landed at 1");
        stop.push(serde_json::json!({
            "hooks": [{"type": "command", "command": "added-later.sh", "timeout": 2}]
        }));
        std::fs::write(hooks_path(&env), serde_json::to_vec_pretty(&file).unwrap()).unwrap();

        let receipt = crate::integrations::run(&env, Target::Codex, Verb::Repair { trust: false })
            .expect("repair");
        let stop = hooks(&env)["hooks"]["Stop"].as_array().unwrap().clone();
        let commands: Vec<&str> = stop
            .iter()
            .map(|g| g["hooks"][0]["command"].as_str().unwrap_or("?"))
            .collect();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert_eq!(commands[0], "mine.sh");
        assert!(is_ours(&stop[1]["hooks"][0], &env), "{commands:?}");
        assert_eq!(
            commands[2], "added-later.sh",
            "repair moved a foreign group and re-pointed its trust hash: {commands:?}"
        );
        // And the receipt records where ours actually is, since `--trust`
        // keys off it.
        let entries = Receipts::load(&env.receipts_path())
            .get(Target::Codex)
            .and_then(|r| r["entries"].as_array().cloned())
            .unwrap_or_default();
        assert!(
            entries.iter().any(|e| e.as_str() == Some("Stop/1")),
            "{entries:?}"
        );
        assert!(
            receipt.conflicts.iter().any(|c| c.contains("Stop")),
            "{:?}",
            receipt.conflicts
        );

        // Uninstall is the one edit that cannot leave the later index alone,
        // so it must say which event needs re-reviewing rather than leave a
        // hook that has silently stopped firing.
        let receipt =
            crate::integrations::run(&env, Target::Codex, Verb::Uninstall).expect("uninstall");
        let left: Vec<String> = hooks(&env)["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["hooks"][0]["command"].as_str().unwrap_or("?").to_string())
            .collect();
        assert_eq!(left, ["mine.sh", "added-later.sh"]);
        assert!(
            receipt
                .next_steps
                .iter()
                .any(|s| s.contains("Stop") && s.contains("/hooks")),
            "{:?}",
            receipt.next_steps
        );
    }

    /// A suffix-exact match: a user's own hook that merely mentions our path
    /// is not ours to delete.
    #[test]
    fn a_hook_that_only_mentions_our_path_is_left_alone() {
        let dir = TestDir::new("codex-suffix");
        let env = test_env(&dir);
        let quoted = markers::shell_word(&env.exe.display().to_string());
        for command in [
            format!("echo {quoted} hook codex"),
            format!("{quoted} hook claude"),
            format!("{quoted} hook codex --extra"),
        ] {
            let handler = serde_json::json!({"type": "command", "command": command});
            assert!(!is_ours(&handler, &env), "{command}");
        }
        let handler =
            serde_json::json!({"type": "command", "command": format!("{quoted} hook codex")});
        assert!(is_ours(&handler, &env));
    }

    #[test]
    fn the_trust_key_is_the_realpath_and_the_snake_case_event() {
        assert_eq!(event_snake("PermissionRequest"), "permission_request");
        assert_eq!(event_snake("SessionStart"), "session_start");
        assert_eq!(event_snake("Stop"), "stop");
        let dir = TestDir::new("codex-trustkey");
        let env = test_env(&dir);
        install(&env);
        let key = trust_key(&hooks_path(&env), "SessionStart", 0, 0);
        assert!(key.ends_with(":session_start:0:0"), "{key}");
        let real = std::fs::canonicalize(hooks_path(&env)).unwrap();
        assert!(key.starts_with(&real.display().to_string()), "{key}");
        assert_eq!(
            std::fs::canonicalize(hooks_path(&env)).unwrap(),
            real,
            "a temp CODEX_HOME resolves through /private on macOS, which is why the key uses the \
             realpath"
        );
    }

    /// C15/C16/C17 in one file: an install writes **nothing** into
    /// `config.toml` — the user's comments, keys and order are untouched
    /// because the file is not opened for writing at all — and no file is
    /// invented where there was none.
    #[test]
    fn config_toml_is_never_written_by_an_install() {
        let dir = TestDir::new("codex-config");
        let env = test_env(&dir);
        let original = "# my codex config\nmodel = \"gpt-6\"\n\n[features]\njs_repl = false\n\n[tui.model_availability_nux]\nseen = true\n";
        std::fs::write(config_path(&env), original).unwrap();
        let receipt = install(&env);
        assert!(
            !receipt.changed.contains(&config_path(&env)),
            "{:?}",
            receipt.changed
        );
        assert_eq!(
            std::fs::read_to_string(config_path(&env)).unwrap(),
            original
        );
        let recorded = crate::integrations::Receipts::load(&env.receipts_path());
        let recorded = recorded.get(Target::Codex).unwrap();
        assert_eq!(recorded["wrote_notify"], false);
        assert_eq!(recorded["wrote_terminal_title"], false);
        crate::integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
        assert_eq!(
            std::fs::read_to_string(config_path(&env)).unwrap(),
            original
        );

        let dir = TestDir::new("codex-config-none");
        let env = test_env(&dir);
        install(&env);
        assert!(!config_path(&env).exists(), "no config.toml is invented");
    }

    /// C17: somebody else's `notify` (on the developer's own machine, Codex
    /// Computer Use) is not touched, not reported and not a conflict — the
    /// key is simply not deckd's business any more.
    #[test]
    fn an_occupied_notify_is_left_alone_and_is_no_conflict() {
        let dir = TestDir::new("codex-notify");
        let env = test_env(&dir);
        let original = "notify = [\"/Applications/SkyComputerUseClient\", \"turn-ended\"]\n";
        std::fs::write(config_path(&env), original).unwrap();
        let receipt = install(&env);
        assert_eq!(
            std::fs::read_to_string(config_path(&env)).unwrap(),
            original
        );
        assert!(receipt.conflicts.is_empty(), "{:?}", receipt.conflicts);
        let report = status(&env);
        assert!(
            !report.limits.iter().any(|l| l.contains("notify")),
            "{:?}",
            report.limits
        );
        crate::integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
        assert_eq!(
            std::fs::read_to_string(config_path(&env)).unwrap(),
            original
        );
    }

    /// The ownership rule, on the way out of the pre-plugin keys. An older
    /// deckd's receipt says it wrote `notify` and `tui.terminal_title`; the
    /// user has since pointed both somewhere else. Neither the migration an
    /// install runs nor an uninstall may delete a key deckd no longer owns —
    /// the receipt flag says "deckd wrote this", never "the value on disk is
    /// still deckd's", so both are re-checked against the file.
    #[test]
    fn migration_and_uninstall_keep_the_config_keys_someone_else_took_back_over() {
        let dir = TestDir::new("codex-config-owner");
        let env = test_env(&dir);
        let config = config_path(&env);
        let taken =
            "notify = [\"/opt/other-notifier\"]\n\n[tui]\nterminal_title = [\"activity\"]\n";
        std::fs::write(&config, taken).unwrap();
        let mut receipts = crate::integrations::Receipts::load(&env.receipts_path());
        receipts.set(
            Target::Codex,
            Some(serde_json::json!({
                "schema": RECEIPT_SCHEMA, "deckd": env.exe.display().to_string(),
                "wrote_terminal_title": true, "created_tui_table": true, "wrote_notify": true,
            })),
        );
        receipts.save(&env.receipts_path()).unwrap();

        install(&env);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            taken,
            "not one byte of a key deckd no longer owns"
        );
        // And the receipt stops claiming them, so a later uninstall has
        // nothing to re-check either.
        let recorded = crate::integrations::Receipts::load(&env.receipts_path());
        assert_eq!(recorded.get(Target::Codex).unwrap()["wrote_notify"], false);
        let receipt = crate::integrations::run(&env, Target::Codex, Verb::Uninstall).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), taken);
        assert!(!receipt.changed.contains(&config), "{:?}", receipt.changed);
    }

    /// A TOML value may span lines, and a long `notify` command is normally
    /// written as a multi-line array. Removing only the key's first line left
    /// the rest behind as syntax errors — and Codex reads `config.toml`
    /// whole, so that cost the user every other setting in the file, not just
    /// the key deckd took back. The migration an install runs over an older
    /// deckd's keys has to get this right too.
    #[test]
    fn migrating_a_multi_line_value_removes_all_of_it_and_the_file_still_parses() {
        let dir = TestDir::new("codex-config-multiline");
        let env = test_env(&dir);
        let config = config_path(&env);
        // The two values an older deckd wrote, spelled the way a person
        // writes a long one — and with a `]` inside a string, so a scanner
        // that only counted brackets would stop in the wrong place.
        let text = format!(
            "model = \"gpt-5\"\n\nnotify = [\n  {},\n  \"hook\",\n  \"codex-notify\",  # ]\n]\n\n             [tui]\nterminal_title = [\n  \"activity\",\n  \"project-name\",\n  \"thread-id\",\n]\n\n             [tui.extra]\nkeep = true\n",
            toml_string(&env.exe.display().to_string())
        );
        toml::from_str::<toml::Table>(&text).expect("the fixture itself is valid TOML");
        std::fs::write(&config, &text).unwrap();
        let mut receipts = crate::integrations::Receipts::load(&env.receipts_path());
        receipts.set(
            Target::Codex,
            Some(serde_json::json!({
                "schema": RECEIPT_SCHEMA, "deckd": env.exe.display().to_string(),
                "wrote_terminal_title": true, "created_tui_table": false, "wrote_notify": true,
            })),
        );
        receipts.save(&env.receipts_path()).unwrap();

        let receipt = install(&env);
        assert!(receipt.changed.contains(&config), "{:?}", receipt.changed);
        let after = std::fs::read_to_string(&config).unwrap();
        let doc: toml::Table = toml::from_str(&after)
            .unwrap_or_else(|e| panic!("the migration left config.toml unparseable: {e}\n{after}"));
        assert!(doc.get("notify").is_none(), "{after}");
        assert!(
            toml_get(&doc, &["tui", "terminal_title"]).is_none(),
            "{after}"
        );
        assert!(
            !after.contains("codex-notify"),
            "no orphaned lines: {after}"
        );
        assert!(!after.contains("thread-id"), "no orphaned lines: {after}");
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            toml_get(&doc, &["tui", "extra", "keep"]).and_then(toml::Value::as_bool),
            Some(true),
            "a table the user wrote under [tui] is untouched"
        );
    }

    #[test]
    fn a_values_last_line_is_found_through_brackets_strings_and_comments() {
        let cases: [(&str, usize); 6] = [
            ("notify = \"one line\"", 0),
            ("notify = [\"a\", \"b\"]  # ]", 0),
            ("notify = [\n  \"a\",\n  \"b\",\n]", 3),
            ("notify = [\n  \"]\",  # ]\n]", 2),
            ("notify = \"\"\"\nspanning ] lines\n\"\"\"", 2),
            ("notify = [\n  { a = 1 },\n]", 2),
        ];
        for (text, want) in cases {
            let lines: Vec<String> = text.lines().map(String::from).collect();
            let limit = lines.len();
            assert_eq!(value_end(&lines, 0, limit), Some(want), "{text}");
        }
        // Never closed: nothing is removed rather than half of it.
        let lines: Vec<String> = ["notify = [", "  \"a\","].map(String::from).to_vec();
        assert_eq!(value_end(&lines, 0, lines.len()), None);
    }

    #[test]
    fn the_review_step_is_printed_and_no_trust_is_written_by_default() {
        let dir = TestDir::new("codex-review");
        let env = test_env(&dir);
        let receipt = install(&env);
        assert!(
            receipt.next_steps.iter().any(|s| s.contains("/hooks")),
            "{:?}",
            receipt.next_steps
        );
        let text = std::fs::read_to_string(config_path(&env)).unwrap_or_default();
        assert!(
            !text.contains("hooks.state"),
            "C14: never written by default"
        );
        assert!(Receipts::load(&env.receipts_path())
            .get(Target::Codex)
            .unwrap()["trust_keys"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// The trust table deckd writes is byte-identical to what the real
    /// `/hooks` review appends (S4 §3d): the empty parent header, the quoted
    /// sub-table, one `trusted_hash` key and no `enabled`.
    #[test]
    fn the_trust_table_matches_what_the_hooks_review_writes() {
        let base = "# codex\n\n[projects.\"/private/tmp\"]\ntrust_level = \"trusted\"\n";
        let key = "/private/var/folders/x/T/owt/hooks.json:session_start:0:0";
        let text = toml_set_trust(base, key, "sha256:aec6d916");
        assert_eq!(
            text,
            format!(
                "# codex\n\n[projects.\"/private/tmp\"]\ntrust_level = \"trusted\"\n\n\
                 [hooks.state]\n\n[hooks.state.\"{key}\"]\ntrusted_hash = \"sha256:aec6d916\"\n"
            ),
            "{text}"
        );
        assert!(!text.contains("enabled"), "the review writes only the hash");
        // A second key reuses the existing parent header.
        let key2 = "/private/var/folders/x/T/owt/hooks.json:stop:0:0";
        let text = toml_set_trust(&text, key2, "sha256:beef");
        assert_eq!(text.matches("[hooks.state]").count(), 1, "{text}");
        assert!(text.contains(&format!(
            "[hooks.state.\"{key2}\"]\ntrusted_hash = \"sha256:beef\""
        )));
        // Re-trusting the same key replaces the hash rather than duplicating
        // the table (a duplicate table is a TOML error Codex would choke on).
        let text = toml_set_trust(&text, key, "sha256:newhash");
        assert_eq!(text.matches(&format!("[hooks.state.\"{key}\"]")).count(), 1);
        assert!(text.contains("trusted_hash = \"sha256:newhash\""));
        let doc: toml::Table = toml::from_str(&text).expect("still valid TOML");
        assert_eq!(
            doc["hooks"]["state"][key]["trusted_hash"].as_str(),
            Some("sha256:newhash")
        );
    }

    /// C12 again, from the other side: `hooks/list`'s warnings are the only
    /// place the whole-file discard shows up, so a warning is fatal here.
    #[test]
    fn a_hooks_list_warning_is_read_as_the_whole_file_being_discarded() {
        let answer = serde_json::json!({
            "id": 1,
            "result": {"data": [{
                "cwd": "/tmp", "hooks": [],
                "warnings": ["failed to parse hooks config /x/hooks.json: invalid type: sequence, expected a string"],
                "errors": [],
            }]},
        });
        let error = parse_hooks_list(&answer).unwrap_err();
        assert!(error.to_string().contains("discarded"), "{error}");
        assert!(
            matches!(error, HooksListError::Discarded(_)),
            "a warning is codex telling us the hooks are dead, not codex being unreachable: \
             `status` maps the two to different rows"
        );
        let answer = serde_json::json!({
            "id": 1,
            "result": {"data": [{
                "cwd": "/tmp",
                "hooks": [{
                    "key": "/private/x/hooks.json:session_start:0:0",
                    "currentHash": "sha256:aec6",
                    "trustStatus": "untrusted",
                    "enabled": true,
                    "sourcePath": "/private/x/hooks.json",
                }],
                "warnings": [], "errors": [],
            }]},
        });
        let rows = parse_hooks_list(&answer).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].trust_status, "untrusted");
        assert_eq!(rows[0].current_hash, "sha256:aec6");
    }

    #[test]
    fn status_reports_path_drift_when_the_binary_moved() {
        let dir = TestDir::new("codex-drift");
        let env = test_env(&dir);
        assert_eq!(status(&env).status, Status::Absent);
        install(&env);
        assert_eq!(status(&env).status, Status::Untrusted);
        let mut file = hooks(&env);
        file["hooks"]["Stop"][0]["hooks"][0]["command"] =
            "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd hook codex".into();
        std::fs::write(hooks_path(&env), super::super::render_json(&file).unwrap()).unwrap();
        assert_eq!(status(&env).status, Status::PathDrift);
        // Three standing limits, plus the line that says this row could not
        // ask codex (`test_env` sets `probe_clis: false`) and read trust off
        // the files alone.
        let limits = status(&env).limits;
        assert_eq!(limits.len(), 4, "{limits:?}");
        assert!(
            limits[3].contains("fell back") && limits[3].contains("[hooks.state]"),
            "{limits:?}"
        );
    }
}
