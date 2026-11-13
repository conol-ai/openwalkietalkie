//! The Service page's rules, read without a window: the rows of "Check my
//! setup" for every input, Start at login for every status the launch agent
//! can have, the local API's line, the update rows, the footprint's rows,
//! order and count, the uninstall command, the port and the tokens of the
//! Local API page, the reads that name an older service, and that every
//! word the page says has a row in all four languages.
use deck_client::ClientError;
use deck_gui_core::admin::{ApiConfig, Info, Report, Secret, Status, TokenRow};
use deck_gui_core::care::{Checks, Footprint, Sources};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ApiEvent, AppEvent, ConfigValue, HostState, UiAction};
use deck_model::{
    ApiError, DeviceState, Event, EventKind, LastLinked, Permission, Principal, Snapshot, Ts,
};
use openwalkietalkie_app::footprint::{self, Action, Place, Removal};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::local_api::{self, PortRefusal, Shown};
use openwalkietalkie_app::present::{self, LoginItem};
use openwalkietalkie_app::service::{
    api_clash, api_line, api_question, check_rows, codex_check, deck_version, executable,
    firmware_sub, legacy_agent, login_row, stale_helpers, support_note, this_copy, update_titles,
    Answer, Api, CheckRow, CodexCheck, Setup, TcpRule, Verdict,
};
use openwalkietalkie_app::updates::{
    self, available, board_name, can_install, confirming, firmware_step, install_board,
    FirmwareStep,
};
use serde_json::{json, Value};
use std::path::Path;

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;

const CARE_CHECKS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_checks.json");
const CARE_FOOTPRINT: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_footprint.json");
const SOURCES_LIST: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/sources_list.json");
const CONFIG_GET: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json");

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).expect("a fixture is JSON")
}

/// The footprint fixture with its paths under this process's home, as the
/// service would answer on this Mac.
fn footprint_here() -> Footprint {
    let home = std::env::var("HOME").expect("a home to print paths from");
    Footprint::parse(&fixture(&CARE_FOOTPRINT.replace("/Users/you", &home)))
}

/// The Service board's setup: every check passed but the old helper.
fn board_setup() -> Setup {
    Setup {
        linked: true,
        this_copy: Some(true),
        claude: Some(Status::Installed),
        stale_helpers: 2,
        codex: CodexCheck::Reachable,
        legacy_agent: Some(true),
        bluetooth: Permission::Granted,
    }
}

fn titles(rows: &[CheckRow]) -> Vec<(Verdict, &str)> {
    rows.iter()
        .map(|row| (row.verdict, row.title.as_str()))
        .collect()
}

#[test]
fn the_seven_checks_read_the_board_and_say_each_problem_with_its_remedy() {
    i18n::install();
    let rows = check_rows(&board_setup(), EN);
    assert_eq!(
        titles(&rows),
        [
            (Verdict::Ok, "The deck is linked"),
            (Verdict::Ok, "The service runs from this copy of the app"),
            (Verdict::Ok, "Claude Code plugin is current"),
            (Verdict::Problem, "2 Claude sessions use an old helper"),
            (Verdict::Ok, "Codex is reachable"),
            (Verdict::Ok, "No leftover launch agent"),
            (Verdict::Ok, "Bluetooth is allowed"),
        ]
    );
    assert_eq!(rows[3].sub.as_deref(), Some("Restart those sessions."));
    assert!(rows
        .iter()
        .filter(|row| row.verdict == Verdict::Ok)
        .all(|row| row.sub.is_none()));
    // Each problem, in its own words, with the remedy where there is one.
    let problems = Setup {
        linked: false,
        this_copy: Some(false),
        claude: Some(Status::Outdated),
        stale_helpers: 1,
        codex: CodexCheck::NotRunning,
        legacy_agent: Some(false),
        bluetooth: Permission::Denied,
    };
    let rows = check_rows(&problems, EN);
    assert!(rows.iter().all(|row| row.verdict == Verdict::Problem));
    let said: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|row| (row.title.as_str(), row.sub.as_deref()))
        .collect();
    assert_eq!(
        said,
        [
            ("The deck is not linked", None),
            (
                "The service runs from another copy of the app",
                Some("Quit the other copy, then restart the service.")
            ),
            ("Claude Code plugin needs repair", None),
            (
                "1 Claude session uses an old helper",
                Some("Restart that session.")
            ),
            ("Codex is not running", None),
            (
                "An old launch agent is still installed",
                Some("Remove it with Uninstall, or by hand in ~/Library/LaunchAgents.")
            ),
            ("Bluetooth is not allowed", None),
        ]
    );
    // A plugin that is not there is not set up; any other word needs repair.
    for (status, title) in [
        (Status::Absent, "Claude Code plugin is not set up"),
        (Status::Conflict, "Claude Code plugin needs repair"),
        (
            Status::Unknown("newer".into()),
            "Claude Code plugin needs repair",
        ),
    ] {
        let setup = Setup {
            claude: Some(status),
            ..board_setup()
        };
        assert_eq!(check_rows(&setup, EN)[2].title, title);
    }
    // In the window's language.
    assert_eq!(check_rows(&board_setup(), ZH)[0].title, "设备已连接");
}

#[test]
fn a_check_the_page_could_not_tell_is_neither_passed_nor_failed_and_codex_off_is_no_row() {
    i18n::install();
    let unknown = Setup {
        linked: true,
        this_copy: None,
        claude: None,
        stale_helpers: 0,
        codex: CodexCheck::NotChecked,
        legacy_agent: None,
        bluetooth: Permission::NotDetermined,
    };
    let rows = check_rows(&unknown, EN);
    // No old helper, so six rows; each that could not be told says nothing
    // either way and keeps the words of the check.
    assert_eq!(
        titles(&rows),
        [
            (Verdict::Ok, "The deck is linked"),
            (
                Verdict::NotChecked,
                "The service runs from this copy of the app"
            ),
            (Verdict::NotChecked, "Claude Code plugin is current"),
            (Verdict::NotChecked, "Codex is reachable"),
            (Verdict::NotChecked, "No leftover launch agent"),
            (Verdict::NotChecked, "Bluetooth is allowed"),
        ]
    );
    assert!(rows.iter().all(|row| row.sub.is_none()));
    // Bluetooth the service has never been told about is not checked either.
    let rows = check_rows(
        &Setup {
            bluetooth: Permission::Unknown,
            ..unknown.clone()
        },
        EN,
    );
    assert_eq!(
        rows.last().map(|row| row.verdict),
        Some(Verdict::NotChecked)
    );
    // The Codex client switched off: the row is not drawn at all.
    let off = check_rows(
        &Setup {
            codex: CodexCheck::Off,
            ..unknown
        },
        EN,
    );
    assert_eq!(off.len(), 5);
    assert!(!off.iter().any(|row| row.title.contains("Codex")));
}

