//! Full-panel pixel-art games with a compact HUD and rounded-corner masking.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::state::{
    arcade::{
        self,
        games::{self, H, W},
        Game, Phase, FIELD,
    },
    UiState,
};
use crate::ui::{
    font::{self, Align, Font},
    kit,
    layout::{self, Rect},
    signature::SigBuilder,
    theme,
};
use core::fmt::Write;
use embedded_graphics::prelude::*;
const SKY: u32 = 0x0A1727;
fn label(b: &mut Band, x: i32, y: i32, text: &str, color: u32, size: Font, width: i32) {
    font::draw(b, size, x, y, text, color, None, width, Align::Left);
}
fn block(b: &mut Band, x: i32, y: i32, w: i32, h: i32, color: u32) {
    kit::fill(b, Rect::new(x, y, w, h), color);
}
fn plane(b: &mut Band, x: i32, y: i32, enemy: bool, scale: i32) {
    let color = if enemy { theme::RED } else { theme::BLUE };
    for (dx, dy, w, h, c) in [
        (-3, -12, 6, 25, color),
        (-13, -1, 26, 7, color),
        (-8, 9, 16, 4, color),
        (-2, -6, 4, 7, theme::TEXT_PRIMARY),
    ] {
        // Enemies descend nose-first. Mirror each rectangle's full height so
        // the cockpit and tail face down while the player's plane faces up.
        let dy = if enemy { -dy - h } else { dy };
        block(b, x + dx * scale, y + dy * scale, w * scale, h * scale, c);
    }
    if !enemy {
        block(
            b,
            x - 2 * scale,
            y + 14 * scale,
            4 * scale,
            5 * scale,
            theme::AMBER,
        );
    }
}
fn bird(b: &mut Band, x: i32, y: i32, scale: i32) {
    for (dx, dy, w, h, c) in [
        (-8, -6, 16, 12, theme::AMBER),
        (-5, -10, 11, 5, theme::AMBER),
        (-11, 0, 10, 5, 0xFFDB75),
        (6, 1, 7, 4, theme::RED),
        (3, -6, 4, 4, theme::TEXT_PRIMARY),
        (5, -5, 2, 2, theme::CANVAS),
    ] {
        block(b, x + dx * scale, y + dy * scale, w * scale, h * scale, c);
    }
}
/// Match the physical round-rectangle panel in native previews as well. Only
/// the current band's corner rows are touched; no full-frame buffer is needed.
fn mask_corners(b: &mut Band) {
    let bounds = b.bounding_box();
    for y in bounds.top_left.y.max(0)..(bounds.top_left.y + bounds.size.height as i32).min(480) {
        if (theme::CORNER_R..480 - theme::CORNER_R).contains(&y) {
            continue;
        }
        let mut inset = 0;
        while !layout::point_in_safe_area(inset, y) {
            inset += 1;
        }
        block(b, 0, y, inset, 1, theme::CANVAS);
        block(b, 480 - inset, y, inset, 1, theme::CANVAS);
    }
}
fn icon(b: &mut Band, game: Game, x: i32, y: i32) {
    match game {
        Game::Flappy => bird(b, x, y, 1),
        Game::Snake => {
            block(b, x - 13, y - 8, 25, 8, theme::GREEN);
            block(b, x + 4, y, 8, 15, theme::GREEN);
            block(b, x - 12, y + 7, 18, 8, theme::GREEN);
            block(b, x - 10, y - 6, 2, 2, theme::CANVAS);
        }
        Game::Raiden => plane(b, x, y, false, 1),
    }
}
pub fn draw(b: &mut Band, ui: &UiState, _id: u8) {
    let a = &ui.arcade;
    b.fill(kit::rgb(theme::CANVAS));
    if a.phase == Phase::Menu {
        kit::header(b, ui.tr("ARCADE"), kit::HeaderRight::Text(ui.tr("8-BIT")));
        for (i, game) in Game::ALL.into_iter().enumerate() {
            let r = arcade::menu_row(i as u8);
            let selected = a.selection == i as u8;
            kit::round(b, r, 16, if selected { SKY } else { theme::CHIP_FILL });
            if selected {
                kit::outline(b, r, 16, theme::BLUE, 2);
            }
            icon(b, game, r.left() + 32, r.top() + 36);
            label(
                b,
                r.left() + 64,
                r.top() + 30,
                ui.tr(game.title()),
                theme::TEXT_PRIMARY,
                Font::Name,
                296,
            );
            label(
                b,
                r.left() + 64,
                r.top() + 57,
                ui.tr(game.controls()),
                theme::TEXT_SECONDARY,
                Font::Small,
                296,
            );
        }
        kit::round(b, arcade::SOUND, 12, theme::CHIP_FILL);
        if a.selection == 3 {
            kit::outline(b, arcade::SOUND, 12, theme::BLUE, 2);
        }
        label(b, 80, 398, ui.tr("Sound"), theme::TEXT_SECONDARY, Font::Small, 80);
        label(
            b,
            164,
            398,
            ui.tr(arcade::sound_label(ui.deck.settings.arcade_sound)),
            theme::TEXT_PRIMARY,
            Font::Small,
            236,
        );
        kit::hint_bar_localized(
            b,
            &[
                kit::Hint {
                    key: "SCROLL",
                    label: "pick",
                },
                kit::Hint {
                    key: "ENTER",
                    label: "select",
                },
            ],
            ui.language,
        );
        return;
    }
    kit::fill(b, FIELD, SKY);
    let (ox, oy) = (FIELD.left(), FIELD.top());
    match a.game {
        Game::Flappy => {
            for p in a.flappy.pipes {
                let x = ox + i32::from(p.x);
                let top = i32::from(p.gap - games::PIPE_GAP);
                let bottom = i32::from(p.gap + games::PIPE_GAP);
                block(b, x, oy, i32::from(games::PIPE_WIDTH), top, 0x238969);
                block(
                    b,
                    x - 4,
                    oy + top - 14,
                    i32::from(games::PIPE_WIDTH) + 8,
                    14,
                    theme::GREEN,
                );
                block(
                    b,
                    x,
                    oy + bottom,
                    i32::from(games::PIPE_WIDTH),
                    i32::from(H) - bottom,
                    0x238969,
                );
                block(
                    b,
                    x - 4,
                    oy + bottom,
                    i32::from(games::PIPE_WIDTH) + 8,
                    14,
                    theme::GREEN,
                );
                block(b, x + 8, oy, 6, (top - 14).max(0), 0x3DBB8D);
            }
            bird(b, ox + i32::from(games::BIRD_X), oy + a.flappy.y / 256, 2);
        }
        Game::Snake => {
            for y in 0..games::ROWS {
                for x in 0..games::COLS {
                    let color = if !games::playable(games::Cell { x, y }) {
                        0x294159
                    } else if (x + y) % 2 == 0 {
                        0x0E1D2D
                    } else {
                        SKY
                    };
                    block(
                        b,
                        ox + i32::from(x) * games::CELL,
                        oy + i32::from(y) * games::CELL,
                        games::CELL,
                        games::CELL,
                        color,
                    );
                }
            }
            for (i, c) in a.snake.body[..a.snake.len].iter().enumerate() {
                block(
                    b,
                    ox + i32::from(c.x) * games::CELL + 1,
                    oy + i32::from(c.y) * games::CELL + 1,
                    games::CELL - 2,
                    games::CELL - 2,
                    if i == 0 { 0xB8FF78 } else { theme::GREEN },
                );
            }
            let f = a.snake.food;
            block(
                b,
                ox + i32::from(f.x) * games::CELL + 3,
                oy + i32::from(f.y) * games::CELL + 3,
                games::CELL - 6,
                games::CELL - 6,
                theme::RED,
            );
        }
        Game::Raiden => {
            for i in 0..50 {
                let x = (i * 97 + 11) % i32::from(W);
                let y = (i * 61 + (a.raiden.ticks % H as u32) as i32) % i32::from(H);
                block(b, ox + x, oy + y, 2, 2, 0x3B526B);
            }
            for s in a.raiden.shots {
                if s.active {
                    block(
                        b,
                        ox + i32::from(s.x) - 2,
                        oy + i32::from(s.y) - 7,
                        4,
                        14,
                        theme::AMBER,
                    );
                }
            }
            for s in a.raiden.bullets {
                if s.active {
                    block(
                        b,
                        ox + i32::from(s.x) - 3,
                        oy + i32::from(s.y) - 3,
                        6,
                        6,
                        theme::RED,
                    );
                }
            }
            for s in a.raiden.enemies {
                if s.active {
                    plane(b, ox + i32::from(s.x), oy + i32::from(s.y), true, 2);
                }
            }
            if a.raiden.invulnerable / 5 % 2 == 0 {
                plane(
                    b,
                    ox + i32::from(a.raiden.x),
                    oy + i32::from(games::PLAYER_Y),
                    false,
                    2,
                );
            }
        }
    }
    let mut stats = Text::<64>::new();
    let _ = write!(
        stats,
        "{:04}  {} {:04}",
        a.score(),
        ui.tr("BEST"),
        a.best[a.game as usize]
    );
    // Transparent HUD leaves the whole board available. Shadow keeps numbers
    // legible as pipes and stars pass underneath, without an opaque header.
    label(b, 82, 32, stats.as_str(), theme::CANVAS, Font::Small, 300);
    label(
        b,
        80,
        30,
        stats.as_str(),
        theme::TEXT_PRIMARY,
        Font::Small,
        300,
    );
    if a.game == Game::Raiden {
        for life in 0..a.raiden.lives {
            block(b, 352 + i32::from(life) * 14, 20, 8, 8, theme::BLUE);
        }
    }
    if a.phase != Phase::Playing {
        let r = Rect::new(56, 154, 368, 174);
        kit::round(b, r, 14, theme::CHIP_FILL);
        kit::outline(b, r, 14, theme::BLUE, 1);
        font::draw(
            b,
            Font::Small,
            240,
            182,
            ui.tr(a.game.title()),
            theme::TEXT_SECONDARY,
            None,
            344,
            Align::Center,
        );
        font::draw(
            b,
            Font::Small,
            240,
            254,
            ui.tr(a.game.controls()),
            theme::TEXT_SECONDARY,
            None,
            344,
            Align::Center,
        );
        let title = match a.phase {
            Phase::Ready => "READY?",
            Phase::Paused => "PAUSED",
            Phase::Over if a.game == Game::Snake && a.snake.won => "YOU WIN!",
            _ => "GAME OVER",
        };
        font::draw(
            b,
            Font::Name,
            240,
            224,
            ui.tr(title),
            theme::TEXT_PRIMARY,
            None,
            344,
            Align::Center,
        );
        font::draw(
            b,
            Font::Small,
            240,
            284,
            ui.tr(match a.phase {
                Phase::Ready if a.game == Game::Flappy => "PTT / Enter to start",
                Phase::Ready => "Enter to start",
                Phase::Paused => "Enter to resume",
                _ => "Enter to retry",
            }),
            theme::TEXT_SECONDARY,
            None,
            344,
            Align::Center,
        );
        font::draw(
            b,
            Font::Small,
            240,
            312,
            ui.tr("Cancel · game list"),
            theme::TEXT_SECONDARY,
            None,
            344,
            Align::Center,
        );
    }
    mask_corners(b);
}

