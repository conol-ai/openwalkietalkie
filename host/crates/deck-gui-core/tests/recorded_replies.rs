//! The GUI's parsers against the replies of the finished service, recorded.
//!
//! `fixtures/daemon/recorded/` holds what the stage-6 service (`deckd`
//! 0.13.0, the build the companion's pages were finished against) answered
//! on 2026-11-12, started the way the companion's tests start one: a data
//! directory of its own (`DECKD_DATA_DIR=d`, relative, so the paths under it
//! are too), a throwaway home for it, for Claude Code and for Codex,
//! `--demo --no-device`, a `PATH` of the system's directories only (so its
//! installer found no `claude` or `codex` to run), and a config of
//! `[api] port = 0`, `[codex] daemon = false`, `[liveness] enabled = false`
//! and `[usage] providers = []`. Each file is the result of one
//! `deckd ctl --scopes admin <method> '<params>'`, pretty-printed and
//! otherwise as the service wrote it, but for one thing: the scratch
//! directory that held its run and its binary reads `/tmp/owt` in its paths
//! (`/tmp/owt/run/l1`, `/tmp/owt/bin/deckd-s6`). Three are of another
//! kind and say so: `voice.engines.json` is the error that call answered,
//! as `{"error": …}`; `session.hello.json` is the hello's snapshot as
//! `deckd events --snapshot --content` printed it (`ctl` says hello without
//! asking for one, and a second hello is refused); and a second recording of
//! one method carries a suffix (`care.test.status.after_question`, after a
//! `care.test.question` with no deck linked; `integrations.status.no_detect`
//! beside `integrations.status`, whose default is `detect: true`).
//!
//! Four were taken again on 2026-11-13, where the hand-off (raising the
//! session's own app in place of the Ghostty tab path) changed them, from
//! the same kind of run of that build: `perm.status` and `sources.list` are
//! its replies whole (no `automation`; the Claude Code row's hand-off is
//! `app`); `config.get` is the 0.13.0 reply without its `desktop` table and
//! `service.info` the 0.13.0 reply with that build's `capabilities` (no
//! `inject.ghostty`, and `order.result`), so the ports, paths and times the
//! other files share still agree. `config.get` was then brought by hand to
//! what the service of the same release answers for the three keys that
//! changed since: no `voice.fallback_input` (decision 11),
//! `voice."tts.key_store"` as `config` (a `--no-device` service keeps the
//! cloud voice key in its own `config.toml`), and the service's own release
//! looks switched on (`updates.check`). Its `api.tcp` is still the stage-6
//! service's (see the test that reads it).
//!
//! They sit beside the hand-written fixtures of `daemon_replies.rs`, which
//! are the contract with the boards' values, and never replace them. What a
//! fresh service really answers that the boards do not draw: a deck that was
//! never linked (no `last`, no strip, the device's own keys left out of the
//! reply), every integration absent, the Claude Channels bridge and Codex
//! switched off, `voice.engines` refused as `-32013` under `--no-device`, a
//! test question with no deck kept as the last one (`no_deck`), and
//! `[api] port = 0` — any free port — which reads as no port, since the one
//! held is `service.info`'s.
//!
//! Every test reads a reply with the parser the page that asks for it uses,
//! and checks what that page shows from it. The pages' own words for these
//! replies are tested in the app's page tests, from the same files.
use deck_client::ClientError;
use deck_gui_core::admin::{method_missing, ApiConfig, Info, Report, Status, TokenRow};
use deck_gui_core::attention::{attention, Extras};
use deck_gui_core::care::{
    Check, Checks, Footprint, Handoff, SourceKind, SourceState, Sources, SourcesConfig, TestState,
    TestStatus, Unit, FOOTPRINT_IDS,
};
use deck_gui_core::i18n::Language;
use deck_gui_core::voice::{SttConfig, SttStatus, TtsConfig, TtsStatus};
use deck_gui_core::{ApiEvent, AppEvent, HostState, IconState, MenuModel, NoService};
use deck_model::{ApiError, DeviceState, Link, Permission, Snapshot, Ts};
use serde_json::{json, Value};