#[test]
fn the_service_runs_from_this_copy_when_its_executable_is_the_deckd_beside_the_app() {
    let bundled = Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    assert_eq!(this_copy(Some(bundled), Some(bundled)), Some(true));
    let other = Path::new("/Users/you/Downloads/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    assert_eq!(this_copy(Some(other), Some(bundled)), Some(false));
    // Either side unknown (an older service, a copy with no deckd beside it):
    // not checked, never "another copy".
    assert_eq!(this_copy(None, Some(bundled)), None);
    assert_eq!(this_copy(Some(bundled), None), None);
}

#[test]
fn codex_is_read_from_the_services_check_then_from_its_source() {
    let checks = Checks::parse(&fixture(CARE_CHECKS));
    let sources = Sources::parse(&fixture(SOURCES_LIST));
    assert_eq!(
        codex_check(Some(&checks), Some(&sources)),
        CodexCheck::Reachable
    );
    let check = |ok: Value| {
        Checks::parse(&json!({"checks": [{"name": "codex daemon", "ok": ok, "detail": "x"}]}))
    };
    assert_eq!(
        codex_check(Some(&check(json!(false))), None),
        CodexCheck::NotRunning
    );
    // The service does not check a client the owner switched off.
    assert_eq!(
        codex_check(Some(&check(Value::Null)), None),
        CodexCheck::Off
    );
    // An older service with no such check: the Codex source says.
    let source = |enabled: bool, state: &str| {
        Sources::parse(&json!({"sources": [
            {"id": "codex", "kind": "tool", "enabled": enabled, "state": state}
        ]}))
    };
    assert_eq!(
        codex_check(None, Some(&source(true, "connected"))),
        CodexCheck::Reachable
    );
    assert_eq!(
        codex_check(None, Some(&source(true, "waiting"))),
        CodexCheck::NotRunning
    );
    assert_eq!(
        codex_check(None, Some(&source(false, "off"))),
        CodexCheck::Off
    );
    assert_eq!(
        codex_check(None, Some(&source(true, "beaming"))),
        CodexCheck::NotChecked
    );
    assert_eq!(codex_check(None, None), CodexCheck::NotChecked);
}

#[test]
fn the_leftover_agent_and_the_old_helpers_are_the_services_to_say() {
    let checks = Checks::parse(&fixture(CARE_CHECKS));
    assert_eq!(legacy_agent(Some(&checks)), Some(true));
    let found = Checks::parse(&json!({"checks": [{
        "name": "legacy launch agent", "ok": false,
        "detail": "~/Library/LaunchAgents/com.conol.openmicrodial.deckd.plist is on disk"
    }]}));
    assert_eq!(legacy_agent(Some(&found)), Some(false));
    assert_eq!(legacy_agent(Some(&Checks::default())), None);
    assert_eq!(legacy_agent(None), None);
    // The Claude Channels source's count, else what Integrations read.
    let sources = Sources::parse(&fixture(SOURCES_LIST));
    assert_eq!(stale_helpers(Some(&sources), 0), 2);
    assert_eq!(stale_helpers(None, 3), 3);
    assert_eq!(stale_helpers(Some(&Sources::default()), 1), 1);
}

#[test]
fn start_at_login_is_the_launch_agent_as_macos_reports_it() {
    i18n::install();
    // On while macOS has it registered, or waits for the owner's approval.
    for (agent, on) in [
        (LoginItem::Enabled, true),
        (LoginItem::RequiresApproval, true),
        (LoginItem::NotRegistered, false),
        (LoginItem::NotFound, false),
        (LoginItem::Unknown(7), false),
    ] {
        let row = login_row(agent, true, true, EN);
        assert_eq!(row.on, on, "{agent:?}");
        assert!(row.enabled, "{agent:?}");
        assert_eq!(
            row.settings,
            agent == LoginItem::RequiresApproval,
            "{agent:?}"
        );
    }
    assert_eq!(login_row(LoginItem::Enabled, true, true, EN).sub, None);
    assert_eq!(
        login_row(LoginItem::RequiresApproval, true, true, EN).sub,
        Some("Waiting for your approval in System Settings > General > Login Items.")
    );
    // Any other run draws the switch and cannot move it, and says why.
    for deck_seen in [false, true] {
        let elsewhere = login_row(LoginItem::NotFound, false, deck_seen, EN);
        assert!(!elsewhere.enabled && !elsewhere.on && !elsewhere.settings);
        assert_eq!(elsewhere.sub, Some("Only in the installed app."));
    }
    assert_eq!(
        login_row(LoginItem::NotFound, false, true, ZH).sub,
        Some("仅限已安装的应用。")
    );
}

#[test]
fn start_at_login_adds_the_service_only_once_this_mac_has_seen_a_deck() {
    i18n::install();
    // Owner decision 7: turning it on registers the service, which waits
    // for a deck. Until one has been seen the switch that is off cannot be
    // moved, and says what to do.
    for agent in [
        LoginItem::NotRegistered,
        LoginItem::NotFound,
        LoginItem::Unknown(7),
    ] {
        let row = login_row(agent, true, false, EN);
        assert!(!row.on && !row.enabled && !row.settings, "{agent:?}");
        assert_eq!(row.sub, Some("Link the deck first."), "{agent:?}");
        assert_eq!(login_row(agent, true, false, ZH).sub, Some("请先连接设备。"));
        // Seen: it can be turned on.
        let seen = login_row(agent, true, true, EN);
        assert!(seen.enabled && seen.sub.is_none(), "{agent:?}");
    }
    // One that is on can always be turned off: that adds nothing.
    let on = login_row(LoginItem::Enabled, true, false, EN);
    assert!(on.on && on.enabled && on.sub.is_none());
    let waiting = login_row(LoginItem::RequiresApproval, true, false, EN);
    assert!(waiting.on && waiting.enabled && waiting.settings);
    assert_eq!(
        waiting.sub,
        Some("Waiting for your approval in System Settings > General > Login Items.")
    );
}

#[test]
fn another_program_on_the_port_is_said_on_the_local_api_row_and_never_as_the_hooks() {
    i18n::install();
    let on = Api::parse(&fixture(CONFIG_GET));
    let mut config = fixture(CONFIG_GET);
    config["api"]["tcp"] = false.into();
    let off = Api::parse(&config);
    let mut service = deck_model::ServiceState {
        port: 47_201,
        ..deck_model::ServiceState::default()
    };
    // Nothing clashes: no row.
    assert_eq!(api_clash(Some(&on), &service, EN), None);
    service.degraded = Some("port_in_use".into());
    assert_eq!(
        api_clash(Some(&on), &service, EN).as_deref(),
        Some(
            "Another program holds port 47201, so the local API is closed until the port is \
             free. Nothing shipped with the deck uses it."
        )
    );
    // Before `config.get` has answered it is still said: the mirror has it.
    assert!(api_clash(None, &service, EN).is_some());
    service.degraded = Some("hooks_hijack_risk".into());
    let held = api_clash(Some(&on), &service, EN).unwrap();
    assert!(held.starts_with("Another program answers on port 47201"), "{held}");
    for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
        let said = api_clash(Some(&on), &service, language).unwrap();
        assert!(said.contains("47201") && said.contains("API"), "{said}");
        assert!(!said.contains("Claude"), "{said}");
    }
    // A door switched off binds nothing, so nothing clashes with it.
    assert_eq!(api_clash(Some(&off), &service, EN), None);
    // Any other degradation is not the local API's to say.
    service.degraded = Some("disk_full".into());
    assert_eq!(api_clash(Some(&on), &service, EN), None);
}

#[test]
fn the_support_file_note_says_whether_session_text_is_in_the_file() {
    i18n::install();
    let left_out = "The file is saved to the Desktop. Session text is left out.";
    let kept = "The file is saved to the Desktop. Session text is kept in it.";
    // The board's line while content logging is off, which is the service's
    // default and what a fresh service's own `config.get` says, and before
    // `config.get` has answered.
    let fresh = Api::parse(&fixture(RECORDED_CONFIG));
    assert_eq!(fresh.log_content, Some(false));
    assert_eq!(support_note(Some(&fresh), EN), left_out);
    assert_eq!(support_note(None, EN), left_out);
    // The service keeps the text in its logs while it is on, and so in the
    // file.
    let mut logging = fixture(RECORDED_CONFIG);
    logging["privacy"]["log_content"] = true.into();
    assert_eq!(support_note(Some(&Api::parse(&logging)), EN), kept);
    for note in [left_out, kept] {
        assert!(deck_gui_core::i18n::has_entry(note), "{note}");
        for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
            let said = language.text(note);
            assert_ne!(said, note, "{language:?}");
            // One line, as the board draws it, in every language.
            assert!(!said.contains('\n'), "{language:?}: {said}");
        }
    }
    // The three-line wording that moved the Footprint section 32 pt down
    // has no row left.
    assert!(!deck_gui_core::i18n::has_entry(
        "The file is saved to the Desktop. Prompts, replies and transcripts are left \
         out unless content logging is on; project names are kept."
    ));
}

