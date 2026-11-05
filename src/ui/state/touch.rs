//! The unified touch recogniser and the top-edge gesture.
//!
//! A Down latches ONE thing for the whole gesture: the toast, the edge zone
//! or a `Target` of the layer on top. Every
//! latched target ends with exactly one of `Activate`, `Swipe` or `Cancel`;
//! a teardown (`orphan`) delivers the Cancel early and swallows the rest.
//!
//! - Edge: a Down at y < `EDGE_ZONE_H` never reaches the face. dy >= 40 with
//!   dy > |dx|, seen on Move or Up, opens the drawer; else an Up within 10 px
//!   on a tappable toast is a toast tap. A Down inside a tappable toast
//!   belongs to the toast first, on every face; the toast rect swallows
//!   touches for 300 ms after the toast expired.
//! - Tap / ListRow: highlight on Down, activate on Up inside, > 10 px off the
//!   rect cancels for good. A ListRow scrolls instead once the finger has
//!   travelled 16 px vertically (one row per `pitch` px after that, the tap
//!   is void), and an Up after a horizontal swipe (|dx| >= 60, |dx| > 2|dy|)
//!   is `Swipe`; only a drawer row keeps a finger that leaves it sideways.
//! - Press: pressed on Down, released (`Cancel`) on Up, leave or Cancel.
//! - Drag: never cancelled by sliding off; `tick` promotes a 350 ms hold
//!   within 10 px to `LongPress`, after which the Up is `Activate` anywhere.
//! A Down while the strip is sliding is swallowed; a Down during a LISTENING
//! session never gets here (`global::on_control` consumes it).
use super::{
    global, notify, touch_deliver, touch_hit, Layer, Source, Target, TargetKind, TouchAct,
    UiState, Update,
};
use crate::control::{Touch, TouchPhase};
use crate::ui::layout::{self, EDGE_ZONE_H};

/// Off-rect travel that cancels a tap, and the stillness a long-press needs.
pub const SLOP_PX: i32 = 10;
/// Vertical travel that turns a list tap into a scroll.
pub const SCROLL_PX: i32 = 16;
/// The drawer gesture.
pub const EDGE_SWIPE_PX: i32 = 40;
/// A horizontal swipe on a row.
pub const SWIPE_PX: i32 = 60;
pub const LONG_PRESS_MS: u32 = 350;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Latched {
    /// Nothing under the finger, or the gesture was cancelled or orphaned: the
    /// rest of it is swallowed.
    Dead,
    /// Went down in the top edge zone (or on the toast: `on_toast`).
    Edge,
    Target(Layer, Target),
    /// A `ListRow` whose tap is void: the finger scrolls the list now.
    Scrolling(Layer, Target),
}

#[derive(Clone, Copy)]
pub struct Recognizer {
    latched: Option<Latched>,
    start: (i32, i32),
    down_ms: u32,
    /// Farthest the finger has been from `start` (Chebyshev), in px.
    moved: i32,
    /// Scroll rows already delivered (+ = later rows).
    scrolled: i32,
    long_pressed: bool,
    /// The Down landed on the tappable toast.
    on_toast: bool,
}

impl Recognizer {
    pub const fn new() -> Self {
        Self {
            latched: None,
            start: (0, 0),
            down_ms: 0,
            moved: 0,
            scrolled: 0,
            long_pressed: false,
            on_toast: false,
        }
    }
    /// A finger is down (whatever it latched).
    pub fn active(&self) -> bool {
        self.latched.is_some()
    }
    /// When the current touch went down, for confirm guards.
    pub fn down_ms(&self) -> u32 {
        self.down_ms
    }
}

pub(super) fn on_touch(ui: &mut UiState, touch: Touch, at_ms: u32, out: &mut Update) {
    let (x, y) = (touch.x as i32, touch.y as i32);
    if touch.phase == TouchPhase::Down {
        down(ui, x, y, at_ms, out);
        return;
    }
    let Some(latched) = ui.touch.latched else {
        return;
    };
    let ended = matches!(touch.phase, TouchPhase::Up | TouchPhase::Cancel);
    let (dx, dy) = (x - ui.touch.start.0, y - ui.touch.start.1);
    ui.touch.moved = ui.touch.moved.max(dx.abs()).max(dy.abs());
    match latched {
        Latched::Dead => {}
        Latched::Edge => edge(ui, touch.phase, dx, dy, at_ms, out),
        Latched::Target(layer, target) => target_touch(ui, layer, target, touch.phase, x, y, at_ms, out),
        Latched::Scrolling(layer, target) => {
            if touch.phase == TouchPhase::Move {
                scroll(ui, layer, target, dy, at_ms, out);
            }
        }
    }
    if ended {
        ui.touch.latched = None;
        ui.touch.on_toast = false;
        global::unhold(ui, Source::Touch);
    }
}