/// A recorded reply, by its file's name.
macro_rules! recorded {
    ($name:literal) => {
        serde_json::from_str::<Value>(include_str!(concat!(
            "fixtures/daemon/recorded/",
            $name,
            ".json"
        )))
        .expect(concat!($name, ".json is the service's JSON"))
    };
}

/// The minute the service answered in, near enough for the attention list,
/// which finds nothing to count in it.
const RECORDED_AT: Ts = Ts(1_794_426_498_975);

/// Every key a `sources.list` row has: counts, times and switches, never a
/// word said in a session.
const SOURCE_KEYS: [&str; 13] = [
    "does",
    "enabled",
    "handoff",
    "hooks",
    "id",
    "kind",
    "last_event_at",
    "name",
    "on_deck",
    "sessions",
    "stale",
    "state",
    "unit",
];

#[test]
fn a_fresh_services_sources_are_its_three_tools_with_nothing_on_the_deck() {
    let reply = recorded!("sources.list");
    let sources = Sources::parse(&reply);
    assert!(!sources.paused);
    assert_eq!(sources.other, 0);
    let ids: Vec<&str> = sources.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["claude", "claude_channels", "codex"]);
    assert_eq!(sources.apps().count(), 0, "no app has connected");
    // The demo's six sessions are the service's own, and no source's.
    for source in &sources.sources {
        assert_eq!(
            (source.on_deck, source.sessions, source.last_event_at),
            (0, 0, None),
            "{}",
            source.id
        );
        assert!(source.does.is_empty(), "{}", source.id);
    }
    let claude = sources.get("claude").expect("the Claude Code row");
    assert_eq!(
        (claude.kind.clone(), claude.name.as_str()),
        (SourceKind::Tool, "Claude Code")
    );
    assert!(claude.enabled);
    assert_eq!(claude.state, None, "its state is the installer's status");
    assert_eq!(claude.unit, Unit::Sessions);
    // The hand-off raises the session's own app: the board's word, now the
    // service's.
    assert_eq!(claude.handoff, Some(Handoff::App));
    assert_eq!((claude.hooks, claude.stale), (Some(13), None));
    let channels = sources
        .get("claude_channels")
        .expect("the Claude Channels row");
    assert_eq!(channels.kind, SourceKind::Channel);
    assert!(!channels.enabled, "a new install leaves the bridge off");
    assert_eq!(channels.state, Some(SourceState::Off));
    assert_eq!((channels.stale, channels.handoff.clone()), (Some(0), None));
    let codex = sources.get("codex").expect("the Codex row");
    assert_eq!(codex.name, "Codex CLI");
    assert!(!codex.enabled, "`[codex] daemon = false`");
    assert_eq!(codex.state, Some(SourceState::Off));
    assert_eq!(
        (codex.unit.clone(), codex.handoff.clone()),
        (Unit::Threads, Some(Handoff::NotYet))
    );
    for row in reply["sources"].as_array().expect("rows") {
        let keys: Vec<&str> = row
            .as_object()
            .expect("a row")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, SOURCE_KEYS, "every key, null where it does not apply");
    }
}

