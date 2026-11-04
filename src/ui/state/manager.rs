//! Grammar of the face manager and its children: gallery, naming, remove
//! confirm. Captures every face-layer control while one of them is open.
//!
//! Rules (docs/agent-command-deck.md, "Face manager"):
//! - Cards are [Agents, …user faces…, Settings, "+"]; "+" is hidden at
//!   `MAX_FACES`. Agents and Settings are pinned: never grabbed, never
//!   displaced, never removed. The 4-slot viewport follows the selection.
//! - Opens with `selected` = the current face. SCROLL selects, or moves the
//!   grabbed card one slot. ENTER: grab / drop a movable card, go to a pinned
//!   one, open the gallery on "+". CANCEL: revert a grab, else ask to remove a
//!   removable card, else nothing. Double-SHIFT (global) and a tap exit to the
//!   selected face; "+" exits to the face the manager opened on.
//! - Touch: tap = go, a quick horizontal drag scrolls the row, a long-press
//!   (from `tick`, < 10 px) grabs; then the card snaps slot to slot under the
//!   finger, Up drops, Cancel reverts.
//! - Gallery -> Naming (host `name` applies only there; ENTER accepts the
//!   text, or the template's label; CANCEL goes back). A new face gets the
//!   lowest free id and goes in before Settings. At most `MAX_MACRO_SETS`
//!   Macropads and one Timer (its clocks are one per deck).
//! - Every strip rewrite keeps `current` (and the origin) on the same face by
//!   id; a removed current face falls back to its left neighbour. One `faces`
//!   event per completed change (drop, remove, add); a reverted grab is no
//!   change. Idle-home (`cancel_all`) closes silently.
//! - Host `faces` (strip restore) is applied here, never while the manager
//!   is open, and never echoed.
use super::{
    global, global::Overlay, Target, TargetKind, TouchAct, UiState, Update, FACE_W, MAX_FACES,
    STRIP_ONE,
};
use crate::control::{Button, Control, Edge};
use crate::deck::model::{Effects, MAX_MACRO_SETS};
use crate::deck::msg::{ErrWhy, Event, FaceEntry, HostMsg, Template};
use crate::deck::text::Text;
use crate::ui::kit::list::Viewport;
use crate::ui::layout;

/// Templates the gallery offers, in row order.
pub const GALLERY: [Template; 6] = [
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::Arcade,
    Template::System,
    Template::Timer,
];

/// Touch target ids: gallery rows are 0..GALLERY.len(), confirm buttons `CONFIRM_ID + n`.
const CONFIRM_ID: u16 = 16;

#[derive(Clone, Copy)]
pub struct View {
    /// Card index: strip index, or `strip_len` for the "+" card.
    pub selected: u8,
    pub grabbed: bool,
    /// First card of the 4-slot viewport.
    pub first_visible: u8,
    /// Row of `GALLERY`.
    pub gallery_sel: u8,
    pub naming_template: Template,
    pub naming_text: Text<16>,
    /// Face id the confirm dialog is about.
    pub confirm_face: u8,
    /// Strip index to return to when "+" is selected on exit.
    pub origin: u8,
    /// The target a finger is down on, for its pressed look: a gallery row in
    /// Gallery, a `layout::confirm_button` index in ConfirmRemove. `None` in
    /// Manager, whose touched card shows through `selected` / `grabbed`.
    pub pressed: Option<u8>,
    /// Where the grabbed card was picked up, for the revert.
    grab_from: u8,
    /// Face id the manager opened on: what `origin` follows across rewrites.
    origin_id: u8,
    /// The strip changed since the last `faces` event.
    changed: bool,
    /// The finger scrolled the row: its Up is not a tap.
    touch_scrolled: bool,
    /// `first_visible` when the finger went down (quick horizontal drag).
    touch_first: u8,
}

impl View {
    pub const fn new() -> Self {
        Self {
            selected: 0,
            grabbed: false,
            first_visible: 0,
            gallery_sel: 0,
            naming_template: Template::Omarchy,
            naming_text: Text::new(),
            confirm_face: 0,
            origin: 0,
            pressed: None,
            grab_from: 0,
            origin_id: 0,
            changed: false,
            touch_scrolled: false,
            touch_first: 0,
        }
    }
}