pub fn signature(ui: &UiState, _id: u8, sig: &mut SigBuilder) {
    let a = &ui.arcade;
    sig.mode(a.phase as u32);
    sig.mode(a.game as u32);
    if a.phase == Phase::Menu {
        sig.region_with(Rect::SCREEN, |h| {
            h.u8(a.selection).u8(ui.deck.settings.arcade_sound);
        });
    } else if a.game == Game::Flappy {
        // Fixed slots let damage erase each sprite's previous bounds as well
        // as draw its new ones. The sky and transparent HUD stay stationary;
        // a simulation revision must not invalidate all 230,400 pixels.
        sig.region(
            Rect::new(i32::from(games::BIRD_X) - 22, a.flappy.y / 256 - 20, 48, 32),
            0,
        );
        for p in a.flappy.pipes {
            let x = i32::from(p.x) - 4;
            let top = i32::from(p.gap - games::PIPE_GAP);
            let bottom = i32::from(p.gap + games::PIPE_GAP);
            let width = i32::from(games::PIPE_WIDTH) + 8;
            sig.region(Rect::new(x, 0, width, top), 0);
            sig.region(Rect::new(x, bottom, width, i32::from(H) - bottom), 0);
        }
        // Includes the text shadow and five-digit u16 scores. Moving pipes
        // beneath it redraw the HUD through the same clipped scene renderer.
        sig.region_with(Rect::new(80, 8, 302, 26), |h| {
            h.u16(a.score()).u16(a.best[a.game as usize]);
        });
    } else {
        sig.region_with(FIELD, |h| {
            h.u32(a.visual).u16(a.score()).u16(a.best[a.game as usize]);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::i18n::Language;

    #[test]
    fn localized_game_names_and_instructions_fit_the_menu() {
        for language in Language::ALL {
            for game in Game::ALL {
                assert!(font::measure(Font::Name, language.text(game.title())) <= 296, "{language:?}: {}", game.title());
                assert!(font::measure(Font::Small, language.text(game.controls())) <= 296, "{language:?}: {}", game.controls());
            }
            for mode in 0..3 {
                assert!(font::measure(Font::Small, language.text(arcade::sound_label(mode))) <= 236);
            }
        }
    }
}
