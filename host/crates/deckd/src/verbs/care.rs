//! `deckd care …`: the Service page's checks, Footprint and two updates, the
//! Integrations page's switches and pause, the Usage face's sources, the
//! tests from this Mac, the permissions, and the decks this Mac keeps a
//! setup for.
//!
//! The switches write what the window's controls write, built by the same
//! rules (the app's `pause_action`, `apps_paused_after` and
//! `UsageConfig::switched`, copied here because the service's crate cannot
//! use the app's): pausing is `sources.paused` alone, resuming turns the
//! older `hooks.enabled` back on in the same patch, and an app's switch
//! rewrites the list of paused apps with every other name kept where it was.
use super::{
    ask, finish, follow, print_json, size_text, text, Service, Stop, ADMIN, OBSERVE, VOICE,
};
use crate::cli::CtlFailure;
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::io::Write;

/// What `deckd care checks` adds when no service answers.
pub const DOCTOR_HINT: &str = "`deckd doctor` checks without the service";

/// How many times a test's `--wait` reads its status, a second apart: the
/// minute the card stays on the deck (`care::test::ANSWER_WITHIN_S`) and a
/// little more.
pub const TEST_READS: u32 = crate::care::test::ANSWER_WITHIN_S + 5;

#[derive(Subcommand, Debug)]
pub enum CareCmd {
    /// The service's own setup checks, the Service page's Check my setup
    /// (`care.checks`). Exit 1 when one fails.
    Checks {
        #[arg(long)]
        json: bool,
    },
    /// Everything the app has put on this Mac, item by item, and how each
    /// goes (`care.footprint`).
    Footprint {
        #[arg(long)]
        json: bool,
    },
    /// Delete one of the service's caches (`care.cache.clear`): `firmware`,
    /// the firmware downloads, with the bootloader backups kept.
    Clear {
        #[arg(value_parser = ["firmware"])]
        what: String,
    },
    /// What puts sessions on the deck: per source its switch, its state,
    /// how many of its sessions are on the deck and its last event
    /// (`sources.list`).
    Sources {
        #[arg(long)]
        json: bool,
    },
    /// Pause integrations: no source puts sessions on the deck until `deckd
    /// care resume`.
    Pause,
    /// Resume integrations: every source that is switched on puts sessions
    /// on the deck again.
    Resume,
    /// Switch one source on or off: `claude`, `claude_channels`, `codex`, or
    /// a connected app as `app:<name>`, the ids `deckd care sources` lists.
    Source { id: String, state: OnOff },
    /// Switch the Usage face's Claude or Codex bars on or off
    /// (`usage.providers`). Claude's are read with your Claude Code login,
    /// from the Keychain.
    Usage { provider: Provider, state: OnOff },
    /// Test the deck from this Mac.
    Test {
        #[command(subcommand)]
        cmd: TestCmd,
    },
    /// The permissions this Mac gives the service (`perm.status`), or, with
    /// `--ask`, have the service ask macOS for one (`perm.request`).
    Permissions {
        #[arg(long = "ask", value_parser = ["microphone", "speech", "accessibility"])]
        request: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The decks this Mac keeps a strip, settings and Macropad keys for, by
    /// label (`care.decks`); `forget` drops one.
    Decks {
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<DecksCmd>,
    },
    /// The Service page's two updates, this app and the deck's firmware, as
    /// the service's own looks for a release found them (`firmware.status`).
    /// With `--check`, the service looks now (`firmware.check`) and the look
    /// is followed to its end, as `deckd firmware check --wait` follows it.
    /// Exit 1 when the look ended in an error, or had not ended in two
    /// minutes.
    Updates {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum TestCmd {
    /// A test question on the deck (`care.test.question`). With `--wait`,
    /// wait up to a minute for ENTER on the deck.
    Question {
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// A card asking to hold PTT and speak (`care.test.voice`). With
    /// `--wait`, wait up to a minute for what the deck heard.
    Voice {
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// A sample sentence spoken on the deck, in its language
    /// (`care.test.say`).
    Say {
        #[arg(long)]
        json: bool,
    },
    /// A chime on the deck's speaker (`voice.test`, the Voice page's Play a
    /// Chime).
    Chime {
        #[arg(long)]
        json: bool,
    },
    /// How the last test question and voice test went (`care.test.status`).
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum DecksCmd {
    /// Forget a deck that is not in use (`care.decks.forget`): its strip,
    /// settings and keys go, and it starts from the firmware's defaults the
    /// next time it links.
    Forget {
        /// Its label, as `deckd care decks` lists it.
        deck: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

impl OnOff {
    fn on(self) -> bool {
        self == OnOff::On
    }

    fn word(self) -> &'static str {
        if self.on() {
            "on"
        } else {
            "off"
        }
    }
}

/// The Usage face's two sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }
}

/// `deckd care …`.
pub fn run(
    service: &dyn Service,
    cmd: CareCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(care(service, cmd, out, err))
}

fn care(
    service: &dyn Service,
    cmd: CareCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        CareCmd::Checks { json } => checks(service, json, out, err),
        CareCmd::Footprint { json } => {
            let reply = ask(service, ADMIN, "care.footprint", json!({}), err)?;
            show(out, &reply, json, footprint_text)?;
            Ok(0)
        }
        CareCmd::Clear { what } => {
            let reply = ask(
                service,
                ADMIN,
                "care.cache.clear",
                json!({ "what": what }),
                err,
            )?;
            writeln!(
                out,
                "cleared the firmware downloads: {} freed; the bootloader backups are kept",
                size_text(reply["removed_bytes"].as_u64().unwrap_or(0))
            )?;
            Ok(0)
        }
        CareCmd::Sources { json } => {
            let reply = ask(service, OBSERVE, "sources.list", json!({}), err)?;
            show(out, &reply, json, sources_text)?;
            Ok(0)
        }
        CareCmd::Pause => {
            ask(service, ADMIN, "config.set", pause_patch(true), err)?;
            writeln!(
                out,
                "paused: no source puts sessions on the deck until `deckd care resume`"
            )?;
            Ok(0)
        }
        CareCmd::Resume => {
            ask(service, ADMIN, "config.set", pause_patch(false), err)?;
            writeln!(
                out,
                "resumed: every source that is switched on puts sessions on the deck again"
            )?;
            Ok(0)
        }
        CareCmd::Source { id, state } => {
            let source = SourceId::parse(&id).map_err(Stop::Refused)?;
            let paused = match &source {
                SourceId::App(_) => {
                    apps_paused(&ask(service, ADMIN, "config.get", json!({}), err)?)
                }
                _ => Vec::new(),
            };
            let patch = source_patch(&source, state.on(), &paused);
            ask(service, ADMIN, "config.set", patch, err)?;
            writeln!(out, "{id}: {}", state.word())?;
            Ok(0)
        }
        CareCmd::Usage { provider, state } => {
            let config = ask(service, ADMIN, "config.get", json!({}), err)?;
            let providers = usage_after(&usage_providers(&config), provider.name(), state.on());
            let patch = json!({ "patch": { "usage.providers": providers } });
            ask(service, ADMIN, "config.set", patch, err)?;
            let list = if providers.is_empty() {
                "no sources".to_string()
            } else {
                providers.join(", ")
            };
            writeln!(out, "Usage face: {list}")?;
            if provider == Provider::Claude && state.on() {
                writeln!(
                    out,
                    "the service reads your Claude Code login from the Keychain to ask Anthropic \
                     for your usage; macOS asks once"
                )?;
            }
            Ok(0)
        }
        CareCmd::Test { cmd } => test(service, cmd, out, err),
        CareCmd::Permissions {
            request: Some(which),
            json,
        } => {
            let reply = ask(
                service,
                ADMIN,
                "perm.request",
                json!({ "which": which }),
                err,
            )?;
            show(out, &reply, json, |reply| {
                let mut said = format!("{which}: {}\n", text(reply, "status"));
                said.push_str(&notes(reply));
                said
            })?;
            Ok(0)
        }
        CareCmd::Permissions {
            request: None,
            json,
        } => {
            let reply = ask(service, VOICE, "perm.status", json!({}), err)?;
            show(out, &reply, json, permissions_text)?;
            Ok(0)
        }
        CareCmd::Decks { json, cmd: None } => {
            let reply = ask(service, ADMIN, "care.decks", json!({}), err)?;
            show(out, &reply, json, decks_text)?;
            Ok(0)
        }
        CareCmd::Decks {
            json,
            cmd:
                Some(DecksCmd::Forget {
                    deck,
                    json: forget_json,
                }),
        } => {
            let reply = ask(
                service,
                ADMIN,
                "care.decks.forget",
                json!({ "deck": deck }),
                err,
            )?;
            show(out, &reply, json || forget_json, |reply| {
                format!(
                    "forgot deck {}: it starts from the firmware's defaults the next time it \
                     links\n",
                    text(reply, "deck")
                )
            })?;
            Ok(0)
        }
        CareCmd::Updates { check, json } => {
            let mut status = if check {
                ask(
                    service,
                    ADMIN,
                    "firmware.check",
                    json!({ "force": true }),
                    err,
                )?
            } else {
                ask(service, OBSERVE, "firmware.status", json!({}), err)?
            };
            // A look the check started is followed to its end: its first
            // answer only says it started.
            let following = check && super::firmware::busy(&status);
            if following {
                status = super::firmware::follow_status(
                    service,
                    status,
                    super::firmware::CHECK_READS,
                    json,
                    out,
                    err,
                )?;
            }
            show(out, &status, json, updates_text)?;
            let unfinished = following && super::firmware::busy(&status);
            Ok(i32::from(super::firmware::failed(&status) || unfinished))
        }
    }
}

/// `deckd care updates`: this app's line when the service compares it with
/// the release feed (`firmware.status`'s `app`), none from a service that
/// does not, then the firmware's status as `deckd firmware status` says it.
#[must_use]
pub fn updates_text(status: &Value) -> String {
    let app = &status["app"];
    let mut words = String::new();
    if app.is_object() {
        if app["available"] == true {
            let version = text(app, "version");
            match text(app, "release_url") {
                "" => words.push_str(&format!("app: OpenWalkieTalkie {version} is available\n")),
                url => words.push_str(&format!(
                    "app: OpenWalkieTalkie {version} is available: {url}\n"
                )),
            }
        } else {
            words.push_str("app: up to date\n");
        }
    }
    words.push_str(&super::firmware::status_text(status));
    words
}

/// `reply` as JSON, or as `words` says it.
fn show(
    out: &mut dyn Write,
    reply: &Value,
    json: bool,
    words: impl FnOnce(&Value) -> String,
) -> Result<(), Stop> {
    if json {
        print_json(out, reply)
    } else {
        write!(out, "{}", words(reply))?;
        Ok(())
    }
}

/// `deckd care checks`: exit 1 when a check failed. With no service, the
/// doctor is the one that checks.
fn checks(
    service: &dyn Service,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    let reply = match service.call(ADMIN, "care.checks", json!({})) {
        Ok(reply) => reply,
        Err(failure) => {
            let unreachable = matches!(failure, CtlFailure::Unreachable(_));
            let code = crate::cli::report(failure, err);
            if unreachable {
                writeln!(err, "deckd: {DOCTOR_HINT}")?;
            }
            return Err(Stop::Failed(code));
        }
    };
    let (words, code) = checks_text(&reply);
    show(out, &reply, json, |_| words)?;
    Ok(code)
}

/// `care.checks` as the doctor prints its rows (`ok  `, `FAIL`, and `--  `
/// for a row the service did not check), and the exit code: 1 when a row
/// failed.
#[must_use]
pub fn checks_text(reply: &Value) -> (String, i32) {
    let mut words = String::new();
    let mut failed = false;
    for check in reply["checks"].as_array().into_iter().flatten() {
        let mark = match check["ok"].as_bool() {
            Some(true) => "ok  ",
            Some(false) => {
                failed = true;
                "FAIL"
            }
            None => "--  ",
        };
        words.push_str(&format!(
            "{mark} {}: {}\n",
            text(check, "name"),
            text(check, "detail")
        ));
    }
    (words, i32::from(failed))
}

/// How a Footprint row's thing goes, as the page's button says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Goes {
    /// The installer's: its uninstall takes it back out.
    Remove(&'static str),
    /// One of the service's caches, deleted on its own.
    Delete(&'static str),
    /// Only with everything else.
    WithUninstall,
    /// Uninstall or not.
    Kept,
}

/// The Footprint page's rows in its order (`deck-gui-core`'s
/// `FOOTPRINT_IDS`), with the page's name for each and its button.
const FOOTPRINT: [(&str, &str, Goes); 11] = [
    ("service", "Background service", Goes::WithUninstall),
    (
        "claude_plugin",
        "Claude Code plugin",
        Goes::Remove("deckd integrations uninstall claude"),
    ),
    (
        "codex_hooks",
        "Leftover Codex setup",
        Goes::Remove("deckd integrations uninstall codex"),
    ),
    (
        "codex_desktop",
        "Codex Desktop",
        Goes::Remove("deckd integrations uninstall codex-desktop"),
    ),
    ("stt", "Whisper model", Goes::Delete("deckd stt remove")),
    ("kokoro", "Kokoro voice", Goes::Delete("deckd tts remove")),
    (
        "firmware",
        "Firmware downloads",
        Goes::Delete("deckd care clear firmware (the bootloader backups are kept)"),
    ),
    ("data", "Settings and backups", Goes::WithUninstall),
    ("prefs", "Preferences", Goes::WithUninstall),
    ("keychain", "Keychain item", Goes::WithUninstall),
    ("logs", "Logs", Goes::Kept),
];

/// `care.footprint` as the Footprint page lists it: a row for each item on
/// this Mac, in the page's order, with its size or count, where it is and
/// how it goes; then how many there are and what they weigh. An item a
/// newer service adds comes last, by its id.
#[must_use]
pub fn footprint_text(reply: &Value) -> String {
    let items: Vec<&Value> = reply["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["present"] == true)
                .collect()
        })
        .unwrap_or_default();
    let mut rows: Vec<(&Value, &str, Option<Goes>)> = FOOTPRINT
        .iter()
        .filter_map(|(id, name, goes)| {
            let item = items.iter().find(|item| item["id"] == *id)?;
            Some((*item, *name, Some(*goes)))
        })
        .collect();
    for item in &items {
        if !FOOTPRINT.iter().any(|(id, ..)| item["id"] == *id) {
            rows.push((item, text(item, "id"), None));
        }
    }
    let mut words = String::new();
    let mut bytes = 0u64;
    for (item, name, goes) in &rows {
        bytes += item["bytes"].as_u64().unwrap_or(0);
        let count = item["count"].as_u64();
        let size = match (text(item, "id"), count) {
            ("claude_plugin", Some(1)) => Some("1 hook".to_string()),
            ("claude_plugin", Some(n)) => Some(format!("{n} hooks")),
            ("codex_hooks", Some(1)) => Some("1 entry".to_string()),
            ("codex_hooks", Some(n)) => Some(format!("{n} entries")),
            _ => item["bytes"].as_u64().map(size_text),
        };
        let place = if text(item, "id") == "keychain" {
            format!("your cloud voice key, {}", text(item, "path"))
        } else {
            text(item, "path").to_string()
        };
        match size {
            Some(size) => words.push_str(&format!("{name} ({size}): {place}\n")),
            None => words.push_str(&format!("{name}: {place}\n")),
        }
        match goes {
            Some(Goes::Remove(command)) => words.push_str(&format!("  remove: {command}\n")),
            Some(Goes::Delete(command)) => words.push_str(&format!("  delete: {command}\n")),
            Some(Goes::WithUninstall) => {
                words.push_str("  goes with: deckd uninstall --purge\n");
            }
            Some(Goes::Kept) => words.push_str("  kept: deckd uninstall leaves the logs\n"),
            None => {}
        }
    }
    let items = if rows.len() == 1 { "item" } else { "items" };
    words.push_str(&format!("{} {items}, {}\n", rows.len(), size_text(bytes)));
    words
}

/// `sources.list` a source to a line: its id (and its name, when the id is
/// not just the name), its switch and state, its counts and its last event;
/// the pause first, when it is on.
#[must_use]
pub fn sources_text(reply: &Value) -> String {
    let mut words = String::new();
    if reply["paused"] == true {
        words.push_str("paused: no source puts sessions on the deck until `deckd care resume`\n");
    }
    for row in reply["sources"].as_array().into_iter().flatten() {
        let (id, name) = (text(row, "id"), text(row, "name"));
        let label = if id == format!("app:{name}") || name.is_empty() {
            id.to_string()
        } else {
            format!("{id} ({name})")
        };
        let switch = if row["enabled"] == true { "on" } else { "off" };
        let state = row["state"]
            .as_str()
            .map(|state| format!(", {state}"))
            .unwrap_or_default();
        let last = row["last_event_at"].as_str().unwrap_or("never");
        words.push_str(&format!(
            "{label}: {switch}{state}; {} of {} {} on the deck; last event {last}\n",
            row["on_deck"].as_u64().unwrap_or(0),
            row["sessions"].as_u64().unwrap_or(0),
            row["unit"].as_str().unwrap_or("sessions"),
        ));
        if let Some(stale) = row["stale"].as_u64().filter(|stale| *stale > 0) {
            words.push_str(&format!(
                "  {stale} sessions use an old helper: restart them\n"
            ));
        }
    }
    if let Some(other) = reply["other"].as_u64().filter(|other| *other > 0) {
        words.push_str(&format!("other adapters: {other} sessions\n"));
    }
    words
}

/// What **Pause integrations** writes, on (`true`) and off: pausing is
/// `sources.paused` alone; resuming turns `hooks.enabled` back on as well,
/// so a pause made with the older switch ends too, in one patch.
#[must_use]
pub fn pause_patch(pause: bool) -> Value {
    if pause {
        json!({ "patch": { "sources.paused": true } })
    } else {
        json!({ "patch": { "sources.paused": false, "hooks.enabled": true } })
    }
}

/// A source by the id `sources.list` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceId {
    Claude,
    ClaudeChannels,
    Codex,
    /// A connected app, by the name it is listed under.
    App(String),
}

impl SourceId {
    /// `claude`, `claude_channels`, `codex` or `app:<name>`; anything else is
    /// refused before the service is asked.
    pub fn parse(id: &str) -> Result<SourceId, String> {
        match id {
            "claude" => Ok(SourceId::Claude),
            "claude_channels" => Ok(SourceId::ClaudeChannels),
            "codex" => Ok(SourceId::Codex),
            other => match other.strip_prefix("app:") {
                Some(name) if !name.trim().is_empty() => Ok(SourceId::App(name.to_string())),
                _ => Err(format!(
                    "{id} is not a source: claude, claude_channels, codex, or app:<name> (`deckd \
                     care sources` lists them)"
                )),
            },
        }
    }

    /// The config key its switch writes.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            SourceId::Claude => "sources.claude",
            SourceId::ClaudeChannels => "sources.claude_channels",
            SourceId::Codex => "codex.daemon",
            SourceId::App(_) => "sources.apps_paused",
        }
    }
}

/// What switching `source` on or off writes: a boolean for the three
/// tools, the whole list of paused apps for an app (`paused` is the list
/// `config.get` reads now).
#[must_use]
pub fn source_patch(source: &SourceId, on: bool, paused: &[String]) -> Value {
    let value = match source {
        SourceId::App(name) => json!(apps_paused_after(paused, name, on)),
        _ => json!(on),
    };
    let mut patch = serde_json::Map::new();
    patch.insert(source.key().into(), value);
    json!({ "patch": patch })
}

/// The paused apps once `name` is switched on (out of the list) or off
/// (onto its end), every other name kept where it was.
#[must_use]
pub fn apps_paused_after(paused: &[String], name: &str, on: bool) -> Vec<String> {
    let mut list: Vec<String> = paused
        .iter()
        .filter(|p| p.as_str() != name)
        .cloned()
        .collect();
    if !on {
        list.push(name.to_string());
    }
    list
}

/// `config.get`'s `sources.apps_paused`.
fn apps_paused(config: &Value) -> Vec<String> {
    strings(&config["sources"]["apps_paused"])
}

/// `config.get`'s `usage.providers`.
fn usage_providers(config: &Value) -> Vec<String> {
    strings(&config["usage"]["providers"])
}

fn strings(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect()
}

/// `usage.providers` once `provider` is switched on or off: the other names
/// stay as and where they are; on, it is added at the end; every variant of
/// it (`claude-statusline`) goes either way, as the service reads a name.
#[must_use]
pub fn usage_after(providers: &[String], provider: &str, on: bool) -> Vec<String> {
    let names = |name: &String| {
        let name = name.trim().to_ascii_lowercase();
        name == provider || name.starts_with(&format!("{provider}-"))
    };
    let mut list: Vec<String> = providers
        .iter()
        .filter(|name| !names(name))
        .cloned()
        .collect();
    if on {
        list.push(provider.to_string());
    }
    list
}

/// One of the two tests that wait for the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    Question,
    Voice,
}

impl Test {
    fn method(self) -> &'static str {
        match self {
            Test::Question => "care.test.question",
            Test::Voice => "care.test.voice",
        }
    }