#[test]
fn the_local_api_row_says_off_or_on_and_its_port() {
    i18n::install();
    let mut config = fixture(CONFIG_GET);
    let on = Api::parse(&config);
    assert_eq!(on.tcp, Some(true));
    assert_eq!(on.config.port, Some(47_201));
    assert_eq!(
        api_line(&on, Some(47_299), EN),
        "On. Port 47299. Local tools can use it with a token."
    );
    // Not held (somebody else's, or the door is coming up): the configured one.
    assert_eq!(
        api_line(&on, None, EN),
        "On. Port 47201. Local tools can use it with a token."
    );
    // Off, which is what a service of this release says unless its owner
    // switched the door on: the words say only that it is off and why that
    // costs nothing, not that something turned it off.
    config["api"]["tcp"] = false.into();
    let off = Api::parse(&config);
    assert_eq!(off.tcp, Some(false));
    assert_eq!(
        api_line(&off, None, EN),
        "Off. Nothing shipped with the deck uses them."
    );
    assert_eq!(
        api_line(&off, None, ZH),
        "已关闭。设备附带的软件都不使用它们。"
    );
    // A service from before the switch: no switch, and its door is open.
    let older = Api::parse(&json!({"api": {"port": 47201}}));
    assert_eq!(older.tcp, None);
    assert!(api_line(&older, None, EN).starts_with("On. Port 47201."));
    // The switch writes one key.
    let (method, params) = UiAction::ConfigSet {
        key: "api.tcp".into(),
        value: ConfigValue::Bool(false),
    }
    .request();
    assert_eq!(method, "config.set");
    assert_eq!(params, json!({"patch": {"api.tcp": false}}));
}

/// The service says why its door is as it is (`api.tcp_rule`), and the
/// switch's question follows it: turning the door off while Claude Code's
/// hooks from before the plugin still use it says so and offers Repair
/// first — the Integrations page's Repair, the door left as it is — until
/// the Claude row is no longer `outdated`, when the question is the plain
/// one again (the service's rule stays until its next start). Any other
/// rule asks the plain questions, and so does a service that does not say.
#[test]
fn the_local_api_off_sheet_offers_repair_first_while_the_old_claude_hooks_use_it() {
    i18n::install();
    let mut config = fixture(CONFIG_GET);
    assert_eq!(Api::parse(&config).tcp_rule, Some(TcpRule::Explicit));
    for (word, rule) in [
        ("default", Some(TcpRule::Default)),
        ("legacy_hooks", Some(TcpRule::LegacyHooks)),
        ("a word from a newer service", None),
    ] {
        config["api"]["tcp_rule"] = word.into();
        assert_eq!(Api::parse(&config).tcp_rule, rule, "{word}");
    }
    let legacy = Api::parse(&config_with_rule("legacy_hooks"));
    let plain_off = |api: Option<&Api>, claude: Option<&Status>| {
        let question = api_question(false, api, claude, EN);
        assert!(!question.repair_first && question.danger, "{question:?}");
        assert_eq!(
            (question.title.as_str(), question.body.as_str(), question.confirm.as_str()),
            ("Turn the local API off?", "The service restarts.", "Turn Off")
        );
    };
    for claude in [None, Some(&Status::Outdated)] {
        let question = api_question(false, Some(&legacy), claude, EN);
        assert!(question.repair_first && !question.danger, "{claude:?}");
        assert_eq!(question.title, "Turn the local API off?");
        assert_eq!(
            question.body,
            "Claude Code's older hooks still use the local API. Repair them first, or Claude \
             Code's questions stay in the terminal."
        );
        assert_eq!(question.confirm, "Repair");
    }
    plain_off(Some(&legacy), Some(&Status::Installed));
    plain_off(Some(&legacy), Some(&Status::Absent));
    for word in ["explicit", "default"] {
        let api = Api::parse(&config_with_rule(word));
        plain_off(Some(&api), Some(&Status::Outdated));
    }
    let older = Api::parse(&json!({"api": {"port": 47201, "tcp": true}}));
    assert_eq!(older.tcp_rule, None);
    plain_off(Some(&older), None);
    plain_off(None, None);
    // Turning it on asks the plain question, whatever the rule.
    let on = api_question(true, Some(&legacy), None, EN);
    assert!(!on.repair_first && !on.danger);
    assert_eq!(
        (on.title.as_str(), on.body.as_str(), on.confirm.as_str()),
        ("Turn the local API on?", "The service restarts.", "Turn On")
    );
    // In the other three languages, with the shipped word for Repair.
    for (language, body, repair) in [
        (
            ZH,
            "Claude Code 插件化之前的钩子仍在使用本地 API。请先“修复”，否则 Claude Code 的问题只会留在终端中。",
            "修复",
        ),
        (
            Language::TraditionalChinese,
            "Claude Code 外掛化之前的掛鉤仍在使用本機 API。請先「修復」，否則 Claude Code 的問題只會留在終端機中。",
            "修復",
        ),
        (
            Language::Japanese,
            "Claude Code の従来のフックがまだローカル API を使っています。先に「修復」してください。修復しないと、Claude Code の質問はターミナルにしか表示されません。",
            "修復",
        ),
    ] {
        let question = api_question(false, Some(&legacy), None, language);
        assert_eq!(
            (question.body.as_str(), question.confirm.as_str()),
            (body, repair),
            "{language:?}"
        );
        assert_eq!(question.title, language.text("Turn the local API off?"));
        assert_ne!(question.title, "Turn the local API off?", "{language:?}");
    }
}

/// The fixture's `config.get`, its door's rule `word`.
fn config_with_rule(word: &str) -> Value {
    let mut config = fixture(CONFIG_GET);
    config["api"]["tcp_rule"] = word.into();
    config
}

