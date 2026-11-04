//! Omarchy, Macropad, Usage, Settings and face-manager grammar (`ui::state`),
//! driven through `UiState::{apply, host, tick}`. Most manager tests enter
//! the overlays by setting `global.overlay` directly; double-SHIFT itself is
//! covered in `grammar.rs`.
use crate::{
    control::{Button, Control, Edge, Touch, TouchPhase, Wheel},
    deck::demo::MACRO_FACE,
    deck::msg::*,
    deck::text::Text,
    ui::state::{manager::GALLERY, settings, Overlay},
    ui::{layout, UiState, Update},
};

const PRESS: Edge = Edge::Press;
const RELEASE: Edge = Edge::Release;

fn fresh() -> Box<UiState> {
    let mut ui = Box::new(UiState::new());
    ui.reset();
    // Retain coverage of restored pre-Arcade layouts. New defaults and Arcade
    // controls are exercised in ui::state::arcade::tests.
    ui.global.strip[4] = FaceEntry::new(5, Template::Settings, "Settings");
    ui.global.strip_len = 5;
    // These tests start from the demo session (off at power-on).
    ui.load_demo();
    ui
}

fn on(template: Template) -> Box<UiState> {
    let mut ui = fresh();
    ui.on_face(template);
    ui
}

fn events(out: &Update) -> Vec<Event> {
    out.deck_events().collect()
}

fn button(ui: &mut UiState, button: Button, edge: Edge, at_ms: u32) -> Update {
    let mut out = Update::new();
    ui.apply(Control::Button(button, edge), at_ms, &mut out);
    out
}

fn press(ui: &mut UiState, b: Button, at_ms: u32) -> Update {
    button(ui, b, PRESS, at_ms)
}

fn wheel(ui: &mut UiState, wheel: Wheel, at_ms: u32) -> Update {
    let mut out = Update::new();
    ui.apply(Control::Wheel(wheel), at_ms, &mut out);
    out
}

fn touch(ui: &mut UiState, phase: TouchPhase, (x, y): (i32, i32), at_ms: u32) -> Update {
    let mut out = Update::new();
    let touch = Touch { phase, x: x as u16, y: y as u16 };
    ui.apply(Control::Touch(touch), at_ms, &mut out);
    out
}

/// Down then Up on the same point: one tap.
fn tap(ui: &mut UiState, at: (i32, i32), at_ms: u32) -> Vec<Event> {
    let mut all = events(&touch(ui, TouchPhase::Down, at, at_ms));
    all.extend(events(&touch(ui, TouchPhase::Up, at, at_ms + 80)));
    all
}

fn host(ui: &mut UiState, msg: &HostMsg, at_ms: u32) -> Update {
    let mut out = Update::new();
    ui.host(msg, at_ms, &mut out);
    out
}

fn tick(ui: &mut UiState, at_ms: u32) -> Update {
    let mut out = Update::new();
    ui.tick(at_ms, 0, true, &mut out);
    out
}

fn ids(ui: &UiState) -> Vec<u8> {
    (0..ui.global.strip_len).map(|i| ui.face_at(i).unwrap().id).collect()
}

fn faces_msg(list: &[(u8, Template)]) -> HostMsg {
    let mut entries = [FaceEntry::EMPTY; 8];
    for (slot, &(id, template)) in list.iter().enumerate() {
        entries[slot] = FaceEntry::new(id, template, template.label());
    }
    HostMsg::Faces { list: entries, count: list.len() as u8 }
}

fn macro_msg(face: u8, bindings: [Option<Binding>; 9]) -> HostMsg {
    let mut keys = [MacroKey::EMPTY; 9];
    for (key, binding) in keys.iter_mut().zip(bindings) {
        if let Some(binding) = binding {
            key.present = true;
            key.binding = binding;
            key.label.set("k");
        }
    }
    HostMsg::Macro { face, source: None, keys }
}

fn manager(ui: &mut UiState) {
    ui.global.overlay = Overlay::Manager;
    ui.manager.selected = ui.global.current;
    ui.manager.origin = ui.global.current;
    ui.manager.first_visible = 0;
    ui.invalidate();
}

fn key(key: KeyId, act: u8, shift: bool, face: u8) -> Event {
    Event::Key { key, act, shift, face }
}

fn mac(index: u8, act: u8) -> Event {
    Event::Macro { face: MACRO_FACE, index, act }
}

// ---- Omarchy ---------------------------------------------------------------------

#[test]
fn omarchy_forwards_keys_with_the_latched_shift_and_face_and_the_release_repeats_them() {
    let mut ui = on(Template::Omarchy);
    assert_eq!(events(&press(&mut ui, Button::Enter, 10)), [key(KeyId::Enter, 1, false, 2)]);
    assert_eq!(
        events(&button(&mut ui, Button::Enter, RELEASE, 20)),
        [key(KeyId::Enter, 0, false, 2)]
    );
    press(&mut ui, Button::Shift, 30);
    assert_eq!(events(&press(&mut ui, Button::Enter, 40)), [key(KeyId::Enter, 1, true, 2)]);
    // SHIFT comes up first: the release still says shift, as latched.
    assert_eq!(events(&button(&mut ui, Button::Shift, RELEASE, 50)), []);
    assert_eq!(
        events(&button(&mut ui, Button::Enter, RELEASE, 60)),
        [key(KeyId::Enter, 0, true, 2)]
    );
    for (b, k) in [(Button::Left, KeyId::Left), (Button::Right, KeyId::Right)] {
        assert_eq!(events(&press(&mut ui, b, 70)), [key(k, 1, false, 2)]);
        assert_eq!(events(&button(&mut ui, b, RELEASE, 80)), [key(k, 0, false, 2)]);
    }
    press(&mut ui, Button::Shift, 90);
    assert_eq!(events(&press(&mut ui, Button::Cancel, 100)), [key(KeyId::Cancel, 1, true, 2)]);
    assert_eq!(ui.current_face().template, Template::Omarchy);
    assert_eq!(
        events(&button(&mut ui, Button::Cancel, RELEASE, 110)),
        [key(KeyId::Cancel, 0, true, 2)]
    );
    button(&mut ui, Button::Shift, RELEASE, 120);
    assert_eq!(events(&press(&mut ui, Button::Cancel, 130)), [Event::Face { id: 1 }]);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn enter_held_in_omarchy_then_slide_emits_omarchy_release_and_swallows_physical_release() {
    let mut ui = on(Template::Omarchy);
    press(&mut ui, Button::Enter, 10);
    press(&mut ui, Button::Shift, 20);
    let out = wheel(&mut ui, Wheel::Ccw, 30);
    assert_eq!(events(&out), [key(KeyId::Enter, 0, false, 2), Event::Face { id: 3 }]);
    assert_eq!(ui.current_face().template, Template::Macropad);
    button(&mut ui, Button::Shift, RELEASE, 40);
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 50)), []);
}

