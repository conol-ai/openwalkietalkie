//! Replay partial damage for real simulated game frames and all menus.
use super::*;
use crate::{
    control::{Button, Control, Edge},
    ui::state::{
        arcade::{games, Game, Phase},
        Update,
    },
};
fn press(ui: &mut UiState, b: Button, now: u32) {
    ui.apply(Control::Button(b, Edge::Press), now, &mut Update::new());
    ui.apply(Control::Button(b, Edge::Release), now, &mut Update::new());
}

#[test]
fn flappy_motion_does_not_repaint_the_whole_screen() {
    let mut ui = on(Template::Arcade);
    ui.deck.settings.demo = false;
    press(&mut ui, Button::Enter, 0);
    press(&mut ui, Button::Enter, 0);
    ui.arcade.flappy.pipes[0].x = 360;
    let mut screens = Screens::new(&ui);
    let old = (*ui).clone();
    ui.tick(20, 0, true, &mut Update::new());
    let pixels = update(&mut screens, &old, &ui);
    eprintln!("Flappy one-pipe frame: {pixels} / 230400 pixels");
    assert!(pixels < 480 * 480 / 4, "one moving pipe and bird transferred {pixels} pixels");
}

#[test]
fn flappy_partial_frames_cover_pipe_recycling_hud_and_phase_changes() {
    let mut ui = on(Template::Arcade);
    ui.deck.settings.demo = false;
    press(&mut ui, Button::Enter, 0);
    press(&mut ui, Button::Enter, 0);
    let mut screens = Screens::new(&ui);
    let mut seed = 123;
    let mut recycled = false;
    let mut total = 0;
    // Keep the bird flying while the real pipe/scoring simulation crosses
    // every screen edge, HUD and corner, then recycles offscreen pipes. Also
    // cover skipped simulation steps as happens when a render is late.
    for frame in 0..360 {
        let old = (*ui).clone();
        for _ in 0..1 + frame % 3 {
            let before = ui.arcade.flappy.pipes;
            ui.arcade.flappy.step(&mut seed);
            recycled |= before.iter().zip(ui.arcade.flappy.pipes).any(|(a, b)| b.x > a.x);
        }
        ui.arcade.flappy.y = (210 + frame % 60) * 256;
        ui.arcade.flappy.velocity = 0;
        ui.arcade.best[0] = ui.arcade.flappy.score;
        ui.arcade.visual += 1;
        total += update(&mut screens, &old, &ui);
    }
    assert!(recycled && ui.arcade.flappy.score > 0);
    eprintln!("Flappy pipe traversal: average {} / 230400 pixels", total / 360);
    assert!(total / 360 < 480 * 480 * 2 / 3);
    for score in [9, 10, 9999, 10000, u16::MAX, 0] {
        let old = (*ui).clone();
        ui.arcade.flappy.score = score;
        ui.arcade.best[0] = score;
        update(&mut screens, &old, &ui);
    }
    // Subpixel/velocity-only changes do not touch the screen.
    let old = (*ui).clone();
    ui.arcade.flappy.y += 1;
    ui.arcade.flappy.velocity += 1;
    ui.arcade.visual += 1;
    assert_eq!(update(&mut screens, &old, &ui), 0);
    for phase in [Phase::Paused, Phase::Playing, Phase::Over, Phase::Ready, Phase::Menu] {
        let old = (*ui).clone();
        ui.arcade.phase = phase;
        update(&mut screens, &old, &ui);
    }
}

#[test]
fn gameplay_reaches_all_edges_and_hides_passive_status_chrome() {
    use crate::ui::{
        bluetooth::{Level, Status},
        state::Overlay,
    };
    for game in Game::ALL {
        let mut ui = on(Template::Arcade);
        ui.deck.settings.demo = false;
        ui.arcade.selection = game as u8;
        press(&mut ui, Button::Enter, 0);
        press(&mut ui, Button::Enter, 0);
        let frame = full(&ui);
        let canvas = crate::ui::kit::rgb(crate::ui::theme::CANVAS).into_storage();
        for (x, y) in [(0, 240), (240, 0), (479, 240), (240, 479)] {
            let at = (y * 480 + x) * 2;
            assert_ne!(u16::from_le_bytes([frame[at], frame[at + 1]]), canvas);
        }
        assert_eq!(lit_outside_safe_area(&frame), None);
        let mut screens = Screens::new(&ui);
        let old = (*ui).clone();
        ui.update_bluetooth(Status::Signal(Level::Strong));
        assert_eq!(update(&mut screens, &old, &ui), 0);
        let old = (*ui).clone();
        ui.global.overlay = Overlay::Drawer;
        update(&mut screens, &old, &ui);
        let old = (*ui).clone();
        ui.global.overlay = Overlay::None;
        update(&mut screens, &old, &ui);
        if game == Game::Raiden {
            for x in [games::PLAYER_MIN_X, games::PLAYER_MAX_X] {
                let old = (*ui).clone();
                ui.arcade.raiden.x = x;
                ui.arcade.visual += 1;
                update(&mut screens, &old, &ui);
                assert_eq!(lit_outside_safe_area(&full(&ui)), None);
            }
        }
        let old = (*ui).clone();
        press(&mut ui, Button::Cancel, 10);
        update(&mut screens, &old, &ui);
        let old = (*ui).clone();
        ui.update_bluetooth(Status::Signal(Level::Weak));
        assert!(
            update(&mut screens, &old, &ui) > 0,
            "status returns in the menu"
        );
    }
}
#[test]
fn arcade_menu_and_games_fit_and_partial_updates_match_full_frames() {
    let mut ui = on(Template::Arcade);
    ui.deck.settings.demo = false;
    assert_eq!(lit_outside_safe_area(&full(&ui)), None);
    let mut screens = Screens::new(&ui);
    for game in Game::ALL {
        ui.arcade.selection = game as u8;
        let old = (*ui).clone();
        press(&mut ui, Button::Enter, 10);
        update(&mut screens, &old, &ui);
        assert_eq!(ui.arcade.phase, Phase::Ready);
        assert_eq!(lit_outside_safe_area(&full(&ui)), None);
        let old = (*ui).clone();
        press(&mut ui, Button::Enter, 20);
        update(&mut screens, &old, &ui);
        for now in (40..=440).step_by(40) {
            let old = (*ui).clone();
            ui.tick(now, 0, true, &mut Update::new());
            update(&mut screens, &old, &ui);
        }
        assert_eq!(lit_outside_safe_area(&full(&ui)), None);
        let old = (*ui).clone();
        press(&mut ui, Button::Enter, 460);
        update(&mut screens, &old, &ui);
        let old = (*ui).clone();
        press(&mut ui, Button::Cancel, 480);
        update(&mut screens, &old, &ui);
        // Reset preview clock for the next game; production uses monotonic time.
        ui.now_ms = 0;
    }
}