#[test]
fn the_update_rows_name_this_app_and_the_firmware_the_deck_runs_or_ran() {
    i18n::install();
    let linked = DeviceState {
        fw: Some("0.13.0-openwalkietalkie".into()),
        ..DeviceState::default()
    };
    assert_eq!(deck_version(&linked).as_deref(), Some("0.13.0"));
    // Away: what it ran when it was last linked.
    let away = DeviceState {
        last: Some(LastLinked {
            at: Ts::from_millis(1),
            fw: Some("0.12.4-openwalkietalkie".into()),
            board: Some("Rev A".into()),
            transport: None,
        }),
        ..DeviceState::default()
    };
    assert_eq!(deck_version(&away).as_deref(), Some("0.12.4"));
    assert_eq!(deck_version(&DeviceState::default()), None);
    let (app, deck) = update_titles(Some("0.13.0"), EN);
    assert_eq!(
        app,
        format!("OpenWalkieTalkie {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(deck, "Deck firmware 0.13.0");
    assert_eq!(update_titles(None, EN).1, "Deck firmware \u{2014}");
    assert_eq!(update_titles(Some("0.13.0"), ZH).1, "设备固件 0.13.0");
}

#[test]
fn the_firmware_row_checks_until_there_is_something_to_review() {
    let idle =
        json!({"phase": "idle", "message": "Connect your deck with USB to update its firmware."});
    assert_eq!(firmware_step(&idle), FirmwareStep::Check);
    assert_eq!(firmware_step(&Value::Null), FirmwareStep::Check);
    let found = json!({
        "phase": "ready", "available": true,
        "device": {"serial": "5E2A91C04B7D", "board": "hw-b", "version": "0.13.0"},
        "release": {"firmware": {"version": "0.14.0", "protocol": 2, "variants": {"hw-b": {}}}}
    });
    assert_eq!(available(&found).as_deref(), Some("0.14.0"));
    assert_eq!(firmware_step(&found), FirmwareStep::Review);
    assert!(can_install(&found, install_board(&found, None)));
    // A board the release has no build for, or a deck that is not on the
    // cable, cannot be installed.
    assert!(!can_install(&found, "hw-a"));
    let mut cableless = found.clone();
    cableless["device"]["serial"] = "".into();
    assert!(!can_install(&cableless, "hw-b"));
    // Working on it, or one stopped half way: review, never check again.
    assert_eq!(
        firmware_step(&json!({"phase": "downloading", "progress": 40})),
        FirmwareStep::Review
    );
    assert!(!can_install(
        &json!({"phase": "installing", "available": true, "device": {"serial": "x"},
                "release": {"firmware": {"version": "0.14.0", "variants": {"hw-b": {}}}}}),
        "hw-b"
    ));
    assert_eq!(
        firmware_step(&json!({"device": {"maintenance": true}})),
        FirmwareStep::Review
    );
    // The board the owner picks wins over the one the deck names.
    assert_eq!(install_board(&found, Some("hw-a")), "hw-a");
    assert_eq!(board_name("hw-a"), Some("Rev A"));
    assert_eq!(board_name("hw-b"), Some("Rev B"));
    assert_eq!(board_name("lilygo"), None);
    assert_eq!(updates::BOARDS.len(), 2);
}

/// Every sentence the service's firmware status can say, read out of
/// `deckd`'s own firmware sources (their tests left out): the literal each
/// `message` is set to, and the message of each `progress("phase", n, "…")`
/// step of an install. The review dialog looks them up by their English,
/// which the page's word check cannot see, so this test reads them.
fn firmware_sentences() -> Vec<String> {
    /// The string literal `text` starts with; `deckd`'s sentences hold no
    /// escape.
    fn literal(text: &str) -> Option<&str> {
        let rest = text.strip_prefix('"')?;
        rest.find('"').map(|end| &rest[..end])
    }
    let mut said: Vec<String> = Vec::new();
    for source in [
        include_str!("../../deckd/src/firmware/mod.rs"),
        include_str!("../../deckd/src/firmware/migration.rs"),
    ] {
        let code = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(source);
        for marker in ["message:", "message ="] {
            for (at, _) in code.match_indices(marker) {
                if let Some(sentence) = literal(code[at + marker.len()..].trim_start()) {
                    said.push(sentence.to_string());
                }
            }
        }
        for (at, _) in code.match_indices("progress(") {
            // The phase, then the percent, then the message.
            let args = code[at + "progress(".len()..].trim_start();
            let Some(phase) = literal(args) else {
                continue;
            };
            let after = &args[phase.len() + 2..];
            let Some(open) = after.find('"') else {
                continue;
            };
            if after[..open].contains(')') {
                continue;
            }
            if let Some(sentence) = literal(&after[open..]) {
                said.push(sentence.to_string());
            }
        }
    }
    said.sort();
    said.dedup();
    said
}

#[test]
fn every_sentence_of_the_services_firmware_status_has_a_row() {
    i18n::install();
    let said = firmware_sentences();
    // Pinned, so a sentence the reading stopped finding fails here.
    assert_eq!(said.len(), 15, "{said:#?}");
    for sentence in [
        "Firmware release checked. Installation requires your confirmation.",
        "Press ENTER on the deck to start the install.",
        "Not confirmed on the deck. Nothing was installed.",
        "Starting the install when the deck stops talking…",
    ] {
        assert!(said.contains(&sentence.into()), "{sentence:?} {said:#?}");
    }
    for sentence in &said {
        assert!(deck_gui_core::i18n::has_entry(sentence), "{sentence:?} has no row");
        for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
            assert_ne!(language.text(sentence), sentence, "{language:?}: {sentence}");
        }
    }
}

#[test]
fn the_firmware_dialog_says_the_services_status_in_the_windows_language() {
    i18n::install();
    // A fresh service's own status.
    let idle = fixture(RECORDED_FIRMWARE);
    assert_eq!(
        updates::status_line(&idle, EN),
        "Connect your deck with USB to update its firmware."
    );
    assert_eq!(
        updates::status_line(&idle, ZH),
        "请通过 USB 连接设备以更新固件。"
    );
    let ready = json!({
        "phase": "ready",
        "message": "Firmware release checked. Installation requires your confirmation."
    });
    assert_eq!(
        updates::status_line(&ready, Language::Japanese),
        "ファームウェアを確認しました。インストールには承認が必要です。"
    );
    // While it works on the deck, with how far it has got.
    let installing = json!({
        "phase": "installing",
        "progress": 45,
        "message": "Installing firmware. Keep USB connected."
    });
    assert_eq!(
        updates::status_line(&installing, EN),
        "Installing firmware. Keep USB connected. 45%"
    );
    assert_eq!(
        updates::status_line(&installing, Language::TraditionalChinese),
        "正在安裝韌體，請保持 USB 連線。 45%"
    );
    // A sentence a newer service adds is shown as it came.
    let newer = json!({"phase": "ready", "message": "Firmware 0.15 is new."});
    assert_eq!(updates::status_line(&newer, ZH), "Firmware 0.15 is new.");
    // Before the service has said anything: how to begin.
    assert_eq!(
        updates::status_line(&Value::Null, ZH),
        "请通过 USB 连接设备后检查固件更新。"
    );
}

/// A firmware status in `phase`, `progress` per cent of the way, saying
/// `message`.
fn phase(phase: &str, progress: u8, message: &str) -> Value {
    json!({"phase": phase, "progress": progress, "message": message})
}

/// The dialog's words for every phase an install goes through, as the
/// service writes them: looking, found, waiting for ENTER on the deck (what
/// to press there, never a percentage), pressed and waiting for the deck to
/// stop talking, working with how far it has got, done, failed — and idle
/// again with the reason when the deck did not confirm.
#[test]
fn the_firmware_dialog_says_every_phase_of_an_install() {
    i18n::install();
    let walk = [
        (
            phase("checking", 0, "Checking for firmware updates…"),
            "Checking for firmware updates… 0%",
        ),
        (
            phase(
                "ready",
                0,
                "Firmware release checked. Installation requires your confirmation.",
            ),
            "Firmware release checked. Installation requires your confirmation.",
        ),
        (
            phase(
                "confirming",
                0,
                "Press ENTER on the deck to start the install.",
            ),
            "Press ENTER on the deck to start the install.",
        ),
        (
            phase(
                "confirming",
                0,
                "Starting the install when the deck stops talking…",
            ),
            "Starting the install when the deck stops talking…",
        ),
        (
            phase("downloading", 12, "Downloading and verifying firmware…"),
            "Downloading and verifying firmware… 12%",
        ),
        (
            phase("installing", 45, "Installing firmware. Keep USB connected."),
            "Installing firmware. Keep USB connected. 45%",
        ),
        (
            phase("complete", 100, "Firmware updated. Your deck is ready."),
            "Firmware updated. Your deck is ready.",
        ),
        (
            phase(
                "error",
                0,
                "Update did not complete. You can check again and retry.",
            ),
            "Update did not complete. You can check again and retry.",
        ),
        (
            phase(
                "idle",
                0,
                "Not confirmed on the deck. Nothing was installed.",
            ),
            "Not confirmed on the deck. Nothing was installed.",
        ),
    ];
    for (status, said) in &walk {
        assert_eq!(updates::status_line(status, EN), *said, "{status}");
    }
    // Waiting for the deck says what to press when the service said
    // nothing else, and has no percentage in any language.
    let waiting = phase("confirming", 0, "");
    for (language, said) in [
        (EN, "Press ENTER on the deck to start the install."),
        (ZH, "请在设备上按 ENTER 开始安装。"),
        (
            Language::TraditionalChinese,
            "請在裝置上按 ENTER 開始安裝。",
        ),
        (
            Language::Japanese,
            "デバイスで ENTER を押して、インストールを開始してください。",
        ),
    ] {
        assert_eq!(
            updates::status_line(&waiting, language),
            said,
            "{language:?}"
        );
    }
    // Pressed, while the voice holds the install back: the service says it
    // has the press and what it waits for, still with no percentage.
    let pressed = &walk[3].0;
    for (language, said) in [
        (ZH, "设备上的语音结束后将开始安装…"),
        (Language::TraditionalChinese, "裝置上的語音結束後將開始安裝…"),
        (
            Language::Japanese,
            "デバイスの音声が終わったら、インストールを開始します…",
        ),
    ] {
        assert_eq!(
            updates::status_line(pressed, language),
            said,
            "{language:?}"
        );
    }
    let not_confirmed = &walk.last().expect("the walk").0;
    for (language, said) in [
        (ZH, "未在设备上确认，未安装任何内容。"),
        (
            Language::TraditionalChinese,
            "未在裝置上確認，未安裝任何內容。",
        ),
        (
            Language::Japanese,
            "デバイスで承認されませんでした。何もインストールしていません。",
        ),
    ] {
        assert_eq!(
            updates::status_line(not_confirmed, language),
            said,
            "{language:?}"
        );
    }
}

/// While the install waits for the deck the service is busy: the row
/// offers the review, the dialog's Install is not offered again and the
/// status is read every second. A card the deck did not confirm leaves the
/// release to install again.
#[test]
fn an_install_waiting_for_the_deck_is_busy_until_the_deck_answers() {
    let found = json!({
        "phase": "ready", "available": true,
        "device": {"serial": "5E2A91C04B7D", "board": "hw-b", "version": "0.13.0"},
        "release": {"firmware": {"version": "0.14.0", "protocol": 2, "variants": {"hw-b": {}}}}
    });
    assert!(can_install(&found, "hw-b") && !confirming(&found));
    let mut waiting = found.clone();
    waiting["phase"] = "confirming".into();
    assert!(confirming(&waiting) && updates::busy(&waiting));
    assert!(!can_install(&waiting, "hw-b"), "one install at a time");
    assert_eq!(firmware_step(&waiting), FirmwareStep::Review);
    let mut not_confirmed = found;
    not_confirmed["phase"] = "idle".into();
    not_confirmed["message"] = "Not confirmed on the deck. Nothing was installed.".into();
    assert!(!updates::busy(&not_confirmed) && !confirming(&not_confirmed));
    assert!(can_install(&not_confirmed, "hw-b"), "it can be asked again");
    assert_eq!(firmware_step(&not_confirmed), FirmwareStep::Review);
}

/// An install the service refuses comes back as its reason, which the
/// window says in its own language: a deck that is not in recovery and has
/// not linked cannot be asked for the press, so it is told to try again
/// once the deck links. The sentence is the service's own
/// (`deckd::firmware::WAIT_FOR_LINK`), word for word. A reason with no row
/// is shown as it came, and a failure with no reason as the error says it.
#[test]
fn an_install_refused_until_the_deck_links_says_why_in_the_windows_language() {
    i18n::install();
    const WAIT: &str = "Wait for the deck to link, then install again.";
    assert!(
        include_str!("../../deckd/src/firmware/mod.rs")
            .contains(&format!("pub const WAIT_FOR_LINK: &str = \"{WAIT}\";")),
        "the service's sentence and this row are one"
    );
    let refused = |why: &str| ClientError::Rpc {
        code: ApiError::INVALID,
        message: "invalid".into(),
        data: Some(json!({"why": why})),
    };
    let said = [EN, ZH, Language::TraditionalChinese, Language::Japanese]
        .map(|language| updates::refusal_line(&refused(WAIT), language));
    assert_eq!(
        said,
        [
            WAIT,
            "请等待设备连接，然后重新安装。",
            "請等待裝置連線，然後重新安裝。",
            "デバイスが接続されるのを待ってから、もう一度インストールしてください。",
        ]
    );
    assert_eq!(
        updates::refusal_line(&refused("A reason from a newer service."), ZH),
        "A reason from a newer service."
    );
    let plain = ClientError::Rpc {
        code: ApiError::INVALID,
        message: "invalid".into(),
        data: None,
    };
    assert_eq!(updates::refusal_line(&plain, ZH), updates::failure_text(&plain));
}

/// The firmware row says how an install reaches the deck, in each of the
/// four languages: by cable, after a press on it.
#[test]
fn the_firmware_row_says_an_install_needs_a_press_on_the_deck() {
    i18n::install();
    let said = [EN, ZH, Language::TraditionalChinese, Language::Japanese].map(firmware_sub);
    assert_eq!(
        said,
        [
            "By cable, after a press on the deck.",
            "通过数据线更新，需先在设备上按键确认。",
            "透過連接線更新，需先在裝置上按鍵確認。",
            "ケーブル接続で、本体のボタンを押して開始。",
        ]
    );
}

#[test]
fn the_footprint_rows_are_the_services_in_the_boards_order() {
    i18n::install();
    let rows = footprint::items(&footprint_here(), EN);
    let names: Vec<&str> = rows.iter().map(|row| row.name).collect();
    assert_eq!(
        names,
        [
            "Background service",
            "Claude Code plugin",
            "Leftover Codex setup",
            "Whisper model",
            "Kokoro voice",
            "Firmware downloads",
            "Settings and backups",
            "Preferences",
            "Keychain item",
            "Logs",
        ]
    );
    let places: Vec<&Place> = rows.iter().map(|row| &row.place).collect();
    assert_eq!(
        places[0],
        &Place::Words("deckd, under Login Items in System Settings")
    );
    // Paths from the home, shortened to fit, the name at the end never cut.
    assert_eq!(
        places[1],
        &Place::Path("~/Library/…/OpenWalkieTalkie/claude-marketplace".into())
    );
    assert_eq!(places[2], &Place::Path("~/.codex/hooks.json".into()));
    assert_eq!(
        places[4],
        &Place::Path("~/Library/Caches/OpenWalkieTalkie/tts/kokoro".into())
    );
    assert_eq!(
        places[7],
        &Place::Path("~/Library/…/ai.cohdl.openwalkietalkie.plist".into())
    );
    // The Keychain item has no path to print: its place says whose it is.
    assert_eq!(places[8], &Place::Words("Your cloud voice key"));
    let sizes: Vec<Option<&str>> = rows.iter().map(|row| row.size.as_deref()).collect();
    assert_eq!(
        sizes,
        [
            None,
            Some("13 hooks"),
            Some("7 entries"),
            Some("1.6 GB"),
            Some("428 MB"),
            Some("9 MB"),
            Some("184 KB"),
            Some("4 KB"),
            None,
            Some("81 MB"),
        ]
    );
    let actions: Vec<Action> = rows.iter().map(|row| row.action).collect();
    assert_eq!(
        actions,
        [
            Action::WithUninstall,
            Action::Remove("claude"),
            Action::Remove("codex"),
            Action::Delete(Removal::Whisper),
            Action::Delete(Removal::Kokoro),
            Action::Delete(Removal::Firmware),
            Action::WithUninstall,
            Action::WithUninstall,
            Action::WithUninstall,
            Action::Kept,
        ]
    );
    // In each language, in macOS's own word for the Keychain.
    let keychain = |language| footprint::items(&footprint_here(), language)[8].clone();
    assert_eq!(keychain(ZH).name, "钥匙串项目");
    assert_eq!(keychain(ZH).place, Place::Words("你的云端语音密钥"));
    assert_eq!(keychain(Language::TraditionalChinese).name, "鑰匙圈項目");
    assert_eq!(keychain(Language::Japanese).name, "キーチェーン項目");
    // Only while the service holds a key: an item it found absent has no row.
    let mut value = fixture(CARE_FOOTPRINT);
    value["items"][10]["present"] = json!(false);
    assert!(!footprint::items(&Footprint::parse(&value), EN)
        .iter()
        .any(|row| row.id == "keychain"));
    // The count is singular for one.
    let one = Footprint::parse(&json!({"items": [
        {"id": "claude_plugin", "path": "/x", "bytes": 10, "count": 1, "present": true},
        {"id": "codex_hooks", "path": "/y", "bytes": 10, "count": 1, "present": true}
    ]}));
    let sizes: Vec<Option<String>> = footprint::items(&one, EN)
        .into_iter()
        .map(|row| row.size)
        .collect();
    assert_eq!(sizes, [Some("1 hook".into()), Some("1 entry".into())]);
    assert_eq!(
        footprint::items(&footprint_here(), ZH)[1].size.as_deref(),
        Some("13 个钩子")
    );
}

#[test]
fn what_is_not_on_this_mac_has_no_row_and_is_not_counted() {
    i18n::install();
    let full = footprint_here();
    assert_eq!(footprint::total(&full), (10, 2_142_803_723));
    assert_eq!(
        footprint::summary(&full, EN),
        "10 items, 2.1 GB. Most can be removed one at a time."
    );
    // The Codex setup the service did not find: no row, no count, no bytes.
    let mut value = fixture(CARE_FOOTPRINT);
    value["items"][2] = json!({
        "id": "codex_hooks", "path": "/Users/you/.codex/hooks.json",
        "bytes": null, "count": null, "present": false
    });
    let without = Footprint::parse(&value);
    let rows = footprint::items(&without, EN);
    assert_eq!(rows.len(), 9);
    assert!(!rows.iter().any(|row| row.id == "codex_hooks"));
    assert_eq!(footprint::total(&without), (9, 2_142_801_675));
    assert_eq!(
        footprint::summary(&without, ZH),
        "共 9 项，2.1 GB。大多数可以逐项移除。"
    );
    // A newer item this build has no words for is not drawn, and an empty
    // answer is no rows at all.
    let newer =
        Footprint::parse(&json!({"items": [{"id": "hologram", "present": true, "bytes": 5}]}));
    assert!(footprint::items(&newer, EN).is_empty());
    assert_eq!(footprint::total(&newer), (0, 0));
    assert!(footprint::items(&Footprint::default(), EN).is_empty());
}

#[test]
fn each_delete_asks_the_service_for_its_own_removal() {
    assert_eq!(Removal::Whisper.action().request().0, "stt.remove");
    assert_eq!(Removal::Kokoro.action().request().0, "tts.remove");
    let (method, params) = Removal::Firmware.action().request();
    assert_eq!(method, "care.cache.clear");
    assert_eq!(params, json!({"what": "firmware"}));
}

#[test]
fn uninstall_runs_the_bundled_service_with_its_purge_and_without_its_question() {
    let deckd = Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    assert_eq!(
        footprint::uninstall_argv(deckd),
        [
            "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
            "uninstall",
            "--purge",
            "--yes",
        ]
    );
    // What went wrong is read off its report's PROBLEMS heading.
    let report = "  Claude Code: 2 file(s) put back as they were\n\nkept:\n  logs\n\nPROBLEMS:\n  cannot remove /x: denied\n  `launchctl bootout` failed\n";
    assert_eq!(
        footprint::uninstall_problems(report).as_deref(),
        Some("cannot remove /x: denied; `launchctl bootout` failed")
    );
    assert_eq!(footprint::uninstall_problems("  purged /x\n"), None);
}

#[test]
fn a_port_is_typed_checked_and_written_as_one_key_of_a_patch() {
    i18n::install();
    assert_eq!(local_api::port_choice(" 47299 "), Ok(47_299));
    assert_eq!(local_api::port_choice(""), Err(PortRefusal::Empty));
    assert_eq!(local_api::port_choice("http"), Err(PortRefusal::NotAPort));
    assert_eq!(local_api::port_choice("70000"), Err(PortRefusal::NotAPort));
    assert_eq!(local_api::port_choice("0"), Err(PortRefusal::NotAPort));
    assert_eq!(local_api::port_choice("80"), Err(PortRefusal::Privileged));
    assert_eq!(
        PortRefusal::Privileged.words(EN),
        "Ports below 1024 need root, and this daemon is a per-user agent."
    );
    assert_eq!(PortRefusal::Empty.words(ZH), "请先输入新端口。");
    // The wire shape is a patch: the service refuses a bare pair.
    let (method, params) = UiAction::ConfigSet {
        key: "api.port".into(),
        value: ConfigValue::Int(47_202),
    }
    .request();
    assert_eq!(method, "config.set");
    assert_eq!(params["patch"]["api.port"], 47_202);
    assert!(params.get("key").is_none());
    // The service binds its door when it starts.
    assert_eq!(UiAction::ServiceRestart.request().0, "service.restart");
}

#[test]
fn the_door_shows_whose_port_it_is_its_origins_and_a_curl_that_needs_a_token() {
    i18n::install();
    let config = ApiConfig::parse(&fixture(CONFIG_GET));
    assert_eq!(config.port, Some(47_201));
    assert_eq!(
        local_api::port_line(Some(47_201), &config, EN),
        "http://127.0.0.1:47201 · listening"
    );
    // The service names no port while it does not hold one; the page never
    // prints the configured one as though it did.
    let line = local_api::port_line(None, &config, EN);
    assert!(line.contains("not held"), "{line}");
    assert!(!line.contains("listening"), "{line}");
    assert_eq!(
        local_api::port_line(None, &ApiConfig::default(), EN),
        "no TCP port: the daemon is on the socket only"
    );
    // No origin is the default, and a policy.
    assert!(local_api::origins_line(&config.allowed_origins, EN).contains("403"));
    assert_eq!(
        local_api::origins_line(
            &["https://example.com".into(), "http://localhost:3000".into()],
            EN
        ),
        "https://example.com, http://localhost:3000"
    );
    let curl = local_api::curl_example(config.port);
    assert!(curl.contains("http://127.0.0.1:47201/v1/rpc"));
    assert!(curl.contains("Authorization: Bearer <token>"));
    assert!(!curl.contains("--unix-socket"), "the socket is not HTTP");
}

/// A minute on this Mac's clock, as a stamp: what the service writes (RFC
/// 3339, UTC) is `.to_string()` of it, and the page prints it back as this
/// minute, whatever the time zone the tests run in.
fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Ts {
    present::local_ts(year, month, day, hour, minute).expect("a minute on this Mac's clock")
}

#[test]
fn the_token_list_reads_its_scopes_and_says_what_revoking_an_embedded_token_breaks() {
    i18n::install();
    let now = local(2026, 11, 11, 10, 2);
    let reply = json!([
        {
            "name": "hooks",
            "scopes": ["hooks:ingest"],
            "created": local(2026, 9, 20, 10, 0).to_string(),
            "expires": null,
            "embedded_in": ["~/.claude/settings.json"],
            "client": null
        },
        {"name": "script", "scopes": ["observe", "operate"], "embedded_in": []},
        {"name": "inert", "scopes": [], "expires": local(2027, 1, 1, 0, 0).to_string()}
    ]);
    let rows = TokenRow::list(&reply);
    assert_eq!(rows.len(), 3);
    assert_eq!(local_api::token_scopes_line(&rows[0], EN), "hooks:ingest");
    assert_eq!(
        local_api::token_life_line(&rows[0], now, EN),
        "created 2026-09-20, 10:00 · no expiry"
    );
    let embedded = local_api::token_embedded_line(&rows[0], EN).expect("an embedded token");
    assert!(embedded.contains("~/.claude/settings.json") && embedded.contains("Repair"));
    assert_eq!(local_api::token_embedded_line(&rows[1], EN), None);
    assert_eq!(
        local_api::token_scopes_line(&rows[1], EN),
        "observe, operate"
    );
    // A token with no scopes can do nothing, and says so.
    assert_eq!(local_api::token_scopes_line(&rows[2], EN), "no scopes");
    assert_eq!(
        local_api::token_life_line(&rows[2], now, EN),
        "created at an unrecorded time · expires 2027-01-01, 00:00"
    );
    // The scopes a new token may get are the wire's.
    let scopes = local_api::scope_words();
    assert_eq!(scopes.len(), 9);
    assert!(scopes.iter().any(|scope| scope == "observe:raw"));
    // Rotate and revoke name the token and nothing else.
    assert_eq!(
        UiAction::TokenRevoke {
            name: "script".into()
        }
        .request()
        .1,
        json!({"name": "script"})
    );
}

/// A token's times are said as every page says a moment, on this Mac's
/// clock, never as the service's UTC stamp; one that does not read as a
/// stamp is printed as it came.
#[test]
fn a_tokens_times_are_said_on_this_macs_clock() {
    i18n::install();
    let now = local(2026, 11, 11, 10, 2);
    let token = |created: String, expires: Option<String>| {
        TokenRow::list(&json!([
            {"name": "t", "scopes": ["observe"], "created": created, "expires": expires}
        ]))
        .remove(0)
    };
    let today = token(
        local(2026, 11, 11, 9, 12).to_string(),
        Some(local(2026, 11, 11, 16, 40).to_string()),
    );
    assert_eq!(
        local_api::token_life_line(&today, now, EN),
        "created today, 09:12 · expires today, 16:40"
    );
    let yesterday = token(local(2026, 11, 10, 16, 40).to_string(), None);
    assert_eq!(
        local_api::token_life_line(&yesterday, now, EN),
        "created yesterday, 16:40 · no expiry"
    );
    assert_eq!(
        local_api::token_life_line(&yesterday, now, ZH),
        "创建于 昨天 16:40 · 无到期时间"
    );
    assert_eq!(
        local_api::token_life_line(&yesterday, now, Language::Japanese),
        "作成 昨日 16:40 · 有効期限なし"
    );
    let odd = token("last Tuesday".into(), None);
    assert_eq!(
        local_api::token_life_line(&odd, now, EN),
        "created last Tuesday · no expiry"
    );
}

/// **A token created in the window is shown once.** The plaintext exists in
/// one reply and in one [`Shown`], and then it is gone: never in the mirror,
/// never in `HostState`, never in the request that made it, and never in a
/// formatted string, which every log line, toast and panic message is built
/// from.
#[test]
fn a_token_made_in_the_window_is_shown_once_and_is_in_no_mirror_state_or_log_line_after() {
    i18n::install();
    const PLAINTEXT: &str = "owt_live_3f8a91c4d70b25e6";
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: Snapshot {
            rev: 10,
            ..Snapshot::default()
        },
    }));
    let (method, params) = UiAction::TokenCreate {
        name: "script".into(),
        scopes: vec!["observe".into()],
        expires: None,
    }
    .request();
    assert_eq!(method, "token.create");
    assert!(!params.to_string().contains(PLAINTEXT));
    // The reply is the only place it ever is. Read once, into a `Secret`.
    let secret = Secret::from_reply(&json!({"token": PLAINTEXT})).expect("a created token");
    let mut shown = Some(Shown::new("script", "Created", secret));
    assert_eq!(shown.as_ref().map(Shown::reveal), Some(PLAINTEXT));
    let toast = local_api::token_made_toast("Created", "script", EN);
    assert_eq!(
        toast,
        "Created the token “script”. Copy its value now: it is shown once."
    );
    for hay in [
        format!("{shown:?}"),
        toast,
        local_api::token_made_toast("Rotated", "script", ZH),
        format!("{state:?}"),
        serde_json::to_string(state.mirror()).unwrap(),
    ] {
        assert!(
            !hay.contains(PLAINTEXT),
            "the plaintext escaped into: {hay}"
        );
    }
    // Done.
    assert!(local_api::close(&mut shown), "there was one to dismiss");
    assert!(shown.is_none());
    assert!(!local_api::close(&mut shown), "and only one");
    // And afterwards it is in nothing this process still holds: `token.*`
    // emits no event, so nothing can put it in the mirror later either.
    state.handle_event(AppEvent::Api(ApiEvent::Event(Event {
        rev: 11,
        ts: Ts::from_millis(1_789_760_523_120),
        kind: EventKind::UsageUpdated,
        principal: Principal::Uid(501),
        data: json!({"usage": {}}),
    })));
    let mirror = serde_json::to_string(state.mirror()).unwrap();
    for hay in [format!("{shown:?}"), format!("{state:?}"), mirror.clone()] {
        assert!(!hay.contains(PLAINTEXT), "the plaintext survived in: {hay}");
    }
    assert!(!mirror.contains("\"token\""), "{mirror}");
}

