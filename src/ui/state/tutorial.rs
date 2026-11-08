//! The first-run tutorial (`Overlay::Tutorial`): six steps that each wait
//! for the control they teach, with a drawing of the deck that lights the
//! control being asked for (`overlay::device`). The runtime opens it on a
//! deck whose preferences say it was never finished; Settings > Display >
//! Tutorial replays it.
//!
//! Steps (`Step`, in order):
//! 1. Buttons: ENTER, CANCEL, SHIFT, LEFT and RIGHT, one at a time in that
//!    order (`BUTTONS`); the page asks for one key, lit on the drawing, and
//!    the right press moves on to the next. Any other button is named
//!    instead ("That's SHIFT, not CANCEL").
//! 2. Wheel: `WHEEL_DETENTS` detents, either way.
//! 3. Panels: SHIFT held plus `PANEL_SWITCHES` detents; the strip's names
//!    show the selection move. A detent without SHIFT only changes the
//!    caption.
//! 4. Latest: hold SHIFT and press ENTER to return to the latest chat text.
//! 5. PTT: press and hold, then release.
//! 6. Done: ENTER (or CANCEL) closes it. Closing from here, or skipping,
//!    sets `Update::tutorial_done`, which the runtime persists.
//! CANCEL on steps 2-5 skips to Done on a first run and closes a replay.
//! Every other control is swallowed here before `global` sees it: SHIFT
//! edges are mirrored into `global.shift` and button releases are routed to
//! their owners, so nothing is left held when the tutorial closes.
use super::{global, global::Overlay, UiState, Update};
use crate::control::{Button, Control, Edge};
use crate::deck::text::Text;
use crate::ui::i18n::Language;

/// The buttons the first step asks for, in order.
pub const BUTTONS: [Button; 5] = [
    Button::Enter,
    Button::Cancel,
    Button::Shift,
    Button::Left,
    Button::Right,
];
pub const WHEEL_DETENTS: u8 = 3;
pub const PANEL_SWITCHES: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Buttons,
    Wheel,
    Panels,
    Latest,
    Ptt,
    Done,
}

impl Step {
    pub const ALL: [Step; 6] = [Step::Buttons, Step::Wheel, Step::Panels, Step::Latest, Step::Ptt, Step::Done];
    /// 1-based, for the "2 / 6" header tag.
    pub const fn number(self) -> u8 {
        self as u8 + 1
    }
    const fn next(self) -> Step {
        match self {
            Step::Buttons => Step::Wheel,
            Step::Wheel => Step::Panels,
            Step::Panels => Step::Latest,
            Step::Latest => Step::Ptt,
            Step::Ptt | Step::Done => Step::Done,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    pub step: Step,
    /// Opened from Settings: CANCEL on steps 2-5 closes instead of skipping.
    pub replay: bool,
    /// Index into `BUTTONS` of the key the Buttons step asks for now.
    pub index: u8,
    /// The last button pressed on the Buttons step, and whether it was not
    /// the one asked for.
    pub named: Option<Button>,
    pub wrong: bool,
    /// Detents so far on the Wheel step.
    pub detents: u8,
    /// Strip index the Panels step shows selected, and SHIFT+detents so far.
    pub panel: u8,
    pub switches: u8,
    /// A detent or ENTER without SHIFT on the Panels or Latest step.
    pub needs_shift: bool,
    /// PTT is down (Ptt step).
    pub ptt_down: bool,
}

impl View {
    pub const fn new() -> Self {
        Self {
            step: Step::Buttons,
            replay: false,
            index: 0,
            named: None,
            wrong: false,
            detents: 0,
            panel: 0,
            switches: 0,
            needs_shift: false,
            ptt_down: false,
        }
    }

    /// The key the Buttons step asks for; `None` once all five are in.
    pub fn current(&self) -> Option<Button> {
        BUTTONS.get(self.index as usize).copied()
    }

    /// `button` was already pressed on the Buttons step.
    pub fn learned(&self, button: Button) -> bool {
        BUTTONS.iter().position(|b| *b == button).is_some_and(|at| at < self.index as usize)
    }
}

/// What the keycap of a button reads.
pub const fn name(button: Button) -> &'static str {
    match button {
        Button::Enter => "ENTER",
        Button::Cancel => "CANCEL",
        Button::Shift => "SHIFT",
        Button::PushToTalk => "PTT",
        Button::Left => "LEFT",
        Button::Right => "RIGHT",
    }
}

/// What a button does: the line under "Press ENTER".
pub const fn role(button: Button) -> &'static str {
    match button {
        Button::Enter => "Confirms, opens, replies.",
        Button::Cancel => "Goes back. Hold 3 s to switch off.",
        Button::Shift => "Hold it to use shortcuts.",
        Button::PushToTalk => "Hold it and speak. Let go to send.",
        Button::Left => "Previous tab or choice.",
        Button::Right => "Next tab or choice.",
    }
}