fn down(ui: &mut UiState, x: i32, y: i32, at_ms: u32, out: &mut Update) {
    if ui.touch.latched.is_some() {
        orphan(ui, out);
    }
    ui.touch.start = (x, y);
    ui.touch.down_ms = at_ms;
    ui.touch.moved = 0;
    ui.touch.scrolled = 0;
    ui.touch.long_pressed = false;
    ui.touch.on_toast = false;
    let latched = if !ui.strip_at_rest() {
        Latched::Dead
    } else if notify::toast_tappable(ui) && layout::toast_at(x, y) {
        ui.touch.on_toast = true;
        ui.notify.toast_pressed = true;
        out.dirty = true;
        Latched::Edge
    } else if notify::swallows_touch(ui, x, y, at_ms) {
        Latched::Dead
    } else if y < EDGE_ZONE_H {
        Latched::Edge
    } else {
        match touch_hit(ui, x, y) {
            Some((layer, target)) => Latched::Target(layer, target),
            None => Latched::Dead,
        }
    };
    ui.touch.latched = Some(latched);
    match latched {
        Latched::Target(layer, target) => {
            touch_deliver(ui, layer, target, TouchAct::Down, at_ms, out)
        }
        Latched::Edge | Latched::Dead | Latched::Scrolling(..) => {}
    }
}

fn edge(ui: &mut UiState, phase: TouchPhase, dx: i32, dy: i32, at_ms: u32, out: &mut Update) {
    if phase != TouchPhase::Cancel && dy >= EDGE_SWIPE_PX && dy > dx.abs() {
        ui.touch.latched = Some(Latched::Dead);
        unpress_toast(ui, out);
        notify::open_drawer(ui, at_ms, out);
        return;
    }
    if !ui.touch.on_toast {
        return;
    }
    if ui.touch.moved > SLOP_PX || phase == TouchPhase::Cancel {
        ui.touch.on_toast = false;
        unpress_toast(ui, out);
    } else if phase == TouchPhase::Up {
        ui.touch.latched = Some(Latched::Dead);
        unpress_toast(ui, out);
        notify::toast_tapped(ui, at_ms, out);
    }
}

