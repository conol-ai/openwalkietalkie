//! Opaque pocket lock: every local control wakes, only the wheel unlocks.
use crate::control::UNLOCK_TICKS;
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::signature::SigBuilder;
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, kit, layout::Rect, theme};

pub const PROGRESS: Rect = Rect::new(150, 304, 180, 88);

pub fn draw(band: &mut Band, ui: &UiState) {
    let Some(progress) = ui.screen_lock else {
        return;
    };
    band.fill(kit::rgb(theme::CANVAS));
    // Padlock silhouette, kept inside the round display's safe area.
    kit::outline(band, Rect::new(218, 106, 44, 52), 20, theme::AMBER, 4);
    kit::round(band, Rect::new(204, 142, 72, 54), 10, theme::AMBER);
    kit::round(band, Rect::new(237, 159, 6, 20), 3, theme::CANVAS);
    font::draw(
        band,
        Font::Title,
        240,
        240,
        ui.tr("Locked"),
        theme::TEXT_PRIMARY,
        Some(theme::CANVAS),
        400,
        Align::Center,
    );
    font::draw(
        band,
        Font::Body,
        240,
        279,
        ui.tr("Turn wheel counterclockwise"),
        theme::TEXT_SECONDARY,
        Some(theme::CANVAS),
        416,
        Align::Center,
    );
    for tick in 0..UNLOCK_TICKS {
        let rect = Rect::new(183 + i32::from(tick) * 32, 318, 18, 18);
        if tick < progress {
            kit::round(band, rect, 9, theme::AMBER);
        } else {
            kit::outline(band, rect, 9, theme::TEXT_FAINT, 1);
        }
    }
    let mut count = Text::<16>::new();
    count.push_char(char::from(b'0' + progress));
    count.push_str(" / ");
    count.push_char(char::from(b'0' + UNLOCK_TICKS));
    font::draw(
        band,
        Font::Row,
        240,
        376,
        count.as_str(),
        theme::TEXT_PRIMARY,
        Some(theme::CANVAS),
        180,
        Align::Center,
    );
}

pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    sig.region(Rect::SCREEN, 0);
    sig.region_with(PROGRESS, |hash| {
        hash.u8(ui.screen_lock.unwrap_or(0));
    });
}