#[test]
fn a_read_keeps_its_last_answer_and_names_a_service_too_old_to_have_the_method() {
    let missing = || ClientError::Rpc {
        code: ApiError::METHOD_NOT_FOUND,
        message: "method not found".into(),
        data: Some(json!({"method": "care.checks"})),
    };
    let mut checks: Answer<Checks> = Answer::default();
    assert_eq!(checks, Answer::Unread);
    assert_eq!(checks.got(), None);
    checks.take(Ok(fixture(CARE_CHECKS)), Checks::parse);
    assert_eq!(checks.got().map(|checks| checks.checks.len()), Some(11));
    // A call that failed, or got no answer, leaves the last answer standing.
    checks.take(Err(ClientError::Disconnected), Checks::parse);
    checks.take(
        Err(ClientError::Rpc {
            code: ApiError::INVALID_PARAMS,
            message: "invalid params".into(),
            data: None,
        }),
        Checks::parse,
    );
    assert!(checks.got().is_some());
    assert!(!checks.missing());
    // A service without the method is older than this app, and says so.
    checks.take(Err(missing()), Checks::parse);
    assert!(checks.missing());
    assert_eq!(checks.got(), None);
}

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::service::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 79 rows, copied as written (the question before Stop…
    // among them, once the console's status item that asked it too was
    // gone; the firmware row's line as the design wrote it, now that the
    // service asks for the press), and the page's own two, less "The
    // service is not running": the page says why no service answers in the
    // Deck page's words now. Then the support file's note for content
    // logging on, and the fourteen sentences of the service's firmware
    // status. Then three more: the local API's question while Claude
    // Code's older hooks still use it, what a pressed install waits for,
    // and why an install waits for the deck to link. Then the Footprint's
    // Keychain row: its name and its place. Then the app row's line once
    // the service found a newer app.
    assert_eq!(i18n::service::STRINGS.len(), 101);
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = concat!(
        include_str!("../src/service.rs"),
        include_str!("../src/updates.rs"),
        include_str!("../src/footprint.rs"),
        include_str!("../src/local_api.rs"),
    );
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate. (185: the local API's
    // questions moved into `service::api_question` whole, and gained the
    // sentence about Claude Code's older hooks and its Repair button; 187
    // with the Footprint's Keychain row; 188 with the app row's line for a
    // newer app.)
    assert_eq!(i18n::literals(source).len(), 188);
}

/// A fresh test service's own replies (`deck-gui-core`'s recorded fixtures,
/// `fixtures/daemon/recorded/`): started from a scratch copy of the binary,
/// `--no-device`, its config asking for any free port, Codex switched off,
/// nothing installed and no deck ever linked.
const RECORDED_INFO: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/service.info.json");
const RECORDED_CONFIG: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/config.get.json");
const RECORDED_CHECKS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/care.checks.json");
const RECORDED_SOURCES: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/sources.list.json");
const RECORDED_STATUS: &str = include_str!(
    "../../deck-gui-core/tests/fixtures/daemon/recorded/integrations.status.no_detect.json"
);
const RECORDED_FOOTPRINT: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/care.footprint.json");
const RECORDED_FIRMWARE: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/firmware.status.json");
const RECORDED_TOKENS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/token.list.json");
const RECORDED_HELLO: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/session.hello.json");

#[test]
fn a_fresh_test_services_own_replies_read_as_the_service_page_says_them() {
    i18n::install();
    let info_reply = fixture(RECORDED_INFO);
    let info = Info::parse(&info_reply);
    let checks = Checks::parse(&fixture(RECORDED_CHECKS));
    let sources = Sources::parse(&fixture(RECORDED_SOURCES));
    let snapshot: Snapshot = serde_json::from_value(fixture(RECORDED_HELLO)["snapshot"].clone())
        .expect("the hello's snapshot");
    // "Check my setup", from what the page reads: the installer without
    // its probes, the checks, the sources, `service.info`'s executable and
    // the mirror.
    let (reports, _) = Report::list(&fixture(RECORDED_STATUS));
    let claude = reports
        .into_iter()
        .find(|report| report.target == "claude")
        .map(|report| report.status);
    assert_eq!(claude, Some(Status::Absent));
    let runs_from = executable(&info_reply);
    assert!(runs_from
        .as_deref()
        .is_some_and(|path| path.ends_with("bin/deckd-s6")));
    let bundled = Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    let setup = Setup {
        linked: false,
        this_copy: this_copy(runs_from.as_deref(), Some(bundled)),
        claude,
        stale_helpers: stale_helpers(Some(&sources), 0),
        codex: codex_check(Some(&checks), Some(&sources)),
        legacy_agent: legacy_agent(Some(&checks)),
        bluetooth: snapshot.voice.permissions.bluetooth,
    };
    assert_eq!(
        setup.codex,
        CodexCheck::Off,
        "switched off: its row is left out"
    );
    let rows = check_rows(&setup, EN);
    let said: Vec<(Verdict, &str, Option<&str>)> = rows
        .iter()
        .map(|row| (row.verdict, row.title.as_str(), row.sub.as_deref()))
        .collect();
    assert_eq!(
        said,
        [
            (Verdict::Problem, "The deck is not linked", None),
            (
                Verdict::Problem,
                "The service runs from another copy of the app",
                Some("Quit the other copy, then restart the service.")
            ),
            (Verdict::Problem, "Claude Code plugin is not set up", None),
            // A service launchd did not start leaves the legacy agent
            // unjudged rather than reporting on whichever copy launchd runs.
            (Verdict::NotChecked, "No leftover launch agent", None),
            (Verdict::NotChecked, "Bluetooth is allowed", None),
        ]
    );
    // The local API: on, at the port the service holds — the recorded
    // stage-6 service bound its door unless told not to, where a service
    // of this release leaves it off (`the_local_api_row_says_off_or_on_and_its_port`).
    // `port = 0` asks for any free port and names none, so before
    // `service.info` answers the line says no port rather than port 0.
    let api = Api::parse(&fixture(RECORDED_CONFIG));
    assert_eq!((api.tcp, api.config.port), (Some(true), None));
    assert_eq!(info.port, Some(52_098));
    assert_eq!(
        api_line(&api, info.port, EN),
        "On. Port 52098. Local tools can use it with a token."
    );
    assert_eq!(
        api_line(&api, None, EN),
        "On. Port \u{2014}. Local tools can use it with a token."
    );
    assert_eq!(
        local_api::port_line(info.port, &api.config, EN),
        "http://127.0.0.1:52098 \u{b7} listening"
    );
    assert_eq!(
        local_api::port_line(None, &api.config, EN),
        "no TCP port: the daemon is on the socket only"
    );
    assert!(local_api::curl_example(info.port.or(api.config.port))
        .contains("http://127.0.0.1:52098/v1/rpc"));
    assert!(local_api::origins_line(&api.config.allowed_origins, EN).contains("403"));
    assert!(TokenRow::list(&fixture(RECORDED_TOKENS)).is_empty());
    // The updates: this app's version, a dash for a deck that never said
    // which firmware it runs, and nothing found to review.
    assert_eq!(
        update_titles(deck_version(&snapshot.device).as_deref(), EN),
        (
            format!("OpenWalkieTalkie {}", env!("CARGO_PKG_VERSION")),
            "Deck firmware \u{2014}".to_string()
        )
    );
    let firmware = fixture(RECORDED_FIRMWARE);
    assert_eq!(firmware_step(&firmware), FirmwareStep::Check);
    assert_eq!(available(&firmware), None);
    assert!(!updates::busy(&firmware) && !updates::interrupted(&firmware));
    assert!(!can_install(&firmware, "hw-b"));
    assert_eq!(install_board(&firmware, None), "");
    assert!(
        !updates::needs_restart(&info_reply, &bundled.to_string_lossy(), "0.13.0"),
        "the service of another copy is never restarted"
    );
    // Footprint: what the service found on this Mac, and the Service
    // page's line about it.
    let footprint = Footprint::parse(&fixture(RECORDED_FOOTPRINT));
    let items = footprint::items(&footprint, EN);
    let listed: Vec<(&str, &str, Option<&str>, Action)> = items
        .iter()
        .map(|item| (item.id, item.name, item.size.as_deref(), item.action))
        .collect();
    assert_eq!(
        listed,
        [
            ("service", "Background service", None, Action::WithUninstall),
            (
                "data",
                "Settings and backups",
                Some("5 KB"),
                Action::WithUninstall
            ),
        ]
    );
    assert_eq!(
        items[0].place,
        Place::Words("deckd, under Login Items in System Settings")
    );
    assert_eq!(items[1].place, Place::Path("d".into()));
    assert_eq!(footprint::total(&footprint), (2, 4_534));
    assert_eq!(
        footprint::summary(&footprint, EN),
        "2 items, 5 KB. Most can be removed one at a time."
    );
}
