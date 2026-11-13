//! The Integrations page's rules, read without a window: what each row of
//! the two tables says for every installer status and source state, the
//! counts and their units, the ages and the hand-offs, the Claude Channels
//! row with and without old helpers, the connected apps, which switches are
//! drawn and what each writes, the pause (and the older `hooks.enabled`),
//! the test question's line, when a run is done and what it left to the
//! owner, the plan dialog's words, and that every word the page says has a
//! row in all four languages.
use deck_client::ClientError;
use deck_gui_core::admin::{method_missing, Plan, Report, Status, Verb};
use deck_gui_core::care::{self, Handoff, QuestionTest, SourceState, Sources, SourcesConfig};
use deck_gui_core::care::{TestState, TestStatus, Unit};
use deck_gui_core::i18n::Language;
use deck_gui_core::UiAction;
use deck_model::{Tone, Ts};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::integrations::{
    age_words, app_icon, apps, apps_paused_after, change_word, channels_stale, count_words,
    handoff_words, others_line, receipt_line, toggle_action, tools, total_words, Control, Facts,
    Line, NextStep, State, Toggle,
};
use openwalkietalkie_app::present::{
    self, finish_command, finished, test_line, Leftover, TestMark,
};
use openwalkietalkie_app::ui::ic;
use serde_json::{json, Value};
use std::path::Path;

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;

/// 10:02:00 UTC on the boards' day: the moment the fixtures' times were
/// written against.
const NOW: Ts = Ts::from_millis(1_794_391_320_000);

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).expect("a fixture that parses")
}

/// The installer's answer: the plugin in `claude` status, Codex Desktop not
/// set up.
fn installer(claude: &str) -> Vec<Report> {
    Report::list(&json!({"targets": [
        {"target": "claude", "status": claude},
        {"target": "codex-desktop", "status": "absent"}
    ]}))
    .0
}

/// The Integrations board's answers: the service's fixtures, the plugin
/// installed.
fn board() -> Facts {
    Facts {
        sources: Some(Sources::parse(&fixture(include_str!(
            "../../deck-gui-core/tests/fixtures/daemon/sources_list.json"
        )))),
        config: Some(SourcesConfig::parse(&fixture(include_str!(
            "../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json"
        )))),
        reports: installer("installed"),
        channels_stale: 0,
    }
}

fn state(word: &str, tone: Tone) -> Option<State> {
    Some(State {
        word: word.into(),
        tone,
    })
}

fn source<'a>(facts: &'a mut Facts, id: &str) -> &'a mut care::Source {
    facts
        .sources
        .as_mut()
        .and_then(|sources| sources.sources.iter_mut().find(|source| source.id == id))
        .expect("the source")
}

fn config(facts: &mut Facts) -> &mut SourcesConfig {
    facts.config.as_mut().expect("the settings")
}

fn row<'a>(lines: &'a [Line], id: &str) -> &'a Line {
    lines.iter().find(|line| line.id == id).expect(id)
}

#[test]
fn the_board_reads_as_the_board_draws_it() {
    let facts = board();
    let rows = tools(&facts, NOW, EN);
    let ids: Vec<&str> = rows.iter().map(|line| line.id.as_str()).collect();
    assert_eq!(ids, ["claude", "claude_channels", "codex", "codex-desktop"]);

    let claude = &rows[0];
    assert_eq!(
        (claude.name.as_str(), claude.icon),
        ("Claude Code", ic::CLAUDE)
    );
    assert_eq!(claude.sub, "Plugin: 13 hooks and\nthe Channels helper.");
    // On the deck, all three of them: no hint of more.
    assert_eq!(claude.count.as_deref(), Some("3 sessions"));
    assert_eq!(claude.total, None);
    assert_eq!(claude.age.as_deref(), Some("4 s ago"));
    // The fixture's word; the service sends "terminal" today (A10).
    assert_eq!(claude.handoff.as_deref(), Some("Raises its app"));
    assert_eq!(claude.state, state("Connected", Tone::Green));
    assert_eq!(claude.control, Control::Switch(Toggle::Claude, true));
    assert!(!claude.wide && !claude.connect);

    let channels = &rows[1];
    assert_eq!((channels.icon, channels.wide), (ic::MICROPHONE, true));
    assert_eq!(
        channels.sub,
        "Voice orders arrive as your own message, not as another session's.\n2 sessions \
         started before the last update still use the old helper."
    );
    assert_eq!(channels.state, state("Restart 2 sessions", Tone::Amber));
    assert_eq!(
        channels.control,
        Control::Switch(Toggle::ClaudeChannels, true)
    );
    assert!(channels.connect, "How to Connect… while the bridge is on");
    assert_eq!(
        (&channels.count, &channels.age, &channels.handoff),
        (&None, &None, &None),
        "a wide row has no count, time or hand-off"
    );

    let codex = &rows[2];
    assert_eq!((codex.name.as_str(), codex.icon), ("Codex CLI", ic::OPENAI));
    assert_eq!(codex.sub, "Through Codex itself.\nNothing is installed.");
    assert_eq!(codex.count.as_deref(), Some("2 threads"));
    assert_eq!(codex.age.as_deref(), Some("1 min ago"));
    assert_eq!(codex.handoff.as_deref(), Some("Not yet"));
    assert_eq!(codex.state, state("Connected", Tone::Green));
    assert_eq!(codex.control, Control::Switch(Toggle::Codex, true));

    let desktop = &rows[3];
    assert_eq!((desktop.icon, desktop.wide), (ic::MONITOR, true));
    assert_eq!(
        desktop.sub,
        "Open \u{201c}Codex with OpenWalkieTalkie\u{201d} instead of Codex.\nIt is in the \
         Applications folder of your home folder."
    );
    assert_eq!(desktop.state, state("Not set up", Tone::Gray));
    assert_eq!(desktop.control, Control::SetUp("codex-desktop"));

    let apps = apps(&facts, NOW, EN);
    let ids: Vec<&str> = apps.iter().map(|line| line.id.as_str()).collect();
    assert_eq!(ids, ["app:Conol", "app:Omarchy remote"]);
    let conol = &apps[0];
    assert_eq!((conol.name.as_str(), conol.icon), ("Conol", ic::CONOL));
    assert_eq!(conol.sub, "Sessions. Voice for its\nchats: Conol's cloud.");
    // The chats in the deck's places: all seven it has.
    assert_eq!(conol.count.as_deref(), Some("7 chats"));
    assert_eq!(conol.total, None);
    assert_eq!(conol.age.as_deref(), Some("12 s ago"));
    assert_eq!(conol.handoff.as_deref(), Some("By itself"));
    assert_eq!(conol.state, state("Connected", Tone::Green));
    assert_eq!(
        conol.control,
        Control::Switch(Toggle::App("Conol".into()), true)
    );
    let omarchy = &apps[1];
    assert_eq!(omarchy.icon, ic::TERMINAL);
    assert_eq!(omarchy.sub, "Sessions, the Omarchy\nface and its keys.");
    assert_eq!(omarchy.count.as_deref(), Some("2 sessions"));
    assert_eq!(omarchy.age.as_deref(), Some("1 min ago"));
    assert_eq!(
        omarchy.control,
        Control::Switch(Toggle::App("Omarchy remote".into()), true)
    );

    // Nothing paused, and the page's switch and the menu's item can say so.
    assert_eq!(facts.paused(), Some(false));
    assert_eq!(facts.stale_helpers(), 2);
}

