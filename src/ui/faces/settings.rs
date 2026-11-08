//! Settings face: SETTINGS header, a tab strip of the six panels (the current
//! one outlined), then the panel's rows: label left, value right. A level
//! setting draws a bar plus its number, a boolean a toggle pill, the rest a
//! value text. The focused row has the selected-card look, outlined in blue
//! while its value is being adjusted; a touched tab or row the pressed fill. Host-side settings (speaker, mic, announce, chime)
//! read "needs host" while no deck host is present and the demo is off.
//!
//! `row_content` describes every row once; `draw` renders it and `signature`
//! hashes it, so the two cannot disagree about what a row shows.
use crate::deck::msg::{SettingKey, FW_VERSION};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::settings::{rows, setting_at, viewport, FACTORY_RESET_ROW, LANGUAGE_ROW, PANEL_NAMES, TUTORIAL_ROW};
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, kit, layout, layout::Rect, theme};
use crate::battery::ChargeState;
use crate::orientation;

/// The value side of a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value {
    /// A bar filled `level` of `max`, with the number beside it.
    Level { level: u8, max: u8 },
    Toggle(bool),
    Text(Text<64>),
    /// Speaker/mic/announce/chime without a deck host: dimmed, inert.
    NeedsHost,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub label: &'static str,
    pub value: Value,
}

fn text(s: &str) -> Value {
    let mut t = Text::new();
    t.set(s);
    Value::Text(t)
}

fn percent_text(percent: u8, suffix: &str) -> Value {
    let mut t = Text::<64>::new();
    let p = percent.min(100);
    if p >= 100 {
        t.push_char('1');
    }
    if p >= 10 {
        t.push_char((b'0' + p / 10 % 10) as char);
    }
    t.push_char((b'0' + p % 10) as char);
    t.push_char('%');
    t.push_str(suffix);
    Value::Text(t)
}

/// Host-side settings need a host to act on them (the demo host counts).
pub fn needs_host(ui: &UiState) -> bool {
    !ui.deck.host_present && !ui.deck.settings.demo
}

/// The row `row` of `panel`, or `None` past the panel's rows.
pub fn row_content(ui: &UiState, panel: u8, row: u8) -> Option<Row> {
    if row >= rows(panel) {
        return None;
    }
    let settings = &ui.deck.settings;
    let host_side = |value: Value| if needs_host(ui) { Value::NeedsHost } else { value };
    let (label, value) = match setting_at(panel, row) {
        Some(SettingKey::Speaker) => ("Speaker", host_side(Value::Level { level: settings.speaker, max: 10 })),
        Some(SettingKey::Mic) => ("Mic gain", host_side(Value::Level { level: settings.mic, max: 10 })),
        Some(SettingKey::Announce) => ("Auto-announce", host_side(Value::Toggle(settings.auto_announce))),
        Some(SettingKey::Chime) => ("Chime", host_side(Value::Toggle(settings.chime))),
        Some(SettingKey::AutoJump) => ("Auto-jump", Value::Toggle(settings.auto_jump)),
        Some(SettingKey::Brightness) => ("Brightness", Value::Level { level: settings.brightness, max: 5 }),
        Some(SettingKey::IdleHome) => (
            "Idle home",
            text(ui.tr(match settings.idle_home_s {
                0 => "Never",
                1..=30 => "30 s",
                31..=60 => "1 min",
                _ => "2 min",
            })),
        ),
        Some(SettingKey::ArcadeSound) => ("Arcade sound", text(ui.tr(crate::ui::state::arcade::sound_label(settings.arcade_sound)))),
        Some(SettingKey::TextSize) => (
            "Text size",
            text(ui.tr(match settings.text_size {
                0 => "Small",
                1 => "Medium",
                _ => "Large",
            })),
        ),
        Some(SettingKey::Demo) => ("Demo data", Value::Toggle(settings.demo)),
        None => match (panel, row) {
            LANGUAGE_ROW => ("Language", text(ui.language.name())),
            TUTORIAL_ROW => ("Tutorial", text(ui.tr("replay"))),
            (4, 0) => ("Link", text(ui.tr(if ui.connected { "Connected" } else { "Disconnected" }))),
            (4, 1) => (
                "Deck host",
                text(ui.tr(if !ui.deck.host_present {
                    "Absent"
                } else if ui.deck.syncing {
                    "Syncing"
                } else {
                    "Present"
                })),
            ),
            (5, 0) => ("Firmware", firmware_value()),
            (5, 1) => (
                "Battery",
                match (ui.battery.percentage(), ui.battery.charge) {
                    (Some(p), ChargeState::Charging) => percent_text(p, ui.tr(" \u{b7} charging")),
                    (Some(p), ChargeState::Full) => percent_text(p, ui.tr(" \u{b7} full")),
                    (Some(p), _) => percent_text(p, ""),
                    (None, ChargeState::Charging) => text(ui.tr("charging")),
                    (None, _) => text("\u{2014}"),
                },
            ),
            // Discoverability only. The gesture is the CANCEL the deck itself
            // owns.
            (5, 2) => ("Power off", text(ui.tr("hold CANCEL 3 s"))),
            FACTORY_RESET_ROW => ("Factory reset", text(ui.tr("press ENTER"))),
            _ => return None,
        },
    };
    Some(Row { label: ui.tr(label), value })
}