#[test]
fn one_config_get_reads_every_switch_and_a_port_of_zero_as_no_port() {
    let reply = recorded!("config.get");
    let sources = SourcesConfig::parse(&reply);
    assert!(sources.managed());
    assert_eq!(
        (sources.paused, sources.claude, sources.claude_channels),
        (Some(false), Some(true), Some(false))
    );
    assert!(sources.apps_paused.is_empty());
    assert_eq!(
        (sources.codex_daemon, sources.hooks_enabled),
        (Some(false), Some(true))
    );
    assert_eq!(
        sources.effective_pause(),
        Some(false),
        "Pause Integrations, drawn and off"
    );
    assert_eq!(
        sources.stt_companion,
        Some(true),
        "the service's default: a connected app's cloud hears its chats"
    );
    // `[api] port = 0` asks for any free port. It is no port to print...
    assert_eq!(reply["api"]["port"], 0);
    let api = ApiConfig::parse(&reply);
    assert_eq!(api.port, None);
    // ...and the one the service holds is the one `service.info` names.
    assert_eq!(Info::parse(&recorded!("service.info")).port, Some(52_098));
    assert!(api.allowed_origins.is_empty());
    assert_eq!(
        (
            api.uds_default_scope.as_deref(),
            api.uds_max_scope.as_deref()
        ),
        (Some("observe"), Some("admin"))
    );
    assert_eq!(
        (api.max_held_hooks, api.hooks_enabled),
        (Some(64), Some(true))
    );
    // The stage-6 service bound its TCP door unless its file said not to.
    // The door has been off by default since (the owner's decision 12), so
    // a service of this release answers `false` here for the same config.
    assert_eq!(reply["api"]["tcp"], true);
    let stt = SttConfig::parse(&reply);
    assert_eq!(
        (
            stt.engine.as_str(),
            stt.language.as_str(),
            stt.locale.as_str()
        ),
        ("apple", "auto", "en-US")
    );
    assert_eq!(stt.model, "");
    assert!(
        stt.companion,
        "the service's default: Conol hears its chats' presses"
    );
    assert!(!stt.prompted);
    let tts = TtsConfig::parse(&reply);
    assert_eq!(
        (tts.engine.as_str(), tts.provider.as_str()),
        ("cloud", "openai")
    );
    assert!(!tts.has_key(), "the key's hint is empty: no key");
    assert_eq!(reply["voice"]["tts.key_store"], "config");
    assert!(
        !tts.in_keychain(),
        "a test service keeps its key in its own config.toml"
    );
    assert!(tts.cloud_selected());
    assert_eq!(tts.effective_model().as_deref(), Some("gpt-4o-mini-tts"));
    assert!(tts.voices.is_empty());
    assert_eq!(reply["usage"]["providers"], json!([]), "the test config's");
    // Decision 11: the Mac's microphone is no fallback, and the key is gone.
    assert!(reply["voice"].get("fallback_input").is_none(), "{reply}");
    // The service's own looks for a release, on by default.
    assert_eq!(reply["updates"], json!({"check": true}));
}

#[test]
fn the_checks_of_a_no_device_service_leave_the_deck_and_codex_unjudged() {
    let checks = Checks::parse(&recorded!("care.checks"));
    let names: Vec<&str> = checks
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "data dir",
            "config.toml",
            "state.json",
            "service",
            "status.json",
            "socket",
            "tcp",
            "tokens.toml",
            "device",
            "launchd deckd",
            Check::LEGACY_LAUNCH_AGENT,
            Check::CODEX_DAEMON,
        ]
    );
    // A service launchd did not start leaves its agent's rows unjudged
    // rather than reporting on whichever copy launchd runs.
    for name in ["launchd deckd", Check::LEGACY_LAUNCH_AGENT] {
        let row = checks.get(name).expect("the row");
        assert_eq!(row.ok, None, "{name}: not checked");
        assert!(row
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("not looked at")));
    }
    let device = checks.get("device").expect("the deck's row");
    assert_eq!(device.ok, None, "`--no-device` does not look: not checked");
    assert!(device
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("--no-device")));
    // Switched off: not checked, which the Service page reads as a row to
    // leave out.
    let codex = checks.get(Check::CODEX_DAEMON).expect("the Codex row");
    assert_eq!(codex.ok, None);
    // The deck, both launchd rows and Codex: four rows nothing judged.
    assert_eq!(
        checks
            .checks
            .iter()
            .filter(|check| check.ok.is_none())
            .count(),
        4
    );
    assert!(checks.checks.iter().all(|check| check.ok != Some(false)));
}

