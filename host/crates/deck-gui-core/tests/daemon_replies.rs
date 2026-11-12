//! The GUI's parsers against the daemon's **own** output.
//!
//! Every other test in this crate feeds the parsers a reply somebody wrote by
//! hand from reading the design, which means both sides of the seam can be
//! wrong together and stay green — that is how week 5 shipped a resolver
//! looking for a beacon nothing wrote, and it is the failure mode this file
//! exists to close. The fixtures beside it were captured from a real `deckd`
//! on 2026-09-22 (`deckd ctl` over its own UDS, engines up, deck attached);
//! regenerate them the same way if a reply shape changes on purpose.
//!
//! The companion's fixtures are of another kind, and the difference is said
//! here so nobody mistakes one for the other. `sources_list`,
//! `care_test_status`, `care_checks`, `care_footprint` and
//! `config_get_sources` are written from the wire contract of the methods
//! the service is still growing, with the boards' values where a board shows
//! one (the Claude row's hand-off `app` is the board's word; the service
//! sends `terminal` until the hand-off repair). They are the contract the
//! service's replies are held to, key for key and type for type; a real
//! reply, once recorded, goes beside its file and never over it. Those
//! recorded from the finished service are in `fixtures/daemon/recorded/`,
//! read in `tests/recorded_replies.rs`.
//! `device_status` is exact: `tests/store_mirror.rs` proves the store emits
//! it byte for byte.

use deck_gui_core::care::{
    Check, Checks, Footprint, Handoff, SourceKind, SourceState, Sources, SourcesConfig, TestState,
    TestStatus, Unit, FOOTPRINT_IDS,
};
use deck_gui_core::voice::{Engines, SttConfig, TestReport, TtsConfig};
use deck_gui_core::{ApiConfig, ApiEvent, AppEvent, HostState};
use deck_model::{EventKind, Principal, Snapshot, Transport, Ts};

fn fixture(name: &str) -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/daemon/");
    let text = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("{path}{name}.json: {e}"));
    serde_json::from_str(&text).expect("the captured reply is json")
}

#[test]
fn the_engines_the_daemon_really_reports_are_the_ones_this_screen_parses() {
    let engines = Engines::parse(&fixture("voice_engines"));
    // The daemon answers from a cached `EngineInfo` report, and what it
    // carries is **locales**, not a voice list — §4.9 said `[{id, voices}]`
    // and the engine has no voices to give.
    let stt = engines.stt.first().expect("an stt engine");
    assert_eq!(stt.id, "apple");
    assert!(
        stt.locales.iter().any(|l| l == "en-US"),
        "the locale the selection names must be one the engine reported: {:?}",
        &stt.locales[..stt.locales.len().min(4)]
    );
    assert!(
        stt.locales.len() > 40,
        "a real Apple recogniser reports dozens of locales, not a handful"
    );
    let tts = engines.tts.first().expect("a tts engine");
    assert_eq!(tts.id, "apple");
    assert!(!tts.locales.is_empty());
    assert_eq!(engines.selected_stt.as_deref(), Some("apple"));
    assert_eq!(engines.locale.as_deref(), Some("en-US"));
}

#[test]
fn the_speaker_test_the_daemon_really_answers_keeps_its_own_honesty_note() {
    let report = TestReport::parse(&fixture("voice_test"));
    assert_eq!(report.what, "speaker");
    assert_eq!(report.chime.as_deref(), Some("ready"));
    // The daemon says on the wire that `played` is a queue and not a sound
    // anybody heard. The screen must carry that sentence rather than
    // paraphrase it into a claim the daemon did not make.
    assert!(
        report
            .limits
            .iter()
            .any(|l| l.contains("not that anybody heard it")),
        "the daemon's own limits text is gone: {:?}",
        report.limits
    );
    assert!(
        report
            .limits
            .iter()
            .any(|l| l.contains("system default device") && l.contains("never touched")),
        "the note that no system device or volume moves is gone"
    );
}

