//! Grammar of the Timer face: a stopwatch, countdown and Pomodoro that run on
//! the device alone. No host, no events: all keep running off-screen, under
//! overlays and with the panel dark, because they are only ever a start stamp
//! and what was banked before it.
//!
//! Rules (docs/agent-command-deck.md, "Timer"):
//! - LEFT/RIGHT or a tab switch between Stopwatch, Timer and Pomodoro; the
//!   others keep running.
//! - ENTER (or the right button) is the primary action: Start, Pause, Resume;
//!   on a finished timer, Restart; on a finished Pomodoro phase, Next.
//! - CANCEL (or the left button) is the secondary action: Lap while the
//!   stopwatch runs, else Reset; with nothing to reset it goes home.
//! - SCROLL sets the timer's length while it is idle: 10 s steps up to 1 min,
//!   30 s up to 5 min, 1 min up to 1 h, then 5 min, between `MIN_S` and
//!   `MAX_S`. A preset chip sets it at once.
//! - A timer reaching zero stops at zero, raises a "Timer done" toast that
//!   wakes the screen and jumps here when tapped, and shows "time's up" until
//!   it is restarted or reset.
//! - Pomodoro alternates 25 min of focus and 5 min breaks, with a 15 min
//!   break after four focus sessions. Each completed phase waits for Next.
//! - A finished timer or Pomodoro phase also rings through the deck's speaker
//!   (`Alarm`): four quick beeps a second for the timer, a rising chime when
//!   focus ends and a falling one when a break ends, at the speaker volume.
//!   It rings for up to `RING_MS`, and stops on any touch of the deck (a
//!   press, a detent or a tap, also the one that wakes the screen) and on
//!   Restart, Reset or Next.
//! - Idle-home leaves a running clock on this face alone (`keeps_face`).
use super::{global, notify, Target, TouchAct, UiState, Update};
use crate::control::{Button, Control, Edge, Wheel};
use crate::deck::model::Effects;
use crate::deck::msg::{HostMsg, Template};
use crate::ui::layout;

/// Shortest and longest timer, in seconds.
pub const MIN_S: u32 = 10;
pub const MAX_S: u32 = 5 * 3600;
/// Timer length at power-on.
pub const DEFAULT_S: u32 = 5 * 60;
/// The preset chips, in seconds.
pub const PRESETS_S: [u32; layout::TIMER_PRESETS as usize] = [60, 3 * 60, 5 * 60, 10 * 60, 25 * 60];
/// Laps the stopwatch remembers (and shows), newest first.
pub const LAPS: usize = 3;
/// How long an alarm rings when nobody touches the deck.
pub const RING_MS: u32 = 30_000;

/// A finished clock ringing through the speaker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Alarm {
    pub sound: super::arcade::sound::Effect,
    pub since_ms: u32,
    /// Counts alarms, so the synthesizer starts each one afresh.
    pub serial: u16,
}

/// Touch target ids, = `View::pressed`.
pub const TAB_ID: u16 = 0; // + mode index
pub const SECONDARY_ID: u16 = 4;
pub const PRIMARY_ID: u16 = 5;
pub const PRESET_ID: u16 = 8; // + preset index

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Stopwatch,
    Countdown,
    Pomodoro,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Stopwatch, Mode::Countdown, Mode::Pomodoro];
    pub const fn label(self) -> &'static str {
        match self {
            Mode::Stopwatch => "Stopwatch",
            Mode::Countdown => "Timer",
            Mode::Pomodoro => "Pomodoro",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
}

impl Phase {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::ShortBreak => "Short break",
            Self::LongBreak => "Long break",
        }
    }

    pub const fn duration_ms(self) -> u32 {
        match self {
            Self::Focus => 25 * 60_000,
            Self::ShortBreak => 5 * 60_000,
            Self::LongBreak => 15 * 60_000,
        }
    }
}

/// A clock that runs from `since_ms` on top of what it banked before.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    pub banked_ms: u32,
    /// Running since this stamp; `None` = stopped.
    pub since_ms: Option<u32>,
}

impl Run {
    pub const fn new() -> Self {
        Self { banked_ms: 0, since_ms: None }
    }
    pub const fn running(&self) -> bool {
        self.since_ms.is_some()
    }
    pub fn elapsed(&self, now_ms: u32) -> u32 {
        let live = match self.since_ms {
            // A queued stamp may lie a little ahead of the last tick.
            Some(since) if now_ms.wrapping_sub(since) < u32::MAX / 2 => now_ms.wrapping_sub(since),
            _ => 0,
        };
        self.banked_ms.saturating_add(live)
    }
    fn start(&mut self, now_ms: u32) {
        if self.since_ms.is_none() {
            self.since_ms = Some(now_ms);
        }
    }
    fn pause(&mut self, now_ms: u32) {
        self.banked_ms = self.elapsed(now_ms);
        self.since_ms = None;
    }
}

/// A four-focus Pomodoro cycle. A finished phase stays at zero until Next,
/// so time away from the device never silently consumes the next session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pomodoro {
    pub phase: Phase,
    pub run: Run,
    /// Focus sessions completed in this cycle, from zero through four.
    pub completed: u8,
    pub done: bool,
}