    /// Its half of `care.test.status`.
    fn key(self) -> &'static str {
        match self {
            Test::Question => "question",
            Test::Voice => "voice",
        }
    }

    /// The state it is in while its card is on the deck.
    fn on_the_deck(self) -> &'static str {
        match self {
            Test::Question => "sent",
            Test::Voice => "waiting",
        }
    }
}

/// What a test's start says: where its card went, or why there is none.
#[must_use]
pub fn started_text(test: Test, state: &str) -> String {
    match (test, state) {
        (Test::Question, "sent") => "sent: press ENTER on the deck".into(),
        (Test::Voice, "waiting") => "sent: hold PTT on the deck and say hello".into(),
        (_, "no_deck") => "no deck is linked: nothing was sent".into(),
        (_, other) => format!("the service answered {other:?}"),
    }
}

/// How a test went, from its record in `care.test.status` (`null` before
/// the first).
#[must_use]
pub fn record_text(record: &Value) -> String {
    if record.is_null() {
        return "none since the service started".into();
    }
    let said = match text(record, "state") {
        "sent" | "waiting" => "on the deck now".to_string(),
        "answered" => format!(
            "answered after {:.1} s",
            record["after_ms"].as_u64().unwrap_or(0) as f64 / 1000.0
        ),
        "heard" => match text(record, "heard") {
            "" => "heard nothing".to_string(),
            heard => format!("heard: \"{heard}\""),
        },
        "expired" => "expired: the deck did not answer within a minute".into(),
        "no_deck" => "no deck is linked: nothing was sent".into(),
        other => other.to_string(),
    };
    match record["at"].as_str() {
        Some(at) => format!("{said} ({at})"),
        None => said,
    }
}

