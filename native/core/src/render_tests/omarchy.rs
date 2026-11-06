//! Omarchy face (`ui::faces::omarchy`): the mockup scene is objective-clean,
//! every scripted change refreshes partially to the full redraw, the lanes
//! are fine-grained, the viewport markers appear, and no origin panics.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    control::{Control, Touch, TouchPhase, Wheel},
    deck::msg::{HostMsg, Tone, WindowRow},
    ui::{
        faces,
        font::{self, Font},
        kit,
        signature::SigBuilder,
        theme, Rect, Update,
    },
};

fn pixel(frame: &[u8], x: i32, y: i32) -> u16 {
    let at = (y as usize * 480 + x as usize) * 2;
    u16::from_le_bytes([frame[at], frame[at + 1]])
}

fn lit_in(frame: &[u8], rect: Rect) -> impl Iterator<Item = (i32, i32)> + '_ {
    (rect.top()..rect.bottom())
        .flat_map(move |y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .filter(move |&(x, y)| pixel(frame, x, y) != 0)
}

fn color(c: u32) -> u16 {
    kit::rgb(c).into_storage()
}

/// The mockup: demo data with a host present, so the tag reads "remote".
fn omarchy() -> Box<UiState> {
    let mut ui = on(Template::Omarchy);
    ui.deck.host_present = true;
    ui
}

fn windows(count: u8, active: u8, chips: u8) -> HostMsg {
    let mut windows = [WindowRow::EMPTY; 6];
    let tones = [Tone::Blue, Tone::Amber, Tone::Green, Tone::Red, Tone::Gray, Tone::Blue];
    for (index, window) in windows.iter_mut().enumerate().take(count as usize) {
        window.tone = tones[index];
        window.title.set(&format!("window {index} \u{2014} title"));
    }
    HostMsg::Workspaces { active, count: chips, selected: 0, windows, window_count: count }
}

struct Session {
    previous: Box<UiState>,
    next: Box<UiState>,
    screen: Screens,
}

impl Session {
    fn new(ui: Box<UiState>) -> Self {
        let screen = Screens::new(&ui);
        Self { previous: Box::new((*ui).clone()), next: ui, screen }
    }
    /// Apply `f`, replay the damage, assert partial == full; pixels moved.
    fn step(&mut self, f: impl FnOnce(&mut UiState)) -> usize {
        f(&mut self.next);
        let transferred = update(&mut self.screen, &self.previous, &self.next);
        self.next.copy_into(&mut self.previous);
        transferred
    }
    fn host(&mut self, msg: HostMsg) -> usize {
        self.step(|ui| {
            let mut out = Update::new();
            ui.host(&msg, ui.now_ms, &mut out);
        })
    }
    fn touch(&mut self, phase: TouchPhase, x: i32, y: i32) -> usize {
        self.step(|ui| {
            let mut out = Update::new();
            let (x, y) = orientation::point(x as u16, y as u16).unwrap();
            ui.apply(Control::Touch(Touch { phase, x, y }), ui.now_ms, &mut out);
        })
    }
}