impl Pomodoro {
    pub const fn new() -> Self {
        Self { phase: Phase::Focus, run: Run::new(), completed: 0, done: false }
    }

    pub fn remaining_ms(&self, now_ms: u32) -> u32 {
        self.phase.duration_ms().saturating_sub(self.run.elapsed(now_ms))
    }

    /// A fresh cycle with nothing to reset.
    pub fn idle(&self) -> bool {
        *self == Self::new()
    }

    /// The phase that Next starts after this one finishes.
    pub const fn next_phase(&self) -> Phase {
        match self.phase {
            Phase::Focus if self.completed + !self.done as u8 >= 4 => Phase::LongBreak,
            Phase::Focus => Phase::ShortBreak,
            Phase::ShortBreak | Phase::LongBreak => Phase::Focus,
        }
    }

    fn next(&mut self, now_ms: u32) {
        if !self.done {
            return;
        }
        if self.phase == Phase::LongBreak {
            self.completed = 0;
        }
        self.phase = self.next_phase();
        self.done = false;
        self.run = Run::new();
        self.run.start(now_ms);
    }
}

/// What the two buttons do right now; `None` = shown disabled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Start,
    Pause,
    Resume,
    Restart,
    Lap,
    Reset,
    Next,
}

impl Action {
    pub const fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Pause => "Pause",
            Action::Resume => "Resume",
            Action::Restart => "Restart",
            Action::Lap => "Lap",
            Action::Reset => "Reset",
            Action::Next => "Next",
        }
    }
}

#[derive(Clone, Copy)]
pub struct View {
    pub mode: Mode,
    pub stopwatch: Run,
    /// Split of each lap, newest first; `lap_count` were taken in all.
    pub laps: [u32; LAPS],
    pub lap_count: u16,
    /// Stopwatch time at the last lap.
    pub lap_mark_ms: u32,
    /// The timer's length.
    pub length_ms: u32,
    pub countdown: Run,
    /// The timer ran out and nobody has restarted or reset it.
    pub done: bool,
    pub pomodoro: Pomodoro,
    /// A finished clock ringing, until a touch, an action or `RING_MS`.
    pub alarm: Option<Alarm>,
    alarms: u16,
    /// What a latched touch shows pressed, = the latched `Target::id`.
    pub pressed: Option<u8>,
    /// `shown_step` when the face was last marked dirty.
    pub shown: u32,
}

impl View {
    pub const fn new() -> Self {
        Self {
            mode: Mode::Stopwatch,
            stopwatch: Run::new(),
            laps: [0; LAPS],
            lap_count: 0,
            lap_mark_ms: 0,
            length_ms: DEFAULT_S * 1000,
            countdown: Run::new(),
            done: false,
            pomodoro: Pomodoro::new(),
            alarm: None,
            alarms: 0,
            pressed: None,
            shown: 0,
        }
    }

    fn ring(&mut self, sound: super::arcade::sound::Effect, now_ms: u32) {
        self.alarms = self.alarms.wrapping_add(1);
        self.alarm = Some(Alarm { sound, since_ms: now_ms, serial: self.alarms });
    }

    pub fn stopwatch_ms(&self, now_ms: u32) -> u32 {
        self.stopwatch.elapsed(now_ms)
    }

    /// What the timer has left, counting down to 0.
    pub fn remaining_ms(&self, now_ms: u32) -> u32 {
        self.length_ms.saturating_sub(self.countdown.elapsed(now_ms))
    }

    /// The timer is set and has not started: SCROLL and the presets apply.
    pub fn countdown_idle(&self) -> bool {
        !self.countdown.running() && self.countdown.banked_ms == 0 && !self.done
    }

    /// Any clock is running.
    pub fn running(&self) -> bool {
        self.stopwatch.running() || self.countdown.running() || self.pomodoro.run.running()
    }

    /// ENTER / the right button.
    pub fn primary(&self) -> Action {
        match self.mode {
            Mode::Stopwatch if self.stopwatch.running() => Action::Pause,
            Mode::Stopwatch if self.stopwatch.banked_ms > 0 => Action::Resume,
            Mode::Stopwatch => Action::Start,
            Mode::Countdown if self.done => Action::Restart,
            Mode::Countdown if self.countdown.running() => Action::Pause,
            Mode::Countdown if self.countdown.banked_ms > 0 => Action::Resume,
            Mode::Countdown => Action::Start,
            Mode::Pomodoro if self.pomodoro.done => Action::Next,
            Mode::Pomodoro if self.pomodoro.run.running() => Action::Pause,
            Mode::Pomodoro if self.pomodoro.run.banked_ms > 0 => Action::Resume,
            Mode::Pomodoro => Action::Start,
        }
    }

    /// CANCEL / the left button; `None` = nothing to do (CANCEL goes home).
    pub fn secondary(&self) -> Option<Action> {
        match self.mode {
            Mode::Stopwatch if self.stopwatch.running() => Some(Action::Lap),
            Mode::Stopwatch => (self.stopwatch.banked_ms > 0).then_some(Action::Reset),
            Mode::Countdown => (!self.countdown_idle()).then_some(Action::Reset),
            Mode::Pomodoro => (!self.pomodoro.idle()).then_some(Action::Reset),
        }
    }

