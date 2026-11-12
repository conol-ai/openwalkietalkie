//! The replies the companion's pages read about what feeds the deck and how
//! the setup is: `sources.list`, `care.test.status`, `care.checks`,
//! `care.footprint`, and the `[sources]` half of `config.get`.
//!
//! They sit beside [`crate::admin`] for the reason it gives: every one is a
//! **call and not an event**, so nothing folds them into the mirror and the
//! page that asked keeps the answer. And they are parsed the way it parses:
//! by hand over `serde_json::Value`, never derived, so a word a newer
//! service sends survives as itself (`Unknown`) and a field this build has
//! not heard of is ignored, instead of failing the row.
//!
//! `sources.list` is counts and times, never a session: nothing here can
//! hold a title, a prompt or a reply, because the reply carries none.
use crate::{ConfigValue, UiAction};
use deck_model::Ts;
use serde_json::Value;

/// A closed set of wire words plus the one this build has not heard of, the
/// way [`crate::Status`] is: `parse` reads the word, `as_str` gives it back,
/// and an unknown word is kept verbatim rather than rounded to a known one.
macro_rules! words {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $word:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
            /// A word this build does not know, kept as the service sent it.
            Unknown(String),
        }

        impl $name {
            /// The known words, in the order the contract lists them.
            pub const KNOWN: &'static [$name] = &[$($name::$variant),+];

            pub fn parse(word: &str) -> $name {
                match word {
                    $($word => $name::$variant,)+
                    other => $name::Unknown(other.to_string()),
                }
            }

            pub fn as_str(&self) -> &str {
                match self {
                    $($name::$variant => $word,)+
                    $name::Unknown(word) => word,
                }
            }
        }
    };
}

words! {
    /// What sort of thing a source is (`kind`).
    SourceKind {
        /// A command-line tool the service integrates with: Claude Code,
        /// Codex.
        Tool = "tool",
        /// The Claude Channels bridge.
        Channel = "channel",
        /// An app that connected to the service itself (`app:<name>`).
        App = "app",
    }
}

words! {
    /// Whether a source is reaching the service (`state`).
    SourceState {
        Connected = "connected",
        /// Switched on, and nothing has connected through it.
        Waiting = "waiting",
        Off = "off",
    }
}

words! {
    /// What a source's sessions are called (`unit`).
    Unit {
        Sessions = "sessions",
        /// Codex.
        Threads = "threads",
        /// A connected app whose agents are chats.
        Chats = "chats",
    }
}

words! {
    /// What a press on the deck does to bring a source's session forward
    /// (`handoff`).
    Handoff {
        /// Focuses the session's terminal tab.
        Terminal = "terminal",
        /// Raises the app that owns the session.
        App = "app",
        /// Nothing yet (`none` on the wire).
        NotYet = "none",
        /// The source does it itself (`self` on the wire).
        Itself = "self",
    }
}

/// One row of `sources.list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// `claude`, `claude_channels`, `codex`, or `app:<client name>`.
    pub id: String,
    pub kind: SourceKind,
    /// The name to show: `Claude Code`, `Codex CLI`, or the app's own.
    pub name: String,
    /// The source's own switch, which the global pause does not change.
    /// Absent reads as on: a row the service did not call switched off is
    /// not shown as paused.
    pub enabled: bool,
    /// `None` for Claude Code, whose state is its installer status.
    pub state: Option<SourceState>,
    /// Its sessions that hold a place on the deck.
    pub on_deck: u32,
    /// All its sessions.
    pub sessions: u32,
    /// What the counts count; `sessions` when the service names nothing.
    pub unit: Unit,
    /// When the service last heard from it; `None` for never.
    pub last_event_at: Option<Ts>,
    pub handoff: Option<Handoff>,
    /// The hook events the plugin installs (Claude Code only).
    pub hooks: Option<u32>,
    /// Sessions still on an old helper the service refuses (Claude
    /// Channels only).
    pub stale: Option<u32>,
    /// What a connected app does, from `sessions`, `voice` and `face`.
    pub does: Vec<String>,
}

impl Source {
    /// The ids of the three sources that are not connected apps.
    pub const CLAUDE: &'static str = "claude";
    pub const CLAUDE_CHANNELS: &'static str = "claude_channels";
    pub const CODEX: &'static str = "codex";
    /// The prefix of a connected app's id.
    pub const APP_PREFIX: &'static str = "app:";