#[test]
fn omarchy_scroll_sends_act_2_and_moves_the_highlight_until_the_next_ws_push() {
    let mut ui = on(Template::Omarchy);
    assert_eq!(ui.omarchy.selected_window, 0);
    let out = wheel(&mut ui, Wheel::Ccw, 10);
    assert_eq!(events(&out), [key(KeyId::Cw, 2, false, 2)]);
    assert!(out.dirty && ui.omarchy.selected_window == 1);
    wheel(&mut ui, Wheel::Ccw, 20);
    let out = wheel(&mut ui, Wheel::Ccw, 30);
    assert_eq!(events(&out), [key(KeyId::Cw, 2, false, 2)]);
    assert!(!out.dirty && ui.omarchy.selected_window == 2, "hard end at the last window");
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 40)), [key(KeyId::Ccw, 2, false, 2)]);
    assert_eq!(ui.omarchy.selected_window, 1);
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 45)), [], "a step holds nothing");
    let ws = HostMsg::Workspaces {
        active: 2,
        count: 4,
        selected: 0,
        windows: ui.deck.workspaces.windows,
        window_count: 3,
    };
    let out = host(&mut ui, &ws, 50);
    assert_eq!(ui.omarchy.selected_window, 0);
    assert_eq!(events(&out), []);
    // A push with fewer windows keeps the highlight in range (a `sel` past
    // the windows is rejected whole by the model, so the host's is in range).
    let mut ui = on(Template::Omarchy);
    wheel(&mut ui, Wheel::Ccw, 10);
    wheel(&mut ui, Wheel::Ccw, 20);
    let ws = HostMsg::Workspaces {
        active: 2,
        count: 4,
        selected: 0,
        windows: ui.deck.workspaces.windows,
        window_count: 1,
    };
    host(&mut ui, &ws, 30);
    assert_eq!(ui.omarchy.selected_window, 0);
}

#[test]
fn omarchy_touch_on_a_chip_or_row_sends_ws_go_or_ws_win() {
    let mut ui = on(Template::Omarchy);
    let chip = layout::omarchy_chip(2).center();
    touch(&mut ui, TouchPhase::Down, chip, 10);
    assert_eq!(ui.omarchy.pressed_chip, Some(2));
    let out = touch(&mut ui, TouchPhase::Up, chip, 20);
    assert_eq!(events(&out), [Event::WsGo(3)]);
    assert_eq!(ui.omarchy.pressed_chip, None);
    let row = layout::omarchy_window(1).center();
    touch(&mut ui, TouchPhase::Down, row, 30);
    assert_eq!(ui.omarchy.pressed_row, Some(1));
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, row, 40)), [Event::WsWin(1)]);
    assert_eq!((ui.omarchy.selected_window, ui.omarchy.pressed_row), (1, None));
    // Sliding off cancels; an empty row and an unused chip are not targets.
    touch(&mut ui, TouchPhase::Down, row, 50);
    touch(&mut ui, TouchPhase::Move, (row.0, row.1 + 60), 60);
    assert_eq!(ui.omarchy.pressed_row, None);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, row, 70)), []);
    assert_eq!(tap(&mut ui, layout::omarchy_window(3).center(), 80), []);
    assert_eq!(tap(&mut ui, layout::omarchy_chip(6).center(), 100), []);
}

// ---- Macropad --------------------------------------------------------------------

#[test]
fn macropad_hardware_bindings_press_their_key_and_never_emit_key_events() {
    let mut ui = on(Template::Macropad);
    // Demo bindings: build=LEFT, test=RIGHT, deploy=ENTER, git push=SHIFT+LEFT,
    // mute=SHIFT+RIGHT, term=SHIFT+ENTER.
    for (b, shift, index) in [
        (Button::Left, false, 0),
        (Button::Right, false, 1),
        (Button::Enter, false, 2),
        (Button::Left, true, 3),
        (Button::Right, true, 6),
        (Button::Enter, true, 7),
    ] {
        if shift {
            press(&mut ui, Button::Shift, 10);
        }
        let out = press(&mut ui, b, 20);
        assert_eq!(events(&out), [mac(index, 1)], "{b:?} shift {shift}");
        assert!(out.dirty);
        assert_eq!((ui.macropad.face, ui.macropad.down), (MACRO_FACE, 1 << index));
        if shift {
            button(&mut ui, Button::Shift, RELEASE, 30);
        }
        let out = button(&mut ui, b, RELEASE, 40);
        assert_eq!(events(&out), [mac(index, 0)]);
        assert!(out.dirty && ui.macropad.down == 0);
    }
    // SHIFT+CANCEL has no key: it puts the editor's cursor on the grid and
    // does not go home. A plain CANCEL drops the cursor; the next goes home.
    press(&mut ui, Button::Shift, 50);
    assert_eq!(events(&press(&mut ui, Button::Cancel, 60)), []);
    assert_eq!(ui.current_face().template, Template::Macropad);
    assert!(ui.macropad.cursor.is_some());
    button(&mut ui, Button::Cancel, RELEASE, 70);
    button(&mut ui, Button::Shift, RELEASE, 80);
    assert_eq!(events(&press(&mut ui, Button::Cancel, 90)), []);
    assert!(ui.macropad.cursor.is_none());
    button(&mut ui, Button::Cancel, RELEASE, 95);
    assert_eq!(events(&press(&mut ui, Button::Cancel, 100)), [Event::Face { id: 1 }]);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn macro_release_uses_index_latched_at_press() {
    let mut ui = on(Template::Macropad);
    assert_eq!(events(&press(&mut ui, Button::Enter, 10)), [mac(2, 1)]);
    // The host rebinds ENTER to key 5 while it is down.
    let mut keys = ui.deck.macro_set(MACRO_FACE).unwrap().keys;
    keys[2].binding = Binding::None;
    keys[5].binding = Binding::Enter;
    let out = host(&mut ui, &HostMsg::Macro { face: MACRO_FACE, source: None, keys }, 20);
    assert_eq!(events(&out), []);
    assert_eq!(ui.macropad.down, 1 << 2);
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 30)), [mac(2, 0)]);
    assert_eq!(events(&press(&mut ui, Button::Enter, 40)), [mac(5, 1)]);
}

#[test]
fn macropad_host_push_removing_a_held_key_tears_the_hold_down() {
    let mut ui = on(Template::Macropad);
    press(&mut ui, Button::Enter, 10);
    let mut keys = ui.deck.macro_set(MACRO_FACE).unwrap().keys;
    keys[2] = MacroKey::EMPTY;
    let out = host(&mut ui, &HostMsg::Macro { face: MACRO_FACE, source: None, keys }, 20);
    assert_eq!(events(&out), [mac(2, 0)]);
    assert_eq!(ui.macropad.down, 0);
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 30)), []);
}