/// The caption line under a step's content. Pure: `draw` and `signature`
/// both go through it.
pub fn caption(view: &View, out: &mut Text<48>) {
    caption_localized(view, out, Language::English);
}

/// Device-owned captions; physical key names remain the labels on the case.
pub fn caption_localized<const N: usize>(view: &View, out: &mut Text<N>, language: Language) {
    out.clear();
    match view.step {
        Step::Buttons => match (view.wrong, view.named, view.current()) {
            (true, Some(pressed), Some(wanted)) => {
                out.push_str(language.text("That's "));
                out.push_str(name(pressed));
                out.push_str(language.text(", not "));
                out.push_str(name(wanted));
            }
            (_, Some(done), _) => {
                out.push_str(language.text("That was "));
                out.push_str(name(done));
                out.push_str(" \u{b7} ");
                progress(view, out, language);
            }
            _ => progress(view, out, language),
        },
        Step::Wheel => out.set(language.text(match WHEEL_DETENTS.saturating_sub(view.detents) {
            0 => "",
            1 => "One more",
            2 => "Two more",
            _ => "Turn the wheel",
        })),
        Step::Panels if view.needs_shift => out.set(language.text("Hold SHIFT while you turn")),
        Step::Panels => out.set(language.text("Hold SHIFT and turn the wheel")),
        Step::Latest if view.needs_shift => out.set(language.text("Hold SHIFT while you press ENTER")),
        Step::Latest => out.set(language.text("Hold SHIFT and press ENTER")),
        Step::Ptt if view.ptt_down => out.set(language.text("Listening\u{2026} let go to send")),
        Step::Ptt => out.set(language.text("Hold PTT")),
        Step::Done => {}
    }
}

/// "2 of 5" on the Buttons step.
fn progress<const N: usize>(view: &View, out: &mut Text<N>, language: Language) {
    out.push_char((b'1' + view.index.min(BUTTONS.len() as u8 - 1)) as char);
    out.push_str(language.text(" of "));
    out.push_char((b'0' + BUTTONS.len() as u8) as char);
}

/// Open at the first step. `replay`: from Settings, not the first run.
pub(super) fn open(ui: &mut UiState, replay: bool, at_ms: u32, out: &mut Update) {
    ui.tutorial = View::new();
    ui.tutorial.replay = replay;
    ui.tutorial.panel = ui.global.current.min(ui.global.strip_len.saturating_sub(1));
    global::set_overlay(ui, Overlay::Tutorial, at_ms, out);
    out.dirty = true;
}

/// Finished or skipped: the deck remembers either.
fn close(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    global::set_overlay(ui, Overlay::None, at_ms, out);
    out.tutorial_done = true;
    out.dirty = true;
}

fn advance(ui: &mut UiState, out: &mut Update) {
    ui.tutorial.step = ui.tutorial.step.next();
    ui.tutorial.needs_shift = false;
    out.dirty = true;
}

