//! The global layer: strip and slide, SHIFT / double-SHIFT, the PTT session,
//! press ownership and teardown, confirm guards, idle-home.
//!
//! Host `faces` is NOT handled here: `manager::on_host` restores the strip
//! and calls `jump_to` for the clamp and teardown; `focus` is.
//!
//! One principle: every press latches an owner (`held`); every context
//! teardown (`release_owned`) emits that owner's release and the physical
//! release finds nothing to do. A PTT session latches its target at the press
//! and the release repeats it whatever is on screen by then; while one is live
//! nothing navigates.
use super::{owner_released, Owner, Source, UiState, Update, FACE_W, MAX_FACES, STRIP_ONE};
use crate::control::{Button, Control, Edge, TouchPhase, Wheel};
use crate::deck::model::Effects;
use crate::deck::msg::{ErrWhy, Event, FaceEntry, HostMsg, PttCtx, Template};

const ERR_PERIOD_MS: u32 = 1000;
/// One face slide; each half of a bounce.
pub const SLIDE_MS: u32 = 220;
const BOUNCE_MS: u32 = 110;
/// How far a hard end gives, in px.
pub const BOUNCE_PX: i32 = 12;
/// At most one accepted face detent per this.
pub const DETENT_MS: u32 = 150;
/// A SHIFT+wheel run ends after this long without a detent (the hard-end
/// bounce happens once per run).
pub const WHEEL_TAIL_MS: u32 = 300;
/// A plain wheel this soon after a SHIFT+wheel is swallowed as the dev
/// keypad's key repeat still running after SHIFT came up. The Rev A wheel is
/// a real encoder with one event per detent, so nothing is swallowed there.
pub const WHEEL_SWALLOW_MS: u32 = if cfg!(feature = "hw-a") { 0 } else { WHEEL_TAIL_MS };
/// A SHIFT tap is shorter than this...
pub const TAP_MS: u32 = 300;
/// ...and the second one starts within this of the first one's release.
pub const TAP_GAP_MS: u32 = 400;
/// A lost PTT key-up cannot keep the mic hot longer than this.
pub const PTT_CAP_MS: u32 = 60_000;
/// Past the host's 120 s watchdog, release a screen whose result was lost.
pub const TRANSCRIBE_CAP_MS: u32 = 125_000;
/// Confirms are ignored this long after a view change the user did not make.
pub const GUARD_MS: u32 = 500;
/// A CANCEL the deck itself owns, held this long, shows the power-off
/// dialog (above every tap threshold, so a CANCEL tap never flashes it)...
pub const POWER_HOLD_SHOW_MS: u32 = 700;
/// ...and held this long powers the deck off (`Update::power_off`).
pub const POWER_HOLD_MS: u32 = 3_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    None,
    Drawer,
    Manager,
    Gallery,
    Naming,
    ConfirmRemove,
    /// Settings > About > Factory reset: the confirm dialog (`settings`).
    ConfirmReset,
    /// The first-run tutorial (`tutorial`): takes every control before this
    /// layer sees it.
    Tutorial,
    /// A Macropad key being edited on the deck (`macropad`).
    MacroEdit,
}

/// Latched at the PTT press; the release repeats exactly this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PttSession {
    pub press: u16,
    pub agent: u8,
    pub ctx: PttCtx,
    /// 0 = none.
    pub card: u16,
    pub started_ms: u32,
}