#[test]
fn macropad_wheel_bindings_step_and_flash_for_150_ms() {
    let mut ui = on(Template::Macropad);
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 10)), [], "no wheel binding in the demo");
    let mut keys = ui.deck.macro_set(MACRO_FACE).unwrap().keys;
    keys[4].binding = Binding::Cw;
    keys[5].binding = Binding::Ccw;
    host(&mut ui, &HostMsg::Macro { face: MACRO_FACE, source: None, keys }, 20);
    let out = wheel(&mut ui, Wheel::Cw, 100);
    assert_eq!(events(&out), [mac(4, 2)]);
    assert!(out.dirty && ui.macropad.down == 1 << 4 && ui.animating());
    assert_eq!(events(&wheel(&mut ui, Wheel::Ccw, 120)), [mac(5, 2)]);
    assert_eq!(ui.macropad.down, 1 << 4 | 1 << 5);
    let out = tick(&mut ui, 249);
    assert!(!out.dirty && ui.macropad.down == 1 << 4 | 1 << 5);
    let out = tick(&mut ui, 250);
    assert!(out.dirty && ui.macropad.down == 1 << 5);
    tick(&mut ui, 270);
    assert!(ui.macropad.down == 0 && !ui.animating());
}

#[test]
fn macropad_touch_presses_on_down_and_releases_on_up_or_leave() {
    let mut ui = on(Template::Macropad);
    let at = layout::macro_key(4).center();
    let out = touch(&mut ui, TouchPhase::Down, at, 10);
    assert_eq!(events(&out), [mac(4, 1)]);
    assert_eq!(ui.macropad.down, 1 << 4);
    let out = touch(&mut ui, TouchPhase::Up, at, 20);
    assert_eq!(events(&out), [mac(4, 0)]);
    assert_eq!(ui.macropad.down, 0);
    // Leaving the key releases it; coming back does not press again.
    touch(&mut ui, TouchPhase::Down, at, 30);
    let out = touch(&mut ui, TouchPhase::Move, (at.0, at.1 + 80), 40);
    assert_eq!(events(&out), [mac(4, 0)]);
    assert_eq!(ui.macropad.down, 0);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Move, at, 50)), []);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, at, 60)), []);
    // A tap on "+ assign" sends nothing: it opens the key editor, which
    // CANCEL closes. A Cancel on a key releases like an Up.
    assert_eq!(tap(&mut ui, layout::macro_key(8).center(), 70), []);
    assert_eq!(ui.global.overlay, crate::ui::state::Overlay::MacroEdit);
    press(&mut ui, Button::Cancel, 80);
    button(&mut ui, Button::Cancel, RELEASE, 85);
    assert_eq!(ui.global.overlay, crate::ui::state::Overlay::None);
    touch(&mut ui, TouchPhase::Down, at, 90);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Cancel, at, 100)), [mac(4, 0)]);
    assert_eq!(ui.macropad.down, 0);
}

#[test]
fn touch_down_on_macro_then_jump_does_not_confirm_anything_on_up() {
    let mut ui = on(Template::Macropad);
    let at = layout::macro_key(0).center();
    touch(&mut ui, TouchPhase::Down, at, 10);
    // Something jumps the strip while the finger is down: the key is released
    // now and the Up lands nowhere.
    press(&mut ui, Button::Shift, 20);
    let out = wheel(&mut ui, Wheel::Cw, 30);
    assert_eq!(events(&out), [mac(0, 0), Event::Face { id: 2 }]);
    button(&mut ui, Button::Shift, RELEASE, 40);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, at, 50)), []);
    assert_eq!(ui.macropad.down, 0);
}

#[test]
fn macropad_masks_belong_to_one_instance() {
    let mut ui = on(Template::Macropad);
    press(&mut ui, Button::Left, 10);
    assert_eq!((ui.macropad.face, ui.macropad.down), (MACRO_FACE, 1));
    // A second Macropad instance (id 7) with its own keys.
    ui.global.strip[6] = FaceEntry::new(7, Template::Macropad, "Ops");
    ui.global.strip_len = 7;
    host(&mut ui, &macro_msg(7, [Some(Binding::Right); 9]), 20);
    ui.global.current = 6;
    ui.invalidate();
    let out = press(&mut ui, Button::Right, 30);
    assert_eq!(events(&out), [Event::Macro { face: 7, index: 0, act: 1 }]);
    assert_eq!((ui.macropad.face, ui.macropad.down), (7, 1));
    // The first instance's LEFT is still held by global and releases by identity.
    assert_eq!(events(&button(&mut ui, Button::Left, RELEASE, 40)), [mac(0, 0)]);
    assert_eq!(ui.macropad.down, 1);
}

// ---- Usage -----------------------------------------------------------------------

fn usage_msg(count: u8) -> HostMsg {
    let mut items = [UsageItem::EMPTY; 6];
    for (index, item) in items.iter_mut().enumerate() {
        item.name.set("p");
        item.percent = index as u8 * 20;
    }
    HostMsg::Usage { period: Text::from_str("SEP"), items, count }
}

#[test]
fn usage_scroll_moves_the_viewport_within_the_list() {
    let mut ui = on(Template::Usage);
    let out = wheel(&mut ui, Wheel::Ccw, 10);
    assert!(!out.dirty && ui.usage.top_row == 0, "three items fit: nothing to scroll");
    host(&mut ui, &usage_msg(6), 20);
    let out = wheel(&mut ui, Wheel::Ccw, 30);
    assert!(out.dirty && ui.usage.top_row == 1);
    wheel(&mut ui, Wheel::Ccw, 40);
    let out = wheel(&mut ui, Wheel::Ccw, 50);
    assert!(!out.dirty && ui.usage.top_row == 2, "hard end");
    wheel(&mut ui, Wheel::Cw, 60);
    assert_eq!(ui.usage.top_row, 1);
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 70)), []);
    // A shorter list pulls the viewport back.
    wheel(&mut ui, Wheel::Ccw, 80);
    wheel(&mut ui, Wheel::Ccw, 90);
    host(&mut ui, &usage_msg(5), 100);
    assert_eq!(ui.usage.top_row, 1);
    host(&mut ui, &usage_msg(2), 110);
    assert_eq!(ui.usage.top_row, 0);
}

#[test]
fn usage_enter_asks_for_a_refresh_and_shows_updating_for_1500_ms() {
    let mut ui = on(Template::Usage);
    assert!(!ui.usage.updating && !ui.animating());
    let out = press(&mut ui, Button::Enter, 1000);
    assert_eq!(events(&out), [Event::Refresh]);
    assert!(out.dirty && ui.usage.updating && ui.usage.refreshed_at_ms == 1000);
    assert!(ui.animating());
    let out = tick(&mut ui, 2499);
    assert!(!out.dirty && ui.usage.updating);
    let out = tick(&mut ui, 2500);
    assert!(out.dirty && !ui.usage.updating && !ui.animating());
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 2600)), []);
    press(&mut ui, Button::Cancel, 2700);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn usage_rows_are_scroll_targets_only() {
    let mut ui = on(Template::Usage);
    let at = layout::usage_row(1).center();
    assert_eq!(tap(&mut ui, at, 10), []);
    assert_eq!(tap(&mut ui, layout::usage_row(3).center(), 30), []);
    assert_eq!(ui.current_face().template, Template::Usage);
}