#[test]
fn the_installer_status_decides_what_a_plugin_row_says_and_offers() {
    let mut words: Vec<Status> = Status::KNOWN.to_vec();
    words.push(Status::Unknown("teleported".into()));
    for status in words {
        let mut facts = board();
        facts.reports = installer(status.as_str());
        let claude = tools(&facts, NOW, EN).remove(0);
        match &status {
            Status::Installed => {
                assert_eq!(claude.state, state("Connected", Tone::Green));
                assert_eq!(claude.control, Control::Switch(Toggle::Claude, true));
            }
            Status::Absent => {
                assert_eq!(claude.state, state("Not set up", Tone::Gray));
                assert_eq!(claude.control, Control::SetUp("claude"));
            }
            // Every other word is something on disk that is wrong: Repair,
            // with the word explained where the pointer rests.
            trouble => {
                assert_eq!(
                    claude.state,
                    state("Needs repair", Tone::Amber),
                    "{trouble:?}"
                );
                assert_eq!(
                    claude.control,
                    Control::Repair("claude", present::status_help(trouble).to_string()),
                    "{trouble:?}"
                );
            }
        }
    }
    // The same rule for Codex Desktop, which is installed or not and has
    // no switch.
    let mut facts = board();
    facts.reports = Report::list(&json!({"targets": [
        {"target": "codex-desktop", "status": "installed"}
    ]}))
    .0;
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[3].state, state("Installed", Tone::Green));
    assert_eq!(rows[3].control, Control::None);
    facts.reports = Report::list(&json!({"targets": [
        {"target": "codex-desktop", "status": "path_drift"}
    ]}))
    .0;
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[3].state, state("Needs repair", Tone::Amber));
    assert!(matches!(
        rows[3].control,
        Control::Repair("codex-desktop", _)
    ));
    // Before the installer has answered, nothing is claimed: the plugin row
    // keeps its switch and says no state.
    assert_eq!(rows[0].state, None);
    assert_eq!(rows[0].control, Control::Switch(Toggle::Claude, true));
    facts.reports.clear();
    assert_eq!(tools(&facts, NOW, EN)[3].state, None);
}

#[test]
fn a_sources_own_state_is_connected_waiting_off_or_its_own_word() {
    for (word, want) in [
        ("connected", state("Connected", Tone::Green)),
        ("waiting", state("Codex is not running", Tone::Gray)),
        ("off", state("Off", Tone::Gray)),
        ("beaming", state("beaming", Tone::Gray)),
    ] {
        let mut facts = board();
        source(&mut facts, "codex").state = Some(SourceState::parse(word));
        assert_eq!(tools(&facts, NOW, EN)[2].state, want, "{word}");
    }
    // The Codex switch off: the row is off, not paused, and its switch says
    // so.
    let mut facts = board();
    config(&mut facts).codex_daemon = Some(false);
    let codex = source(&mut facts, "codex");
    codex.enabled = false;
    codex.state = Some(SourceState::Off);
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[2].state, state("Off", Tone::Gray));
    assert_eq!(rows[2].control, Control::Switch(Toggle::Codex, false));
    // An app that has gone away is waiting for itself to come back.
    let mut facts = board();
    source(&mut facts, "app:Conol").state = Some(SourceState::Waiting);
    assert_eq!(apps(&facts, NOW, EN)[0].state, state("Waiting", Tone::Gray));
    // Claude Code switched off on its own.
    let mut facts = board();
    config(&mut facts).claude = Some(false);
    source(&mut facts, "claude").enabled = false;
    let claude = tools(&facts, NOW, EN).remove(0);
    assert_eq!(claude.state, state("Off", Tone::Gray));
    assert_eq!(claude.control, Control::Switch(Toggle::Claude, false));
    i18n::install();
    assert_eq!(
        tools(&facts, NOW, ZH).remove(0).state,
        state("关闭", Tone::Gray)
    );
}

