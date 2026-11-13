//! The Deck page's rules, read without a window: which layout the page is
//! in, what each row of the deck's card says in each state, the caption, the
//! idle-home steps, the stop a level shows while the deck catches up, when
//! the deck was last linked, the attention cards, the link's details, and
//! that every word the page says has a row in all four languages.
use deck_gui_core::attention::Attention;
use deck_gui_core::i18n::Language;
use deck_model::{
    DeviceState, DormancyState, FaceEntry, LastLinked, Link, LinkDrop, ServiceState, Snapshot,
    Template, Tone, Transport, Ts,
};
use openwalkietalkie_app::deck::{
    battery_word, caption, drop_line, drop_reason, face_label, face_name, face_shown,
    firmware_word, idle_home_at, idle_home_word, last_linked, layout, level_shown, link_word,
    no_service, no_service_title, notice, recent_drops, stopped_collecting, strip, Layout,
    NoService, Notice, Remedy, Sent, IDLE_HOME, LEVEL_HOLD,
};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::present::{self, LoginItem};
use std::time::Duration;

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;

/// `hour:minute` on 2026-11-11 on this Mac's clock, the boards' day.
fn at(hour: u32, minute: u32) -> Ts {
    present::local_ts(2026, 11, 11, hour, minute).expect("a minute every clock shows")
}

/// The boards' deck as far as this page reads it: linked over Bluetooth at
/// 85 %, a Rev B on 0.13.0, the Agents face up.
fn linked_deck() -> DeviceState {
    DeviceState {
        link: Link::Connected,
        fw: Some("0.13.0-openwalkietalkie".into()),
        board: Some("Rev B".into()),
        transport: Some(Transport::Bluetooth),
        battery: Some(85),
        charging: Some(false),
        face: Some(1),
        last: Some(LastLinked {
            at: at(9, 59),
            fw: Some("0.13.0-openwalkietalkie".into()),
            board: Some("Rev B".into()),
            transport: Some(Transport::Bluetooth),
        }),
        ..DeviceState::default()
    }
}

/// The same deck gone at 09:41: what it said about itself while linked is
/// cleared, and what the service keeps is not.
fn gone_deck() -> DeviceState {
    DeviceState {
        link: Link::Offline,
        fw: None,
        board: None,
        transport: None,
        battery: None,
        charging: None,
        face: None,
        last: Some(LastLinked {
            at: at(9, 41),
            fw: Some("0.13.0-openwalkietalkie".into()),
            board: Some("Rev B".into()),
            transport: Some(Transport::Bluetooth),
        }),
        ..DeviceState::default()
    }
}

fn face(f: u8, t: Template, n: &str) -> FaceEntry {
    FaceEntry { f, t, n: n.into() }
}

#[test]
fn the_page_is_linked_while_the_service_answers_and_the_link_is_up() {
    assert_eq!(layout(true, Link::Connected), Layout::Linked);
    // Up and silent is still linked: the page says it is not answering.
    assert_eq!(layout(true, Link::Degraded), Layout::Linked);
    assert_eq!(layout(true, Link::Offline), Layout::NotLinked);
    // A service that does not answer: its last mirror says nothing about
    // the deck now.
    for link in [Link::Connected, Link::Degraded, Link::Offline] {
        assert_eq!(layout(false, link), Layout::NoService, "{link:?}");
    }
}

#[test]
fn a_service_that_does_not_answer_is_not_installed_only_where_first_run_would_set_it_up() {
    // The installed app whose agent macOS does not have: first run's to do.
    assert_eq!(
        no_service(true, LoginItem::NotRegistered),
        NoService::NotInstalled
    );
    assert_eq!(
        no_service(true, LoginItem::NotFound),
        NoService::NotInstalled
    );
    // Held by macOS until the owner allows it in Login Items: waiting for
    // that, and Start could not start it.
    assert_eq!(
        no_service(true, LoginItem::RequiresApproval),
        NoService::AwaitingApproval
    );
    // Registered, or a status this Mac cannot name: the service is there
    // and not running.
    for agent in [LoginItem::Enabled, LoginItem::Unknown(7)] {
        assert_eq!(no_service(true, agent), NoService::NotRunning, "{agent:?}");
    }
    // A build, a harness walk or a window on a test service cannot tell,
    // and never offers to set the owner's service up.
    for agent in [
        LoginItem::NotRegistered,
        LoginItem::NotFound,
        LoginItem::RequiresApproval,
        LoginItem::Enabled,
    ] {
        assert_eq!(no_service(false, agent), NoService::NotRunning, "{agent:?}");
    }
}