/// Every control while the tutorial is up (`global::on_control` hands them
/// over before any of its own handling).
pub(super) fn on_control(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) {
    let step = ui.tutorial.step;
    match control {
        Control::Button(Button::Shift, edge) => {
            // Mirrored, so the modifier is right when the tutorial closes
            // with SHIFT still down.
            ui.global.shift = edge == Edge::Press;
            if edge == Edge::Press {
                pressed(ui, Button::Shift, out);
            }
        }
        Control::Button(button, Edge::Release) => {
            global::release_source(ui, super::Source::Button(button), out);
            if button == Button::PushToTalk && step == Step::Ptt && ui.tutorial.ptt_down {
                ui.tutorial.ptt_down = false;
                advance(ui, out);
            }
        }
        Control::Button(Button::PushToTalk, Edge::Press) => match step {
            Step::Ptt => {
                ui.tutorial.ptt_down = true;
                out.dirty = true;
            }
            Step::Buttons => pressed(ui, Button::PushToTalk, out),
            _ => {}
        },
        Control::Button(button, Edge::Press) => match step {
            Step::Buttons => pressed(ui, button, out),
            Step::Latest if button == Button::Enter => {
                if ui.global.shift {
                    advance(ui, out);
                } else {
                    out.dirty |= !ui.tutorial.needs_shift;
                    ui.tutorial.needs_shift = true;
                }
            }
            Step::Done => {
                if matches!(button, Button::Enter | Button::Cancel) {
                    close(ui, at_ms, out);
                }
            }
            _ if button == Button::Cancel => {
                if ui.tutorial.replay {
                    close(ui, at_ms, out);
                } else {
                    ui.tutorial.step = Step::Done;
                    out.dirty = true;
                }
            }
            _ => {}
        },
        Control::Wheel(wheel) => match step {
            Step::Wheel => {
                ui.tutorial.detents += 1;
                if ui.tutorial.detents >= WHEEL_DETENTS {
                    advance(ui, out);
                }
                out.dirty = true;
            }
            Step::Panels if ui.global.shift => {
                let last = ui.global.strip_len.saturating_sub(1);
                let next = ui.tutorial.panel as i32 + super::selection_step(wheel);
                ui.tutorial.panel = next.clamp(0, last as i32) as u8;
                ui.tutorial.needs_shift = false;
                ui.tutorial.switches += 1;
                if ui.tutorial.switches >= PANEL_SWITCHES {
                    advance(ui, out);
                }
                out.dirty = true;
            }
            Step::Panels => {
                out.dirty |= !ui.tutorial.needs_shift;
                ui.tutorial.needs_shift = true;
            }
            _ => {}
        },
        Control::Touch(_) => {}
    }
}

/// A button press on the Buttons step: the asked-for one moves on (the
/// fifth ends the step), any other is only named.
fn pressed(ui: &mut UiState, button: Button, out: &mut Update) {
    if ui.tutorial.step != Step::Buttons {
        return;
    }
    ui.tutorial.named = Some(button);
    if ui.tutorial.current() == Some(button) {
        ui.tutorial.wrong = false;
        ui.tutorial.index += 1;
        if ui.tutorial.current().is_none() {
            advance(ui, out);
        }
    } else {
        ui.tutorial.wrong = true;
    }
    out.dirty = true;
}

pub(super) fn on_tick(_ui: &mut UiState, _now_ms: u32, _out: &mut Update) {}