#[test]
fn off_is_a_sources_own_switch_and_paused_is_only_ever_the_pause_of_everything() {
    // Everything paused, and Codex and Conol switched off on their own as
    // well: those two say Off (their own switch, which stays off when the
    // pause ends), the rest Paused.
    let mut facts = board();
    config(&mut facts).paused = Some(true);
    config(&mut facts).codex_daemon = Some(false);
    config(&mut facts).apps_paused = vec!["Conol".into()];
    let codex = source(&mut facts, "codex");
    codex.enabled = false;
    codex.state = Some(SourceState::Off);
    source(&mut facts, "app:Conol").enabled = false;
    let rows = tools(&facts, NOW, EN);
    assert_eq!(row(&rows, "claude").state, state("Paused", Tone::Gray));
    assert_eq!(row(&rows, "codex").state, state("Off", Tone::Gray));
    let apps = apps(&facts, NOW, EN);
    assert_eq!(row(&apps, "app:Conol").state, state("Off", Tone::Gray));
    assert_eq!(
        row(&apps, "app:Omarchy remote").state,
        state("Paused", Tone::Gray)
    );
    // The bridge a new install leaves off (decision 13) is off too, before
    // anything else about it is said.
    let mut facts = board();
    config(&mut facts).claude_channels = Some(false);
    let channels = source(&mut facts, "claude_channels");
    channels.enabled = false;
    channels.state = Some(SourceState::Off);
    assert_eq!(
        row(&tools(&facts, NOW, EN), "claude_channels").state,
        state("Off", Tone::Gray)
    );
}

#[test]
fn one_switch_pauses_every_source_and_the_older_hooks_switch_pauses_claude_code() {
    let mut facts = board();
    config(&mut facts).paused = Some(true);
    assert_eq!(facts.paused(), Some(true));
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[0].state, state("Paused", Tone::Gray));
    // Sessions on an old helper need a restart, paused or not.
    assert_eq!(rows[1].state, state("Restart 2 sessions", Tone::Amber));
    assert_eq!(rows[2].state, state("Paused", Tone::Gray));
    // The installer's word stands: there is nothing to pause.
    assert_eq!(rows[3].state, state("Not set up", Tone::Gray));
    for app in apps(&facts, NOW, EN) {
        assert_eq!(app.state, state("Paused", Tone::Gray), "{}", app.name);
        // Each source's own switch is still its own.
        assert!(matches!(app.control, Control::Switch(_, true)));
    }
    // A pause made with the older switch before the update (O8): the page
    // and the menu show it, and it stops Claude Code's hooks alone.
    let mut facts = board();
    config(&mut facts).hooks_enabled = Some(false);
    assert_eq!(facts.paused(), Some(true));
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[0].state, state("Paused", Tone::Gray));
    assert_eq!(rows[2].state, state("Connected", Tone::Green));
    // Pausing writes the one switch; resuming turns the older one back on
    // in the same patch, so a pause made either way ends.
    assert_eq!(
        care::pause_action(true).request(),
        ("config.set", json!({"patch": {"sources.paused": true}}))
    );
    assert_eq!(
        care::pause_action(false).request(),
        (
            "config.set",
            json!({"patch": {"sources.paused": false, "hooks.enabled": true}})
        )
    );
    // Nothing read, nothing shown: the switch is not drawn until a service
    // with it has answered.
    assert_eq!(Facts::default().paused(), None);
}

#[test]
fn counts_are_said_in_each_sources_own_unit() {
    i18n::install();
    for (count, unit, words) in [
        (1, Unit::Sessions, "1 session"),
        (3, Unit::Sessions, "3 sessions"),
        (0, Unit::Sessions, "0 sessions"),
        (1, Unit::Threads, "1 thread"),
        (2, Unit::Threads, "2 threads"),
        (1, Unit::Chats, "1 chat"),
        (7, Unit::Chats, "7 chats"),
        // A unit this build has not heard of counts sessions.
        (2, Unit::Unknown("conversations".into()), "2 sessions"),
    ] {
        assert_eq!(count_words(count, &unit, EN), words);
    }
    assert_eq!(count_words(3, &Unit::Sessions, ZH), "3 个会话");
    assert_eq!(count_words(1, &Unit::Chats, ZH), "1 个聊天");
}

#[test]
fn the_count_is_what_holds_a_place_on_the_deck_with_every_session_as_its_hint() {
    i18n::install();
    // Five of Conol's seven chats on the deck: five under "On the deck",
    // seven on hover.
    let mut facts = board();
    let conol = source(&mut facts, "app:Conol");
    conol.on_deck = 5;
    conol.sessions = 7;
    let conol = apps(&facts, NOW, EN).remove(0);
    assert_eq!(conol.count.as_deref(), Some("5 chats"));
    assert_eq!(conol.total.as_deref(), Some("7 chats in all"));
    assert_eq!(
        apps(&facts, NOW, ZH)[0].total.as_deref(),
        Some("共 7 个聊天")
    );
    // Paused, every session loses its place and none is on the deck; the
    // count says so, and its hint how many are waiting to come back.
    let mut facts = board();
    config(&mut facts).paused = Some(true);
    for source in facts.sources.as_mut().expect("the sources").sources.iter_mut() {
        source.on_deck = 0;
    }
    let rows = tools(&facts, NOW, EN);
    let claude = row(&rows, "claude");
    assert_eq!(claude.count.as_deref(), Some("0 sessions"));
    assert_eq!(claude.total.as_deref(), Some("3 sessions in all"));
    assert_eq!(claude.state, state("Paused", Tone::Gray));
    let codex = row(&rows, "codex");
    assert_eq!(
        (codex.count.as_deref(), codex.total.as_deref()),
        (Some("0 threads"), Some("2 threads in all"))
    );
    // In each unit, and one of it.
    assert_eq!(total_words(1, &Unit::Sessions, EN), "1 session in all");
    assert_eq!(total_words(4, &Unit::Threads, EN), "4 threads in all");
    assert_eq!(
        total_words(2, &Unit::Chats, Language::Japanese),
        "合計 2 チャット"
    );
}

