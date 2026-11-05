//! Timer face (`ui::faces::timer`): every state is clean inside the corner
//! arcs, the running readout repaints only its own lane, a scripted session
//! refreshes partially to the full redraw, and no origin panics.
use super::*;
use crate::{
    control::{Button, Control, Edge, Touch, TouchPhase, Wheel},
    deck::msg::FaceEntry,
    ui::{
        faces,
        state::timer::{Mode, Phase, Pomodoro, Run},
        Update,
    },
};

/// The demo strip with a Timer face before Settings, shown.
fn timer() -> Box<UiState> {
    let mut ui = ui();
    let len = ui.global.strip_len as usize;
    ui.global.strip[len] = ui.global.strip[len - 1];
    ui.global.strip[len - 1] = FaceEntry::new(Template::Timer.default_id(), Template::Timer, "Timer");
    ui.global.strip_len += 1;
    ui.on_face(Template::Timer);
    ui
}

/// Every look the face has: idle, running, paused, laps, done, long, pressed.
fn states() -> Vec<Box<UiState>> {
    let mut states = vec![timer()];
    let mut running = timer();
    running.now_ms = 800_000;
    running.timer.stopwatch = Run { banked_ms: 754_500, since_ms: Some(800_000) };
    running.timer.laps = [61_200, 59_800, 62_400];
    running.timer.lap_count = 11;
    states.push(running.clone());
    let mut long = running.clone();
    long.timer.stopwatch.banked_ms = 10 * 3_600_000 + 59 * 60_000 + 59_900;
    states.push(long);
    let mut paused = running.clone();
    paused.timer.stopwatch.since_ms = None;
    paused.timer.pressed = Some(4);
    states.push(paused);
    for (length, banked, since, done) in [
        (300_000, 0, None, false),
        (1_500_000, 432_000, Some(800_000), false),
        (1_500_000, 432_000, None, false),
        (300_000, 295_000, Some(800_000), false),
        (300_000, 300_000, None, true),
        (5 * 3_600_000, 0, None, false),
    ] {
        let mut countdown = running.clone();
        countdown.timer.mode = Mode::Countdown;
        countdown.timer.length_ms = length;
        countdown.timer.countdown = Run { banked_ms: banked, since_ms: since };
        countdown.timer.done = done;
        states.push(countdown);
    }
    let mut pressed = timer();
    pressed.timer.mode = Mode::Countdown;
    pressed.timer.pressed = Some(9);
    states.push(pressed);
    for (phase, completed) in [(Phase::Focus, 0), (Phase::ShortBreak, 1), (Phase::LongBreak, 4)] {
        for (banked, since, done) in [
            (0, None, false),
            (32_000, Some(800_000), false),
            (32_000, None, false),
            (phase.duration_ms() - 5_000, Some(800_000), false),
            (phase.duration_ms(), None, true),
        ] {
            let mut pomodoro = timer();
            pomodoro.now_ms = 800_000;
            pomodoro.timer.mode = Mode::Pomodoro;
            pomodoro.timer.pomodoro = Pomodoro {
                phase,
                completed: if done && phase == Phase::Focus { completed + 1 } else { completed },
                run: Run { banked_ms: banked, since_ms: since },
                done,
            };
            states.push(pomodoro);
        }
    }
    states
}

#[test]
fn every_state_is_clean_inside_the_corners() {
    for (index, ui) in states().iter().enumerate() {
        let frame = full(ui);
        assert_eq!(lit_outside_safe_area(&frame), None, "state {index}");
    }
}

#[test]
fn tab_labels_fit_beside_a_background_clock_indicator() {
    for (index, mode) in Mode::ALL.into_iter().enumerate() {
        let rect = layout::timer_tab(index as u8);
        let text_width = crate::ui::font::measure(crate::ui::font::Font::Small, mode.label());
        let text_right = rect.center().0 + (text_width + 1) / 2;
        let indicator_left = rect.right() - 10 - 3;
        assert!(text_right + 4 <= indicator_left, "{} overlaps its running dot", mode.label());
    }
}

#[test]
fn a_running_stopwatch_repaints_only_its_readout() {
    let mut previous = timer();
    let mut out = Update::new();
    previous.apply(Control::Button(Button::Enter, Edge::Press), 1000, &mut out);
    previous.apply(Control::Button(Button::Enter, Edge::Release), 1001, &mut out);
    previous.tick(1050, 0, true, &mut out);
    let mut screen = Screens::new(&previous);
    let mut next = Box::new(*previous);
    let mut out = Update::new();
    next.tick(1100, 0, true, &mut out);
    assert!(out.dirty);
    let transferred = update(&mut screen, &previous, &next);
    assert!(transferred > 0 && transferred <= 480 * 120, "{transferred} px for a tenth");
    let readout = layout::TIMER_READOUT;
    let mut dirty = damage(Some(&previous), &next);
    while let Some(rect) = dirty.next() {
        assert!(rect.y0 as i32 >= readout.top() - 2 && rect.y1 as i32 <= readout.bottom() + 2, "{rect:?}");
    }
}

struct Session {
    previous: Box<UiState>,
    next: Box<UiState>,
    screen: Screens,
    now: u32,
}