/// How a test that ended went, as an exit code: 0 for an answer and for
/// words heard, 1 for anything else.
#[must_use]
pub fn record_code(record: &Value) -> i32 {
    match text(record, "state") {
        "answered" => 0,
        "heard" if !text(record, "heard").is_empty() => 0,
        _ => 1,
    }
}

/// Where a followed test stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Ended {
    /// Its record, once it left the deck.
    Record(Value),
    /// Another test took its place: the record is someone else's now.
    Replaced,
}

/// `status` read while following `test`, whose record was first seen at
/// `at` (set from the first read): `None` while its card is still on the
/// deck.
pub fn settled(test: Test, status: &Value, at: &mut Option<String>) -> Option<Ended> {
    let record = &status[test.key()];
    let seen = record["at"].as_str()?.to_string();
    if at.get_or_insert_with(|| seen.clone()) != &seen {
        return Some(Ended::Replaced);
    }
    (text(record, "state") != test.on_the_deck()).then(|| Ended::Record(record.clone()))
}

fn test(
    service: &dyn Service,
    cmd: TestCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        TestCmd::Question { wait, json } => start(service, Test::Question, wait, json, out, err),
        TestCmd::Voice { wait, json } => start(service, Test::Voice, wait, json, out, err),
        TestCmd::Say { json } => {
            let reply = ask(service, ADMIN, "care.test.say", json!({}), err)?;
            show(out, &reply, json, |_| {
                "queued: the deck speaks a sample sentence in its language\n".into()
            })?;
            Ok(0)
        }
        TestCmd::Chime { json } => {
            let reply = ask(
                service,
                VOICE,
                "voice.test",
                json!({"what": "speaker"}),
                err,
            )?;
            let played = reply["played"] == true;
            show(out, &reply, json, |reply| {
                let mut said = if played {
                    "a chime was queued on the deck's speaker\n".to_string()
                } else {
                    "the chime was not queued: the audio side refused it\n".to_string()
                };
                said.push_str(&notes(reply));
                said
            })?;
            Ok(i32::from(!played))
        }
        TestCmd::Status { json } => {
            let reply = ask(service, ADMIN, "care.test.status", json!({}), err)?;
            show(out, &reply, json, |reply| {
                format!(
                    "question: {}\nvoice: {}\n",
                    record_text(&reply["question"]),
                    record_text(&reply["voice"])
                )
            })?;
            Ok(0)
        }
    }
}

