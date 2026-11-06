//! Macropad face (`ui::faces::macropad`): the mockup scene is objective-clean,
//! a key flash repaints one key, scripted flashes / host pushes / touches
//! refresh partially to the full redraw, and no origin panics.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    control::{Button, Control, Edge, Touch, TouchPhase},
    deck::{
        demo::MACRO_FACE,
        msg::{Binding, HostMsg, MacroKey},
        text::Text,
    },
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

/// The mockup: demo keys with a host present, so the tag reads "deck.toml".
fn macropad() -> Box<UiState> {
    let mut ui = on(Template::Macropad);
    ui.deck.host_present = true;
    assert_eq!(ui.current_face().id, MACRO_FACE);
    ui
}

fn keys(present: u8, bound: u8, label: &str) -> [MacroKey; 9] {
    let bindings = [
        Binding::Left,
        Binding::Right,
        Binding::Enter,
        Binding::Cw,
        Binding::Ccw,
        Binding::ShiftLeft,
        Binding::ShiftRight,
        Binding::ShiftEnter,
        Binding::Left,
    ];
    let mut keys = [MacroKey::EMPTY; 9];
    for (index, key) in keys.iter_mut().enumerate() {
        key.present = (index as u8) < present;
        key.binding = if (index as u8) < bound { bindings[index] } else { Binding::None };
        key.label.set(&format!("{label}{index}"));
    }
    keys
}

fn macro_msg(face: u8, source: Option<&str>, keys: [MacroKey; 9]) -> HostMsg {
    HostMsg::Macro {
        face,
        source: source.map(|s| {
            let mut text = Text::new();
            text.set(s);
            text
        }),
        keys,
    }
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
    fn control(&mut self, control: Control) -> usize {
        self.step(|ui| {
            let mut out = Update::new();
            ui.apply(control, ui.now_ms, &mut out);
        })
    }
    fn touch(&mut self, phase: TouchPhase, x: i32, y: i32) -> usize {
        let (x, y) = orientation::point(x as u16, y as u16).unwrap();
        self.control(Control::Touch(Touch { phase, x, y }))
    }
    fn down(&mut self, mask: u16) -> usize {
        self.step(|ui| {
            ui.macropad.face = ui.current_face().id;
            ui.macropad.down = mask;
            ui.invalidate();
        })
    }
}

#[test]
fn the_mockup_scene_is_clean_fits_and_needs_no_ellipsis() {
    let ui = macropad();
    let frame = full(&ui);
    assert_eq!(lit_outside_safe_area(&frame), None);
    let bar = color(theme::HINT_FILL);
    let content: Vec<_> = lit_in(&frame, layout::HINT_BAR).filter(|&(x, y)| pixel(&frame, x, y) != bar).collect();
    assert!(content.len() > 300);
    assert!(content.iter().all(|&(x, _)| (32..448).contains(&x)));
    // Plain text, no keycaps.
    assert!(content.iter().all(|&(x, y)| pixel(&frame, x, y) != color(theme::KEYCAP_FILL)));
    let set = ui.deck.macro_set(MACRO_FACE).unwrap();
    let label_w = layout::macro_key(0).width() - 16;
    for key in set.keys.iter().filter(|key| key.present) {
        let w = font::measure(Font::Macro, key.label.as_str());
        assert!(w <= label_w, "{}: {w} of {label_w}", key.label.as_str());
    }
    assert!(font::measure(Font::Macro, "+ assign") <= label_w);
    assert!(font::measure(Font::Small, "deck.toml") <= 170);
    // Bound keys are solid cards with an outline; unbound ones are dashed on
    // canvas; the absent ninth is dashed too.
    for index in 0..9u8 {
        let rect = layout::macro_key(index);
        let (cx, cy) = rect.center();
        assert_eq!(layout::macro_key_at(cx, cy), Some(index));
        let key = set.keys[index as usize];
        // A keystroke or a hardware binding makes a key solid.
        let bound = key.present && (key.keys.is_some() || key.binding != Binding::None);
        let inside = pixel(&frame, rect.left() + 3, cy);
        assert_eq!(inside, color(if bound { theme::CARD_FILL } else { theme::CANVAS }), "key {index} fill");
        assert_eq!(pixel(&frame, rect.left(), cy), color(theme::OUTLINE_KEY), "key {index} left edge");
        let top: Vec<bool> = (rect.left() + 20..rect.right() - 20).map(|x| pixel(&frame, x, rect.top()) != 0).collect();
        if bound {
            assert!(top.iter().all(|&lit| lit), "key {index} solid top edge");
        } else {
            assert!(top.iter().any(|&lit| lit) && top.iter().any(|&lit| !lit), "key {index} dashed top edge");
        }
        // The label is centred: lit pixels on both sides of the centre line.
        let lane = Rect::new(rect.left() + 8, cy - 12, rect.width() - 16, 24);
        let label: Vec<_> = lit_in(&frame, lane).filter(|&(x, y)| pixel(&frame, x, y) != inside).collect();
        assert!(label.len() > 40, "key {index} label");
        let (min, max) = label.iter().fold((480, 0), |(lo, hi), &(x, _)| (lo.min(x), hi.max(x)));
        assert!((cx - min - (max - cx)).abs() <= 3, "key {index} label off-centre: {min}..{max} around {cx}");
        // Bound keys carry the binding glyph in the top-right corner.
        let corner = Rect::new(rect.right() - 70, rect.top() + 6, 62, 16);
        let glyph = lit_in(&frame, corner).filter(|&(x, y)| pixel(&frame, x, y) != inside).count();
        assert_eq!(glyph > 6, bound, "key {index} glyph: {glyph} px");
    }
}

