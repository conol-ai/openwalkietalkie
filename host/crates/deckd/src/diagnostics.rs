//! `deckd diagnostics` (design §10.5): **one file the operator can send**.
//!
//! The whole point of this file is that it can be handed to a stranger, so
//! its contract is what is *not* in it. §10.5 and §11 name the classes: no
//! token plaintext, no hook payload bodies, no transcripts, no card/reply/
//! order text, no prompt- or tool-derived titles, no device shadow lines —
//! unless `[privacy] log_content` was already on, in which case the operator
//! has already said those may be written down and the bundle says so at the
//! top instead of pretending otherwise.
//!
//! Two redactions do that work, and they are different in kind:
//!
//! * **Secrets** are scrubbed by *pattern* over the whole rendered text, as
//!   the last step, so no section can leak one by being added later:
//!   `dk_…` (the mint format, `tokens::PREFIX`) and `Bearer …` anywhere at
//!   all. A token the daemon minted is the one secret that is certain to
//!   exist — it is sitting in `~/.claude/settings.json` as an
//!   `Authorization` header — and the CLIs' own config files are therefore
//!   **never copied in**; only `integrations.status`, which carries paths and
//!   verdicts and no bearer.
//! * **Content** is dropped *structurally* while `log_content` is off. A log
//!   line keeps its timestamp, level, target and the **names** of its fields,
//!   with numbers and booleans — which is what a diagnosis is made of (a pid,
//!   a port, a count, an exit code, a duration) — and everything else becomes
//!   `<redacted>`. That is a blunter rule than "redact the fields we know are
//!   content", and deliberately: the daemon redacts what it knows at write
//!   time, and this file exists precisely for the case where something got
//!   written down that nobody predicted. A line with no log header at all
//!   (a panic payload, a backtrace, anything that reached `deckd.launchd.log`
//!   before `tracing` was up) is dropped whole, because §11 counts panic
//!   payloads as content.
//!
//! The bundle is a single **text** file, not §10.5's `.zip`: one file is what
//! the contract asks for, a zip would need a new dependency in a workspace
//! that pins every version, and a text file is the only shape in which
//! "none of these three strings appears anywhere in the output" is a claim
//! the operator can check for themselves with `grep`.
use crate::config::{Config, Paths};
use crate::integrations;
use deck_model::Ts;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// §10.5: the last 2000 lines of each log.
pub const LOG_TAIL: usize = 2000;
/// What a redacted value is replaced by, everywhere.
pub const REDACTED: &str = "<redacted>";
/// What a log line whose body was dropped is replaced by.
pub const REDACTED_LOG: &str = "<redacted: [privacy] log_content is off>";
/// The `[privacy]`-independent secret marker.
pub const REDACTED_SECRET: &str = "<redacted-secret>";
/// Config keys whose value is never copied, whatever it is.
const SECRET_KEYS: [&str; 7] = [
    "token",
    "secret",
    "password",
    "passphrase",
    "api_key",
    "apikey",
    "credential",
];

/// What the bundle run is allowed to do.
#[derive(Clone, Debug)]
pub struct Options {
    /// Run the external probes: `launchctl print`, `codesign -dv`,
    /// `sw_vers`, the CLIs' `--version`. Off in tests — a unit test may not
    /// shell out to the operator's own launchd domain.
    pub probe: bool,
    /// Lines kept from the tail of each log.
    pub log_tail: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            probe: true,
            log_tail: LOG_TAIL,
        }
    }
}

/// The rendered bundle and the two lists it prints at the top.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    pub text: String,
    /// What was left out, in the operator's words.
    pub redacted: Vec<String>,
    /// What *is* in here, so "send me your diagnostics" is informed consent.
    pub kept: Vec<String>,
}

/// `~/Desktop`, where §10.5 puts the file.
pub fn default_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join("Desktop"))
}