#[test]
fn a_connected_apps_chats_are_heard_on_this_mac_while_the_voice_pages_switch_is_off() {
    i18n::install();
    let mut facts = board();
    assert_eq!(config(&mut facts).stt_companion, Some(true));
    assert!(facts.companion());
    assert_eq!(
        apps(&facts, NOW, EN)[0].sub,
        "Sessions. Voice for its\nchats: Conol's cloud."
    );
    // The Voice page's "Conol's cloud, for Conol chats only" switched off:
    // those presses are recognised here, and the row says it.
    config(&mut facts).stt_companion = Some(false);
    assert!(!facts.companion());
    let rows = apps(&facts, NOW, EN);
    assert_eq!(row(&rows, "app:Conol").sub, "Sessions. Its chats are\nheard on this Mac.");
    // An app that does not transcribe its chats says what it does either way.
    assert_eq!(
        row(&rows, "app:Omarchy remote").sub,
        "Sessions, the Omarchy\nface and its keys."
    );
    assert_eq!(
        apps(&facts, NOW, ZH)[0].sub,
        "会话。聊天语音\n在此 Mac 上识别。"
    );
    // A service with no such switch: the app's cloud hears them, as there.
    config(&mut facts).stt_companion = None;
    assert!(facts.companion());
    assert_eq!(
        apps(&facts, NOW, EN)[0].sub,
        "Sessions. Voice for its\nchats: Conol's cloud."
    );
}

#[test]
fn leftover_codex_hooks_are_said_on_the_codex_row_with_where_they_are_removed() {
    i18n::install();
    let with_codex = |status: &str| {
        let mut facts = board();
        facts.reports = Report::list(&json!({"targets": [
            {"target": "claude", "status": "installed"},
            {"target": "codex", "status": status},
            {"target": "codex-desktop", "status": "absent"}
        ]}))
        .0;
        facts
    };
    // As they were written: they still put Codex sessions on the deck.
    let reaching = "Through Codex itself.\nLeftover hooks also reach the deck: remove them in \
                    Footprint.";
    let facts = with_codex("installed");
    assert_eq!(row(&tools(&facts, NOW, EN), "codex").sub, reaching);
    // Whatever the Codex switch says, which is the connection through Codex
    // itself and not those hooks: the row is off, and still says they reach
    // the deck.
    let mut off = with_codex("installed");
    config(&mut off).codex_daemon = Some(false);
    let codex = source(&mut off, "codex");
    codex.enabled = false;
    codex.state = Some(SourceState::Off);
    let line = tools(&off, NOW, EN).remove(2);
    assert_eq!(line.state, state("Off", Tone::Gray));
    assert_eq!(line.sub, reaching);
    // On this Mac in another state: there, without a claim that they work.
    for status in ["outdated", "modified", "untrusted", "paused", "teleported"] {
        assert_eq!(
            row(&tools(&with_codex(status), NOW, EN), "codex").sub,
            "Through Codex itself.\nLeftover hooks are still on this Mac: remove them in \
             Footprint.",
            "{status}"
        );
    }
    // None: nothing is installed, as the board says.
    assert_eq!(
        row(&tools(&with_codex("absent"), NOW, EN), "codex").sub,
        "Through Codex itself.\nNothing is installed."
    );
    // Footprint by the name its page carries in each language.
    assert!(row(&tools(&facts, NOW, ZH), "codex").sub.contains("“安装内容”"));
    assert!(row(&tools(&facts, NOW, Language::TraditionalChinese), "codex")
        .sub
        .contains("「安裝內容」"));
    assert!(row(&tools(&facts, NOW, Language::Japanese), "codex")
        .sub
        .contains("「インストール内容」"));
}

#[test]
fn sessions_no_row_counts_are_said_under_the_agent_tools_while_there_are_any() {
    i18n::install();
    let with_other = |other: u32| {
        let mut facts = board();
        facts.sources.as_mut().expect("the sources").other = other;
        facts
    };
    assert_eq!(others_line(&with_other(0), EN), None);
    assert_eq!(
        others_line(&with_other(1), EN).as_deref(),
        Some("1 other session, from MCP or Gemini.")
    );
    assert_eq!(
        others_line(&with_other(3), EN).as_deref(),
        Some("3 other sessions, from MCP or Gemini.")
    );
    assert_eq!(
        others_line(&with_other(3), ZH).as_deref(),
        Some("另有 3 个会话来自 MCP 或 Gemini。")
    );
    // Nothing read, nothing said.
    assert_eq!(others_line(&Facts::default(), EN), None);
}

#[test]
fn ages_are_whole_and_never_is_a_dash_and_each_hand_off_has_its_words() {
    i18n::install();
    let before = |seconds: u64| Some(Ts::from_millis(NOW.millis() - seconds * 1_000));
    assert_eq!(age_words(before(0), NOW, EN), "just now");
    assert_eq!(age_words(before(4), NOW, EN), "4 s ago");
    assert_eq!(age_words(before(60), NOW, EN), "1 min ago");
    assert_eq!(age_words(before(2 * 3_600), NOW, EN), "2 h ago");
    assert_eq!(age_words(None, NOW, EN), "\u{2014}");
    // The service's clock a moment ahead of this one is not a negative age.
    assert_eq!(
        age_words(Some(Ts::from_millis(NOW.millis() + 900)), NOW, EN),
        "just now"
    );
    assert_eq!(age_words(before(12), NOW, ZH), "12 秒前");
    // The service raises a session's own app; a 0.13 service focused a
    // terminal tab in Ghostty alone, and its words say so.
    for (handoff, words) in [
        (Handoff::Terminal, "Focuses its Ghostty tab"),
        (Handoff::App, "Raises its app"),
        (Handoff::NotYet, "Not yet"),
        (Handoff::Itself, "By itself"),
        (Handoff::Unknown("beam".into()), "beam"),
    ] {
        assert_eq!(handoff_words(&handoff, EN), words);
    }
    assert_eq!(
        handoff_words(&Handoff::Terminal, ZH),
        "切换到其\nGhostty 标签页"
    );
    assert_eq!(
        handoff_words(&Handoff::Terminal, Language::Japanese),
        "Ghostty の\nタブを前面へ"
    );
    // The shipped words for the app hand-off, in each language.
    for (language, words) in [
        (ZH, "切换到其应用"),
        (Language::TraditionalChinese, "切至其應用程式"),
        (Language::Japanese, "アプリを前面へ"),
    ] {
        assert_eq!(handoff_words(&Handoff::App, language), words, "{language:?}");
    }
    // What a source never said stays empty, and a source not yet read has
    // no figures at all.
    let mut facts = board();
    source(&mut facts, "codex").last_event_at = None;
    assert_eq!(tools(&facts, NOW, EN)[2].age.as_deref(), Some("\u{2014}"));
    facts.sources = None;
    let rows = tools(&facts, NOW, EN);
    assert_eq!((&rows[0].count, &rows[0].age), (&None, &None));
    assert_eq!(
        rows[0].sub, "",
        "the hooks are counted by the service, never guessed"
    );
}