const fn is_open(overlay: Overlay) -> bool {
    matches!(
        overlay,
        Overlay::Manager | Overlay::Gallery | Overlay::Naming | Overlay::ConfirmRemove
    )
}

fn strip_len(ui: &UiState) -> u8 {
    ui.global.strip_len.min(MAX_FACES as u8)
}

/// Faces plus the "+" card, which is hidden at `MAX_FACES`.
fn card_count(ui: &UiState) -> u8 {
    let len = strip_len(ui);
    len + u8::from((len as usize) < MAX_FACES)
}

fn is_plus(ui: &UiState, index: u8) -> bool {
    index >= strip_len(ui)
}

/// Agents, Settings and "+" stay where they are.
fn pinned(ui: &UiState, index: u8) -> bool {
    ui.face_at(index)
        .is_none_or(|face| matches!(face.template, Template::Agents | Template::Settings))
}

/// Moving `from` to `to` displaces every card between them.
fn can_move(ui: &UiState, from: u8, to: u8) -> bool {
    to < strip_len(ui)
        && (from.min(to)..=from.max(to)).all(|index| index == from || !pinned(ui, index))
}

fn viewport(ui: &UiState) -> Viewport {
    Viewport::new(ui.manager.first_visible, layout::MANAGER_VISIBLE, card_count(ui))
}

/// True when the selection or the viewport moved (a direct poke of the pub
/// fields is clamped here, which is a visible change).
fn select(ui: &mut UiState, index: u8) -> bool {
    let before = (ui.manager.selected, ui.manager.first_visible);
    ui.manager.selected = index.min(card_count(ui).saturating_sub(1));
    let mut view = viewport(ui);
    view.follow(ui.manager.selected);
    ui.manager.first_visible = view.top;
    before != (ui.manager.selected, ui.manager.first_visible)
}

/// Move strip[from] to `to`, shifting what lies between.
fn move_face(ui: &mut UiState, from: u8, to: u8) {
    let len = strip_len(ui) as usize;
    let (from, to) = (from as usize, to as usize);
    if from >= len || to >= len || from == to {
        return;
    }
    let strip = &mut ui.global.strip[..len];
    if from < to {
        strip[from..=to].rotate_left(1);
    } else {
        strip[to..=from].rotate_right(1);
    }
}

/// The strip was rewritten: `current` and the origin stay on the same faces
/// (`fallback` when the current one is gone), the selection stays in range.
fn refit(ui: &mut UiState, current_id: u8, fallback: u8) {
    let len = strip_len(ui);
    let index = ui
        .index_of_face(current_id)
        .unwrap_or(fallback.min(len.saturating_sub(1)));
    ui.global.current = index;
    ui.global.strip_x = index as i32 * FACE_W * STRIP_ONE;
    ui.manager.origin = ui.index_of_face(ui.manager.origin_id).unwrap_or(index);
    ui.manager.changed = true;
    select(ui, ui.manager.selected);
}

/// One `faces` per completed change.
fn commit(ui: &mut UiState, out: &mut Update) {
    if ui.manager.changed {
        ui.manager.changed = false;
        out.push_deck(Event::Faces);
    }
}

fn grab(ui: &mut UiState, out: &mut Update) {
    if !ui.manager.grabbed && !pinned(ui, ui.manager.selected) {
        ui.manager.grabbed = true;
        ui.manager.grab_from = ui.manager.selected;
        out.dirty = true;
    }
}

/// Carry the grabbed card toward `to`, stopping beside anything pinned.
fn carry(ui: &mut UiState, mut to: u8, out: &mut Update) {
    let from = ui.manager.selected;
    // A pinned card can only be "grabbed" by a direct poke of the pub fields.
    if !ui.manager.grabbed || pinned(ui, from) {
        return;
    }
    while to != from && !can_move(ui, from, to) {
        to = if to > from { to - 1 } else { to + 1 };
    }
    if to == from {
        return;
    }
    let current_id = ui.current_face().id;
    move_face(ui, from, to);
    ui.manager.selected = to;
    refit(ui, current_id, to);
    out.dirty = true;
}

fn drop(ui: &mut UiState, out: &mut Update) {
    if ui.manager.grabbed {
        ui.manager.grabbed = false;
        out.dirty = true;
        commit(ui, out);
    }
}