#[test]
fn each_reason_no_service_answers_has_its_own_words_in_every_language() {
    i18n::install();
    let said = [
        NoService::NotRunning,
        NoService::NotInstalled,
        NoService::AwaitingApproval,
    ]
    .map(|why| no_service_title(why, EN));
    assert_eq!(
        said,
        [
            "The background service is not running",
            "The background service is not installed",
            "The background service is waiting for your approval",
        ]
    );
    assert_eq!(
        no_service_title(NoService::AwaitingApproval, ZH),
        "后台服务正在等待你的批准"
    );
    for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
        for (why, english) in [
            NoService::NotRunning,
            NoService::NotInstalled,
            NoService::AwaitingApproval,
        ]
        .into_iter()
        .zip(said)
        {
            assert_ne!(no_service_title(why, language), english, "{language:?}");
        }
    }
    // The buttons beside them are shared words, in every language.
    for button in ["Start", "Open Log", "Set Up\u{2026}", "Open Settings"] {
        assert!(deck_gui_core::i18n::has_entry(button), "{button}");
        assert_ne!(Language::Japanese.text(button), button);
    }
}

#[test]
fn the_link_row_says_how_the_deck_is_linked_or_why_it_is_not() {
    i18n::install();
    let mut deck = linked_deck();
    assert_eq!(
        link_word(Layout::Linked, &deck, EN),
        ("Bluetooth", Tone::Green)
    );
    assert_eq!(link_word(Layout::Linked, &deck, ZH), ("蓝牙", Tone::Green));
    deck.transport = Some(Transport::Usb);
    assert_eq!(link_word(Layout::Linked, &deck, EN), ("Cable", Tone::Green));
    assert_eq!(
        link_word(Layout::Linked, &deck, ZH),
        ("数据线", Tone::Green)
    );
    // A service that predates the transport.
    deck.transport = None;
    assert_eq!(
        link_word(Layout::Linked, &deck, EN),
        ("Linked", Tone::Green)
    );
    // Up and silent: the one state on the card with colour behind it.
    deck.link = Link::Degraded;
    assert_eq!(
        link_word(Layout::Linked, &deck, EN),
        ("Not answering", Tone::Amber)
    );
    assert_eq!(
        link_word(Layout::Linked, &deck, ZH),
        ("无响应", Tone::Amber)
    );
    // Away: the service is waiting; or there is no service to wait.
    let gone = gone_deck();
    assert_eq!(
        link_word(Layout::NotLinked, &gone, EN),
        ("Waiting", Tone::Gray)
    );
    assert_eq!(
        link_word(Layout::NoService, &gone, EN),
        ("Not linked", Tone::Gray)
    );
    assert_eq!(
        link_word(Layout::NoService, &gone, ZH),
        ("未连接", Tone::Gray)
    );
}

#[test]
fn the_battery_row_says_the_charge_whether_it_is_charging_and_a_dash_when_unknown() {
    i18n::install();
    assert_eq!(battery_word(Some(85), Some(false), EN), "85%");
    assert_eq!(battery_word(Some(85), None, EN), "85%");
    assert_eq!(battery_word(Some(40), Some(true), EN), "40% · charging");
    assert_eq!(battery_word(Some(40), Some(true), ZH), "40% · 充电中");
    assert_eq!(
        battery_word(Some(100), Some(true), Language::Japanese),
        "100% · 充電中"
    );
    // The deck has not said: a dash, charging or not.
    assert_eq!(battery_word(None, None, EN), "\u{2014}");
    assert_eq!(battery_word(None, Some(true), ZH), "\u{2014}");
    // Battery is the deck's own word, not the board's 电量.
    assert_eq!(ZH.text("Battery"), "电池");
}