#[test]
fn the_channels_row_names_the_old_helpers_and_says_when_no_session_is_connected() {
    i18n::install();
    let first = "Voice orders arrive as your own message, not as another session's.";
    for (stale, second, want) in [
        (0, None, state("Connected", Tone::Green)),
        (
            1,
            Some("1 session started before the last update still uses the old helper."),
            state("Restart 1 session", Tone::Amber),
        ),
        (
            2,
            Some("2 sessions started before the last update still use the old helper."),
            state("Restart 2 sessions", Tone::Amber),
        ),
    ] {
        let mut facts = board();
        source(&mut facts, "claude_channels").stale = Some(stale);
        let channels = tools(&facts, NOW, EN).remove(1);
        let sub = match second {
            Some(second) => format!("{first}\n{second}"),
            None => first.to_string(),
        };
        assert_eq!(channels.sub, sub, "{stale}");
        assert_eq!(channels.state, want, "{stale}");
        assert_eq!(facts.stale_helpers(), stale);
    }
    // On, and no session has connected through it (O7): said neutrally, and
    // the way to connect one is offered.
    let mut facts = board();
    let channels = source(&mut facts, "claude_channels");
    channels.stale = Some(0);
    channels.state = Some(SourceState::Waiting);
    let row = tools(&facts, NOW, EN).remove(1);
    assert_eq!(row.state, state("No session connected", Tone::Gray));
    assert!(row.connect);
    // Switched off: off, the plugin without its helper, nothing to
    // connect.
    config(&mut facts).claude_channels = Some(false);
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[1].state, state("Off", Tone::Gray));
    assert_eq!(
        rows[1].control,
        Control::Switch(Toggle::ClaudeChannels, false)
    );
    assert!(!rows[1].connect);
    assert_eq!(rows[0].sub, "Plugin: 13 hooks.");
    // A service from before `sources.list` counts the old helpers in its
    // installer answer only.
    let older = Facts {
        channels_stale: 3,
        ..Facts::default()
    };
    assert_eq!(older.stale_helpers(), 3);
    assert_eq!(
        tools(&older, NOW, ZH)[1].state,
        state("重启 3 个会话", Tone::Amber)
    );
}

#[test]
fn the_apps_are_the_ones_that_connected_each_said_by_what_it_does() {
    let mut facts = board();
    facts.sources = Some(Sources::parse(&json!({
        "paused": false,
        "sources": [
            {"id": "app:Acme", "kind": "app", "name": "Acme", "enabled": true,
             "state": "connected", "on_deck": 1, "sessions": 1, "unit": "sessions",
             "last_event_at": null, "handoff": "self", "does": ["teleport"]},
            {"id": "app:Pager", "kind": "app", "name": "Pager", "state": "connected",
             "sessions": 4, "unit": "chats", "does": ["sessions", "voice"]}
        ],
        "other": 3
    })));
    let rows = apps(&facts, NOW, EN);
    assert_eq!(rows.len(), 2);
    // A word of `does` this build does not know adds nothing.
    assert_eq!(rows[0].sub, "Sessions.");
    assert_eq!(rows[0].icon, ic::PLUG);
    assert_eq!(rows[0].count.as_deref(), Some("1 session"));
    assert_eq!(rows[0].total, None);
    assert_eq!(rows[0].age.as_deref(), Some("\u{2014}"));
    // The voice line names the app whose cloud hears it.
    assert_eq!(
        rows[1].sub,
        "Sessions. Voice for its\nchats: Pager's cloud."
    );
    // A row that does not say what is on the deck claims nothing there,
    // and its chats are still counted on hover.
    assert_eq!(rows[1].count.as_deref(), Some("0 chats"));
    assert_eq!(rows[1].total.as_deref(), Some("4 chats in all"));
    // None connected: the card is its closing line alone.
    facts.sources = Some(Sources::parse(
        &json!({"paused": false, "sources": [], "other": 2}),
    ));
    assert!(apps(&facts, NOW, EN).is_empty());
    // Nothing read yet: no app is claimed.
    facts.sources = None;
    assert!(apps(&facts, NOW, EN).is_empty());
    assert_eq!(app_icon("conol"), ic::CONOL);
    assert_eq!(app_icon("Omarchy remote"), ic::TERMINAL);
    assert_eq!(app_icon("Acme"), ic::PLUG);
}