    /// The left button's label, also when it is disabled.
    pub fn secondary_label(&self) -> &'static str {
        match (self.secondary(), self.mode) {
            (Some(action), _) => action.label(),
            (None, Mode::Stopwatch) => Action::Lap.label(),
            (None, Mode::Countdown | Mode::Pomodoro) => Action::Reset.label(),
        }
    }

    /// The displayed value of the current clock, in its own unit: hundredths
    /// for the stopwatch, whole seconds (rounded up) for the timer.
    pub fn shown_step(&self, now_ms: u32) -> u32 {
        match self.mode {
            Mode::Stopwatch => self.stopwatch_ms(now_ms) / 10,
            Mode::Countdown => self.remaining_ms(now_ms).div_ceil(1000),
            Mode::Pomodoro => self.pomodoro.remaining_ms(now_ms).div_ceil(1000),
        }
    }
}

/// The timer length one detent away, in seconds; hard ends.
pub fn stepped_s(seconds: u32, up: bool) -> u32 {
    let step = |s: u32| match s {
        0..60 => 10,
        60..300 => 30,
        300..3600 => 60,
        _ => 300,
    };
    let next = if up {
        seconds + step(seconds)
    } else {
        // Down by the step of the band below, so up then down is a no-op.
        seconds.saturating_sub(step(seconds.saturating_sub(1)))
    };
    next.clamp(MIN_S, MAX_S)
}

/// A Timer face is (partly) on the panel.
pub fn visible(ui: &UiState) -> bool {
    ui.visible_faces()
        .iter()
        .flatten()
        .any(|visible| visible.face.is_some_and(|face| face.template == Template::Timer))
}

/// Idle-home stays put: a running clock on the current Timer face.
pub(super) fn keeps_face(ui: &UiState) -> bool {
    ui.current_face().template == Template::Timer && ui.timer.running()
}

fn act(ui: &mut UiState, action: Action, at_ms: u32, out: &mut Update) {
    // An input can reach us before the tick at the same deadline. Finalize
    // before pausing, otherwise a stopped run at zero could never finish.
    if ui.timer.mode == Mode::Pomodoro && action == Action::Pause && finish_pomodoro(ui, at_ms, out) {
        return;
    }
    let view = &mut ui.timer;
    if matches!(action, Action::Restart | Action::Reset | Action::Next) && view.mode != Mode::Stopwatch {
        view.alarm = None;
    }
    match (view.mode, action) {
        (Mode::Stopwatch, Action::Start | Action::Resume) => view.stopwatch.start(at_ms),
        (Mode::Stopwatch, Action::Pause) => view.stopwatch.pause(at_ms),
        (Mode::Stopwatch, Action::Lap) => {
            let now = view.stopwatch.elapsed(at_ms);
            view.laps.copy_within(0..LAPS - 1, 1);
            view.laps[0] = now.saturating_sub(view.lap_mark_ms);
            view.lap_mark_ms = now;
            view.lap_count = view.lap_count.saturating_add(1);
        }
        (Mode::Stopwatch, Action::Reset) => {
            view.stopwatch = Run::new();
            view.laps = [0; LAPS];
            view.lap_count = 0;
            view.lap_mark_ms = 0;
        }
        (Mode::Countdown, Action::Start | Action::Resume) => view.countdown.start(at_ms),
        (Mode::Countdown, Action::Pause) => view.countdown.pause(at_ms),
        (Mode::Countdown, Action::Restart) => {
            view.done = false;
            view.countdown = Run::new();
            view.countdown.start(at_ms);
        }
        (Mode::Countdown, Action::Reset) => {
            view.done = false;
            view.countdown = Run::new();
        }
        (Mode::Pomodoro, Action::Start | Action::Resume) => view.pomodoro.run.start(at_ms),
        (Mode::Pomodoro, Action::Pause) => view.pomodoro.run.pause(at_ms),
        (Mode::Pomodoro, Action::Next) => view.pomodoro.next(at_ms),
        (Mode::Pomodoro, Action::Reset) => view.pomodoro = Pomodoro::new(),
        _ => return,
    }
    out.dirty = true;
}

fn select_mode(ui: &mut UiState, mode: Mode, out: &mut Update) {
    if ui.timer.mode != mode {
        ui.timer.mode = mode;
        out.dirty = true;
    }
}

fn set_length(ui: &mut UiState, seconds: u32, out: &mut Update) {
    let ms = seconds.clamp(MIN_S, MAX_S) * 1000;
    if ui.timer.countdown_idle() && ui.timer.length_ms != ms {
        ui.timer.length_ms = ms;
        out.dirty = true;
    }
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
    match control {
        Control::Wheel(wheel) if ui.timer.mode == Mode::Countdown => {
            let seconds = stepped_s(ui.timer.length_ms / 1000, wheel == Wheel::Cw);
            set_length(ui, seconds, out);
        }
        Control::Button(Button::Enter, Edge::Press) => {
            let action = ui.timer.primary();
            act(ui, action, at_ms, out);
        }
        Control::Button(Button::Cancel, Edge::Press) => match ui.timer.secondary() {
            Some(action) => act(ui, action, at_ms, out),
            None => global::go_home(ui, at_ms, out),
        },
        Control::Button(Button::Left, Edge::Press) => {
            let index = (ui.timer.mode as usize).saturating_sub(1);
            select_mode(ui, Mode::ALL[index], out);
        }
        Control::Button(Button::Right, Edge::Press) => {
            let index = (ui.timer.mode as usize + 1).min(Mode::ALL.len() - 1);
            select_mode(ui, Mode::ALL[index], out);
        }
        _ => {}
    }
    true
}