#[test]
fn the_firmware_row_reads_the_linked_deck_else_the_last_one_and_is_left_out_when_unknown() {
    let deck = linked_deck();
    assert_eq!(
        firmware_word(Layout::Linked, &deck).as_deref(),
        Some("0.13.0 · Rev B")
    );
    let mut nameless = deck.clone();
    nameless.board = None;
    assert_eq!(
        firmware_word(Layout::Linked, &nameless).as_deref(),
        Some("0.13.0")
    );
    // Linked and not said yet: no row, never the last deck's version.
    nameless.fw = None;
    assert_eq!(firmware_word(Layout::Linked, &nameless), None);
    // Away, or no service: what the last deck ran.
    let gone = gone_deck();
    for layout in [Layout::NotLinked, Layout::NoService] {
        assert_eq!(
            firmware_word(layout, &gone).as_deref(),
            Some("0.13.0 · Rev B"),
            "{layout:?}"
        );
    }
    // Never linked: no row.
    assert_eq!(
        firmware_word(Layout::NotLinked, &DeviceState::default()),
        None
    );
    assert_eq!(
        firmware_word(Layout::NoService, &DeviceState::default()),
        None
    );
}

#[test]
fn the_caption_names_the_face_on_screen_and_translates_only_a_templates_own_name() {
    i18n::install();
    let now = at(10, 2);
    let faces = vec![
        face(1, Template::Agents, "Agents"),
        face(3, Template::Macropad, "build"),
        face(9, Template::Omarchy, "Omarchy"),
    ];
    let mut deck = linked_deck();
    assert_eq!(
        caption(Layout::Linked, &deck, &faces, now, EN).as_deref(),
        Some("Showing the Agents face")
    );
    assert_eq!(
        caption(Layout::Linked, &deck, &faces, now, ZH).as_deref(),
        Some("正在显示“助手”页面")
    );
    // A name the owner gave is the owner's, in every language.
    deck.face = Some(3);
    assert_eq!(
        caption(Layout::Linked, &deck, &faces, now, ZH).as_deref(),
        Some("正在显示“build”页面")
    );
    // A name is never translated.
    deck.face = Some(9);
    assert_eq!(
        caption(Layout::Linked, &deck, &faces, now, ZH).as_deref(),
        Some("正在显示“Omarchy”页面")
    );
    // The deck has not said which face, or names one the strip lacks.
    for unknown in [None, Some(5)] {
        deck.face = unknown;
        assert_eq!(face_shown(&deck, &faces), None);
        assert_eq!(
            caption(Layout::Linked, &deck, &faces, now, EN).as_deref(),
            Some("Linked")
        );
    }
    // Away: when it was last linked, whatever face it last showed.
    let gone = gone_deck();
    assert_eq!(
        caption(Layout::NotLinked, &gone, &faces, now, EN).as_deref(),
        Some("Last linked today, 09:41")
    );
    assert_eq!(
        caption(Layout::NoService, &DeviceState::default(), &faces, now, EN),
        None
    );
    // An empty name says nothing; the template's own name does.
    assert_eq!(face_name(&face(4, Template::Usage, ""), ZH), "用量");
}

#[test]
fn the_deck_reports_its_own_strip_and_the_page_falls_back_to_the_services() {
    let mut mirror = Snapshot {
        faces: vec![face(1, Template::Agents, "Agents")],
        ..Snapshot::default()
    };
    assert_eq!(strip(&mirror), &mirror.faces[..]);
    mirror.device.faces = vec![
        face(1, Template::Agents, "Agents"),
        face(2, Template::Timer, "Timer"),
    ];
    assert_eq!(strip(&mirror), &mirror.device.faces[..]);
}