#[test]
fn a_switch_is_drawn_only_where_the_service_manages_it_and_writes_its_own_key() {
    let facts = board();
    let settings = facts.config.clone().expect("the settings");
    for (toggle, on, params) in [
        (
            Toggle::Claude,
            false,
            json!({"patch": {"sources.claude": false}}),
        ),
        (
            Toggle::ClaudeChannels,
            true,
            json!({"patch": {"sources.claude_channels": true}}),
        ),
        (
            Toggle::Codex,
            false,
            json!({"patch": {"codex.daemon": false}}),
        ),
        (
            Toggle::App("Conol".into()),
            false,
            json!({"patch": {"sources.apps_paused": ["Conol"]}}),
        ),
    ] {
        assert_eq!(
            toggle_action(&toggle, on, &settings).request(),
            ("config.set", params),
            "{toggle:?}"
        );
    }
    // An app's switch writes the whole list, every other name kept.
    let paused = vec!["Omarchy remote".to_string(), "Conol".to_string()];
    assert_eq!(
        apps_paused_after(&paused, "Conol", true),
        ["Omarchy remote"]
    );
    assert_eq!(
        apps_paused_after(&paused, "Pager", false),
        ["Omarchy remote", "Conol", "Pager"]
    );
    assert_eq!(
        apps_paused_after(&paused, "Conol", false),
        ["Omarchy remote", "Conol"]
    );
    let mut paused_config = settings.clone();
    paused_config.apps_paused = vec!["Omarchy remote".into()];
    assert_eq!(
        toggle_action(&Toggle::App("Conol".into()), false, &paused_config).request(),
        (
            "config.set",
            json!({"patch": {"sources.apps_paused": ["Omarchy remote", "Conol"]}})
        )
    );
    // An app in the paused list is off, as its switch shows.
    let mut facts = board();
    config(&mut facts).apps_paused = vec!["Conol".into()];
    source(&mut facts, "app:Conol").enabled = false;
    let conol = apps(&facts, NOW, EN).remove(0);
    assert_eq!(
        conol.control,
        Control::Switch(Toggle::App("Conol".into()), false)
    );
    assert_eq!(conol.state, state("Off", Tone::Gray));
    // A service with no `[sources]` manages none of these switches: none is
    // drawn, and the pause is not offered.
    let mut facts = board();
    facts.config = Some(SourcesConfig::parse(&json!({
        "codex": {"daemon": true}, "hooks": {"enabled": true}
    })));
    assert_eq!(facts.paused(), None);
    for line in tools(&facts, NOW, EN).iter().chain(&apps(&facts, NOW, EN)) {
        assert!(
            !matches!(line.control, Control::Switch(..)),
            "{} has a switch",
            line.id
        );
    }
    // One that has `[sources]` and no Claude Channels switch yet draws
    // every switch but that one.
    let mut facts = board();
    config(&mut facts).claude_channels = None;
    let rows = tools(&facts, NOW, EN);
    assert_eq!(rows[1].control, Control::None);
    assert_eq!(rows[0].control, Control::Switch(Toggle::Claude, true));
    assert!(rows[1].connect, "a bridge with no switch is on");
    // Settings not read at all: no switch anywhere.
    facts.config = None;
    assert!(tools(&facts, NOW, EN)
        .iter()
        .all(|line| !matches!(line.control, Control::Switch(..))));
}

#[test]
fn the_test_line_says_where_the_last_question_got_to() {
    i18n::install();
    let question = |state: TestState, after_ms: Option<u64>| QuestionTest {
        state,
        at: Some(NOW),
        after_ms,
    };
    assert_eq!(test_line(None, EN), None, "nothing before the first test");
    for (test, words, mark) in [
        (
            question(TestState::Sent, None),
            "A test question is on the deck. Answer it there.",
            TestMark::Waiting,
        ),
        (
            question(TestState::Answered, Some(3_120)),
            "The last test question arrived on the deck and was answered there after 3 s.",
            TestMark::Done,
        ),
        (
            question(TestState::Answered, Some(400)),
            "The last test question arrived on the deck and was answered there after 1 s.",
            TestMark::Done,
        ),
        (
            question(TestState::Expired, None),
            "The last test question was not answered on the deck.",
            TestMark::Missed,
        ),
        (
            question(TestState::NoDeck, None),
            "The deck is not linked, so no test question was sent.",
            TestMark::Note,
        ),
    ] {
        assert_eq!(test_line(Some(&test), EN), Some((words.to_string(), mark)));
    }
    // Words that are not a question's, and an answer with no time, say
    // nothing rather than something made up.
    assert_eq!(test_line(Some(&question(TestState::Heard, None)), EN), None);
    assert_eq!(
        test_line(Some(&question(TestState::Unknown("lost".into()), None)), EN),
        None
    );
    assert_eq!(
        test_line(Some(&question(TestState::Answered, None)), EN),
        None
    );
    // The board's: the fixture's question, answered after 3.12 s.
    let status = TestStatus::parse(&fixture(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/care_test_status.json"
    )));
    let (words, mark) = test_line(status.question.as_ref(), EN).expect("a line");
    assert_eq!(
        words,
        "The last test question arrived on the deck and was answered there after 3 s."
    );
    assert_eq!(mark, TestMark::Done);
    let (words, _) = test_line(status.question.as_ref(), ZH).expect("a line");
    assert_eq!(words, "上次的测试提问已送达设备，3 秒后在设备上得到回答。");
}

/// A repair whose service could not find `claude`: the files are written,
/// the two registrations are left to the owner, and one warning.
fn repair_receipt() -> Value {
    json!({
        "target": "claude",
        "changed": ["/Users/you/a.json", "/Users/you/b.json"],
        "backups": ["/Users/you/a.json.1764.bak"],
        "notes": [],
        "conflicts": [],
        "next_steps": [
            "a session already open loads the plugin after `/reload-plugins`; new sessions load \
             it at start",
            "run: claude plugin marketplace add /Users/you/marketplace --scope user --json",
            "run: claude plugin install openwalkietalkie@openwalkietalkie --scope user --json",
            "WARNING: `claude plugin uninstall openwalkietalkie` failed (exit 1); run it by hand"
        ]
    })
}

#[test]
fn a_run_that_left_a_command_or_a_wrong_status_is_not_reported_as_done() {
    let leftover = Leftover::of(&repair_receipt());
    assert_eq!(
        leftover.commands,
        [
            "claude plugin marketplace add /Users/you/marketplace --scope user --json",
            "claude plugin install openwalkietalkie@openwalkietalkie --scope user --json",
        ]
    );
    assert_eq!(leftover.warnings.len(), 1);
    assert!(leftover.warnings[0].starts_with("WARNING"));
    // Left a command: not done, whatever the installer says now.
    assert!(!finished(Verb::Repair, &leftover, Some(&Status::Installed)));
    // A step that only tells leaves nothing to do.
    let told = Leftover::of(&json!({"next_steps": [
        "a session already open loads the plugin after `/reload-plugins`; new sessions load it \
         at start"
    ]}));
    assert!(told.is_empty());
    assert!(finished(Verb::Install, &told, Some(&Status::Installed)));
    // Nothing left, and read again: done only when the installer agrees.
    let none = Leftover::default();
    assert!(!finished(Verb::Install, &none, Some(&Status::Outdated)));
    assert!(!finished(Verb::Repair, &none, Some(&Status::Absent)));
    assert!(finished(Verb::Repair, &none, Some(&Status::Installed)));
    assert!(finished(Verb::Uninstall, &none, Some(&Status::Absent)));
    assert!(!finished(Verb::Uninstall, &none, Some(&Status::Installed)));
    // A read that failed takes the receipt at its word.
    assert!(finished(Verb::Install, &none, None));
    // The line a run that is done says, from its receipt.
    assert_eq!(
        receipt_line("Repair \u{2014} Claude Code", &repair_receipt(), EN),
        "Repair \u{2014} Claude Code: 2 file(s) written, 1 backed up."
    );
    assert_eq!(
        receipt_line("Install \u{2014} Codex Desktop", &Value::Null, EN),
        "Install \u{2014} Codex Desktop: 0 file(s) written, 0 backed up."
    );
}