/// Put the grabbed card back: no change since the last `faces`.
fn revert(ui: &mut UiState) {
    if !ui.manager.grabbed {
        return;
    }
    let (from, to) = (ui.manager.selected, ui.manager.grab_from);
    if pinned(ui, from) || !can_move(ui, from, to) {
        ui.manager.grabbed = false;
        return;
    }
    let current_id = ui.current_face().id;
    move_face(ui, from, to);
    ui.manager.selected = to;
    ui.manager.grabbed = false;
    refit(ui, current_id, to);
    ui.manager.changed = false;
}

fn macropads(ui: &UiState) -> usize {
    (0..strip_len(ui))
        .filter_map(|index| ui.face_at(index))
        .filter(|face| face.template == Template::Macropad)
        .count()
}

/// Lowest id no face uses.
fn free_id(ui: &UiState) -> Option<u8> {
    (1..=MAX_FACES as u8).find(|&id| ui.index_of_face(id).is_none())
}

fn open_gallery(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if (strip_len(ui) as usize) < MAX_FACES {
        ui.manager.gallery_sel = 0;
        ui.manager.pressed = None;
        global::set_overlay(ui, Overlay::Gallery, at_ms, out);
    }
}

/// The strip already has a Timer face: the gallery offers no second one.
pub(crate) fn has_timer(ui: &UiState) -> bool {
    (0..ui.global.strip_len).any(|index| ui.face_at(index).is_some_and(|face| face.template == Template::Timer))
}

/// Gallery row chosen: on to naming, unless the template is used up.
fn pick(ui: &mut UiState, row: u8, at_ms: u32, out: &mut Update) {
    let Some(&template) = GALLERY.get(row as usize) else {
        return;
    };
    let full = (strip_len(ui) as usize) >= MAX_FACES
        || (template == Template::Macropad && macropads(ui) >= MAX_MACRO_SETS)
        || (template == Template::Timer && has_timer(ui));
    if full {
        return;
    }
    ui.manager.naming_template = template;
    ui.manager.naming_text.clear();
    ui.manager.pressed = None;
    global::set_overlay(ui, Overlay::Naming, at_ms, out);
}

/// Naming accepted: the new face goes in before Settings with the lowest
/// free id and becomes the selection.
fn add_face(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    let len = strip_len(ui) as usize;
    let Some(id) = free_id(ui).filter(|_| len < MAX_FACES) else {
        global::set_overlay(ui, Overlay::Manager, at_ms, out);
        return;
    };
    let template = ui.manager.naming_template;
    let name = if ui.manager.naming_text.is_empty() {
        Text::from_str(template.label())
    } else {
        ui.manager.naming_text
    };
    let at = match ui.global.strip[..len].last() {
        Some(last) if last.template == Template::Settings => len - 1,
        _ => len,
    };
    let current_id = ui.current_face().id;
    ui.global.strip[len] = FaceEntry { id, template, name };
    ui.global.strip[at..=len].rotate_right(1);
    ui.global.strip_len = len as u8 + 1;
    ui.manager.selected = at as u8;
    refit(ui, current_id, at as u8);
    commit(ui, out);
    global::set_overlay(ui, Overlay::Manager, at_ms, out);
    out.dirty = true;
}

/// Confirmed: take face `id` out of the strip and free its macro set.
fn remove_face(ui: &mut UiState, id: u8, at_ms: u32, out: &mut Update) {
    let len = strip_len(ui) as usize;
    if let Some(index) = ui.index_of_face(id).filter(|&index| !pinned(ui, index)) {
        let current_id = ui.current_face().id;
        ui.global.strip[index as usize..len].rotate_left(1);
        ui.global.strip[len - 1] = FaceEntry::EMPTY;
        ui.global.strip_len = len as u8 - 1;
        for set in &mut ui.deck.macros {
            if set.face == id {
                set.face = 0;
                set.source.clear();
            }
        }
        ui.manager.selected = index.saturating_sub(1);
        refit(ui, current_id, index.saturating_sub(1));
        commit(ui, out);
    }
    global::set_overlay(ui, Overlay::Manager, at_ms, out);
    out.dirty = true;
}

