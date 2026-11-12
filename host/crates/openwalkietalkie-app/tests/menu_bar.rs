//! The status item: its three icons, and its menu line by line, from the
//! model the window's own pages read (`deck_gui_core`'s `MenuModel` and
//! attention list) and the mirror as the reducer folds the service's
//! events. The status item itself needs a menu bar; everything it says and
//! draws is here.
use deck_gui_core::attention::{attention, Extras};
use deck_gui_core::i18n::Language;
use deck_gui_core::{
    ApiEvent, AppEvent, HostState, IconState, MenuCommand, MenuModel, NoService, UiAction,
};
use deck_model::{Event, EventKind, Principal, SetKey, Snapshot};
use openwalkietalkie_app::i18n::{install, literals, missing_rows};
use openwalkietalkie_app::menu::{describe, icon_rgba, lines, Line, ICON_PX};
use openwalkietalkie_app::shots::{board_now, board_snapshot};
use serde_json::json;

const STATES: [IconState; 3] = [IconState::Linked, IconState::NotLinked, IconState::Care];

const SIDE: usize = ICON_PX as usize;

fn alpha(bitmap: &[u8], x: usize, y: usize) -> u8 {
    bitmap[(y * SIDE + x) * 4 + 3]
}

/// The pixels `more` covers more of than `less` does.
fn added(more: &[u8], less: &[u8]) -> Vec<(usize, usize)> {
    (0..SIDE * SIDE)
        .map(|i| (i % SIDE, i / SIDE))
        .filter(|&(x, y)| alpha(more, x, y) > alpha(less, x, y))
        .collect()
}

/// The menu for `snapshot` at the boards' minute, in English, with a
/// service that is set up when it does not answer.
fn model(snapshot: &Snapshot, connected: bool, paused: Option<bool>) -> MenuModel {
    let list = attention(snapshot, connected, board_now(), &Extras::default());
    MenuModel::of(
        snapshot,
        connected,
        NoService::NotRunning,
        paused,
        &list,
        Language::English,
    )
}

/// The menu as one line of text each: `(status)`, a tick as `[x]` or
/// `[ ]`, a disabled item `(greyed)`, a separator `---`.
fn shown(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            Line::Status(text) => format!("({text})"),
            Line::Separator => "---".into(),
            Line::Item {
                label,
                enabled,
                checked,
                ..
            } => {
                let tick = match checked {
                    Some(true) => "[x] ",
                    Some(false) => "[ ] ",
                    None => "",
                };
                let greyed = if *enabled { "" } else { " (greyed)" };
                format!("{tick}{label}{greyed}")
            }
        })
        .collect()
}

/// The command of every item, in order.
fn commands(lines: &[Line]) -> Vec<MenuCommand> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Item { command, .. } => Some(*command),
            _ => None,
        })
        .collect()
}

#[test]
fn the_three_icons_are_whole_36_px_template_images_that_differ() {
    let bitmaps: Vec<Vec<u8>> = STATES.into_iter().map(icon_rgba).collect();
    for (state, bitmap) in STATES.iter().zip(&bitmaps) {
        assert_eq!(bitmap.len(), SIDE * SIDE * 4, "{state:?}");
        // A template image: black, with its shape in the alpha alone.
        assert!(
            bitmap.chunks(4).all(|pixel| pixel[..3] == [0, 0, 0]),
            "{state:?}"
        );
        // A glyph of strokes: neither a speck nor a filled block.
        let solid = bitmap.chunks(4).filter(|pixel| pixel[3] >= 128).count();
        assert!((200..=450).contains(&solid), "{state:?}: {solid} solid pixels");
        // Whole: nothing of it reaches the bitmap's edge, so nothing is cut.
        for i in 0..SIDE {
            for (x, y) in [(i, 0), (i, SIDE - 1), (0, i), (SIDE - 1, i)] {
                assert_eq!(alpha(bitmap, x, y), 0, "{state:?} at ({x}, {y})");
            }
        }
    }
    for (i, a) in bitmaps.iter().enumerate() {
        for b in &bitmaps[i + 1..] {
            assert_ne!(a, b, "three states, three shapes");
        }
    }
    // The slab, the screen and the key are the same in all three: a state
    // only adds to the glyph.
    for bitmap in &bitmaps[1..] {
        assert!(added(&bitmaps[0], bitmap).is_empty());
    }
}