#[test]
fn the_command_that_finishes_a_run_is_this_apps_deckd_doing_it_again() {
    let leftover = Leftover::of(&repair_receipt());
    let bundled = Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    assert_eq!(
        finish_command(Some(bundled), Verb::Repair, "claude", &leftover.commands),
        "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd integrations repair claude"
    );
    // A path a shell would split, or one with a quote in it, is quoted.
    let spaced = Path::new("/Users/you/My Apps/OpenWalkieTalkie.app/Contents/MacOS/deckd");
    assert_eq!(
        finish_command(
            Some(spaced),
            Verb::Install,
            "codex-desktop",
            &leftover.commands
        ),
        "'/Users/you/My Apps/OpenWalkieTalkie.app/Contents/MacOS/deckd' integrations install \
         codex-desktop"
    );
    assert_eq!(
        finish_command(
            Some(Path::new("/Users/o'neil/deckd")),
            Verb::Uninstall,
            "claude",
            &leftover.commands
        ),
        "'/Users/o'\\''neil/deckd' integrations uninstall claude"
    );
    // No `deckd` beside this copy: the service's own commands, in order.
    assert_eq!(
        finish_command(None, Verb::Repair, "claude", &leftover.commands),
        "claude plugin marketplace add /Users/you/marketplace --scope user --json && claude \
         plugin install openwalkietalkie@openwalkietalkie --scope user --json"
    );
    // The dialog offers a command only for a run that left one, and shows
    // its warnings as the service wrote them.
    let next = NextStep::of(leftover, Verb::Repair, "claude", None);
    assert!(next.command.is_some());
    assert_eq!(next.warnings.len(), 1);
    let warned =
        Leftover::of(&json!({"next_steps": ["WARNING: `claude plugin uninstall` failed"]}));
    let next = NextStep::of(warned, Verb::Uninstall, "claude", Some(bundled));
    assert_eq!(next.command, None);
    assert_eq!(next.warnings, ["WARNING: `claude plugin uninstall` failed"]);
}

#[test]
fn the_plan_dialog_says_the_verb_the_target_and_each_change() {
    i18n::install();
    // The service's own dry run.
    let reply = json!({
        "target": "claude",
        "dry_run": true,
        "changes": [
            {"path": "/Users/you/plugins/openwalkietalkie/hooks/hooks.json", "action": "update"},
            {"path": "/Users/you/plugins/openwalkietalkie/.mcp.json", "action": "create"}
        ],
        "notes": ["writes the `openwalkietalkie` plugin"],
        "conflicts": [],
        "next_steps": ["a session already open loads the plugin after `/reload-plugins`"]
    });
    assert!(Plan::is_dry_run(&reply), "nothing is written by a preview");
    let plan = Plan::parse(&reply);
    assert_eq!(
        present::plan_headline_localized(Verb::Repair, &plan.target, EN),
        "Repair \u{2014} Claude Code"
    );
    assert_eq!(
        present::plan_headline_localized(Verb::Install, "codex-desktop", EN),
        "Install \u{2014} Codex Desktop"
    );
    assert_eq!(
        present::plan_headline_localized(Verb::Uninstall, "claude", ZH),
        "移除 \u{2014} Claude Code"
    );
    assert_eq!(change_word(&plan.changes[0].action, EN), "update");
    assert_eq!(change_word(&plan.changes[1].action, ZH), "创建");
    assert_eq!(change_word("remove", EN), "remove");
    assert_eq!(change_word("teleport", ZH), "teleport");
    // A receipt is not a preview: a service that ignored `dry_run` is told
    // apart.
    assert!(!Plan::is_dry_run(&repair_receipt()));
    // A plan that writes nothing offers nothing to write.
    assert!(Plan::parse(&json!({"target": "claude", "dry_run": true})).is_noop());
    // Codex's trust is offered on an install or a repair, never a removal,
    // and a preview always says it is one.
    assert!(Verb::Install.takes_trust() && Verb::Repair.takes_trust());
    assert!(!Verb::Uninstall.takes_trust());
    let (method, params) = UiAction::Integrations {
        verb: Verb::Install,
        target: "codex-desktop".into(),
        trust: false,
        dry_run: true,
    }
    .request();
    assert_eq!(method, "integrations.install");
    assert_eq!(params["dry_run"], true);
}

#[test]
fn a_method_the_service_does_not_have_is_said_in_every_language() {
    i18n::install();
    let missing = ClientError::Rpc {
        code: -32601,
        message: "method not found".into(),
        data: None,
    };
    assert!(method_missing(&missing));
    assert!(!method_missing(&ClientError::Disconnected));
    let sentence = "This needs a newer background service.";
    for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
        assert_ne!(language.text(sentence), sentence, "{language:?}");
    }
}