#[test]
fn a_config_set_reply_tells_a_live_change_from_one_that_wants_a_restart() {
    let reply: serde_json::Value = fixture("config_set");
    // `hooks.enabled` is a hot key (week 6 put it in `HOT_KEYS`), so the real
    // daemon reports it applied with nothing left to restart for. A screen
    // that read only `changed` could not tell that from a port change.
    assert_eq!(reply["applied"][0], "hooks.enabled");
    assert_eq!(reply["changed"][0], "hooks.enabled");
    assert_eq!(reply["keys"][0], "hooks.enabled");
    assert!(
        reply["restart_needed"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "a hot key must not ask for a restart"
    );
}

#[test]
fn the_sources_list_reads_into_one_row_per_source_with_counts_and_never_a_session() {
    let sources = Sources::parse(&fixture("sources_list"));
    assert!(!sources.paused);
    assert_eq!(sources.other, 0);
    let ids: Vec<&str> = sources.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["claude", "claude_channels", "codex", "app:Conol", "app:Omarchy remote"]
    );
    let claude = sources.get("claude").expect("the claude row");
    assert_eq!(claude.kind, SourceKind::Tool);
    assert_eq!(claude.state, None, "claude's state is its installer status");
    assert_eq!((claude.on_deck, claude.sessions, claude.hooks), (3, 3, Some(13)));
    assert_eq!(claude.handoff, Some(Handoff::App));
    assert_eq!(
        claude.last_event_at.map(Ts::millis),
        Some(1_794_391_316_000),
        "10:01:56, four seconds before the boards' 10:02"
    );
    let channels = sources.get("claude_channels").expect("the channels row");
    assert_eq!(channels.kind, SourceKind::Channel);
    assert_eq!(channels.stale, Some(2));
    assert_eq!((channels.last_event_at, channels.handoff.clone()), (None, None));
    let codex = sources.get("codex").expect("the codex row");
    assert_eq!((codex.unit.clone(), codex.handoff.clone()), (Unit::Threads, Some(Handoff::NotYet)));
    assert_eq!(codex.state, Some(SourceState::Connected));
    let apps: Vec<&str> = sources.apps().filter_map(|app| app.app_name()).collect();
    assert_eq!(apps, vec!["Conol", "Omarchy remote"]);
    let conol = sources.get("app:Conol").expect("the conol row");
    // The board's "7 chats" stands under "On the deck": every chat it has
    // holds a place there.
    assert_eq!((conol.unit.clone(), conol.on_deck, conol.sessions), (Unit::Chats, 7, 7));
    assert!(conol.does("voice") && !conol.does("face"));
    assert_eq!(conol.handoff, Some(Handoff::Itself));
    assert!(sources.sources.iter().all(|s| s.enabled));
    // No row carries anything said in a session: only these keys exist.
    for row in fixture("sources_list")["sources"].as_array().expect("rows") {
        let keys: Vec<&str> = row.as_object().expect("a row").keys().map(String::as_str).collect();
        for key in &keys {
            assert!(
                [
                    "id", "kind", "name", "enabled", "state", "on_deck", "sessions", "unit",
                    "last_event_at", "handoff", "hooks", "stale", "does"
                ]
                .contains(key),
                "{key}"
            );
        }
        assert_eq!(keys.len(), 13, "every row has every key, null where it does not apply");
    }
}

#[test]
fn the_test_status_reads_a_question_answered_and_a_voice_test_heard() {
    let status = TestStatus::parse(&fixture("care_test_status"));
    let question = status.question.expect("a question");
    assert_eq!(question.state, TestState::Answered);
    assert_eq!(question.after_ms, Some(3120));
    assert!(question.at.is_some());
    let voice = status.voice.expect("a voice test");
    assert_eq!(voice.state, TestState::Heard);
    assert_eq!(voice.heard.as_deref(), Some("hello"));
    assert!(!question.state.pending() && !voice.state.pending());
}

#[test]
fn the_checks_carry_the_doctors_rows_and_the_two_the_service_adds() {
    let checks = Checks::parse(&fixture("care_checks"));
    assert_eq!(checks.checks.len(), 11);
    assert!(checks.checks.iter().all(|check| check.ok == Some(true)));
    for name in [
        "data dir",
        "service",
        "socket",
        "device",
        Check::LEGACY_LAUNCH_AGENT,
        Check::CODEX_DAEMON,
    ] {
        assert!(checks.get(name).is_some(), "{name}");
    }
    assert_eq!(
        checks.get("config.toml").and_then(|check| check.detail.as_deref()),
        Some("valid")
    );
}