/// "0.1.0 · Rev B": the version and the board it was built for (the panel
/// rotation that used to have its own row follows from the board).
pub fn firmware_value() -> Value {
    let mut t = Text::<64>::new();
    t.set(FW_VERSION);
    t.push_str(" \u{b7} ");
    t.push_str(orientation::BOARD);
    Value::Text(t)
}

/// Toggle pill inside a row: its rect relative to the row's origin.
const TOGGLE: Rect = Rect::new(layout::SETTING_VALUE_RIGHT - 44, 17, 44, 24);
const TABS: Rect = Rect::new(theme::MARGIN, 68, theme::SCREEN - 2 * theme::MARGIN, 34);
const ABOUT_BRAND: Rect = Rect::new(32, 376, 416, 52);

const HINTS: [kit::Hint<'static>; 3] = [
    kit::Hint { key: "SCROLL", label: "select" },
    kit::Hint { key: "ENTER", label: "change" },
    kit::Hint { key: "\u{2039} \u{203a}", label: "panel" },
];
const ADJUST_HINTS: [kit::Hint<'static>; 2] = [
    kit::Hint { key: "SCROLL", label: "adjust" },
    kit::Hint { key: "ENTER", label: "done" },
];

/// `View::pressed` encoding: 0..6 a tab, 8 + n row n.
const PRESSED_ROW: u8 = 8;

fn level_text(level: u8) -> Text<4> {
    let mut t = Text::new();
    if level >= 10 {
        t.push_char((b'0' + level / 10 % 10) as char);
    }
    t.push_char((b'0' + level % 10) as char);
    t
}

fn draw_row(band: &mut Band, ui: &UiState, rect: Rect, row: Row, focused: bool, adjusting: bool, pressed: bool) {
    let (fill, label_color) = if adjusting {
        kit::round(band, rect, theme::CARD_R, theme::CARD_FILL);
        kit::outline(band, rect, theme::CARD_R, theme::BLUE, 2);
        (theme::CARD_FILL, theme::TEXT_PRIMARY)
    } else if pressed {
        kit::round(band, rect, theme::CARD_R, theme::CHIP_FILL);
        kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_SELECTED, 1);
        (theme::CHIP_FILL, theme::TEXT_PRIMARY)
    } else if focused {
        kit::card(band, rect, true, false);
        (theme::CARD_FILL, theme::TEXT_PRIMARY)
    } else {
        kit::card(band, rect, false, false);
        (theme::CANVAS, theme::TEXT_ROW)
    };
    let dim = row.value == Value::NeedsHost;
    let baseline = rect.top() + layout::SETTING_BASELINE;
    let right = rect.left() + layout::SETTING_VALUE_RIGHT;
    font::draw(
        band,
        Font::Row,
        rect.left() + layout::SETTING_LABEL_X,
        baseline,
        row.label,
        if dim { theme::TEXT_FAINT } else { label_color },
        Some(fill),
        layout::SETTING_BAR.left() - layout::SETTING_LABEL_X - 8,
        Align::Left,
    );
    match row.value {
        Value::Level { level, max } => {
            let bar = layout::SETTING_BAR.offset(rect.left(), rect.top());
            let percent = (level.min(max) as u32 * 100 / max.max(1) as u32) as u8;
            kit::bar(band, bar, percent, theme::BLUE);
            font::draw(band, Font::Row, right, baseline, level_text(level).as_str(), theme::TEXT_PRIMARY, Some(fill), right - bar.right() - 8, Align::Right);
        }
        Value::Toggle(on) => {
            let pill = TOGGLE.offset(rect.left(), rect.top());
            let radius = (pill.height() / 2) as u32;
            if on {
                kit::round(band, pill, radius, theme::GREEN);
                kit::dot(band, pill.right() - 12, pill.top() + 12, 8, theme::TEXT_PRIMARY);
            } else {
                kit::round(band, pill, radius, theme::CHIP_FILL);
                kit::outline(band, pill, radius, theme::OUTLINE_KEY, 1);
                kit::dot(band, pill.left() + 12, pill.top() + 12, 8, theme::TEXT_LABEL);
            }
            font::draw(band, Font::Row, pill.left() - 12, baseline, ui.tr(if on { "On" } else { "Off" }), theme::TEXT_ROW, Some(fill), 60, Align::Right);
        }
        Value::Text(value) => {
            font::draw(band, Font::Row, right, baseline, value.as_str(), theme::TEXT_ROW, Some(fill), 210, Align::Right);
        }
        Value::NeedsHost => {
            font::draw(band, Font::Small, right, baseline, ui.tr("needs host"), theme::TEXT_FAINT, Some(fill), 120, Align::Right);
        }
    }
}

