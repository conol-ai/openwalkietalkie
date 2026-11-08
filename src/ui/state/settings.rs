//! Grammar of the Settings face, the deck's list grammar: SCROLL selects a
//! row, ENTER acts on it, and an adjustable value is changed with SCROLL
//! while it is being adjusted -> `deck.settings.set` + `Event::Set`.
//!
//! Rules (docs/agent-command-deck.md, "Settings"):
//! - SCROLL moves the selection through every row of every panel in order,
//!   the tab following it; hard ends. LEFT/RIGHT or a tab jump to a panel's
//!   first row.
//! - ENTER on a toggle flips it. ENTER on a value (speaker/mic 0..=10,
//!   brightness 1..=5, idle-home through `IDLE_HOME_STEPS`, text size)
//!   starts adjusting it: SCROLL then steps it by one (hard ends) and ENTER
//!   or CANCEL ends adjusting. Read-only rows ignore ENTER. A change is
//!   applied locally through `Settings::set` and reported once as `set`.
//!   Language instead increments its persisted revision and sends `language`
//!   only after the current host has acknowledged support for that protocol.
//! - CANCEL while not adjusting goes home. Leaving the face ends adjusting.
//! - Turning demo data on/off without a deck host loads/clears the built-in
//!   data at once.
//! - `needs_host`: a host-side setting (speaker, mic, announce, chime) has no
//!   effect without a deck host and not in the demo; the renderer tags it.
//! - Two rows act instead of adjusting: Display > Tutorial replays the
//!   tutorial (`tutorial::open`), About > Factory reset opens the
//!   `ConfirmReset` dialog, where ENTER (or a tap on Reset) sets
//!   `Update::factory_reset` and CANCEL (or Keep) closes it.
use super::{agents, global, global::Overlay, Target, TouchAct, UiState, Update};
use crate::control::{Button, Control, Edge, Wheel};
use crate::deck::demo;
use crate::deck::model::{Effects, Settings};
use crate::deck::msg::{Event, HostMsg, SettingKey};
use crate::ui::layout;
use crate::ui::i18n::Language;
use crate::ui::kit::list::Viewport;

/// The idle-home values SCROLL steps through, in seconds; 0 = never.
pub const IDLE_HOME_STEPS: [u16; 4] = [0, 30, 60, 120];
/// `View::pressed` of a touched factory-reset button: `CONFIRM_PRESSED` +
/// its `layout::confirm_button` index (0 keep, 1 reset).
pub const CONFIRM_PRESSED: u8 = 32;

#[derive(Clone, Copy)]
pub struct View {
    /// 0..6: Volume, Speech, Alerts, Display, Host, About.
    pub panel: u8,
    /// Selected row inside the panel.
    pub row: u8,
    /// SCROLL is changing the selected row's value, not the selection.
    pub adjusting: bool,
    /// What a latched touch shows pressed, = the latched `Target::id`:
    /// 0..6 = tab (panel index), 8 + n = row n of the current panel,
    /// `CONFIRM_PRESSED` + n = a factory-reset dialog button.
    pub pressed: Option<u8>,
}

impl View {
    pub const fn new() -> Self {
        Self {
            panel: 0,
            row: 0,
            adjusting: false,
            pressed: None,
        }
    }
}

/// Applied by the host, not the device: needs a deck host to take effect.
pub const fn host_side(key: SettingKey) -> bool {
    matches!(
        key,
        SettingKey::Speaker | SettingKey::Mic | SettingKey::Announce | SettingKey::Chime
    )
}

pub const fn is_toggle(key: SettingKey) -> bool {
    matches!(
        key,
        SettingKey::Announce | SettingKey::Chime | SettingKey::AutoJump | SettingKey::Demo
    )
}

/// The renderer's "needs host" tag: a host-side setting with nobody to apply it.
pub fn needs_host(ui: &UiState, key: SettingKey) -> bool {
    host_side(key) && !ui.deck.host_present && !ui.deck.demo_active()
}