/// Every wheel detent and button press while Manager / Gallery / Naming /
/// ConfirmRemove is open. `true` = consumed (always: overlays capture).
pub(super) fn on_control(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) -> bool {
    if ui.global.shift {
        return true; // No chords here; never fall through.
    }
    // Sideways steps are the wheel's; anything else is a press or nothing.
    let step = match control {
        Control::Wheel(wheel) => super::selection_step(wheel),
        Control::Button(Button::Right, Edge::Press) => 1,
        Control::Button(Button::Left, Edge::Press) => -1,
        _ => 0,
    };
    let enter = control == Control::Button(Button::Enter, Edge::Press);
    let cancel = control == Control::Button(Button::Cancel, Edge::Press);
    match ui.global.overlay {
        Overlay::Manager => {
            out.dirty |= select(ui, ui.manager.selected);
            let selected = ui.manager.selected;
            if step != 0 {
                let next = selected as i32 + step;
                if ui.manager.grabbed {
                    if next >= 0 {
                        carry(ui, next as u8, out);
                    }
                } else if (0..card_count(ui) as i32).contains(&next) {
                    select(ui, next as u8);
                    out.dirty = true;
                }
            } else if enter {
                if is_plus(ui, selected) {
                    open_gallery(ui, at_ms, out);
                } else if ui.manager.grabbed {
                    drop(ui, out);
                } else if pinned(ui, selected) {
                    close(ui, at_ms, out);
                } else {
                    grab(ui, out);
                }
            } else if cancel {
                if ui.manager.grabbed {
                    revert(ui);
                    out.dirty = true;
                } else if !pinned(ui, selected) {
                    if let Some(face) = ui.face_at(selected) {
                        ui.manager.confirm_face = face.id;
                        ui.manager.pressed = None;
                        global::set_overlay(ui, Overlay::ConfirmRemove, at_ms, out);
                    }
                }
            }
        }
        Overlay::Gallery => {
            if step != 0 {
                let next = ui.manager.gallery_sel as i32 + step;
                if (0..GALLERY.len() as i32).contains(&next) {
                    ui.manager.gallery_sel = next as u8;
                    out.dirty = true;
                }
            } else if enter {
                pick(ui, ui.manager.gallery_sel, at_ms, out);
            } else if cancel {
                global::set_overlay(ui, Overlay::Manager, at_ms, out);
            }
        }
        Overlay::Naming => {
            if enter {
                add_face(ui, at_ms, out);
            } else if cancel {
                global::set_overlay(ui, Overlay::Gallery, at_ms, out);
            }
        }
        Overlay::ConfirmRemove => {
            if enter {
                remove_face(ui, ui.manager.confirm_face, at_ms, out);
            } else if cancel {
                global::set_overlay(ui, Overlay::Manager, at_ms, out);
            }
        }
        Overlay::None | Overlay::Drawer | Overlay::ConfirmReset | Overlay::Tutorial | Overlay::MacroEdit => {}
    }
    true
}

pub(super) fn hit(ui: &UiState, x: i32, y: i32) -> Option<Target> {
    match ui.global.overlay {
        Overlay::Manager => {
            let slot = layout::manager_card_at(x, y)?;
            let index = viewport(ui).index_at(slot)?;
            Some(Target::drag(layout::manager_slot(slot), index as u16))
        }
        Overlay::Gallery => {
            let row = layout::gallery_row_at(x, y)?;
            Some(Target::tap(layout::gallery_row(row), row as u16))
        }
        Overlay::ConfirmRemove => {
            let button = layout::confirm_button_at(x, y)?;
            Some(Target::tap(layout::confirm_button(button), CONFIRM_ID + button as u16))
        }
        Overlay::Naming | Overlay::None | Overlay::Drawer | Overlay::ConfirmReset | Overlay::Tutorial | Overlay::MacroEdit => None,
    }
}

/// The card index a dragged finger at `x` asks for: the slot under it, or
/// one beyond the viewport's edge when it is past the first / last slot.
fn slot_index_at(ui: &UiState, x: i32) -> u8 {
    let first = ui.manager.first_visible as i32;
    let last_slot = layout::MANAGER_VISIBLE as i32 - 1;
    let index = if x < layout::manager_card(0).left() {
        first - 1
    } else if x >= layout::manager_card(last_slot as u8).right() {
        first + last_slot + 1
    } else {
        let pitch = layout::manager_card(1).left() - layout::manager_card(0).left();
        first + ((x - layout::manager_card(0).left()) / pitch).min(last_slot)
    };
    index.clamp(0, strip_len(ui).saturating_sub(1) as i32) as u8
}

