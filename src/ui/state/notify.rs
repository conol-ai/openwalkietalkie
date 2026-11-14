//! Notices: the toast, the drawer and the top-edge indicator.
//!
//! Lifecycle of a notice (`deck.notices`, a ring of 8 keyed by (agent, card)):
//! created by a NEW card (`raise`) and toasted for 4 s from the frame that
//! shows it; it enters the drawer (`in_drawer`) when the toast times out or is
//! displaced, when it is raised under an opaque overlay, during a resync, for
//! a snoozed card, and when the user snoozes (`file`); a tapped toast is
//! dismissed, not filed, and so is one opened by ENTER (`claims_enter`: a
//! card toast raised with nothing to interrupt takes the next ENTER if
//! nothing else is pressed first). It is removed on answer / acknowledge
//! (`remove`), on a host clear, replace or gone (`prune`) and on a drawer
//! dismiss (the card stays). A full ring drops the oldest Ready first. The
//! indicator shows iff the drawer is non-empty (`Deck::notice_count`), red
//! for a Question.
//!
//! Drawer rows are the filed notices in RING order (what `overlay::notify`
//! draws), `drawer_top` first; the selection is an identity, so a notice
//! arriving meanwhile never moves it. SCROLL selects, ENTER / tap jumps to
//! the card, LEFT / RIGHT / a horizontal swipe dismisses, CANCEL closes.
//!
//! Everything outside this file builds a `Toast` through `Toast::new` /
//! `Toast::card` / `Toast::message`, never with a struct literal.
//!
//! The toast and the indicator are drawn UNDER every opaque overlay (drawer,
//! manager, gallery, naming, LISTENING; `overlay::opaque`), as in the mockups.
//! So while one is up, a new notice goes straight to the drawer without a
//! toast, a message toast is dropped, and a toast that was already showing
//! keeps its timer but is not tappable (`toast_tappable`).
//!
//! `Update.wake`: `raise` sets it for every new notice outside a resync. The
//! "demo notices never wake" rule is NOT implemented here: `UiState` clears
//! `wake` while `deck.demo_active()` at the end of every entry point.
use super::{global, Target, TouchAct, UiState, Update};
use crate::control::{Button, Control, Edge, TouchPhase};
use crate::deck::model::{Effects, Notice, MAX_NOTICES};
use crate::deck::msg::{CardKind, HostMsg, Template};
use crate::ui::kit::list::Viewport;
use crate::ui::layout;

pub const TOAST_MS: u32 = 4000;
/// The "brief" message toasts (no channel, not delivered).
pub const MESSAGE_MS: u32 = 2000;
/// Touches inside the toast rect are swallowed this long after it expired.
pub const TOAST_SWALLOW_MS: u32 = 300;
/// Auto-jump waits for this much quiet after the last input.
pub const AUTO_JUMP_QUIET_MS: u32 = 2000;

/// What the toast says. A `Card` toast shows the agent and its card text and
/// jumps there when tapped; the message toasts show a fixed string
/// (`ToastKind::message`), belong to no notice and never enter the drawer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Card(CardKind),
    /// PTT pressed with no target agent; `agent` and `card` are 0.
    NoChannel,
    /// An answer went unacknowledged for 5 s; `agent`/`card` name the card.
    NotDelivered,
    TranscriptionFailed,
    TranscriptionUnavailable,
    /// The countdown ran out; `agent` is the Timer face's id, `card` 0.
    /// Lasts as long as a card toast, wakes, and jumps to that face on a tap.
    TimerDone,
    /// A Pomodoro focus / break phase finished; `agent` is its Timer face.
    PomodoroFocusDone,
    PomodoroBreakDone,
}