/// The file name §10.5 names, with the timestamp flattened so it survives a
/// mail attachment: `OpenWalkieTalkie-diagnostics-20260922-194203.txt`.
pub fn file_name(now: Ts) -> String {
    let stamp: String = now
        .to_string()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    // `20260922T194203120Z` → the date, `T`, the time; the millis and the Z
    // add nothing to a file name.
    let stamp = stamp.get(..15).unwrap_or(&stamp).replace('T', "-");
    format!("OpenWalkieTalkie-diagnostics-{stamp}.txt")
}

/// Write the bundle into `dir`, 0600 — it is a diagnostic of a 0700 data
/// directory and must not become the one world-readable copy of it.
pub fn write(dir: &Path, bundle: &Bundle, now: Ts) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(now));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write as _;
    opts.open(&path)
        .and_then(|mut f| f.write_all(bundle.text.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Everything §10.5 lists, in one string.
///
/// `env` is the installer's view of this machine (`integrations::Env`); it is
/// optional because the bundle must still render on a machine where the
/// installer cannot even be constructed — that failure is itself worth
/// sending.
pub fn render(
    paths: &Paths,
    config: &Config,
    env: Option<&integrations::Env>,
    opts: &Options,
    now: Ts,
) -> Bundle {
    let log_content = config.privacy.log_content;
    let mut body = String::new();
    let mut section = |title: &str, text: String| {
        let _ = write!(body, "\n===== {title} =====\n{}\n", text.trim_end());
    };

    section(
        "versions",
        versions(opts)
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<String>>()
            .join("\n"),
    );
    section("paths", paths_text(paths));
    section("doctor", doctor_text());
    section(
        "config.toml",
        match std::fs::read_to_string(&paths.config) {
            Ok(text) => redact_config(&text),
            Err(e) => format!("{}: {e}", paths.config.display()),
        },
    );
    section("privacy", privacy_text(config));
    section("status.json", read_or_why(&paths.status));
    section(
        "state.json",
        match std::fs::read_to_string(&paths.state) {
            Ok(text) => redact_state(&text, log_content),
            Err(e) => format!("{}: {e}", paths.state.display()),
        },
    );
    section("tokens.toml", tokens_text(paths));
    for name in log_files(&paths.logs) {
        section(
            &format!(
                "log {}",
                name.file_name().unwrap_or_default().to_string_lossy()
            ),
            redact_log(&read_or_why(&name), log_content, opts.log_tail),
        );
    }
    section("codesign", codesign_text(opts));
    section("launchctl", launchctl_text(opts));
    section("integrations", integrations_text(env));

    // The secret scrub runs over the *whole* body, after every section, so a
    // section added later cannot opt out of it by forgetting to call it.
    let mut extra = Vec::new();
    if let Some(env) = env {
        extra.extend(embedded_secrets(env));
    }
    let body = scrub_secrets(&body, &extra);

    let redacted = redaction_list(log_content);
    let kept = kept_list();
    let mut text = header(now, log_content, &redacted, &kept);
    text.push_str(&body);
    Bundle {
        text,
        redacted,
        kept,
    }
}

/// The first thing the reader sees: what this file is, and what was taken
/// out of it before it was written.
fn header(now: Ts, log_content: bool, redacted: &[String], kept: &[String]) -> String {
    let mut out = format!(
        "OpenWalkieTalkie diagnostics\n\
         generated: {now}\n\
         deckd:     {}\n\
         \n\
         This file is meant to be sent to someone else, so read this first.\n\
         \n\
         [privacy] log_content = {log_content}{}\n\
         \n\
         REDACTED — not in this file:\n",
        env!("CARGO_PKG_VERSION"),
        if log_content {
            "  <- ON: the operator turned content logging on, so the log sections below carry \
             whatever was logged, including text the daemon would otherwise never write down."
        } else {
            ""
        }
    );
    for line in redacted {
        let _ = writeln!(out, "  - {line}");
    }
    out.push_str("\nKEPT — what is in this file:\n");
    for line in kept {
        let _ = writeln!(out, "  - {line}");
    }
    out
}

fn redaction_list(log_content: bool) -> Vec<String> {
    let mut list = vec![
        format!(
            "every bearer token: `{}…` and anything after `Bearer ` is replaced by `{REDACTED_SECRET}` \
             across the whole file",
            crate::tokens::PREFIX
        ),
        "the CLIs' own config files (`settings.json`, `hooks.json`, `config.toml`) — they carry \
         the `hooks` token, so only `integrations.status` is included"
            .into(),
        "`tokens.toml`'s hashes — names, scopes and dates only".into(),
        "each deck's id in `state.json` (its Bluetooth address): the decks are numbered \
         `deck-1`, `deck-2`… instead"
            .into(),
        format!(
            "any `config.toml` value whose key contains {} ",
            SECRET_KEYS
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<String>>()
                .join(", ")
        ),
    ];
    if log_content {
        list.push(
            "NOTHING ELSE: `[privacy] log_content` is on, so hook payloads, prompts, transcripts, \
             card and reply text and panic payloads appear in the log sections as they were logged"
                .into(),
        );
    } else {
        list.push(
            "every log line's message and every non-numeric field value: a line keeps its \
             timestamp, level, target and field *names*, which covers hook payload bodies, \
             prompts, transcripts, card/reply/order text and prompt- or tool-derived titles"
                .into(),
        );
        list.push(
            "every log line with no log header at all — panic payloads and backtraces in \
             `deckd.launchd.log` (§11 counts a panic payload as content)"
                .into(),
        );
        list.push(
            "`state.json`'s `terminal_map` values, left by an older daemon (§5.5 desktop identity)"
                .into(),
        );
    }
    list
}

fn kept_list() -> Vec<String> {
    vec![
        "the `deckd doctor` report, including launchd's last exit status and the dangling-agent \
         check"
            .into(),
        "`config.toml` as the operator wrote it, minus the secret-shaped keys above".into(),
        "`status.json`, and `state.json`'s slots, faces, settings, keys and card ids, each deck's \
         own faces, settings and keys among them — agent names and working directories too, which \
         are *state* (§4.3) and will name the operator's projects"
            .into(),
        "`state.json`'s delivery record: for each recent card its ids, times and what became of \
         it, and counts of the deck's reader requests — never the words of a card, a reply, an \
         order or an app's reason"
            .into(),
        "`codesign -dv` for both Mach-Os of the bundle, and `launchctl print` for the LaunchAgent"
            .into(),
        "`integrations.status`: which files deckd wrote, where, and whether they still verify"
            .into(),
        format!("the last {LOG_TAIL} lines of each log, redacted as above"),
    ]
}

fn read_or_why(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => format!("{}: {e}", path.display()),
    }
}

fn paths_text(paths: &Paths) -> String {
    [
        ("data", &paths.data),
        ("config", &paths.config),
        ("state", &paths.state),
        ("tokens", &paths.tokens),
        ("status", &paths.status),
        ("socket", &paths.socket),
        ("port", &paths.port),
        ("logs", &paths.logs),
        ("caches", &paths.caches),
    ]
    .iter()
    .map(|(name, path)| {
        format!(
            "{name}: {} ({})",
            path.display(),
            if path.exists() { "present" } else { "absent" }
        )
    })
    .collect::<Vec<String>>()
    .join("\n")
}

fn privacy_text(config: &Config) -> String {
    let p = &config.privacy;
    format!(
        "keep_tool_input = {:?}\nkeep_prompts = {}\nkeep_replies = {:?}\ntranscript_log = \
         {}\nlog_content = {}\nevent_ring = {}",
        p.keep_tool_input,
        p.keep_prompts,
        p.keep_replies,
        p.transcript_log,
        p.log_content,
        p.event_ring
    )
}

fn doctor_text() -> String {
    crate::launchd::Agent::load(crate::launchd::DECKD_LABEL)
        .map(|agent| {
            format!(
                "launchd {}: state {}, {}\nprogram: {}",
                agent.label,
                agent.state,
                agent.exit_meaning(),
                agent.dangling().unwrap_or_else(|| agent
                    .program
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "launchctl named no program".into()))
            )
        })
        .unwrap_or_else(|| {
            format!(
                "{} is not loaded in gui/{}",
                crate::launchd::DECKD_LABEL,
                crate::launchd::uid()
            )
        })
}