    /// One row. `None` only when it has no `id`, which is not a row about
    /// anything.
    pub fn parse(row: &Value) -> Option<Source> {
        let id = text(row.get("id"))?;
        Some(Source {
            kind: SourceKind::parse(row.get("kind").and_then(Value::as_str).unwrap_or_default()),
            name: text(row.get("name")).unwrap_or_else(|| id.clone()),
            enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            state: text(row.get("state")).map(|word| SourceState::parse(&word)),
            on_deck: count(row.get("on_deck")).unwrap_or(0),
            sessions: count(row.get("sessions")).unwrap_or(0),
            unit: text(row.get("unit")).map_or(Unit::Sessions, |word| Unit::parse(&word)),
            last_event_at: stamp(row.get("last_event_at")),
            handoff: text(row.get("handoff")).map(|word| Handoff::parse(&word)),
            hooks: count(row.get("hooks")),
            stale: count(row.get("stale")),
            does: strings(row.get("does")),
            id,
        })
    }

    /// The app's own name, for a connected app; `None` for the others.
    pub fn app_name(&self) -> Option<&str> {
        self.id.strip_prefix(Self::APP_PREFIX)
    }

    /// Whether a connected app does `what` (`sessions`, `voice`, `face`).
    pub fn does(&self, what: &str) -> bool {
        self.does.iter().any(|done| done == what)
    }
}

/// `sources.list` → `{paused, sources, other}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// Every source paused at once (`sources.paused`).
    pub paused: bool,
    pub sources: Vec<Source>,
    /// Sessions that belong to no row (a Gemini or MCP adapter): counted,
    /// never named.
    pub other: u32,
}

impl Sources {
    pub fn parse(result: &Value) -> Sources {
        Sources {
            paused: result
                .get("paused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            sources: result
                .get("sources")
                .and_then(Value::as_array)
                .map(|rows| rows.iter().filter_map(Source::parse).collect())
                .unwrap_or_default(),
            other: count(result.get("other")).unwrap_or(0),
        }
    }

    /// The row with this id.
    pub fn get(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    /// The connected apps, in the service's order.
    pub fn apps(&self) -> impl Iterator<Item = &Source> {
        self.sources
            .iter()
            .filter(|source| source.kind == SourceKind::App)
    }
}

words! {
    /// Where a test stands. A question is `sent`, then `answered` or
    /// `expired`; a voice test is `waiting`, then `heard` or `expired`;
    /// either is `no_deck` when no deck was linked to send it to.
    TestState {
        Sent = "sent",
        Waiting = "waiting",
        Answered = "answered",
        Heard = "heard",
        Expired = "expired",
        NoDeck = "no_deck",
    }
}

impl TestState {
    /// The `{state}` that `care.test.question` and `care.test.voice` answer
    /// at once. `None` when the reply carries no state.
    pub fn of_reply(result: &Value) -> Option<TestState> {
        text(result.get("state")).map(|word| TestState::parse(&word))
    }

    /// Still waiting on the deck: the page asks for the status again.
    pub fn pending(&self) -> bool {
        matches!(self, TestState::Sent | TestState::Waiting)
    }
}

/// The last test question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionTest {
    pub state: TestState,
    /// When it was sent.
    pub at: Option<Ts>,
    /// How long the deck took to answer, once it has.
    pub after_ms: Option<u64>,
}

/// The last voice test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceTest {
    pub state: TestState,
    /// When it was started.
    pub at: Option<Ts>,
    /// What the deck heard; `None` for nothing.
    pub heard: Option<String>,
}

/// `care.test.status` → `{question, voice}`, each `null` before its first
/// test since the service started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestStatus {
    pub question: Option<QuestionTest>,
    pub voice: Option<VoiceTest>,
}

impl TestStatus {
    pub fn parse(result: &Value) -> TestStatus {
        TestStatus {
            question: result.get("question").and_then(|test| {
                Some(QuestionTest {
                    state: TestState::parse(&text(test.get("state"))?),
                    at: stamp(test.get("at")),
                    after_ms: test.get("after_ms").and_then(Value::as_u64),
                })
            }),
            voice: result.get("voice").and_then(|test| {
                Some(VoiceTest {
                    state: TestState::parse(&text(test.get("state"))?),
                    at: stamp(test.get("at")),
                    heard: text(test.get("heard")),
                })
            }),
        }
    }
}