#[test]
fn a_flash_changes_one_key_and_repaints_little_more_than_it() {
    let ui = macropad();
    let plain = full(&ui);
    for index in 0..9u8 {
        let mut pressed = Box::new((*ui).clone());
        pressed.macropad.face = MACRO_FACE;
        pressed.macropad.down = 1 << index;
        pressed.invalidate();
        let frame = full(&pressed);
        let rect = layout::macro_key(index);
        let mut changed = 0;
        for y in 0..480 {
            for x in 0..480 {
                if pixel(&frame, x, y) != pixel(&plain, x, y) {
                    assert!(rect.contains(x, y), "key {index}: ({x}, {y}) changed outside it");
                    changed += 1;
                }
            }
        }
        assert!(changed > 1000, "key {index} pressed look: {changed} px");
        assert_eq!(pixel(&frame, rect.left() + 3, rect.center().1), color(theme::CHIP_FILL));
        let mut screen = Screens::new(&ui);
        let transferred = update(&mut screen, &ui, &pressed);
        assert!(transferred <= (rect.width() + 2) as usize * (rect.height() + 2) as usize, "key {index}: {transferred} px");
        let released = update(&mut screen, &pressed, &ui);
        assert_eq!(released, transferred);
    }
    // The mask of another face's keys shows nothing here.
    let mut other = Box::new((*ui).clone());
    other.macropad.face = MACRO_FACE + 1;
    other.macropad.down = 0x1FF;
    other.invalidate();
    assert_frame(&full(&other), &plain, "another face's mask must not press these keys");
}

#[test]
fn the_signature_is_one_lane_per_key() {
    fn face_signature(ui: &UiState) -> Box<Signature> {
        let mut sig = Box::new(Signature::new());
        let mut builder = SigBuilder::new(&mut sig);
        faces::macropad::signature(ui, ui.current_face().id, &mut builder);
        sig
    }
    fn differing(a: &Signature, b: &Signature) -> Vec<usize> {
        assert_eq!(a.regions().len(), b.regions().len());
        (0..a.regions().len()).filter(|&i| a.regions()[i] != b.regions()[i]).collect()
    }
    let ui = macropad();
    let base = face_signature(&ui);
    assert_eq!(base.regions().len(), 11);
    for index in 0..9u8 {
        assert_eq!(base.regions()[1 + index as usize].0, layout::macro_key(index));
        let mut pressed = Box::new((*ui).clone());
        pressed.macropad.face = MACRO_FACE;
        pressed.macropad.down = 1 << index;
        pressed.invalidate();
        assert_eq!(differing(&base, &face_signature(&pressed)), [1 + index as usize]);
    }
    let mut relabelled = Box::new((*ui).clone());
    let mut out = Update::new();
    let mut keys = ui.deck.macro_set(MACRO_FACE).unwrap().keys;
    keys[4].label.set("shot");
    relabelled.host(&macro_msg(MACRO_FACE, None, keys), 0, &mut out);
    assert_eq!(differing(&base, &face_signature(&relabelled)), [5]);
    let mut renamed = Box::new((*ui).clone());
    renamed.host(&macro_msg(MACRO_FACE, Some("work.toml"), keys), 0, &mut out);
    assert_eq!(differing(&base, &face_signature(&renamed)), [0, 5]);
    let mut same = Box::new((*ui).clone());
    same.invalidate();
    assert_eq!(face_signature(&same).regions(), base.regions());
}