#[test]
fn every_face_but_omarchy_has_the_decks_own_words_for_its_name() {
    i18n::install();
    for t in [
        Template::Agents,
        Template::Macropad,
        Template::Usage,
        Template::System,
        Template::Timer,
        Template::Arcade,
        Template::Settings,
    ] {
        let label = face_label(t);
        assert!(deck_gui_core::i18n::has_entry(label), "{label} has no row");
        assert_ne!(ZH.text(label), label, "{label}");
        assert_eq!(face_name(&face(1, t, label), EN), label);
    }
    // Omarchy is a name.
    assert_eq!(face_label(Template::Omarchy), "Omarchy");
    assert_eq!(
        face_name(&face(2, Template::Omarchy, "Omarchy"), ZH),
        "Omarchy"
    );
}

#[test]
fn idle_home_shows_the_step_the_deck_rounds_to_and_each_step_is_its_seconds() {
    i18n::install();
    // Seconds to the choice shown, as the deck rounds them.
    for (seconds, shown) in [
        (0, "Never"),
        (1, "30 s"),
        (30, "30 s"),
        (31, "1 min"),
        (60, "1 min"),
        (61, "2 min"),
        (120, "2 min"),
        (900, "2 min"),
    ] {
        assert_eq!(
            idle_home_word(idle_home_at(seconds), EN),
            shown,
            "{seconds} s"
        );
    }
    // Each choice, back to the seconds it sends, and round again.
    assert_eq!(IDLE_HOME, [0, 30, 60, 120]);
    for (at, seconds) in IDLE_HOME.into_iter().enumerate() {
        assert_eq!(idle_home_at(seconds), at);
    }
    assert_eq!(
        (0..4).map(|at| idle_home_word(at, ZH)).collect::<Vec<_>>(),
        ["从不", "30 秒", "1 分钟", "2 分钟"]
    );
}

#[test]
fn a_level_shows_the_stop_it_sent_until_the_deck_agrees_or_two_seconds_pass() {
    let sent_at = at(10, 2);
    let sent = Sent {
        value: 8,
        at: sent_at,
    };
    // Nothing sent: the deck's own setting.
    assert_eq!(level_shown(6, None, sent_at), 6);
    // Sent and not yet echoed: the stop the pointer left it at.
    assert_eq!(level_shown(6, Some(sent), sent_at), 8);
    assert_eq!(
        level_shown(6, Some(sent), sent_at.plus(Duration::from_millis(1_999))),
        8
    );
    // Two seconds on, the deck's setting is what it is.
    assert_eq!(level_shown(6, Some(sent), sent_at.plus(LEVEL_HOLD)), 6);
    assert!(!sent.holds(sent_at.plus(Duration::from_secs(3))));
    // The deck agrees: the same stop either way.
    assert_eq!(level_shown(8, Some(sent), sent_at), 8);
    // A clock a little behind the stamp is not a reason to let go.
    assert!(sent.holds(Ts(sent_at.millis() - 500)));
    assert_eq!(LEVEL_HOLD, Duration::from_secs(2));
}

#[test]
fn last_linked_reads_today_yesterday_an_older_day_or_nothing() {
    i18n::install();
    let now = at(10, 2);
    let mut deck = gone_deck();
    assert_eq!(
        last_linked(&deck, now, EN).as_deref(),
        Some("Last linked today, 09:41")
    );
    assert_eq!(
        last_linked(&deck, now, ZH).as_deref(),
        Some("上次连接：今天 09:41")
    );
    let yesterday = present::local_ts(2026, 11, 10, 23, 5).unwrap();
    deck.last.as_mut().unwrap().at = yesterday;
    assert_eq!(
        last_linked(&deck, now, EN).as_deref(),
        Some("Last linked yesterday, 23:05")
    );
    let older = present::local_ts(2026, 11, 2, 8, 0).unwrap();
    deck.last.as_mut().unwrap().at = older;
    assert_eq!(
        last_linked(&deck, now, EN).as_deref(),
        Some("Last linked 2026-11-02, 08:00")
    );
    assert_eq!(
        last_linked(&deck, now, Language::Japanese).as_deref(),
        Some("最終接続: 2026-11-02 08:00")
    );
    // Never linked: no caption at all.
    assert_eq!(last_linked(&DeviceState::default(), now, EN), None);
}