#[test]
fn care_is_a_dot_at_the_top_right() {
    let linked = icon_rgba(IconState::Linked);
    let care = icon_rgba(IconState::Care);
    let dot = added(&care, &linked);
    assert!(dot.len() > 20, "a dot, not a speck: {}", dot.len());
    let half = SIDE / 2;
    for &(x, y) in &dot {
        assert!(x >= half && y < half, "({x}, {y}) is outside the top-right quadrant");
    }
    // Its centre (19, 4.5 on the glyph's 24-unit grid) is solid.
    assert_eq!(alpha(&care, 28, 6), 255);
}

#[test]
fn not_linked_is_a_slash_across_the_screen() {
    let linked = icon_rgba(IconState::Linked);
    let off = icon_rgba(IconState::NotLinked);
    let slash = added(&off, &linked);
    assert!(slash.len() > 20, "a slash, not a speck: {}", slash.len());
    // Inside the screen's square (8.5 to 15.5 across, 5 to 12 down on the
    // grid, 12.75 to 23.25 and 7.5 to 18 px) and its stroke.
    for &(x, y) in &slash {
        assert!(
            (11..=24).contains(&x) && (6..=19).contains(&y),
            "({x}, {y}) is off the screen"
        );
    }
    // Through its middle, which the glyph leaves empty...
    assert_eq!(alpha(&linked, 17, 12), 0);
    assert_eq!(alpha(&off, 17, 12), 255);
    // ...from its lower left to its upper right.
    let centre = (18, 13);
    assert!(slash.iter().any(|&(x, y)| x < centre.0 - 2 && y > centre.1 + 2));
    assert!(slash.iter().any(|&(x, y)| x > centre.0 + 2 && y < centre.1 - 2));
    assert!(!slash.iter().any(|&(x, y)| x < centre.0 - 2 && y < centre.1 - 2));
}

#[test]
fn the_menu_is_the_status_line_the_two_toggles_and_the_windows_own_three() {
    // An ordinary day: linked, no drops, the pause read and off.
    let mut quiet = board_snapshot();
    quiet.device.drops.clear();
    let menu = model(&quiet, true, Some(false));
    assert_eq!(menu.icon, IconState::Linked);
    let lines = lines(&menu, true, Language::English);
    assert_eq!(
        shown(&lines),
        [
            "(Linked over Bluetooth \u{b7} 85%)",
            "---",
            "[x] Auto-Announce",
            "[ ] Pause Integrations",
            "---",
            "Open OpenWalkieTalkie",
            "Check for Updates\u{2026}",
            "---",
            "Quit OpenWalkieTalkie",
        ]
    );
    assert_eq!(
        commands(&lines),
        [
            MenuCommand::AutoAnnounce,
            MenuCommand::PauseIntegrations,
            MenuCommand::Open,
            MenuCommand::CheckUpdates,
            MenuCommand::Quit,
        ]
    );
    // A tick on the two toggles and on nothing else.
    for line in &lines {
        if let Line::Item {
            command, checked, ..
        } = line
        {
            assert_eq!(checked.is_some(), command.is_toggle(), "{command:?}");
        }
    }
}

#[test]
fn a_link_problem_puts_its_item_under_the_status_line_and_the_care_icon_up() {
    // The MenuBar board: the Main board's 41 reconnects in the hour.
    let menu = model(&board_snapshot(), true, Some(false));
    assert_eq!(menu.icon, IconState::Care);
    assert_eq!(
        shown(&lines(&menu, true, Language::English)),
        [
            "(Linked over Bluetooth \u{b7} 85%)",
            "Show Link Problem\u{2026}",
            "---",
            "[x] Auto-Announce",
            "[ ] Pause Integrations",
            "---",
            "Open OpenWalkieTalkie",
            "Check for Updates\u{2026}",
            "---",
            "Quit OpenWalkieTalkie",
        ]
    );
    // Care that is not about the link draws the icon and offers no
    // details: a firmware update.
    let mut quiet = board_snapshot();
    quiet.device.drops.clear();
    let extras = Extras {
        stale_helpers: 0,
        firmware: Some("0.14.0".into()),
    };
    let list = attention(&quiet, true, board_now(), &extras);
    let menu = MenuModel::of(
        &quiet,
        true,
        NoService::NotRunning,
        Some(false),
        &list,
        Language::English,
    );
    assert_eq!(menu.icon, IconState::Care);
    let commands = commands(&lines(&menu, true, Language::English));
    assert!(!commands.contains(&MenuCommand::ShowLinkProblem));
}