// ---- Settings --------------------------------------------------------------------

#[test]
fn settings_scroll_selects_across_panels_and_left_right_jump_panels() {
    let mut ui = on(Template::Settings);
    assert_eq!((ui.settings.panel, ui.settings.row), (0, 0));
    // CW at the first row is the hard end.
    let out = wheel(&mut ui, Wheel::Cw, 10);
    assert!(!out.dirty && events(&out).is_empty());
    // CCW walks every row of every panel in order, the tab following.
    let before = ui.deck.settings;
    let mut walk = vec![(0, 0)];
    for at in 0..20 {
        let out = wheel(&mut ui, Wheel::Ccw, 20 + at);
        assert!(events(&out).is_empty(), "selecting never sets anything");
        walk.push((ui.settings.panel, ui.settings.row));
    }
    walk.dedup();
    let expected: Vec<(u8, u8)> = (0..settings::PANELS)
        .flat_map(|panel| (0..settings::rows(panel)).map(move |row| (panel, row)))
        .collect();
    assert_eq!(walk, expected, "hard end at the last row of About");
    assert_eq!(ui.deck.settings, before, "no value moved");
    // CW from a panel's first row lands on the previous panel's last row.
    ui.settings.panel = 3;
    ui.settings.row = 0;
    wheel(&mut ui, Wheel::Cw, 50);
    assert_eq!((ui.settings.panel, ui.settings.row), (2, 1));
    // LEFT/RIGHT jump to a panel's first row; the ends are hard.
    let out = press(&mut ui, Button::Right, 60);
    assert!(out.dirty);
    assert_eq!((ui.settings.panel, ui.settings.row), (3, 0));
    ui.settings.panel = 0;
    let out = press(&mut ui, Button::Left, 70);
    assert!(!out.dirty && ui.settings.panel == 0);
    // No chords; CANCEL goes home.
    press(&mut ui, Button::Shift, 90);
    assert_eq!(events(&press(&mut ui, Button::Right, 100)), []);
    assert_eq!(ui.settings.panel, 0);
    button(&mut ui, Button::Shift, RELEASE, 110);
    press(&mut ui, Button::Cancel, 120);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn settings_enter_adjusts_a_value_with_hard_ends_and_emits_set_once() {
    let mut ui = on(Template::Settings);
    assert_eq!(ui.deck.settings.speaker, 6);
    let out = press(&mut ui, Button::Enter, 10);
    assert!(out.dirty && ui.settings.adjusting && events(&out).is_empty());
    let out = wheel(&mut ui, Wheel::Cw, 20);
    assert_eq!(events(&out), [Event::Set { key: SettingKey::Speaker, value: 7 }]);
    assert!(out.dirty && ui.deck.settings.speaker == 7);
    assert_eq!((ui.settings.panel, ui.settings.row), (0, 0), "adjusting does not move the selection");
    for _ in 0..5 {
        wheel(&mut ui, Wheel::Cw, 30);
    }
    let out = wheel(&mut ui, Wheel::Cw, 40);
    assert!(!out.dirty && events(&out).is_empty() && ui.deck.settings.speaker == 10);
    assert_eq!(events(&wheel(&mut ui, Wheel::Ccw, 50)), [Event::Set { key: SettingKey::Speaker, value: 9 }]);
    // ENTER ends adjusting; SCROLL selects again.
    let out = press(&mut ui, Button::Enter, 60);
    assert!(out.dirty && !ui.settings.adjusting && events(&out).is_empty());
    wheel(&mut ui, Wheel::Ccw, 70);
    assert_eq!((ui.settings.row, ui.deck.settings.speaker), (1, 9));
    // CANCEL ends adjusting too, without going home.
    press(&mut ui, Button::Enter, 80);
    for _ in 0..7 {
        wheel(&mut ui, Wheel::Ccw, 90);
    }
    assert_eq!(ui.deck.settings.mic, 0);
    assert_eq!(events(&wheel(&mut ui, Wheel::Ccw, 100)), []);
    press(&mut ui, Button::Cancel, 110);
    assert!(!ui.settings.adjusting);
    assert_eq!(ui.current_face().template, Template::Settings);
}

#[test]
fn settings_brightness_and_idle_home_step_through_their_values() {
    let mut ui = on(Template::Settings);
    ui.settings.panel = 3;
    ui.invalidate();
    press(&mut ui, Button::Enter, 5);
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 10)), [], "brightness is already 5");
    assert_eq!(
        events(&wheel(&mut ui, Wheel::Ccw, 20)),
        [Event::Set { key: SettingKey::Brightness, value: 4 }]
    );
    assert_eq!(ui.user_brightness(), 4);
    for _ in 0..5 {
        wheel(&mut ui, Wheel::Ccw, 30);
    }
    assert_eq!(ui.user_brightness(), 1);
    press(&mut ui, Button::Enter, 40);
    wheel(&mut ui, Wheel::Ccw, 41);
    press(&mut ui, Button::Enter, 42);
    assert_eq!(ui.deck.settings.idle_home_s, 120);
    let mut seen = Vec::new();
    for at in 0..4 {
        seen.extend(events(&wheel(&mut ui, Wheel::Ccw, 50 + at)));
    }
    assert_eq!(
        seen,
        [60, 30, 0].map(|value| Event::Set { key: SettingKey::IdleHome, value })
    );
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 60)), [Event::Set { key: SettingKey::IdleHome, value: 30 }]);
}

#[test]
fn settings_enter_flips_a_toggle_and_read_only_rows_ignore_it() {
    let mut ui = on(Template::Settings);
    ui.settings.panel = 2;
    ui.invalidate();
    assert!(ui.deck.settings.chime);
    let out = press(&mut ui, Button::Enter, 10);
    assert_eq!(events(&out), [Event::Set { key: SettingKey::Chime, value: 0 }]);
    assert!(!ui.deck.settings.chime && !ui.settings.adjusting, "a toggle flips, it is not adjusted");
    assert_eq!(events(&press(&mut ui, Button::Enter, 20)), [Event::Set { key: SettingKey::Chime, value: 1 }]);
    wheel(&mut ui, Wheel::Ccw, 30);
    assert_eq!(events(&press(&mut ui, Button::Enter, 40)), [Event::Set { key: SettingKey::AutoJump, value: 1 }]);
    assert!(ui.deck.settings.auto_jump);
    // A read-only row (About → Firmware) neither flips nor adjusts.
    ui.settings.panel = settings::PANELS - 1;
    ui.settings.row = 0;
    let out = press(&mut ui, Button::Enter, 50);
    assert!(!out.dirty && events(&out).is_empty() && !ui.settings.adjusting);
}