/// Under "The background service is waiting for your deck", once the
/// service went quiet (`service.dormancy`): when it stopped, ten minutes
/// after the drop, as the NotLinked board draws it; only that it did when
/// that was on an earlier day; and nothing while it still collects, from a
/// service with no dormancy, or when no deck was ever linked.
#[test]
fn the_not_linked_page_says_when_the_service_stopped_collecting() {
    i18n::install();
    let now = at(10, 2);
    let quiet = |since: Option<Ts>, collecting: bool| ServiceState {
        dormancy: Some(DormancyState {
            window_s: 600,
            collecting,
            dormant_since: since,
        }),
        ..ServiceState::default()
    };
    let today = quiet(Some(at(9, 51)), false);
    assert_eq!(
        stopped_collecting(&today, now, EN).as_deref(),
        Some("It stopped collecting from your agent sessions at 09:51,\nten minutes after the link dropped.")
    );
    assert_eq!(
        stopped_collecting(&today, now, ZH).as_deref(),
        Some("连接断开十分钟后，\n它已于 09:51 停止从你的助手会话收集信息。")
    );
    assert_eq!(
        stopped_collecting(&today, now, Language::Japanese).as_deref(),
        Some("接続が切れて 10 分後の 09:51 に、\nエージェントのセッションからの情報収集を停止しました。")
    );
    let yesterday = quiet(present::local_ts(2026, 11, 10, 23, 5), false);
    assert_eq!(
        stopped_collecting(&yesterday, now, EN).as_deref(),
        Some("It stopped collecting from your agent sessions\nten minutes after the link dropped.")
    );
    assert_eq!(
        stopped_collecting(&yesterday, now, Language::TraditionalChinese).as_deref(),
        Some("連線中斷十分鐘後，\n它已停止從你的助理工作階段收集資訊。")
    );
    for (why, service) in [
        ("still collecting", quiet(None, true)),
        ("never linked", quiet(None, false)),
        ("no dormancy", ServiceState::default()),
    ] {
        assert_eq!(stopped_collecting(&service, now, EN), None, "{why}");
    }
    // The NotLinked board's fixture: dropped at 09:41, quiet since 09:51.
    let board = openwalkietalkie_app::shots::offline_snapshot();
    assert_eq!(
        stopped_collecting(&board.service, openwalkietalkie_app::shots::board_now(), EN).as_deref(),
        Some("It stopped collecting from your agent sessions at 09:51,\nten minutes after the link dropped.")
    );
}

#[test]
fn each_attention_card_says_what_is_wrong_and_offers_its_remedy() {
    i18n::install();
    let card = |item: Attention| notice(&item, 47_201, EN);
    assert_eq!(
        card(Attention::LinkDrops { count: 41 }),
        Notice {
            title: "The link reconnected 41 times in the last hour".into(),
            sub: "Try the cable. If it goes on, send the support file.".into(),
            remedy: Some(Remedy::Details),
        }
    );
    assert_eq!(
        card(Attention::NotAnswering),
        Notice {
            title: "The deck is not answering".into(),
            sub: "It is linked but silent. Move it closer or use the cable.".into(),
            remedy: None,
        }
    );
    // The service's own words for what is wrong with it. (A port another
    // program holds is never one: the attention list leaves it to the
    // Service page's Local API row.)
    let degraded = card(Attention::ServiceDegraded {
        reason: "disk_full".into(),
    });
    assert_eq!(
        degraded,
        Notice {
            title: "The service needs attention".into(),
            sub: "The service is degraded: disk_full.".into(),
            remedy: Some(Remedy::OpenService),
        }
    );
    // One session, or several.
    assert_eq!(
        card(Attention::StaleHelpers { count: 1 }),
        Notice {
            title: "1 Claude session uses an old helper".into(),
            sub: "Restart that session.".into(),
            remedy: None,
        }
    );
    assert_eq!(
        card(Attention::StaleHelpers { count: 3 }),
        Notice {
            title: "3 Claude sessions use an old helper".into(),
            sub: "Restart those sessions.".into(),
            remedy: None,
        }
    );
    // The deck's build name is the version alone.
    assert_eq!(
        card(Attention::FirmwareUpdate {
            version: "0.14.0-openwalkietalkie".into()
        }),
        Notice {
            title: "Deck firmware 0.14.0 is available".into(),
            sub: "Install it by cable from Service.".into(),
            remedy: Some(Remedy::OpenService),
        }
    );
    // In the window's language, the shipped words.
    let zh = notice(&Attention::LinkDrops { count: 41 }, 47_201, ZH);
    assert_eq!(zh.title, "过去一小时内连接断开重连了 41 次");
    assert_eq!(ZH.text("Needs attention"), "需要关注");
}