fn unpress_toast(ui: &mut UiState, out: &mut Update) {
    if ui.notify.toast_pressed {
        ui.notify.toast_pressed = false;
        out.dirty = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn target_touch(
    ui: &mut UiState,
    layer: Layer,
    target: Target,
    phase: TouchPhase,
    x: i32,
    y: i32,
    at_ms: u32,
    out: &mut Update,
) {
    let (dx, dy) = (x - ui.touch.start.0, y - ui.touch.start.1);
    let inside = target.rect.inset(-SLOP_PX).contains(x, y);
    let act = match (target.kind, phase) {
        (_, TouchPhase::Cancel) => Some(TouchAct::Cancel),
        (TargetKind::Drag, TouchPhase::Up) => Some(
            if ui.touch.long_pressed || target.rect.contains(x, y) {
                TouchAct::Activate
            } else {
                TouchAct::Cancel
            },
        ),
        (TargetKind::Drag, _) => None,
        (TargetKind::Press, TouchPhase::Up) => Some(TouchAct::Cancel),
        (TargetKind::Press, _) if !inside => Some(TouchAct::Cancel),
        (TargetKind::Press, _) => None,
        (TargetKind::ListRow, _)
            if phase == TouchPhase::Move && dy.abs() >= SCROLL_PX && dy.abs() >= dx.abs() =>
        {
            // The tap is void; the list follows the finger from here on.
            ui.touch.latched = Some(Latched::Scrolling(layer, target));
            touch_deliver(ui, layer, target, TouchAct::Cancel, at_ms, out);
            scroll(ui, layer, target, dy, at_ms, out);
            return;
        }
        (TargetKind::ListRow, TouchPhase::Up) if dx.abs() >= SWIPE_PX && dx.abs() > 2 * dy.abs() => {
            Some(TouchAct::Swipe { dx, dy })
        }
        (_, TouchPhase::Up) if inside => Some(TouchAct::Activate),
        (_, TouchPhase::Up) => Some(TouchAct::Cancel),
        // A drawer row keeps a sideways finger for its swipe; anything else is off.
        (TargetKind::ListRow, _) if layer == Layer::Notify && !inside && dx.abs() > dy.abs() => None,
        (_, _) if !inside => Some(TouchAct::Cancel),
        _ => None,
    };
    match act {
        Some(act) => {
            ui.touch.latched = Some(Latched::Dead);
            // A `Press` target's owner gets its act 0 from global.
            global::release_source(ui, Source::Touch, out);
            touch_deliver(ui, layer, target, act, at_ms, out);
        }
        None => touch_deliver(ui, layer, target, TouchAct::Move { x, y, dx, dy }, at_ms, out),
    }
}

/// Rows scrolled for a vertical travel of `dy`: the first at 16 px, one more
/// per `pitch` px; the content follows the finger (down = earlier rows).
fn scroll(ui: &mut UiState, layer: Layer, target: Target, dy: i32, at_ms: u32, out: &mut Update) {
    let pitch = (target.pitch as i32).max(1);
    let rows = if dy.abs() < SCROLL_PX { 0 } else { (dy.abs() - SCROLL_PX) / pitch + 1 };
    let total = if dy > 0 { -rows } else { rows };
    let delta = total - ui.touch.scrolled;
    if delta != 0 {
        ui.touch.scrolled = total;
        let step = delta.clamp(i8::MIN as i32, i8::MAX as i32) as i8;
        touch_deliver(ui, layer, target, TouchAct::Scroll(step), at_ms, out);
    }
}

/// Long-press promotion and other time-driven gesture steps.
pub(super) fn on_tick(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    if let Some(Latched::Target(layer, target)) = ui.touch.latched {
        if target.kind == TargetKind::Drag
            && !ui.touch.long_pressed
            && ui.touch.moved <= SLOP_PX
            && now_ms.wrapping_sub(ui.touch.down_ms) >= LONG_PRESS_MS
        {
            ui.touch.long_pressed = true;
            touch_deliver(ui, layer, target, TouchAct::LongPress, now_ms, out);
        }
    }
}

pub(super) fn animating(_ui: &UiState) -> bool {
    false
}

/// Teardown: the latched layer sees its Cancel now and the rest of the gesture
/// is swallowed (no Up activation in whatever comes next).
pub(super) fn orphan(ui: &mut UiState, out: &mut Update) {
    match ui.touch.latched {
        Some(Latched::Target(layer, target)) => {
            ui.touch.latched = Some(Latched::Dead);
            let now = ui.now_ms;
            touch_deliver(ui, layer, target, TouchAct::Cancel, now, out);
        }
        Some(Latched::Edge) => {
            ui.touch.latched = Some(Latched::Dead);
            ui.touch.on_toast = false;
            unpress_toast(ui, out);
        }
        // A scrolling row saw its Cancel when the scroll began.
        Some(_) => ui.touch.latched = Some(Latched::Dead),
        None => {}
    }
}

/// Link drop: the latched layer drops its pressed look (a Cancel; global has
/// already forgotten every hold, so nothing is emitted) and the rest of the
/// gesture is swallowed.
pub(super) fn forget(ui: &mut UiState, out: &mut Update) {
    orphan(ui, out);
    ui.touch.on_toast = false;
    unpress_toast(ui, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Button, Control, Edge};
    use crate::deck::model::Notice;
    use crate::deck::msg::{CardKind, Template};
    use crate::ui::layout::Rect;
    use crate::ui::state::{agents::AgentsView, Overlay};

    fn touch(ui: &mut UiState, phase: TouchPhase, x: i32, y: i32, at_ms: u32) -> Update {
        let mut out = Update::new();
        let touch = Touch {
            phase,
            x: x as u16,
            y: y as u16,
        };
        ui.apply(Control::Touch(touch), at_ms, &mut out);
        out
    }

    fn fresh() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.host_present = true;
        ui.deck.settings.demo = false;
        ui
    }

    /// Centre of the roster row showing agent `id` (demo order 1..5).
    fn row_of(ui: &UiState, id: u8) -> (i32, i32) {
        let row = ui.deck.row_of(id).unwrap() - ui.agents.top_row;
        layout::roster_row(row).center()
    }

    #[test]
    fn the_edge_zone_is_the_top_48_px_of_a_face() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.on_face(Template::Macropad);
        let out = touch(&mut ui, TouchPhase::Down, 240, EDGE_ZONE_H - 1, 80);
        assert_eq!(ui.touch.latched, Some(Latched::Edge));
        assert_eq!(out.deck_events().count(), 0);
    }

    #[test]
    fn choice_tap_activates_on_up_and_slide_off_cancels() {
        let mut ui = fresh();
        let (x, y) = row_of(&ui, 2);
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        assert_eq!(ui.agents.pressed, Some(2), "highlighted on Down, not activated");
        assert_eq!(ui.agents.highlight, 1);
        touch(&mut ui, TouchPhase::Move, x + 8, y + 6, 10);
        assert_eq!(ui.agents.pressed, Some(2), "within the slop");
        touch(&mut ui, TouchPhase::Up, x + 8, y + 6, 20);
        assert_eq!((ui.agents.pressed, ui.agents.highlight), (None, 2));
        // Sliding off cancels for good: coming back changes nothing.
        let (x, y) = row_of(&ui, 3);
        touch(&mut ui, TouchPhase::Down, x, y, 100);
        touch(&mut ui, TouchPhase::Move, x, y + 60, 110);
        assert_eq!(ui.agents.pressed, None);
        touch(&mut ui, TouchPhase::Move, x, y, 120);
        assert_eq!(ui.agents.pressed, None);
        touch(&mut ui, TouchPhase::Up, x, y, 130);
        assert_eq!(ui.agents.highlight, 2);
        assert!(!ui.touch.active());
    }

    #[test]
    fn cancel_phase_never_activates() {
        let mut ui = fresh();
        let (x, y) = row_of(&ui, 2);
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        touch(&mut ui, TouchPhase::Cancel, x, y, 10);
        assert_eq!((ui.agents.pressed, ui.agents.highlight), (None, 1));
        touch(&mut ui, TouchPhase::Up, x, y, 20);
        assert_eq!(ui.agents.highlight, 1);
    }

    #[test]
    fn vertical_drag_scrolls_a_list_by_row_and_voids_the_tap() {
        let mut ui = fresh();
        // One page of five rows: the demo's second project joins the first.
        for slot in 3..5 {
            ui.deck.agents[slot].project.set(crate::deck::demo::PROJECTS[0]);
        }
        assert_eq!((ui.deck.order_len, layout::ROSTER_VISIBLE), (5, 4));
        let (x, y) = row_of(&ui, 3);
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        assert_eq!(ui.agents.pressed, Some(3));
        touch(&mut ui, TouchPhase::Move, x + 3, y - 15, 10);
        assert_eq!((ui.agents.pressed, ui.agents.top_row), (Some(3), 0));
        touch(&mut ui, TouchPhase::Move, x + 3, y - 16, 20);
        assert_eq!((ui.agents.pressed, ui.agents.top_row), (None, 1), "16 px up shows later rows");
        touch(&mut ui, TouchPhase::Move, x + 3, y - 16 - layout::ROSTER_PITCH as i32, 30);
        assert_eq!(ui.agents.top_row, 1, "the list has only one row to give");
        touch(&mut ui, TouchPhase::Move, x + 3, y + 30, 40);
        assert_eq!(ui.agents.top_row, 0, "back down: earlier rows again");
        touch(&mut ui, TouchPhase::Up, x + 3, y + 30, 50);
        assert_eq!((ui.agents.highlight, ui.agents.view), (1, AgentsView::Roster));
    }

    #[test]
    fn fast_top_swipe_with_only_down_up_opens_drawer() {
        let mut ui = fresh();
        touch(&mut ui, TouchPhase::Down, 200, 30, 0);
        touch(&mut ui, TouchPhase::Up, 210, 75, 20);
        assert_eq!(ui.global.overlay, Overlay::Drawer);
        // A sideways or short one does not, nor does a Cancel.
        for (dx, dy, phase) in [(60, 45, TouchPhase::Up), (5, 39, TouchPhase::Up), (0, 80, TouchPhase::Cancel)] {
            let mut ui = fresh();
            touch(&mut ui, TouchPhase::Down, 200, 30, 0);
            touch(&mut ui, phase, 200 + dx, 30 + dy, 20);
            assert_eq!(ui.global.overlay, Overlay::None, "{dx} {dy} {phase:?}");
        }
        // The edge zone never reaches the face, whatever the Up does.
        let mut ui = fresh();
        touch(&mut ui, TouchPhase::Down, 200, 47, 0);
        touch(&mut ui, TouchPhase::Up, 200, 100, 20);
        assert_eq!((ui.global.overlay, ui.agents.pressed), (Overlay::Drawer, None));
    }

    #[test]
    fn tap_on_toast_starting_at_y20_jumps_not_drawer() {
        let mut ui = fresh();
        ui.on_face(Template::Usage);
        notify::raise(&mut ui, Notice::new(1, 1, CardKind::Question, 0), 0, &mut Update::new());
        touch(&mut ui, TouchPhase::Down, 240, 20, 100);
        assert!(ui.notify.toast_pressed);
        touch(&mut ui, TouchPhase::Move, 244, 26, 110);
        touch(&mut ui, TouchPhase::Up, 244, 26, 120);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 1));
        assert!(ui.notify.toast.is_none() && !ui.notify.toast_pressed);
        // A swipe that starts on the toast is still the drawer gesture.
        let mut ui = fresh();
        ui.on_face(Template::Usage);
        notify::raise(&mut ui, Notice::new(1, 1, CardKind::Question, 0), 0, &mut Update::new());
        touch(&mut ui, TouchPhase::Down, 240, 20, 100);
        touch(&mut ui, TouchPhase::Move, 240, 70, 110);
        assert_eq!(ui.global.overlay, Overlay::Drawer);
        assert!(!ui.notify.toast_pressed);
        touch(&mut ui, TouchPhase::Up, 240, 70, 120);
        assert_eq!(ui.current_face().template, Template::Usage);
        // Sliding more than 10 px off cancels the tap for good.
        let mut ui = fresh();
        notify::raise(&mut ui, Notice::new(1, 1, CardKind::Question, 0), 0, &mut Update::new());
        touch(&mut ui, TouchPhase::Down, 240, 40, 100);
        touch(&mut ui, TouchPhase::Move, 260, 40, 110);
        assert!(!ui.notify.toast_pressed);
        touch(&mut ui, TouchPhase::Up, 240, 40, 120);
        assert!(ui.notify.toast.is_some());
        assert_eq!(ui.agents.view, AgentsView::Roster);
    }

    #[test]
    fn the_old_toast_rect_swallows_touches_for_300ms_after_expiry() {
        let mut ui = fresh();
        ui.on_face(Template::Macropad);
        notify::raise(&mut ui, Notice::new(1, 1, CardKind::Question, 0), 0, &mut Update::new());
        assert!(notify::toast_tappable(&ui));
        ui.tick(notify::TOAST_MS, 0, true, &mut Update::new());
        assert!(ui.notify.toast.is_none());
        // Inside the old toast rect, within 300 ms: swallowed.
        assert!(notify::swallows_touch(&ui, 200, 60, notify::TOAST_MS + 299));
        touch(&mut ui, TouchPhase::Down, 200, 60, notify::TOAST_MS + 299);
        assert_eq!(ui.touch.latched, Some(Latched::Dead));
        touch(&mut ui, TouchPhase::Up, 200, 60, notify::TOAST_MS + 305);
        // After it, and outside the rect, the swallow never applies.
        assert!(!notify::swallows_touch(&ui, 200, 60, notify::TOAST_MS + 600));
        assert!(!notify::swallows_touch(&ui, 200, 100, notify::TOAST_MS + 100));
    }

    #[test]
    fn a_toast_under_an_opaque_overlay_is_not_tapped() {
        let mut ui = fresh();
        ui.global.overlay = Overlay::Manager;
        notify::raise(&mut ui, Notice::new(1, 1, CardKind::Question, 0), 0, &mut Update::new());
        ui.notify.toast = Some(notify::Toast::card(1, 1, CardKind::Question, 0));
        touch(&mut ui, TouchPhase::Down, 240, 40, 100);
        assert!(!ui.notify.toast_pressed);
        assert_eq!(ui.touch.latched, Some(Latched::Edge));
    }

    #[test]
    fn touch_during_a_slide_or_a_listening_session_is_swallowed() {
        let mut ui = fresh();
        let (x, y) = row_of(&ui, 2);
        ui.global.current = 1;
        ui.global.strip_x = 100 * crate::ui::state::STRIP_ONE;
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        assert_eq!(ui.touch.latched, Some(Latched::Dead));
        touch(&mut ui, TouchPhase::Up, x, y, 10);
        let mut ui = fresh();
        let mut out = Update::new();
        ui.apply(Control::Button(Button::PushToTalk, Edge::Press), 0, &mut out);
        assert!(ui.global.ptt.is_some());
        touch(&mut ui, TouchPhase::Down, x, y, 10);
        assert!(!ui.touch.active() && ui.agents.pressed.is_none());
        touch(&mut ui, TouchPhase::Up, x, y, 20);
        assert_eq!(ui.agents.highlight, 1);
        // A finger already down when the session starts is orphaned, and
        // its Up during the session still ends the gesture.
        ui.apply(Control::Button(Button::PushToTalk, Edge::Release), 30, &mut out);
        ui.host(&crate::deck::msg::HostMsg::Transcript { agent: 1, text: crate::deck::text::Text::from_str("done"), last: true, press: Some(1), failed: false, channel_tip: false , rec: false}, 31, &mut out);
        touch(&mut ui, TouchPhase::Down, x, y, 40);
        assert_eq!(ui.agents.pressed, Some(2));
        ui.apply(Control::Button(Button::PushToTalk, Edge::Press), 50, &mut out);
        assert!(ui.agents.pressed.is_none() && ui.touch.active());
        touch(&mut ui, TouchPhase::Up, x, y, 60);
        assert!(!ui.touch.active());
        assert_eq!(ui.agents.highlight, 1);
    }

    #[test]
    fn a_touch_held_across_a_teardown_is_orphaned_and_its_up_does_nothing() {
        let mut ui = fresh();
        let (x, y) = row_of(&ui, 2);
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        assert_eq!(ui.agents.pressed, Some(2));
        global::jump_to(&mut ui, 1, 10, &mut Update::new());
        assert_eq!(ui.agents.pressed, None);
        assert_eq!(ui.touch.latched, Some(Latched::Dead));
        ui.on_face(Template::Agents);
        touch(&mut ui, TouchPhase::Up, x, y, 20);
        assert_eq!(ui.agents.highlight, 1);
        assert!(!ui.touch.active());
    }

    #[test]
    fn long_press_via_tick_promotes_a_still_drag_target_and_cancel_reverts() {
        let mut ui = fresh();
        let rect = Rect::new(100, 100, 97, 141);
        let target = Target::drag(rect, 0);
        touch(&mut ui, TouchPhase::Down, 140, 160, 0);
        ui.touch.latched = Some(Latched::Target(Layer::Manager, target));
        ui.tick(LONG_PRESS_MS - 1, 0, true, &mut Update::new());
        assert!(!ui.touch.long_pressed);
        ui.tick(LONG_PRESS_MS, 0, true, &mut Update::new());
        assert!(ui.touch.long_pressed);
        // A drag target survives sliding off; Cancel ends it without activating.
        touch(&mut ui, TouchPhase::Move, 300, 160, 400);
        assert_eq!(ui.touch.latched, Some(Latched::Target(Layer::Manager, target)));
        touch(&mut ui, TouchPhase::Cancel, 300, 160, 410);
        assert!(!ui.touch.active());
        // A finger that moved never promotes.
        touch(&mut ui, TouchPhase::Down, 140, 160, 500);
        ui.touch.latched = Some(Latched::Target(Layer::Manager, target));
        touch(&mut ui, TouchPhase::Move, 152, 160, 510);
        ui.tick(500 + LONG_PRESS_MS, 0, true, &mut Update::new());
        assert!(!ui.touch.long_pressed);
        touch(&mut ui, TouchPhase::Up, 152, 160, 900);
    }

    #[test]
    fn scroll_steps_follow_the_pitch_in_both_directions() {
        let mut ui = fresh();
        let target = Target::list_row(Rect::new(32, 70, 416, 74), 9, 50);
        ui.touch.start = (240, 300);
        ui.touch.scrolled = 0;
        ui.touch.latched = Some(Latched::Scrolling(Layer::Notify, target));
        let steps = |ui: &mut UiState, dy: i32| {
            ui.touch.scrolled = 0;
            let before = ui.touch.scrolled;
            scroll(ui, Layer::Notify, target, dy, 0, &mut Update::new());
            ui.touch.scrolled - before
        };
        assert_eq!(steps(&mut ui, 15), 0);
        assert_eq!(steps(&mut ui, -16), 1);
        assert_eq!(steps(&mut ui, -65), 1);
        assert_eq!(steps(&mut ui, -66), 2);
        assert_eq!(steps(&mut ui, 116), -3);
    }

    /// Anything a face draws pressed for a latched finger or a held key.
    fn pressed(ui: &UiState) -> bool {
        ui.macropad.down != 0
            || ui.agents.pressed.is_some()
            || ui.omarchy.pressed_chip.is_some()
            || ui.omarchy.pressed_row.is_some()
            || ui.settings.pressed.is_some()
    }

    #[test]
    fn link_drop_under_a_key_or_finger_drops_the_pressed_look_on_every_face() {
        // A key held through the drop: no act 0, nothing drawn pressed after.
        let mut ui = fresh();
        ui.on_face(Template::Macropad);
        ui.apply(Control::Button(Button::Enter, Edge::Press), 100, &mut Update::new());
        assert_ne!(ui.macropad.down, 0);
        let mut out = Update::new();
        ui.set_connected(false, &mut out);
        assert_eq!((out.deck_events().count(), ui.macropad.down), (0, 0));
        let mut out = Update::new();
        ui.apply(Control::Button(Button::Enter, Edge::Release), 200, &mut out);
        ui.tick(2000, 0, true, &mut Update::new());
        assert_eq!((out.deck_events().count(), ui.macropad.down), (0, 0));
        // A finger down on each face's pressable thing.
        for (template, (x, y)) in [
            (Template::Macropad, layout::macro_key(1).center()),
            (Template::Agents, layout::roster_row(0).center()),
            (Template::Omarchy, layout::omarchy_chip(0).center()),
            (Template::Omarchy, layout::omarchy_window(0).center()),
            (Template::Settings, layout::settings_row(0).center()),
        ] {
            let mut ui = fresh();
            ui.on_face(template);
            touch(&mut ui, TouchPhase::Down, x, y, 100);
            assert!(pressed(&ui), "{template:?} at {x},{y}");
            let mut out = Update::new();
            ui.set_connected(false, &mut out);
            assert_eq!(out.deck_events().count(), 0, "{template:?}");
            assert!(!pressed(&ui), "{template:?} after the drop");
            assert_eq!(ui.touch.latched, Some(Latched::Dead));
            let out = touch(&mut ui, TouchPhase::Up, x, y, 200);
            ui.tick(2000, 0, true, &mut Update::new());
            assert_eq!(out.deck_events().count(), 0, "{template:?}");
            assert!(!pressed(&ui) && !ui.touch.active(), "{template:?} after the Up");
        }
    }

    #[test]
    fn omarchy_row_slide_off_sideways_does_not_activate() {
        let mut ui = fresh();
        ui.on_face(Template::Omarchy);
        let (x, y) = layout::omarchy_window(0).center();
        touch(&mut ui, TouchPhase::Down, x, y, 0);
        assert_eq!(ui.omarchy.pressed_row, Some(0));
        touch(&mut ui, TouchPhase::Move, 470, y, 10);
        assert_eq!((ui.omarchy.pressed_row, ui.touch.latched), (None, Some(Latched::Dead)));
        let out = touch(&mut ui, TouchPhase::Up, x, y, 20);
        assert_eq!(out.deck_events().count(), 0, "cancelled for good");
        assert!(!ui.touch.active());
        // A drawer row keeps the sideways finger: its Up is the dismiss swipe.
        let mut ui = fresh();
        for id in [30u16, 40] {
            let notice = Notice::new(3, id, CardKind::Question, 0);
            notify::raise(&mut ui, notice, 0, &mut Update::new());
        }
        ui.tick(notify::TOAST_MS, 0, true, &mut Update::new());
        touch(&mut ui, TouchPhase::Down, 200, 30, 100);
        touch(&mut ui, TouchPhase::Up, 200, 75, 110);
        assert_eq!(ui.global.overlay, Overlay::Drawer);
        let (x, y) = layout::drawer_row(0).center();
        touch(&mut ui, TouchPhase::Down, x, y, 200);
        let sel = ui.notify.drawer_sel;
        touch(&mut ui, TouchPhase::Move, 470, y + 5, 210);
        assert!(ui.touch.active() && ui.touch.latched != Some(Latched::Dead));
        touch(&mut ui, TouchPhase::Up, 470, y + 5, 220);
        assert_ne!(ui.notify.drawer_sel, sel, "the swipe dismissed the row");
        assert_eq!(ui.global.overlay, Overlay::Drawer);
    }
}