/// A test question or a voice test, and with `wait` its outcome.
fn start(
    service: &dyn Service,
    test: Test,
    wait: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    let reply = ask(service, ADMIN, test.method(), json!({}), err)?;
    let state = text(&reply, "state");
    let on_the_deck = state == test.on_the_deck();
    if !(wait && on_the_deck) {
        show(out, &reply, json, |_| {
            format!("{}\n", started_text(test, state))
        })?;
        return Ok(i32::from(!on_the_deck));
    }
    if !json {
        writeln!(out, "{}", started_text(test, state))?;
    }
    let mut at = None;
    let read = || ask(service, ADMIN, "care.test.status", json!({}), &mut *err);
    let ended = follow(
        service,
        read,
        |status| settled(test, status, &mut at),
        TEST_READS,
    )?;
    match ended {
        Some(Ended::Record(record)) => {
            show(out, &record, json, |record| {
                format!("{}\n", record_text(record))
            })?;
            Ok(record_code(&record))
        }
        Some(Ended::Replaced) => {
            writeln!(out, "another test took its place on the deck")?;
            Ok(1)
        }
        None => {
            writeln!(
                out,
                "still on the deck after {TEST_READS} s: `deckd care test status` reads how it \
                 went"
            )?;
            Ok(1)
        }
    }
}