#[test]
fn the_link_details_list_the_last_hours_drops_newest_first_with_their_reasons() {
    i18n::install();
    let now = at(10, 2);
    let drop = |at: Ts, why: &str| LinkDrop {
        at,
        why: why.into(),
    };
    let mut deck = linked_deck();
    deck.drops = vec![
        // More than an hour old: the service has not pruned it yet.
        drop(at(9, 1), "read"),
        // Exactly an hour old: still in the window.
        drop(at(9, 2), "write"),
        drop(at(9, 40), "removed"),
        drop(at(9, 58), "misses"),
        drop(at(10, 1), "write_stall"),
    ];
    let lines: Vec<String> = recent_drops(&deck, now)
        .into_iter()
        .map(|d| drop_line(d, EN))
        .collect();
    assert_eq!(
        lines,
        [
            "10:01 · write stalled",
            "09:58 · stopped answering",
            "09:40 · unplugged or out of range",
            "09:02 · write error",
        ]
    );
    assert_eq!(drop_line(&drop(at(9, 1), "read"), ZH), "09:01 · 读取错误");
    // A reason this window has no words for is shown as it came.
    assert_eq!(drop_reason("cosmic_ray", ZH), "cosmic_ray");
    // An hour with none.
    deck.drops.truncate(1);
    assert!(recent_drops(&deck, now).is_empty());
}

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::deck::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 56 rows, copied as written, but for the three the frame's
    // strip and the Service page now say too (not installed, Set Up…, Open
    // Service), which are shared words in `kit`; and the two lines of a
    // service that went quiet.
    assert_eq!(i18n::deck::STRINGS.len(), 55);
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = include_str!("../src/deck.rs");
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate.
    assert_eq!(i18n::literals(source).len(), 87);
}

/// A fresh test service's hello (`deck-gui-core`'s recorded fixtures,
/// `fixtures/daemon/recorded/session.hello.json`): `--no-device`, and no
/// deck ever linked to its data directory.
const RECORDED_HELLO: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/session.hello.json");

#[test]
fn a_fresh_test_services_mirror_is_a_deck_never_linked_with_nothing_remembered() {
    use deck_gui_core::{ApiEvent, AppEvent, HostState};
    i18n::install();
    let hello: serde_json::Value = serde_json::from_str(RECORDED_HELLO).expect("JSON");
    let snapshot: Snapshot =
        serde_json::from_value(hello["snapshot"].clone()).expect("the hello's snapshot");
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    let mirror = state.mirror();
    let device = &mirror.device;
    let now = at(10, 2);
    let page = layout(state.connected(), device.link);
    assert_eq!(page, Layout::NotLinked);
    assert_eq!(link_word(page, device, EN), ("Waiting", Tone::Gray));
    // Nothing remembered: no Firmware row, no "Last linked" caption.
    assert_eq!(firmware_word(page, device), None);
    assert_eq!(last_linked(device, now, EN), None);
    assert_eq!(caption(page, device, strip(mirror), now, EN), None);
    assert!(strip(mirror).is_empty());
    assert_eq!(
        battery_word(device.battery, device.charging, EN),
        "\u{2014}"
    );
    assert!(recent_drops(device, now).is_empty());
    // The deck's settings as the service keeps them.
    assert_eq!(
        idle_home_word(idle_home_at(mirror.settings.idlehome), EN),
        "2 min"
    );
    assert_eq!(level_shown(mirror.settings.speaker, None, now), 6);
}
