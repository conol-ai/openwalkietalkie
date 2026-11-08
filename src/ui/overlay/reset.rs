//! The factory-reset dialog (`Overlay::ConfirmReset`): "Factory reset?" in
//! the remove-confirm card over the Settings face, with Keep and Reset
//! buttons. Not opaque, like the power-off dialog: the face stays beneath.
//! The Settings grammar (`state::settings`) owns what ENTER, CANCEL and a
//! tap on a button do; the runtime acts on `Update::factory_reset`.
use crate::display::Band;
use crate::ui::kit::{self, Hint};
use crate::ui::signature::SigBuilder;
use crate::ui::state::settings::CONFIRM_PRESSED;
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, layout, theme};

pub const HINTS: [Hint<'static>; 2] =
    [Hint { key: "ENTER", label: "reset" }, Hint { key: "CANCEL", label: "keep" }];
const TITLE: &str = "Factory reset?";
const LINES: [&str; 2] = ["Forgets the Bluetooth pairing,", "shows the tutorial, restarts."];
/// Button 0 = keep (CANCEL), 1 = reset (ENTER), as `layout::confirm_button`.
const LABELS: [&str; 2] = ["Keep", "Reset"];

pub fn draw(band: &mut Band, ui: &UiState) {
    kit::hint_bar_localized(band, &HINTS, ui.language);
    let bx = layout::CONFIRM_BOX;
    if !kit::visible(band, bx) {
        return;
    }
    kit::round(band, bx, theme::CARD_R, theme::CARD_FILL);
    kit::outline(band, bx, theme::CARD_R, theme::RED, 1);
    let text = layout::CONFIRM_TEXT;
    let (cx, _) = text.center();
    let lh = font::line_height(Font::Body);
    font::draw(band, Font::Name, cx, text.top() + 28, ui.tr(TITLE), theme::TEXT_PRIMARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    for (index, line) in LINES.iter().enumerate() {
        font::draw(band, Font::Body, cx, text.top() + 28 + 32 + index as i32 * lh, ui.tr(line), theme::TEXT_SECONDARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    }
    for (index, label) in LABELS.iter().enumerate() {
        let rect = layout::confirm_button(index as u8);
        let pressed = ui.settings.pressed == Some(CONFIRM_PRESSED + index as u8);
        let (fill, color) = if pressed {
            kit::round(band, rect, theme::CARD_R, theme::CHIP_FILL);
            kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_SELECTED, 1);
            (theme::CHIP_FILL, theme::TEXT_PRIMARY)
        } else if index == 1 {
            kit::round(band, rect, theme::CARD_R, theme::CARD_FILL);
            kit::outline(band, rect, theme::CARD_R, theme::RED, 1);
            (theme::CARD_FILL, theme::RED)
        } else {
            kit::round(band, rect, theme::CARD_R, theme::CARD_FILL);
            kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_THUMB, 1);
            (theme::CARD_FILL, theme::TEXT_ROW)
        };
        let (bx, by) = rect.center();
        font::draw(band, Font::Name, bx, by + font::ascent(Font::Name) * 9 / 20, ui.tr(label), color, Some(fill), rect.width() - 16, Align::Center);
    }
}

/// The card's text is constant; only a touched button changes it.
pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    sig.region(layout::HINT_BAR, 2);
    sig.region_with(layout::CONFIRM_BOX, |h| {
        h.opt_u32(ui.settings.pressed.filter(|&p| p >= CONFIRM_PRESSED).map(u32::from));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_strings_fit_the_card() {
        let room = layout::CONFIRM_TEXT.width();
        assert!(font::measure(Font::Name, TITLE) <= room);
        for line in LINES {
            assert!(font::measure(Font::Body, line) <= room, "{line}");
        }
        for label in LABELS {
            assert!(font::measure(Font::Name, label) <= layout::confirm_button(0).width() - 16);
        }
        assert_eq!(kit::hints_that_fit(&HINTS).0, 2);
    }
}
