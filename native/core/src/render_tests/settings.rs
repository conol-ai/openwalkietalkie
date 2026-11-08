//! Settings face (`ui::faces::settings`): every panel fits and stays inside
//! the arcs, the outlined tab and row are the ones `layout` hit-tests, host
//! side rows dim without a host, partial refresh equals a full redraw through
//! a script of panel/row/value/host/battery changes, and no origin panics.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    control::{Button, Control, Edge, Touch, TouchPhase, Wheel},
    deck::msg::{HostMsg, SettingKey},
    ui::{
        faces::settings::{needs_host, row_content, Value},
        font::{self, Font},
        kit, theme, Rect, Update,
    },
};
use crate::ui::state::settings::{rows, viewport, LANGUAGE_ROW, PANELS, TUTORIAL_ROW};

fn pixel(frame: &[u8], x: i32, y: i32) -> u16 {
    let at = (y as usize * 480 + x as usize) * 2;
    u16::from_le_bytes([frame[at], frame[at + 1]])
}

fn rgb(color: u32) -> u16 {
    kit::rgb(color).into_storage()
}

fn differs_in(a: &[u8], b: &[u8], rect: Rect) -> bool {
    (rect.top()..rect.bottom()).any(|y| (rect.left()..rect.right()).any(|x| pixel(a, x, y) != pixel(b, x, y)))
}

/// The pixels that differ between two frames, as a bounding box.
fn diff_box(a: &[u8], b: &[u8]) -> Rect {
    let mut bounds = Rect::EMPTY;
    for y in 0..480 {
        for x in 0..480 {
            if pixel(a, x, y) != pixel(b, x, y) {
                bounds = bounds.union(&Rect::new(x, y, 1, 1));
            }
        }
    }
    bounds
}

fn settings(panel: u8, row: u8) -> Box<UiState> {
    let mut ui = on(Template::Settings);
    ui.deck.host_present = true;
    ui.settings.panel = panel;
    ui.settings.row = row;
    ui.invalidate();
    ui
}

#[test]
fn every_panel_fits_its_strings_and_stays_inside_the_arcs() {
    // Every tab name fits its chip in the one face the strip uses.
    let tab_font = crate::ui::faces::settings::tab_font();
    for name in crate::ui::state::settings::PANEL_NAMES {
        assert!(font::measure(tab_font, name) <= layout::settings_tab(0).width() - 8, "{name}");
    }
    for panel in 0..PANELS {
        for selected in 0..rows(panel) {
            let frame = full(&settings(panel, selected));
            assert_eq!(lit_outside_safe_area(&frame), None, "panel {panel}, row {selected}");
        }
        let ui = settings(panel, 0);
        for row in 0..rows(panel) {
            let content = row_content(&ui, panel, row).unwrap();
            let label_room = layout::SETTING_BAR.left() - layout::SETTING_LABEL_X - 8;
            assert!(font::measure(Font::Row, content.label) <= label_room, "{}", content.label);
            if let Value::Text(text) = content.value {
                assert!(font::measure(Font::Row, text.as_str()) <= 210, "{}", text.as_str());
            }
        }
    }
    // The hint bar keeps all three hints, between x 32 and 448.
    let frame = full(&settings(0, 0));
    let bar = rgb(theme::HINT_FILL);
    for y in layout::HINT_CONTENT.top()..layout::HINT_CONTENT.bottom() {
        for x in 0..480 {
            if pixel(&frame, x, y) != bar {
                assert!((32..448).contains(&x), "hint content at x {x}");
            }
        }
    }
    let hints = [
        kit::Hint { key: "\u{2039} \u{203a}", label: "panel" },
        kit::Hint { key: "ENTER", label: "row" },
        kit::Hint { key: "SCROLL", label: "adjust" },
    ];
    assert_eq!(kit::hints_that_fit(&hints).0, 3);
}