/// A reply's `limits`, a `note:` line each.
fn notes(reply: &Value) -> String {
    reply["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|limit| format!("note: {limit}\n"))
        .collect()
}

/// `perm.status`: a line per permission the service reports, then what the
/// answer cannot tell.
#[must_use]
pub fn permissions_text(reply: &Value) -> String {
    let mut words = String::new();
    // No Automation grant: the hand-off raises an app and sends no Apple
    // Event, so the service no longer reports one (an older service's
    // `automation` is not read).
    for name in ["microphone", "speech", "bluetooth", "accessibility"] {
        if let Some(grant) = reply[name].as_str() {
            words.push_str(&format!("{name}: {grant}\n"));
        }
    }
    words.push_str(&notes(reply));
    words
}

/// `care.decks`: a deck to two lines, the one in use marked; or what
/// happens when there is none yet.
#[must_use]
pub fn decks_text(reply: &Value) -> String {
    let decks = reply["decks"].as_array().cloned().unwrap_or_default();
    if decks.is_empty() {
        return "no deck is remembered here yet: the first deck identified takes this Mac's \
                strip, settings and keys\n"
            .into();
    }
    let mut words = String::new();
    for deck in &decks {
        let in_use = if deck["current"] == true {
            " (in use)"
        } else {
            ""
        };
        let last = &deck["last_linked"];
        let linked = match last["at"].as_str() {
            None => "never linked here".to_string(),
            Some(at) => {
                let mut linked = format!("last linked {at}");
                if let Some(transport) = last["transport"].as_str() {
                    linked.push_str(&format!(" over {transport}"));
                }
                if let Some(board) = last["board"].as_str() {
                    linked.push_str(&format!(", {board}"));
                }
                if let Some(fw) = last["fw"].as_str() {
                    linked.push_str(&format!(", firmware {fw}"));
                }
                linked
            }
        };
        words.push_str(&format!("{}{in_use}: {linked}\n", text(deck, "deck")));
        let faces: Vec<&str> = deck["faces"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|face| text(face, "n"))
            .collect();
        let strip = if faces.is_empty() {
            "no strip kept yet".to_string()
        } else {
            format!("strip: {}", faces.join(", "))
        };
        let keys = match deck["macros"].as_u64().unwrap_or(0) {
            0 => String::new(),
            1 => "; keys for 1 Macropad".into(),
            n => format!("; keys for {n} Macropads"),
        };
        words.push_str(&format!("  {strip}{keys}\n"));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{fixture, run, Script};
    use super::*;
    use clap::Parser;
    use deck_model::ApiError;

    fn parse(args: &[&str]) -> Result<CareCmd, clap::Error> {
        let mut argv = vec!["deckd", "care"];
        argv.extend_from_slice(args);
        crate::cli::Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            crate::cli::Cmd::Care { cmd } => cmd,
            other => panic!("{other:?}"),
        })
    }

    fn care_run(script: &Script, args: &[&str]) -> super::super::testkit::Ran {
        let cmd = parse(args).unwrap();
        run(|out, err| super::run(script, cmd, out, err))
    }

    /// Every verb parses, and what is not one of them is refused by the
    /// parser before anything is asked.
    #[test]
    fn every_care_verb_parses_and_a_wrong_word_is_refused() {
        for args in [
            &["checks"][..],
            &["checks", "--json"],
            &["footprint", "--json"],
            &["clear", "firmware"],
            &["sources"],
            &["pause"],
            &["resume"],
            &["source", "claude", "off"],
            &["source", "app:Omarchy remote", "on"],
            &["usage", "claude", "on"],
            &["usage", "codex", "off"],
            &["test", "question", "--wait"],
            &["test", "voice", "--json"],
            &["test", "say"],
            &["test", "chime"],
            &["test", "status", "--json"],
            &["permissions"],
            &["permissions", "--ask", "accessibility"],
            &["decks"],
            &["decks", "--json"],
            &["decks", "forget", "3f2a9c1d"],
            &["updates"],
            &["updates", "--check", "--json"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}: {:?}", parse(args).err());
        }
        for args in [
            &["clear", "stt"][..],
            &["source", "claude", "maybe"],
            &["usage", "gemini", "on"],
            &["permissions", "--ask", "bluetooth"],
            &["test"],
            &["decks", "forget"],
            &["updates", "now"],
            &[],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        assert!(matches!(
            parse(&["source", "codex", "on"]).unwrap(),
            CareCmd::Source { id, state: OnOff::On } if id == "codex"
        ));
    }

    /// `checks` prints the doctor's marks and fails when a row failed; a
    /// row the service did not check is neither. With no service it points
    /// at the doctor and exits 2.
    #[test]
    fn the_checks_print_the_doctors_marks_and_fail_when_one_failed() {
        let replies = fixture("care_checks.json");
        let (words, code) = checks_text(&replies);
        assert_eq!(code, 0);
        assert!(
            words.starts_with(
                "ok   data dir: /Users/you/Library/Application Support/OpenWalkieTalkie\n"
            ),
            "{words}"
        );
        assert_eq!(
            words.lines().count(),
            replies["checks"].as_array().unwrap().len(),
            "a row a line"
        );
        let mixed = json!({"checks": [
            {"name": "device", "ok": null, "detail": "not looked for: --no-device"},
            {"name": "socket", "ok": false, "detail": "absent"},
        ]});
        assert_eq!(
            checks_text(&mixed),
            (
                "--   device: not looked for: --no-device\nFAIL socket: absent\n".to_string(),
                1
            )
        );
        let script = Script::new(vec![Ok(mixed.clone())]);
        let ran = care_run(&script, &["checks"]);
        assert_eq!(ran.result, Ok(1));
        assert_eq!(script.call(0).scopes, ["admin"]);
        let script = Script::new(vec![Ok(mixed.clone())]);
        let ran = care_run(&script, &["checks", "--json"]);
        assert_eq!(
            (ran.result, serde_json::from_str::<Value>(&ran.out).unwrap()),
            (Ok(1), mixed)
        );
        let script = Script::new(vec![Err(CtlFailure::Unreachable("no socket".into()))]);
        let ran = care_run(&script, &["checks"]);
        assert_eq!(ran.result, Ok(2));
        assert_eq!(ran.out, "");
        assert_eq!(ran.err, format!("deckd: no socket\ndeckd: {DOCTOR_HINT}\n"));
    }

    /// The footprint reads as the page does: the page's order and names, a
    /// count for the plugin and the Codex setup, a size for the rest, where
    /// each thing is, how it goes, and the total of what is there.
    #[test]
    fn the_footprint_lists_what_is_there_in_the_pages_order_with_how_each_goes() {
        let words = footprint_text(&fixture("care_footprint.json"));
        let lines: Vec<&str> = words.lines().collect();
        assert_eq!(
            lines[0],
            "Background service: /Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"
        );
        assert_eq!(lines[1], "  goes with: deckd uninstall --purge");
        assert_eq!(
            lines[2],
            "Claude Code plugin (13 hooks): /Users/you/Library/Application \
             Support/OpenWalkieTalkie/claude-marketplace"
        );
        assert_eq!(lines[3], "  remove: deckd integrations uninstall claude");
        assert_eq!(
            lines[4],
            "Leftover Codex setup (7 entries): /Users/you/.codex/hooks.json"
        );
        assert!(words.contains("Whisper model (1.6 GB): /Users/you/Library/Caches/OpenWalkieTalkie/stt\n  delete: deckd stt remove\n"), "{words}");
        assert!(words.contains("Kokoro voice (428 MB):"), "{words}");
        assert!(words.contains(
            "Firmware downloads (9 MB): /Users/you/Library/Caches/OpenWalkieTalkie/firmware\n  \
             delete: deckd care clear firmware (the bootloader backups are kept)\n"
        ));
        assert!(words.contains(
            "Keychain item: your cloud voice key, ai.cohdl.openwalkietalkie\n  goes with: deckd \
             uninstall --purge\n"
        ));
        assert!(
            !words.contains("Codex Desktop"),
            "absent, so not listed: {words}"
        );
        // Logs last, as the page has them; then ten items and their weight.
        assert_eq!(
            lines[lines.len() - 3],
            "Logs (81 MB): /Users/you/Library/Logs/OpenWalkieTalkie"
        );
        assert_eq!(
            lines[lines.len() - 2],
            "  kept: deckd uninstall leaves the logs"
        );
        assert_eq!(lines[lines.len() - 1], "10 items, 2.1 GB");
        // An item of a newer service is listed by its id, last.
        let newer = json!({"items": [
            {"id": "data", "path": "/d", "bytes": 1000, "count": null, "present": true},
            {"id": "spare", "path": "/s", "bytes": 2000, "count": null, "present": true},
        ]});
        assert_eq!(
            footprint_text(&newer),
            "Settings and backups (1 KB): /d\n  goes with: deckd uninstall --purge\nspare (2 KB): \
             /s\n2 items, 3 KB\n"
        );
    }

    /// The sources a line each, by the id `deckd care source` takes, with the
    /// pause first and the helpers that need a restart.
    #[test]
    fn the_sources_read_a_line_each_by_the_id_the_switch_takes() {
        let words = sources_text(&fixture("sources_list.json"));
        let lines: Vec<&str> = words.lines().collect();
        assert_eq!(
            lines[0],
            "claude (Claude Code): on; 3 of 3 sessions on the deck; last event \
             2026-11-11T10:01:56.000Z"
        );
        assert_eq!(
            lines[1],
            "claude_channels (Claude Channels): on, connected; 0 of 0 sessions on the deck; last \
             event never"
        );
        assert_eq!(lines[2], "  2 sessions use an old helper: restart them");
        assert_eq!(
            lines[3],
            "codex (Codex CLI): on, connected; 2 of 2 threads on the deck; last event \
             2026-11-11T10:01:00.000Z"
        );
        assert_eq!(
            lines[4],
            "app:Conol: on, connected; 7 of 7 chats on the deck; last event 2026-11-11T10:01:48.000Z"
        );
        assert_eq!(lines.len(), 6);
        let paused = json!({"paused": true, "sources": [], "other": 2});
        assert_eq!(
            sources_text(&paused),
            "paused: no source puts sessions on the deck until `deckd care resume`\nother \
             adapters: 2 sessions\n"
        );
        // Read with the least scope, so an agent's terminal reads it too.
        let script = Script::new(vec![Ok(fixture("sources_list.json"))]);
        let ran = care_run(&script, &["sources"]);
        assert_eq!((ran.result, ran.out), (Ok(0), words));
        assert_eq!(script.call(0).scopes, Vec::<String>::new());
    }

    /// Pausing and resuming write what the menu and the Integrations page
    /// write, in one patch each.
    #[test]
    fn pause_and_resume_write_what_the_window_writes() {
        assert_eq!(
            pause_patch(true),
            json!({"patch": {"sources.paused": true}})
        );
        assert_eq!(
            pause_patch(false),
            json!({"patch": {"sources.paused": false, "hooks.enabled": true}})
        );
        let script = Script::new(vec![Ok(json!({})), Ok(json!({}))]);
        let paused = care_run(&script, &["pause"]);
        let resumed = care_run(&script, &["resume"]);
        assert_eq!(
            (paused.result, paused.out.as_str()),
            (
                Ok(0),
                "paused: no source puts sessions on the deck until `deckd care resume`\n"
            )
        );
        assert_eq!(
            (resumed.result, resumed.out.as_str()),
            (
                Ok(0),
                "resumed: every source that is switched on puts sessions on the deck again\n"
            )
        );
        assert_eq!(script.call(0).params, pause_patch(true));
        assert_eq!(script.call(1).params, pause_patch(false));
        assert_eq!(script.methods(), ["config.set", "config.set"]);
    }

    /// A source's switch is its own key; an app's rewrites the list of
    /// paused apps by the app's own rule, the others kept in place. An id
    /// that names no source is refused before the service is asked.
    #[test]
    fn a_sources_switch_writes_its_key_and_an_apps_rewrites_the_list() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            source_patch(&SourceId::parse("claude").unwrap(), false, &none),
            json!({"patch": {"sources.claude": false}})
        );
        assert_eq!(
            source_patch(&SourceId::parse("claude_channels").unwrap(), true, &none),
            json!({"patch": {"sources.claude_channels": true}})
        );
        assert_eq!(
            source_patch(&SourceId::parse("codex").unwrap(), false, &none),
            json!({"patch": {"codex.daemon": false}})
        );
        let omarchy = vec!["Omarchy".to_string()];
        let conol = SourceId::parse("app:Conol").unwrap();
        assert_eq!(conol, SourceId::App("Conol".into()));
        assert_eq!(
            source_patch(&conol, false, &omarchy),
            json!({"patch": {"sources.apps_paused": ["Omarchy", "Conol"]}})
        );
        let both = vec!["Conol".to_string(), "Omarchy".to_string()];
        assert_eq!(apps_paused_after(&both, "Conol", true), ["Omarchy"]);
        assert_eq!(
            apps_paused_after(&both, "Conol", false),
            ["Omarchy", "Conol"]
        );
        assert_eq!(apps_paused_after(&omarchy, "Conol", true), ["Omarchy"]);
        for id in ["gemini", "app:", "app: ", "Claude", ""] {
            assert!(SourceId::parse(id).is_err(), "{id:?}");
        }
        let script = Script::new(Vec::new());
        let ran = care_run(&script, &["source", "kimi", "off"]);
        assert_eq!(
            ran.result,
            Err(
                "kimi is not a source: claude, claude_channels, codex, or app:<name> (`deckd care \
                 sources` lists them)"
                    .into()
            )
        );
        assert!(script.methods().is_empty(), "nothing was asked");
        // An app's switch reads the list first; a tool's does not.
        let script = Script::new(vec![
            Ok(json!({"sources": {"apps_paused": ["Omarchy"]}})),
            Ok(json!({})),
        ]);
        let ran = care_run(&script, &["source", "app:Conol", "off"]);
        assert_eq!((ran.result, ran.out.as_str()), (Ok(0), "app:Conol: off\n"));
        assert_eq!(script.methods(), ["config.get", "config.set"]);
        assert_eq!(
            script.call(1).params,
            json!({"patch": {"sources.apps_paused": ["Omarchy", "Conol"]}})
        );
    }

    /// The Usage face's sources by the panel's rule: on adds the name at the
    /// end, off takes it and every variant of it, the others stay in place.
    #[test]
    fn the_usage_sources_switch_by_the_panels_rule() {
        let list = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            usage_after(&list(&["codex"]), "claude", true),
            ["codex", "claude"]
        );
        assert_eq!(
            usage_after(&list(&["claude-statusline", "codex"]), "claude", false),
            ["codex"]
        );
        assert_eq!(
            usage_after(&list(&["Claude", "codex"]), "claude", true),
            ["codex", "claude"]
        );
        assert!(usage_after(&list(&["codex"]), "codex", false).is_empty());
        let script = Script::new(vec![
            Ok(json!({"usage": {"providers": ["codex"]}})),
            Ok(json!({})),
        ]);
        let ran = care_run(&script, &["usage", "claude", "on"]);
        assert_eq!(ran.result, Ok(0));
        assert_eq!(
            ran.out,
            "Usage face: codex, claude\nthe service reads your Claude Code login from the \
             Keychain to ask Anthropic for your usage; macOS asks once\n"
        );
        assert_eq!(
            script.call(1).params,
            json!({"patch": {"usage.providers": ["codex", "claude"]}})
        );
        let script = Script::new(vec![
            Ok(json!({"usage": {"providers": ["codex"]}})),
            Ok(json!({})),
        ]);
        let ran = care_run(&script, &["usage", "codex", "off"]);
        assert_eq!(ran.out, "Usage face: no sources\n");
    }

    /// A test question with no deck sends nothing and exits 1; one on the
    /// deck says where to press. With `--wait` it follows the status until
    /// the deck answers, and says how long that took.
    #[test]
    fn a_test_question_says_where_to_press_and_waits_for_the_answer() {
        let script = Script::new(vec![Ok(json!({"state": "no_deck"}))]);
        let ran = care_run(&script, &["test", "question", "--wait"]);
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (Ok(1), "no deck is linked: nothing was sent\n")
        );
        assert_eq!(
            script.methods(),
            ["care.test.question"],
            "no wait without a card"
        );
        let script = Script::new(vec![Ok(json!({"state": "sent"}))]);
        let ran = care_run(&script, &["test", "question"]);
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (Ok(0), "sent: press ENTER on the deck\n")
        );
        let on_deck =
            json!({"question": {"state": "sent", "at": "t1", "after_ms": null}, "voice": null});
        let answered =
            json!({"question": {"state": "answered", "at": "t1", "after_ms": 3120}, "voice": null});
        let script = Script::new(vec![
            Ok(json!({"state": "sent"})),
            Ok(on_deck.clone()),
            Ok(on_deck),
            Ok(answered.clone()),
        ]);
        let ran = care_run(&script, &["test", "question", "--wait"]);
        assert_eq!(ran.result, Ok(0));
        assert_eq!(
            ran.out,
            "sent: press ENTER on the deck\nanswered after 3.1 s (t1)\n"
        );
        assert_eq!(*script.pauses.borrow(), 2);
        // `--json` with `--wait` is the test's record as it ended.
        let script = Script::new(vec![Ok(json!({"state": "sent"})), Ok(answered.clone())]);
        let ran = care_run(&script, &["test", "question", "--wait", "--json"]);
        assert_eq!(
            serde_json::from_str::<Value>(&ran.out).unwrap(),
            answered["question"]
        );
    }

    /// A voice test ends in words heard (exit 0), nothing heard, the minute
    /// out, or another test in its place (exit 1).
    #[test]
    fn a_voice_test_ends_in_words_heard_or_says_why_not() {
        let record = |state: &str, heard: Value| json!({"question": null, "voice": {"state": state, "at": "t2", "heard": heard}});
        for (end, said, code) in [
            (record("heard", json!("hello")), "heard: \"hello\" (t2)", 0),
            (record("heard", json!("")), "heard nothing (t2)", 1),
            (
                record("expired", Value::Null),
                "expired: the deck did not answer within a minute (t2)",
                1,
            ),
        ] {
            let script = Script::new(vec![
                Ok(json!({"state": "waiting"})),
                Ok(record("waiting", Value::Null)),
                Ok(end),
            ]);
            let ran = care_run(&script, &["test", "voice", "--wait"]);
            assert_eq!(ran.result, Ok(code), "{said}");
            assert_eq!(
                ran.out,
                format!("sent: hold PTT on the deck and say hello\n{said}\n")
            );
        }
        let mut at = None;
        assert_eq!(
            settled(Test::Voice, &record("waiting", Value::Null), &mut at),
            None
        );
        let later = json!({"voice": {"state": "waiting", "at": "t3", "heard": null}});
        assert_eq!(settled(Test::Voice, &later, &mut at), Some(Ended::Replaced));
        assert_eq!(
            settled(Test::Voice, &json!({"voice": null}), &mut None),
            None
        );
    }

    /// The status of both tests, from the app's fixture and before any.
    #[test]
    fn the_test_status_says_how_each_test_went() {
        let script = Script::new(vec![Ok(fixture("care_test_status.json"))]);
        let ran = care_run(&script, &["test", "status"]);
        assert_eq!(
            ran.out,
            "question: answered after 3.1 s (2026-11-11T09:58:12.000Z)\nvoice: heard: \"hello\" \
             (2026-11-11T09:59:40.000Z)\n"
        );
        let script = Script::new(vec![Ok(fixture("recorded/care.test.status.json"))]);
        let ran = care_run(&script, &["test", "status"]);
        assert_eq!(
            ran.out,
            "question: none since the service started\nvoice: none since the service started\n"
        );
        let recorded = fixture("recorded/care.test.status.after_question.json");
        assert_eq!(
            record_text(&recorded["question"]),
            format!(
                "no deck is linked: nothing was sent ({})",
                recorded["question"]["at"].as_str().unwrap()
            )
        );
    }

    /// The chime is the Voice page's, asked with the voice scope; the say
    /// is the care session's sample.
    #[test]
    fn the_chime_and_the_sample_go_through_the_voice_and_care_methods() {
        let script = Script::new(vec![Ok(fixture("voice_test.json"))]);
        let ran = care_run(&script, &["test", "chime"]);
        let asked = script.call(0);
        assert_eq!(
            (asked.method.as_str(), asked.scopes),
            ("voice.test", vec!["voice".to_string()])
        );
        assert_eq!(asked.params, json!({"what": "speaker"}));
        let played = fixture("voice_test.json")["played"] == true;
        assert_eq!(ran.result, Ok(i32::from(!played)));
        assert!(
            ran.out
                .lines()
                .skip(1)
                .all(|line| line.starts_with("note: ")),
            "{}",
            ran.out
        );
        let script = Script::new(vec![Ok(
            json!({"played": false, "limits": ["queued only"]}),
        )]);
        let ran = care_run(&script, &["test", "chime"]);
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (
                Ok(1),
                "the chime was not queued: the audio side refused it\nnote: queued only\n"
            )
        );
        let script = Script::new(vec![Ok(json!({"utterance": "u_1"}))]);
        let ran = care_run(&script, &["test", "say"]);
        assert_eq!(
            ran.out,
            "queued: the deck speaks a sample sentence in its language\n"
        );
        assert_eq!(script.methods(), ["care.test.say"]);
    }

    /// The permissions a line each, whatever the service reports, with its
    /// limits; `--ask` goes through `perm.request`.
    #[test]
    fn the_permissions_read_a_line_each_and_ask_goes_through_perm_request() {
        // A recorded reply: a line per grant it names, a note per limit.
        let recorded = fixture("recorded/perm.status.json");
        let words = permissions_text(&recorded);
        for name in ["microphone", "speech", "bluetooth", "accessibility"] {
            let grant = recorded[name].as_str().unwrap();
            assert!(words.contains(&format!("{name}: {grant}\n")), "{words}");
        }
        assert_eq!(
            words.matches("\nnote: ").count() + usize::from(words.starts_with("note: ")),
            recorded["limits"].as_array().unwrap().len(),
            "{words}"
        );
        // Whatever the service names, and nothing it does not.
        assert_eq!(
            permissions_text(&json!({"microphone": "granted", "accessibility": "denied"})),
            "microphone: granted\naccessibility: denied\n"
        );
        // The recorded reply has no Automation grant, and an older
        // service's is not printed: the service needs none any more.
        assert!(recorded.get("automation").is_none());
        assert!(!words.contains("automation"), "{words}");
        assert_eq!(
            permissions_text(&json!({"speech": "granted", "automation": {"ghostty": "denied"}})),
            "speech: granted\n"
        );
        let script = Script::new(vec![Ok(json!({
            "which": "accessibility", "status": "prompted", "limits": ["once per run"],
        }))]);
        let ran = care_run(&script, &["permissions", "--ask", "accessibility"]);
        assert_eq!(ran.out, "accessibility: prompted\nnote: once per run\n");
        assert_eq!(script.call(0).params, json!({"which": "accessibility"}));
        assert_eq!(script.call(0).scopes, ["admin"]);
        let script = Script::new(vec![Ok(fixture("recorded/perm.status.json"))]);
        care_run(&script, &["permissions"]);
        assert_eq!(
            script.call(0).scopes,
            ["voice"],
            "an agent's terminal reads it too"
        );
    }

    /// The decks two lines each, the one in use marked; `forget` sends the
    /// label as it was typed, and the service decides.
    #[test]
    fn the_decks_read_two_lines_each_and_forget_takes_the_label() {
        let reply = json!({"decks": [
            {"deck": "3f2a9c1d", "current": true, "faces": [
                {"f": 1, "t": "agents", "n": "Agents"}, {"f": 5, "t": "settings", "n": "Settings"},
            ], "macros": 1, "last_linked": {
                "at": "2026-11-12T10:03:00.000Z", "fw": "0.13.0-openwalkietalkie",
                "board": "Rev B", "transport": "usb",
            }},
            {"deck": "0badc0de", "current": false, "faces": [], "macros": 0, "last_linked": null},
        ]});
        assert_eq!(
            decks_text(&reply),
            "3f2a9c1d (in use): last linked 2026-11-12T10:03:00.000Z over usb, Rev B, firmware \
             0.13.0-openwalkietalkie\n  strip: Agents, Settings; keys for 1 Macropad\n0badc0de: \
             never linked here\n  no strip kept yet\n"
        );
        assert!(decks_text(&json!({"decks": []})).starts_with("no deck is remembered here yet"));
        let script = Script::new(vec![Ok(json!({"deck": "0badc0de", "forgotten": true}))]);
        let ran = care_run(&script, &["decks", "forget", "0BADC0DE"]);
        assert_eq!(
            (ran.result, ran.out.as_str()),
            (
                Ok(0),
                "forgot deck 0badc0de: it starts from the firmware's defaults the next time it \
                 links\n"
            )
        );
        assert_eq!(script.call(0).method, "care.decks.forget");
        assert_eq!(script.call(0).params, json!({"deck": "0BADC0DE"}));
        // The deck in use is the service's refusal, reported as `ctl` reports one.
        let script = Script::new(vec![Err(CtlFailure::Rpc(ApiError::invalid(
            "deck",
            crate::care::decks::IN_USE,
        )))]);
        let ran = care_run(&script, &["decks", "forget", "3f2a9c1d"]);
        assert_eq!(ran.result, Ok(1));
        assert!(ran.err.contains("\"field\":\"deck\""), "{}", ran.err);
    }

    /// The two updates: this app's line from the service's own looks (none
    /// from a service that does not compare the app), then the firmware's
    /// status as `deckd firmware status` says it. Reading takes `observe`;
    /// `--check` has the service look now, with `admin`; a look that ended
    /// in an error exits 1; `--json` is the status as it came.
    #[test]
    fn updates_say_the_apps_line_then_the_firmwares_and_check_looks_now() {
        let mut status = json!({
            "phase": "ready",
            "message": "Firmware 0.14.0 is available.",
            "available": true,
            "release": {"firmware": {"version": "0.14.0"}},
            "device": {"board": "hw-b", "version": "0.13.0"},
            "app": {
                "version": "0.14.0",
                "available": true,
                "release_url": "https://example.invalid/releases/v0.14.0",
            },
            "linked": {"fw": "0.13.0", "available": true},
        });
        let firmware = super::super::firmware::status_text(&status);
        let script = Script::new(vec![Ok(status.clone())]);
        let ran = care_run(&script, &["updates"]);
        assert_eq!(ran.result, Ok(0));
        assert_eq!(
            ran.out,
            format!(
                "app: OpenWalkieTalkie 0.14.0 is available: \
                 https://example.invalid/releases/v0.14.0\n{firmware}"
            )
        );
        assert_eq!(script.methods(), ["firmware.status"]);
        assert!(
            script.call(0).scopes.is_empty(),
            "the socket's own `observe`: an agent's terminal reads it too"
        );
        // This app is current; a service that does not compare it says
        // nothing of it.
        status["app"]["available"] = json!(false);
        assert_eq!(
            updates_text(&status),
            format!("app: up to date\n{firmware}")
        );
        let mut older = status.clone();
        older.as_object_mut().unwrap().remove("app");
        assert_eq!(updates_text(&older), firmware);
        // `--check`: the service looks now, forced past its six hours. The
        // look starts (`checking`) and is followed to its end, as
        // `deckd firmware check --wait` follows it; one that failed exits 1.
        let mut checking = status.clone();
        checking["phase"] = json!("checking");
        checking["message"] = json!("Checking for firmware updates…");
        let mut broken = status.clone();
        broken["phase"] = json!("error");
        broken["error"] = json!("the release feed did not answer");
        let script = Script::new(vec![Ok(checking.clone()), Ok(checking.clone()), Ok(broken)]);
        let ran = care_run(&script, &["updates", "--check"]);
        assert_eq!(ran.result, Ok(1), "a look that failed");
        assert_eq!(
            script.methods(),
            ["firmware.check", "firmware.status", "firmware.status"]
        );
        assert_eq!(script.call(0).params, json!({"force": true}));
        assert_eq!(script.call(0).scopes, ["admin"]);
        assert!(
            ran.out
                .ends_with("error: the release feed did not answer\n"),
            "{}",
            ran.out
        );
        let script = Script::new(vec![Ok(checking), Ok(status.clone())]);
        let ran = care_run(&script, &["updates", "--check"]);
        assert_eq!(ran.result, Ok(0), "a look that found the release");
        assert_eq!(
            ran.out,
            format!("app: up to date\n{firmware}"),
            "the status it ended on"
        );
        let script = Script::new(vec![Ok(status.clone())]);
        let ran = care_run(&script, &["updates", "--json"]);
        assert_eq!(serde_json::from_str::<Value>(&ran.out).unwrap(), status);
        // A deck on Bluetooth alone: no deck on the cable, and the firmware
        // its hello named is behind the release, as the window says it.
        let mut wireless = status.clone();
        wireless.as_object_mut().unwrap().remove("device");
        wireless["available"] = json!(false);
        assert!(
            updates_text(&wireless).contains(
                "linked deck: firmware 0.13.0, older than 0.14.0: plug it in by USB and run \
                 `deckd firmware check`\n"
            ),
            "{}",
            updates_text(&wireless)
        );
    }

    /// A terminal an agent started is refused the admin scope: the verb says
    /// why and exits 1, and nothing more is asked.
    #[test]
    fn an_agents_terminal_is_told_why_its_admin_verb_was_refused() {
        let script = Script::new(vec![Err(CtlFailure::Rpc(ApiError::forbidden_ancestry(
            "admin",
        )))]);
        let ran = care_run(&script, &["usage", "claude", "on"]);
        assert_eq!(ran.result, Ok(1));
        assert_eq!(script.methods(), ["config.get"]);
        assert!(
            ran.err
                .ends_with(&format!("deckd: {}\n", crate::cli::ANCESTRY_HINT)),
            "{}",
            ran.err
        );
        assert_eq!(ran.out, "");
    }
}
