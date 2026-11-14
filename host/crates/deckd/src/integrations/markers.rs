//! How the installer recognises its own entries, and the one shape it must
//! never write (C24, C24a, C23).
//!
//! **Ownership is by `url`/`command`, never by a marker.** S2 measured
//! (`spike-markers.md` Q3) that Claude *deletes* a `"_managed":"openwalkietalkie"`
//! member from a hook object the next time it writes `settings.json` — a
//! theme change stripped three of three. An uninstall that keyed on the
//! marker would orphan our hooks after any Claude settings write, so the
//! marker is written as a human hint and nothing depends on it.
//!
//! **The inventory must trip none of Claude's fatal settings predicates.**
//! S2 read the predicate out of the 2.1.276 binary: a key literally named
//! `hooks` holding a non-empty array, or a key `PreToolUse`/`PermissionRequest`
//! holding a non-empty value, *anywhere* under a top-level key that is not
//! `hooks` itself, is a `severity:"fatal"` diagnostic that **discards the
//! whole settings file** — measured: `model` fell back to the built-in
//! default and `Hooks: Found 0 total hooks in registry`, with nothing on
//! screen and nothing in the transcript. [`fatal_settings_key`] is that
//! predicate, written deliberately stricter than the binary's, and the
//! installer runs it over the file it is about to write *and* over the file
//! it has just written.
use std::path::Path;

/// The top-level key that holds deckd's managed inventory in
/// `~/.claude/settings.json`. S2: accepted by `claude doctor`, does not stop
/// hooks firing, and survives Claude's own rewrite of the file.
pub const MANAGED_KEY: &str = "_openwalkietalkie";
/// The same inventory as written before the project was renamed from
/// OpenMicroDial: read when [`MANAGED_KEY`] is absent, removed on every write.
pub const LEGACY_MANAGED_KEY: &str = "_openmicrodial";
/// The per-hook hint. Claude deletes it on its next write; nothing may
/// depend on it (C24).
pub const MANAGED_HINT_KEY: &str = "_managed";
pub const MANAGED_HINT: &str = "openwalkietalkie";
/// The shipped bundle path. A hook file written by an installed bundle
/// embeds this, and a development build must still recognise it as ours.
pub const BUNDLE_BINARY: &str = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";
/// The executable basename this installer owns. openmicrokbd's matcher also
/// accepted "any `Contents/MacOS/<Name>`", which on this machine matches
/// *openmicrokbd's own* live hooks — deliberately not ported.
pub const BINARY_NAME: &str = "deckd";

/// Settings keys Claude exempts from the fatal scan (binary `Es`).
const EXEMPT_TOP_LEVEL: &[&str] = &[
    "mcpServers",
    "managedMcpServers",
    "lspServers",
    "pluginConfigs",
    "enabledPlugins",
    "extraKnownMarketplaces",
    "env",
    "skillOverrides",
    "modelSettings",
];
/// Binary `lK`: the two event names whose presence outside `hooks` is fatal.
const FATAL_EVENT_KEYS: &[&str] = &["PreToolUse", "PermissionRequest"];
/// Binary `Ule`'s depth budget. We scan deeper than Claude does.
const SCAN_DEPTH: u32 = 8;

/// The top-level key that would make Claude reject the whole file, if any.
///
/// Mirrors the binary's `Rj(e, Es)`: the real `hooks` key and the exempt set
/// are skipped, everything else is scanned for a nested hook declaration.
pub fn fatal_settings_key(root: &serde_json::Value) -> Option<String> {
    let object = root.as_object()?;
    for (key, value) in object {
        if key == "hooks" || EXEMPT_TOP_LEVEL.contains(&key.as_str()) {
            continue;
        }
        if let Some(why) = fatal_within(value, SCAN_DEPTH) {
            return Some(format!("{key}{why}"));
        }
    }
    None
}

/// The same predicate applied to one value — what the installer runs over
/// the inventory it renders, before the inventory is ever attached to a
/// file. `Ok(())` means the shape is safe at any depth.
pub fn inventory_is_safe(inventory: &serde_json::Value) -> Result<(), String> {
    match fatal_within(inventory, SCAN_DEPTH) {
        None => Ok(()),
        Some(why) => Err(format!(
            "the managed inventory would void the whole settings file at {MANAGED_KEY}{why}"
        )),
    }
}