#[test]
fn settings_adjusting_ends_when_the_face_is_left_or_a_panel_or_row_is_picked() {
    let mut ui = on(Template::Settings);
    press(&mut ui, Button::Enter, 10);
    assert!(ui.settings.adjusting);
    press(&mut ui, Button::Right, 20);
    assert!(!ui.settings.adjusting, "a panel jump ends it");
    let out = press(&mut ui, Button::Enter, 30);
    assert_eq!(events(&out), [Event::Set { key: SettingKey::Announce, value: 0 }]);
    assert!(!ui.settings.adjusting, "Speech → Auto-announce is a toggle");
    let mut ui = on(Template::Settings);
    press(&mut ui, Button::Enter, 10);
    // A tap on another row selects it and ends adjusting; a tap on the
    // selected row starts it again.
    assert_eq!(tap(&mut ui, layout::settings_row(1).center(), 20), []);
    assert_eq!((ui.settings.row, ui.settings.adjusting), (1, false));
    tap(&mut ui, layout::settings_row(1).center(), 40);
    assert!(ui.settings.adjusting);
    // Sliding away ends it: coming back, SCROLL selects.
    press(&mut ui, Button::Shift, 50);
    wheel(&mut ui, Wheel::Cw, 60);
    button(&mut ui, Button::Shift, RELEASE, 70);
    tick(&mut ui, 400);
    assert!(!ui.settings.adjusting);
}

#[test]
fn settings_needs_host_only_without_a_deck_host_and_outside_the_demo() {
    let mut ui = on(Template::Settings);
    assert!(ui.deck.demo_active());
    assert!(!settings::needs_host(&ui, SettingKey::Speaker));
    ui.deck.settings.demo = false;
    assert!(settings::needs_host(&ui, SettingKey::Speaker));
    assert!(settings::needs_host(&ui, SettingKey::Chime));
    assert!(!settings::needs_host(&ui, SettingKey::Brightness));
    assert!(!settings::needs_host(&ui, SettingKey::IdleHome));
    ui.deck.host_present = true;
    assert!(!settings::needs_host(&ui, SettingKey::Speaker));
}

#[test]
fn settings_demo_toggle_loads_and_clears_the_built_in_data() {
    let mut ui = on(Template::Settings);
    ui.settings.panel = 4;
    ui.settings.row = 2;
    ui.invalidate();
    assert!(ui.deck.demo_active() && ui.deck.order_len == 5);
    let out = press(&mut ui, Button::Enter, 10);
    assert_eq!(events(&out), [Event::Set { key: SettingKey::Demo, value: 0 }]);
    assert!(!ui.deck.demo_active() && ui.deck.order_len == 0 && ui.notify.toast.is_none());
    assert_eq!(ui.deck.notice_count(), 0);
    assert_eq!(ui.agents.highlight, 0);
    let out = press(&mut ui, Button::Enter, 20);
    assert_eq!(events(&out), [Event::Set { key: SettingKey::Demo, value: 1 }]);
    assert!(ui.deck.demo_active() && ui.deck.order_len == 5);
    assert_eq!(ui.agents.highlight, 1);
    // With a real host the flag only changes what happens after it leaves.
    ui.deck.host_present = true;
    ui.deck.clear_host_data();
    press(&mut ui, Button::Enter, 30);
    press(&mut ui, Button::Enter, 40);
    assert_eq!(ui.deck.order_len, 0);
}

#[test]
fn settings_touch_tabs_switch_panels_and_rows_select_or_toggle() {
    let mut ui = on(Template::Settings);
    let tab = layout::settings_tab(2).center();
    touch(&mut ui, TouchPhase::Down, tab, 10);
    assert_eq!(ui.settings.pressed, Some(2));
    touch(&mut ui, TouchPhase::Up, tab, 20);
    assert_eq!((ui.settings.panel, ui.settings.row, ui.settings.pressed), (2, 0, None));
    let row = layout::settings_row(1).center();
    touch(&mut ui, TouchPhase::Down, row, 30);
    assert_eq!(ui.settings.pressed, Some(9));
    assert_eq!(tap(&mut ui, row, 40), [], "the first tap on a row only selects it");
    assert_eq!(ui.settings.row, 1);
    assert_eq!(tap(&mut ui, row, 60), [Event::Set { key: SettingKey::AutoJump, value: 1 }]);
    assert_eq!(tap(&mut ui, row, 80), [Event::Set { key: SettingKey::AutoJump, value: 0 }]);
    // No third row on this panel; sliding off cancels.
    assert_eq!(tap(&mut ui, layout::settings_row(2).center(), 100), []);
    touch(&mut ui, TouchPhase::Down, row, 120);
    touch(&mut ui, TouchPhase::Move, (row.0, row.1 + 70), 130);
    assert_eq!(ui.settings.pressed, None);
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, row, 140)), []);
}

#[test]
fn a_host_set_applies_without_an_echo() {
    let mut ui = on(Template::Settings);
    let out = host(&mut ui, &HostMsg::Set { key: SettingKey::Brightness, value: 2 }, 10);
    assert_eq!(events(&out), []);
    assert!(out.dirty && ui.user_brightness() == 2);
}

// ---- Manager ---------------------------------------------------------------------

#[test]
fn manager_scroll_selects_with_the_viewport_following_and_plus_last() {
    let mut ui = fresh();
    manager(&mut ui);
    for expected in 1..=5 {
        let out = wheel(&mut ui, Wheel::Ccw, 10);
        assert!(out.dirty && ui.manager.selected == expected);
        assert!(ui.manager.first_visible <= expected);
        assert!(expected < ui.manager.first_visible + layout::MANAGER_VISIBLE);
    }
    assert_eq!(ui.manager.first_visible, 2);
    let out = wheel(&mut ui, Wheel::Ccw, 20);
    assert!(!out.dirty && ui.manager.selected == 5, "the '+' card is the last one");
    for _ in 0..8 {
        wheel(&mut ui, Wheel::Cw, 30);
    }
    assert_eq!((ui.manager.selected, ui.manager.first_visible), (0, 0));
    assert_eq!(events(&wheel(&mut ui, Wheel::Cw, 40)), [], "nothing reaches the host");
    assert_eq!(ui.global.overlay, Overlay::Manager);
}

