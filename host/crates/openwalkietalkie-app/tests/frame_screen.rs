//! The frame around the pages: what the sidebar's foot card says for each
//! state of the deck and the service, and which strip each page shows over
//! it — decided by the frame's two pure functions, from the boards' own
//! fixtures (`shots::board_snapshot`, `shots::offline_snapshot`).
use deck_gui_core::i18n::Language;
use deck_model::{Link, Tone, Transport};
use openwalkietalkie_app::deck::{no_service_title, NoService};
use openwalkietalkie_app::frame::{foot, strip, Foot, FootDetails, Strip};
use openwalkietalkie_app::i18n::{install, literals, missing_rows};
use openwalkietalkie_app::shell::Screen;
use openwalkietalkie_app::shots::{board_snapshot, offline_snapshot};

/// Every screen the sidebar is drawn beside but the Deck's.
const OTHER_PAGES: [Screen; 8] = [
    Screen::FacesKeys,
    Screen::Integrations,
    Screen::Voice,
    Screen::Service,
    Screen::Footprint,
    Screen::LocalApi,
    Screen::Kit,
    Screen::MenuPreview,
];

#[test]
fn the_deck_page_says_only_that_the_deck_is_linked_and_every_other_page_says_how() {
    // The Main board: over Bluetooth at 85 %, a Rev B on 0.13.0.
    let device = board_snapshot().device;
    let on_deck = foot(&device, true, Screen::Deck);
    assert_eq!(
        on_deck,
        Foot {
            linked: true,
            tone: Tone::Green,
            details: None,
        }
    );
    assert_eq!(on_deck.title(Language::English), "Deck linked");
    for screen in OTHER_PAGES {
        let card = foot(&device, true, screen);
        assert_eq!((card.linked, card.tone), (true, Tone::Green), "{screen:?}");
        let details = card.details.expect("every other page says how it is linked");
        assert_eq!(
            details,
            FootDetails {
                transport: Some(Transport::Bluetooth),
                battery: Some(85),
                // The deck's own suffix is dropped, its board named.
                firmware: Some("0.13.0 \u{b7} Rev B".into()),
            },
            "{screen:?}"
        );
        assert_eq!(details.transport_word(Language::English), Some("Bluetooth"));
        assert_eq!(details.percent(Language::English).as_deref(), Some("85%"));
    }
}

#[test]
fn a_deck_on_the_cable_says_cable_in_every_language() {
    install();
    let mut device = board_snapshot().device;
    device.transport = Some(Transport::Usb);
    device.battery = Some(100);
    let details = foot(&device, true, Screen::Voice).details.unwrap();
    assert_eq!(details.transport, Some(Transport::Usb));
    for (language, word) in [
        (Language::English, "Cable"),
        (Language::SimplifiedChinese, "数据线"),
        (Language::TraditionalChinese, "連接線"),
        (Language::Japanese, "ケーブル"),
    ] {
        assert_eq!(details.transport_word(language), Some(word), "{language:?}");
        // The figure is the same everywhere; only the word moves.
        assert_eq!(details.percent(language).as_deref(), Some("100%"));
    }
}

#[test]
fn what_the_deck_has_not_said_is_not_drawn_at_all() {
    // The deck-unknown scene's deck: by cable, its charge not said.
    let mut device = board_snapshot().device;
    device.transport = Some(Transport::Usb);
    device.battery = None;
    device.charging = None;
    let details = foot(&device, true, Screen::Integrations).details.unwrap();
    assert_eq!(details.battery, None, "no glyph");
    assert_eq!(details.percent(Language::English), None, "and no figure");
    assert_eq!(details.transport_word(Language::English), Some("Cable"));
    // A service from before the deck said its bus: no word for it either.
    device.transport = None;
    let details = foot(&device, true, Screen::Integrations).details.unwrap();
    assert_eq!(details.transport_word(Language::English), None);
    // The firmware line holds what the deck named, and is gone with neither.
    device.board = None;
    let details = foot(&device, true, Screen::Integrations).details.unwrap();
    assert_eq!(details.firmware.as_deref(), Some("0.13.0"));
    device.fw = None;
    device.board = Some("Rev A".into());
    let details = foot(&device, true, Screen::Integrations).details.unwrap();
    assert_eq!(details.firmware.as_deref(), Some("Rev A"));
    device.board = None;
    let details = foot(&device, true, Screen::Integrations).details.unwrap();
    assert_eq!(details.firmware, None);
}