/// One of the service's setup checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// `None` when the service did not say: not checked, which is neither
    /// a pass nor a problem.
    pub ok: Option<bool>,
    pub detail: Option<String>,
}

impl Check {
    /// The two rows `care.checks` adds to `deckd doctor`'s.
    pub const LEGACY_LAUNCH_AGENT: &'static str = "legacy launch agent";
    pub const CODEX_DAEMON: &'static str = "codex daemon";
}

/// `care.checks` → `{checks:[{name, ok, detail}]}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    pub checks: Vec<Check>,
}

impl Checks {
    pub fn parse(result: &Value) -> Checks {
        Checks {
            checks: result
                .get("checks")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some(Check {
                                name: text(row.get("name"))?,
                                ok: row.get("ok").and_then(Value::as_bool),
                                detail: text(row.get("detail")),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The check with this name.
    pub fn get(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|check| check.name == name)
    }
}

/// The footprint's item ids, in the order the Footprint page lists them; it
/// finds each by its id. `codex_desktop` is the launcher the Integrations
/// page's Codex Desktop **Set Up** writes, listed here beside the Codex
/// setup. The service lists the other nine in this order and the launcher
/// after them, and one from before the launcher names the nine alone.
pub const FOOTPRINT_IDS: [&str; 10] = [
    "service",
    "claude_plugin",
    "codex_hooks",
    "codex_desktop",
    "stt",
    "kokoro",
    "firmware",
    "data",
    "prefs",
    "logs",
];

/// One thing the app has put on this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FootprintItem {
    /// One of [`FOOTPRINT_IDS`], or a newer one.
    pub id: String,
    pub path: Option<String>,
    /// `None` where a size does not apply (the service itself).
    pub bytes: Option<u64>,
    /// Hooks for the plugin, entries for the Codex setup; `None` elsewhere.
    pub count: Option<u64>,
    /// On disk now. Absent reads as not: the page claims nothing the
    /// service did not.
    pub present: bool,
}

/// `care.footprint` → `{items, total_bytes}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub items: Vec<FootprintItem>,
    pub total_bytes: Option<u64>,
}

impl Footprint {
    pub fn parse(result: &Value) -> Footprint {
        Footprint {
            items: result
                .get("items")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some(FootprintItem {
                                id: text(row.get("id"))?,
                                path: text(row.get("path")),
                                bytes: row.get("bytes").and_then(Value::as_u64),
                                count: row.get("count").and_then(Value::as_u64),
                                present: row
                                    .get("present")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            total_bytes: result.get("total_bytes").and_then(Value::as_u64),
        }
    }

    /// The item with this id.
    pub fn get(&self, id: &str) -> Option<&FootprintItem> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The dotted `config.set` names of the switches the Integrations page and
/// the menu write.
pub mod keys {
    /// Every source paused at once.
    pub const PAUSED: &str = "sources.paused";
    /// Claude Code's hooks on the deck.
    pub const CLAUDE: &str = "sources.claude";
    /// The Claude Channels bridge.
    pub const CLAUDE_CHANNELS: &str = "sources.claude_channels";
    /// The connected apps paused one by one, by client name.
    pub const APPS_PAUSED: &str = "sources.apps_paused";
    /// The Codex CLI switch.
    pub const CODEX_DAEMON: &str = "codex.daemon";
    /// The older pause, for the hooks alone.
    pub const HOOKS_ENABLED: &str = "hooks.enabled";
}

/// The switches of `config.get` the Integrations page and the menu read:
/// `[sources]`, the two older keys beside it, and the one voice switch that
/// changes what a connected app's row says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourcesConfig {
    /// `sources.paused`. `None` from a service that has no `[sources]`,
    /// which manages none of these switches.
    pub paused: Option<bool>,
    /// `sources.claude`.
    pub claude: Option<bool>,
    /// `sources.claude_channels`, as the service resolves it.
    pub claude_channels: Option<bool>,
    /// `sources.apps_paused`: client names.
    pub apps_paused: Vec<String>,
    /// `codex.daemon`.
    pub codex_daemon: Option<bool>,
    /// `hooks.enabled`.
    pub hooks_enabled: Option<bool>,
    /// `voice.stt.companion`: a connected app that transcribes its own
    /// chats is sent the presses meant for them. Off, those presses are
    /// recognised on this Mac. `None` from a service that has no such
    /// switch, whose apps' own recognisers always hear them.
    pub stt_companion: Option<bool>,
}

impl SourcesConfig {
    pub fn parse(result: &Value) -> SourcesConfig {
        let table = |name: &str| result.get(name).unwrap_or(&Value::Null);
        let flag = |name: &str, key: &str| table(name).get(key).and_then(Value::as_bool);
        SourcesConfig {
            paused: flag("sources", "paused"),
            claude: flag("sources", "claude"),
            claude_channels: flag("sources", "claude_channels"),
            apps_paused: strings(table("sources").get("apps_paused")),
            codex_daemon: flag("codex", "daemon"),
            hooks_enabled: flag("hooks", "enabled"),
            // `[voice]` is published flat, its subtables' keys dotted.
            stt_companion: flag("voice", "stt.companion"),
        }
    }

    /// Whether this service has the `[sources]` switches at all.
    pub fn managed(&self) -> bool {
        self.paused.is_some()
    }

    /// What **Pause integrations** shows: paused by its own switch, or by
    /// the older `hooks.enabled = false`, which stops the same hooks.
    /// `None` until a service with `[sources]` has answered: a switch that
    /// guessed would offer to undo a pause it cannot see.
    pub fn effective_pause(&self) -> Option<bool> {
        Some(self.paused? || self.hooks_enabled == Some(false))
    }
}

/// What turning **Pause integrations** on or off writes, for the page's
/// switch and the menu's item alike. Pausing is `sources.paused` alone.
/// Resuming turns `hooks.enabled` back on as well, so a pause made with the
/// older switch ends too; one patch, so the two land together or not at
/// all.
pub fn pause_action(on: bool) -> UiAction {
    let mut entries = vec![(keys::PAUSED.to_string(), ConfigValue::Bool(on))];
    if !on {
        entries.push((keys::HOOKS_ENABLED.to_string(), ConfigValue::Bool(true)));
    }
    UiAction::ConfigPatch { entries }
}

/// A non-empty string field. `null`, absent and `""` are all `None`.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

/// A `Vec<String>` from a JSON array, dropping anything that is not a
/// string. Absent and null are an empty list and never an error.
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A count that fits a `u32`; `None` for null, absent or anything else.
fn count(value: Option<&Value>) -> Option<u32> {
    u32::try_from(value?.as_u64()?).ok()
}

/// An RFC 3339 stamp, as the service writes them; `None` for null, absent
/// or a string that is not one.
fn stamp(value: Option<&Value>) -> Option<Ts> {
    serde_json::from_value(value?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_word_round_trips_and_a_newer_one_survives_as_itself() {
        for kind in SourceKind::KNOWN {
            assert_eq!(&SourceKind::parse(kind.as_str()), kind);
        }
        for state in SourceState::KNOWN {
            assert_eq!(&SourceState::parse(state.as_str()), state);
        }
        for unit in Unit::KNOWN {
            assert_eq!(&Unit::parse(unit.as_str()), unit);
        }
        for handoff in Handoff::KNOWN {
            assert_eq!(&Handoff::parse(handoff.as_str()), handoff);
        }
        for state in TestState::KNOWN {
            assert_eq!(&TestState::parse(state.as_str()), state);
        }
        assert_eq!(Handoff::parse("none"), Handoff::NotYet);
        assert_eq!(Handoff::parse("self"), Handoff::Itself);
        assert_eq!(TestState::parse("no_deck"), TestState::NoDeck);
        let newer = Unit::parse("conversations");
        assert_eq!(newer, Unit::Unknown("conversations".into()));
        assert_eq!(newer.as_str(), "conversations");
    }

    #[test]
    fn a_source_row_keeps_its_counts_and_words_and_tolerates_what_it_lacks() {
        let reply = json!({
            "paused": true,
            "sources": [
                {"id": "codex", "kind": "tool", "name": "Codex CLI", "enabled": false,
                 "state": "waiting", "on_deck": 1, "sessions": 4, "unit": "threads",
                 "last_event_at": "2026-11-11T10:01:00.000Z", "handoff": "none",
                 "hooks": null, "stale": null, "does": null, "a_newer_field": 7},
                {"id": "app:Acme", "kind": "app", "name": "Acme", "state": "beaming",
                 "unit": "chats", "handoff": "self", "does": ["sessions", "teleport"]},
                {"kind": "tool", "name": "a row about nothing"}
            ],
            "other": 2
        });
        let sources = Sources::parse(&reply);
        assert!(sources.paused);
        assert_eq!(sources.other, 2);
        assert_eq!(sources.sources.len(), 2, "a row with no id is not a row");
        let codex = sources.get(Source::CODEX).expect("the codex row");
        assert!(!codex.enabled);
        assert_eq!(codex.state, Some(SourceState::Waiting));
        assert_eq!((codex.on_deck, codex.sessions), (1, 4));
        assert_eq!(codex.unit, Unit::Threads);
        assert_eq!(codex.last_event_at, Some(Ts(1_794_391_260_000)));
        assert_eq!(codex.handoff, Some(Handoff::NotYet));
        assert_eq!((codex.hooks, codex.stale), (None, None));
        assert!(codex.does.is_empty() && codex.app_name().is_none());
        let acme = sources.apps().next().expect("one app");
        assert_eq!(acme.app_name(), Some("Acme"));
        assert_eq!(acme.state, Some(SourceState::Unknown("beaming".into())));
        assert!(acme.enabled, "a row nobody switched off is on");
        assert_eq!((acme.on_deck, acme.sessions), (0, 0));
        assert_eq!(acme.last_event_at, None, "never heard from");
        assert!(acme.does("sessions") && !acme.does("voice"));
        assert!(acme.does("teleport"), "a newer word is kept");
        assert_eq!(sources.apps().count(), 1);
    }

    #[test]
    fn the_test_status_reads_both_tests_and_nothing_before_the_first() {
        let status = TestStatus::parse(&json!({
            "question": {"state": "expired", "at": "2026-11-11T09:58:12.000Z", "after_ms": null},
            "voice": {"state": "heard", "at": "2026-11-11T09:59:40.000Z", "heard": ""}
        }));
        let question = status.question.expect("a question");
        assert_eq!(question.state, TestState::Expired);
        assert!(!question.state.pending());
        assert_eq!(question.after_ms, None);
        let voice = status.voice.expect("a voice test");
        assert_eq!(voice.heard, None, "an empty transcript is nothing heard");
        assert!(voice.at.is_some());
        // Before any test both are null, and a test with no state is none.
        let fresh = TestStatus::parse(&json!({"question": null, "voice": {"at": null}}));
        assert_eq!(fresh, TestStatus::default());
        // The immediate replies.
        assert_eq!(TestState::of_reply(&json!({"state": "sent"})), Some(TestState::Sent));
        assert!(TestState::Sent.pending() && TestState::Waiting.pending());
        assert_eq!(
            TestState::of_reply(&json!({"state": "no_deck"})),
            Some(TestState::NoDeck)
        );
        assert_eq!(TestState::of_reply(&json!({})), None);
    }

    #[test]
    fn a_check_the_service_did_not_judge_is_neither_passed_nor_failed() {
        let checks = Checks::parse(&json!({"checks": [
            {"name": "legacy launch agent", "ok": false,
             "detail": "~/Library/LaunchAgents/com.conol.openmicrodial.deckd.plist"},
            {"name": "codex daemon", "detail": "not configured"},
            {"ok": true, "detail": "a row about nothing"}
        ]}));
        assert_eq!(checks.checks.len(), 2, "a check with no name is not one");
        let legacy = checks.get(Check::LEGACY_LAUNCH_AGENT).expect("the legacy row");
        assert_eq!(legacy.ok, Some(false));
        assert!(legacy.detail.as_deref().is_some_and(|d| d.contains("LaunchAgents")));
        assert_eq!(checks.get(Check::CODEX_DAEMON).and_then(|c| c.ok), None);
        assert!(checks.get("device").is_none());
    }

    #[test]
    fn a_footprint_item_says_only_what_the_service_measured() {
        let footprint = Footprint::parse(&json!({
            "items": [
                {"id": "codex_hooks", "path": "/Users/you/.codex/hooks.json",
                 "bytes": null, "count": null, "present": false},
                {"id": "service",
                 "path": "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"},
                {"path": "/tmp/a row about nothing", "present": true}
            ],
            "total_bytes": 0
        }));
        assert_eq!(footprint.items.len(), 2, "an item with no id is not one");
        let codex = footprint.get("codex_hooks").expect("the codex item");
        assert!(!codex.present);
        assert_eq!((codex.bytes, codex.count), (None, None));
        assert!(
            !footprint.get("service").expect("the service").present,
            "absent is not present"
        );
        assert_eq!(footprint.total_bytes, Some(0));
        assert_eq!(FOOTPRINT_IDS.len(), 10);
    }

    #[test]
    fn the_codex_desktop_launcher_is_listed_after_the_codex_setup_and_read_like_the_rest() {
        let at = |id: &str| FOOTPRINT_IDS.iter().position(|known| *known == id);
        assert_eq!(
            at("codex_desktop"),
            at("codex_hooks").map(|codex| codex + 1),
            "the page lists it beside the Codex setup"
        );
        let footprint = Footprint::parse(&json!({"items": [
            {"id": "codex_desktop",
             "path": "/Users/you/Applications/Codex with OpenWalkieTalkie.app",
             "bytes": 1536, "count": null, "present": true}
        ]}));
        let launcher = footprint.get("codex_desktop").expect("the launcher");
        assert!(launcher.present);
        assert_eq!((launcher.bytes, launcher.count), (Some(1_536), None));
        assert!(launcher
            .path
            .as_deref()
            .is_some_and(|path| path.ends_with("Codex with OpenWalkieTalkie.app")));
    }

    #[test]
    fn the_sources_switches_read_off_config_get_and_an_older_service_has_none() {
        let config = SourcesConfig::parse(&json!({
            "sources": {"paused": false, "claude": true, "claude_channels": false,
                        "apps_paused": ["Conol", 7]},
            "codex": {"daemon": false, "daemon_socket": null},
            "hooks": {"enabled": true}
        }));
        assert!(config.managed());
        assert_eq!(config.paused, Some(false));
        assert_eq!(config.claude, Some(true));
        assert_eq!(config.claude_channels, Some(false));
        assert_eq!(config.apps_paused, vec!["Conol"]);
        assert_eq!(config.codex_daemon, Some(false));
        assert_eq!(config.effective_pause(), Some(false));
        // The older pause stops the same hooks, so the switch shows it.
        let hooks_off = SourcesConfig {
            hooks_enabled: Some(false),
            ..config.clone()
        };
        assert_eq!(hooks_off.effective_pause(), Some(true));
        // A service from before `[sources]`: nothing to show, nothing to
        // offer.
        let older = SourcesConfig::parse(&json!({
            "codex": {"daemon": true},
            "hooks": {"enabled": false}
        }));
        assert!(!older.managed());
        assert_eq!(older.effective_pause(), None);
        assert_eq!(older.codex_daemon, Some(true));
        assert_eq!(older.stt_companion, None, "no voice switch published");
    }

    #[test]
    fn the_companion_switch_is_read_off_the_flat_voice_table() {
        let switched = |value: Value| {
            SourcesConfig::parse(&json!({
                "sources": {"paused": false},
                "voice": {"stt.engine": "apple", "stt.companion": value}
            }))
            .stt_companion
        };
        assert_eq!(switched(json!(true)), Some(true));
        assert_eq!(switched(json!(false)), Some(false));
        assert_eq!(switched(json!("yes")), None, "not a switch");
        // Nested the way a TOML file writes it is not how `config.get`
        // publishes it.
        let nested = SourcesConfig::parse(&json!({"voice": {"stt": {"companion": false}}}));
        assert_eq!(nested.stt_companion, None);
    }

    #[test]
    fn pausing_writes_one_key_and_resuming_turns_the_hooks_back_on_in_the_same_patch() {
        assert_eq!(
            pause_action(true),
            UiAction::ConfigPatch {
                entries: vec![("sources.paused".into(), ConfigValue::Bool(true))]
            }
        );
        let (method, params) = pause_action(false).request();
        assert_eq!(method, "config.set");
        assert_eq!(
            params,
            json!({"patch": {"sources.paused": false, "hooks.enabled": true}})
        );
    }

    #[test]
    fn every_parser_reads_an_empty_object_as_nothing() {
        let empty = json!({});
        assert_eq!(Sources::parse(&empty), Sources::default());
        assert_eq!(TestStatus::parse(&empty), TestStatus::default());
        assert_eq!(Checks::parse(&empty), Checks::default());
        assert_eq!(Footprint::parse(&empty), Footprint::default());
        assert_eq!(SourcesConfig::parse(&empty), SourcesConfig::default());
        for nothing in [Value::Null, json!([]), json!("sources")] {
            assert_eq!(Sources::parse(&nothing), Sources::default());
            assert_eq!(SourcesConfig::parse(&nothing), SourcesConfig::default());
        }
    }
}