#[test]
fn the_mockup_scene_is_clean_fits_and_needs_no_ellipsis() {
    let ui = omarchy();
    let frame = full(&ui);
    assert_eq!(lit_outside_safe_area(&frame), None);
    // Hint content stays inside x in [32, 448); the bar has content.
    let bar = color(theme::HINT_FILL);
    let content: Vec<_> = lit_in(&frame, layout::HINT_BAR).filter(|&(x, y)| pixel(&frame, x, y) != bar).collect();
    assert!(content.len() > 300);
    assert!(content.iter().all(|&(x, _)| (32..448).contains(&x)));
    // Every string of the scene fits the width it is drawn in (the rule
    // `font::draw` applies before it appends "…").
    assert!(font::measure(Font::Display, "Workspace 2") <= layout::OMARCHY_TITLE.width());
    let text_w = layout::omarchy_window(0).width() - layout::WINDOW_TEXT_X - 10;
    for window in &ui.deck.workspaces.windows[..3] {
        let w = font::measure(Font::Row, window.title.as_str());
        assert!(w <= text_w, "{}: {w} of {text_w}", window.title.as_str());
    }
    assert!(font::measure(Font::Small, "remote") <= 170);
    // The title, the WINDOWS label and the four chips are there.
    assert!(lit_in(&frame, layout::OMARCHY_TITLE).count() > 500);
    assert!(lit_in(&frame, layout::OMARCHY_WINDOWS_LABEL).count() > 50);
    for index in 0..4 {
        assert!(lit_in(&frame, layout::omarchy_chip(index)).count() > 30, "chip {index}");
    }
    assert_eq!(lit_in(&frame, layout::omarchy_chip(4)).count(), 0);
    // Window 0 is selected: the card outline is bright on its edge midpoints.
    let row = layout::omarchy_window(0);
    let (cx, cy) = row.center();
    assert_eq!(pixel(&frame, row.left(), cy), color(theme::OUTLINE_SELECTED));
    assert_eq!(pixel(&frame, cx, row.top()), color(theme::OUTLINE_SELECTED));
    assert_eq!(pixel(&frame, cx, row.bottom() - 1), color(theme::OUTLINE_SELECTED));
    // Idle rows have no outline but a tone square.
    let idle = layout::omarchy_window(1);
    assert_eq!(pixel(&frame, idle.left(), idle.center().1), 0);
    let square = layout::WINDOW_SQUARE.offset(idle.left(), idle.top());
    assert_eq!(pixel(&frame, square.center().0, square.center().1), color(theme::AMBER));
    // Nothing between the last row and the hint bar (no marker for 3 rows).
    assert_eq!(lit_in(&frame, Rect::new(0, 396, 480, 31)).count(), 0);
}

#[test]
fn every_layout_target_is_drawn_where_it_hit_tests() {
    let mut ui = omarchy();
    ui.omarchy.selected_window = 2;
    let frame = full(&ui);
    for index in 0..layout::OMARCHY_CHIPS {
        let chip = layout::omarchy_chip(index);
        let (cx, cy) = chip.center();
        assert_eq!(layout::omarchy_chip_at(cx, cy), Some(index));
        if index < 4 {
            let outline = if index == 1 { theme::OUTLINE_SELECTED } else { theme::OUTLINE };
            assert_eq!(pixel(&frame, chip.left(), cy), color(outline), "chip {index}");
            assert_eq!(pixel(&frame, cx, chip.top()), color(outline), "chip {index}");
        }
    }
    for slot in 0..layout::OMARCHY_VISIBLE {
        let row = layout::omarchy_window(slot);
        let (cx, cy) = row.center();
        assert_eq!(layout::omarchy_window_at(cx, cy), Some(slot));
        if slot == 2 {
            assert_eq!(pixel(&frame, row.left(), cy), color(theme::OUTLINE_SELECTED));
        } else {
            assert_eq!(pixel(&frame, row.left(), cy), 0);
        }
    }
}

#[test]
fn pressed_chip_and_row_change_pixels_inside_their_rects_only() {
    let ui = omarchy();
    let plain = full(&ui);
    let mut pressed = Box::new((*ui).clone());
    pressed.omarchy.pressed_chip = Some(2);
    pressed.omarchy.pressed_row = Some(1);
    pressed.invalidate();
    let frame = full(&pressed);
    let chip = layout::omarchy_chip(2);
    let row = layout::omarchy_window(1);
    let mut changed = (0, 0);
    for y in 0..480 {
        for x in 0..480 {
            if pixel(&frame, x, y) != pixel(&plain, x, y) {
                if chip.contains(x, y) {
                    changed.0 += 1;
                } else if row.contains(x, y) {
                    changed.1 += 1;
                } else {
                    panic!("({x}, {y}) changed outside the pressed targets");
                }
            }
        }
    }
    assert!(changed.0 > 100 && changed.1 > 1000, "{changed:?}");
    assert_eq!(pixel(&frame, chip.left() + 3, chip.center().1), color(theme::CHIP_FILL));
    assert_eq!(pixel(&frame, row.left() + 40, row.top() + 4), color(theme::CHIP_FILL));
    // Pressing the selected row keeps the bright outline.
    pressed.omarchy.pressed_row = Some(0);
    pressed.invalidate();
    let frame = full(&pressed);
    let row = layout::omarchy_window(0);
    assert_eq!(pixel(&frame, row.left(), row.center().1), color(theme::OUTLINE_SELECTED));
    assert_eq!(pixel(&frame, row.left() + 4, row.center().1), color(theme::CHIP_FILL));
}