#[test]
fn no_deck_or_no_service_is_not_linked_on_every_page_with_nothing_under_it() {
    install();
    let not_linked = Foot {
        linked: false,
        tone: Tone::Gray,
        details: None,
    };
    // The NotLinked board: the service is there, the deck is away.
    let away = offline_snapshot().device;
    // The service has gone: whatever its last snapshot said, nothing links
    // the deck to this Mac now.
    let stale = board_snapshot().device;
    for screen in std::iter::once(Screen::Deck).chain(OTHER_PAGES) {
        assert_eq!(foot(&away, true, screen), not_linked, "{screen:?}");
        assert_eq!(foot(&stale, false, screen), not_linked, "{screen:?}");
    }
    for (language, title) in [
        (Language::English, "Not linked"),
        (Language::SimplifiedChinese, "未连接"),
        (Language::TraditionalChinese, "未連線"),
        (Language::Japanese, "未接続"),
    ] {
        assert_eq!(not_linked.title(language), title, "{language:?}");
    }
}

#[test]
fn a_deck_that_is_linked_and_silent_is_linked_with_an_amber_dot() {
    install();
    let mut device = board_snapshot().device;
    device.link = Link::Degraded;
    let card = foot(&device, true, Screen::Service);
    assert_eq!((card.linked, card.tone), (true, Tone::Amber));
    assert!(card.details.is_some(), "it says how it is linked, as ever");
    for (language, title) in [
        (Language::English, "Deck linked"),
        (Language::SimplifiedChinese, "设备已连接"),
        (Language::TraditionalChinese, "裝置已連線"),
        (Language::Japanese, "デバイス接続済み"),
    ] {
        assert_eq!(card.title(language), title, "{language:?}");
    }
}

#[test]
fn the_service_strip_is_over_every_page_but_the_deck_and_the_boards() {
    let not_running = NoService::NotRunning;
    for screen in [
        Screen::FacesKeys,
        Screen::Integrations,
        Screen::Voice,
        Screen::Service,
        Screen::Footprint,
        Screen::LocalApi,
    ] {
        let absent = Some(Strip::NoService(not_running));
        assert_eq!(strip(false, false, not_running, screen), absent, "{screen:?}");
        // Not running says it all: there is nothing to read again from.
        assert_eq!(strip(false, true, not_running, screen), absent, "{screen:?}");
        assert_eq!(strip(true, true, not_running, screen), Some(Strip::Resync), "{screen:?}");
        assert_eq!(strip(true, false, not_running, screen), None, "{screen:?}");
    }
    // The Deck page says the service is not running in a section of its
    // own; a mirror being read again it says nothing about, so the strip
    // does.
    assert_eq!(strip(false, false, not_running, Screen::Deck), None);
    assert_eq!(strip(true, true, not_running, Screen::Deck), Some(Strip::Resync));
    assert_eq!(strip(true, false, not_running, Screen::Deck), None);
    // The harness's boards draw parts and the menu, never the frame's state.
    for screen in [Screen::Kit, Screen::MenuPreview] {
        for (connected, resyncing) in [(false, false), (false, true), (true, true)] {
            assert_eq!(strip(connected, resyncing, not_running, screen), None, "{screen:?}");
        }
    }
}

#[test]
fn the_strip_says_why_no_service_answers_as_the_deck_page_does() {
    install();
    // Never set up, held by macOS for the owner's approval, set up and
    // stopped: each is its own strip, in the Deck page's own words.
    for (absent, said) in [
        (NoService::NotRunning, "The background service is not running"),
        (NoService::NotInstalled, "The background service is not installed"),
        (
            NoService::AwaitingApproval,
            "The background service is waiting for your approval",
        ),
    ] {
        assert_eq!(
            strip(false, false, absent, Screen::Integrations),
            Some(Strip::NoService(absent))
        );
        assert_eq!(no_service_title(absent, Language::English), said);
        for language in [
            Language::SimplifiedChinese,
            Language::TraditionalChinese,
            Language::Japanese,
        ] {
            assert_ne!(no_service_title(absent, language), said, "{language:?}");
        }
        // A service that answers has no strip, whatever the window last
        // thought of it.
        assert_eq!(strip(true, false, absent, Screen::Voice), None);
    }
}

#[test]
fn every_word_the_frame_says_has_a_row() {
    let source = include_str!("../src/frame.rs");
    assert_eq!(missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather
    // than passing as a frame with less to translate. (The strip's words
    // and buttons for a service that does not answer are the Deck page's,
    // `no_service_title` and `no_service_buttons`, and are counted there.)
    assert_eq!(literals(source).len(), 6);
    // Its own rows are translated, not copied.
    for language in [
        Language::SimplifiedChinese,
        Language::TraditionalChinese,
        Language::Japanese,
    ] {
        assert_ne!(language.text("Deck linked"), "Deck linked", "{language:?}");
    }
}