impl PttSession {
    pub const fn new(agent: u8, ctx: PttCtx, card: u16, started_ms: u32) -> Self {
        Self {
            press: 0,
            agent,
            ctx,
            card,
            started_ms,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Global {
    pub strip: [FaceEntry; MAX_FACES],
    pub strip_len: u8,
    /// Index into `strip`; input goes here even while the slide catches up.
    pub current: u8,
    /// px * 256, easing toward `current * 480 * 256`.
    pub strip_x: i32,
    pub overlay: Overlay,
    pub shift: bool,
    pub ptt: Option<PttSession>,
    /// Capture has stopped; retain its words until its own final arrives.
    pub ptt_pending: Option<PttSession>,
    ptt_seq: u16,
    /// Agent id; 0 = none.
    pub active_channel: u8,
    /// One slot per `Button` in declaration order, then the finger.
    held: [Option<Owner>; 7],
    channel_seq: u16,
    last_input_ms: u32,
    guard_at_ms: u32,
    /// The last `err` sent, for the 1/s limit.
    err_at_ms: Option<u32>,
    // The slide: `strip_x` eases from `slide_from_x` (at `slide_at_ms`) to
    // `slide_target` over `slide_ms`; `bounce` is the sign of an overshoot
    // still to be shown.
    slide_from_x: i32,
    slide_target: i32,
    slide_at_ms: u32,
    slide_ms: u32,
    bounce: i8,
    bounced: bool,
    /// The last SHIFT+wheel of any kind, and the last accepted detent.
    shift_wheel_ms: Option<u32>,
    detent_ms: Option<u32>,
    // Double-SHIFT: a tap is a clean SHIFT press/release under `TAP_MS`.
    shift_at_ms: u32,
    shift_clean: bool,
    first_tap_ms: Option<u32>,
    /// A host `focus` that arrived during a PTT session; 0 = none.
    pending_focus: u8,
    /// Mirror of `deck.host_present`, so `on_tick` sees the heartbeat go.
    host_seen: bool,
    /// The heartbeat went with a release the host never got (the link is
    /// still up, so it did not release by itself): the next hello owes `up`.
    owe_up: bool,
    idle_homed: bool,
    /// `screen_on` of the last tick, and whether it was on at the last two:
    /// time-derived pixels are only reported dirty while there is a screen to
    /// repaint (a wake draws a whole frame anyway).
    screen_on: bool,
    lit: bool,
    /// The LISTENING waveform phase of the last tick.
    wave_phase: u32,
    /// A CANCEL press the deck itself owns, still held: when it was pressed.
    power_hold: Option<u32>,
    /// The power-off dialog's bar, 0..=100 in steps of 5, while it is up.
    power_hold_shown: Option<u8>,
}

impl Global {
    pub const fn new() -> Self {
        Self {
            strip: [FaceEntry::EMPTY; MAX_FACES],
            strip_len: 0,
            current: 0,
            strip_x: 0,
            overlay: Overlay::None,
            shift: false,
            ptt: None,
            ptt_pending: None,
            ptt_seq: 0,
            active_channel: 0,
            held: [None; 7],
            channel_seq: 0,
            last_input_ms: 0,
            guard_at_ms: 0,
            err_at_ms: None,
            slide_from_x: 0,
            slide_target: 0,
            slide_at_ms: 0,
            slide_ms: SLIDE_MS,
            bounce: 0,
            bounced: false,
            shift_wheel_ms: None,
            detent_ms: None,
            shift_at_ms: 0,
            shift_clean: false,
            first_tap_ms: None,
            pending_focus: 0,
            host_seen: false,
            owe_up: false,
            idle_homed: false,
            screen_on: true,
            lit: true,
            wave_phase: 0,
            power_hold: None,
            power_hold_shown: None,
        }
    }
    /// The `seq` of the last `channel` event, for `focus.ack`.
    pub fn channel_seq(&self) -> u16 {
        self.channel_seq
    }
}

fn slot(source: Source) -> usize {
    match source {
        Source::Button(button) => button as usize,
        Source::Touch => 6,
    }
}

fn base_x(ui: &UiState) -> i32 {
    ui.global.current as i32 * FACE_W * STRIP_ONE
}

/// Where the slide is heading right now: the face, or the bounce beyond it.
fn target_x(ui: &UiState) -> i32 {
    base_x(ui) + ui.global.bounce as i32 * BOUNCE_PX * STRIP_ONE
}

/// A LISTENING session: nothing navigates until it ends.
fn session_blocks(ui: &UiState) -> bool {
    ui.global.ptt.is_some() || ui.global.ptt_pending.is_some()
}

fn elapsed(now_ms: u32, since_ms: u32) -> u32 {
    now_ms.wrapping_sub(since_ms)
}

// ---- dispatch hooks (called by mod.rs) ----------------------------------------

/// Sees every control first. `true` = consumed.
pub(super) fn on_control(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) -> bool {
    ui.global.last_input_ms = at_ms;
    // A fresh gesture supersedes a deferred host selection. Release edges
    // only finish the input that prevented the focus from being shown.
    if match control {
        Control::Button(_, Edge::Press) | Control::Wheel(_) => true,
        Control::Touch(touch) => touch.phase == TouchPhase::Down,
        _ => false,
    } {
        ui.agents.deferred_focus = 0;
    }
    // A change of mind ends a power-off hold: any button or detent (the
    // CANCEL's own release included) or a new finger. SHIFT edges and the
    // tail of a gesture that was already down are not one.
    let change_of_mind = match control {
        Control::Button(Button::Shift, _) | Control::Button(Button::Cancel, Edge::Press) => false,
        Control::Touch(touch) => touch.phase == TouchPhase::Down,
        _ => true,
    };
    if change_of_mind {
        power_hold_cancel(ui, out);
    }
    // Anything but a SHIFT edge or the rest of a touch spoils a SHIFT tap.
    let spoils = match control {
        Control::Button(Button::Shift, _) => false,
        Control::Touch(touch) => touch.phase == TouchPhase::Down,
        _ => true,
    };
    if spoils {
        ui.global.shift_clean = false;
        ui.global.first_tap_ms = None;
    }
    // The tutorial teaches the very chords and sessions handled below, so it
    // takes every control first (the power-off hold above still counts).
    if ui.global.overlay == Overlay::Tutorial {
        super::tutorial::on_control(ui, control, at_ms, out);
        return true;
    }
    match control {
        Control::Button(Button::Shift, Edge::Press) => {
            // A press while already down is a fresh press (self-heal).
            ui.global.shift = true;
            out.dirty = true;
            ui.global.shift_at_ms = at_ms;
            ui.global.shift_clean = true;
            true
        }
        Control::Button(Button::Shift, Edge::Release) => {
            ui.global.shift = false;
            out.dirty = true;
            shift_released(ui, at_ms, out);
            true
        }
        Control::Button(button, Edge::Release) => {
            release_source(ui, Source::Button(button), out);
            if button == Button::PushToTalk {
                end_ptt(ui, false, out);
            }
            true
        }
        Control::Wheel(wheel) if ui.global.shift => {
            shift_wheel(ui, wheel, at_ms, out);
            true
        }
        Control::Wheel(_)
            if ui
                .global
                .shift_wheel_ms
                .is_some_and(|last| elapsed(at_ms, last) < WHEEL_SWALLOW_MS) =>
        {
            true // The wheel key is still repeating after SHIFT came up.
        }
        _ if session_blocks(ui) => {
            match control {
                Control::Button(Button::Cancel, Edge::Press) => {
                    if ui.global.ptt_pending.take().is_some() {
                        out.push_deck(Event::Up);
                        out.dirty = true;
                    }
                    end_ptt(ui, true, out);
                }
                Control::Button(Button::PushToTalk, Edge::Press) if ui.global.ptt.is_none() => {
                    ptt_pressed(ui, at_ms, out);
                }
                // A finger that was down before the session still has to
                // come up somewhere (its gesture was orphaned at the start).
                Control::Touch(touch) if touch.phase != TouchPhase::Down => {
                    super::touch::on_touch(ui, touch, at_ms, out)
                }
                _ => {}
            }
            true
        }
        Control::Button(Button::PushToTalk, Edge::Press) if super::arcade::active(ui) => {
            super::arcade::on_control(ui, control, at_ms, out)
        }
        Control::Button(Button::PushToTalk, Edge::Press) => {
            ptt_pressed(ui, at_ms, out);
            true
        }
        _ => false,
    }
}

/// Runs after the overlay/face handler (or the recogniser) saw the control.
pub(super) fn after_control(
    ui: &mut UiState,
    control: Control,
    consumed: bool,
    at_ms: u32,
    _out: &mut Update,
) {
    // A press nobody claimed is still owned, so its release is accounted for.
    if let (Control::Button(button, Edge::Press), true) = (control, consumed) {
        let held = &mut ui.global.held[slot(Source::Button(button))];
        if held.is_none() && button != Button::Shift {
            *held = Some(Owner::Local);
        }
    }
    // A CANCEL the deck itself owns (not a host key), kept down, is the
    // power-off gesture; `on_tick` times it. Its
    // press-time action (home, back, close, keep, snooze, abort) has fired.
    if let Control::Button(Button::Cancel, Edge::Press) = control {
        let host_owned = matches!(
            ui.global.held[slot(Source::Button(Button::Cancel))],
            Some(Owner::HostKey { .. } | Owner::Macro { .. })
        );
        if !host_owned {
            ui.global.power_hold = Some(at_ms);
        }
    }
}

pub(super) fn on_host(
    ui: &mut UiState,
    msg: &HostMsg,
    fx: &Effects,
    at_ms: u32,
    out: &mut Update,
) {
    if fx.hello {
        out.push_deck(Event::Hello);
        if core::mem::take(&mut ui.global.owe_up) {
            out.push_deck(Event::Up);
        }
    }
    if fx.new_session {
        // Whatever was held, spoken or focused belonged to the previous host
        // (or the demo): the first PTT press re-establishes the channel.
        release_owned(ui, out);
        out.dirty |= ui.global.ptt.take().is_some();
        out.dirty |= ui.global.ptt_pending.take().is_some();
        ui.global.pending_focus = 0;
        out.dirty |= ui.global.active_channel != 0;
        ui.global.active_channel = 0;
    }
    ui.global.host_seen = ui.deck.host_present;
    if let HostMsg::Transcript { last: true, failed, .. } = msg {
        out.dirty |= ui.global.ptt_pending.take().is_some();
        if *failed {
            // A failure can arrive while the button is still held. Emit the
            // local release too: it is what stops the device-owned mic.
            end_ptt(ui, true, out);
            super::notify::raise_message(ui, super::notify::ToastKind::TranscriptionFailed, 0, 0, at_ms, out);
        }
    }
    if let HostMsg::Focus { agent, ack } = msg {
        // An echo of our own `channel` that is older than the last one we
        // sent would roll the user back; an unsolicited focus always applies.
        let stale = ack.is_some_and(|ack| ui.global.channel_seq.wrapping_sub(ack) as i16 > 0);
        if stale || ui.deck.agent(*agent).is_none() {
            return;
        }
        if ui.global.ptt.is_some() {
            ui.global.pending_focus = *agent;
        } else {
            apply_focus(ui, *agent, at_ms, out);
        }
    }
}

fn apply_focus(ui: &mut UiState, agent: u8, at_ms: u32, out: &mut Update) {
    ui.global.pending_focus = 0;
    if ui.global.active_channel != agent {
        ui.global.active_channel = agent;
        out.dirty = true;
    }
    super::agents::on_focus(ui, agent, at_ms, out);
}

/// `Deck::apply` refused the message.
/// Every rejection comes through here (`UiState::host` for the model's,
/// `UiState::reject` for the runtime's line and parse errors): one `err` per
/// second in total, the rest are dropped.
pub(super) fn on_reject(ui: &mut UiState, why: ErrWhy, at_ms: u32, out: &mut Update) {
    if ui.global.err_at_ms.is_some_and(|sent| elapsed(at_ms, sent) < ERR_PERIOD_MS) {
        return;
    }
    ui.global.err_at_ms = Some(at_ms);
    out.push_deck(Event::Err { why });
}

pub(super) fn on_tick(
    ui: &mut UiState,
    now_ms: u32,
    idle_ms: u32,
    screen_on: bool,
    out: &mut Update,
) {
    ui.global.lit = ui.global.screen_on && screen_on;
    ui.global.screen_on = screen_on;
    // The heartbeat went: the runtime no longer sends what this teardown
    // emits, so a host-visible hold is settled by `up` after the next hello.
    if ui.global.host_seen && !ui.deck.host_present {
        let host_held = ui.global.held.iter().any(|owner| {
            matches!(owner, Some(Owner::HostKey { .. } | Owner::Macro { .. }))
        });
        ui.global.owe_up |= host_held || session_blocks(ui);
        release_owned(ui, out);
        ui.global.pending_focus = 0;
        end_ptt(ui, true, out);
        out.dirty |= ui.global.ptt_pending.take().is_some();
    }
    ui.global.host_seen = ui.deck.host_present;
    slide(ui, now_ms, out);
    if let Some(session) = ui.global.ptt {
        if elapsed(now_ms, session.started_ms) >= PTT_CAP_MS {
            end_ptt(ui, false, out);
        }
    }
    if ui.global.ptt_pending.is_some_and(|p| elapsed(now_ms, p.started_ms) >= TRANSCRIBE_CAP_MS) {
        ui.global.ptt_pending = None;
        out.push_deck(Event::Up);
        out.dirty = true;
        super::notify::raise_message(ui, super::notify::ToastKind::TranscriptionUnavailable, 0, 0, now_ms, out);
    }
    // The synthetic waveform moves with time: its lane repaints per phase.
    let phase = crate::ui::overlay::ptt::wave_phase(now_ms);
    if phase != ui.global.wave_phase {
        ui.global.wave_phase = phase;
        out.dirty |= ui.global.lit && crate::ui::overlay::ptt_visible(ui);
    }
    power_hold_tick(ui, now_ms, out);
    idle_home(ui, now_ms, idle_ms, out);
}

// ---- power-off hold -------------------------------------------------------------------------

/// The dialog's bar while the hold is past `POWER_HOLD_SHOW_MS`.
pub(super) fn power_hold(ui: &UiState) -> Option<u8> {
    ui.global.power_hold_shown
}

fn power_hold_cancel(ui: &mut UiState, out: &mut Update) {
    if ui.global.power_hold.take().is_some() {
        out.dirty |= ui.global.power_hold_shown.take().is_some();
    }
}

/// Time a held CANCEL: the dialog appears at `POWER_HOLD_SHOW_MS` and its bar
/// fills in 5 % steps (a repaint of the bar alone, like the LISTENING
/// waveform) until `POWER_HOLD_MS` sets `Update::power_off`, once.
fn power_hold_tick(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    let Some(pressed_ms) = ui.global.power_hold else {
        return;
    };
    let held = elapsed(now_ms, pressed_ms);
    if held >= POWER_HOLD_MS {
        ui.global.power_hold = None;
        ui.global.power_hold_shown = None;
        out.power_off = true;
        out.dirty = true;
        return;
    }
    let shown = held.checked_sub(POWER_HOLD_SHOW_MS).map(|past| {
        let percent = past * 100 / (POWER_HOLD_MS - POWER_HOLD_SHOW_MS);
        (percent / 5 * 5) as u8
    });
    if shown != ui.global.power_hold_shown {
        ui.global.power_hold_shown = shown;
        out.dirty = true;
    }
}

/// Power-off teardown: every host-visible hold gets its release, a PTT
/// session is aborted, and the deck host is told `up` as the last line.
pub(super) fn power_down(ui: &mut UiState, out: &mut Update) {
    ui.global.power_hold = None;
    ui.global.power_hold_shown = None;
    release_owned(ui, out);
    end_ptt(ui, true, out);
    ui.global.ptt_pending = None;
    out.push_deck(Event::Up);
    out.dirty = true;
}

/// The screen was on at this tick and the one before: a change of a
/// time-derived pixel since then is a repaint (a wake draws a whole frame).
pub(super) fn lit(ui: &UiState) -> bool {
    ui.global.lit
}

/// HID link down: holds vanish WITHOUT events (their pressed look goes with
/// them: the physical release finds no owner), a PTT session and a touch
/// gesture are dropped, the deck goes stale. Link up cuts nothing: a demo
/// session or a held key goes on (a later `hello` resets the deck).
pub(super) fn on_link(ui: &mut UiState, connected: bool, out: &mut Update) {
    ui.global.pending_focus = 0;
    ui.global.first_tap_ms = None;
    ui.global.shift_clean = false;
    if !connected {
        // A host releases everything on a link drop by itself.
        ui.global.owe_up = false;
        for index in 0..ui.global.held.len() {
            if let Some(owner) = ui.global.held[index].take() {
                owner_released(ui, owner, out);
            }
        }
        out.dirty |= ui.global.ptt.take().is_some();
        out.dirty |= ui.global.ptt_pending.take().is_some();
        out.dirty |= ui.deck.link_lost();
        super::touch::forget(ui, out);
    }
    ui.global.host_seen = ui.deck.host_present;
}

pub(super) fn animating(ui: &UiState) -> bool {
    ui.global.strip_x != target_x(ui) || ui.global.bounce != 0 || session_blocks(ui)
}

// ---- helper API for the other state files ----------------------------------------

/// Record that `source` is down on behalf of `owner`, whose press event the
/// caller has just emitted. Global emits the release.
pub(super) fn hold(ui: &mut UiState, source: Source, owner: Owner) {
    ui.global.held[slot(source)] = Some(owner);
}

pub(super) fn held(ui: &UiState, source: Source) -> Option<Owner> {
    ui.global.held[slot(source)]
}

/// Stop tracking `source` without emitting anything (its owner released
/// itself).
pub(super) fn unhold(ui: &mut UiState, source: Source) -> Option<Owner> {
    ui.global.held[slot(source)].take()
}

/// `source` came up (or its gesture ended): emit its owner's release.
pub(super) fn release_source(ui: &mut UiState, source: Source, out: &mut Update) {
    if let Some(owner) = ui.global.held[slot(source)].take() {
        release(ui, source, owner, out);
    }
}

pub(super) fn anything_held(ui: &UiState) -> bool {
    ui.global.held.iter().any(Option::is_some) || ui.global.ptt.is_some()
}

/// Some source is still down on behalf of `owner`.
pub(super) fn holds(ui: &UiState, owner: Owner) -> bool {
    ui.global.held.contains(&Some(owner))
}

/// Context teardown: emit every owner's release now, swallow the physical
/// releases later, drop the rest of a touch gesture.
pub(super) fn release_owned(ui: &mut UiState, out: &mut Update) {
    super::arcade::suspend(ui, out);
    for index in 0..ui.global.held.len() {
        if let Some(owner) = ui.global.held[index].take() {
            let source = match Button::ALL.get(index) {
                Some(&button) => Source::Button(button),
                None => Source::Touch,
            };
            release(ui, source, owner, out);
        }
    }
    super::touch::orphan(ui, out);
}

/// Jump-cut to the Agents roster (CANCEL at a face's top level, idle-home).
pub(super) fn go_home(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    let home = (0..ui.global.strip_len)
        .find(|&index| ui.face_at(index).is_some_and(|face| face.template == Template::Agents))
        .unwrap_or(0);
    jump_to(ui, home, at_ms, out);
    super::agents::show_roster(ui, at_ms);
}

/// Programmatic jump: teardown, cut (no slide), `face` event.
pub(super) fn jump_to(ui: &mut UiState, index: u8, _at_ms: u32, out: &mut Update) {
    if index >= ui.global.strip_len {
        return;
    }
    release_owned(ui, out);
    let changed = ui.global.current != index;
    ui.global.current = index;
    ui.global.bounce = 0;
    ui.global.strip_x = base_x(ui);
    ui.global.slide_from_x = ui.global.strip_x;
    ui.global.slide_target = ui.global.strip_x;
    out.dirty = true;
    if changed {
        emit_face(ui, out);
    }
}

/// Open or close an overlay, with teardown.
pub(super) fn set_overlay(ui: &mut UiState, overlay: Overlay, _at_ms: u32, out: &mut Update) {
    if ui.global.overlay != overlay {
        release_owned(ui, out);
        ui.global.overlay = overlay;
        out.dirty = true;
    }
}

/// The view or card under the user changed without their doing: confirms
/// are ignored for 500 ms and for presses older than this.
pub(super) fn arm_guard(ui: &mut UiState, at_ms: u32) {
    ui.global.guard_at_ms = at_ms;
}

/// May a confirm whose press/Down happened at `press_ms` act now?
pub(super) fn guard_passed(ui: &UiState, press_ms: u32, now_ms: u32) -> bool {
    elapsed(now_ms, ui.global.guard_at_ms) >= GUARD_MS
        && elapsed(press_ms, ui.global.guard_at_ms) < u32::MAX / 2
}

/// An explicit user action (or a PTT press) made `agent` the active channel.
pub(super) fn emit_channel(ui: &mut UiState, agent: u8, out: &mut Update) {
    if ui.global.active_channel == agent {
        return;
    }
    ui.global.active_channel = agent;
    ui.global.channel_seq = ui.global.channel_seq.wrapping_add(1);
    out.dirty = true;
    out.push_deck(Event::Channel {
        agent,
        seq: ui.global.channel_seq,
    });
}

/// The visible face changed.
pub(super) fn emit_face(ui: &mut UiState, out: &mut Update) {
    out.push_deck(Event::Face {
        id: ui.current_face().id,
    });
}

/// No control for at least `ms`.
pub(super) fn quiet_for(ui: &UiState, now_ms: u32, ms: u32) -> bool {
    elapsed(now_ms, ui.global.last_input_ms) >= ms
}

// ---- private -------------------------------------------------------------------------

fn release(ui: &mut UiState, _source: Source, owner: Owner, out: &mut Update) {
    match owner {
        Owner::HostKey { face, key, shift } => out.push_deck(Event::Key {
            key,
            act: 0,
            shift,
            face,
        }),
        // A key two sources hold releases with the last of them.
        Owner::Macro { .. } if holds(ui, owner) => return,
        Owner::Macro { face, index } => out.push_deck(Event::Macro {
            face,
            index,
            act: 0,
        }),
        Owner::Local | Owner::Arcade(_) => {}
    }
    owner_released(ui, owner, out);
}

// ---- SHIFT ------------------------------------------------------------------------------

fn shift_released(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    let g = &mut ui.global;
    let tap = g.shift_clean && elapsed(at_ms, g.shift_at_ms) < TAP_MS;
    g.shift_clean = false;
    if !tap {
        g.first_tap_ms = None;
        return;
    }
    match g.first_tap_ms.take() {
        Some(first) if elapsed(g.shift_at_ms, first) < TAP_GAP_MS => double_shift(ui, at_ms, out),
        _ => g.first_tap_ms = Some(at_ms),
    }
}

/// Toggle the manager: ignored during PTT; inside, `manager::close` reverts a
/// grab, cancels an add and exits to the selected face.
fn double_shift(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if ui.global.ptt.is_some() {
        return;
    }
    match ui.global.overlay {
        Overlay::None | Overlay::Drawer => super::manager::open(ui, at_ms, out),
        Overlay::Manager | Overlay::Gallery | Overlay::Naming | Overlay::ConfirmRemove => {
            super::manager::close(ui, at_ms, out)
        }
        // A dialog with its own answer, an editor, and a tutorial that never
        // gets here.
        Overlay::ConfirmReset | Overlay::Tutorial | Overlay::MacroEdit => {}
    }
}

// ---- the strip ----------------------------------------------------------------------------

fn shift_wheel(ui: &mut UiState, wheel: Wheel, at_ms: u32, out: &mut Update) {
    // A run ends after 300 ms without a detent; the next one may bounce again.
    if ui
        .global
        .shift_wheel_ms
        .is_none_or(|last| elapsed(at_ms, last) >= WHEEL_TAIL_MS)
    {
        ui.global.bounced = false;
    }
    ui.global.shift_wheel_ms = Some(at_ms);
    if matches!(
        ui.global.overlay,
        Overlay::Manager
            | Overlay::Gallery
            | Overlay::Naming
            | Overlay::ConfirmRemove
            | Overlay::ConfirmReset
            | Overlay::Tutorial
            | Overlay::MacroEdit
    ) || session_blocks(ui)
    {
        return;
    }
    // Even a detent that goes nowhere (a hard end, too soon) shuts the drawer.
    super::notify::close_drawer(ui, at_ms, out);
    if ui
        .global
        .detent_ms
        .is_some_and(|last| elapsed(at_ms, last) < DETENT_MS)
    {
        return;
    }
    ui.global.detent_ms = Some(at_ms);
    let step = super::selection_step(wheel);
    let next = ui.global.current as i32 + step;
    if (0..ui.global.strip_len as i32).contains(&next) {
        slide_to(ui, next as u8, at_ms, out);
    } else if !ui.global.bounced && ui.global.strip_len != 0 {
        ui.global.bounced = true;
        ui.global.bounce = step as i8;
        retarget(ui, at_ms, BOUNCE_MS);
        out.dirty = true;
    }
}

fn slide_to(ui: &mut UiState, index: u8, at_ms: u32, out: &mut Update) {
    release_owned(ui, out);
    if ui.global.overlay == Overlay::Drawer {
        ui.global.overlay = Overlay::None;
    }
    ui.global.current = index;
    ui.global.bounce = 0;
    retarget(ui, at_ms, SLIDE_MS);
    out.dirty = true;
    emit_face(ui, out);
}

/// Start easing from wherever `strip_x` is now toward `target_x`.
fn retarget(ui: &mut UiState, at_ms: u32, ms: u32) {
    ui.global.slide_from_x = ui.global.strip_x;
    ui.global.slide_target = target_x(ui);
    ui.global.slide_at_ms = at_ms;
    ui.global.slide_ms = ms;
}

fn slide(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    let target = target_x(ui);
    if ui.global.strip_x == target {
        if ui.global.bounce != 0 {
            // The overshoot is on screen: ease back to the face.
            ui.global.bounce = 0;
            retarget(ui, now_ms, BOUNCE_MS);
        }
        return;
    }
    if target != ui.global.slide_target {
        // `current` or `strip_x` was set directly (a preview, a test).
        retarget(ui, now_ms, SLIDE_MS);
    }
    let g = &mut ui.global;
    let t = elapsed(now_ms, g.slide_at_ms).min(g.slide_ms.max(1));
    // Smoothstep in 1/256ths: exact at both ends, so the slide settles.
    let k = (t * 256 / g.slide_ms.max(1)) as i32;
    let s = (3 * k * k * 256 - 2 * k * k * k) >> 16;
    let pos = g.slide_from_x + ((target - g.slide_from_x) as i64 * s as i64 / 256) as i32;
    let pos = if t >= g.slide_ms { target } else { pos };
    if pos != g.strip_x {
        g.strip_x = pos;
        out.dirty = true;
    }
    if g.strip_x == target && g.bounce != 0 {
        g.bounce = 0;
        retarget(ui, now_ms, BOUNCE_MS);
    }
}

// ---- PTT ------------------------------------------------------------------------------------

fn ptt_pressed(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if ui.global.ptt.is_some() {
        return;
    }
    let (agent, ctx, card) = super::agents::ptt_target(ui);
    if agent == 0 && ctx != PttCtx::Name {
        super::notify::raise_message(ui, super::notify::ToastKind::NoChannel, 0, 0, at_ms, out);
        return;
    }
    // The LISTENING overlay takes the screen: what was held is released.
    release_owned(ui, out);
    if matches!(ctx, PttCtx::Talk | PttCtx::Answer) {
        emit_channel(ui, agent, out);
    }
    start_session(ui, agent, ctx, card, at_ms, out);
}

fn start_session(ui: &mut UiState, agent: u8, ctx: PttCtx, card: u16, at_ms: u32, out: &mut Update) {
    ui.global.ptt_seq = ui.global.ptt_seq.wrapping_add(1).max(1);
    let press = ui.global.ptt_seq;
    let mut session = PttSession::new(agent, ctx, card, at_ms);
    session.press = press;
    ui.global.ptt = Some(session);
    ui.global.ptt_pending = None;
    ui.deck.transcript.clear();
    ui.deck.transcript_final = false;
    ui.deck.transcript_channel_tip = false;
    out.dirty = true;
    out.push_deck(Event::Ptt {
        act: 1,
        agent,
        ctx,
        card,
        abort: false, press,
    });
}

fn end_ptt(ui: &mut UiState, abort: bool, out: &mut Update) {
    let Some(session) = ui.global.ptt.take() else {
        return;
    };
    out.dirty = true;
    out.push_deck(Event::Ptt {
        act: 0,
        agent: session.agent,
        ctx: session.ctx,
        card: session.card,
        abort, press: session.press,
    });
    if !abort && ui.deck.host_present && !ui.deck.transcript_final {
        let mut pending = session;
        pending.started_ms = ui.now_ms;
        ui.global.ptt_pending = Some(pending);
    }
    let focus = core::mem::take(&mut ui.global.pending_focus);
    if focus != 0 && ui.deck.agent(focus).is_some() {
        let now = ui.now_ms;
        apply_focus(ui, focus, now, out);
    }
}

// ---- idle-home ------------------------------------------------------------------------------

fn idle_home(ui: &mut UiState, now_ms: u32, idle_ms: u32, out: &mut Update) {
    let limit = ui.deck.settings.idle_home_s as u32 * 1000;
    if limit == 0 || idle_ms < limit || super::arcade::playing(ui) {
        ui.global.idle_homed = false;
        return;
    }
    if ui.global.idle_homed {
        return;
    }
    // Dictation, a name being spoken and the tutorial go on.
    if session_blocks(ui) || matches!(ui.global.overlay, Overlay::Naming | Overlay::Tutorial) {
        return;
    }
    ui.global.idle_homed = true;
    let home = ui.current_face().template == Template::Agents
        && ui.global.overlay == Overlay::None
        && ui.agents.view == super::agents::AgentsView::Roster
        && !ui.manager.grabbed
        && ui.strip_at_rest();
    if home {
        return;
    }
    // Silent cancel-all: no `faces`, no `snooze`, then a cut, not a slide.
    super::manager::cancel_all(ui);
    if matches!(ui.global.overlay, Overlay::Drawer | Overlay::ConfirmReset | Overlay::MacroEdit) {
        ui.global.overlay = Overlay::None;
    }
    ui.macropad.cursor = None;
    release_owned(ui, out);
    go_home(ui, now_ms, out);
    arm_guard(ui, now_ms);
    out.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Touch;
    use crate::deck::msg::{AgentState, CardKind, KeyId};
    use crate::deck::text::Text;
    use crate::ui::state::{agents::AgentsView, ToastKind};

    fn fresh() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui
    }

    /// A real host (hello at 0) with the demo roster's shape: agents 1
    /// (Question, card 1) and 2 (Ready, card 2), 3 Working, 4 Idle, 5 Error.
    /// Tests that run past 15 s must `heartbeat`.
    fn hosted() -> std::boxed::Box<UiState> {
        let mut ui = fresh();
        ui.host(&HostMsg::Hello { v: 1, sid: 1, minute_of_day: None }, 0, &mut Update::new());
        crate::deck::demo::load(&mut ui.deck);
        ui.deck.settings.demo = false;
        super::super::agents::on_reset(&mut ui);
        ui.invalidate();
        ui
    }

    fn heartbeat(ui: &mut UiState, at: u32) {
        ui.host(&HostMsg::None, at, &mut Update::new());
    }

    fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
        control(ui, Control::Button(button, Edge::Press), at)
    }
    fn release(ui: &mut UiState, button: Button, at: u32) -> Update {
        control(ui, Control::Button(button, Edge::Release), at)
    }
    fn wheel(ui: &mut UiState, wheel: Wheel, at: u32) -> Update {
        control(ui, Control::Wheel(wheel), at)
    }
    fn control(ui: &mut UiState, control: Control, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(control, at, &mut out);
        out
    }
    fn tick(ui: &mut UiState, at: u32) -> Update {
        let mut out = Update::new();
        ui.tick(at, 0, true, &mut out);
        out
    }
    fn deck_events(out: &Update) -> std::vec::Vec<Event> {
        out.deck_events().collect()
    }
    fn settle(ui: &mut UiState, from: u32) {
        for step in 1..=40 {
            tick(ui, from + step * 33);
        }
        assert!(ui.strip_at_rest() && !ui.animating());
    }

    #[test]
    fn a_new_session_drops_the_active_channel_so_the_first_ptt_announces_it() {
        let mut ui = fresh();
        assert!(ui.deck.demo_active());
        press(&mut ui, Button::PushToTalk, 100);
        release(&mut ui, Button::PushToTalk, 200);
        assert_eq!(ui.global.active_channel, 1);
        let mut out = Update::new();
        ui.host(&HostMsg::Hello { v: 1, sid: 5, minute_of_day: None }, 300, &mut out);
        assert!(out.dirty && ui.global.active_channel == 0);
        let agent = HostMsg::Agent {
            id: 1,
            gone: false,
            kind: None,
            state: Some(AgentState::Working),
            name: None,
            project: None,
            title: None,
            since_s: None,
        };
        ui.host(&agent, 301, &mut Update::new());
        let out = press(&mut ui, Button::PushToTalk, 400);
        assert_eq!(
            deck_events(&out),
            [
                Event::Channel { agent: 1, seq: ui.global.channel_seq() },
                Event::Ptt { act: 1, agent: 1, ctx: PttCtx::Talk, card: 0, abort: false, press: 2, }
            ]
        );
        // Time-derived pixels are reported only while lit; a wake redraws.
        release(&mut ui, Button::PushToTalk, 500);
        ui.tick(600, 0, true, &mut Update::new());
        let mut out = Update::new();
        ui.tick(700, 0, false, &mut out);
        assert!(!out.dirty);
        ui.tick(800, 0, true, &mut out);
        assert!(!out.dirty);
    }

    #[test]
    fn the_waveform_phase_repaints_the_listening_overlay_while_lit() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 1000);
        tick(&mut ui, 1000);
        let phase = crate::ui::overlay::ptt::wave_phase(1000);
        let mut at = 1001;
        while crate::ui::overlay::ptt::wave_phase(at) == phase {
            assert!(!tick(&mut ui, at).dirty, "{at}");
            at += 1;
        }
        assert!(tick(&mut ui, at).dirty);
        let mut out = Update::new();
        ui.tick(at + 200, 0, false, &mut out);
        assert!(!out.dirty, "no screen, no repaint");
        release(&mut ui, Button::PushToTalk, at + 300);
        assert!(ui.global.ptt_pending.is_some());
        ui.host(&HostMsg::Transcript { agent: 1, text: crate::deck::text::Text::from_str("done"), last: true, press: Some(1), failed: false, channel_tip: false }, at + 301, &mut Update::new());
        ui.tick(at + 300, 0, true, &mut Update::new());
        assert!(!tick(&mut ui, at + 600).dirty, "no overlay, no waveform");
    }

    #[test]
    fn three_detents_in_100ms_move_at_most_one_face_and_pos_is_continuous() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        let out = wheel(&mut ui, Wheel::Ccw, 10);
        assert_eq!(deck_events(&out), [Event::Face { id: 2 }]);
        assert_eq!(wheel(&mut ui, Wheel::Ccw, 60).deck_events().count(), 0);
        assert_eq!(wheel(&mut ui, Wheel::Ccw, 110).deck_events().count(), 0);
        assert_eq!(ui.global.current, 1);
        // The position never jumps: every tick moves it toward the target.
        let mut last = ui.global.strip_x;
        for at in (120..400).step_by(20) {
            tick(&mut ui, at);
            assert!(ui.global.strip_x >= last && ui.global.strip_x <= FACE_W * STRIP_ONE);
            last = ui.global.strip_x;
        }
        assert!(ui.strip_at_rest());
        // 150 ms later the next detent counts, retargeting mid-slide.
        let out = wheel(&mut ui, Wheel::Ccw, 400);
        assert_eq!(deck_events(&out), [Event::Face { id: 3 }]);
        tick(&mut ui, 450);
        let mid = ui.global.strip_x;
        assert!(mid > FACE_W * STRIP_ONE && mid < 2 * FACE_W * STRIP_ONE && ui.animating());
        wheel(&mut ui, Wheel::Cw, 560);
        assert_eq!(ui.global.current, 1);
        tick(&mut ui, 570);
        assert!(ui.global.strip_x <= mid, "a retargeted slide turns around from where it is");
        settle(&mut ui, 570);
        assert_eq!(ui.global.strip_x, FACE_W * STRIP_ONE);
    }

    #[test]
    fn held_wheel_at_strip_end_bounces_once() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        // Cw at the left end: one overshoot, then nothing until the run ends.
        wheel(&mut ui, Wheel::Cw, 10);
        assert!(ui.animating());
        tick(&mut ui, 20);
        assert!(!ui.strip_at_rest());
        let mut low = 0;
        for at in (30..500).step_by(10) {
            tick(&mut ui, at);
            low = low.min(ui.global.strip_x);
        }
        assert_eq!(low, -BOUNCE_PX * STRIP_ONE);
        assert!(ui.strip_at_rest() && !ui.animating());
        for at in [500, 590, 680] {
            wheel(&mut ui, Wheel::Cw, at);
            assert!(ui.strip_at_rest(), "at {at}: a held key bounces once per run");
        }
        // 300 ms without a detent ends the run: the next one bounces again.
        wheel(&mut ui, Wheel::Cw, 1000);
        tick(&mut ui, 1010);
        assert!(!ui.strip_at_rest());
        settle(&mut ui, 1010);
        // The right end shows canvas on the right.
        ui.on_face(Template::Settings);
        wheel(&mut ui, Wheel::Ccw, 3000);
        tick(&mut ui, 3060);
        let [_, second] = ui.visible_faces();
        assert_eq!(second.unwrap().face, None);
        assert_eq!(ui.global.current, ui.global.strip_len - 1);
    }

    #[test]
    fn plain_wheel_after_shift_release_is_swallowed_only_where_keys_repeat() {
        let mut ui = fresh();
        ui.on_face(Template::Omarchy);
        press(&mut ui, Button::Shift, 0);
        wheel(&mut ui, Wheel::Ccw, 10);
        release(&mut ui, Button::Shift, 100);
        assert_eq!(ui.current_face().template, Template::Macropad);
        ui.on_face(Template::Omarchy);
        for at in [190, 280] {
            let out = wheel(&mut ui, Wheel::Cw, at);
            // The dev keypad's repeat tail is swallowed; a real detent counts.
            assert_eq!(
                out.deck_events().count(),
                usize::from(WHEEL_SWALLOW_MS == 0),
                "at {at}"
            );
        }
        let out = wheel(&mut ui, Wheel::Cw, 310);
        assert_eq!(out.deck_events().count(), 1);
    }

    #[test]
    fn input_during_slide_goes_to_destination_face() {
        let mut ui = fresh();
        assert_eq!(ui.face_at(1).unwrap().template, Template::Omarchy);
        press(&mut ui, Button::Shift, 0);
        wheel(&mut ui, Wheel::Ccw, 10);
        release(&mut ui, Button::Shift, 20);
        tick(&mut ui, 60);
        assert!(!ui.strip_at_rest());
        let out = press(&mut ui, Button::Enter, 70);
        assert!(
            out.deck_events().any(|event| matches!(event, Event::Key { act: 1, face: 2, .. })),
            "the destination face forwarded ENTER"
        );
        release(&mut ui, Button::Enter, 90);
        settle(&mut ui, 90);
    }

    #[test]
    fn shift_scroll_is_ignored_in_the_manager_family_and_closes_the_drawer_first() {
        for overlay in [Overlay::Manager, Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove] {
            let mut ui = fresh();
            ui.global.overlay = overlay;
            press(&mut ui, Button::Shift, 0);
            let out = wheel(&mut ui, Wheel::Ccw, 10);
            assert_eq!((ui.global.current, out.deck_events().count()), (0, 0), "{overlay:?}");
            assert_eq!(ui.global.overlay, overlay);
        }
        let mut ui = fresh();
        ui.global.overlay = Overlay::Drawer;
        press(&mut ui, Button::Shift, 0);
        wheel(&mut ui, Wheel::Ccw, 10);
        assert_eq!((ui.global.overlay, ui.global.current), (Overlay::None, 1));
        // At a hard end, and inside the detent limit, it still closes.
        ui.global.overlay = Overlay::Drawer;
        wheel(&mut ui, Wheel::Cw, 20);
        assert_eq!((ui.global.overlay, ui.global.current), (Overlay::None, 1));
        let mut ui = fresh();
        ui.global.overlay = Overlay::Drawer;
        press(&mut ui, Button::Shift, 0);
        assert!(wheel(&mut ui, Wheel::Cw, 10).dirty);
        assert_eq!((ui.global.overlay, ui.global.current), (Overlay::None, 0));
    }

    #[test]
    fn two_quick_shift_scroll_flicks_do_not_open_manager() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        wheel(&mut ui, Wheel::Ccw, 80);
        release(&mut ui, Button::Shift, 200);
        press(&mut ui, Button::Shift, 400);
        wheel(&mut ui, Wheel::Ccw, 480);
        release(&mut ui, Button::Shift, 600);
        assert_eq!(ui.global.overlay, Overlay::None);
        assert_eq!(ui.global.current, 2);
    }

    #[test]
    fn both_taps_delivered_in_one_tick_open_manager_and_a_third_tap_starts_over() {
        for base in [0u32, u32::MAX - 1] {
            let mut ui = fresh();
            for _ in 0..2 {
                press(&mut ui, Button::Shift, base);
                release(&mut ui, Button::Shift, base);
            }
            assert_eq!(ui.global.overlay, Overlay::Manager, "base {base}");
            press(&mut ui, Button::Shift, base.wrapping_add(50));
            release(&mut ui, Button::Shift, base.wrapping_add(60));
            assert_eq!(ui.global.overlay, Overlay::Manager);
            press(&mut ui, Button::Shift, base.wrapping_add(300));
            release(&mut ui, Button::Shift, base.wrapping_add(310));
            assert_eq!(ui.global.overlay, Overlay::None);
        }
    }

    #[test]
    fn taps_across_the_clock_wrap_and_at_the_timing_limits() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, u32::MAX - 100);
        release(&mut ui, Button::Shift, (u32::MAX - 100).wrapping_add(299));
        press(&mut ui, Button::Shift, 298);
        release(&mut ui, Button::Shift, 400);
        assert_eq!(ui.global.overlay, Overlay::Manager);
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        release(&mut ui, Button::Shift, 100);
        press(&mut ui, Button::Shift, 500); // The gap is 400: too late.
        release(&mut ui, Button::Shift, 600);
        assert_eq!(ui.global.overlay, Overlay::None);
        press(&mut ui, Button::Shift, 900); // ...but this one pairs with it.
        release(&mut ui, Button::Shift, 950);
        assert_eq!(ui.global.overlay, Overlay::Manager);
    }

    #[test]
    fn long_hold_then_tap_does_not_open_the_manager() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        release(&mut ui, Button::Shift, 300);
        press(&mut ui, Button::Shift, 350);
        release(&mut ui, Button::Shift, 400);
        assert_eq!(ui.global.overlay, Overlay::None);
    }

    #[test]
    fn touch_down_between_taps_resets_but_a_lifting_finger_does_not() {
        let touch = |phase, x, y| {
            Control::Touch(Touch {
                phase,
                x,
                y,
            })
        };
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        release(&mut ui, Button::Shift, 50);
        control(&mut ui, touch(TouchPhase::Down, 240, 240), 100);
        control(&mut ui, touch(TouchPhase::Up, 240, 240), 150);
        press(&mut ui, Button::Shift, 200);
        release(&mut ui, Button::Shift, 250);
        assert_eq!(ui.global.overlay, Overlay::None);
        // A finger that was already down and comes up between taps is fine.
        let mut ui = fresh();
        control(&mut ui, touch(TouchPhase::Down, 240, 240), 0);
        press(&mut ui, Button::Shift, 10);
        release(&mut ui, Button::Shift, 50);
        control(&mut ui, touch(TouchPhase::Up, 240, 240), 100);
        press(&mut ui, Button::Shift, 200);
        release(&mut ui, Button::Shift, 250);
        assert_eq!(ui.global.overlay, Overlay::Manager);
    }

    #[test]
    fn shift_stuck_then_tap_clears_modifier() {
        let mut ui = fresh();
        press(&mut ui, Button::Shift, 0);
        assert!(ui.global.shift);
        // The release was lost; the next press is a fresh one.
        press(&mut ui, Button::Shift, 5000);
        release(&mut ui, Button::Shift, 5050);
        assert!(!ui.global.shift);
        wheel(&mut ui, Wheel::Cw, 6000);
        assert_eq!(ui.global.current, 0, "a plain wheel no longer slides");
    }

    #[test]
    fn double_shift_is_ignored_during_ptt_and_leaves_the_manager_family() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        assert!(ui.global.ptt.is_some());
        press(&mut ui, Button::Shift, 10);
        release(&mut ui, Button::Shift, 20);
        press(&mut ui, Button::Shift, 30);
        release(&mut ui, Button::Shift, 40);
        assert_eq!(ui.global.overlay, Overlay::None);
        release(&mut ui, Button::PushToTalk, 50);
        for overlay in [Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove, Overlay::Drawer] {
            let mut ui = fresh();
            ui.global.overlay = overlay;
            press(&mut ui, Button::Shift, 0);
            release(&mut ui, Button::Shift, 10);
            press(&mut ui, Button::Shift, 20);
            release(&mut ui, Button::Shift, 30);
            let expected = if overlay == Overlay::Drawer { Overlay::Manager } else { Overlay::None };
            assert_eq!(ui.global.overlay, expected, "{overlay:?}");
        }
    }

    fn hold_host_key(ui: &mut UiState, at: u32) {
        ui.on_face(Template::Omarchy);
        press(ui, Button::Enter, at);
        let face = ui.current_face().id;
        hold(
            ui,
            Source::Button(Button::Enter),
            Owner::HostKey {
                face,
                key: KeyId::Enter,
                shift: false,
            },
        );
    }

    #[test]
    fn enter_held_in_omarchy_then_slide_emits_omarchy_release_and_swallows_physical_release() {
        let mut ui = fresh();
        hold_host_key(&mut ui, 0);
        press(&mut ui, Button::Shift, 10);
        let out = wheel(&mut ui, Wheel::Ccw, 20);
        let expected = Event::Key {
            key: KeyId::Enter,
            act: 0,
            shift: false,
            face: 2,
        };
        assert_eq!(deck_events(&out), [expected, Event::Face { id: 3 }]);
        release(&mut ui, Button::Shift, 30);
        let out = release(&mut ui, Button::Enter, 40);
        assert_eq!(out.deck_events().count(), 0);
        assert!(!anything_held(&ui));
    }

    #[test]
    fn macro_release_uses_index_latched_at_press_and_every_teardown_releases() {
        let mut ui = fresh();
        ui.on_face(Template::Macropad);
        press(&mut ui, Button::Left, 0);
        hold(&mut ui, Source::Button(Button::Left), Owner::Macro { face: 3, index: 4 });
        let out = release(&mut ui, Button::Left, 50);
        assert_eq!(deck_events(&out), [Event::Macro { face: 3, index: 4, act: 0 }]);
        // Teardowns: overlay open, jump, idle-home, host lost, new session.
        let expect_release = |ui: &mut UiState, teardown: &dyn Fn(&mut UiState, &mut Update)| {
            press(ui, Button::Right, 100);
            hold(ui, Source::Button(Button::Right), Owner::Macro { face: 3, index: 1 });
            let mut out = Update::new();
            teardown(ui, &mut out);
            assert!(out
                .deck_events()
                .any(|e| e == Event::Macro { face: 3, index: 1, act: 0 }));
            assert_eq!(release(ui, Button::Right, 200).deck_events().count(), 0);
        };
        expect_release(&mut ui, &|ui, out| set_overlay(ui, Overlay::Drawer, 110, out));
        ui.global.overlay = Overlay::None;
        expect_release(&mut ui, &|ui, out| jump_to(ui, 0, 110, out));
        ui.on_face(Template::Macropad);
        expect_release(&mut ui, &|ui, out| ui.host(&HostMsg::Hello { v: 1, sid: 5, minute_of_day: None }, 110, out));
        ui.on_face(Template::Macropad);
        expect_release(&mut ui, &|ui, out| ui.tick(110 + crate::deck::model::HEARTBEAT_MS, 0, true, out));
    }

    #[test]
    fn link_drop_clears_holds_without_emitting_and_marks_the_deck_offline() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 10);
        assert!(ui.global.ptt.is_some());
        hold_host_key(&mut ui, 20);
        let mut out = Update::new();
        ui.set_connected(false, &mut out);
        assert_eq!(out.deck_events().count(), 0);
        assert!(!anything_held(&ui) && ui.global.ptt.is_none());
        assert!(!ui.deck.host_present && !ui.deck.demo_active());
        assert!(ui.deck.agent(1).unwrap().stale);
        assert_eq!(release(&mut ui, Button::PushToTalk, 20).deck_events().count(), 0);
        assert_eq!(release(&mut ui, Button::Enter, 30).deck_events().count(), 0);
    }

    #[test]
    fn link_up_does_not_tear_down_a_demo_ptt_session() {
        let mut ui = fresh();
        assert!(ui.deck.demo_active());
        press(&mut ui, Button::PushToTalk, 100);
        let session = ui.global.ptt;
        assert!(session.is_some());
        let mut out = Update::new();
        ui.set_connected(true, &mut out);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!(ui.global.ptt, session, "the session goes on");
        assert!(ui.global.host_seen == ui.deck.host_present);
        // The physical release still ends it.
        let out = release(&mut ui, Button::PushToTalk, 300);
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }]
        );
        // Nor a held macro key: it stays pressed until its own release.
        let mut ui = fresh();
        ui.on_face(Template::Macropad);
        let out = press(&mut ui, Button::Enter, 10);
        assert_eq!(deck_events(&out), [Event::Macro { face: 3, index: 2, act: 1 }]);
        let mut out = Update::new();
        ui.set_connected(true, &mut out);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!(
            held(&ui, Source::Button(Button::Enter)),
            Some(Owner::Macro { face: 3, index: 2 })
        );
        assert_eq!(ui.macropad.down, 1 << 2);
        let out = release(&mut ui, Button::Enter, 200);
        assert_eq!(deck_events(&out), [Event::Macro { face: 3, index: 2, act: 0 }]);
        assert!(!anything_held(&ui) && ui.macropad.down == 0);
    }

    #[test]
    fn ptt_press_latches_the_target_and_emits_channel_first() {
        let mut ui = hosted();
        ui.agents.highlight = 2;
        let out = press(&mut ui, Button::PushToTalk, 0);
        assert_eq!(
            deck_events(&out),
            [
                Event::Channel { agent: 2, seq: 1 },
                Event::Ptt { act: 1, agent: 2, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }
            ]
        );
        assert!(crate::ui::overlay::ptt_visible(&ui) && ui.animating());
        let out = release(&mut ui, Button::PushToTalk, 500);
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 0, agent: 2, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }]
        );
        // Same target again: no second `channel`.
        let out = press(&mut ui, Button::PushToTalk, 600);
        assert_eq!(out.deck_events().count(), 1);
        release(&mut ui, Button::PushToTalk, 700);
        // A card: ctx answer with the card id.
        super::super::agents::show_card(&mut ui, 1, 800);
        let out = press(&mut ui, Button::PushToTalk, 900);
        assert_eq!(
            deck_events(&out),
            [
                Event::Channel { agent: 1, seq: 2 },
                Event::Ptt { act: 1, agent: 1, ctx: PttCtx::Answer, card: 1, abort: false, press: 3, }
            ]
        );
    }

    fn transcript(ui: &mut UiState, press: u16, text: &str, last: bool, failed: bool, at: u32) {
        ui.host(&HostMsg::Transcript { agent: 1, text: crate::deck::text::Text::from_str(text), last, press: Some(press), failed, channel_tip: false }, at, &mut Update::new());
    }

    #[test]
    fn release_keeps_the_transcript_visible_until_its_final_arrives() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        transcript(&mut ui, 1, "the last words", false, false, 100);
        release(&mut ui, Button::PushToTalk, 200);
        tick(&mut ui, 2200);
        assert!(ui.global.ptt.is_none() && ui.global.ptt_pending.is_some());
        assert!(crate::ui::overlay::ptt_visible(&ui));
        assert_eq!(ui.deck.transcript, "the last words");
        transcript(&mut ui, 1, "the last words are here", true, false, 2300);
        assert!(!crate::ui::overlay::ptt_visible(&ui));
        assert_eq!(ui.deck.transcript, "the last words are here");
    }

    #[test]
    fn a_previous_result_cannot_replace_or_close_a_new_press() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        release(&mut ui, Button::PushToTalk, 100);
        press(&mut ui, Button::PushToTalk, 200);
        transcript(&mut ui, 2, "new words", false, false, 250);
        transcript(&mut ui, 1, "old result", true, false, 300);
        assert!(ui.global.ptt.is_some());
        assert_eq!(ui.deck.transcript, "new words");
        release(&mut ui, Button::PushToTalk, 400);
        transcript(&mut ui, 1, "old failure", true, true, 450);
        assert!(ui.global.ptt_pending.is_some());
        assert_eq!(ui.deck.transcript, "new words");
        transcript(&mut ui, 2, "new complete words", true, false, 500);
        assert!(!crate::ui::overlay::ptt_visible(&ui));
    }

    #[test]
    fn recognition_failure_and_cancel_release_the_wait_screen() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        release(&mut ui, Button::PushToTalk, 100);
        transcript(&mut ui, 1, "failed", true, true, 200);
        assert!(!crate::ui::overlay::ptt_visible(&ui));
        assert_eq!(ui.notify.toast.unwrap().kind, super::super::notify::ToastKind::TranscriptionFailed);
        press(&mut ui, Button::PushToTalk, 300);
        release(&mut ui, Button::PushToTalk, 400);
        assert_eq!(deck_events(&press(&mut ui, Button::Cancel, 500)), [Event::Up]);
        assert!(!crate::ui::overlay::ptt_visible(&ui));
        transcript(&mut ui, 2, "late", true, false, 600);
        assert_ne!(ui.deck.transcript, "late");
    }

    #[test]
    fn recognition_failure_while_held_stops_the_device_owned_microphone() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        let mut out = Update::new();
        ui.host(&HostMsg::Transcript { agent: 1, text: crate::deck::text::Text::from_str("failed"), last: true, press: Some(1), failed: true, channel_tip: false }, 100, &mut out);
        assert_eq!(deck_events(&out), [Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Talk, card: 0, abort: true, press: 1 }]);
        assert!(ui.global.ptt.is_none());
        assert_eq!(release(&mut ui, Button::PushToTalk, 200).deck_events().count(), 0);
    }

    #[test]
    fn a_lost_result_and_a_disconnect_cannot_leave_the_wait_screen_stuck() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        release(&mut ui, Button::PushToTalk, 100);
        for at in (5000..=TRANSCRIBE_CAP_MS).step_by(5000) { heartbeat(&mut ui, at); }
        let out = tick(&mut ui, TRANSCRIBE_CAP_MS + 100);
        assert!(deck_events(&out).contains(&Event::Up));
        assert!(!crate::ui::overlay::ptt_visible(&ui));
        press(&mut ui, Button::PushToTalk, TRANSCRIBE_CAP_MS + 200);
        release(&mut ui, Button::PushToTalk, TRANSCRIBE_CAP_MS + 300);
        ui.set_connected(false, &mut Update::new());
        assert!(!crate::ui::overlay::ptt_visible(&ui));
    }

    #[test]
    fn ptt_release_matches_press_after_every_context_change() {
        let table: [(&str, &dyn Fn(&mut UiState)); 8] = [
            ("shift_ccw", &|ui| {
                press(ui, Button::Shift, 100);
                wheel(ui, Wheel::Ccw, 110);
                release(ui, Button::Shift, 120);
            }),
            ("left", &|ui| {
                press(ui, Button::Left, 100);
                release(ui, Button::Left, 110);
            }),
            ("enter", &|ui| {
                press(ui, Button::Enter, 100);
                release(ui, Button::Enter, 110);
            }),
            ("host_focus", &|ui| {
                ui.host(&HostMsg::Focus { agent: 3, ack: None }, 100, &mut Update::new());
            }),
            ("autojump_card", &|ui| {
                ui.deck.settings.auto_jump = true;
                ui.host(&card_msg(3, 30), 100, &mut Update::new());
            }),
            ("agent_gone", &|ui| {
                ui.host(&gone(1), 100, &mut Update::new());
            }),
            ("card_clear", &|ui| {
                ui.host(&clear_msg(1, 1), 100, &mut Update::new());
            }),
            ("touch", &|ui| {
                let touch = |phase, x, y| Control::Touch(Touch { phase, x, y });
                control(ui, touch(TouchPhase::Down, 240, 240), 100);
                control(ui, touch(TouchPhase::Up, 240, 240), 110);
            }),
        ];
        for (name, disturb) in table {
            let mut ui = hosted();
            super::super::agents::show_card(&mut ui, 1, 0);
            let out = press(&mut ui, Button::PushToTalk, 10);
            let pressed = Event::Ptt { act: 1, agent: 1, ctx: PttCtx::Answer, card: 1, abort: false, press: 1, };
            assert!(deck_events(&out).contains(&pressed), "{name}");
            disturb(&mut ui);
            assert_eq!(ui.global.ptt.map(|s| (s.agent, s.ctx, s.card)), Some((1, PttCtx::Answer, 1)), "{name}");
            assert_eq!(ui.global.current, 0, "{name}: no navigation while live");
            let out = release(&mut ui, Button::PushToTalk, 200);
            let released = Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Answer, card: 1, abort: false, press: 1, };
            assert_eq!(deck_events(&out), [released], "{name}");
            // The view after release is valid for what the host did meanwhile.
            let valid = match ui.agents.view {
                AgentsView::Roster => true,
                AgentsView::Channel => ui.deck.agent(ui.agents.agent).is_some(),
                AgentsView::Card => ui.deck.card(ui.agents.agent).is_some(),
            };
            assert!(valid, "{name}: {:?}", ui.agents.view);
        }
    }

    #[test]
    fn host_focus_during_ptt_applies_after_release() {
        let mut ui = hosted();
        ui.global.active_channel = 1;
        press(&mut ui, Button::PushToTalk, 0);
        ui.host(&HostMsg::Focus { agent: 3, ack: None }, 100, &mut Update::new());
        assert_eq!(ui.global.active_channel, 1);
        release(&mut ui, Button::PushToTalk, 200);
        assert_eq!(ui.global.active_channel, 3);
    }

    #[test]
    fn cancel_aborts_ptt_and_the_release_then_emits_nothing() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        let out = press(&mut ui, Button::Cancel, 100);
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Talk, card: 0, abort: true, press: 1, }]
        );
        assert!(ui.global.ptt.is_none());
        assert_eq!(ui.agents.view, AgentsView::Roster, "CANCEL did not reach the face");
        release(&mut ui, Button::Cancel, 110);
        let out = release(&mut ui, Button::PushToTalk, 200);
        assert_eq!(out.deck_events().count(), 0);
    }

    #[test]
    fn ptt_times_out_after_60s_and_the_release_after_a_disconnect_emits_nothing() {
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        for at in (5000..PTT_CAP_MS).step_by(5000) {
            heartbeat(&mut ui, at);
        }
        assert!(tick(&mut ui, PTT_CAP_MS - 1).deck_events().count() == 0);
        let out = tick(&mut ui, PTT_CAP_MS);
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }]
        );
        assert_eq!(release(&mut ui, Button::PushToTalk, PTT_CAP_MS + 5).deck_events().count(), 0);
        let mut ui = hosted();
        press(&mut ui, Button::PushToTalk, 0);
        ui.set_connected(false, &mut Update::new());
        ui.set_connected(true, &mut Update::new());
        assert_eq!(release(&mut ui, Button::PushToTalk, 50).deck_events().count(), 0);
    }

    #[test]
    fn ptt_without_a_target_shows_the_no_channel_toast_and_emits_nothing() {
        let mut ui = hosted();
        ui.deck.clear_host_data();
        let out = press(&mut ui, Button::PushToTalk, 0);
        assert_eq!(out.deck_events().count(), 0);
        assert!(ui.global.ptt.is_none());
        assert_eq!(ui.notify.toast.map(|t| t.kind), Some(ToastKind::NoChannel));
        // Naming needs no agent: ctx name.
        let mut ui = hosted();
        ui.deck.clear_host_data();
        ui.global.overlay = Overlay::Naming;
        let out = press(&mut ui, Button::PushToTalk, 10);
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 1, agent: 0, ctx: PttCtx::Name, card: 0, abort: false, press: 1, }]
        );
        // In the drawer and the manager PTT talks to the active channel.
        for overlay in [Overlay::Drawer, Overlay::Manager] {
            let mut ui = hosted();
            ui.global.overlay = overlay;
            ui.global.active_channel = 4;
            let out = press(&mut ui, Button::PushToTalk, 10);
            assert_eq!(
                deck_events(&out),
                [Event::Ptt { act: 1, agent: 4, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }],
                "{overlay:?}"
            );
        }
    }

    #[test]
    fn stale_focus_echo_ignored_after_two_quick_channel_switches() {
        let mut ui = hosted();
        ui.agents.highlight = 2;
        // ENTER opens the channel (seq 1); in it, RIGHT walks the siblings.
        let out = press(&mut ui, Button::Enter, 0);
        assert!(deck_events(&out).contains(&Event::Channel { agent: 2, seq: 1 }));
        release(&mut ui, Button::Enter, 5);
        let out = press(&mut ui, Button::Right, 10);
        assert!(deck_events(&out).contains(&Event::Channel { agent: 3, seq: 2 }));
        release(&mut ui, Button::Right, 15);
        press(&mut ui, Button::Right, 20);
        assert_eq!(ui.global.active_channel, 4);
        assert_eq!(ui.global.channel_seq(), 3);
        ui.host(&HostMsg::Focus { agent: 3, ack: Some(2) }, 30, &mut Update::new());
        assert_eq!(ui.global.active_channel, 4, "the late echo of seq 2 is ignored");
        ui.host(&HostMsg::Focus { agent: 4, ack: Some(3) }, 40, &mut Update::new());
        assert_eq!(ui.global.active_channel, 4);
        let out = control(&mut ui, Control::Button(Button::Right, Edge::Release), 50);
        assert_eq!(out.deck_events().count(), 0);
        // An unsolicited PC-side change always applies; an unknown agent never.
        let mut out = Update::new();
        ui.host(&HostMsg::Focus { agent: 2, ack: None }, 60, &mut out);
        assert_eq!(ui.global.active_channel, 2);
        assert_eq!(out.deck_events().count(), 0, "applied state never echoes");
        ui.host(&HostMsg::Focus { agent: 8, ack: None }, 70, &mut Update::new());
        assert_eq!(ui.global.active_channel, 2);
    }

    #[test]
    fn heartbeat_loss_tears_down_holds_and_the_session_and_owes_the_next_hello_an_up() {
        let hello = HostMsg::Hello { v: 1, sid: 1, minute_of_day: None };
        let mut ui = fresh();
        ui.host(&hello, 0, &mut Update::new());
        ui.host(&agent_msg(1, AgentState::Working), 1, &mut Update::new());
        press(&mut ui, Button::PushToTalk, 10);
        let lost = crate::deck::model::HEARTBEAT_MS + 1;
        let out = tick(&mut ui, lost);
        assert!(!ui.deck.host_present && ui.global.ptt.is_none());
        // The runtime no longer sends these: the host is not listening.
        assert_eq!(
            deck_events(&out),
            [Event::Ptt { act: 0, agent: 1, ctx: PttCtx::Talk, card: 0, abort: true, press: 1, }]
        );
        assert!(out.dirty);
        assert!(release(&mut ui, Button::PushToTalk, lost + 10).deck_events().count() == 0);
        // The same daemon comes back on the same link: `up` settles the utterance.
        let mut out = Update::new();
        ui.host(&hello, lost + 20, &mut out);
        assert_eq!(deck_events(&out), [Event::Hello, Event::Up]);
        let mut out = Update::new();
        ui.host(&hello, lost + 30, &mut out);
        assert_eq!(deck_events(&out), [Event::Hello]);
        // A host key held into the gap is owed the same.
        ui.on_face(Template::Omarchy);
        hold(&mut ui, Source::Button(Button::Left), Owner::HostKey { face: 2, key: KeyId::Left, shift: false });
        let at = 2 * lost + 30;
        tick(&mut ui, at);
        let mut out = Update::new();
        ui.host(&hello, at + 10, &mut out);
        assert_eq!(deck_events(&out), [Event::Hello, Event::Up]);
        // Nothing held, or a link drop in between (the host released by
        // itself): no `up`.
        let at = at + lost + 10;
        tick(&mut ui, at);
        let mut out = Update::new();
        ui.host(&hello, at + 10, &mut out);
        assert_eq!(deck_events(&out), [Event::Hello]);
        press(&mut ui, Button::PushToTalk, at + 20);
        assert!(ui.global.ptt.is_some());
        let at = at + lost + 10;
        tick(&mut ui, at);
        ui.set_connected(false, &mut Update::new());
        ui.set_connected(true, &mut Update::new());
        let mut out = Update::new();
        ui.host(&hello, at + 10, &mut out);
        assert_eq!(deck_events(&out), [Event::Hello]);
    }

    fn idle(ui: &mut UiState, at: u32, idle_ms: u32) -> Update {
        let mut out = Update::new();
        ui.tick(at, idle_ms, true, &mut out);
        out
    }

    #[test]
    fn idle_home_cuts_home_silently_and_arms_the_guard() {
        let mut ui = hosted();
        ui.on_face(Template::Usage);
        press(&mut ui, Button::Enter, 0);
        hold(&mut ui, Source::Button(Button::Enter), Owner::Macro { face: 3, index: 2 });
        idle(&mut ui, 500, 0); // The first tick settles the displayed ages.
        let out = idle(&mut ui, 1000, 119_999);
        assert_eq!(ui.current_face().template, Template::Usage);
        assert!(!out.dirty);
        let out = idle(&mut ui, 2000, 120_000);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert!(ui.strip_at_rest());
        assert_eq!(
            deck_events(&out),
            [Event::Macro { face: 3, index: 2, act: 0 }, Event::Face { id: 1 }]
        );
        assert!(!guard_passed(&ui, 2100, 2400) && guard_passed(&ui, 2600, 2600));
        // Still idle: it fires once. Any input re-arms it.
        assert!(!idle(&mut ui, 3000, 121_000).dirty);
        idle(&mut ui, 4000, 0);
        ui.on_face(Template::Settings);
        idle(&mut ui, 5000, 120_000);
        assert_eq!(ui.current_face().template, Template::Agents);
        // Disabled: never.
        ui.deck.settings.idle_home_s = 0;
        ui.on_face(Template::Settings);
        idle(&mut ui, 6000, 500_000);
        assert_eq!(ui.current_face().template, Template::Settings);
    }

    #[test]
    fn idle_home_from_card_emits_no_snooze_and_reverts_a_grab_without_faces() {
        let mut ui = hosted();
        super::super::agents::show_card(&mut ui, 1, 0);
        let out = idle(&mut ui, 1000, 120_000);
        assert_eq!(ui.agents.view, AgentsView::Roster);
        assert!(out.deck_events().all(|e| !matches!(e, Event::Snooze { .. })));
        assert!(!ui.deck.card(1).unwrap().snoozed);
        let mut ui = hosted();
        ui.on_face(Template::Omarchy);
        ui.global.overlay = Overlay::Manager;
        ui.manager.grabbed = true;
        let out = idle(&mut ui, 1000, 120_000);
        assert_eq!((ui.global.overlay, ui.manager.grabbed), (Overlay::None, false));
        assert_eq!(deck_events(&out), [Event::Face { id: 1 }]);
        let mut ui = hosted();
        ui.global.overlay = Overlay::Drawer;
        idle(&mut ui, 1000, 120_000);
        assert_eq!(ui.global.overlay, Overlay::None);
    }

    #[test]
    fn ptt_held_90s_and_naming_do_not_idle_home() {
        let mut ui = hosted();
        ui.on_face(Template::Usage);
        ui.global.active_channel = 2;
        press(&mut ui, Button::PushToTalk, 0);
        for at in (5000..=55_000).step_by(5000) {
            heartbeat(&mut ui, at);
            idle(&mut ui, at + 1, 130_000);
            assert_eq!(ui.current_face().template, Template::Usage, "at {at}");
            assert!(ui.global.ptt.is_some());
        }
        heartbeat(&mut ui, 60_000);
        idle(&mut ui, 60_000, 130_000);
        assert!(ui.global.ptt.is_none(), "the 60 s cap released it");
        assert!(ui.global.ptt_pending.is_some(), "recognition still owns the screen");
        assert_eq!(ui.current_face().template, Template::Usage);
        ui.host(&HostMsg::Transcript { agent: 2, text: crate::deck::text::Text::from_str("done"), last: true, press: Some(1), failed: false, channel_tip: false }, 60_010, &mut Update::new());
        idle(&mut ui, 60_020, 130_000);
        assert_eq!(ui.current_face().template, Template::Agents, "and idle-home was only deferred");
        let mut ui = hosted();
        ui.on_face(Template::Usage);
        ui.global.overlay = Overlay::Naming;
        idle(&mut ui, 1000, 130_000);
        assert_eq!(ui.global.overlay, Overlay::Naming);
    }

    #[test]
    fn the_guard_needs_500ms_and_a_press_after_the_change() {
        let mut ui = fresh();
        arm_guard(&mut ui, 1000);
        assert!(!guard_passed(&ui, 1100, 1499));
        assert!(guard_passed(&ui, 1100, 1500));
        assert!(!guard_passed(&ui, 900, 2000), "pressed before the change");
        arm_guard(&mut ui, u32::MAX - 100);
        assert!(guard_passed(&ui, 50, 400));
    }

    fn card_msg(agent: u8, id: u16) -> HostMsg {
        let mut choices = [Text::new(); 4];
        choices[0].set("yes");
        choices[1].set("no");
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind: CardKind::Question,
            text: Text::from_str("Ship?"),
            choices,
            choice_count: 2,
            recommended: Some(0),
        }
    }
    fn clear_msg(agent: u8, id: u16) -> HostMsg {
        let mut msg = card_msg(agent, id);
        if let HostMsg::Card { clear, .. } = &mut msg {
            *clear = true;
        }
        msg
    }
    fn agent_msg(id: u8, state: AgentState) -> HostMsg {
        HostMsg::Agent {
            id,
            gone: false,
            kind: None,
            state: Some(state),
            name: None,
            project: None,
            title: None,
            since_s: None,
        }
    }
    fn gone(id: u8) -> HostMsg {
        let mut msg = agent_msg(id, AgentState::Idle);
        if let HostMsg::Agent { gone, .. } = &mut msg {
            *gone = true;
        }
        msg
    }

    // ---- power-off hold -----------------------------------------------------------

    #[test]
    fn holding_cancel_shows_the_dialog_then_powers_off_once() {
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 1000);
        let out = tick(&mut ui, 1600);
        assert!(!out.power_off && ui.power_hold().is_none(), "a 600 ms hold shows nothing");
        let out = tick(&mut ui, 1710);
        assert!(out.dirty && ui.power_hold() == Some(0), "the dialog opens at 700 ms");
        tick(&mut ui, 2850);
        assert_eq!(ui.power_hold(), Some(50), "1150 of 2300 ms past the threshold");
        let out = tick(&mut ui, 4000);
        assert!(out.power_off && out.dirty && ui.power_hold().is_none());
        assert!(!tick(&mut ui, 4100).power_off, "one-shot per hold");
    }

    #[test]
    fn releasing_cancel_early_keeps_the_deck_on() {
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 0);
        tick(&mut ui, 1000);
        assert!(ui.power_hold().is_some());
        let out = release(&mut ui, Button::Cancel, 1500);
        assert!(out.dirty && ui.power_hold().is_none(), "the dialog closes");
        assert!(!tick(&mut ui, 4000).power_off);
    }

    #[test]
    fn any_control_but_shift_ends_the_power_hold() {
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 0);
        press(&mut ui, Button::Shift, 500);
        release(&mut ui, Button::Shift, 600);
        assert!(tick(&mut ui, 3100).power_off, "SHIFT edges are not a change of mind");
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 0);
        wheel(&mut ui, Wheel::Cw, 500);
        assert!(ui.power_hold().is_none() && !tick(&mut ui, 3100).power_off);
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 0);
        tick(&mut ui, 1000);
        let out = press(&mut ui, Button::Enter, 1100);
        assert!(out.dirty && ui.power_hold().is_none() && !tick(&mut ui, 3100).power_off);
    }

    #[test]
    fn a_cancel_the_host_owns_does_not_arm_the_power_hold() {
        // On Omarchy SHIFT+CANCEL is forwarded to the host; plain CANCEL is
        // the one that goes home, and that one powers off when held.
        let mut ui = hosted();
        ui.on_face(Template::Omarchy);
        press(&mut ui, Button::Shift, 0);
        press(&mut ui, Button::Cancel, 10);
        release(&mut ui, Button::Shift, 20);
        assert!(!tick(&mut ui, 3100).power_off);
        release(&mut ui, Button::Cancel, 3200);
        press(&mut ui, Button::Cancel, 4100);
        assert!(tick(&mut ui, 7200).power_off);
    }

    #[test]
    fn power_down_releases_every_hold_and_says_up_last() {
        let mut ui = hosted();
        ui.on_face(Template::Omarchy);
        press(&mut ui, Button::Enter, 100);
        let mut out = Update::new();
        ui.power_down(&mut out);
        let events = deck_events(&out);
        assert!(events.iter().any(|event| matches!(event, Event::Key { act: 0, .. })), "{events:?}");
        assert_eq!(events.last(), Some(&Event::Up));
        assert!(release(&mut ui, Button::Enter, 200).deck_events().count() == 0, "the physical release finds nothing");
    }

    #[test]
    fn a_finger_already_down_does_not_end_the_power_hold_when_it_lifts() {
        let touch = |phase, x, y| Control::Touch(Touch { phase, x, y });
        let mut ui = fresh();
        control(&mut ui, touch(TouchPhase::Down, 240, 240), 0);
        press(&mut ui, Button::Cancel, 100);
        tick(&mut ui, 1000);
        assert!(ui.power_hold().is_some());
        control(&mut ui, touch(TouchPhase::Up, 240, 240), 1500);
        assert!(ui.power_hold().is_some(), "an orphaned gesture's tail is not a change of mind");
        assert!(tick(&mut ui, 3200).power_off);
        // A new finger is one.
        let mut ui = fresh();
        press(&mut ui, Button::Cancel, 0);
        tick(&mut ui, 1000);
        control(&mut ui, touch(TouchPhase::Down, 240, 240), 1500);
        assert!(ui.power_hold().is_none() && !tick(&mut ui, 3200).power_off);
    }

    #[test]
    fn a_cancel_that_closes_the_drawer_arms_the_power_hold() {
        let mut ui = hosted();
        ui.on_face(Template::Omarchy);
        ui.global.overlay = Overlay::Drawer;
        press(&mut ui, Button::Cancel, 0);
        assert_eq!(ui.global.overlay, Overlay::None, "the press closed the drawer");
        assert!(tick(&mut ui, 3100).power_off, "the face never saw that CANCEL");
    }
}