#[test]
fn manager_enter_grabs_moves_and_drops_with_one_faces_event() {
    let mut ui = fresh();
    manager(&mut ui);
    wheel(&mut ui, Wheel::Ccw, 10);
    let out = press(&mut ui, Button::Enter, 20);
    assert!(out.dirty && ui.manager.grabbed && events(&out).is_empty());
    wheel(&mut ui, Wheel::Ccw, 30);
    assert_eq!(ids(&ui), [1, 3, 2, 4, 5]);
    let out = wheel(&mut ui, Wheel::Ccw, 40);
    assert_eq!(ids(&ui), [1, 3, 4, 2, 5]);
    assert!(out.dirty && events(&out).is_empty(), "a move is not a completed change");
    assert_eq!(ui.manager.selected, 3);
    for _ in 0..3 {
        wheel(&mut ui, Wheel::Ccw, 50);
    }
    assert_eq!(ids(&ui), [1, 3, 4, 2, 5], "Settings is never displaced");
    assert_eq!(ui.manager.selected, 3);
    let out = press(&mut ui, Button::Enter, 60);
    assert_eq!(events(&out), [Event::Faces]);
    assert!(!ui.manager.grabbed);
    // Grab and drop in place: no change, no event.
    press(&mut ui, Button::Enter, 70);
    assert_eq!(events(&press(&mut ui, Button::Enter, 80)), []);
    assert_eq!(ui.current_face().template, Template::Agents);
    assert_eq!(ui.global.overlay, Overlay::Manager);
}

#[test]
fn cancel_while_grabbed_reverts_order() {
    let mut ui = on(Template::Usage);
    manager(&mut ui);
    press(&mut ui, Button::Enter, 10);
    wheel(&mut ui, Wheel::Cw, 20);
    wheel(&mut ui, Wheel::Cw, 30);
    assert_eq!(ids(&ui), [1, 4, 2, 3, 5]);
    assert_eq!(ui.global.current, 1, "the current face follows its card");
    let out = press(&mut ui, Button::Cancel, 40);
    assert_eq!(ids(&ui), [1, 2, 3, 4, 5]);
    assert!(out.dirty && events(&out).is_empty() && !ui.manager.grabbed);
    assert_eq!((ui.manager.selected, ui.global.current), (3, 3));
    assert_eq!(ui.global.overlay, Overlay::Manager);
}

#[test]
fn manager_cancel_on_a_removable_card_asks_and_enter_removes() {
    let mut ui = on(Template::Usage);
    manager(&mut ui);
    wheel(&mut ui, Wheel::Cw, 10);
    let out = press(&mut ui, Button::Cancel, 20);
    assert!(out.dirty && events(&out).is_empty());
    assert_eq!((ui.global.overlay, ui.manager.confirm_face), (Overlay::ConfirmRemove, 3));
    let out = press(&mut ui, Button::Enter, 30);
    assert_eq!(events(&out), [Event::Faces]);
    assert_eq!(ids(&ui), [1, 2, 4, 5]);
    assert_eq!((ui.global.overlay, ui.manager.selected), (Overlay::Manager, 1));
    assert!(ui.deck.macro_set(MACRO_FACE).is_none(), "its macro set is freed");
    assert_eq!((ui.global.current, ui.current_face().id), (2, 4), "current kept by identity");
}