/// The one face every tab is drawn in: Small when all six names fit their
/// chips, else Hint, so the strip never mixes sizes.
pub fn tab_font() -> Font {
    let room = layout::settings_tab(0).width() - 8;
    if PANEL_NAMES.iter().all(|name| font::measure(Font::Small, name) <= room) {
        Font::Small
    } else {
        Font::Hint
    }
}

fn draw_tab(band: &mut Band, ui: &UiState, rect: Rect, name: &str, selected: bool, pressed: bool) {
    let font = if PANEL_NAMES.iter().all(|name| font::measure(Font::Small, ui.tr(name)) <= rect.width() - 8) { Font::Small } else { Font::Hint };
    if pressed && !selected {
        kit::round(band, rect, theme::CHIP_R, theme::CHIP_FILL);
        kit::outline(band, rect, theme::CHIP_R, theme::OUTLINE, 1);
        let (cx, cy) = rect.center();
        font::draw(band, font, cx, cy + font::ascent(font) * 9 / 20, name, theme::TEXT_PRIMARY, Some(theme::CHIP_FILL), rect.width() - 8, Align::Center);
    } else {
        kit::chip_in(band, rect, name, selected, font);
    }
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    let _ = instance;
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("SETTINGS"), kit::header_right(ui, kit::status_tag(ui)));
    let panel = ui.settings.panel.min(layout::SETTINGS_PANELS - 1);
    if kit::visible(band, TABS) {
        for index in 0..layout::SETTINGS_PANELS {
            let name = PANEL_NAMES.get(index as usize).copied().unwrap_or("");
            draw_tab(band, ui, layout::settings_tab(index), ui.tr(name), index == panel, ui.settings.pressed == Some(index));
        }
    }
    for (slot, index) in viewport(ui).slots() {
        let rect = layout::settings_row(slot);
        let Some(row) = row_content(ui, panel, index) else { continue };
        if kit::visible(band, rect) {
            let focused = index == ui.settings.row;
            let adjusting = focused && ui.settings.adjusting;
            draw_row(band, ui, rect, row, focused, adjusting, ui.settings.pressed == Some(PRESSED_ROW + index));
        }
    }
    if panel == 5 && kit::visible(band, ABOUT_BRAND) {
        crate::ui::splash::draw_radio(band, 72, 380, 3);
        font::draw(band, Font::Small, 124, 394, "OpenWalkieTalkie", theme::TEXT_PRIMARY, Some(theme::CANVAS), 300, Align::Left);
        crate::ui::splash::draw_credit(band, 204, 397, theme::TEXT_LABEL);
    }
    let mut hints = HINTS;
    let mut adjust_hints = ADJUST_HINTS;
    for hint in hints.iter_mut().chain(adjust_hints.iter_mut()) {
        hint.label = ui.tr(hint.label);
    }
    kit::hint_bar(band, if ui.settings.adjusting { &adjust_hints } else { &hints });
}

fn hash_row(row: Option<Row>, h: &mut Hasher) {
    let Some(row) = row else {
        h.bool(false);
        return;
    };
    h.bool(true).str(row.label);
    match row.value {
        Value::Level { level, max } => h.u8(1).u8(level).u8(max),
        Value::Toggle(on) => h.u8(2).bool(on),
        Value::Text(text) => h.u8(3).text(&text),
        Value::NeedsHost => h.u8(4),
    };
}