#[test]
fn the_footprint_of_a_fresh_service_names_nine_items_and_finds_two() {
    let footprint = Footprint::parse(&recorded!("care.footprint"));
    let ids: Vec<&str> = footprint
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    // In the page's order. This service predates the Codex Desktop
    // launcher's item and the Keychain item, the two the page lists that
    // it does not.
    let known: Vec<&str> = FOOTPRINT_IDS
        .into_iter()
        .filter(|id| !["codex_desktop", "keychain"].contains(id))
        .collect();
    assert_eq!(ids, known);
    let present: Vec<&str> = footprint
        .items
        .iter()
        .filter(|item| item.present)
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(present, ["service", "data"]);
    assert!(footprint.items.iter().all(|item| item.path.is_some()));
    // What is not there weighs nothing the service could say.
    for item in footprint.items.iter().filter(|item| !item.present) {
        assert_eq!((item.bytes, item.count), (None, None), "{}", item.id);
    }
    let data = footprint.get("data").expect("the data directory");
    assert_eq!((data.bytes, data.path.as_deref()), (Some(4_534), Some("d")));
    assert_eq!(footprint.get("service").and_then(|item| item.bytes), None);
    assert_eq!(footprint.total_bytes, Some(4_534));
}

#[test]
fn the_test_status_is_empty_before_a_test_and_keeps_a_question_no_deck_could_take() {
    assert_eq!(
        TestStatus::parse(&recorded!("care.test.status")),
        TestStatus::default(),
        "both null before the first test"
    );
    let state = TestState::of_reply(&recorded!("care.test.question"));
    assert_eq!(state, Some(TestState::NoDeck));
    assert!(
        !TestState::NoDeck.pending(),
        "nothing on the deck to ask after"
    );
    // The service keeps it as the last question: the page reads the same
    // line back.
    let after = TestStatus::parse(&recorded!("care.test.status.after_question"));
    let question = after.question.expect("the last question");
    assert_eq!(question.state, TestState::NoDeck);
    assert_eq!(question.after_ms, None);
    assert!(question.at.is_some());
    assert_eq!(after.voice, None);
}

#[test]
fn the_installer_finds_every_target_absent_and_its_probes_change_no_status() {
    let detect = recorded!("integrations.status");
    let files = recorded!("integrations.status.no_detect");
    let (rows, warnings) = Report::list(&detect);
    let (quick, quick_warnings) = Report::list(&files);
    let targets: Vec<&str> = rows.iter().map(|report| report.target.as_str()).collect();
    assert_eq!(
        targets,
        [
            "claude",
            "codex",
            "codex-desktop",
            "gemini",
            "claude-desktop"
        ]
    );
    assert!(rows.iter().all(|report| report.status == Status::Absent));
    let statuses = |rows: &[Report]| -> Vec<(String, Status)> {
        rows.iter()
            .map(|report| (report.target.clone(), report.status.clone()))
            .collect()
    };
    assert_eq!(statuses(&rows), statuses(&quick));
    assert_eq!(warnings, quick_warnings);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("is outside /Applications"));
    // The service's PATH had neither CLI, so the probe found neither; the
    // desktop app is found by a look in the Applications folders.
    let claude = rows
        .iter()
        .find(|report| report.target == "claude")
        .expect("claude");
    assert_eq!(claude.detected, None);
    let desktop = rows
        .iter()
        .find(|report| report.target == "codex-desktop")
        .expect("Codex Desktop");
    assert_eq!(
        desktop
            .detected
            .as_ref()
            .map(|found| (found.path.as_str(), found.version.as_str())),
        Some(("/Applications/ChatGPT.app", ""))
    );
    assert!(quick.iter().all(|report| report.detected.is_none()));
    assert_eq!((claude.files.len(), claude.backups.len()), (4, 0));
    assert!(!claude.limits.is_empty() && claude.last_event_at.is_none());
    // The old-helper count beside the rows.
    assert_eq!(detect["claude_channels_stale"], 0);
    assert_eq!(files["claude_channels_stale"], 0);
}