#[test]
fn the_outlined_tab_and_row_are_the_ones_layout_hit_tests() {
    for panel in 0..PANELS {
        for row in 0..rows(panel) {
            let ui = settings(panel, row);
            let view = viewport(&ui);
            let frame = full(&ui);
            for index in 0..layout::SETTINGS_PANELS {
                let tab = layout::settings_tab(index);
                let (cx, cy) = tab.center();
                assert_eq!(layout::settings_tab_at(cx, cy), Some(index));
                let edge = pixel(&frame, tab.left(), cy);
                let expected = if index == panel { theme::OUTLINE_SELECTED } else { theme::OUTLINE };
                assert_eq!(edge, rgb(expected), "panel {panel} tab {index}");
            }
            for (slot, index) in view.slots() {
                let rect = layout::settings_row(slot);
                let (cx, cy) = rect.center();
                assert_eq!(layout::settings_row_at(cx, cy), Some(slot));
                let expected = if index == row { theme::OUTLINE_SELECTED } else { theme::OUTLINE };
                assert_eq!(pixel(&frame, rect.left(), cy), rgb(expected), "panel {panel} row {index}");
                // The focused row is filled, the others show canvas.
                let fill = if index == row { theme::CARD_FILL } else { theme::CANVAS };
                assert_eq!(pixel(&frame, cx, rect.top() + 6), rgb(fill));
                // Input maps the physical lane back to its logical row,
                // including the bottom lane after Display scrolls to Tutorial.
                let mut touched = ui.clone();
                let mut out = Update::new();
                for (phase, at) in [(TouchPhase::Down, 1000), (TouchPhase::Up, 1001)] {
                    touched.apply(Control::Touch(Touch { phase, x: cx as u16, y: cy as u16 }), at, &mut out);
                }
                assert_eq!(touched.settings.row, index, "panel {panel}, slot {slot}");
            }
            // Rows the panel does not have are not drawn.
            for slot in 0..layout::SETTINGS_ROWS {
                if view.index_at(slot).is_some() { continue; }
                let rect = layout::settings_row(slot);
                assert_eq!(pixel(&frame, rect.left(), rect.center().1), 0);
            }
        }
    }
}

#[test]
fn a_touched_tab_or_row_shows_the_pressed_fill_only_there() {
    let base = settings(2, 0);
    let plain = full(&base);
    let mut pressed = base.clone();
    pressed.settings.pressed = Some(4);
    pressed.invalidate();
    let frame = full(&pressed);
    let tab = layout::settings_tab(4);
    assert_eq!(tab.intersection(&diff_box(&plain, &frame)), diff_box(&plain, &frame));
    assert_eq!(pixel(&frame, tab.center().0, tab.top() + 4), rgb(theme::CHIP_FILL));
    let mut row = base.clone();
    row.settings.pressed = Some(8 + 1);
    row.invalidate();
    let frame = full(&row);
    let rect = layout::settings_row(1);
    assert_eq!(rect.intersection(&diff_box(&plain, &frame)), diff_box(&plain, &frame));
    assert_eq!(pixel(&frame, rect.center().0, rect.top() + 6), rgb(theme::CHIP_FILL));
    let mut screens = Screens::new(&base);
    update(&mut screens, &base, &pressed);
    update(&mut screens, &pressed, &row);
    update(&mut screens, &row, &base);
}