/// The three logs §10.5 names, newest rolled file first for `deckd.log`
/// (`tracing-appender` suffixes it with the date).
pub fn log_files(logs: &Path) -> Vec<PathBuf> {
    let mut deckd: Vec<PathBuf> = std::fs::read_dir(logs)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("deckd.log"))
                })
                .collect()
        })
        .unwrap_or_default();
    deckd.sort();
    // Always three sections, even when a file is not there: a *missing*
    // `deckd.log` is itself the diagnosis (the daemon has only ever run
    // `--foreground`, or the log directory is not writable), and a section
    // that silently disappears reads as "nothing to report".
    let newest = deckd.pop().unwrap_or_else(|| logs.join("deckd.log"));
    vec![newest, logs.join("app.log"), logs.join("deckd.launchd.log")]
}

/// `tokens.toml` without its hashes: the names, scopes and dates are what a
/// diagnosis needs (is there a `hooks` token? when was it rotated? where is
/// it embedded?) and the hash is the one field that is neither.
fn tokens_text(paths: &Paths) -> String {
    match crate::tokens::Tokens::load(&paths.tokens) {
        Err(e) => format!("{}: {e}", paths.tokens.display()),
        Ok(tokens) if tokens.list().is_empty() => "no tokens".into(),
        Ok(tokens) => tokens
            .list()
            .iter()
            .map(|t| {
                format!(
                    "{}: scopes {}, created {}, expires {}, embedded in {}",
                    t.name,
                    t.scopes
                        .iter()
                        .map(|s| format!("{s:?}"))
                        .collect::<Vec<String>>()
                        .join(","),
                    t.created,
                    t.expires
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "never".into()),
                    if t.embedded_in.is_empty() {
                        "nothing".to_string()
                    } else {
                        t.embedded_in.join(", ")
                    }
                )
            })
            .collect::<Vec<String>>()
            .join("\n"),
    }
}