fn fatal_within(value: &serde_json::Value, depth: u32) -> Option<String> {
    if let Some(object) = value.as_object() {
        for name in FATAL_EVENT_KEYS {
            match object.get(*name) {
                None | Some(serde_json::Value::Null) => {}
                Some(serde_json::Value::Array(items)) if items.is_empty() => {}
                Some(_) => return Some(format!(".{name}")),
            }
        }
        // `hooks` holding anything list- or object-shaped reads as a hook
        // declaration. A plain string is safe (measured, w9) but we never
        // write the name at all.
        match object.get("hooks") {
            Some(serde_json::Value::Array(items)) if !items.is_empty() => {
                return Some(".hooks".into())
            }
            Some(serde_json::Value::Object(_)) => return Some(".hooks".into()),
            _ => {}
        }
    }
    if depth == 0 {
        return None;
    }
    match value {
        serde_json::Value::Object(object) => object.iter().find_map(|(key, child)| {
            fatal_within(child, depth - 1).map(|why| format!(".{key}{why}"))
        }),
        serde_json::Value::Array(items) => items.iter().enumerate().find_map(|(at, child)| {
            fatal_within(child, depth - 1).map(|why| format!("[{at}]{why}"))
        }),
        _ => None,
    }
}

/// Is this the deckd binary — ours, or a bundle's?
///
/// Accepts the running executable, the shipped bundle path, and any absolute
/// path whose basename is `deckd` (a dev build, an older bundle that has
/// since moved: `path_drift`, not "someone else's hook").
pub fn is_managed_binary(path: &str, exe: &Path) -> bool {
    let candidate = Path::new(path);
    if candidate == exe || path == BUNDLE_BINARY {
        return true;
    }
    candidate.is_absolute()
        && candidate
            .file_name()
            .is_some_and(|name| name == BINARY_NAME || name == "deckd.exe")
}