#[test]
fn another_program_on_the_local_apis_port_is_no_care_for_the_menu_bar() {
    // An ordinary day, but for a port another program holds: the local
    // API is closed, and nothing the deck or a session needs is, so the
    // icon is the linked one and the menu offers nothing about it.
    let mut quiet = board_snapshot();
    quiet.device.drops.clear();
    for reason in ["port_in_use", "hooks_hijack_risk"] {
        quiet.service.degraded = Some(reason.into());
        let menu = model(&quiet, true, Some(false));
        assert_eq!(menu.icon, IconState::Linked, "{reason}");
        assert_eq!(
            shown(&lines(&menu, true, Language::English))[0],
            "(Linked over Bluetooth \u{b7} 85%)"
        );
        assert!(!menu.link_problem, "{reason}");
    }
}

#[test]
fn with_no_service_the_status_line_says_whether_it_is_not_running_not_installed_or_waiting() {
    let snapshot = board_snapshot();
    for (absent, status) in [
        (NoService::NotRunning, "(Service not running)"),
        (NoService::NotInstalled, "(Service not installed)"),
        (NoService::AwaitingApproval, "(Service waiting for approval)"),
    ] {
        let menu = MenuModel::of(&snapshot, false, absent, None, &[], Language::English);
        assert_eq!(menu.icon, IconState::NotLinked, "{absent:?}");
        let lines = lines(&menu, false, Language::English);
        assert_eq!(shown(&lines)[0], status);
        // Nothing the service would do can be pressed without it.
        for line in &lines {
            if let Line::Item {
                command, enabled, ..
            } = line
            {
                if command.is_toggle() {
                    assert!(!enabled, "{absent:?}: {command:?}");
                }
            }
        }
    }
}

#[test]
fn the_announce_tick_follows_the_mirror_and_asks_for_the_opposite() {
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected {
        snapshot: board_snapshot(),
    }));
    let ticked = |state: &HostState| {
        let menu = model(state.mirror(), state.connected(), None);
        let announce = lines(&menu, true, Language::English)
            .into_iter()
            .find_map(|line| match line {
                Line::Item {
                    command: MenuCommand::AutoAnnounce,
                    checked,
                    ..
                } => checked,
                _ => None,
            });
        (announce, menu.announce_action())
    };
    // The boards' deck speaks its replies: ticked, and a click turns it off.
    assert_eq!(
        ticked(&state),
        (
            Some(true),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 0,
            }
        )
    );
    // The service's own event moves the tick; nothing else does.
    state.handle_event(AppEvent::Api(ApiEvent::Event(Event {
        rev: 2,
        ts: board_now(),
        kind: EventKind::DeviceSet,
        principal: Principal::Uid(501),
        data: json!({"k": "announce", "v": 0}),
    })));
    assert_eq!(
        ticked(&state),
        (
            Some(false),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 1,
            }
        )
    );
}