/// The value one detent away, at the range's hard ends the same value.
fn stepped(settings: &Settings, key: SettingKey, up: bool) -> u16 {
    let value = settings.get(key);
    match key {
        SettingKey::Speaker | SettingKey::Mic => {
            if up {
                (value + 1).min(10)
            } else {
                value.saturating_sub(1)
            }
        }
        SettingKey::Brightness => {
            if up {
                (value + 1).min(5)
            } else {
                value.saturating_sub(1).max(1)
            }
        }
        SettingKey::IdleHome => {
            let at = IDLE_HOME_STEPS
                .iter()
                .position(|&step| step == value)
                .unwrap_or(IDLE_HOME_STEPS.len() - 1);
            let next = if up {
                (at + 1).min(IDLE_HOME_STEPS.len() - 1)
            } else {
                at.saturating_sub(1)
            };
            IDLE_HOME_STEPS[next]
        }
        SettingKey::ArcadeSound => if up { (value + 1) % 3 } else { (value + 2) % 3 },
        SettingKey::TextSize => {
            if up {
                (value + 1).min(2)
            } else {
                value.saturating_sub(1)
            }
        }
        SettingKey::Announce | SettingKey::Chime | SettingKey::AutoJump | SettingKey::Demo => {
            up as u16
        }
    }
}

/// Apply `value` locally and tell the host once. True when it changed.
fn set(ui: &mut UiState, key: SettingKey, value: u16, at_ms: u32, out: &mut Update) -> bool {
    if !ui.deck.settings.set(key, value) {
        return false;
    }
    out.push_deck(Event::Set { key, value: ui.deck.settings.get(key) });
    out.dirty = true;
    if key == SettingKey::Demo && !ui.deck.host_present {
        if ui.deck.settings.demo {
            demo::load(&mut ui.deck);
        } else {
            ui.deck.clear_host_data();
            ui.notify.toast = None;
            agents::show_roster(ui, at_ms);
        }
        agents::on_reset(ui);
    }
    true
}

fn select_panel(ui: &mut UiState, panel: u8, out: &mut Update) {
    stop_adjusting(ui, out);
    if panel < PANELS && (ui.settings.panel != panel || ui.settings.row != 0) {
        ui.settings.panel = panel;
        ui.settings.row = 0;
        out.dirty = true;
    }
}

fn stop_adjusting(ui: &mut UiState, out: &mut Update) {
    out.dirty |= ui.settings.adjusting;
    ui.settings.adjusting = false;
}

/// The row one detent away across panels, or `None` at either end.
fn neighbour(panel: u8, row: u8, forward: bool) -> Option<(u8, u8)> {
    if forward {
        if row + 1 < rows(panel) {
            Some((panel, row + 1))
        } else {
            (panel + 1 < PANELS).then_some((panel + 1, 0))
        }
    } else if row > 0 {
        Some((panel, row - 1))
    } else {
        (panel > 0).then(|| (panel - 1, rows(panel - 1).saturating_sub(1)))
    }
}

/// ENTER (or a tap on the selected row): flip a toggle, start adjusting a
/// value, act on the two action rows, nothing on a read-only row.
fn activate(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if (ui.settings.panel, ui.settings.row) == LANGUAGE_ROW {
        ui.settings.adjusting = true;
        out.dirty = true;
        return;
    }
    let Some(key) = setting_at(ui.settings.panel, ui.settings.row) else {
        match (ui.settings.panel, ui.settings.row) {
            TUTORIAL_ROW => super::tutorial::open(ui, true, at_ms, out),
            FACTORY_RESET_ROW => {
                ui.settings.pressed = None;
                global::set_overlay(ui, Overlay::ConfirmReset, at_ms, out);
            }
            _ => {}
        }
        return;
    };
    if is_toggle(key) {
        let value = u16::from(ui.deck.settings.get(key) == 0);
        set(ui, key, value, at_ms, out);
    } else {
        ui.settings.adjusting = true;
        out.dirty = true;
    }
}

/// The dialog's answer: Reset raises `Update::factory_reset` for the runtime
/// (which erases and restarts) and closes; Keep just closes.
fn answer_reset(ui: &mut UiState, reset: bool, at_ms: u32, out: &mut Update) {
    ui.settings.pressed = None;
    global::set_overlay(ui, Overlay::None, at_ms, out);
    out.factory_reset |= reset;
    out.dirty = true;
}