#[test]
fn a_face_without_a_set_shows_nine_assign_slots_and_no_tag() {
    let mut ui = macropad();
    ui.deck.macros[0].face = 0;
    ui.invalidate();
    let frame = full(&ui);
    assert_eq!(lit_outside_safe_area(&frame), None);
    let assign = full_draw(|band| faces::macropad::draw(band, &ui, 77));
    assert_eq!(lit_in(&frame, Rect::new(300, 30, 150, 30)).count(), 0, "no source tag");
    for index in 0..9u8 {
        let rect = layout::macro_key(index);
        assert_eq!(pixel(&frame, rect.left() + 3, rect.center().1), 0, "key {index} on canvas");
        let a: Vec<_> = lit_in(&frame, rect).collect();
        let b: Vec<_> = lit_in(&assign, rect).collect();
        assert_eq!(a, b, "key {index} is the same + assign slot as any unknown face's");
    }
}

#[test]
fn partial_refresh_matches_full_through_flashes_pushes_and_touches() {
    let mut session = Session::new(macropad());
    for index in 0..9 {
        let flash = session.down(1 << index);
        assert!(flash <= 134 * 110, "key {index} flash moved {flash} px");
        session.down(0);
    }
    session.down(0b1_0101_0101);
    session.down(0x1FF);
    session.down(0);
    // Hardware presses and releases of every bindable control.
    for button in [Button::Left, Button::Right, Button::Enter, Button::Shift, Button::Cancel] {
        session.control(Control::Button(button, Edge::Press));
        session.control(Control::Button(button, Edge::Release));
    }
    // Touch down / move / up and a cancel on every key.
    for index in 0..9u8 {
        let (x, y) = layout::macro_key(index).center();
        session.touch(TouchPhase::Down, x, y);
        session.touch(TouchPhase::Move, x + 4, y + 3);
        session.touch(TouchPhase::Up, x + 4, y + 3);
        session.touch(TouchPhase::Down, x, y);
        session.touch(TouchPhase::Cancel, x, y);
    }
    session.host(macro_msg(MACRO_FACE, Some("work.toml"), keys(9, 9, "k")));
    session.down(1 << 8);
    session.host(macro_msg(MACRO_FACE, None, keys(5, 2, "a very long key label")));
    session.host(macro_msg(MACRO_FACE + 1, Some("other"), keys(9, 9, "o")));
    session.host(macro_msg(MACRO_FACE, Some(""), keys(0, 0, "")));
    session.step(|ui| {
        ui.macropad.face = MACRO_FACE + 1;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.update_battery(BatteryState { estimated_percent: Some(3), charge: ChargeState::Charging, ..BatteryState::UNKNOWN });
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

#[test]
fn every_origin_draws_the_face_shifted_and_never_panics() {
    let mut ui = macropad();
    let mut out = Update::new();
    ui.host(&macro_msg(MACRO_FACE, Some("a source that is far too long"), keys(8, 5, "label \u{e9}\u{2014}")), 0, &mut out);
    ui.macropad.face = MACRO_FACE;
    ui.macropad.down = 0b1_0010_0100;
    let id = ui.current_face().id;
    let reference = full_draw(|band| faces::macropad::draw(band, &ui, id));
    for dx in -480..=480 {
        let frame = full_draw(|band| {
            band.set_origin_x(dx);
            faces::macropad::draw(band, &ui, id);
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