fn versions(opts: &Options) -> Vec<(String, String)> {
    let mut out = vec![
        ("deckd".into(), env!("CARGO_PKG_VERSION").to_string()),
        (
            "executable".into(),
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| e.to_string()),
        ),
    ];
    if opts.probe {
        out.push((
            "macos".into(),
            probe("/usr/bin/sw_vers", &["-productVersion"]),
        ));
        out.push(("arch".into(), probe("/usr/bin/uname", &["-m"])));
    }
    out
}

/// `codesign -dv --entitlements -` for both Mach-Os of the bundle: the
/// week-7 claim is that they share an `Identifier` and a `TeamIdentifier`
/// and both carry the two entitlements (§10.1), and a diagnostics bundle
/// that does not carry the evidence cannot answer "why does the daemon not
/// get the microphone the GUI was granted".
fn codesign_text(opts: &Options) -> String {
    if !opts.probe {
        return "not probed".into();
    }
    let Ok(exe) = std::env::current_exe() else {
        return "the running executable cannot be located".into();
    };
    codesign_targets(&exe)
        .iter()
        .map(|path| {
            format!(
                "--- {} ---\n{}",
                path.display(),
                probe(
                    "/usr/bin/codesign",
                    &["-dv", "--entitlements", "-", &path.to_string_lossy()],
                )
            )
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// The Mach-Os to check: this binary, and — when it sits in a bundle — the
/// GUI beside it in `Contents/MacOS`.
pub fn codesign_targets(exe: &Path) -> Vec<PathBuf> {
    let mut out = vec![exe.to_path_buf()];
    if let Some(app) = crate::launchd::app_bundle_of(exe) {
        let gui = app.join("Contents/MacOS/OpenWalkieTalkie");
        if gui != *exe {
            out.push(gui);
        }
    }
    out
}

fn launchctl_text(opts: &Options) -> String {
    if !opts.probe {
        return "not probed".into();
    }
    [crate::launchd::DECKD_LABEL]
        .iter()
        .map(|label| {
            format!(
                "--- {label} ---\n{}",
                crate::launchd::print(label)
                    .unwrap_or_else(|| format!("not loaded in gui/{}", crate::launchd::uid()))
            )
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn integrations_text(env: Option<&integrations::Env>) -> String {
    let Some(env) = env else {
        return "the installer's environment could not be read on this machine".into();
    };
    integrations::status(env, None)
        .iter()
        .map(|report| {
            let mut text = format!(
                "{}: {}{}",
                report.target.name(),
                report.status.as_str(),
                report
                    .detail
                    .as_ref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default()
            );
            for file in &report.files {
                let _ = write!(text, "\n  file: {}", file.display());
            }
            for limit in &report.limits {
                let _ = write!(text, "\n  limit: {limit}");
            }
            text
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// One external command's combined output, or why it did not run.
fn probe(program: &str, args: &[&str]) -> String {
    match std::process::Command::new(program).args(args).output() {
        Err(e) => format!("{program}: {e}"),
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            let text = text.trim().to_string();
            if text.is_empty() {
                format!("{program} said nothing (status {})", out.status)
            } else {
                text
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Replace every `dk_…` and every `Bearer …` in `text`, plus any literal in
/// `extra` (the plaintexts actually embedded in the CLIs' files, which is
/// the one set of secrets that certainly exists on a working install).
///
/// A pattern scrub rather than a per-section one: this runs last, over the
/// whole body, so a section added later inherits it.
pub fn scrub_secrets(text: &str, extra: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with(crate::tokens::PREFIX) {
            out.push_str(REDACTED_SECRET);
            i += crate::tokens::PREFIX.len() + token_run(&text[i + crate::tokens::PREFIX.len()..]);
            continue;
        }
        if text[i..].starts_with("Bearer ") {
            out.push_str("Bearer ");
            out.push_str(REDACTED_SECRET);
            let rest = &text[i + "Bearer ".len()..];
            i += "Bearer ".len()
                + rest
                    .find(|c: char| c.is_whitespace() || c == '"')
                    .unwrap_or(rest.len());
            continue;
        }
        let c = text[i..].chars().next().unwrap_or('\0');
        out.push(c);
        i += c.len_utf8();
    }
    // Anything the caller knows is a secret, however it is spelled.
    extra.iter().fold(out, |acc, secret| {
        if secret.len() < 8 {
            // Too short to replace safely: a 3-character "secret" would eat
            // half the file. A token is 32 random bytes base64url.
            acc
        } else {
            acc.replace(secret.as_str(), REDACTED_SECRET)
        }
    })
}

/// How many base64url characters follow a `dk_`.
fn token_run(rest: &str) -> usize {
    rest.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .map(char::len_utf8)
        .sum()
}

/// The bearer plaintexts actually embedded in the CLIs' files. Read so they
/// can be **removed**, never so they can be printed.
fn embedded_secrets(env: &integrations::Env) -> Vec<String> {
    let mut out = Vec::new();
    for path in [
        integrations::claude::settings_path(env),
        integrations::codex::hooks_path(env),
    ] {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut rest = text.as_str();
        while let Some(at) = rest.find("Bearer ") {
            rest = &rest[at + "Bearer ".len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"')
                .unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

/// `config.toml` with the secret-shaped keys blanked. The file is the
/// operator's own and its shape is half of every configuration bug, so it
/// goes in whole; only a value whose key *names* a secret is replaced.
pub fn redact_config(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let Some((key, _)) = trimmed.split_once('=') else {
                return line.to_string();
            };
            let key_lower = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
            if SECRET_KEYS.iter().any(|s| key_lower.contains(s)) {
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}{} = \"{REDACTED_SECRET}\"", key.trim())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// `state.json` per `[privacy]`. Nothing content-bearing is written there by
/// design (§9: faces, settings, slots, card ids, voices, usage, and the
/// delivery record, which is ids, times, counts and closed words). The one
/// exception was `terminal_map`, the Ghostty tab pins: the daemon no longer
/// writes it, but a file an older daemon left still carries it until the
/// next save, so its values still go while `log_content` is off. Each deck
/// is named by its number, whatever `log_content` says: its id is its
/// Bluetooth address. Anything that cannot be parsed is dropped rather than
/// passed through: an unparseable `state.json` is worth knowing about but
/// not worth copying blind.
pub fn redact_state(text: &str, log_content: bool) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) else {
        return format!("state.json does not parse as JSON ({} bytes)", text.len());
    };
    number_decks(&mut value);
    if !log_content {
        if let Some(map) = value
            .get_mut("terminal_map")
            .and_then(|m| m.as_object_mut())
        {
            for (_, v) in map.iter_mut() {
                *v = serde_json::Value::String(REDACTED.into());
            }
        }
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|e| e.to_string())
}

/// `decks` keyed `deck-1`, `deck-2`… in the file's order, and `deck` as the
/// number of the one it names (redacted when it names none of them).
fn number_decks(value: &mut serde_json::Value) {
    let ids: Vec<String> = value
        .get("decks")
        .and_then(serde_json::Value::as_object)
        .map(|decks| decks.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(decks) = value
        .get_mut("decks")
        .and_then(serde_json::Value::as_object_mut)
    {
        let numbered: serde_json::Map<String, serde_json::Value> = ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| Some((format!("deck-{}", i + 1), decks.remove(id)?)))
            .collect();
        *decks = numbered;
    }
    if let Some(current) = value.get_mut("deck").filter(|deck| !deck.is_null()) {
        let number = current
            .as_str()
            .and_then(|id| ids.iter().position(|known| known == id));
        *current = serde_json::Value::String(match number {
            Some(i) => format!("deck-{}", i + 1),
            None => REDACTED.into(),
        });
    }
}

/// The tail of one log, line by line through [`redact_log_line`].
pub fn redact_log(text: &str, log_content: bool, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let skipped = lines.len().saturating_sub(tail);
    let mut out = String::new();
    if skipped > 0 {
        let _ = writeln!(out, "… {skipped} earlier line(s) not included");
    }
    for line in lines.iter().skip(skipped) {
        let _ = writeln!(out, "{}", redact_log_line(line, log_content));
    }
    out
}

/// One log line, redacted structurally.
///
/// With `log_content` on the line goes through as written — the operator
/// asked for content to be logged and this file says so at the top. With it
/// off, a line keeps `<timestamp> <LEVEL> <target>:` and then only its
/// `key=` **names**, with numeric and boolean values kept because a pid, a
/// port, a count, an exit code or a duration is what a diagnosis is made of
/// and none of them is content. A line without that header — a panic
/// payload, a backtrace, a stray `println!` in `deckd.launchd.log` — has no
/// structure to preserve and is dropped whole.
pub fn redact_log_line(line: &str, log_content: bool) -> String {
    if log_content {
        return line.to_string();
    }
    let mut tokens = line.split_whitespace();
    let (Some(ts), Some(level), Some(target)) = (tokens.next(), tokens.next(), tokens.next())
    else {
        return REDACTED_LOG.into();
    };
    let dated = ts.len() >= 10 && ts.as_bytes()[4] == b'-' && ts.as_bytes()[7] == b'-';
    let levelled = matches!(level, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR");
    if !dated || !levelled || !target.ends_with(':') {
        return REDACTED_LOG.into();
    }
    let mut out = format!("{ts} {level} {target} {REDACTED_LOG}");
    for token in tokens {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        if is_plain_scalar(value) {
            let _ = write!(out, " {key}={value}");
        } else {
            let _ = write!(out, " {key}={REDACTED}");
        }
    }
    out
}

/// A number, a boolean or `None` — never content.
fn is_plain_scalar(value: &str) -> bool {
    matches!(value, "true" | "false" | "None")
        || (!value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
            && value.chars().any(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;

    #[test]
    fn a_mint_format_token_is_scrubbed_wherever_it_appears() {
        let token = format!("{}AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHH", crate::tokens::PREFIX);
        let text = format!(
            "Authorization: Bearer {token}\nurl=http://127.0.0.1:47201 tok={token}\n\"{token}\"\n"
        );
        let out = scrub_secrets(&text, &[]);
        assert!(!out.contains(&token), "{out}");
        assert!(!out.contains("AAAABBBB"), "{out}");
        // The structure around it survives, so the line is still readable.
        assert!(out.contains("url=http://127.0.0.1:47201"), "{out}");
        assert!(out.contains("Bearer <redacted-secret>"), "{out}");
    }

    /// A bearer that is **not** in the mint format — hand-edited into the
    /// CLI's file — is still a secret, and `Bearer ` is what marks it.
    #[test]
    fn a_bearer_that_is_not_a_mint_token_is_scrubbed_by_its_header() {
        let out = scrub_secrets("\"Authorization\": \"Bearer hunter2-hunter2\"", &[]);
        assert!(!out.contains("hunter2"), "{out}");
    }

    #[test]
    fn a_literal_the_caller_knows_is_scrubbed_but_a_short_one_is_not() {
        let out = scrub_secrets(
            "secret=longenoughsecret and x=no",
            &["longenoughsecret".into()],
        );
        assert!(!out.contains("longenoughsecret"), "{out}");
        // A three-letter "secret" would eat the file; it is refused.
        let out = scrub_secrets("secret=abc and x=no", &["abc".into()]);
        assert!(out.contains("abc"), "{out}");
    }

    #[test]
    fn config_keeps_its_shape_and_loses_only_the_secret_shaped_keys() {
        let text = "[api]\nport = 47201\n\n[voice]\n# a comment\ndevice_uid = \"C2-2D\"\napi_key = \"sk-live-xyz\"\n";
        let out = redact_config(text);
        assert!(out.contains("port = 47201"), "{out}");
        assert!(out.contains("# a comment"), "{out}");
        assert!(out.contains("device_uid = \"C2-2D\""), "{out}");
        assert!(!out.contains("sk-live-xyz"), "{out}");
        assert!(out.contains("api_key = \"<redacted-secret>\""), "{out}");
    }

    #[test]
    fn a_log_line_keeps_its_header_and_its_numbers_and_loses_everything_else() {
        let line = "2026-09-22T19:42:03.120123Z  INFO deckd::core: deckd running sid=2491940960 \
                    demo=false prompt=\"write me a deploy script\"";
        let out = redact_log_line(line, false);
        assert!(
            out.starts_with("2026-09-22T19:42:03.120123Z INFO deckd::core:"),
            "{out}"
        );
        assert!(out.contains("sid=2491940960"), "{out}");
        assert!(out.contains("demo=false"), "{out}");
        assert!(!out.contains("deploy script"), "{out}");
        assert!(!out.contains("deckd running"), "{out}");
        assert!(out.contains("prompt=<redacted>"), "{out}");
        // With the operator's own switch on, the line is what was written.
        assert_eq!(redact_log_line(line, true), line);
    }

    #[test]
    fn a_line_with_no_log_header_is_dropped_whole() {
        for line in [
            "thread 'main' panicked at src/core/mod.rs:12: the card said 'deploy to prod'",
            "   3: deckd::core::run",
            "",
            "not a timestamp INFO x: y",
        ] {
            assert_eq!(redact_log_line(line, false), REDACTED_LOG, "{line}");
        }
    }

    #[test]
    fn the_log_tail_says_how_many_lines_it_left_out() {
        let text: String = (0..10)
            .map(|n| format!("2026-09-22T19:42:03.120123Z  INFO deckd: msg n={n}\n"))
            .collect();
        let out = redact_log(&text, false, 3);
        assert!(out.contains("… 7 earlier line(s) not included"), "{out}");
        assert!(out.contains("n=9"), "{out}");
        assert!(!out.contains("n=6"), "{out}");
    }

    #[test]
    fn state_json_loses_its_terminal_map_and_keeps_the_rest() {
        let text = r#"{"slots":{"claude:api":{"slot":1,"cwd":"/Users/t/p","kind":"claude","last_seen":"2026-09-22T00:00:00.000Z"}},"terminal_map":{"ttys003":{"app":"Ghostty","window":42}},"deliveries":{"cards":{"total":4,"outcomes":{"answered":3,"elsewhere":1}},"recent":[{"agent":"ag_7","card":12,"outcome":"answered"}]}}"#;
        let out = redact_state(text, false);
        assert!(out.contains("claude:api"), "{out}");
        assert!(out.contains("/Users/t/p"), "{out}");
        assert!(!out.contains("Ghostty"), "{out}");
        assert!(out.contains(REDACTED), "{out}");
        // The delivery record is content-free by construction, and kept.
        let kept: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(kept["deliveries"]["cards"]["outcomes"]["answered"], 3);
        assert_eq!(kept["deliveries"]["recent"][0]["card"], 12);
        // The operator's own switch keeps it.
        assert!(redact_state(text, true).contains("Ghostty"));
        // Garbage is described, not copied.
        let out = redact_state("{not json", false);
        assert!(out.contains("does not parse"), "{out}");
    }

    /// A deck's id is its Bluetooth address, which a support file has no use
    /// for: each deck is a number, with its strip, settings and keys kept,
    /// and the current one says which number it is. The operator's
    /// `log_content` switch does not bring the addresses back.
    #[test]
    fn state_json_names_decks_by_number_not_by_address() {
        let text = r#"{"faces":[{"f":1,"t":"agents","n":"Agents"}],"deck":"F6E5D4C3B2A1","decks":{
            "A1B2C3D4E5F6":{"faces":[],"settings":{"speaker":3}},
            "F6E5D4C3B2A1":{"faces":[{"f":1,"t":"agents","n":"Agents"}],"settings":{"speaker":6}}}}"#;
        for log_content in [false, true] {
            let out = redact_state(text, log_content);
            let value: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert!(
                !out.contains("A1B2C3D4E5F6") && !out.contains("F6E5D4C3B2A1"),
                "{out}"
            );
            assert_eq!(value["deck"], "deck-2");
            assert_eq!(value["decks"]["deck-1"]["settings"]["speaker"], 3);
            assert_eq!(value["decks"]["deck-2"]["faces"][0]["t"], "agents");
            assert_eq!(value["faces"][0]["n"], "Agents", "the rest is kept");
        }
        // A current deck the file does not list is redacted, not copied;
        // a file from before decks is as it was.
        let out = redact_state(r#"{"deck":"A1B2C3D4E5F6","decks":{}}"#, false);
        assert!(!out.contains("A1B2C3D4E5F6"), "{out}");
        assert!(out.contains(REDACTED), "{out}");
        let old = redact_state(r#"{"card_ids":{"1":5}}"#, false);
        assert!(!old.contains("deck"), "{old}");
    }

    #[test]
    fn the_file_name_is_the_timestamp_flattened_for_a_mail_attachment() {
        let name = file_name(Ts(1_789_760_523_120));
        assert_eq!(name, "OpenWalkieTalkie-diagnostics-20260918-194203.txt");
    }

    #[test]
    fn the_three_logs_are_the_newest_rolled_deckd_log_and_the_other_two() {
        let dir = TestDir::new("diag-logs");
        let logs = dir.sub("logs");
        for name in ["deckd.log.2026-09-20", "deckd.log.2026-09-22", "app.log"] {
            std::fs::write(logs.join(name), b"x").unwrap();
        }
        let files = log_files(&logs);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["deckd.log.2026-09-22", "app.log", "deckd.launchd.log"],
            "the newest rolled log, then the GUI's, then launchd's"
        );
    }

    /// The bundle written next to the data dir is 0600: it is a diagnostic
    /// of a 0700 directory and must not be the world-readable copy of it.
    #[cfg(unix)]
    #[test]
    fn the_written_bundle_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("diag-mode");
        let bundle = Bundle {
            text: "x".into(),
            ..Bundle::default()
        };
        let path = write(&dir.sub("out"), &bundle, Ts(1_789_760_523_120)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