#[test]
fn host_side_rows_dim_to_needs_host_without_a_host_or_demo() {
    for panel in 0..PANELS {
        let hosted = settings(panel, 0);
        let mut alone = hosted.clone();
        alone.deck.host_present = false;
        alone.deck.settings.demo = false;
        alone.invalidate();
        assert!(needs_host(&alone) && !needs_host(&hosted));
        let (a, b) = (full(&hosted), full(&alone));
        // The Host panel's "Deck host" and "Demo data" rows report the change itself.
        let reports = |row: u8| panel == 4 && (row == 1 || row == 2);
        for (slot, row) in viewport(&alone).slots() {
            let rect = layout::settings_row(slot);
            let dims = row_content(&alone, panel, row).map(|r| r.value) == Some(Value::NeedsHost);
            assert_eq!(differs_in(&a, &b, rect), dims || reports(row), "panel {panel} row {row}");
            if dims {
                // The bar / toggle is gone: the value area holds only text.
                let bar = layout::SETTING_BAR.offset(rect.left(), rect.top());
                let fill = if row == 0 { theme::CARD_FILL } else { theme::CANVAS };
                assert_eq!(pixel(&b, bar.left() + 2, bar.center().1), rgb(fill));
            }
        }
        // The demo host stands in: nothing dims.
        let mut demo = alone.clone();
        demo.deck.settings.demo = true;
        demo.invalidate();
        let c = full(&demo);
        for (slot, row) in viewport(&demo).slots() {
            let rect = layout::settings_row(slot);
            assert_eq!(differs_in(&a, &c, rect), panel == 4 && row == 1, "demo panel {panel} row {row}");
        }
    }
}

#[test]
fn partial_refresh_tracks_the_display_viewport_language_adjustment_and_tutorial() {
    let mut ui = settings(LANGUAGE_ROW.0, LANGUAGE_ROW.1);
    let mut screens = Screens::new(&ui);
    let step = |ui: &mut Box<UiState>, screens: &mut Screens, control: Control, at| {
        let before = ui.clone();
        ui.apply(control, at, &mut Update::new());
        update(screens, &before, ui);
    };
    assert_eq!(viewport(&ui).top, 0);
    step(&mut ui, &mut screens, Control::Wheel(Wheel::Ccw), 1000);
    assert_eq!(ui.settings.row, TUTORIAL_ROW.1);
    assert_eq!(viewport(&ui).top, 1);
    assert_eq!(viewport(&ui).index_at(3), Some(TUTORIAL_ROW.1));
    step(&mut ui, &mut screens, Control::Wheel(Wheel::Cw), 1010);
    assert_eq!(ui.settings.row, LANGUAGE_ROW.1);
    assert_eq!(viewport(&ui).top, 0);
    step(&mut ui, &mut screens, Control::Button(Button::Enter, Edge::Press), 1020);
    step(&mut ui, &mut screens, Control::Button(Button::Enter, Edge::Release), 1021);
    assert!(ui.settings.adjusting);
    step(&mut ui, &mut screens, Control::Wheel(Wheel::Cw), 1030);
    assert_eq!(ui.language, crate::ui::i18n::Language::SimplifiedChinese);
    step(&mut ui, &mut screens, Control::Button(Button::Enter, Edge::Press), 1040);
    step(&mut ui, &mut screens, Control::Button(Button::Enter, Edge::Release), 1041);
    assert!(!ui.settings.adjusting);
    step(&mut ui, &mut screens, Control::Wheel(Wheel::Ccw), 1050);
    assert_eq!(viewport(&ui).top, 1);
    step(&mut ui, &mut screens, Control::Button(Button::Enter, Edge::Press), 1060);
    assert_eq!(ui.global.overlay, crate::ui::state::Overlay::Tutorial);
    assert!(ui.tutorial.replay);
}