#[test]
fn voice_engines_without_an_audio_engine_is_a_state_and_not_an_older_service() {
    let reply = recorded!("voice.engines");
    let error: ApiError = serde_json::from_value(reply["error"].clone()).expect("an API error");
    assert_eq!(error.code, ApiError::DEGRADED);
    assert_eq!(error.message, "degraded");
    assert_eq!(error.data["reason"], "voice_unavailable");
    let failure = ClientError::Rpc {
        code: error.code,
        message: error.message,
        data: Some(error.data),
    };
    // The Voice page keeps its engines unread and offers the settings'
    // locale; it never says a newer service is needed.
    assert!(!method_missing(&failure));
}

#[test]
fn neither_local_model_is_on_a_fresh_mac_and_each_says_its_size() {
    let stt = SttStatus::parse(&recorded!("stt.status"));
    assert_eq!(
        (stt.engine.as_str(), stt.model.as_str()),
        ("apple", "large-v3-turbo")
    );
    assert_eq!(stt.total, 1_624_555_275);
    assert!(!stt.present && !stt.loaded && !stt.loading && !stt.active);
    assert!(stt.download.is_none() && stt.load_error.is_none());
    assert!(!stt.downloading() && !stt.prompted);
    let tts = TtsStatus::parse(&recorded!("tts.status"));
    assert_eq!(
        (tts.engine.as_str(), tts.model.as_str()),
        ("cloud", "kokoro-multi-lang-v1_1")
    );
    assert_eq!(tts.total, 364_816_464);
    assert!(!tts.present && !tts.loading && !tts.downloading());
    assert!(tts.download.is_none() && tts.load_error.is_none());
    assert_eq!(tts.voices, ["af_maple", "zm_010", "bf_vale", "zf_001"]);
}

#[test]
fn service_info_names_the_port_it_holds_and_the_executable_it_runs_from() {
    let reply = recorded!("service.info");
    let info = Info::parse(&reply);
    assert_eq!(info.api.as_deref(), Some("1"));
    assert_eq!(info.daemon.as_deref(), Some("0.13.0"));
    assert_eq!(info.platform.as_deref(), Some("macos"));
    assert_eq!(info.uds.as_deref(), Some("d/deckd.sock"));
    assert_eq!(info.port, Some(52_098));
    assert_eq!(info.degraded, None);
    assert_eq!(info.listener, None);
    assert_eq!(info.sessions, Some(2));
    assert_eq!(reply["executable"], "/tmp/owt/bin/deckd-s6");
    // The same port as the snapshot's own.
    let snapshot = recorded!("state.get");
    assert_eq!(snapshot["snapshot"]["service"]["port"], 52_098);
}

#[test]
fn a_fresh_service_has_no_tokens_and_answers_a_bare_empty_array() {
    let reply = recorded!("token.list");
    assert_eq!(reply, json!([]));
    assert!(TokenRow::list(&reply).is_empty());
}

#[test]
fn device_get_is_the_snapshots_device_and_a_deck_never_linked_says_nothing_of_itself() {
    let reply = recorded!("device.get");
    let device: DeviceState = serde_json::from_value(reply.clone()).expect("a device state");
    let state = recorded!("state.get");
    let snapshot: Snapshot = serde_json::from_value(state["snapshot"].clone()).expect("a snapshot");
    assert_eq!(device, snapshot.device);
    assert_eq!(device.link, Link::Offline);
    assert_eq!(
        (device.fw.as_deref(), device.board.as_deref(), device.face),
        (None, None, None)
    );
    assert_eq!(
        (device.transport, device.battery, device.charging),
        (None, None, None)
    );
    assert!(
        device.last.is_none(),
        "no deck was ever linked to this data directory"
    );
    assert!(device.faces.is_empty() && device.drops.is_empty());
    // The keys a deck fills in are left out of the reply, not sent as null.
    for key in ["transport", "battery", "charging", "board", "last", "drops"] {
        assert!(reply.get(key).is_none(), "{key}");
    }
}