/// The touch target under (x, y), if any. Pure. Ids as `View::pressed`.
pub(super) fn hit(ui: &UiState, _instance: u8, x: i32, y: i32) -> Option<Target> {
    if let Some(tab) = layout::timer_tab_at(x, y) {
        return Some(Target::tap(layout::timer_tab(tab), TAB_ID + tab as u16));
    }
    if layout::TIMER_PRIMARY.contains(x, y) {
        return Some(Target::tap(layout::TIMER_PRIMARY, PRIMARY_ID));
    }
    if layout::TIMER_SECONDARY.contains(x, y) {
        return ui.timer.secondary().map(|_| Target::tap(layout::TIMER_SECONDARY, SECONDARY_ID));
    }
    let preset = layout::timer_preset_at(x, y)?;
    (ui.timer.mode == Mode::Countdown && ui.timer.countdown_idle())
        .then(|| Target::tap(layout::timer_preset(preset), PRESET_ID + preset as u16))
}

/// One act of a touch that latched a target of this face.
pub(super) fn on_touch(
    ui: &mut UiState,
    _instance: u8,
    target: Target,
    touch: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    let id = target.id;
    match touch {
        TouchAct::Down => ui.timer.pressed = Some(id.min(255) as u8),
        TouchAct::Activate => {
            ui.timer.pressed = None;
            match id {
                PRIMARY_ID => {
                    let action = ui.timer.primary();
                    act(ui, action, at_ms, out);
                }
                SECONDARY_ID => {
                    if let Some(action) = ui.timer.secondary() {
                        act(ui, action, at_ms, out);
                    }
                }
                _ if id >= PRESET_ID => {
                    if let Some(&seconds) = PRESETS_S.get((id - PRESET_ID) as usize) {
                        set_length(ui, seconds, out);
                    }
                }
                _ => {
                    if let Some(&mode) = Mode::ALL.get((id - TAB_ID) as usize) {
                        select_mode(ui, mode, out);
                    }
                }
            }
        }
        TouchAct::Cancel | TouchAct::Swipe { .. } => ui.timer.pressed = None,
        _ => return,
    }
    out.dirty = true;
}

/// Nothing from the host concerns the clocks.
pub(super) fn on_host(
    _ui: &mut UiState,
    _msg: &HostMsg,
    _fx: &Effects,
    _at_ms: u32,
    _out: &mut Update,
) {
}

/// The timer runs out here, wherever the strip is; the readout repaints when
/// what it shows changes, and only while a Timer face is on the panel.
pub(super) fn on_tick(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    let view = &mut ui.timer;
    if view.countdown.running() && view.countdown.elapsed(now_ms) >= view.length_ms {
        view.countdown = Run { banked_ms: view.length_ms, since_ms: None };
        view.done = true;
        view.ring(super::arcade::sound::Effect::Alarm, now_ms);
        out.dirty = true;
        let face = (0..ui.global.strip_len)
            .filter_map(|index| ui.face_at(index))
            .find(|face| face.template == Template::Timer)
            .map_or(0, |face| face.id);
        notify::raise_timer(ui, face, now_ms, out);
    }
    finish_pomodoro(ui, now_ms, out);
    if ui.timer.alarm.is_some_and(|alarm| now_ms.wrapping_sub(alarm.since_ms) >= RING_MS) {
        ui.timer.alarm = None;
    }
    let step = ui.timer.shown_step(now_ms);
    if step != ui.timer.shown {
        ui.timer.shown = step;
        out.dirty |= visible(ui);
    }
}

/// Finish only once, including when a pause arrives before the next tick.
fn finish_pomodoro(ui: &mut UiState, now_ms: u32, out: &mut Update) -> bool {
    let pomodoro = &mut ui.timer.pomodoro;
    if pomodoro.run.running() && pomodoro.run.elapsed(now_ms) >= pomodoro.phase.duration_ms() {
        pomodoro.run = Run { banked_ms: pomodoro.phase.duration_ms(), since_ms: None };
        pomodoro.done = true;
        let focus = pomodoro.phase == Phase::Focus;
        if focus {
            pomodoro.completed = (pomodoro.completed + 1).min(4);
        }
        let sound = if focus {
            super::arcade::sound::Effect::FocusDone
        } else {
            super::arcade::sound::Effect::BreakDone
        };
        ui.timer.ring(sound, now_ms);
        out.dirty = true;
        let face = (0..ui.global.strip_len)
            .filter_map(|index| ui.face_at(index))
            .find(|face| face.template == Template::Timer)
            .map_or(0, |face| face.id);
        notify::raise_pomodoro(ui, face, focus, now_ms, out);
        return true;
    }
    false
}