/// Every wheel detent and button press while the factory-reset dialog is
/// open: ENTER resets, CANCEL keeps, the rest is swallowed.
pub(super) fn on_reset_control(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) -> bool {
    if ui.global.shift {
        return true;
    }
    match control {
        Control::Button(Button::Enter, Edge::Press) => answer_reset(ui, true, at_ms, out),
        Control::Button(Button::Cancel, Edge::Press) => answer_reset(ui, false, at_ms, out),
        _ => {}
    }
    true
}

/// The dialog's two buttons, as `layout::confirm_button` lays them out.
pub(super) fn reset_hit(_ui: &UiState, x: i32, y: i32) -> Option<Target> {
    let button = layout::confirm_button_at(x, y)?;
    Some(Target::tap(layout::confirm_button(button), CONFIRM_PRESSED as u16 + button as u16))
}

/// Wheel detents and button presses while this face is current and no overlay
/// is open. `true` = consumed (a locally handled face swallows everything).
pub(super) fn on_control(
    ui: &mut UiState,
    _instance: u8,
    control: Control,
    at_ms: u32,
    out: &mut Update,
) -> bool {
    if ui.global.shift {
        return true; // No chords on this face; never fall through.
    }
    let (panel, row) = (ui.settings.panel, ui.settings.row);
    match control {
        Control::Wheel(wheel) if ui.settings.adjusting => {
            if (panel, row) == LANGUAGE_ROW {
                let index = ui.language as usize;
                let next = if wheel == Wheel::Cw {
                    (index + 1).min(Language::ALL.len() - 1)
                } else {
                    index.saturating_sub(1)
                };
                let language = Language::ALL[next];
                if ui.language != language {
                    ui.language = language;
                    ui.language_revision = ui.language_revision.saturating_add(1);
                    out.language = Some(language);
                    out.dirty = true;
                    if ui.language_sync_supported && ui.deck.host_present {
                        out.push_deck(Event::Language);
                    }
                }
            } else if let Some(key) = setting_at(panel, row) {
                let value = stepped(&ui.deck.settings, key, wheel == Wheel::Cw);
                set(ui, key, value, at_ms, out);
            }
        }
        Control::Wheel(wheel) => {
            if let Some((panel, row)) = neighbour(panel, row, super::selection_step(wheel) > 0) {
                ui.settings.panel = panel;
                ui.settings.row = row;
                out.dirty = true;
            }
        }
        Control::Button(Button::Cancel | Button::Enter, Edge::Press) if ui.settings.adjusting => {
            stop_adjusting(ui, out);
        }
        Control::Button(Button::Cancel, Edge::Press) => global::go_home(ui, at_ms, out),
        Control::Button(Button::Enter, Edge::Press) => activate(ui, at_ms, out),
        Control::Button(button @ (Button::Left | Button::Right), Edge::Press) => {
            let next = panel as i32 + if button == Button::Right { 1 } else { -1 };
            if (0..PANELS as i32).contains(&next) {
                select_panel(ui, next as u8, out);
            }
        }
        _ => {}
    }
    true
}

/// The touch target under (x, y), if any. Pure. Ids as `View::pressed`.
pub(super) fn hit(ui: &UiState, _instance: u8, x: i32, y: i32) -> Option<Target> {
    if let Some(tab) = layout::settings_tab_at(x, y) {
        return Some(Target::tap(layout::settings_tab(tab), tab as u16));
    }
    let slot = layout::settings_row_at(x, y)?;
    let row = viewport(ui).index_at(slot)?;
    Some(Target::list_row(layout::settings_row(slot), 8 + row as u16, 66))
}