impl ToastKind {
    /// The fixed text of a message toast; `None` for a card toast.
    pub const fn message(self) -> Option<&'static str> {
        match self {
            ToastKind::Card(_) => None,
            ToastKind::NoChannel => Some("No channel"),
            ToastKind::NotDelivered => Some("Not delivered"),
            ToastKind::TranscriptionFailed => Some("Transcription failed. Nothing sent."),
            ToastKind::TranscriptionUnavailable => Some("Transcription unavailable"),
            ToastKind::TimerDone => Some("Timer done"),
            ToastKind::PomodoroFocusDone => Some("Focus complete"),
            ToastKind::PomodoroBreakDone => Some("Break complete"),
        }
    }

    const fn timer_mode(self) -> Option<super::timer::Mode> {
        match self {
            Self::TimerDone => Some(super::timer::Mode::Countdown),
            Self::PomodoroFocusDone | Self::PomodoroBreakDone => Some(super::timer::Mode::Pomodoro),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Toast {
    pub agent: u8,
    pub card: u16,
    pub kind: ToastKind,
    /// Slides down from here...
    pub shown_ms: u32,
    /// ...and is gone here.
    pub until_ms: u32,
    /// The toast may take the next ENTER (`claims_enter`): it was raised
    /// with nothing to interrupt (`undisturbed`) and nothing has been
    /// pressed, turned or touched since. Only `raise` sets it.
    pub takes_enter: bool,
}

impl Toast {
    pub const fn new(agent: u8, card: u16, kind: ToastKind, shown_ms: u32, until_ms: u32) -> Self {
        Self {
            agent,
            card,
            kind,
            shown_ms,
            until_ms,
            takes_enter: false,
        }
    }

    /// A card toast with the standard 4 s.
    pub const fn card(agent: u8, card: u16, kind: CardKind, shown_ms: u32) -> Self {
        Self::new(agent, card, ToastKind::Card(kind), shown_ms, shown_ms.wrapping_add(TOAST_MS))
    }

    /// A message toast with the standard 2 s.
    pub const fn message(kind: ToastKind, agent: u8, card: u16, shown_ms: u32) -> Self {
        Self::new(agent, card, kind, shown_ms, shown_ms.wrapping_add(MESSAGE_MS))
    }

    /// The notice this toast stands for; `None` for a message toast.
    pub const fn notice(&self) -> Option<(u8, u16)> {
        match self.kind {
            ToastKind::Card(_) => Some((self.agent, self.card)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Notify {
    pub toast: Option<Toast>,
    /// Identity (agent, card) of the selected drawer row; (0, 0) = none.
    pub drawer_sel: (u8, u16),
    pub drawer_top: u8,
    /// A finger is down on the toast (its pressed look).
    pub toast_pressed: bool,
    /// When the last toast left the glass, for the post-expiry swallow.
    expired_at_ms: Option<u32>,
    /// `deck.syncing` at the last tick: a sync that ends in `tick` purged.
    was_syncing: bool,
}

impl Notify {
    pub const fn new() -> Self {
        Self {
            toast: None,
            drawer_sel: (0, 0),
            drawer_top: 0,
            toast_pressed: false,
            expired_at_ms: None,
            was_syncing: false,
        }
    }
}

// ---- the drawer's rows ------------------------------------------------------------------

/// Ring slots of the filed notices, in display order.
fn filed(ui: &UiState) -> impl Iterator<Item = usize> + '_ {
    ui.deck
        .notices
        .iter()
        .enumerate()
        .filter(|(_, n)| n.used && n.in_drawer)
        .map(|(slot, _)| slot)
}

fn identity(notice: &Notice) -> (u8, u16) {
    (notice.agent, notice.card)
}

fn filed_count(ui: &UiState) -> u8 {
    filed(ui).count() as u8
}

/// Display index of the selected notice.
fn selected_index(ui: &UiState) -> Option<u8> {
    filed(ui)
        .position(|slot| identity(&ui.deck.notices[slot]) == ui.notify.drawer_sel)
        .map(|index| index as u8)
}

fn viewport(ui: &UiState) -> Viewport {
    Viewport::new(ui.notify.drawer_top, layout::DRAWER_VISIBLE, filed_count(ui))
}

/// Make the selection a filed notice (the nearest to `index` when the old
/// one is gone) and keep it in view.
fn fix_selection(ui: &mut UiState, index: Option<u8>, out: &mut Update) {
    let count = filed_count(ui);
    let selected = match selected_index(ui) {
        Some(index) => Some(index),
        None if count == 0 => None,
        None => Some(index.unwrap_or(0).min(count - 1)),
    };
    let identity = selected
        .and_then(|index| filed(ui).nth(index as usize))
        .map(|slot| identity(&ui.deck.notices[slot]))
        .unwrap_or((0, 0));
    let mut viewport = viewport(ui);
    if let Some(index) = selected {
        viewport.follow(index);
    }
    if identity != ui.notify.drawer_sel || viewport.top != ui.notify.drawer_top {
        ui.notify.drawer_sel = identity;
        ui.notify.drawer_top = viewport.top;
        out.dirty = true;
    }
}

/// `Target::id` of a drawer row: ring slot, agent and the card's low byte, so
/// an Up on a slot that changed hands meanwhile activates nothing.
fn row_id(slot: usize, notice: &Notice) -> u16 {
    slot as u16 | (notice.agent as u16 & 0xf) << 3 | (notice.card & 0xff) << 7
}

fn row_notice(ui: &UiState, id: u16) -> Option<(u8, u16)> {
    let notice = ui.deck.notices.get((id & 7) as usize)?;
    (notice.used && notice.in_drawer && row_id((id & 7) as usize, notice) == id)
        .then(|| identity(notice))
}

/// Drawer open: it captures every face-layer control.
pub(super) fn on_control(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) -> bool {
    if ui.global.shift {
        return true;
    }
    match control {
        Control::Wheel(wheel) => {
            let step = super::selection_step(wheel);
            let index = selected_index(ui).map_or(0, |index| index as i32 + step);
            let count = filed_count(ui) as i32;
            if (0..count).contains(&index) {
                ui.notify.drawer_sel = (0, 0);
                fix_selection(ui, Some(index as u8), out);
            }
        }
        Control::Button(Button::Enter, Edge::Press) => {
            let (agent, card) = ui.notify.drawer_sel;
            if agent != 0 {
                jump(ui, agent, card, at_ms, out);
            }
        }
        Control::Button(Button::Left | Button::Right, Edge::Press) => {
            let (agent, card) = ui.notify.drawer_sel;
            if agent != 0 {
                dismiss(ui, agent, card, out);
            }
        }
        Control::Button(Button::Cancel, Edge::Press) => close_drawer(ui, at_ms, out),
        _ => {}
    }
    true
}

/// Drawer rows (and, for the recogniser's edge path, the toast).
pub(super) fn hit(ui: &UiState, x: i32, y: i32) -> Option<Target> {
    let row = layout::drawer_row_at(x, y)?;
    let index = viewport(ui).index_at(row)?;
    let slot = filed(ui).nth(index as usize)?;
    let notice = &ui.deck.notices[slot];
    Some(Target::list_row(
        layout::drawer_row(row),
        row_id(slot, notice),
        layout::DRAWER_PITCH,
    ))
}

pub(super) fn on_touch(
    ui: &mut UiState,
    target: Target,
    act: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    match act {
        TouchAct::Scroll(rows) => {
            let mut viewport = viewport(ui);
            if viewport.scroll(rows as i32) {
                ui.notify.drawer_top = viewport.top;
                out.dirty = true;
            }
            return;
        }
        TouchAct::Cancel | TouchAct::Move { .. } | TouchAct::LongPress => return,
        TouchAct::Down | TouchAct::Activate | TouchAct::Swipe { .. } => {}
    }
    let Some((agent, card)) = row_notice(ui, target.id) else {
        return;
    };
    match act {
        TouchAct::Down => {
            if ui.notify.drawer_sel != (agent, card) {
                ui.notify.drawer_sel = (agent, card);
                out.dirty = true;
            }
        }
        TouchAct::Activate => jump(ui, agent, card, at_ms, out),
        TouchAct::Swipe { .. } => dismiss(ui, agent, card, out),
        _ => {}
    }
}

pub(super) fn on_host(
    ui: &mut UiState,
    _msg: &HostMsg,
    fx: &Effects,
    at_ms: u32,
    out: &mut Update,
) {
    if let Some(notice) = fx.notice {
        raise(ui, notice, at_ms, out);
    }
    prune(ui, out);
    ui.notify.was_syncing = ui.deck.syncing;
}

pub(super) fn on_tick(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    // Only the model changes what a notice refers to: after a message
    // (`on_host`), or when a sync times out in `Deck::tick` and purges.
    if ui.notify.was_syncing && !ui.deck.syncing {
        prune(ui, out);
    }
    ui.notify.was_syncing = ui.deck.syncing;
    let Some(toast) = ui.notify.toast else {
        return;
    };
    // Sliding in: every frame of the 220 ms moves the lane.
    let shown_for = now_ms.wrapping_sub(toast.shown_ms);
    if shown_for < crate::ui::overlay::notify::SLIDE_MS && global::lit(ui) {
        out.dirty |= !crate::ui::overlay::opaque(ui);
    }
    if now_ms.wrapping_sub(toast.until_ms) < u32::MAX / 2 {
        ui.notify.toast = None;
        ui.notify.toast_pressed = false;
        ui.notify.expired_at_ms = Some(now_ms);
        if let Some((agent, card)) = toast.notice() {
            if let Some(slot) = ensure_notice(ui, agent, card, toast.shown_ms) {
                ui.deck.notices[slot].in_drawer = true;
            }
        }
        out.dirty = true;
    }
}

pub(super) fn animating(ui: &UiState) -> bool {
    ui.notify.toast.is_some()
}

/// The first frame after a wake is visible: the toast slides in from it and
/// its 4 s start there.
pub(super) fn on_frame_shown(ui: &mut UiState, now_ms: u32) {
    if let Some(toast) = &mut ui.notify.toast {
        let life = if toast.notice().is_some() || toast.kind.timer_mode().is_some() { TOAST_MS } else { MESSAGE_MS };
        toast.shown_ms = now_ms;
        toast.until_ms = now_ms.wrapping_add(life);
    }
}

// ---- helper API ----------------------------------------------------------------------

/// A toast is on the glass and may be tapped: not hidden under an opaque
/// overlay.
pub(super) fn toast_tappable(ui: &UiState) -> bool {
    ui.notify.toast.is_some() && !crate::ui::overlay::opaque(ui)
}

/// A Down here would have hit the toast that just expired: swallow it.
pub(super) fn swallows_touch(ui: &UiState, x: i32, y: i32, at_ms: u32) -> bool {
    ui.notify
        .expired_at_ms
        .is_some_and(|expired| at_ms.wrapping_sub(expired) < TOAST_SWALLOW_MS)
        && layout::toast_at(x, y)
}

/// The ring slot holding (agent, card), or a slot for it: a free one, else
/// the oldest Ready, else the oldest. `None` only for a card that no longer
/// exists (nothing to notify about).
fn ensure_notice(ui: &mut UiState, agent: u8, card: u16, raised_ms: u32) -> Option<usize> {
    let notices = &ui.deck.notices;
    if let Some(slot) = notices.iter().position(|n| n.used && identity(n) == (agent, card)) {
        return Some(slot);
    }
    let live = ui.deck.card(agent)?;
    let kind = live.kind;
    let oldest = |kind: Option<CardKind>| {
        notices
            .iter()
            .enumerate()
            .filter(|(_, n)| n.used && kind.is_none_or(|kind| n.kind == kind))
            .max_by_key(|(_, n)| raised_ms.wrapping_sub(n.raised_ms))
            .map(|(slot, _)| slot)
    };
    let slot = notices
        .iter()
        .position(|n| !n.used)
        .or_else(|| oldest(Some(CardKind::Ready)))
        .or_else(|| oldest(None))?;
    ui.deck.notices[slot] = Notice::new(agent, card, kind, raised_ms);
    Some(slot)
}

/// A new notice: remember it and toast it; ask for a lit screen. Under an
/// opaque overlay nobody could see the toast, during a resync and for a
/// snoozed card nobody should: the notice is filed at once, without a wake
/// for the resync.
pub(super) fn raise(ui: &mut UiState, notice: Notice, at_ms: u32, out: &mut Update) {
    let slot = ui
        .deck
        .notices
        .iter()
        .position(|n| n.used && identity(n) == identity(&notice))
        .or_else(|| ui.deck.notices.iter().position(|n| !n.used))
        .or_else(|| {
            let oldest = |kind: Option<CardKind>| {
                ui.deck
                    .notices
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| kind.is_none_or(|kind| n.kind == kind))
                    .max_by_key(|(_, n)| at_ms.wrapping_sub(n.raised_ms))
                    .map(|(slot, _)| slot)
            };
            oldest(Some(CardKind::Ready)).or_else(|| oldest(None))
        })
        .unwrap_or(0);
    ui.deck.notices[slot] = Notice::new(notice.agent, notice.card, notice.kind, notice.raised_ms);
    let snoozed = ui
        .deck
        .card(notice.agent)
        .is_some_and(|card| card.id == notice.card && card.snoozed);
    let quiet = crate::ui::overlay::opaque(ui) || ui.deck.syncing || snoozed;
    out.dirty = true;
    out.wake = !ui.deck.syncing;
    if quiet {
        ui.deck.notices[slot].in_drawer = true;
        return;
    }
    let undisturbed = undisturbed(ui, at_ms);
    if notice.kind == CardKind::Question && undisturbed && auto_jump_on(ui) {
        jump(ui, notice.agent, notice.card, at_ms, out);
        return;
    }
    displace(ui);
    let mut toast = Toast::card(notice.agent, notice.card, notice.kind, at_ms);
    // With nothing it could interrupt, the next ENTER may open the card, as
    // a tap does.
    toast.takes_enter = undisturbed;
    ui.notify.toast = Some(toast);
    ui.notify.toast_pressed = false;
}

/// The auto-jump setting takes the user to a new Question only when nothing
/// they are doing could be interrupted by it (`undisturbed`); otherwise it is
/// a toast.
fn auto_jump_on(ui: &UiState) -> bool {
    ui.deck.settings.auto_jump && (ui.deck.host_present || ui.deck.demo_active())
}

/// Nothing the user is doing could be interrupted by a new notice: nothing
/// held or touched, no PTT session, no overlay, no card on screen, the strip
/// at rest and 2 s without input. Only then does auto-jump take a Question,
/// and only then does a card's toast take the next ENTER.
fn undisturbed(ui: &UiState, at_ms: u32) -> bool {
    !global::anything_held(ui)
        && !ui.touch.active()
        && ui.global.ptt.is_none() && ui.global.ptt_pending.is_none()
        && ui.global.overlay == global::Overlay::None
        && !card_on_screen(ui)
        && ui.strip_at_rest()
        && global::quiet_for(ui, at_ms, AUTO_JUMP_QUIET_MS)
}

/// A card is on screen on the Agents face, where ENTER answers it.
fn card_on_screen(ui: &UiState) -> bool {
    ui.agents.view == super::agents::AgentsView::Card
        && ui.current_face().template == Template::Agents
}

/// The face's keys go to the computer (Omarchy, Macropad), often pressed
/// without a look at the deck: a notice never eats one of them.
fn keys_go_to_the_computer(ui: &UiState) -> bool {
    matches!(ui.current_face().template, Template::Omarchy | Template::Macropad)
}

/// Every control, before global sees it. A press, a detent or a new touch
/// ends a card toast's claim on ENTER (`Toast::takes_enter`): whatever the
/// user turned to first, ENTER keeps its own meaning there. True when the
/// control is the plain ENTER the claim was waiting for and ENTER is the
/// deck's own where it lands (no SHIFT chord, no overlay up, no card on
/// screen, not a face whose keys go to the computer); the caller then opens
/// the toast (`toast_tapped`) unless global took the press.
pub(super) fn claims_enter(ui: &mut UiState, control: Control) -> bool {
    let fresh = match control {
        Control::Button(_, edge) => edge == Edge::Press,
        Control::Wheel(_) => true,
        Control::Touch(touch) => touch.phase == TouchPhase::Down,
    };
    let claimed = match ui.notify.toast.as_mut() {
        Some(toast) if fresh => core::mem::take(&mut toast.takes_enter) && toast.notice().is_some(),
        _ => false,
    };
    claimed
        && control == Control::Button(Button::Enter, Edge::Press)
        && !ui.global.shift
        && ui.global.overlay == global::Overlay::None
        && !card_on_screen(ui)
        && !keys_go_to_the_computer(ui)
}

/// The card toast on the glass, if any, goes to the drawer.
fn displace(ui: &mut UiState) {
    ui.notify.toast_pressed = false;
    if let Some(toast) = ui.notify.toast.take() {
        if let Some((agent, card)) = toast.notice() {
            if let Some(slot) = ensure_notice(ui, agent, card, toast.shown_ms) {
                ui.deck.notices[slot].in_drawer = true;
            }
        }
    }
}

/// A "no channel" / "not delivered" toast: no notice, no drawer entry, no
/// wake (the user's own input already lit the screen). It displaces a card
/// toast, whose notice goes to the drawer as on a timeout.
pub(super) fn raise_message(
    ui: &mut UiState,
    kind: ToastKind,
    agent: u8,
    card: u16,
    at_ms: u32,
    out: &mut Update,
) {
    if kind.message().is_none() || crate::ui::overlay::opaque(ui) {
        return;
    }
    displace(ui);
    ui.notify.toast = Some(Toast::message(kind, agent, card, at_ms));
    out.dirty = true;
}

/// The timer on face `face` ran out: a toast that lights the screen like a
/// new notice, on every face but under an opaque overlay. No drawer entry.
pub(super) fn raise_timer(ui: &mut UiState, face: u8, at_ms: u32, out: &mut Update) {
    raise_clock(ui, face, ToastKind::TimerDone, at_ms, out);
}

pub(super) fn raise_pomodoro(ui: &mut UiState, face: u8, focus: bool, at_ms: u32, out: &mut Update) {
    let kind = if focus { ToastKind::PomodoroFocusDone } else { ToastKind::PomodoroBreakDone };
    raise_clock(ui, face, kind, at_ms, out);
}

fn raise_clock(ui: &mut UiState, face: u8, kind: ToastKind, at_ms: u32, out: &mut Update) {
    out.wake = true;
    if crate::ui::overlay::opaque(ui) {
        return;
    }
    displace(ui);
    ui.notify.toast = Some(Toast::new(face, 0, kind, at_ms, at_ms.wrapping_add(TOAST_MS)));
    out.dirty = true;
}

/// The card was answered, acknowledged, cleared, replaced or its agent left.
pub(super) fn remove(ui: &mut UiState, agent: u8, card: u16, out: &mut Update) {
    for notice in &mut ui.deck.notices {
        if notice.used && identity(notice) == (agent, card) {
            notice.used = false;
            out.dirty = true;
        }
    }
    if ui.notify.toast.is_some_and(|t| t.notice() == Some((agent, card))) {
        ui.notify.toast = None;
        ui.notify.toast_pressed = false;
        out.dirty = true;
    }
    fix_selection(ui, None, out);
}

/// The user snoozed the card (or opened it: its toast is done): the notice
/// waits in the drawer.
pub(super) fn file(ui: &mut UiState, agent: u8, card: u16, at_ms: u32, out: &mut Update) {
    if let Some(slot) = ensure_notice(ui, agent, card, at_ms) {
        ui.deck.notices[slot].in_drawer = true;
        out.dirty = true;
    }
    if ui.notify.toast.is_some_and(|t| t.notice() == Some((agent, card))) {
        ui.notify.toast = None;
        ui.notify.toast_pressed = false;
        out.dirty = true;
    }
    fix_selection(ui, None, out);
}

/// The card is on screen: its toast has done its job and is dismissed, not
/// filed. The notice itself stays until the card is resolved.
pub(super) fn dismiss_toast_for(ui: &mut UiState, agent: u8, card: u16, out: &mut Update) {
    if ui.notify.toast.is_some_and(|t| t.notice() == Some((agent, card))) {
        ui.notify.toast = None;
        ui.notify.toast_pressed = false;
        out.dirty = true;
    }
}

/// Notices whose card is gone (cleared, replaced, agent gone or purged) go
/// with it; so does their toast. The drawer selection moves on.
pub(super) fn prune(ui: &mut UiState, out: &mut Update) {
    for slot in 0..MAX_NOTICES {
        let notice = ui.deck.notices[slot];
        if notice.used
            && !ui
                .deck
                .card(notice.agent)
                .is_some_and(|card| card.id == notice.card)
        {
            ui.deck.notices[slot].used = false;
            out.dirty = true;
        }
    }
    if let Some((agent, card)) = ui.notify.toast.and_then(|t| t.notice()) {
        if !ui.deck.card(agent).is_some_and(|c| c.id == card) {
            ui.notify.toast = None;
            ui.notify.toast_pressed = false;
            out.dirty = true;
        }
    }
    fix_selection(ui, None, out);
}

/// The toast was tapped, or a card toast took ENTER (`claims_enter`): to its
/// card (or channel), the toast dismissed.
pub(super) fn toast_tapped(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    let Some(toast) = ui.notify.toast.take() else {
        return;
    };
    ui.notify.toast_pressed = false;
    out.dirty = true;
    if let Some((agent, card)) = toast.notice() {
        jump(ui, agent, card, at_ms, out);
    } else if let Some(mode) = toast.kind.timer_mode() {
        if ui.global.overlay == global::Overlay::Drawer {
            global::set_overlay(ui, global::Overlay::None, at_ms, out);
        }
        if let Some(index) = ui.index_of_face(toast.agent)
            .filter(|&index| ui.face_at(index).is_some_and(|face| face.template == Template::Timer))
        {
            ui.timer.mode = mode;
            global::jump_to(ui, index, at_ms, out);
        }
    }
}

/// To the Agents face showing this card (its channel if the card is gone,
/// nothing if the agent is): a programmatic jump, so no `channel`.
fn jump(ui: &mut UiState, agent: u8, card: u16, at_ms: u32, out: &mut Update) {
    if ui.deck.agent(agent).is_none() {
        return;
    }
    if ui.global.overlay == global::Overlay::Drawer {
        global::set_overlay(ui, global::Overlay::None, at_ms, out);
    }
    let home = (0..ui.global.strip_len)
        .find(|&index| ui.face_at(index).is_some_and(|face| face.template == Template::Agents));
    if let Some(index) = home {
        global::jump_to(ui, index, at_ms, out);
    }
    if ui.deck.card(agent).is_some_and(|c| c.id == card) {
        super::agents::show_card(ui, agent, at_ms);
    } else {
        super::agents::show_channel(ui, agent, at_ms);
    }
    dismiss_toast_for(ui, agent, card, out);
    out.dirty = true;
}

/// Drawer dismiss: the notice goes, the card stays.
fn dismiss(ui: &mut UiState, agent: u8, card: u16, out: &mut Update) {
    let index = selected_index(ui);
    for notice in &mut ui.deck.notices {
        if notice.used && identity(notice) == (agent, card) {
            notice.used = false;
            out.dirty = true;
        }
    }
    fix_selection(ui, index, out);
}

pub(super) fn open_drawer(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if ui.global.overlay == global::Overlay::None {
        global::set_overlay(ui, global::Overlay::Drawer, at_ms, out);
        fix_selection(ui, Some(0), out);
    }
}

pub(super) fn close_drawer(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if ui.global.overlay == global::Overlay::Drawer {
        global::set_overlay(ui, global::Overlay::None, at_ms, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Wheel;
    use crate::control::{Touch, TouchPhase};
    use crate::deck::text::Text;
    use crate::ui::state::{agents::AgentsView, Overlay, PttSession};

    fn hosted() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.clear_host_data();
        ui.deck.host_present = true;
        ui
    }

    /// A real host (hello at 0) with the demo roster (agents 1..5, cards on
    /// 1 and 2) and an empty drawer. Tests stay under the 15 s heartbeat.
    fn roster() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.host(&HostMsg::Hello { v: 1, sid: 1, minute_of_day: None }, 0, &mut Update::new());
        crate::deck::demo::load(&mut ui.deck);
        ui.deck.settings.demo = false;
        for notice in &mut ui.deck.notices {
            notice.used = false;
        }
        super::super::agents::on_reset(&mut ui);
        ui.invalidate();
        ui
    }

    fn agent(id: u8) -> HostMsg {
        HostMsg::Agent {
            id,
            gone: false,
            kind: None,
            state: Some(crate::deck::msg::AgentState::Working),
            name: None,
            project: None,
            title: None,
            since_s: None, task: None, subs: None,
        }
    }

    fn card(agent: u8, id: u16, kind: CardKind) -> HostMsg {
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind,
            text: Text::from_str("Ship it?"),
            choices: [Text::new(); 4],
            choice_count: 0,
            recommended: None,
        }
    }

    /// A question with one choice, recommended: the shape of the card deckd
    /// puts on the deck before a firmware install.
    fn question(agent: u8, id: u16) -> HostMsg {
        let mut choices = [Text::new(); 4];
        choices[0] = Text::from_str("Install");
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind: CardKind::Question,
            text: Text::from_str("Install firmware 0.14.0? Press ENTER."),
            choices,
            choice_count: 1,
            recommended: Some(0),
        }
    }

    fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(Control::Button(button, Edge::Press), at, &mut out);
        ui.apply(Control::Button(button, Edge::Release), at + 1, &mut Update::new());
        out
    }

    fn events(out: &Update) -> std::vec::Vec<crate::deck::msg::Event> {
        out.deck_events().collect()
    }

    fn wheel(ui: &mut UiState, wheel: Wheel, at: u32) {
        ui.apply(Control::Wheel(wheel), at, &mut Update::new());
    }

    fn drawer_ids(ui: &UiState) -> std::vec::Vec<(u8, u16)> {
        filed(ui).map(|slot| identity(&ui.deck.notices[slot])).collect()
    }

    #[test]
    fn a_notice_under_an_opaque_overlay_is_filed_without_a_toast() {
        for overlay in [
            global::Overlay::Drawer,
            global::Overlay::Manager,
            global::Overlay::Gallery,
            global::Overlay::Naming,
        ] {
            let mut ui = hosted();
            ui.global.overlay = overlay;
            let mut out = Update::new();
            raise(&mut ui, Notice::new(2, 7, CardKind::Question, 100), 100, &mut out);
            assert!(ui.notify.toast.is_none(), "{overlay:?}");
            assert_eq!(ui.deck.notice_count(), 1);
            assert!(out.dirty && out.wake);
            raise_message(&mut ui, ToastKind::NotDelivered, 2, 7, 200, &mut out);
            assert!(ui.notify.toast.is_none());
        }
        // The LISTENING overlay hides a toast too; the confirm dialog does not.
        let mut ui = hosted();
        ui.global.ptt = Some(PttSession::new(2, crate::deck::msg::PttCtx::Talk, 0, 0));
        raise(&mut ui, Notice::new(2, 7, CardKind::Ready, 100), 100, &mut Update::new());
        assert!(ui.notify.toast.is_none() && ui.deck.notice_count() == 1);
        let mut ui = hosted();
        ui.global.overlay = global::Overlay::ConfirmRemove;
        raise(&mut ui, Notice::new(2, 7, CardKind::Ready, 100), 100, &mut Update::new());
        assert!(ui.notify.toast.is_some() && ui.deck.notice_count() == 0);
    }

    #[test]
    fn a_toast_is_tappable_only_while_it_is_on_the_glass() {
        let mut ui = hosted();
        assert!(!toast_tappable(&ui));
        raise(&mut ui, Notice::new(2, 7, CardKind::Ready, 100), 100, &mut Update::new());
        assert!(toast_tappable(&ui));
        ui.global.overlay = global::Overlay::Manager;
        assert!(!toast_tappable(&ui));
        ui.global.overlay = global::Overlay::None;
        // A demo toast is tappable like any other.
        ui.deck.host_present = false;
        assert!(ui.deck.demo_active() && toast_tappable(&ui));
    }

    #[test]
    fn a_toast_times_out_into_the_drawer_anchored_at_the_shown_frame() {
        let mut ui = roster();
        let mut out = Update::new();
        ui.host(&card(3, 30, CardKind::Question), 1000, &mut out);
        assert!(out.wake && out.dirty);
        let toast = ui.notify.toast.unwrap();
        assert_eq!((toast.notice(), toast.until_ms), (Some((3, 30)), 5000));
        assert_eq!(ui.deck.notice_count(), 0, "a toast is not in the drawer");
        // The panel woke late: the 4 s start with the first visible frame.
        ui.frame_shown(2500);
        ui.tick(5000, 0, true, &mut Update::new());
        assert!(ui.notify.toast.is_some());
        let mut out = Update::new();
        ui.tick(6500, 0, true, &mut out);
        assert!(ui.notify.toast.is_none() && out.dirty);
        assert_eq!((ui.deck.notice_count(), ui.deck.drawer_has_question()), (1, true));
        assert!(swallows_touch(&ui, 240, 40, 6500 + TOAST_SWALLOW_MS - 1));
        assert!(!swallows_touch(&ui, 240, 40, 6500 + TOAST_SWALLOW_MS));
        assert!(!swallows_touch(&ui, 240, 100, 6600));
    }

    #[test]
    fn same_card_pushed_twice_toasts_once_and_a_snoozed_repush_never_re_toasts() {
        let mut ui = roster();
        ui.host(&card(3, 30, CardKind::Question), 1000, &mut Update::new());
        ui.tick(5000, 0, true, &mut Update::new());
        assert_eq!(ui.deck.notice_count(), 1);
        let mut out = Update::new();
        ui.host(&card(3, 30, CardKind::Question), 6000, &mut out);
        assert!(ui.notify.toast.is_none() && !out.wake);
        assert_eq!(ui.deck.notice_count(), 1);
        // Snoozed: the notice is in the drawer; a resync re-push stays quiet.
        ui.deck.agent_mut(3).unwrap().card.snoozed = true;
        ui.deck.settings.auto_jump = true;
        ui.host(&HostMsg::Hello { v: 1, sid: 2, minute_of_day: None }, 7000, &mut Update::new());
        assert!(ui.deck.syncing);
        ui.host(&card(3, 30, CardKind::Question), 7001, &mut Update::new());
        assert!(ui.notify.toast.is_none());
        assert_eq!(ui.agents.view, AgentsView::Roster);
        // Even raised directly (a card known only to the drawer), snoozed
        // means filed.
        raise(&mut ui, Notice::new(3, 30, CardKind::Question, 7002), 7002, &mut Update::new());
        assert!(ui.notify.toast.is_none() && ui.deck.notice_count() == 1);
    }

    #[test]
    fn a_resync_files_new_notices_without_toast_or_wake() {
        let mut ui = roster();
        ui.host(&HostMsg::Hello { v: 1, sid: 5, minute_of_day: None }, 100, &mut Update::new());
        assert!(ui.deck.syncing);
        let mut out = Update::new();
        ui.host(&card(3, 31, CardKind::Question), 200, &mut out);
        assert!(!out.wake && ui.notify.toast.is_none());
        assert_eq!(ui.deck.notice_count(), 1);
        ui.host(&agent(3), 250, &mut Update::new());
        ui.host(&HostMsg::Synced, 300, &mut Update::new());
        assert_eq!(ui.deck.notice_count(), 1, "confirmed: the notice survived the purge");
        let mut out = Update::new();
        ui.host(&card(3, 32, CardKind::Ready), 400, &mut out);
        assert!(out.wake && ui.notify.toast.is_some());
    }

    #[test]
    fn a_newer_toast_displaces_the_old_one_into_the_drawer() {
        let mut ui = roster();
        ui.host(&card(3, 30, CardKind::Ready), 1000, &mut Update::new());
        ui.host(&card(4, 40, CardKind::Question), 1500, &mut Update::new());
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((4, 40)));
        assert_eq!(drawer_ids(&ui), [(3, 30)]);
        assert!(ui.deck.drawer_has_question() == false);
        raise_message(&mut ui, ToastKind::NoChannel, 0, 0, 1600, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40)]);
        assert!(ui.deck.drawer_has_question());
    }

    #[test]
    fn the_ring_drops_the_oldest_ready_first_then_the_oldest() {
        let mut ui = roster();
        for id in 1..=8u16 {
            let kind = if id % 2 == 0 { CardKind::Ready } else { CardKind::Question };
            let agent = 3;
            ui.deck.agent_mut(agent).unwrap().has_card = true;
            ui.deck.agent_mut(agent).unwrap().card.id = id;
            raise(&mut ui, Notice::new(agent, id, kind, id as u32 * 100), id as u32 * 100, &mut Update::new());
        }
        // Eight notices for one agent: only the live card survives a prune,
        // so file them by hand instead of through the model.
        assert_eq!(ui.deck.notices.iter().filter(|n| n.used).count(), 8);
        ui.deck.agent_mut(3).unwrap().card.id = 9;
        raise(&mut ui, Notice::new(3, 9, CardKind::Question, 900), 900, &mut Update::new());
        let ids: std::vec::Vec<u16> = ui.deck.notices.iter().filter(|n| n.used).map(|n| n.card).collect();
        assert!(!ids.contains(&2), "the oldest Ready (card 2) went: {ids:?}");
        assert!(ids.contains(&1) && ids.contains(&9));
        for notice in &mut ui.deck.notices {
            notice.kind = CardKind::Question;
        }
        ui.deck.agent_mut(3).unwrap().card.id = 10;
        raise(&mut ui, Notice::new(3, 10, CardKind::Question, 1000), 1000, &mut Update::new());
        let ids: std::vec::Vec<u16> = ui.deck.notices.iter().filter(|n| n.used).map(|n| n.card).collect();
        assert!(!ids.contains(&1), "no Ready left: the oldest went: {ids:?}");
    }

    #[test]
    fn notices_go_with_their_card_on_clear_replace_gone_and_purge() {
        let mut ui = roster();
        ui.host(&card(3, 30, CardKind::Question), 1000, &mut Update::new());
        ui.host(&card(4, 40, CardKind::Question), 1100, &mut Update::new());
        ui.tick(6000, 0, true, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40)]);
        let mut clear = card(3, 30, CardKind::Question);
        if let HostMsg::Card { clear, .. } = &mut clear {
            *clear = true;
        }
        ui.host(&clear, 6100, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(4, 40)]);
        ui.host(&card(4, 41, CardKind::Ready), 6200, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [], "replaced: the old notice went, the new one toasts");
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((4, 41)));
        let gone = HostMsg::Agent {
            id: 4,
            gone: true,
            kind: None,
            state: None,
            name: None,
            project: None,
            title: None,
            since_s: None, task: None, subs: None,
        };
        ui.host(&gone, 6300, &mut Update::new());
        assert!(ui.notify.toast.is_none(), "the toast of a gone agent goes too");
        // A purge happens in `tick`: the drawer follows there as well.
        ui.host(&card(3, 33, CardKind::Question), 7000, &mut Update::new());
        ui.tick(12_000, 0, true, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 33)]);
        ui.host(&HostMsg::Hello { v: 1, sid: 9, minute_of_day: None }, 12_100, &mut Update::new());
        let mut out = Update::new();
        ui.tick(12_100 + crate::deck::model::SYNC_TIMEOUT_MS, 0, true, &mut out);
        assert_eq!(drawer_ids(&ui), []);
        assert!(out.dirty);
    }

    /// A notice arriving (or leaving) while the drawer is open
    /// moves rows, never the selection, and ENTER still jumps to the selected one.
    #[test]
    fn notice_arriving_while_drawer_open_keeps_selection_identity() {
        let mut ui = roster();
        for (agent, id) in [(3, 30), (4, 40), (5, 50)] {
            ui.host(&card(agent, id, CardKind::Question), 1000, &mut Update::new());
        }
        ui.tick(6000, 0, true, &mut Update::new());
        ui.on_face(Template::Usage);
        open_drawer(&mut ui, 6100, &mut Update::new());
        wheel(&mut ui, Wheel::Ccw, 6200);
        assert_eq!(ui.notify.drawer_sel, (4, 40));
        let mut out = Update::new();
        ui.host(&card(1, 10, CardKind::Question), 6300, &mut out);
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40), (5, 50), (1, 10)]);
        assert_eq!(ui.notify.drawer_sel, (4, 40), "a new notice never moves the selection");
        assert!(ui.notify.toast.is_none() && out.dirty);
        // A notice before the selection leaving moves its row, not the selection.
        let mut clear = card(3, 30, CardKind::Question);
        if let HostMsg::Card { clear: gone, .. } = &mut clear {
            *gone = true;
        }
        ui.host(&clear, 6400, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(4, 40), (5, 50), (1, 10)]);
        assert_eq!(ui.notify.drawer_sel, (4, 40));
        // The selected card replaced under it: the notice keeps its row and
        // the selection follows the agent's new card.
        ui.host(&card(4, 41, CardKind::Question), 6500, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(4, 41), (5, 50), (1, 10)]);
        assert_eq!(ui.notify.drawer_sel, (4, 41));
        let out = press(&mut ui, Button::Enter, 6600);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 4));
        assert_eq!(ui.deck.card(4).map(|c| c.id), Some(41));
        assert!(out.deck_events().all(|e| !matches!(e, crate::deck::msg::Event::Channel { .. })));
    }

    #[test]
    fn drawer_selection_is_identity_keyed_and_dismiss_keeps_the_card() {
        let mut ui = roster();
        for (agent, id) in [(3, 30), (4, 40), (5, 50)] {
            ui.host(&card(agent, id, CardKind::Question), 1000, &mut Update::new());
        }
        ui.tick(6000, 0, true, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40), (5, 50)]);
        let mut out = Update::new();
        open_drawer(&mut ui, 6100, &mut out);
        assert_eq!((ui.global.overlay, ui.notify.drawer_sel), (Overlay::Drawer, (3, 30)));
        wheel(&mut ui, Wheel::Ccw, 6200);
        assert_eq!(ui.notify.drawer_sel, (4, 40));
        // A notice arriving while the drawer is open never moves the selection.
        ui.host(&card(1, 10, CardKind::Ready), 6300, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40), (5, 50), (1, 10)]);
        assert_eq!(ui.notify.drawer_sel, (4, 40));
        assert!(ui.notify.toast.is_none(), "no toast while the drawer is open");
        wheel(&mut ui, Wheel::Ccw, 6400);
        wheel(&mut ui, Wheel::Ccw, 6500);
        wheel(&mut ui, Wheel::Ccw, 6600);
        assert_eq!(ui.notify.drawer_sel, (1, 10), "hard end");
        wheel(&mut ui, Wheel::Cw, 6700);
        let out = press(&mut ui, Button::Right, 6800);
        assert!(out.deck_events().count() == 0 && out.dirty);
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40), (1, 10)]);
        assert_eq!(ui.notify.drawer_sel, (1, 10), "the next one is selected");
        assert!(ui.deck.card(5).is_some(), "dismiss leaves the card alone");
        press(&mut ui, Button::Left, 6900);
        assert_eq!(ui.notify.drawer_sel, (4, 40), "the last one: its predecessor");
        press(&mut ui, Button::Cancel, 7000);
        assert_eq!(ui.global.overlay, Overlay::None);
        // ENTER jumps to the card and closes the drawer.
        open_drawer(&mut ui, 7100, &mut Update::new());
        ui.on_face(Template::Usage);
        ui.global.overlay = Overlay::Drawer;
        let out = press(&mut ui, Button::Enter, 7200);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 4));
        assert!(out.deck_events().all(|e| !matches!(e, crate::deck::msg::Event::Channel { .. })));
        assert_eq!(drawer_ids(&ui), [(3, 30), (4, 40)], "opening keeps the notice until resolved");
    }

    #[test]
    fn drawer_touch_taps_swipes_and_scrolls_rows_by_identity() {
        let mut ui = roster();
        for (agent, id) in [(3, 30), (4, 40), (5, 50)] {
            ui.host(&card(agent, id, CardKind::Question), 1000, &mut Update::new());
        }
        ui.tick(6000, 0, true, &mut Update::new());
        open_drawer(&mut ui, 6100, &mut Update::new());
        let touch = |ui: &mut UiState, phase, x: i32, y: i32, at| {
            let touch = Touch { phase, x: x as u16, y: y as u16 };
            ui.apply(Control::Touch(touch), at, &mut Update::new());
        };
        let (x, y) = layout::drawer_row(1).center();
        touch(&mut ui, TouchPhase::Down, x, y, 6200);
        assert_eq!(ui.notify.drawer_sel, (4, 40));
        touch(&mut ui, TouchPhase::Move, x + 70, y + 5, 6210);
        touch(&mut ui, TouchPhase::Up, x + 70, y + 5, 6220);
        assert_eq!(drawer_ids(&ui), [(3, 30), (5, 50)], "a swipe dismisses");
        assert_eq!(ui.global.overlay, Overlay::Drawer);
        // The row under the finger changed hands between Down and Up: no-op.
        let (x, y) = layout::drawer_row(1).center();
        touch(&mut ui, TouchPhase::Down, x, y, 6300);
        assert_eq!(ui.notify.drawer_sel, (5, 50));
        dismiss(&mut ui, 5, 50, &mut Update::new());
        ui.host(&agent(6), 6305, &mut Update::new());
        ui.host(&card(6, 60, CardKind::Question), 6310, &mut Update::new());
        assert_eq!(drawer_ids(&ui), [(3, 30), (6, 60)]);
        touch(&mut ui, TouchPhase::Up, x, y, 6320);
        assert_eq!(ui.global.overlay, Overlay::Drawer);
        // A tap jumps.
        let (x, y) = layout::drawer_row(0).center();
        touch(&mut ui, TouchPhase::Down, x, y, 6400);
        touch(&mut ui, TouchPhase::Up, x, y, 6410);
        assert_eq!((ui.global.overlay, ui.agents.view, ui.agents.agent), (Overlay::None, AgentsView::Card, 3));
        // Scrolling: six notices, five rows.
        let mut ui = roster();
        ui.host(&agent(6), 900, &mut Update::new());
        for agent in 1..=6u8 {
            ui.host(&card(agent, agent as u16 * 10, CardKind::Ready), 1000, &mut Update::new());
        }
        ui.tick(6000, 0, true, &mut Update::new());
        assert_eq!(filed_count(&ui), 6);
        open_drawer(&mut ui, 6100, &mut Update::new());
        let (x, y) = layout::drawer_row(2).center();
        touch(&mut ui, TouchPhase::Down, x, y, 6200);
        touch(&mut ui, TouchPhase::Move, x, y - 20, 6210);
        assert_eq!(ui.notify.drawer_top, 1);
        touch(&mut ui, TouchPhase::Up, x, y - 20, 6220);
        assert_eq!(ui.global.overlay, Overlay::Drawer, "a scroll is not a tap");
        assert_eq!(hit(&ui, x, y).map(|t| t.id & 7), filed(&ui).nth(3).map(|slot| slot as u16));
    }

    #[test]
    fn toast_tap_jumps_without_filing_and_a_message_toast_just_goes() {
        let mut ui = roster();
        ui.on_face(Template::Omarchy);
        ui.host(&card(3, 30, CardKind::Ready), 1000, &mut Update::new());
        let mut out = Update::new();
        toast_tapped(&mut ui, 1100, &mut out);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        assert_eq!(ui.deck.notice_count(), 0);
        assert!(!global::guard_passed(&ui, 1200, 1300));
        assert!(out.deck_events().all(|e| !matches!(e, crate::deck::msg::Event::Channel { .. })));
        raise_message(&mut ui, ToastKind::NotDelivered, 3, 30, 2000, &mut Update::new());
        toast_tapped(&mut ui, 2100, &mut Update::new());
        assert!(ui.notify.toast.is_none());
        // A toast whose card was resolved meanwhile still lands on the channel.
        ui.on_face(Template::Omarchy);
        ui.notify.toast = Some(Toast::card(4, 99, CardKind::Question, 2200));
        toast_tapped(&mut ui, 2300, &mut Update::new());
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 4));
    }

    #[test]
    fn auto_jump_takes_a_question_only_when_nothing_would_be_interrupted() {
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Usage);
        let mut out = Update::new();
        ui.host(&card(3, 30, CardKind::Question), 5000, &mut out);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        assert!(ui.notify.toast.is_none() && out.wake);
        assert!(!global::guard_passed(&ui, 5100, 5400), "enter 50 ms after the jump does nothing");
        // Deferred (a toast instead) while a Card shows, anything is held, an
        // overlay is open, the strip slides, or input was < 2 s ago.
        ui.host(&card(4, 40, CardKind::Question), 6000, &mut Update::new());
        assert_eq!(ui.agents.agent, 3);
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((4, 40)));
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Usage);
        ui.apply(Control::Button(Button::Enter, Edge::Press), 4000, &mut Update::new());
        ui.host(&card(3, 30, CardKind::Question), 5000, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Usage);
        ui.apply(Control::Button(Button::Enter, Edge::Release), 5100, &mut Update::new());
        ui.host(&card(4, 40, CardKind::Question), 6000, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Usage, "input 900 ms ago");
        ui.host(&card(5, 50, CardKind::Question), 7100, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Agents);
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.global.overlay = Overlay::ConfirmRemove;
        ui.host(&card(3, 30, CardKind::Question), 5000, &mut Update::new());
        assert_eq!(ui.global.overlay, Overlay::ConfirmRemove);
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Usage);
        ui.global.strip_x += 1000;
        ui.host(&card(3, 30, CardKind::Question), 5000, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Usage);
        // Ready never jumps; without a host nothing does.
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Usage);
        ui.host(&card(3, 30, CardKind::Ready), 5000, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Usage);
        // A card left open on the Agents face is not showing from Usage.
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Agents);
        super::super::agents::show_card(&mut ui, 1, 100);
        assert_eq!(ui.agents.view, AgentsView::Card);
        ui.on_face(Template::Usage);
        ui.host(&card(3, 30, CardKind::Question), 5000, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        assert!(ui.notify.toast.is_none());
    }

    /// Auto-jump is off by default, so a question raised while the deck sits
    /// on the roster is a toast. With nothing it could interrupt, the next
    /// ENTER opens its card (not the highlighted session's channel), and the
    /// card answers the ENTER after that under its own guard.
    #[test]
    fn enter_opens_a_card_toast_that_arrived_with_nothing_to_interrupt() {
        use crate::deck::msg::Event;
        let mut ui = roster();
        assert!(!ui.deck.settings.auto_jump);
        assert_eq!((ui.agents.view, ui.current_face().template), (AgentsView::Roster, Template::Agents));
        assert_ne!(ui.agents.highlight, 3);
        ui.host(&question(3, 30), 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| t.takes_enter));
        let out = press(&mut ui, Button::Enter, 5600);
        assert_eq!((ui.agents.view, ui.agents.agent, ui.agents.choice), (AgentsView::Card, 3, Some(0)));
        assert!(ui.notify.toast.is_none());
        assert_eq!(ui.deck.notice_count(), 0, "opened, not filed");
        assert_eq!(events(&out), [], "a jump: no channel, and no answer yet");
        assert!(press(&mut ui, Button::Enter, 5900).deck_events().next().is_none(), "within 0.5 s of opening");
        let out = press(&mut ui, Button::Enter, 6200);
        assert_eq!(events(&out), [Event::Answer { agent: 3, card: 30, choice: 0 }]);
        // From another face it is the same jump a tap makes.
        let mut ui = roster();
        ui.on_face(Template::Usage);
        ui.host(&card(4, 40, CardKind::Ready), 5000, &mut Update::new());
        let out = press(&mut ui, Button::Enter, 5600);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 4));
        assert!(out.deck_events().all(|e| !matches!(e, Event::Refresh | Event::Channel { .. })));
    }

    /// Whoever was using the deck keeps ENTER: input in the 2 s before the
    /// toast, anything pressed, turned or touched after it, a SHIFT chord, an
    /// overlay, a card on screen. The toast is still tappable throughout.
    #[test]
    fn a_card_toast_never_takes_an_enter_that_means_something_else() {
        use crate::deck::msg::Event;
        // Input 1 s before: ENTER opens the highlighted channel, as ever.
        let mut ui = roster();
        wheel(&mut ui, Wheel::Cw, 4000);
        let highlight = ui.agents.highlight;
        assert_ne!(highlight, 3);
        ui.host(&question(3, 30), 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
        press(&mut ui, Button::Enter, 5600);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, highlight));
        assert!(ui.notify.toast.is_some() && toast_tappable(&ui));
        // A detent first: the user turned to something else.
        let mut ui = roster();
        ui.host(&question(3, 30), 5000, &mut Update::new());
        wheel(&mut ui, Wheel::Cw, 5200);
        let highlight = ui.agents.highlight;
        press(&mut ui, Button::Enter, 5400);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, highlight));
        // A new touch ends the claim too; a release of an earlier press does not.
        let mut ui = roster();
        ui.host(&question(3, 30), 5000, &mut Update::new());
        assert!(!claims_enter(&mut ui, Control::Button(Button::Left, Edge::Release)));
        assert!(ui.notify.toast.is_some_and(|t| t.takes_enter));
        let down = Control::Touch(Touch { phase: TouchPhase::Down, x: 240, y: 300 });
        assert!(!claims_enter(&mut ui, down));
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
        // SHIFT already down when the toast came: SHIFT+ENTER stays a chord.
        let mut ui = roster();
        ui.apply(Control::Button(Button::Shift, Edge::Press), 1000, &mut Update::new());
        ui.host(&question(3, 30), 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| t.takes_enter));
        press(&mut ui, Button::Enter, 5600);
        assert_eq!(ui.agents.view, AgentsView::Roster);
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
        // Omarchy and the Macropad send their keys to the computer, the deck
        // looked at or not: ENTER stays the key.
        for (template, is_key) in [
            (Template::Omarchy, (|e: &Event| matches!(e, Event::Key { key: crate::deck::msg::KeyId::Enter, act: 1, .. })) as fn(&Event) -> bool),
            (Template::Macropad, |e: &Event| matches!(e, Event::Macro { act: 1, .. })),
        ] {
            let mut ui = roster();
            ui.on_face(template);
            ui.host(&question(3, 30), 5000, &mut Update::new());
            let out = press(&mut ui, Button::Enter, 5600);
            assert_eq!(ui.current_face().template, template);
            assert!(out.deck_events().any(|e| is_key(&e)), "{template:?}: {:?}", events(&out));
            assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter) && toast_tappable(&ui));
        }
        // The remove dialog shows the toast above it, but ENTER is the dialog's.
        let mut ui = roster();
        ui.host(&question(3, 30), 5000, &mut Update::new());
        ui.global.overlay = Overlay::ConfirmRemove;
        assert!(!claims_enter(&mut ui, Control::Button(Button::Enter, Edge::Press)));
        // A card on screen when the toast came: its ENTER answers it.
        let mut ui = roster();
        super::super::agents::show_card(&mut ui, 1, 100);
        ui.host(&question(3, 30), 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
        press(&mut ui, Button::Enter, 5600);
        assert_ne!(ui.agents.agent, 3);
        // A card that came on screen by itself since then (auto-jump took a
        // question; a Ready card never jumps, so its toast stayed up).
        let mut ui = roster();
        ui.deck.settings.auto_jump = true;
        ui.host(&card(4, 40, CardKind::Ready), 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| t.takes_enter));
        ui.host(&question(3, 30), 5100, &mut Update::new());
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((4, 40)));
        let out = press(&mut ui, Button::Enter, 5700);
        assert_eq!(events(&out), [Event::Answer { agent: 3, card: 30, choice: 0 }]);
    }

    /// Message and clock toasts have no card to open: ENTER is the face's.
    #[test]
    fn only_a_card_toast_takes_enter() {
        use crate::deck::msg::Event;
        let mut ui = roster();
        ui.on_face(Template::Usage);
        raise_message(&mut ui, ToastKind::NotDelivered, 3, 30, 5000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
        let out = press(&mut ui, Button::Enter, 5600);
        assert_eq!(ui.current_face().template, Template::Usage);
        assert!(out.deck_events().any(|e| matches!(e, Event::Refresh)));
        raise_timer(&mut ui, 1, 9000, &mut Update::new());
        assert!(ui.notify.toast.is_some_and(|t| !t.takes_enter));
    }
}