#[test]
fn a_state_get_reply_installs_the_mirror_and_leaves_every_session_out() {
    let reply = recorded!("state.get");
    assert_eq!(
        reply["snapshot"]["agents"]
            .as_object()
            .map(|agents| agents.len()),
        Some(6),
        "the demo's roster is in the reply"
    );
    let event = ApiEvent::snapshot_reply(&reply).expect("a state.get reply is a snapshot");
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(event));
    let mirror = state.mirror();
    assert_eq!(mirror.rev, 30);
    assert!(mirror.agents.is_empty(), "never a session");
    assert_eq!(mirror.device.link, Link::Offline);
    assert!(!state.connected(), "a reply claims no connection");
}

#[test]
fn the_hellos_snapshot_is_the_mirror_the_pages_and_the_menu_read() {
    let hello = recorded!("session.hello");
    assert_eq!(hello["rev"], 30);
    // What `deck_client` makes of a hello's `snapshot`.
    let snapshot: Snapshot =
        serde_json::from_value(hello["snapshot"].clone()).expect("the hello's snapshot");
    assert_eq!(
        snapshot.agents.len(),
        6,
        "the demo's roster, which no page shows"
    );
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    assert!(state.connected() && state.seen());
    let mirror = state.mirror();
    assert!(mirror.agents.is_empty(), "never a session");
    // The same service at the same rev: `state.get` makes the same mirror.
    let mut again = HostState::detached();
    let reply = ApiEvent::snapshot_reply(&recorded!("state.get")).expect("a snapshot");
    again.handle_event(AppEvent::Api(reply));
    assert_eq!(again.mirror(), mirror);
    // A deck never linked: no strip, nothing remembered.
    assert_eq!(mirror.device.link, Link::Offline);
    assert!(mirror.device.last.is_none() && mirror.device.drops.is_empty());
    assert!(mirror.faces.is_empty() && mirror.device.faces.is_empty());
    // The settings the Deck page's controls show.
    let settings = mirror.settings;
    assert_eq!(
        (
            settings.speaker,
            settings.mic,
            settings.brightness,
            settings.idlehome,
            settings.textsize,
            settings.language,
        ),
        (6, 6, 5, 120, 1, 0)
    );
    assert_eq!(
        (
            settings.announce,
            settings.chime,
            settings.autojump,
            settings.demo
        ),
        (1, 1, 0, 1)
    );
    assert_eq!(mirror.device.settings, settings);
    // The Macropad keys the service keeps for face 3, which no face on the
    // (empty) strip opens yet.
    let macropad = mirror.macros.get(&3).expect("face 3's keys");
    assert_eq!(macropad.src.as_deref(), Some("deck.toml"));
    let labels: Vec<&str> = macropad.keys.iter().map(|key| key.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "build",
            "test",
            "deploy",
            "git push",
            "screenshot",
            "lock",
            "mute",
            "term"
        ]
    );
    assert!(
        macropad.keys.iter().all(|key| key.keys.is_none()),
        "no keystroke set"
    );
    assert!(macropad.keys[4].binding.is_none() && macropad.keys[0].binding.is_some());
    // The permissions the Voice and Service pages show: none read yet.
    let permissions = mirror.voice.permissions;
    assert_eq!(
        (
            permissions.bluetooth,
            permissions.speech,
            permissions.microphone
        ),
        (
            Permission::Unknown,
            Permission::Unknown,
            Permission::Unknown
        )
    );
    assert_eq!(
        (mirror.service.port, mirror.service.degraded.as_deref()),
        (52_098, None)
    );
    // The status item: not linked, nothing to attend to, Auto-Announce
    // ticked, and Pause Integrations unticked once `config.get` has said.
    let list = attention(mirror, true, RECORDED_AT, &Extras::default());
    assert!(list.is_empty());
    let paused = SourcesConfig::parse(&recorded!("config.get")).effective_pause();
    let menu = MenuModel::of(
        mirror,
        true,
        NoService::NotRunning,
        paused,
        &list,
        Language::English,
    );
    assert_eq!(menu.icon, IconState::NotLinked);
    assert_eq!(menu.status, "Not linked");
    assert!(!menu.link_problem);
    assert!(menu.announce);
    assert_eq!(menu.paused, Some(false));
}
