//! Language changes must erase previous labels on both panel orientations.
use super::*;
use crate::{
    deck::msg::{FaceEntry, PttCtx},
    ui::{i18n::Language, state::{Overlay, PttSession, tutorial::Step}},
};

fn states() -> Vec<Box<UiState>> {
    let mut states = Vec::new();
    for template in [Template::Agents, Template::Omarchy, Template::Macropad, Template::Usage, Template::System, Template::Arcade, Template::Timer] {
        let mut state = ui();
        if template == Template::Timer {
            let len = state.global.strip_len as usize;
            state.global.strip[len] = state.global.strip[len - 1];
            state.global.strip[len - 1] = FaceEntry::new(Template::Timer.default_id(), Template::Timer, "Timer");
            state.global.strip_len += 1;
            state.timer.mode = crate::ui::state::timer::Mode::Pomodoro;
        }
        state.on_face(template);
        states.push(state);
    }
    for panel in 0..crate::ui::state::settings::PANELS {
        let mut state = on(Template::Settings);
        state.settings.panel = panel;
        states.push(state);
    }
    let mut scrolled = on(Template::Settings);
    scrolled.settings.panel = 3;
    scrolled.settings.row = 4;
    states.push(scrolled);
    for overlay in [Overlay::Manager, Overlay::Gallery, Overlay::Naming, Overlay::ConfirmRemove, Overlay::ConfirmReset, Overlay::Drawer, Overlay::MacroEdit] {
        let mut state = ui();
        state.global.overlay = overlay;
        state.manager.naming_template = Template::Timer;
        state.manager.confirm_face = state.global.strip[0].id;
        states.push(state);
    }
    for step in Step::ALL {
        let mut state = ui();
        state.start_tutorial(true);
        state.tutorial.step = step;
        states.push(state);
    }
    let mut ptt = ui();
    ptt.global.active_channel = 2;
    ptt.global.ptt = Some(PttSession::new(2, PttCtx::Talk, 0, 0));
    states.push(ptt);
    let mut lock = ui();
    lock.set_screen_lock(Some(0));
    states.push(lock);
    let mut image = ui();
    image.agents.agent = 2;
    image.deck.host_present = false;
    crate::ui::image::open(&mut image, 0, 1, 0, &mut crate::ui::Update::new());
    states.push(image);
    states
}

#[test]
fn every_language_keeps_interface_ink_inside_the_round_display() {
    for (index, mut state) in states().into_iter().enumerate() {
        for language in Language::ALL {
            state.language = language;
            let frame = full(&state);
            assert_eq!(lit_outside_safe_area(&frame), None, "state {index}, {}", language.code());
        }
    }
}

#[test]
fn changing_language_repaints_faces_and_opaque_overlays_completely() {
    for state in states() {
        let mut previous = state;
        let mut screens = Screens::new(&previous);
        for language in [Language::SimplifiedChinese, Language::TraditionalChinese, Language::Japanese, Language::English] {
            let mut next = previous.clone();
            next.language = language;
            assert_ne!(signature(&previous).mode(), signature(&next).mode());
            assert_eq!(update(&mut screens, &previous, &next), 480 * 480);
            previous = next;
        }
    }
}

#[test]
fn language_is_device_local_and_custom_names_are_preserved() {
    let mut state = Box::new(UiState::new());
    state.reset();
    for language in Language::ALL {
        state.language = language;
        assert!(state.still_default(), "locale must not suppress host boot sync");
        let default = FaceEntry::new(Template::Timer.default_id(), Template::Timer, "Timer");
        assert_eq!(state.face_name(&default), language.text("Timer"));
        let custom = FaceEntry::new(Template::Timer.default_id(), Template::Timer, "My计时器");
        assert_eq!(state.face_name(&custom), "My计时器");
    }
}

#[test]
fn host_question_text_that_matches_catalogue_keys_keeps_its_original_pixels() {
    let mut state = ui();
    state.agents.view = crate::ui::state::AgentsView::Card;
    state.agents.agent = 1;
    let agent = state.deck.agent_mut(1).unwrap();
    agent.has_card = true;
    agent.card = crate::deck::model::Card::new();
    agent.card.text.set("Ready Settings Start");
    let reference = full(&state);
    let lane = layout::CARD_QUESTION;
    for language in Language::ALL {
        state.language = language;
        let translated = full(&state);
        for y in lane.top()..lane.bottom() {
            let start = (y as usize * 480 + lane.left() as usize) * 2;
            let end = start + lane.width() as usize * 2;
            assert_eq!(&reference[start..end], &translated[start..end], "host body changed in {} at row {y}", language.code());
        }
    }
}