impl Session {
    fn new(ui: Box<UiState>) -> Self {
        let screen = Screens::new(&ui);
        Self { previous: Box::new(*ui), next: ui, screen, now: 1000 }
    }
    fn step(&mut self, f: impl FnOnce(&mut UiState, u32)) {
        self.now += 137;
        f(&mut self.next, self.now);
        update(&mut self.screen, &self.previous, &self.next);
        self.next.copy_into(&mut self.previous);
    }
    fn press(&mut self, button: Button) {
        self.step(|ui, at| {
            let mut out = Update::new();
            ui.apply(Control::Button(button, Edge::Press), at, &mut out);
            ui.apply(Control::Button(button, Edge::Release), at + 1, &mut out);
        });
    }
    fn wheel(&mut self, wheel: Wheel, detents: u32) {
        self.step(|ui, at| {
            let mut out = Update::new();
            for _ in 0..detents {
                ui.apply(Control::Wheel(wheel), at, &mut out);
            }
        });
    }
    fn tick(&mut self) {
        self.step(|ui, at| ui.tick(at, 0, true, &mut Update::new()));
    }
    fn advance(&mut self, millis: u32) {
        self.now += millis;
        self.tick();
    }
    fn tap(&mut self, rect: layout::Rect) {
        self.step(|ui, at| {
            let (x, y) = rect.center();
            // Control::Touch already uses logical coordinates; the input
            // contact tracker applies the panel mounting before this layer.
            let (x, y) = (x as u16, y as u16);
            let mut out = Update::new();
            ui.apply(Control::Touch(Touch { phase: TouchPhase::Down, x, y }), at, &mut out);
            ui.apply(Control::Touch(Touch { phase: TouchPhase::Up, x, y }), at + 1, &mut out);
        });
    }
}

#[test]
fn partial_refresh_matches_full_through_a_scripted_session() {
    let mut session = Session::new(timer());
    session.press(Button::Enter);
    for _ in 0..5 {
        session.tick();
    }
    session.press(Button::Cancel);
    session.tick();
    session.press(Button::Cancel);
    session.press(Button::Enter);
    session.tap(layout::TIMER_PRIMARY);
    session.press(Button::Right);
    session.wheel(Wheel::Ccw, 12);
    session.tap(layout::timer_preset(4));
    session.tap(layout::timer_preset(0));
    // 1 min down to the 10 s floor.
    session.wheel(Wheel::Ccw, 30);
    assert_eq!(session.next.timer.length_ms, 10_000);
    session.press(Button::Enter);
    for _ in 0..90 {
        session.tick();
    }
    assert!(session.next.timer.done && session.next.notify.toast.is_some());
    session.tap(layout::TIMER_SECONDARY);
    session.press(Button::Left);
    session.tap(layout::timer_tab(1));
    session.press(Button::Enter);
    session.tick();
    session.tap(layout::timer_tab(0));
    session.tick();
}

#[test]
fn pomodoro_partial_refresh_matches_full_through_focus_and_breaks() {
    let mut session = Session::new(timer());
    session.tap(layout::timer_tab(2));
    assert_eq!(session.next.timer.mode, Mode::Pomodoro);
    session.tap(layout::TIMER_PRIMARY);
    session.advance(5_000);
    session.tap(layout::TIMER_PRIMARY);
    assert!(!session.next.timer.pomodoro.run.running());
    session.advance(5_000);
    session.tap(layout::TIMER_PRIMARY);
    assert!(session.next.timer.pomodoro.run.running());

    for completed in 1..=4 {
        session.advance(Phase::Focus.duration_ms());
        assert!(session.next.timer.pomodoro.done);
        assert_eq!(session.next.timer.pomodoro.completed, completed);
        // The completion toast has left before interacting with the face.
        session.advance(5_000);
        session.tap(layout::TIMER_PRIMARY);
        let phase = if completed == 4 { Phase::LongBreak } else { Phase::ShortBreak };
        assert_eq!(session.next.timer.pomodoro.phase, phase);
        assert!(session.next.timer.pomodoro.run.running());
        session.advance(phase.duration_ms());
        assert!(session.next.timer.pomodoro.done);
        session.advance(5_000);
        session.tap(layout::TIMER_PRIMARY);
        assert_eq!(session.next.timer.pomodoro.phase, Phase::Focus);
        assert_eq!(session.next.timer.pomodoro.completed, if completed == 4 { 0 } else { completed });
    }
    session.tap(layout::TIMER_SECONDARY);
    assert_eq!(session.next.timer.pomodoro.run, Run::new());
    assert_eq!(session.next.timer.pomodoro.completed, 0);
    session.press(Button::Left);
    assert_eq!(session.next.timer.mode, Mode::Countdown);
    session.press(Button::Right);
    assert_eq!(session.next.timer.mode, Mode::Pomodoro);
}

#[test]
fn switching_away_from_a_running_pomodoro_refreshes_its_tab_indicator() {
    let mut session = Session::new(timer());
    session.tap(layout::timer_tab(2));
    session.press(Button::Enter);
    session.tick();
    session.tap(layout::timer_tab(0));
    assert_eq!(session.next.timer.mode, Mode::Stopwatch);
    session.advance(Phase::Focus.duration_ms());
    assert!(session.next.timer.pomodoro.done);
    session.advance(5_000);
    session.tap(layout::timer_tab(2));
    assert_eq!(session.next.timer.mode, Mode::Pomodoro);
}

#[test]
fn every_origin_draws_the_face_shifted_and_never_panics() {
    for ui in states() {
        let id = ui.current_face().id;
        let reference = full_draw(|band| faces::timer::draw(band, &ui, id));
        for dx in [-480, -333, -1, 1, 77, 479, 480] {
            let frame = full_draw(|band| {
                band.set_origin_x(dx);
                faces::timer::draw(band, &ui, id);
            });
            for y in (0..480).step_by(5) {
                for x in 0..480 {
                    let at = (y * 480 + x) as usize * 2;
                    let expected = if (0..480).contains(&(x - dx)) {
                        let from = (y * 480 + x - dx) as usize * 2;
                        [reference[from], reference[from + 1]]
                    } else {
                        [0, 0]
                    };
                    assert_eq!([frame[at], frame[at + 1]], expected, "origin {dx} at ({x}, {y})");
                }
            }
        }
    }
}