pub(super) fn animating(_ui: &UiState) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_captions_keep_physical_key_names_and_fit_the_lane() {
        use crate::ui::{font, font::Font};
        for language in Language::ALL {
            for step in Step::ALL {
                let mut view = View::new();
                view.step = step;
                for needs_shift in [false, true] {
                    view.needs_shift = needs_shift;
                    for named in Button::ALL {
                        view.named = Some(named);
                        for wrong in [false, true] {
                            view.wrong = wrong;
                            let mut text = Text::<96>::new();
                            caption_localized(&view, &mut text, language);
                            assert!(font::measure(Font::Row, text.as_str()) <= 404, "{} {step:?}: {}", language.code(), text.as_str());
                            if step == Step::Buttons {
                                assert!(text.as_str().contains(name(named)));
                            }
                        }
                    }
                }
            }
        }
    }
    use crate::control::Wheel;
    use crate::deck::msg::Template;

    fn fresh() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui
    }

    fn control(ui: &mut UiState, control: Control, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(control, at, &mut out);
        out
    }
    fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
        control(ui, Control::Button(button, Edge::Press), at)
    }
    fn release(ui: &mut UiState, button: Button, at: u32) -> Update {
        control(ui, Control::Button(button, Edge::Release), at)
    }
    fn tap(ui: &mut UiState, button: Button, at: u32) -> Update {
        let out = press(ui, button, at);
        release(ui, button, at + 20);
        out
    }
    fn caption_of(ui: &UiState) -> std::string::String {
        let mut text = Text::new();
        caption(&ui.tutorial, &mut text);
        std::string::String::from(text.as_str())
    }

    #[test]
    fn the_first_run_walks_every_step_and_marks_itself_done_at_the_end() {
        let mut ui = fresh();
        ui.start_tutorial(false);
        assert_eq!((ui.global.overlay, ui.tutorial.step), (Overlay::Tutorial, Step::Buttons));
        assert_eq!((ui.tutorial.current(), caption_of(&ui).as_str()), (Some(Button::Enter), "1 of 5"));
        // The wrong key is named and nothing moves; PTT is a wrong key too.
        tap(&mut ui, Button::Cancel, 10);
        assert_eq!((ui.tutorial.index, caption_of(&ui).as_str()), (0, "That's CANCEL, not ENTER"));
        tap(&mut ui, Button::PushToTalk, 40);
        assert_eq!(caption_of(&ui), "That's PTT, not ENTER");
        assert!(ui.global.ptt.is_none(), "no PTT session in the tutorial");
        control(&mut ui, Control::Wheel(Wheel::Cw), 60);
        assert_eq!(ui.tutorial.step, Step::Buttons);
        // The asked-for key moves on.
        tap(&mut ui, Button::Enter, 100);
        assert_eq!((ui.tutorial.current(), caption_of(&ui).as_str()), (Some(Button::Cancel), "That was ENTER \u{b7} 2 of 5"));
        assert!(ui.tutorial.learned(Button::Enter) && !ui.tutorial.learned(Button::Cancel));
        tap(&mut ui, Button::Cancel, 150);
        assert_eq!(ui.tutorial.current(), Some(Button::Shift));
        // SHIFT: the modifier is mirrored and released again.
        press(&mut ui, Button::Shift, 200);
        assert!(ui.global.shift);
        assert_eq!(ui.tutorial.current(), Some(Button::Left));
        release(&mut ui, Button::Shift, 220);
        assert!(!ui.global.shift);
        assert_eq!(ui.global.overlay, Overlay::Tutorial, "two SHIFT taps never open the manager here");
        tap(&mut ui, Button::Left, 300);
        tap(&mut ui, Button::Right, 340);
        assert_eq!(ui.tutorial.step, Step::Wheel);
        assert_eq!(caption_of(&ui), "Turn the wheel");
        for turn in 1..=WHEEL_DETENTS {
            control(&mut ui, Control::Wheel(if turn % 2 == 0 { Wheel::Cw } else { Wheel::Ccw }), 400 + turn as u32 * 10);
        }
        assert_eq!(ui.tutorial.step, Step::Panels);
        // Without SHIFT the strip does not move and the caption says so.
        control(&mut ui, Control::Wheel(Wheel::Cw), 500);
        assert_eq!((ui.tutorial.switches, caption_of(&ui).as_str()), (0, "Hold SHIFT while you turn"));
        press(&mut ui, Button::Shift, 510);
        control(&mut ui, Control::Wheel(Wheel::Ccw), 520);
        assert_eq!((ui.tutorial.panel, ui.tutorial.switches), (1, 1));
        assert_eq!(ui.global.current, 0, "the real strip never slides");
        control(&mut ui, Control::Wheel(Wheel::Cw), 540);
        assert_eq!(ui.tutorial.panel, 0, "SHIFT+CW selects the previous panel");
        release(&mut ui, Button::Shift, 560);
        assert_eq!(ui.tutorial.step, Step::Latest);
        tap(&mut ui, Button::Enter, 570);
        assert_eq!(ui.tutorial.step, Step::Latest, "ENTER alone cannot complete the chord step");
        assert_eq!(caption_of(&ui), "Hold SHIFT while you press ENTER");
        press(&mut ui, Button::Shift, 600);
        control(&mut ui, Control::Wheel(Wheel::Cw), 610);
        assert_eq!(ui.tutorial.step, Step::Latest, "SHIFT + wheel does not complete this step");
        tap(&mut ui, Button::Enter, 620);
        release(&mut ui, Button::Shift, 650);
        assert_eq!(ui.tutorial.step, Step::Ptt);
        assert!(!ui.global.shift);
        press(&mut ui, Button::PushToTalk, 660);
        assert!(ui.tutorial.ptt_down && ui.global.ptt.is_none());
        assert_eq!(caption_of(&ui), "Listening\u{2026} let go to send");
        let out = release(&mut ui, Button::PushToTalk, 700);
        assert_eq!(ui.tutorial.step, Step::Done);
        assert!(!out.tutorial_done);
        let out = press(&mut ui, Button::Enter, 800);
        assert!(out.tutorial_done && ui.global.overlay == Overlay::None);
        release(&mut ui, Button::Enter, 820);
        assert!(!global::anything_held(&ui), "every press was released to its owner");
        assert_eq!(ui.current_face().template, Template::Agents);
    }

    #[test]
    fn cancel_skips_a_first_run_to_the_end_and_closes_a_replay() {
        let mut ui = fresh();
        ui.start_tutorial(false);
        ui.tutorial.step = Step::Wheel;
        tap(&mut ui, Button::Cancel, 10);
        assert_eq!((ui.tutorial.step, ui.global.overlay), (Step::Done, Overlay::Tutorial));
        let out = tap(&mut ui, Button::Cancel, 100);
        assert!(out.tutorial_done && ui.global.overlay == Overlay::None);

        let mut ui = fresh();
        ui.on_face(Template::Settings);
        ui.start_tutorial(true);
        assert!(ui.tutorial.replay);
        // On the first step CANCEL is one of the keys asked for.
        tap(&mut ui, Button::Cancel, 10);
        assert_eq!(ui.global.overlay, Overlay::Tutorial);
        assert!(ui.tutorial.wrong);
        ui.tutorial.step = Step::Ptt;
        let out = tap(&mut ui, Button::Cancel, 100);
        assert!(out.tutorial_done && ui.global.overlay == Overlay::None);
        assert_eq!(ui.current_face().template, Template::Settings);
    }

    #[test]
    fn a_held_cancel_still_powers_the_deck_off_from_inside_the_tutorial() {
        let mut ui = fresh();
        ui.start_tutorial(false);
        press(&mut ui, Button::Cancel, 0);
        let mut out = Update::new();
        ui.tick(global::POWER_HOLD_MS, 0, true, &mut out);
        assert!(out.power_off);
    }

    #[test]
    fn the_captions_and_names_cover_every_state() {
        let mut view = View::new();
        let mut text = Text::new();
        for button in Button::ALL {
            assert!(!name(button).is_empty() && !role(button).is_empty());
        }
        view.index = 4;
        view.named = Some(Button::Left);
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "That was LEFT \u{b7} 5 of 5");
        view.step = Step::Wheel;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "Turn the wheel");
        view.detents = WHEEL_DETENTS - 1;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "One more");
        view.step = Step::Panels;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "Hold SHIFT and turn the wheel");
        view.step = Step::Latest;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "Hold SHIFT and press ENTER");
        view.step = Step::Ptt;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "Hold PTT");
        view.step = Step::Done;
        caption(&view, &mut text);
        assert_eq!(text.as_str(), "");
        assert_eq!(Step::Done.number(), 6);
        assert_eq!(Step::ALL.len(), 6);
    }
}