#[test]
fn viewport_markers_count_the_hidden_windows() {
    let mut ui = omarchy();
    let mut out = Update::new();
    ui.host(&windows(6, 2, 4), 0, &mut out);
    let below = Rect::new(240, 396, 208, 30);
    let above = Rect::new(240, 176, 208, 24);
    let frame = full(&ui);
    assert!(lit_in(&frame, below).count() > 40, "2 more \u{2193}");
    assert_eq!(lit_in(&frame, above).count(), 0);
    ui.omarchy.top_row = 2;
    ui.omarchy.selected_window = 5;
    ui.invalidate();
    let frame = full(&ui);
    assert_eq!(lit_in(&frame, below).count(), 0);
    assert!(lit_in(&frame, above).count() > 40, "\u{2191} 2 more");
    // Slot 3 now shows window 5, selected.
    let row = layout::omarchy_window(3);
    assert_eq!(pixel(&frame, row.left(), row.center().1), color(theme::OUTLINE_SELECTED));
    assert_eq!(lit_outside_safe_area(&frame), None);
    ui.omarchy.top_row = 1;
    ui.invalidate();
    let frame = full(&ui);
    assert!(lit_in(&frame, below).count() > 40 && lit_in(&frame, above).count() > 40);
}

#[test]
fn an_empty_workspace_list_reads_no_windows_and_a_missing_host_reads_offline() {
    let mut ui = omarchy();
    let mut out = Update::new();
    ui.host(&windows(0, 0, 0), 0, &mut out);
    let frame = full(&ui);
    assert!(lit_in(&frame, layout::omarchy_window(0)).count() > 100, "no windows");
    assert_eq!(lit_in(&frame, layout::omarchy_window(1)).count(), 0);
    assert_eq!(lit_in(&frame, layout::omarchy_chip(0)).count(), 0);
    assert!(lit_in(&frame, layout::OMARCHY_TITLE).count() > 300, "Workspace \u{2014}");
    assert_eq!(lit_outside_safe_area(&frame), None);
    let remote = full(&omarchy());
    ui = omarchy();
    ui.deck.settings.demo = false;
    ui.deck.host_present = false;
    ui.invalidate();
    let offline = full(&ui);
    assert!(lit_in(&remote, layout::HEADER).zip(lit_in(&offline, layout::HEADER)).any(|(a, b)| a != b));
    // A low battery displaces the tag with an amber percentage.
    ui.update_battery(BatteryState { estimated_percent: Some(9), charge: ChargeState::Battery, ..BatteryState::UNKNOWN });
    let low = full(&ui);
    let tag = Rect::new(300, 34, 142, 24);
    assert!(lit_in(&low, tag).any(|(x, y)| pixel(&low, x, y) == color(theme::AMBER)));
    assert!(lit_in(&remote, tag).all(|(x, y)| pixel(&remote, x, y) != color(theme::AMBER)));
}

#[test]
fn partial_refresh_matches_full_through_a_scripted_session() {
    let mut session = Session::new(omarchy());
    // A wheel step (optimistic highlight): two rows, nothing else.
    let moved = session.step(|ui| {
        ui.omarchy.selected_window = 1;
        ui.invalidate();
    });
    assert!(moved <= 2 * 416 * 48 + 2 * 480, "{moved} px for a highlight move");
    session.step(|ui| {
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Ccw), 100, &mut out);
    });
    session.host(windows(6, 3, 4));
    session.step(|ui| {
        ui.omarchy.top_row = 2;
        ui.omarchy.selected_window = 5;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.omarchy.top_row = 1;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.omarchy.pressed_chip = Some(3);
        ui.invalidate();
    });
    session.step(|ui| {
        ui.omarchy.pressed_chip = None;
        ui.omarchy.pressed_row = Some(2);
        ui.invalidate();
    });
    session.step(|ui| {
        ui.omarchy.pressed_row = None;
        ui.invalidate();
    });
    // Touches through the recogniser at every chip and row centre.
    for index in 0..layout::OMARCHY_CHIPS {
        let (x, y) = layout::omarchy_chip(index).center();
        session.touch(TouchPhase::Down, x, y);
        session.touch(TouchPhase::Up, x, y);
    }
    for slot in 0..layout::OMARCHY_VISIBLE {
        let (x, y) = layout::omarchy_window(slot).center();
        session.touch(TouchPhase::Down, x, y);
        session.touch(TouchPhase::Move, x, y + 40);
        session.touch(TouchPhase::Up, x, y + 40);
    }
    session.host(windows(4, 9, 9));
    session.host(windows(0, 0, 0));
    session.host(windows(3, 2, 4));
    session.step(|ui| {
        ui.update_battery(BatteryState { estimated_percent: Some(9), charge: ChargeState::Battery, ..BatteryState::UNKNOWN });
        ui.invalidate();
    });
    session.step(|ui| {
        ui.update_battery(BatteryState { estimated_percent: Some(80), charge: ChargeState::Battery, ..BatteryState::UNKNOWN });
        ui.invalidate();
    });
    session.step(|ui| {
        ui.deck.settings.demo = false;
        ui.deck.host_present = false;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.deck.host_present = true;
        ui.invalidate();
    });
}

