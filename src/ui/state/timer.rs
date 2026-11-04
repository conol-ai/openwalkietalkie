//! Grammar of the Timer face: a stopwatch and a countdown timer that run on
//! the device alone. No host, no events: both keep running off-screen, under
//! overlays and with the panel dark, because they are only ever a start stamp
//! and what was banked before it.
//!
//! Rules (docs/agent-command-deck.md, "Timer"):
//! - LEFT/RIGHT or a tab switch between Stopwatch and Timer; the other one
//!   keeps running.
//! - ENTER (or the right button) is the primary action: Start, Pause, Resume;
//!   on a finished timer, Restart.
//! - CANCEL (or the left button) is the secondary action: Lap while the
//!   stopwatch runs, else Reset; with nothing to reset it goes home.
//! - SCROLL sets the timer's length while it is idle: 10 s steps up to 1 min,
//!   30 s up to 5 min, 1 min up to 1 h, then 5 min, between `MIN_S` and
//!   `MAX_S`. A preset chip sets it at once.
//! - A timer reaching zero stops at zero, raises a "Timer done" toast that
//!   wakes the screen and jumps here when tapped, and shows "time's up" until
//!   it is restarted or reset.
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

/// Touch target ids, = `View::pressed`.
pub const TAB_ID: u16 = 0; // + mode index
pub const SECONDARY_ID: u16 = 4;
pub const PRIMARY_ID: u16 = 5;
pub const PRESET_ID: u16 = 8; // + preset index

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Stopwatch,
    Countdown,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Stopwatch, Mode::Countdown];
    pub const fn label(self) -> &'static str {
        match self {
            Mode::Stopwatch => "Stopwatch",
            Mode::Countdown => "Timer",
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

/// What the two buttons do right now; `None` = shown disabled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Start,
    Pause,
    Resume,
    Restart,
    Lap,
    Reset,
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
            pressed: None,
            shown: 0,
        }
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

    /// Either clock is running.
    pub fn running(&self) -> bool {
        self.stopwatch.running() || self.countdown.running()
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
        }
    }

    /// CANCEL / the left button; `None` = nothing to do (CANCEL goes home).
    pub fn secondary(&self) -> Option<Action> {
        match self.mode {
            Mode::Stopwatch if self.stopwatch.running() => Some(Action::Lap),
            Mode::Stopwatch => (self.stopwatch.banked_ms > 0).then_some(Action::Reset),
            Mode::Countdown => (!self.countdown_idle()).then_some(Action::Reset),
        }
    }

    /// The left button's label, also when it is disabled.
    pub fn secondary_label(&self) -> &'static str {
        match (self.secondary(), self.mode) {
            (Some(action), _) => action.label(),
            (None, Mode::Stopwatch) => Action::Lap.label(),
            (None, Mode::Countdown) => Action::Reset.label(),
        }
    }

    /// The displayed value of the current clock, in its own unit: tenths for
    /// the stopwatch, whole seconds (rounded up) for the timer.
    pub fn shown_step(&self, now_ms: u32) -> u32 {
        match self.mode {
            Mode::Stopwatch => self.stopwatch_ms(now_ms) / 100,
            Mode::Countdown => self.remaining_ms(now_ms).div_ceil(1000),
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
    let view = &mut ui.timer;
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
        (Mode::Countdown, Action::Lap) | (Mode::Stopwatch, Action::Restart) => return,
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
        Control::Button(Button::Left, Edge::Press) => select_mode(ui, Mode::Stopwatch, out),
        Control::Button(Button::Right, Edge::Press) => select_mode(ui, Mode::Countdown, out),
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
        out.dirty = true;
        let face = (0..ui.global.strip_len)
            .filter_map(|index| ui.face_at(index))
            .find(|face| face.template == Template::Timer)
            .map_or(0, |face| face.id);
        notify::raise_timer(ui, face, now_ms, out);
    }
    let step = ui.timer.shown_step(now_ms);
    if step != ui.timer.shown {
        ui.timer.shown = step;
        out.dirty |= visible(ui);
    }
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
    fn the_readout_repaints_once_per_tenth_and_only_when_visible() {
        let mut ui = on_timer();
        press(&mut ui, Button::Enter, 1000);
        assert!(tick(&mut ui, 1100).dirty);
        assert!(!tick(&mut ui, 1150).dirty);
        assert!(tick(&mut ui, 1200).dirty);
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