#[test]
fn manager_cancel_on_pinned_does_nothing_and_confirm_cancel_keeps() {
    let mut ui = fresh();
    manager(&mut ui);
    let out = press(&mut ui, Button::Cancel, 10);
    assert!(!out.dirty && ui.global.overlay == Overlay::Manager);
    for _ in 0..4 {
        wheel(&mut ui, Wheel::Ccw, 20);
    }
    assert_eq!(ui.face_at(ui.manager.selected).unwrap().template, Template::Settings);
    press(&mut ui, Button::Cancel, 30);
    assert_eq!(ui.global.overlay, Overlay::Manager);
    wheel(&mut ui, Wheel::Ccw, 40);
    press(&mut ui, Button::Cancel, 50);
    assert_eq!(ui.global.overlay, Overlay::Manager, "'+' is not removable");
    wheel(&mut ui, Wheel::Cw, 60);
    wheel(&mut ui, Wheel::Cw, 70);
    press(&mut ui, Button::Cancel, 80);
    assert_eq!(ui.global.overlay, Overlay::ConfirmRemove);
    let out = press(&mut ui, Button::Cancel, 90);
    assert_eq!(ui.global.overlay, Overlay::Manager);
    assert!(events(&out).is_empty() && ids(&ui) == [1, 2, 3, 4, 5]);
    // The pinned ends cannot be grabbed either.
    for _ in 0..5 {
        wheel(&mut ui, Wheel::Cw, 100);
    }
    let out = press(&mut ui, Button::Enter, 110);
    assert_eq!(ui.global.overlay, Overlay::None, "ENTER on a pinned card goes there");
    assert_eq!(events(&out), []);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn removing_face_left_of_current_keeps_current_face() {
    let mut ui = on(Template::Usage);
    manager(&mut ui);
    wheel(&mut ui, Wheel::Cw, 10);
    wheel(&mut ui, Wheel::Cw, 20);
    press(&mut ui, Button::Cancel, 30);
    press(&mut ui, Button::Enter, 40);
    assert_eq!(ids(&ui), [1, 3, 4, 5]);
    assert_eq!((ui.global.current, ui.current_face().template), (2, Template::Usage));
    assert!(ui.strip_at_rest());
    // Removing the current face itself falls back to its left neighbour.
    assert_eq!(ui.manager.selected, 0, "the removed card's left neighbour");
    wheel(&mut ui, Wheel::Ccw, 50);
    wheel(&mut ui, Wheel::Ccw, 55);
    assert_eq!(ui.manager.selected, 2);
    press(&mut ui, Button::Cancel, 60);
    press(&mut ui, Button::Enter, 70);
    assert_eq!(ids(&ui), [1, 3, 5]);
    assert_eq!((ui.global.current, ui.current_face().id, ui.manager.selected), (1, 3, 1));
}

#[test]
fn manager_exit_lands_on_selected_face() {
    let mut ui = on(Template::Macropad);
    manager(&mut ui);
    for _ in 0..2 {
        wheel(&mut ui, Wheel::Ccw, 10);
    }
    let out = press(&mut ui, Button::Enter, 20);
    assert_eq!(ui.global.overlay, Overlay::None);
    assert_eq!(ui.current_face().template, Template::Settings);
    assert_eq!(events(&out), [Event::Face { id: 5 }]);
    assert!(ui.strip_at_rest());
}

#[test]
fn plus_opens_the_gallery_which_leads_to_naming_and_enter_accepts_the_default_name() {
    let mut ui = fresh();
    manager(&mut ui);
    for _ in 0..5 {
        wheel(&mut ui, Wheel::Ccw, 10);
    }
    let out = press(&mut ui, Button::Enter, 20);
    assert!(out.dirty && ui.global.overlay == Overlay::Gallery && ui.manager.gallery_sel == 0);
    wheel(&mut ui, Wheel::Ccw, 30);
    wheel(&mut ui, Wheel::Ccw, 40);
    let out = wheel(&mut ui, Wheel::Ccw, 60);
    assert!(out.dirty && ui.manager.gallery_sel == 3);
    let out = wheel(&mut ui, Wheel::Ccw, 65);
    assert!(out.dirty && ui.manager.gallery_sel == 4);
    assert_eq!(GALLERY[4], Template::System);
    let out = wheel(&mut ui, Wheel::Ccw, 66);
    assert!(out.dirty && ui.manager.gallery_sel == 5);
    assert_eq!(GALLERY[5], Template::Timer);
    let out = wheel(&mut ui, Wheel::Ccw, 66);
    assert!(!out.dirty && ui.manager.gallery_sel == 5);
    wheel(&mut ui, Wheel::Cw, 67);
    wheel(&mut ui, Wheel::Cw, 67);
    wheel(&mut ui, Wheel::Cw, 68);
    wheel(&mut ui, Wheel::Cw, 70);
    assert_eq!(GALLERY[ui.manager.gallery_sel as usize], Template::Macropad);
    press(&mut ui, Button::Enter, 80);
    assert_eq!(ui.global.overlay, Overlay::Naming);
    assert_eq!(ui.manager.naming_template, Template::Macropad);
    assert!(ui.manager.naming_text.is_empty());
    let out = press(&mut ui, Button::Enter, 90);
    assert_eq!(events(&out), [Event::Faces]);
    assert_eq!(ui.global.overlay, Overlay::Manager);
    assert_eq!(ids(&ui), [1, 2, 3, 4, 6, 5], "before Settings, lowest free id");
    let added = ui.face_at(4).unwrap();
    assert_eq!((added.template, added.name.as_str()), (Template::Macropad, "Macropad"));
    assert_eq!(ui.manager.selected, 4);
    assert_eq!(ui.current_face().template, Template::Agents);
}

#[test]
fn late_name_message_ignored() {
    let mut ui = fresh();
    manager(&mut ui);
    ui.manager.selected = 5;
    press(&mut ui, Button::Enter, 10);
    press(&mut ui, Button::Enter, 20);
    assert_eq!(ui.global.overlay, Overlay::Naming);
    let out = host(&mut ui, &HostMsg::Name { text: Text::from_str("Deploy") }, 30);
    assert!(out.dirty && ui.manager.naming_text == "Deploy");
    // CANCEL aborts back to the gallery; a name arriving now is nothing.
    press(&mut ui, Button::Cancel, 40);
    assert_eq!(ui.global.overlay, Overlay::Gallery);
    let out = host(&mut ui, &HostMsg::Name { text: Text::from_str("Late") }, 50);
    assert!(!out.dirty && ui.manager.naming_text == "Deploy");
    press(&mut ui, Button::Cancel, 60);
    assert_eq!(ui.global.overlay, Overlay::Manager);
    host(&mut ui, &HostMsg::Name { text: Text::from_str("Later") }, 70);
    assert_eq!(ids(&ui), [1, 2, 3, 4, 5]);
    // Picking again starts with an empty name; the host's name is accepted.
    press(&mut ui, Button::Enter, 80);
    wheel(&mut ui, Wheel::Ccw, 85);
    press(&mut ui, Button::Enter, 90);
    assert!(ui.manager.naming_text.is_empty());
    host(&mut ui, &HostMsg::Name { text: Text::from_str("Ops") }, 100);
    press(&mut ui, Button::Enter, 110);
    let added = ui.face_at(4).unwrap();
    assert_eq!((added.id, added.template, added.name.as_str()), (6, Template::Macropad, "Ops"));
}

#[test]
fn gallery_refuses_a_fourth_macropad_and_plus_hides_at_eight_faces() {
    let mut ui = fresh();
    host(
        &mut ui,
        &faces_msg(&[
            (1, Template::Agents),
            (2, Template::Macropad),
            (3, Template::Macropad),
            (4, Template::Macropad),
            (6, Template::Settings),
        ]),
        10,
    );
    manager(&mut ui);
    ui.manager.selected = 5;
    press(&mut ui, Button::Enter, 20);
    assert_eq!(ui.global.overlay, Overlay::Gallery);
    ui.manager.gallery_sel = 1;
    let out = press(&mut ui, Button::Enter, 30);
    assert_eq!(ui.global.overlay, Overlay::Gallery, "a fourth Macropad is refused");
    assert!(!out.dirty);
    ui.manager.gallery_sel = 2;
    press(&mut ui, Button::Enter, 40);
    press(&mut ui, Button::Enter, 50);
    assert_eq!(ids(&ui), [1, 2, 3, 4, 5, 6]);
    for _ in 0..2 {
        ui.manager.selected = ui.global.strip_len;
        press(&mut ui, Button::Enter, 70);
        press(&mut ui, Button::Enter, 80);
        press(&mut ui, Button::Enter, 90);
    }
    assert_eq!(ids(&ui), [1, 2, 3, 4, 5, 7, 8, 6]);
    assert_eq!(ui.global.strip_len, 8);
    // Eight faces: no "+" card, SCROLL stops on Settings.
    for _ in 0..4 {
        wheel(&mut ui, Wheel::Ccw, 100);
    }
    assert_eq!(ui.manager.selected, 7);
    assert_eq!(ui.face_at(7).unwrap().template, Template::Settings);
    assert_eq!(ui.global.overlay, Overlay::Manager);
}

#[test]
fn ids_are_allocated_lowest_free() {
    let mut ui = fresh();
    manager(&mut ui);
    wheel(&mut ui, Wheel::Ccw, 10);
    press(&mut ui, Button::Cancel, 20);
    press(&mut ui, Button::Enter, 30);
    assert_eq!(ids(&ui), [1, 3, 4, 5]);
    ui.manager.selected = 4;
    press(&mut ui, Button::Enter, 40);
    press(&mut ui, Button::Enter, 50);
    press(&mut ui, Button::Enter, 60);
    assert_eq!(ids(&ui), [1, 3, 4, 2, 5]);
    assert_eq!(ui.face_at(3).unwrap().template, Template::Omarchy);
}

#[test]
fn host_faces_restore_keeps_the_current_face_by_id_and_never_echoes() {
    let mut ui = on(Template::Usage);
    let out = host(
        &mut ui,
        &faces_msg(&[(1, Template::Agents), (4, Template::Usage), (2, Template::Omarchy), (6, Template::Settings)]),
        10,
    );
    assert_eq!(events(&out), [Event::Face { id: 4 }]);
    assert!(out.dirty);
    assert_eq!(ids(&ui), [1, 4, 2, 6]);
    assert_eq!((ui.global.current, ui.current_face().template), (1, Template::Usage));
    assert!(ui.strip_at_rest());
    // The current face is gone: home.
    let out = host(
        &mut ui,
        &faces_msg(&[(1, Template::Agents), (2, Template::Omarchy), (6, Template::Settings)]),
        20,
    );
    assert_eq!(events(&out), [Event::Face { id: 1 }]);
    assert_eq!(ui.current_face().template, Template::Agents);
    // Same index, another id under it: still a visible change.
    let out = host(
        &mut ui,
        &faces_msg(&[(7, Template::Agents), (2, Template::Omarchy), (6, Template::Settings)]),
        30,
    );
    assert_eq!(events(&out), [Event::Face { id: 7 }]);
    assert_eq!(ui.global.current, 0);
    // Same strip again: nothing to say.
    let out = host(
        &mut ui,
        &faces_msg(&[(7, Template::Agents), (2, Template::Omarchy), (6, Template::Settings)]),
        40,
    );
    assert_eq!(events(&out), []);
}

#[test]
fn host_faces_restore_tears_down_a_held_key_and_frees_stale_macro_sets() {
    let mut ui = on(Template::Macropad);
    press(&mut ui, Button::Enter, 10);
    assert!(ui.deck.macro_set(MACRO_FACE).is_some());
    let out = host(
        &mut ui,
        &faces_msg(&[(1, Template::Agents), (3, Template::Usage), (6, Template::Settings)]),
        20,
    );
    assert_eq!(events(&out), [mac(2, 0), Event::Face { id: 3 }]);
    assert!(ui.deck.macro_set(MACRO_FACE).is_none(), "id 3 is a Usage face now");
    assert_eq!(ui.macropad.down, 0);
    assert_eq!(events(&button(&mut ui, Button::Enter, RELEASE, 30)), []);
    // A Macropad that keeps its id keeps its keys.
    let mut ui = on(Template::Macropad);
    host(&mut ui, &faces_msg(&[(1, Template::Agents), (3, Template::Macropad), (6, Template::Settings)]), 40);
    assert!(ui.deck.macro_set(MACRO_FACE).is_some());
}

#[test]
fn faces_push_during_grab_is_deferred() {
    let mut ui = fresh();
    manager(&mut ui);
    wheel(&mut ui, Wheel::Ccw, 10);
    press(&mut ui, Button::Enter, 20);
    wheel(&mut ui, Wheel::Ccw, 30);
    let restore = faces_msg(&[(1, Template::Agents), (5, Template::Usage), (6, Template::Settings)]);
    let out = host(&mut ui, &restore, 40);
    assert_eq!(ids(&ui), [1, 3, 2, 4, 5]);
    assert_eq!(events(&out), []);
    for overlay in [Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove] {
        ui.global.overlay = overlay;
        host(&mut ui, &restore, 50);
        assert_eq!(ids(&ui), [1, 3, 2, 4, 5], "{overlay:?}");
    }
    ui.global.overlay = Overlay::Drawer;
    host(&mut ui, &restore, 60);
    assert_eq!(ids(&ui), [1, 5, 6], "the drawer does not hold it back");
}

#[test]
fn malformed_faces_lists_are_rejected_with_range() {
    // Power-on state (no demo), so `still_default` holds at the end.
    let mut ui = Box::new(UiState::new());
    ui.reset();
    let bad = [
        faces_msg(&[(1, Template::Agents), (6, Template::Settings), (2, Template::Omarchy)]),
        faces_msg(&[(1, Template::Agents), (2, Template::Omarchy), (2, Template::Usage), (6, Template::Settings)]),
        faces_msg(&[(2, Template::Omarchy), (6, Template::Settings)]),
        faces_msg(&[(1, Template::Agents), (9, Template::Omarchy), (6, Template::Settings)]),
        faces_msg(&[(1, Template::Agents), (0, Template::Omarchy), (6, Template::Settings)]),
        faces_msg(&[(1, Template::Agents), (3, Template::Agents), (6, Template::Settings)]),
        faces_msg(&[(1, Template::Agents)]),
        faces_msg(&[
            (1, Template::Agents),
            (2, Template::Macropad),
            (3, Template::Macropad),
            (4, Template::Macropad),
            (5, Template::Macropad),
            (6, Template::Settings),
        ]),
    ];
    for (n, msg) in bad.iter().enumerate() {
        let out = host(&mut ui, msg, 5000 * n as u32);
        assert_eq!(events(&out), [Event::Err { why: ErrWhy::Range }], "list {n}");
        assert_eq!(ids(&ui), [1, 2, 3, 4, 7, 6, 5]);
    }
    assert!(ui.still_default());
}

#[test]
fn manager_touch_tap_goes_to_the_card_and_plus_opens_the_gallery() {
    let mut ui = fresh();
    manager(&mut ui);
    let card = layout::manager_card(1).center();
    let out = touch(&mut ui, TouchPhase::Down, card, 10);
    assert!(out.dirty && ui.manager.selected == 1 && ui.manager.pressed.is_none());
    let out = touch(&mut ui, TouchPhase::Up, card, 100);
    assert_eq!(ui.global.overlay, Overlay::None);
    assert_eq!(ui.current_face().template, Template::Omarchy);
    assert_eq!(events(&out), [Event::Face { id: 2 }]);
    manager(&mut ui);
    ui.manager.first_visible = 2;
    assert_eq!(tap(&mut ui, layout::manager_card(3).center(), 200), []);
    assert_eq!(ui.global.overlay, Overlay::Gallery);
    // Gallery rows and confirm buttons are tap targets with a pressed look.
    let row = layout::gallery_row(2).center();
    touch(&mut ui, TouchPhase::Down, row, 300);
    assert_eq!(ui.manager.pressed, Some(2));
    touch(&mut ui, TouchPhase::Up, row, 380);
    assert_eq!((ui.global.overlay, ui.manager.naming_template), (Overlay::Naming, Template::Usage));
    assert_eq!(ui.manager.pressed, None);
    press(&mut ui, Button::Cancel, 400);
    press(&mut ui, Button::Cancel, 410);
    ui.manager.selected = 2;
    press(&mut ui, Button::Cancel, 420);
    assert_eq!(ui.global.overlay, Overlay::ConfirmRemove);
    let keep = layout::confirm_button(0).center();
    touch(&mut ui, TouchPhase::Down, keep, 430);
    assert_eq!(ui.manager.pressed, Some(0));
    assert_eq!(events(&touch(&mut ui, TouchPhase::Up, keep, 500)), []);
    assert_eq!(ui.global.overlay, Overlay::Manager);
    press(&mut ui, Button::Cancel, 510);
    let remove = layout::confirm_button(1).center();
    assert_eq!(tap(&mut ui, remove, 520), [Event::Faces]);
    assert_eq!(ids(&ui), [1, 2, 4, 5]);
    assert_eq!(ui.global.overlay, Overlay::Manager);
}

#[test]
fn overlays_capture_everything_and_forward_nothing() {
    let mut ui = on(Template::Omarchy);
    for overlay in [Overlay::Manager, Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove] {
        ui.global.overlay = overlay;
        ui.manager.selected = 1;
        ui.invalidate();
        let mut all = events(&press(&mut ui, Button::Left, 10));
        all.extend(events(&button(&mut ui, Button::Left, RELEASE, 20)));
        all.extend(events(&press(&mut ui, Button::Right, 30)));
        all.extend(events(&button(&mut ui, Button::Right, RELEASE, 40)));
        all.extend(events(&wheel(&mut ui, Wheel::Ccw, 50)));
        assert_eq!(all, [], "{overlay:?}");
        assert_eq!(ui.global.overlay, overlay);
        assert_eq!(ui.current_face().template, Template::Omarchy);
    }
}