pub(super) fn on_touch(
    ui: &mut UiState,
    target: Target,
    act: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    if target.kind == TargetKind::Drag {
        card_touch(ui, target.id.min(255) as u8, act, at_ms, out);
        return;
    }
    let (confirm, index) = if target.id >= CONFIRM_ID {
        (true, (target.id - CONFIRM_ID).min(1) as u8)
    } else {
        (false, target.id.min(255) as u8)
    };
    match act {
        TouchAct::Down => ui.manager.pressed = Some(index),
        TouchAct::Activate => {
            ui.manager.pressed = None;
            match (confirm, ui.global.overlay) {
                (true, Overlay::ConfirmRemove) if index == 1 => {
                    remove_face(ui, ui.manager.confirm_face, at_ms, out)
                }
                (true, Overlay::ConfirmRemove) => {
                    global::set_overlay(ui, Overlay::Manager, at_ms, out)
                }
                (false, Overlay::Gallery) => {
                    ui.manager.gallery_sel = index.min(GALLERY.len() as u8 - 1);
                    pick(ui, ui.manager.gallery_sel, at_ms, out);
                }
                _ => {}
            }
        }
        TouchAct::Cancel | TouchAct::Swipe { .. } => ui.manager.pressed = None,
        _ => return,
    }
    out.dirty = true;
}

fn card_touch(ui: &mut UiState, index: u8, act: TouchAct, at_ms: u32, out: &mut Update) {
    match act {
        TouchAct::Down => {
            ui.manager.pressed = None;
            ui.manager.touch_scrolled = false;
            ui.manager.touch_first = ui.manager.first_visible;
            select(ui, index);
        }
        TouchAct::Move { x, dx, .. } => {
            if ui.manager.grabbed {
                carry(ui, slot_index_at(ui, x), out);
                return;
            }
            if dx.abs() < 10 && !ui.manager.touch_scrolled {
                return;
            }
            ui.manager.touch_scrolled = true;
            let pitch = layout::manager_card(1).left() - layout::manager_card(0).left();
            let mut view = viewport(ui);
            view.top = ui.manager.touch_first;
            let moved = view.scroll(-dx / pitch);
            ui.manager.first_visible = view.top;
            out.dirty |= moved;
            return;
        }
        TouchAct::LongPress => {
            if !ui.manager.touch_scrolled {
                grab(ui, out);
            }
            return;
        }
        TouchAct::Activate | TouchAct::Swipe { .. } => {
            if ui.manager.grabbed {
                drop(ui, out);
            } else if !ui.manager.touch_scrolled && act == TouchAct::Activate {
                if is_plus(ui, ui.manager.selected) {
                    open_gallery(ui, at_ms, out);
                } else {
                    close(ui, at_ms, out);
                }
            }
        }
        TouchAct::Cancel => revert(ui),
        TouchAct::Scroll(_) => return,
    }
    out.dirty = true;
}

/// A well-formed strip: Agents first, Settings last, unique ids, at most
/// `MAX_MACRO_SETS` Macropads.
fn valid_strip(list: &[FaceEntry]) -> bool {
    let Some((first, rest)) = list.split_first() else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return false;
    };
    let mut ids = 0u16;
    let unique = list.iter().all(|face| {
        let bit = 1u16 << (face.id & 15);
        let fresh = (1..=MAX_FACES as u8).contains(&face.id) && ids & bit == 0;
        ids |= bit;
        fresh
    });
    unique
        && first.template == Template::Agents
        && last.template == Template::Settings
        && middle
            .iter()
            .all(|face| !matches!(face.template, Template::Agents | Template::Settings))
        && list.iter().filter(|face| face.template == Template::Macropad).count() <= MAX_MACRO_SETS
}