/// Any touch of the deck silences a ringing alarm: a press, a detent or a
/// tap, including the one that only wakes the screen (the caller asks
/// before deciding whether the control is delivered). `true` = it rang.
pub fn hush(ui: &mut UiState, control: Control) -> bool {
    let touched = match control {
        Control::Button(_, Edge::Press) | Control::Wheel(_) => true,
        Control::Touch(touch) => touch.phase == crate::control::TouchPhase::Down,
        _ => false,
    };
    touched && ui.timer.alarm.take().is_some()
}

/// The speaker command of a ringing alarm, at the speaker volume.
pub fn alarm_command(ui: &UiState) -> Option<u32> {
    let alarm = ui.timer.alarm?;
    Some(super::arcade::sound::command(0, 1, alarm.sound, alarm.serial, ui.deck.settings.speaker))
}

/// A running clock on the panel repaints at the animation rate.
pub(super) fn animating(ui: &UiState) -> bool {
    ui.timer.running() && visible(ui)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_timer() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        // Before Settings, with the id the gallery would give it.
        let len = ui.global.strip_len as usize;
        ui.global.strip[len] = ui.global.strip[len - 1];
        ui.global.strip[len - 1] = crate::deck::msg::FaceEntry::new(Template::Timer.default_id(), Template::Timer, "Timer");
        ui.global.strip_len += 1;
        ui.on_face(Template::Timer);
        ui
    }

    fn press(ui: &mut UiState, button: Button, at_ms: u32) -> Update {
        let mut out = Update::new();
        ui.apply(Control::Button(button, Edge::Press), at_ms, &mut out);
        ui.apply(Control::Button(button, Edge::Release), at_ms + 1, &mut Update::new());
        out
    }

    fn tick(ui: &mut UiState, now_ms: u32) -> Update {
        let mut out = Update::new();
        ui.tick(now_ms, 0, true, &mut out);
        out
    }

    fn sound(ui: &UiState) -> Option<crate::ui::state::arcade::sound::Effect> {
        ui.timer.alarm.map(|alarm| alarm.sound)
    }

    #[test]
    fn a_finished_timer_rings_until_touched_or_for_thirty_seconds() {
        use crate::ui::state::arcade::sound::{self as synth, Effect};
        let mut ui = on_timer();
        press(&mut ui, Button::Right, 0);
        set_length(&mut ui, MIN_S, &mut Update::new());
        press(&mut ui, Button::Enter, 1000);
        assert_eq!((sound(&ui), crate::ui::state::sound_command(&ui)), (None, 0), "quiet while it runs");
        tick(&mut ui, 11_000);
        assert_eq!(sound(&ui), Some(Effect::Alarm));
        let word = crate::ui::state::sound_command(&ui);
        assert!(synth::enabled(word), "at the speaker volume: {word:#x}");
        assert_eq!((word >> 24) & 15, u32::from(ui.deck.settings.speaker));
        // Nobody comes: it stops by itself.
        tick(&mut ui, 11_000 + RING_MS - 1);
        assert!(ui.timer.alarm.is_some());
        tick(&mut ui, 11_000 + RING_MS);
        assert_eq!((sound(&ui), crate::ui::state::sound_command(&ui)), (None, 0));
        // Restarted and finished again: a new alarm, ended by any detent.
        press(&mut ui, Button::Enter, 50_000);
        tick(&mut ui, 60_000);
        let first = ui.timer.alarm.expect("rings again");
        assert!(hush(&mut ui, Control::Wheel(Wheel::Cw)));
        assert_eq!(ui.timer.alarm, None);
        assert!(!hush(&mut ui, Control::Wheel(Wheel::Cw)), "nothing left to hush");
        // A release or a drag is not a touch of the deck; a tap is.
        press(&mut ui, Button::Enter, 70_000);
        tick(&mut ui, 80_000);
        assert_ne!(ui.timer.alarm.map(|a| a.serial), Some(first.serial), "each alarm starts afresh");
        assert!(!hush(&mut ui, Control::Button(Button::Enter, Edge::Release)));
        let at = |phase| Control::Touch(crate::control::Touch { phase, x: 240, y: 240 });
        assert!(!hush(&mut ui, at(crate::control::TouchPhase::Move)));
        assert!(hush(&mut ui, at(crate::control::TouchPhase::Down)));
    }

    #[test]
    fn restart_and_reset_stop_the_alarm_and_the_stopwatch_never_rings() {
        let mut ui = on_timer();
        press(&mut ui, Button::Enter, 0);
        tick(&mut ui, 5 * 3_600_000);
        assert_eq!(ui.timer.alarm, None, "a stopwatch has no end");
        press(&mut ui, Button::Right, 10);
        set_length(&mut ui, MIN_S, &mut Update::new());
        press(&mut ui, Button::Enter, 1000);
        tick(&mut ui, 11_000);
        assert!(ui.timer.alarm.is_some());
        // Restart (here without a press, as a tap on the button acts)
        // ends the alarm as well.
        act(&mut ui, Action::Restart, 12_000, &mut Update::new());
        assert_eq!(ui.timer.alarm, None);
    }

    #[test]
    fn pomodoro_chimes_rise_after_focus_and_fall_after_a_break() {
        use crate::ui::state::arcade::sound::Effect;
        let mut ui = on_timer();
        press(&mut ui, Button::Right, 0);
        press(&mut ui, Button::Right, 1);
        assert_eq!(ui.timer.mode, Mode::Pomodoro);
        press(&mut ui, Button::Enter, 1000);
        tick(&mut ui, 1000 + Phase::Focus.duration_ms());
        assert_eq!(sound(&ui), Some(Effect::FocusDone));
        // Next both silences and starts the break.
        press(&mut ui, Button::Enter, 2_000_000);
        assert_eq!(sound(&ui), None);
        tick(&mut ui, 2_000_000 + Phase::ShortBreak.duration_ms());
        assert_eq!(sound(&ui), Some(Effect::BreakDone));
    }

    #[test]
    fn length_steps_have_hard_ends_and_undo_each_other() {
        assert_eq!(stepped_s(MIN_S, false), MIN_S);
        assert_eq!(stepped_s(MAX_S, true), MAX_S);
        assert_eq!(stepped_s(50, true), 60);
        assert_eq!(stepped_s(60, true), 90);
        assert_eq!(stepped_s(300, true), 360);
        assert_eq!(stepped_s(3600, true), 3900);
        for seconds in [10, 60, 90, 300, 360, 3600, 3900] {
            assert_eq!(stepped_s(stepped_s(seconds, true), false), seconds, "{seconds}");
        }
    }

    #[test]
    fn the_stopwatch_starts_laps_pauses_resumes_and_resets() {
        let mut ui = on_timer();
        assert_eq!(ui.timer.primary(), Action::Start);
        assert_eq!(ui.timer.secondary(), None);
        assert!(press(&mut ui, Button::Enter, 1000).dirty);
        assert!(ui.timer.stopwatch.running() && animating(&ui));
        assert_eq!(ui.timer.secondary(), Some(Action::Lap));
        press(&mut ui, Button::Cancel, 2500);
        press(&mut ui, Button::Cancel, 3000);
        assert_eq!((ui.timer.lap_count, ui.timer.laps[0], ui.timer.laps[1]), (2, 500, 1500));
        press(&mut ui, Button::Enter, 4000);
        assert_eq!(ui.timer.stopwatch_ms(9000), 3000);
        assert_eq!((ui.timer.primary(), ui.timer.secondary()), (Action::Resume, Some(Action::Reset)));
        press(&mut ui, Button::Enter, 10_000);
        assert_eq!(ui.timer.stopwatch_ms(10_250), 3250);
        press(&mut ui, Button::Enter, 10_250);
        press(&mut ui, Button::Cancel, 11_000);
        assert_eq!((ui.timer.stopwatch, ui.timer.lap_count), (Run::new(), 0));
        // With nothing to reset, CANCEL goes home.
        press(&mut ui, Button::Cancel, 12_000);
        assert_eq!(ui.current_face().template, Template::Agents);
    }

    #[test]
    fn the_readout_repaints_once_per_hundredth_and_only_when_visible() {
        let mut ui = on_timer();
        press(&mut ui, Button::Enter, 1000);
        assert!(tick(&mut ui, 1010).dirty);
        assert!(!tick(&mut ui, 1015).dirty);
        assert!(tick(&mut ui, 1020).dirty);
        ui.on_face(Template::Usage);
        assert!(!tick(&mut ui, 1500).dirty && !animating(&ui));
        assert!(ui.timer.stopwatch.running());
    }

    #[test]
    fn the_timer_counts_down_stops_at_zero_and_raises_a_waking_toast() {
        let mut ui = on_timer();
        press(&mut ui, Button::Right, 0);
        assert_eq!(ui.timer.mode, Mode::Countdown);
        // SCROLL sets the length while idle.
        let mut out = Update::new();
        ui.apply(Control::Wheel(Wheel::Ccw), 10, &mut out);
        assert_eq!(ui.timer.length_ms, 270_000);
        for _ in 0..30 {
            ui.apply(Control::Wheel(Wheel::Ccw), 10, &mut out);
        }
        assert_eq!(ui.timer.length_ms, MIN_S * 1000);
        press(&mut ui, Button::Enter, 1000);
        ui.apply(Control::Wheel(Wheel::Cw), 1100, &mut out);
        assert_eq!(ui.timer.length_ms, MIN_S * 1000, "no SCROLL while running");
        assert_eq!(ui.timer.shown_step(1001), 10);
        assert_eq!(ui.timer.shown_step(1999), 10);
        assert_eq!(ui.timer.shown_step(2001), 9);
        // It runs out off-screen too.
        ui.on_face(Template::Usage);
        assert!(!tick(&mut ui, 10_999).wake);
        let out = tick(&mut ui, 11_000);
        assert!(out.wake && ui.timer.done && !ui.timer.running());
        assert_eq!(ui.notify.toast.map(|t| t.kind), Some(notify::ToastKind::TimerDone));
        assert_eq!(ui.timer.remaining_ms(20_000), 0);
        // Tapping the toast jumps back here.
        let mut out = Update::new();
        notify::toast_tapped(&mut ui, 11_100, &mut out);
        assert_eq!(ui.current_face().template, Template::Timer);
        assert_eq!(ui.timer.primary(), Action::Restart);
        press(&mut ui, Button::Enter, 12_000);
        assert!(!ui.timer.done && ui.timer.countdown.running());
        press(&mut ui, Button::Cancel, 13_000);
        assert!(ui.timer.countdown_idle() && ui.timer.secondary().is_none());
    }

    #[test]
    fn touch_targets_follow_what_is_enabled() {
        let mut ui = on_timer();
        let (x, y) = layout::TIMER_SECONDARY.center();
        assert_eq!(hit(&ui, 7, x, y), None, "nothing to reset");
        let (x, y) = layout::timer_preset(0).center();
        assert_eq!(hit(&ui, 7, x, y), None, "no presets on the stopwatch");
        ui.timer.mode = Mode::Countdown;
        let target = hit(&ui, 7, x, y).unwrap();
        on_touch(&mut ui, 7, target, TouchAct::Activate, 0, &mut Update::new());
        assert_eq!(ui.timer.length_ms, 60_000);
        let (x, y) = layout::timer_tab(0).center();
        let target = hit(&ui, 7, x, y).unwrap();
        on_touch(&mut ui, 7, target, TouchAct::Activate, 0, &mut Update::new());
        assert_eq!(ui.timer.mode, Mode::Stopwatch);
    }

    #[test]
    fn pomodoro_completes_four_focus_sessions_then_a_long_break() {
        let mut ui = on_timer();
        ui.timer.mode = Mode::Pomodoro;
        let mut now = 1000u32;
        assert_eq!(ui.timer.primary(), Action::Start);
        assert_eq!(ui.timer.secondary(), None);
        press(&mut ui, Button::Enter, now);
        for completed in 1..=4 {
            assert_eq!(ui.timer.pomodoro.phase, Phase::Focus);
            assert_eq!(ui.timer.pomodoro.next_phase(), if completed == 4 { Phase::LongBreak } else { Phase::ShortBreak });
            now += Phase::Focus.duration_ms();
            assert!(tick(&mut ui, now).wake);
            assert!(ui.timer.pomodoro.done && !ui.timer.pomodoro.run.running());
            assert_eq!(ui.timer.pomodoro.remaining_ms(now), 0);
            assert_eq!(ui.timer.pomodoro.completed, completed);
            assert_eq!(ui.timer.primary(), Action::Next);
            assert_eq!(ui.notify.toast.unwrap().kind, notify::ToastKind::PomodoroFocusDone);
            // A delayed response does not consume the break or count another focus.
            now += 10 * 60_000;
            tick(&mut ui, now);
            assert_eq!(ui.timer.pomodoro.completed, completed);
            assert_eq!(ui.timer.pomodoro.phase, Phase::Focus);
            now += 1;
            press(&mut ui, Button::Enter, now);
            let phase = if completed == 4 { Phase::LongBreak } else { Phase::ShortBreak };
            assert_eq!(ui.timer.pomodoro.phase, phase);
            assert_eq!(ui.timer.pomodoro.completed, completed);
            assert!(ui.timer.pomodoro.run.running() && !ui.timer.pomodoro.done);
            now += phase.duration_ms();
            assert!(tick(&mut ui, now).wake);
            assert_eq!(ui.notify.toast.unwrap().kind, notify::ToastKind::PomodoroBreakDone);
            now += 1;
            press(&mut ui, Button::Enter, now);
        }
        assert_eq!(ui.timer.pomodoro.phase, Phase::Focus);
        assert_eq!(ui.timer.pomodoro.completed, 0);
        assert_eq!(ui.timer.pomodoro.remaining_ms(now), Phase::Focus.duration_ms());
        assert!(ui.timer.pomodoro.run.running());
    }

    #[test]
    fn pomodoro_pauses_resumes_and_resets_without_changing_other_timers() {
        let mut ui = on_timer();
        press(&mut ui, Button::Enter, 1000);
        press(&mut ui, Button::Right, 1010);
        press(&mut ui, Button::Enter, 1020);
        press(&mut ui, Button::Right, 1030);
        assert_eq!(ui.timer.mode, Mode::Pomodoro);
        press(&mut ui, Button::Enter, 2000);
        press(&mut ui, Button::Enter, 4000);
        assert_eq!(ui.timer.primary(), Action::Resume);
        assert_eq!(ui.timer.pomodoro.remaining_ms(100_000), Phase::Focus.duration_ms() - 2000);
        assert_eq!(ui.timer.secondary(), Some(Action::Reset));
        press(&mut ui, Button::Enter, 100_000);
        assert_eq!(ui.timer.pomodoro.remaining_ms(101_000), Phase::Focus.duration_ms() - 3000);
        press(&mut ui, Button::Cancel, 101_000);
        assert_eq!(ui.timer.pomodoro, Pomodoro::new());
        assert_eq!(ui.timer.stopwatch_ms(101_000), 100_000);
        assert_eq!(ui.timer.remaining_ms(101_000), DEFAULT_S * 1000 - 99_980);
        assert!(ui.timer.stopwatch.running() && ui.timer.countdown.running());
    }

    #[test]
    fn pomodoro_finishes_offscreen_in_the_dark_and_its_toast_opens_the_right_tab() {
        let mut ui = on_timer();
        ui.timer.mode = Mode::Pomodoro;
        press(&mut ui, Button::Enter, 1000);
        ui.timer.mode = Mode::Stopwatch;
        ui.on_face(Template::Usage);
        let mut out = Update::new();
        let done = 1000 + Phase::Focus.duration_ms();
        ui.tick(done, 0, false, &mut out);
        assert!(out.wake && ui.timer.pomodoro.done);
        assert!(!ui.timer.running());
        assert_eq!(ui.notify.toast.unwrap().kind, notify::ToastKind::PomodoroFocusDone);
        // The full toast duration starts when the waking frame reaches the glass.
        notify::on_frame_shown(&mut ui, done + 500);
        let toast = ui.notify.toast.unwrap();
        assert_eq!(toast.until_ms.wrapping_sub(toast.shown_ms), notify::TOAST_MS);
        notify::toast_tapped(&mut ui, done + 600, &mut Update::new());
        assert_eq!(ui.current_face().template, Template::Timer);
        assert_eq!(ui.timer.mode, Mode::Pomodoro);
        assert_eq!(ui.timer.primary(), Action::Next);
        assert!(!ui.timer.pomodoro.run.running(), "opening the toast waits for an explicit Next");
    }

    #[test]
    fn countdown_toast_returns_to_countdown_when_pomodoro_is_selected() {
        let mut ui = on_timer();
        ui.timer.mode = Mode::Countdown;
        ui.timer.length_ms = MIN_S * 1000;
        press(&mut ui, Button::Enter, 1000);
        ui.timer.mode = Mode::Pomodoro;
        tick(&mut ui, 11_000);
        notify::toast_tapped(&mut ui, 11_100, &mut Update::new());
        assert_eq!(ui.timer.mode, Mode::Countdown);
        assert!(ui.timer.done);
    }

    #[test]
    fn pomodoro_survives_the_millisecond_clock_wrap() {
        let mut ui = on_timer();
        ui.timer.mode = Mode::Pomodoro;
        let start = u32::MAX - 600_000;
        press(&mut ui, Button::Enter, start);
        let halfway = start.wrapping_add(Phase::Focus.duration_ms() / 2);
        tick(&mut ui, halfway);
        assert_eq!(ui.timer.pomodoro.remaining_ms(halfway), Phase::Focus.duration_ms() / 2);
        let done = start.wrapping_add(Phase::Focus.duration_ms());
        assert!(tick(&mut ui, done).wake);
        assert_eq!(ui.timer.pomodoro.completed, 1);
        assert_eq!(ui.timer.primary(), Action::Next);
        press(&mut ui, Button::Enter, done + 1);
        assert_eq!(ui.timer.pomodoro.phase, Phase::ShortBreak);
        assert_eq!(ui.timer.pomodoro.remaining_ms(done + 1), Phase::ShortBreak.duration_ms());
    }

    #[test]
    fn pausing_at_or_after_expiry_finishes_before_the_tick() {
        for delay in [0, 1000] {
            let mut ui = on_timer();
            ui.timer.mode = Mode::Pomodoro;
            press(&mut ui, Button::Enter, 1000);
            let deadline = 1000 + Phase::Focus.duration_ms();
            let out = press(&mut ui, Button::Enter, deadline + delay);
            assert!(out.wake && ui.timer.pomodoro.done);
            assert_eq!(ui.timer.pomodoro.completed, 1);
            assert_eq!(ui.timer.primary(), Action::Next);
            assert!(!ui.timer.pomodoro.run.running());
            tick(&mut ui, deadline + delay + 10);
            assert_eq!(ui.timer.pomodoro.completed, 1, "the following tick does not count twice");
        }
    }

    #[test]
    fn all_three_tabs_work_with_buttons_and_touch() {
        let mut ui = on_timer();
        for mode in [Mode::Countdown, Mode::Pomodoro, Mode::Pomodoro] {
            press(&mut ui, Button::Right, 0);
            assert_eq!(ui.timer.mode, mode);
        }
        for mode in [Mode::Countdown, Mode::Stopwatch, Mode::Stopwatch] {
            press(&mut ui, Button::Left, 0);
            assert_eq!(ui.timer.mode, mode);
        }
        let (x, y) = layout::timer_tab(2).center();
        let target = hit(&ui, 7, x, y).unwrap();
        on_touch(&mut ui, 7, target, TouchAct::Activate, 0, &mut Update::new());
        assert_eq!(ui.timer.mode, Mode::Pomodoro);
        let (x, y) = layout::TIMER_PRIMARY.center();
        let target = hit(&ui, 7, x, y).unwrap();
        on_touch(&mut ui, 7, target, TouchAct::Activate, 1000, &mut Update::new());
        assert!(ui.timer.pomodoro.run.running());
        let (x, y) = layout::timer_preset(0).center();
        assert_eq!(hit(&ui, 7, x, y), None, "Pomodoro phases use fixed lengths");
    }

    #[test]
    fn idle_home_waits_for_a_running_clock_on_this_face() {
        let mut ui = on_timer();
        press(&mut ui, Button::Enter, 1000);
        let limit = ui.deck.settings.idle_home_s as u32 * 1000;
        let mut out = Update::new();
        ui.tick(1000 + limit, limit, true, &mut out);
        assert_eq!(ui.current_face().template, Template::Timer);
        press(&mut ui, Button::Enter, 2000);
        let mut out = Update::new();
        ui.tick(3000 + limit, limit + 1000, true, &mut out);
        assert_eq!(ui.current_face().template, Template::Agents);
    }
}