/// The face's own regions (the frame signature appends the overlays').
fn face_signature(ui: &UiState) -> Box<Signature> {
    let mut sig = Box::new(Signature::new());
    let mut builder = SigBuilder::new(&mut sig);
    faces::omarchy::signature(ui, ui.current_face().id, &mut builder);
    sig
}

fn differing(a: &Signature, b: &Signature) -> Vec<usize> {
    assert_eq!(a.regions().len(), b.regions().len());
    (0..a.regions().len()).filter(|&i| a.regions()[i] != b.regions()[i]).collect()
}

#[test]
fn the_signature_is_fixed_lanes_that_change_one_at_a_time() {
    let ui = omarchy();
    let base = face_signature(&ui);
    assert_eq!(base.regions().len(), 10);
    assert!(!base.overflowed());
    let mut moved = Box::new((*ui).clone());
    moved.omarchy.selected_window = 1;
    moved.invalidate();
    assert_eq!(differing(&base, &face_signature(&moved)), [4, 5], "window rows 0 and 1");
    let mut pressed = Box::new((*ui).clone());
    pressed.omarchy.pressed_chip = Some(0);
    pressed.invalidate();
    assert_eq!(differing(&base, &face_signature(&pressed)), [2], "the chip row");
    let mut low = Box::new((*ui).clone());
    low.update_battery(BatteryState { estimated_percent: Some(5), charge: ChargeState::Battery, ..BatteryState::UNKNOWN });
    assert_eq!(differing(&base, &face_signature(&low)), [0], "the header");
    let mut scrolled = Box::new((*ui).clone());
    let mut out = Update::new();
    scrolled.host(&windows(6, 2, 4), 0, &mut out);
    scrolled.omarchy.top_row = 2;
    assert_eq!(differing(&base, &face_signature(&scrolled)), [3, 4, 5, 6, 7], "WINDOWS line and rows");
    // A bump of `revision` alone changes nothing: the lanes hash content.
    let mut same = Box::new((*ui).clone());
    same.invalidate();
    assert_eq!(face_signature(&same).regions(), base.regions());
}

#[test]
fn every_origin_draws_the_face_shifted_and_never_panics() {
    let mut ui = omarchy();
    let mut out = Update::new();
    ui.host(&windows(6, 12, 9), 0, &mut out);
    ui.omarchy.top_row = 1;
    ui.omarchy.pressed_chip = Some(8);
    ui.omarchy.pressed_row = Some(3);
    let id = ui.current_face().id;
    let reference = full_draw(|band| faces::omarchy::draw(band, &ui, id));
    for dx in -480..=480 {
        let frame = full_draw(|band| {
            band.set_origin_x(dx);
            faces::omarchy::draw(band, &ui, id);
        });
        if ![-480, -333, -1, 0, 1, 77, 479, 480].contains(&dx) {
            continue;
        }
        for y in (0..480).step_by(3) {
            for x in 0..480 {
                let expected = if (0..480).contains(&(x - dx)) { pixel(&reference, x - dx, y) } else { 0 };
                assert_eq!(pixel(&frame, x, y), expected, "origin {dx} at ({x}, {y})");
            }
        }
    }
}