/// One act of a touch that latched a target of this face: a tab switches
/// panels, a row selects it, a tap on the selected row acts as ENTER does.
pub(super) fn on_touch(
    ui: &mut UiState,
    _instance: u8,
    target: Target,
    act: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    let id = target.id.min(255) as u8;
    match act {
        TouchAct::Down => ui.settings.pressed = Some(id),
        TouchAct::Activate if id >= CONFIRM_PRESSED => {
            if ui.global.overlay == Overlay::ConfirmReset {
                answer_reset(ui, id - CONFIRM_PRESSED == 1, at_ms, out);
            }
            ui.settings.pressed = None;
        }
        TouchAct::Activate => {
            ui.settings.pressed = None;
            if id < 8 {
                select_panel(ui, id, out);
            } else if id - 8 < rows(ui.settings.panel) {
                let row = id - 8;
                if ui.settings.row != row {
                    stop_adjusting(ui, out);
                    ui.settings.row = row;
                } else if ui.settings.adjusting {
                    stop_adjusting(ui, out);
                } else {
                    activate(ui, at_ms, out);
                }
            }
        }
        TouchAct::Cancel | TouchAct::Swipe { .. } => ui.settings.pressed = None,
        TouchAct::Scroll(steps) => {
            stop_adjusting(ui, out);
            ui.settings.pressed = None;
            let next = (ui.settings.row as i32 + steps as i32)
                .clamp(0, rows(ui.settings.panel).saturating_sub(1) as i32);
            ui.settings.row = next as u8;
        }
        _ => return,
    }
    out.dirty = true;
}

/// After `Deck::apply`: a host `set` is already applied; nothing to clamp.
pub(super) fn on_host(
    _ui: &mut UiState,
    _msg: &HostMsg,
    _fx: &Effects,
    _at_ms: u32,
    _out: &mut Update,
) {
}

/// Adjusting ends once the face is no longer the one on top (a slide away,
/// home, an overlay): coming back must not find SCROLL changing a value.
pub(super) fn on_tick(ui: &mut UiState, _now_ms: u32, out: &mut Update) {
    let on_top = ui.global.overlay == global::Overlay::None
        && ui.current_face().template == crate::deck::msg::Template::Settings;
    if ui.settings.adjusting && !on_top {
        stop_adjusting(ui, out);
    }
}

pub(super) fn animating(_ui: &UiState) -> bool {
    false
}

pub const PANELS: u8 = 6;
/// "Speech" (auto-announce) is the one panel name that fits a 66 px tab in
/// the Small face; "Announce" would drop that tab alone to the Hint face.
pub const PANEL_NAMES: [&str; PANELS as usize] =
    ["Volume", "Speech", "Alerts", "Display", "Host", "About"];

/// Rows of a panel (adjustable, action and read-only alike).
pub const fn rows(panel: u8) -> u8 {
    match panel {
        0 => 2, // speaker, mic
        1 => 1, // auto-announce
        2 => 2, // chime, auto-jump
        3 => 5, // brightness, idle-home, text size, language, tutorial
        4 => 3, // link, deck host, demo data
        _ => 4, // firmware, battery, power off, factory reset
    }
}

/// The two rows that act on ENTER instead of adjusting a setting.
pub const LANGUAGE_ROW: (u8, u8) = (3, 3);
pub const TUTORIAL_ROW: (u8, u8) = (3, 4);
pub const FACTORY_RESET_ROW: (u8, u8) = (5, 3);

/// Four physical row lanes, following the selection on longer panels.
pub fn viewport(ui: &UiState) -> Viewport {
    let mut view = Viewport::new(0, layout::SETTINGS_ROWS, rows(ui.settings.panel));
    view.follow(ui.settings.row);
    view
}