#[test]
fn partial_refresh_matches_full_through_panels_rows_values_and_host_state() {
    let mut ui = settings(0, 0);
    let mut screens = Screens::new(&ui);
    let mut out = Update::new();
    let mut now = 1000;
    let step = |ui: &mut Box<UiState>, screens: &mut Screens, change: &mut dyn FnMut(&mut UiState)| {
        let before = ui.clone();
        change(ui);
        update(screens, &before, ui)
    };
    for _ in 0..PANELS {
        step(&mut ui, &mut screens, &mut |ui| {
            now += 200;
            ui.apply(Control::Button(Button::Right, Edge::Press), now, &mut out);
            ui.apply(Control::Button(Button::Right, Edge::Release), now + 50, &mut out);
        });
    }
    step(&mut ui, &mut screens, &mut |ui| {
        ui.settings.panel = 0;
        ui.settings.row = 1;
        ui.invalidate();
    });
    // A value change repaints its row, not the frame.
    let moved = step(&mut ui, &mut screens, &mut |ui| {
        ui.host(&HostMsg::Set { key: SettingKey::Mic, value: 2 }, now, &mut out);
    });
    assert!(moved < 480 * 480 / 4, "{moved} pixels for one row");
    for (panel, key, value) in [
        (3, SettingKey::Brightness, 1),
        (3, SettingKey::IdleHome, 0),
        (2, SettingKey::Chime, 0),
        (2, SettingKey::AutoJump, 1),
        (1, SettingKey::Announce, 0),
        (4, SettingKey::Demo, 1),
    ] {
        step(&mut ui, &mut screens, &mut |ui| {
            ui.settings.panel = panel;
            ui.invalidate();
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.host(&HostMsg::Set { key, value }, now, &mut out);
        });
    }
    for panel in [0, 4, 5] {
        step(&mut ui, &mut screens, &mut |ui| {
            ui.settings.panel = panel;
            ui.invalidate();
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.deck.host_present = false;
            ui.deck.settings.demo = false;
            ui.invalidate();
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.set_connected(true, &mut out);
            ui.deck.syncing = true;
            ui.invalidate();
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.update_battery(BatteryState {
                estimated_percent: Some(9),
                charge: ChargeState::Battery,
                ..BatteryState::UNKNOWN
            });
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.update_battery(BatteryState {
                estimated_percent: Some(100),
                charge: ChargeState::Charging,
                ..BatteryState::UNKNOWN
            });
        });
        step(&mut ui, &mut screens, &mut |ui| {
            ui.deck.host_present = true;
            ui.deck.settings.demo = true;
            ui.invalidate();
        });
    }
}

#[test]
fn a_low_battery_takes_the_header_tag_on_the_face_and_shows_in_about() {
    let mut ui = settings(5, 1);
    let plain = full(&ui);
    ui.update_battery(BatteryState {
        estimated_percent: Some(9),
        charge: ChargeState::Battery,
        ..BatteryState::UNKNOWN
    });
    let low = full(&ui);
    assert!(differs_in(&plain, &low, layout::HEADER) && differs_in(&plain, &low, layout::settings_row(1)));
    assert!(!differs_in(&plain, &low, layout::settings_row(0)) && !differs_in(&plain, &low, layout::settings_row(2)));
    // The tag is amber, right-aligned at the header's right edge.
    let amber = (layout::HEADER.top()..layout::HEADER.bottom())
        .flat_map(|y| (380..theme::TEXT_RIGHT).map(move |x| (x, y)))
        .any(|(x, y)| pixel(&low, x, y) == rgb(theme::AMBER));
    assert!(amber, "amber battery tag");
}

#[test]
fn the_settings_face_draws_at_every_origin_and_clip_without_panicking() {
    let mut states = Vec::new();
    for panel in 0..PANELS {
        let mut ui = settings(panel, rows(panel) - 1);
        ui.settings.pressed = Some(if panel % 2 == 0 { panel } else { 8 });
        ui.deck.host_present = panel % 3 != 0;
        ui.deck.settings.demo = panel % 3 == 1;
        states.push(ui);
    }
    let mut band = display::Band::new();
    for ui in &states {
        let face = ui.current_face();
        for origin in -480i32..=480 {
            let bands: &[u16] = if origin % 8 == 0 { &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9] } else { &[1, 2, 4] };
            for &index in bands {
                band.set_row(index);
                if origin % 5 == 0 {
                    band.clip_to(Rectangle::new(Point::new((origin + 480) / 4, index as i32 * 48 + 3), Size::new(61, 17)));
                }
                band.set_origin_x(origin);
                scene::draw_face(&mut band, ui, face);
            }
        }
    }
}