/// Header, tab strip, the four row lanes and the hint bar.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    let _ = instance;
    let panel = ui.settings.panel.min(layout::SETTINGS_PANELS - 1);
    sig.region_with(layout::HEADER, |h| crate::ui::overlay::notify::header_inputs(ui, h));
    sig.region_with(TABS, |h| {
        h.u8(panel).opt_u32(ui.settings.pressed.filter(|&p| p < PRESSED_ROW).map(u32::from));
    });
    let viewport = viewport(ui);
    for slot in 0..layout::SETTINGS_ROWS {
        sig.region_with(layout::settings_row(slot), |h| {
            let Some(index) = viewport.index_at(slot) else { h.bool(false); return };
            let focused = index == ui.settings.row;
            h.bool(focused)
                .bool(focused && ui.settings.adjusting)
                .bool(ui.settings.pressed == Some(PRESSED_ROW + index));
            hash_row(row_content(ui, panel, index), h);
        });
    }
    sig.region(layout::HINT_BAR, u32::from(ui.settings.adjusting));
    sig.region(ABOUT_BRAND, u32::from(panel == 5));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::msg::HostMsg;
    use crate::ui::state::settings::PANELS;
    use crate::ui::Update;

    fn hosted(host: bool, demo: bool) -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.host_present = host;
        ui.deck.settings.demo = demo;
        ui
    }

    #[test]
    fn every_row_of_every_panel_has_content_and_the_rest_is_none() {
        let ui = hosted(true, false);
        for panel in 0..PANELS {
            for row in 0..8 {
                assert_eq!(row_content(&ui, panel, row).is_some(), row < rows(panel), "{panel}/{row}");
            }
        }
        assert_eq!(row_content(&ui, PANELS, 0), None);
        assert_eq!(row_content(&ui, 5, 0).map(|r| r.value), Some(firmware_value()));
        let Value::Text(firmware) = firmware_value() else { panic!() };
        assert!(firmware.as_str().starts_with(FW_VERSION), "the firmware row leads with the version");
        assert_eq!(row_content(&ui, TUTORIAL_ROW.0, TUTORIAL_ROW.1).map(|r| r.label), Some("Tutorial"));
        assert_eq!(row_content(&ui, FACTORY_RESET_ROW.0, FACTORY_RESET_ROW.1).map(|r| r.label), Some("Factory reset"));
    }

    #[test]
    fn host_side_settings_need_a_host_unless_the_demo_stands_in() {
        let host_side = [(0, 0), (0, 1), (1, 0), (2, 0)];
        let local = [(2, 1), (3, 0), (3, 1), (4, 2)];
        let cases = [(false, false, true), (true, false, false), (false, true, false), (true, true, false)];
        for (host, demo, expected) in cases {
            let ui = hosted(host, demo);
            assert_eq!(needs_host(&ui), expected);
            for (panel, row) in host_side {
                let value = row_content(&ui, panel, row).map(|r| r.value);
                assert_eq!(value == Some(Value::NeedsHost), expected, "{panel}/{row} host {host} demo {demo}");
            }
            for (panel, row) in local {
                assert_ne!(row_content(&ui, panel, row).map(|r| r.value), Some(Value::NeedsHost));
            }
        }
    }

    #[test]
    fn values_follow_the_model() {
        let mut ui = hosted(true, false);
        let mut out = Update::new();
        for (key, value, panel, row, expected) in [
            (SettingKey::Speaker, 3, 0, 0, Value::Level { level: 3, max: 10 }),
            (SettingKey::Brightness, 2, 3, 0, Value::Level { level: 2, max: 5 }),
            (SettingKey::AutoJump, 1, 2, 1, Value::Toggle(true)),
            (SettingKey::IdleHome, 0, 3, 1, text("Never")),
            (SettingKey::IdleHome, 30, 3, 1, text("30 s")),
            (SettingKey::IdleHome, 60, 3, 1, text("1 min")),
            (SettingKey::IdleHome, 120, 3, 1, text("2 min")),
        ] {
            ui.host(&HostMsg::Set { key, value }, 0, &mut out);
            assert_eq!(row_content(&ui, panel, row).map(|r| r.value), Some(expected), "{key:?}");
        }
        ui.connected = true;
        assert_eq!(row_content(&ui, 4, 0).map(|r| r.value), Some(text("Connected")));
        ui.deck.syncing = true;
        assert_eq!(row_content(&ui, 4, 1).map(|r| r.value), Some(text("Syncing")));
        assert_eq!(row_content(&ui, 5, 1).map(|r| r.value), Some(text("\u{2014}")));
        ui.battery.estimated_percent = Some(100);
        ui.battery.charge = ChargeState::Charging;
        assert_eq!(row_content(&ui, 5, 1).map(|r| r.value), Some(text("100% \u{b7} charging")));
        ui.battery.estimated_percent = Some(7);
        ui.battery.charge = ChargeState::Battery;
        assert_eq!(row_content(&ui, 5, 1).map(|r| r.value), Some(text("7%")));
        assert_eq!(level_text(10).as_str(), "10");
        assert_eq!(level_text(0).as_str(), "0");
    }
}