#[test]
fn the_footprint_reads_every_item_the_page_lists_and_the_total_of_those_present() {
    let footprint = Footprint::parse(&fixture("care_footprint"));
    let ids: Vec<&str> = footprint.items.iter().map(|item| item.id.as_str()).collect();
    // In the service's order: the nine it always had in the page's order,
    // then the Codex Desktop launcher. The page finds each by its id.
    let (launcher, nine): (Vec<&str>, Vec<&str>) =
        FOOTPRINT_IDS.into_iter().partition(|id| *id == "codex_desktop");
    assert_eq!(ids[..9], nine[..]);
    assert_eq!(ids[9..], launcher[..]);
    assert!(footprint.items.iter().all(|item| item.path.is_some()));
    // The boards' Mac has the nine the Footprint board draws; its Codex
    // Desktop was never set up (the Integrations board offers Set Up), so
    // the launcher is named where it would be and is not there.
    let present: Vec<&str> = footprint
        .items
        .iter()
        .filter(|item| item.present)
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(present.len(), 9);
    assert!(!present.contains(&"codex_desktop"));
    let launcher = footprint.get("codex_desktop").expect("the launcher");
    assert_eq!((launcher.bytes, launcher.count), (None, None));
    let service = footprint.get("service").expect("the service");
    assert_eq!((service.bytes, service.count), (None, None), "no size applies");
    assert_eq!(footprint.get("claude_plugin").and_then(|i| i.count), Some(13));
    assert_eq!(footprint.get("codex_hooks").and_then(|i| i.count), Some(7));
    let sum: u64 = footprint.items.iter().filter_map(|item| item.bytes).sum();
    assert_eq!(footprint.total_bytes, Some(sum));
}

#[test]
fn one_config_get_reads_the_switches_the_api_and_both_halves_of_voice() {
    let reply = fixture("config_get_sources");
    let sources = SourcesConfig::parse(&reply);
    assert!(sources.managed());
    assert_eq!(
        (sources.paused, sources.claude, sources.claude_channels),
        (Some(false), Some(true), Some(true))
    );
    assert!(sources.apps_paused.is_empty(), "no app paused, as on the board");
    assert_eq!(reply["sources"]["apps_paused"], serde_json::json!([]));
    assert_eq!((sources.codex_daemon, sources.hooks_enabled), (Some(true), Some(true)));
    assert_eq!(sources.effective_pause(), Some(false));
    // The voice switch the connected apps' rows follow, read in the same
    // pass as the voice page reads it.
    assert_eq!(sources.stt_companion, Some(true));
    // The parsers that were there before read the same reply as ever.
    let api = ApiConfig::parse(&reply);
    assert_eq!((api.port, api.hooks_enabled), (Some(47201), Some(true)));
    assert_eq!(reply["api"]["tcp"], true);
    let stt = SttConfig::parse(&reply);
    assert_eq!((stt.engine.as_str(), stt.language.as_str()), ("whisper", "en"));
    assert_eq!(stt.locale, "en-GB");
    assert!(stt.companion && stt.prompted);
    let tts = TtsConfig::parse(&reply);
    assert_eq!(tts.engine, "kokoro");
    assert!(!tts.has_key());
    assert_eq!(reply["usage"]["providers"], serde_json::json!(["codex"]));
}

#[test]
fn the_device_status_fixture_folds_into_every_field_it_names() {
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: Snapshot {
            rev: 7,
            ..Snapshot::default()
        },
    }));
    state.handle_event(AppEvent::Api(ApiEvent::Event(deck_model::Event {
        rev: 8,
        ts: Ts(1_794_390_062_000),
        kind: EventKind::DeviceStatus,
        principal: Principal::Device,
        data: fixture("device_status"),
    })));
    let device = &state.mirror().device;
    assert_eq!(device.transport, Some(Transport::Bluetooth));
    assert_eq!((device.battery, device.charging), (Some(85), Some(false)));
    assert_eq!(device.board.as_deref(), Some("Rev B"));
    let last = device.last.as_ref().expect("the remembered deck");
    assert_eq!(last.fw.as_deref(), Some("0.13.0-openwalkietalkie"));
    assert_eq!(last.at, Ts(1_794_390_060_000));
    let whys: Vec<&str> = device.drops.iter().map(|drop| drop.why.as_str()).collect();
    assert_eq!(whys, vec!["read", "write_stall"]);
}