/// `Faces` (strip restore; ignored while Manager/Gallery/Naming/ConfirmRemove
/// is open) and `Name` (only in Naming). This file, not `global.rs`, owns the
/// `Faces` restore: validate the list, rewrite `global.strip`/`strip_len`, keep
/// the current face by id when it survives, then call `global::jump_to` for
/// the clamp of `current`, the teardown and the `face` event (`jump_to` emits
/// it when the INDEX changes; call `global::emit_face` when the id under an
/// unchanged index changed). It never emits `faces` (no mirror of what it
/// applied). `global::on_host` handles `Focus`.
pub(super) fn on_host(
    ui: &mut UiState,
    msg: &HostMsg,
    _fx: &Effects,
    at_ms: u32,
    out: &mut Update,
) {
    match msg {
        HostMsg::Faces { list, count } => {
            if is_open(ui.global.overlay) {
                return;
            }
            let count = (*count as usize).min(MAX_FACES);
            if !valid_strip(&list[..count]) {
                global::on_reject(ui, ErrWhy::Range, at_ms, out);
                return;
            }
            let was = (ui.global.current, ui.current_face().id);
            ui.global.strip[..count].copy_from_slice(&list[..count]);
            for slot in &mut ui.global.strip[count..] {
                *slot = FaceEntry::EMPTY;
            }
            ui.global.strip_len = count as u8;
            // A macro set for an id that is no longer a Macropad is stale.
            for set in &mut ui.deck.macros {
                let keeps = list[..count]
                    .iter()
                    .any(|face| face.id == set.face && face.template == Template::Macropad);
                if set.face != 0 && !keeps {
                    set.face = 0;
                    set.source.clear();
                }
            }
            match ui.index_of_face(was.1) {
                Some(index) => global::jump_to(ui, index, at_ms, out),
                None => global::go_home(ui, at_ms, out),
            }
            if ui.global.current == was.0 && ui.current_face().id != was.1 {
                global::emit_face(ui, out);
            }
            select(ui, ui.manager.selected);
            out.dirty = true;
        }
        HostMsg::Name { text } => {
            if ui.global.overlay == Overlay::Naming {
                ui.manager.naming_text = *text;
                out.dirty = true;
            }
        }
        _ => {}
    }
}

pub(super) fn on_tick(_ui: &mut UiState, _now_ms: u32, _out: &mut Update) {}

pub(super) fn animating(_ui: &UiState) -> bool {
    false
}

// ---- helper API (double-SHIFT and idle-home live in global.rs) ---------------------

/// Double-SHIFT from a face: open with `selected` = the current face.
pub(super) fn open(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    ui.manager.origin = ui.global.current;
    ui.manager.origin_id = ui.current_face().id;
    ui.manager.grabbed = false;
    ui.manager.changed = false;
    ui.manager.pressed = None;
    select(ui, ui.global.current);
    global::set_overlay(ui, Overlay::Manager, at_ms, out);
    out.dirty = true;
}

/// Double-SHIFT inside: revert a grab, cancel an add, exit to the selected
/// face ("+" -> origin) and emit one `faces` if the strip changed.
pub(super) fn close(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if !is_open(ui.global.overlay) {
        return;
    }
    revert(ui);
    ui.manager.pressed = None;
    let target = if is_plus(ui, ui.manager.selected) {
        ui.index_of_face(ui.manager.origin_id).unwrap_or(ui.global.current)
    } else {
        ui.manager.selected
    };
    commit(ui, out);
    let was = (ui.global.current, ui.current_face().id);
    global::set_overlay(ui, Overlay::None, at_ms, out);
    global::jump_to(ui, target, at_ms, out);
    if ui.global.current == was.0 && ui.current_face().id != was.1 {
        global::emit_face(ui, out);
    }
}