#[test]
fn the_board_in_chinese_uses_the_sheets_words() {
    i18n::install();
    let facts = board();
    let rows = tools(&facts, NOW, ZH);
    assert_eq!(row(&rows, "claude").count.as_deref(), Some("3 个会话"));
    assert_eq!(
        row(&rows, "claude").handoff.as_deref(),
        Some("切换到其应用")
    );
    assert_eq!(row(&rows, "claude").state, state("已连接", Tone::Green));
    assert_eq!(
        row(&rows, "claude").sub,
        "插件：13 个钩子和\nChannels 辅助程序。"
    );
    assert_eq!(
        row(&rows, "claude_channels").state,
        state("重启 2 个会话", Tone::Amber)
    );
    assert_eq!(row(&rows, "codex").count.as_deref(), Some("2 个对话"));
    assert_eq!(
        row(&rows, "codex-desktop").state,
        state("未设置", Tone::Gray)
    );
    let apps = apps(&facts, NOW, ZH);
    assert_eq!(apps[0].sub, "会话。聊天语音\n由 Conol 云端识别。");
    assert_eq!(apps[0].handoff.as_deref(), Some("自行处理"));
}

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::integrations::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 37 rows, copied as written but for the hand-off word that
    // now names Ghostty; the page's own sentence for an out-of-date plugin;
    // the six it added since (the count's hint, a connected app heard on
    // this Mac, two for leftover Codex hooks, two for the sessions no row
    // counts); and the Local API page's Advanced, held here.
    assert_eq!(i18n::integrations::STRINGS.len(), 45);
    assert!(!i18n::integrations::STRINGS
        .iter()
        .any(|row| row.0 == "Focuses its tab"));
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = include_str!("../src/integrations.rs");
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate.
    assert_eq!(i18n::literals(source).len(), 91);
}

/// A fresh test service's own replies (`deck-gui-core`'s recorded fixtures,
/// `fixtures/daemon/recorded/`): no integration set up, the Claude Channels
/// bridge and Codex switched off, no app connected, and one test question
/// sent with no deck linked.
const RECORDED_SOURCES: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/sources.list.json");
const RECORDED_CONFIG: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/config.get.json");
const RECORDED_STATUS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/integrations.status.json");
const RECORDED_TEST: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/care.test.status.json");
const RECORDED_QUESTION: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/care.test.question.json");
const RECORDED_TEST_AFTER: &str = include_str!(
    "../../deck-gui-core/tests/fixtures/daemon/recorded/care.test.status.after_question.json"
);

#[test]
fn a_fresh_services_own_replies_offer_set_up_and_show_the_two_switched_off_as_off() {
    i18n::install();
    // Read as the page reads them: `sources.list`, `config.get`'s switches,
    // and `integrations.status` with its old-helper count.
    let status = fixture(RECORDED_STATUS);
    let facts = Facts {
        sources: Some(Sources::parse(&fixture(RECORDED_SOURCES))),
        config: Some(SourcesConfig::parse(&fixture(RECORDED_CONFIG))),
        reports: Report::list(&status).0,
        channels_stale: channels_stale(&status),
    };
    assert_eq!(
        facts.paused(),
        Some(false),
        "Pause integrations: drawn, off"
    );
    assert_eq!(facts.stale_helpers(), 0);
    let rows = tools(&facts, NOW, EN);
    let ids: Vec<&str> = rows.iter().map(|line| line.id.as_str()).collect();
    assert_eq!(ids, ["claude", "claude_channels", "codex", "codex-desktop"]);
    // Claude Code: the plugin is not on this Mac, so it is set up from
    // here; its hooks are counted by the service all the same.
    let claude = row(&rows, "claude");
    assert_eq!(claude.sub, "Plugin: 13 hooks.");
    assert_eq!(
        (
            claude.count.as_deref(),
            claude.age.as_deref(),
            claude.handoff.as_deref()
        ),
        (
            Some("0 sessions"),
            Some("\u{2014}"),
            Some("Raises its app")
        ),
        "the service raises a Claude Code session's own app"
    );
    assert_eq!(claude.total, None);
    assert_eq!(claude.state, state("Not set up", Tone::Gray));
    assert_eq!(claude.control, Control::SetUp("claude"));
    // Claude Channels: off on a new install (decision 13), which is not a
    // pause: off, with its switch, and nothing to connect yet.
    let channels = row(&rows, "claude_channels");
    assert_eq!(
        channels.sub,
        "Voice orders arrive as your own message, not as another session's."
    );
    assert_eq!(channels.state, state("Off", Tone::Gray));
    assert_eq!(
        channels.control,
        Control::Switch(Toggle::ClaudeChannels, false)
    );
    assert!(!channels.connect);
    // Codex CLI: `[codex] daemon = false`, and no older hooks of Codex's
    // left on this Mac.
    let codex = row(&rows, "codex");
    assert_eq!(
        (
            codex.count.as_deref(),
            codex.age.as_deref(),
            codex.handoff.as_deref()
        ),
        (Some("0 threads"), Some("\u{2014}"), Some("Not yet"))
    );
    assert_eq!(codex.state, state("Off", Tone::Gray));
    assert_eq!(codex.control, Control::Switch(Toggle::Codex, false));
    assert_eq!(codex.sub, "Through Codex itself.\nNothing is installed.");
    // The service's default for the Conol switch: a connected app's cloud
    // would hear its chats.
    assert!(facts.companion());
    assert_eq!(others_line(&facts, EN), None, "no session under no row");
    // Codex Desktop: its launcher is not written.
    let desktop = row(&rows, "codex-desktop");
    assert_eq!(desktop.state, state("Not set up", Tone::Gray));
    assert_eq!(desktop.control, Control::SetUp("codex-desktop"));
    assert!(apps(&facts, NOW, EN).is_empty(), "no app has connected");
    // The test question: no line before the first; then the one a question
    // with no deck gets, sent and read back alike.
    let before = TestStatus::parse(&fixture(RECORDED_TEST));
    assert_eq!(test_line(before.question.as_ref(), EN), None);
    assert_eq!(
        TestState::of_reply(&fixture(RECORDED_QUESTION)),
        Some(TestState::NoDeck)
    );
    let after = TestStatus::parse(&fixture(RECORDED_TEST_AFTER));
    assert_eq!(
        test_line(after.question.as_ref(), EN),
        Some((
            "The deck is not linked, so no test question was sent.".to_string(),
            TestMark::Note
        ))
    );
}