#[test]
fn pause_waits_to_be_read_and_both_toggles_wait_for_the_service() {
    let snapshot = board_snapshot();
    let pause = |connected: bool, paused: Option<bool>| {
        let menu = model(&snapshot, connected, paused);
        lines(&menu, connected, Language::English)
            .into_iter()
            .find(|line| {
                matches!(
                    line,
                    Line::Item {
                        command: MenuCommand::PauseIntegrations,
                        ..
                    }
                )
            })
            .map(|line| shown(&[line]).remove(0))
    };
    assert_eq!(
        pause(true, None).as_deref(),
        Some("[ ] Pause Integrations (greyed)"),
        "unread, it guesses nothing"
    );
    assert_eq!(pause(true, Some(false)).as_deref(), Some("[ ] Pause Integrations"));
    assert_eq!(pause(true, Some(true)).as_deref(), Some("[x] Pause Integrations"));
    // The service has gone: both toggles keep what it last said, and
    // neither can be pressed; nor can the link's details be asked for.
    let gone = model(&snapshot, false, Some(true));
    assert_eq!(gone.icon, IconState::NotLinked);
    assert_eq!(
        shown(&lines(&gone, false, Language::English)),
        [
            "(Service not running)",
            "---",
            "[x] Auto-Announce (greyed)",
            "[x] Pause Integrations (greyed)",
            "---",
            "Open OpenWalkieTalkie",
            "Check for Updates\u{2026}",
            "---",
            "Quit OpenWalkieTalkie",
        ]
    );
}

#[test]
fn the_menu_and_its_icon_speak_the_windows_language() {
    install();
    let snapshot = board_snapshot();
    let english = lines(&model(&snapshot, true, Some(false)), true, Language::English);
    for language in [
        Language::SimplifiedChinese,
        Language::TraditionalChinese,
        Language::Japanese,
    ] {
        let list = attention(&snapshot, true, board_now(), &Extras::default());
        let menu = MenuModel::of(
            &snapshot,
            true,
            NoService::NotRunning,
            Some(false),
            &list,
            language,
        );
        let translated = lines(&menu, true, language);
        assert_eq!(translated.len(), english.len());
        for (line, original) in translated.iter().zip(&english) {
            match (line, original) {
                (Line::Status(text), Line::Status(original)) => {
                    assert_ne!(text, original, "{language:?}");
                    assert!(text.contains("85%"), "{language:?}: {text}");
                }
                (Line::Item { label, .. }, Line::Item { label: original, .. }) => {
                    assert_ne!(label, original, "{language:?}");
                }
                (Line::Separator, Line::Separator) => {}
                _ => panic!("{language:?}: {line:?} where English has {original:?}"),
            }
        }
        for state in STATES {
            let words = describe(state, language);
            assert_ne!(words, describe(state, Language::English), "{language:?}");
            assert!(words.contains("OpenWalkieTalkie"), "{language:?}: {words}");
        }
    }
    // The tooltip says what the shape means.
    assert_eq!(
        STATES.map(|state| describe(state, Language::English)),
        [
            "OpenWalkieTalkie status item, the deck is linked",
            "OpenWalkieTalkie status item, the deck is not linked",
            "OpenWalkieTalkie status item, the deck needs care",
        ]
    );
}

#[test]
fn every_word_the_status_item_says_has_a_row() {
    let source = include_str!("../src/menu.rs");
    assert_eq!(missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather
    // than passing as a menu with less to translate.
    assert_eq!(literals(source).len(), 6);
}

#[test]
fn a_fresh_test_services_own_replies_make_the_not_linked_menu_with_pause_unticked() {
    install();
    // `deck-gui-core`'s recorded fixtures: the hello's snapshot (no deck
    // ever linked, `announce` on) and `config.get`'s switches.
    let hello: serde_json::Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/recorded/session.hello.json"
    ))
    .expect("JSON");
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/recorded/config.get.json"
    ))
    .expect("JSON");
    let snapshot: Snapshot =
        serde_json::from_value(hello["snapshot"].clone()).expect("the hello's snapshot");
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    let paused = deck_gui_core::SourcesConfig::parse(&config).effective_pause();
    let menu = model(state.mirror(), true, paused);
    assert_eq!(menu.icon, IconState::NotLinked);
    assert_eq!(
        shown(&lines(&menu, true, Language::English)),
        [
            "(Not linked)",
            "---",
            "[x] Auto-Announce",
            "[ ] Pause Integrations",
            "---",
            "Open OpenWalkieTalkie",
            "Check for Updates\u{2026}",
            "---",
            "Quit OpenWalkieTalkie",
        ]
    );
}