/// Quote a word for a shell command line, exactly as openmicrokbd does
/// (`agent_integrations.rs:847-867`), so [`decode_shell_word`] can read it
/// back. This is the quoting deckd writes into a file: `statusLine` has no
/// `args` and runs in a shell (C23), and a Codex `command` is a single
/// shell-interpreted string (C12). A command printed for the owner to paste
/// is quoted by [`super::command_line`] instead.
#[cfg(not(windows))]
pub fn shell_word(word: &str) -> String {
    let bare = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_-.+:".contains(c));
    if bare {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

/// [`shell_word`] on Windows, where the same command line may be run by
/// cmd.exe, PowerShell or Git Bash: single quotes are literal characters to
/// cmd.exe, and a backslash is an escape to bash. So a path is written with
/// forward slashes (every one of them, and Windows, reads those), bare while
/// it holds only characters none of the three treats specially, and in
/// double quotes otherwise, which cmd.exe and bash both strip.
/// [`read_shell_word`] reads either back.
#[cfg(windows)]
pub fn shell_word(word: &str) -> String {
    let word = word.replace('\\', "/");
    let bare = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_-.+:~".contains(c));
    if bare {
        return word;
    }
    format!("\"{word}\"")
}

/// Read one shell word off the front of `text`, returning it and the rest.
///
/// A word is a concatenation of bare characters, `'…'` and `"…"` segments,
/// which is what makes `'"'"'` decode back to a single quote for free.
pub fn read_shell_word(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let mut word = String::new();
    let mut rest = text;
    let mut any = false;
    while let Some(c) = rest.chars().next() {
        match c {
            ' ' | '\t' | '\n' => break,
            '\'' => {
                let body = &rest[1..];
                let end = body.find('\'')?;
                word.push_str(&body[..end]);
                rest = &body[end + 1..];
                any = true;
            }
            '"' => {
                let body = &rest[1..];
                let end = body.find('"')?;
                // A backslash escape inside "…" is not part of the grammar
                // `shell_word` emits; refuse rather than mis-decode.
                if body[..end].contains('\\') {
                    return None;
                }
                word.push_str(&body[..end]);
                rest = &body[end + 1..];
                any = true;
            }
            '\\' => return None,
            _ => {
                word.push(c);
                rest = &rest[c.len_utf8()..];
                any = true;
            }
        }
    }
    any.then_some((word, rest))
}

/// `text` as exactly one shell word, or `None`.
pub fn decode_shell_word(text: &str) -> Option<String> {
    let (word, rest) = read_shell_word(text)?;
    rest.trim().is_empty().then_some(word)
}

/// Split a managed command string into its binary and the rest of the line,
/// e.g. `'/path/to/deckd' hook codex` → `("/path/to/deckd", "hook codex")`.
pub fn split_command(command: &str) -> Option<(String, String)> {
    let (binary, rest) = read_shell_word(command)?;
    Some((binary, rest.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_round_trip_through_spaces_and_apostrophes() {
        for word in [
            "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
            "/tmp/Tony's OpenWalkieTalkie/deckd",
            "npx ccstatusline@latest --theme 'dark'",
            "plain",
        ] {
            let quoted = shell_word(word);
            assert_eq!(decode_shell_word(&quoted).as_deref(), Some(word), "{word}");
        }
        assert_eq!(shell_word("/usr/bin/deckd"), "/usr/bin/deckd");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("Tony's"), "'Tony'\"'\"'s'");
        assert_eq!(decode_shell_word("echo hi"), None, "two words");
        assert_eq!(decode_shell_word("'unclosed"), None);
        assert_eq!(decode_shell_word("a\\ b"), None, "backslashes are not ours");
    }

    #[test]
    fn a_managed_command_splits_into_its_binary_and_arguments() {
        let command = format!("{} hook codex", shell_word("/tmp/Tony's deck/deckd"));
        let (binary, rest) = split_command(&command).unwrap();
        assert_eq!(binary, "/tmp/Tony's deck/deckd");
        assert_eq!(rest, "hook codex");
        let (binary, rest) = split_command(&format!(
            "{} hook claude-statusline -- {}",
            shell_word("/tmp/a b/deckd"),
            shell_word("npx ccstatusline@latest")
        ))
        .unwrap();
        assert_eq!(binary, "/tmp/a b/deckd");
        assert_eq!(rest, "hook claude-statusline -- 'npx ccstatusline@latest'");
        assert_eq!(
            decode_shell_word("'npx ccstatusline@latest'").as_deref(),
            Some("npx ccstatusline@latest")
        );
    }

    #[test]
    fn only_a_deckd_path_is_recognised_as_ours() {
        let exe = Path::new("/tmp/build/deckd-dev");
        assert!(
            is_managed_binary("/tmp/build/deckd-dev", exe),
            "current_exe"
        );
        assert!(is_managed_binary(BUNDLE_BINARY, exe));
        assert!(is_managed_binary("/opt/other/deckd", exe), "path_drift");
        // The openmicrokbd heuristics that would claim a sibling product's
        // live hooks on this very machine.
        assert!(!is_managed_binary(
            "/Users/tony/Applications/OpenMicro-PR3.app/Contents/MacOS/OpenMicro",
            exe
        ));
        assert!(!is_managed_binary("deckd", exe), "relative");
        assert!(!is_managed_binary("/usr/bin/node", exe));
    }

    /// C24a. Every shape here was run against `claude doctor` 2.1.276 in S2
    /// (`spike-markers.md` Q1, bisect2); `FATAL=True` rows must be rejected
    /// and `FATAL=False` rows must be accepted.
    #[test]
    fn the_fatal_settings_predicate_matches_what_claude_doctor_rejected() {
        let safe = [
            serde_json::json!({"previousStatusLine":{"type":"command","command":"x"}}),
            serde_json::json!({"entries":[{"event":"PermissionRequest","group":0,"url":"u"}]}),
            serde_json::json!({"events":{"PermissionRequest":[]}}),
            serde_json::json!({"events":{"PostToolUse":["sha256:.."]}}),
            serde_json::json!({"hooks":[]}),
            serde_json::json!({"hooks":"SessionStart/0/0"}),
            serde_json::json!({"schema":1,"entries":["PermissionRequest/0/0"],"token":"dk_x"}),
            serde_json::json!("installed"),
        ];
        for value in safe {
            assert_eq!(inventory_is_safe(&value), Ok(()), "{value}");
        }
        let fatal = [
            serde_json::json!({"hooks":["SessionStart/0/0"]}),
            serde_json::json!({"entries":[{"event":"x","group":{"hooks":[{"type":"http"}]}}]}),
            serde_json::json!({"events":{"PermissionRequest":["sha256:.."]}}),
            serde_json::json!({"a":{"b":{"c":{"hooks":["x"]}}}}),
            serde_json::json!({"a":{"b":{"hooks":["x"]}}}),
            serde_json::json!({"PreToolUse":{"matcher":"AskUserQuestion"}}),
        ];
        for value in fatal {
            assert!(inventory_is_safe(&value).is_err(), "{value}");
        }
    }

    /// The whole-file form: `hooks` at the top level is the real thing and
    /// must never be flagged, and the exempt keys are exempt.
    #[test]
    fn the_real_hooks_key_and_the_exempt_keys_are_never_flagged() {
        let settings = serde_json::json!({
            "hooks": {"PermissionRequest": [{"hooks": [{"type": "http", "url": "u"}]}]},
            "mcpServers": {"x": {"hooks": ["anything"]}},
            "model": "claude-haiku-4-5",
            MANAGED_KEY: {"schema": 1, "entries": ["PermissionRequest/0/0"]},
        });
        assert_eq!(fatal_settings_key(&settings), None);
        let bad = serde_json::json!({
            "hooks": {},
            MANAGED_KEY: {"managed": {"hooks": ["PermissionRequest/0/0"]}},
        });
        assert_eq!(
            fatal_settings_key(&bad).as_deref(),
            Some("_openwalkietalkie.managed.hooks")
        );
    }
}