/// Idle-home: revert a grab and close everything silently (no `faces`).
pub(super) fn cancel_all(ui: &mut UiState) {
    revert(ui);
    ui.manager.changed = false;
    ui.manager.pressed = None;
    if is_open(ui.global.overlay) {
        ui.global.overlay = Overlay::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Wheel;

    fn fresh() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui
    }

    fn ids(ui: &UiState) -> std::vec::Vec<u8> {
        (0..ui.global.strip_len).map(|i| ui.face_at(i).unwrap().id).collect()
    }

    fn faces(out: &Update) -> usize {
        out.deck_events().filter(|e| *e == Event::Faces).count()
    }

    fn card(ui: &UiState, index: u8) -> Target {
        let slot = viewport(ui).slot_of(index).unwrap();
        Target::drag(layout::manager_slot(slot), index as u16)
    }

    #[test]
    fn opens_on_the_current_face_and_exits_to_the_selected_one() {
        let mut ui = fresh();
        ui.on_face(Template::Usage);
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        assert_eq!(ui.global.overlay, Overlay::Manager);
        assert_eq!((ui.manager.selected, ui.manager.origin, ui.manager.first_visible), (3, 3, 0));
        on_control(&mut ui, Control::Wheel(Wheel::Ccw), 10, &mut out);
        assert_eq!((ui.manager.selected, ui.manager.first_visible), (4, 1));
        let mut out = Update::new();
        close(&mut ui, 30, &mut out);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.current_face().template, Template::System);
        assert_eq!(out.deck_events().collect::<std::vec::Vec<_>>(), [Event::Face { id: 7 }]);
        // "+" exits to the origin, even after the origin moved.
        let mut out = Update::new();
        ui.on_face(Template::Usage);
        open(&mut ui, 40, &mut out);
        select(&mut ui, 1);
        grab(&mut ui, &mut out);
        carry(&mut ui, 3, &mut out);
        drop(&mut ui, &mut out);
        assert_eq!(ids(&ui), [1, 3, 4, 2, 7, 6, 5]);
        assert_eq!((ui.manager.origin, ui.global.current), (2, 2));
        assert_eq!(faces(&out), 1);
        select(&mut ui, 7);
        let mut out = Update::new();
        close(&mut ui, 50, &mut out);
        assert_eq!(ui.current_face().template, Template::Usage);
        assert_eq!(faces(&out), 0);
        assert_eq!(out.deck_events().count(), 0);
    }

    #[test]
    fn long_press_via_tick_grabs_and_cancel_reverts() {
        let mut ui = fresh();
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        let target = card(&ui, 2);
        on_touch(&mut ui, target, TouchAct::Down, 0, &mut out);
        assert_eq!((ui.manager.selected, ui.manager.grabbed), (2, false));
        on_touch(&mut ui, target, TouchAct::LongPress, 350, &mut out);
        assert!(ui.manager.grabbed);
        // Slot 0 is Agents: the card stops beside it.
        let (x, _) = layout::manager_card(0).center();
        on_touch(&mut ui, target, TouchAct::Move { x, y: 240, dx: -222, dy: 0 }, 400, &mut out);
        assert_eq!((ui.manager.selected, ids(&ui)[1]), (1, 3));
        let (x, _) = layout::manager_card(3).center();
        on_touch(&mut ui, target, TouchAct::Move { x, y: 240, dx: 111, dy: 0 }, 450, &mut out);
        assert_eq!(ids(&ui), [1, 2, 4, 3, 7, 6, 5]);
        let mut out = Update::new();
        on_touch(&mut ui, target, TouchAct::Cancel, 500, &mut out);
        assert_eq!(ids(&ui), [1, 2, 3, 4, 7, 6, 5]);
        assert!(!ui.manager.grabbed && faces(&out) == 0);
        assert_eq!(ui.manager.selected, 2);
        // Dropping where it was picked up is no change either.
        on_touch(&mut ui, target, TouchAct::Down, 600, &mut out);
        on_touch(&mut ui, target, TouchAct::LongPress, 950, &mut out);
        on_touch(&mut ui, target, TouchAct::Activate, 1000, &mut out);
        assert!(!ui.manager.grabbed && faces(&out) == 0);
        assert_eq!(ui.global.overlay, Overlay::Manager);
        // A drop somewhere else is one `faces`.
        on_touch(&mut ui, target, TouchAct::Down, 1100, &mut out);
        on_touch(&mut ui, target, TouchAct::LongPress, 1450, &mut out);
        let (x, _) = layout::manager_card(3).center();
        on_touch(&mut ui, target, TouchAct::Move { x, y: 240, dx: 111, dy: 0 }, 1460, &mut out);
        on_touch(&mut ui, target, TouchAct::Activate, 1500, &mut out);
        assert_eq!(ids(&ui), [1, 2, 4, 3, 7, 6, 5]);
        assert_eq!(faces(&out), 1);
    }

    #[test]
    fn pinned_cards_cannot_be_grabbed_and_a_tap_goes_there() {
        let mut ui = fresh();
        ui.on_face(Template::Macropad);
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        let agents = card(&ui, 0);
        on_touch(&mut ui, agents, TouchAct::Down, 0, &mut out);
        on_touch(&mut ui, agents, TouchAct::LongPress, 350, &mut out);
        assert!(!ui.manager.grabbed);
        let (x, _) = layout::manager_card(3).center();
        on_touch(&mut ui, agents, TouchAct::Move { x, y: 240, dx: 333, dy: 0 }, 400, &mut out);
        assert_eq!(ids(&ui)[0], 1);
        let mut out = Update::new();
        on_touch(&mut ui, agents, TouchAct::Activate, 500, &mut out);
        // The finger scrolled the row, so its Up was not a tap.
        assert_eq!(ui.global.overlay, Overlay::Manager);
        assert_eq!(ui.manager.first_visible, 0);
        let agents = card(&ui, 0);
        on_touch(&mut ui, agents, TouchAct::Down, 600, &mut out);
        on_touch(&mut ui, agents, TouchAct::Activate, 700, &mut out);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!(out.deck_events().collect::<std::vec::Vec<_>>(), [Event::Face { id: 1 }]);
    }

    #[test]
    fn a_quick_horizontal_drag_scrolls_the_row() {
        let mut ui = fresh();
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        let target = card(&ui, 1);
        on_touch(&mut ui, target, TouchAct::Down, 0, &mut out);
        on_touch(&mut ui, target, TouchAct::Move { x: 20, y: 240, dx: -230, dy: 3 }, 50, &mut out);
        assert_eq!(ui.manager.first_visible, 2);
        on_touch(&mut ui, target, TouchAct::Move { x: 130, y: 240, dx: -120, dy: 3 }, 60, &mut out);
        assert_eq!(ui.manager.first_visible, 1);
        // It never scrolls past the last card, and its Up is not a tap.
        on_touch(&mut ui, target, TouchAct::Move { x: 0, y: 240, dx: -900, dy: 3 }, 70, &mut out);
        assert_eq!(ui.manager.first_visible, 4);
        on_touch(&mut ui, target, TouchAct::Activate, 80, &mut out);
        assert_eq!(ui.global.overlay, Overlay::Manager);
        assert!(!ui.manager.grabbed);
    }

    #[test]
    fn a_poked_grab_of_a_pinned_card_moves_nothing() {
        let mut ui = fresh();
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        ui.manager.grabbed = true;
        for (selected, step) in [(0, Wheel::Ccw), (6, Wheel::Cw), (7, Wheel::Cw)] {
            ui.manager.selected = selected;
            ui.manager.grabbed = true;
            on_control(&mut ui, Control::Wheel(step), 10, &mut out);
            assert_eq!(ids(&ui), [1, 2, 3, 4, 7, 6, 5], "selected {selected}");
            revert(&mut ui);
            assert!(!ui.manager.grabbed && ids(&ui) == [1, 2, 3, 4, 7, 6, 5]);
        }
        assert_eq!(faces(&out), 0);
    }

    #[test]
    fn idle_home_cancel_all_reverts_and_closes_without_events() {
        let mut ui = fresh();
        let mut out = Update::new();
        open(&mut ui, 0, &mut out);
        select(&mut ui, 3);
        grab(&mut ui, &mut out);
        carry(&mut ui, 1, &mut out);
        assert_eq!(ids(&ui), [1, 4, 2, 3, 7, 6, 5]);
        cancel_all(&mut ui);
        assert_eq!(ids(&ui), [1, 2, 3, 4, 7, 6, 5]);
        assert!(!ui.manager.grabbed);
        assert_eq!(ui.global.overlay, Overlay::None);
        for overlay in [Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove] {
            ui.global.overlay = overlay;
            cancel_all(&mut ui);
            assert_eq!(ui.global.overlay, Overlay::None);
        }
        ui.global.overlay = Overlay::Drawer;
        cancel_all(&mut ui);
        assert_eq!(ui.global.overlay, Overlay::Drawer);
    }
}