/// The setting a row adjusts; `None` = an action row (`TUTORIAL_ROW`,
/// `FACTORY_RESET_ROW`), a read-only row (Host: link, deck host; the rest
/// of About) or no such row. The grammar and the renderer both go through
/// this, so they cannot disagree about what a row is.
pub const fn setting_at(panel: u8, row: u8) -> Option<SettingKey> {
    match (panel, row) {
        (0, 0) => Some(SettingKey::Speaker),
        (0, 1) => Some(SettingKey::Mic),
        (1, 0) => Some(SettingKey::Announce),
        (2, 0) => Some(SettingKey::Chime),
        (2, 1) => Some(SettingKey::AutoJump),
        (3, 0) => Some(SettingKey::Brightness),
        (3, 1) => Some(SettingKey::IdleHome),
        (3, 2) => Some(SettingKey::TextSize),
        (4, 2) => Some(SettingKey::Demo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_has_exactly_one_row_inside_its_panel() {
        let mut seen = std::vec::Vec::new();
        for panel in 0..PANELS {
            for row in 0..8 {
                if let Some(key) = setting_at(panel, row) {
                    assert!(row < rows(panel), "{key:?}");
                    assert!(!seen.contains(&key), "{key:?}");
                    seen.push(key);
                }
            }
        }
        assert_eq!(seen.len(), 9);
        assert_eq!(setting_at(PANELS, 0), None);
        for (panel, row) in [TUTORIAL_ROW, FACTORY_RESET_ROW] {
            assert!(row < rows(panel) && setting_at(panel, row).is_none());
        }
    }

    fn on_row(row: (u8, u8)) -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.on_face(crate::deck::msg::Template::Settings);
        ui.settings.panel = row.0;
        ui.settings.row = row.1;
        ui
    }

    fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(Control::Button(button, Edge::Press), at, &mut out);
        ui.apply(Control::Button(button, Edge::Release), at + 20, &mut Update::new());
        out
    }

    #[test]
    fn enter_on_the_tutorial_row_replays_the_tutorial() {
        let mut ui = on_row(TUTORIAL_ROW);
        let out = press(&mut ui, Button::Enter, 10);
        assert!(out.dirty && !ui.settings.adjusting);
        assert_eq!(ui.global.overlay, Overlay::Tutorial);
        assert!(ui.tutorial.replay);
    }

    #[test]
    fn language_is_adjusted_locally_without_a_host_setting_event() {
        let mut ui = on_row(LANGUAGE_ROW);
        ui.deck.settings.demo = false;
        ui.deck.host_present = false;
        press(&mut ui, Button::Enter, 10);
        assert!(ui.settings.adjusting);
        for language in Language::ALL.into_iter().skip(1) {
            let mut out = Update::new();
            ui.apply(Control::Wheel(Wheel::Cw), 20, &mut out);
            assert_eq!(ui.language, language);
            assert_eq!(out.language, Some(language));
            assert!(out.dirty);
            assert!(out.deck_events().next().is_none(), "UI language never enters the host settings protocol");
        }
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Cw), 30, &mut out);
        assert_eq!(out.language, None, "the last language is a hard end");
        ui.apply(Control::Wheel(Wheel::Ccw), 40, &mut out);
        assert_eq!(ui.language, Language::TraditionalChinese);
        press(&mut ui, Button::Enter, 50);
        assert!(!ui.settings.adjusting);
    }

    #[test]
    fn display_viewport_and_touch_scroll_reach_language_and_tutorial() {
        let mut ui = on_row(LANGUAGE_ROW);
        assert_eq!(viewport(&ui).top, 0);
        let (x, y) = layout::settings_row(3).center();
        let target = hit(&ui, 5, x, y).unwrap();
        assert_eq!(target.id, 8 + LANGUAGE_ROW.1 as u16);
        assert_eq!(target.kind, super::super::TargetKind::ListRow);
        on_touch(&mut ui, 5, target, TouchAct::Scroll(1), 10, &mut Update::new());
        assert_eq!(ui.settings.row, TUTORIAL_ROW.1);
        assert_eq!(viewport(&ui).top, 1);
        let target = hit(&ui, 5, x, y).unwrap();
        assert_eq!(target.id, 8 + TUTORIAL_ROW.1 as u16);
        on_touch(&mut ui, 5, target, TouchAct::Activate, 20, &mut Update::new());
        assert_eq!(ui.global.overlay, Overlay::Tutorial);
        assert!(ui.tutorial.replay);
    }

    #[test]
    fn versioned_language_handshake_syncs_changes_without_echo_or_boot_semantic_changes() {
        let mut ui = on_row(LANGUAGE_ROW);
        ui.deck.settings.demo = false;
        ui.host(&HostMsg::Hello { v: 1, sid: 7, minute_of_day: None }, 1, &mut Update::new());
        assert!(!ui.language_sync_supported, "an old host never receives new message kinds");
        ui.host(&HostMsg::Language { value: 0, revision: 0 }, 2, &mut Update::new());
        assert!(ui.language_sync_supported);
        press(&mut ui, Button::Enter, 10);
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Cw), 40, &mut out);
        assert_eq!(ui.language_revision, 1);
        assert_eq!(out.deck_events().collect::<std::vec::Vec<_>>(), [Event::Language]);
        let mut out = Update::new();
        ui.host(&HostMsg::Language { value: 3, revision: 0 }, 41, &mut out);
        assert_eq!(ui.language, Language::SimplifiedChinese, "a stale in-flight app sync loses to a newer local change");
        ui.host(&HostMsg::Language { value: 2, revision: 1 }, 42, &mut out);
        assert_eq!(ui.language, Language::TraditionalChinese, "app wins a revision tie");
        assert!(out.deck_events().next().is_none(), "host changes never mirror back");
        assert_eq!(out.language, Some(Language::TraditionalChinese));
        assert!(ui.still_default(), "persisted language is independent of boot settings freshness");
        ui.set_connected(false, &mut Update::new());
        assert!(!ui.language_sync_supported);
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Cw), 43, &mut out);
        assert_eq!(ui.language_revision, 2);
        assert!(out.deck_events().next().is_none());
    }

    #[test]
    fn factory_reset_asks_first_and_only_enter_or_the_reset_button_confirms() {
        let mut ui = on_row(FACTORY_RESET_ROW);
        let out = press(&mut ui, Button::Enter, 10);
        assert_eq!(ui.global.overlay, Overlay::ConfirmReset);
        assert!(out.dirty && !out.factory_reset);
        // A detent and SHIFT do nothing; CANCEL keeps.
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Cw), 20, &mut out);
        assert_eq!((ui.settings.row, ui.global.overlay), (FACTORY_RESET_ROW.1, Overlay::ConfirmReset));
        let out = press(&mut ui, Button::Cancel, 30);
        assert!(!out.factory_reset && ui.global.overlay == Overlay::None);
        assert_eq!(ui.current_face().template, crate::deck::msg::Template::Settings, "keep is not home");
        press(&mut ui, Button::Enter, 40);
        let out = press(&mut ui, Button::Enter, 50);
        assert!(out.factory_reset && ui.global.overlay == Overlay::None);
        // By touch: the Reset button confirms, Keep does not.
        press(&mut ui, Button::Enter, 60);
        let (x, y) = layout::confirm_button(1).center();
        let (layer, target) = super::super::touch_hit(&ui, x, y).unwrap();
        assert_eq!(target.id, CONFIRM_PRESSED as u16 + 1);
        let mut out = Update::new();
        super::super::touch_deliver(&mut ui, layer, target, TouchAct::Down, 70, &mut out);
        assert_eq!(ui.settings.pressed, Some(CONFIRM_PRESSED + 1));
        super::super::touch_deliver(&mut ui, layer, target, TouchAct::Activate, 80, &mut out);
        assert!(out.factory_reset && ui.settings.pressed.is_none() && ui.global.overlay == Overlay::None);
        press(&mut ui, Button::Enter, 90);
        let (x, y) = layout::confirm_button(0).center();
        let (layer, target) = super::super::touch_hit(&ui, x, y).unwrap();
        let mut out = Update::new();
        super::super::touch_deliver(&mut ui, layer, target, TouchAct::Activate, 100, &mut out);
        assert!(!out.factory_reset && ui.global.overlay == Overlay::None);
    }

    #[test]
    fn stepping_has_hard_ends_and_known_idle_home_values() {
        let mut settings = Settings::new();
        settings.speaker = 10;
        assert_eq!(stepped(&settings, SettingKey::Speaker, true), 10);
        settings.speaker = 0;
        assert_eq!(stepped(&settings, SettingKey::Speaker, false), 0);
        assert_eq!(stepped(&settings, SettingKey::Mic, true), 7);
        settings.brightness = 1;
        assert_eq!(stepped(&settings, SettingKey::Brightness, false), 1);
        assert_eq!(stepped(&settings, SettingKey::Brightness, true), 2);
        assert_eq!(stepped(&settings, SettingKey::IdleHome, true), 120);
        assert_eq!(stepped(&settings, SettingKey::IdleHome, false), 60);
        settings.idle_home_s = 0;
        assert_eq!(stepped(&settings, SettingKey::IdleHome, false), 0);
        assert_eq!(stepped(&settings, SettingKey::IdleHome, true), 30);
        assert_eq!(stepped(&settings, SettingKey::Chime, false), 0);
        assert_eq!(stepped(&settings, SettingKey::Chime, true), 1);
        assert!(is_toggle(SettingKey::Demo) && !is_toggle(SettingKey::Mic));
        assert!(host_side(SettingKey::Chime) && !host_side(SettingKey::Brightness));
    }
}
