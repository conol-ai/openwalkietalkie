//! The power-off hold dialog: "Power off?" over whatever is on screen, with
//! a bar that fills while CANCEL stays held (`global::POWER_HOLD_SHOW_MS` to
//! `global::POWER_HOLD_MS`). Not opaque, like the remove-confirm dialog whose
//! card it reuses: releasing CANCEL takes it away and the face beneath is
//! still there. The runtime powers the deck off when the bar is full.
use crate::display::Band;
use crate::ui::kit::Hint;
use crate::ui::signature::SigBuilder;
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, kit, layout, layout::Rect, theme};

pub const HINTS: [Hint<'static>; 2] =
    [Hint { key: "", label: "release" }, Hint { key: "CANCEL", label: "to stay on" }];
/// The bar sits where the remove-confirm dialog has its two buttons.
pub const BAR: Rect = Rect::new(68, 280, 344, 20);
const TITLE: &str = "Power off?";
const HELP: &str = "keep holding CANCEL";

pub fn draw(band: &mut Band, ui: &UiState) {
    let Some(percent) = ui.power_hold() else {
        return;
    };
    kit::hint_bar_localized(band, &HINTS, ui.language);
    let bx = layout::CONFIRM_BOX;
    if !kit::visible(band, bx) {
        return;
    }
    kit::round(band, bx, theme::CARD_R, theme::CARD_FILL);
    kit::outline(band, bx, theme::CARD_R, theme::AMBER, 1);
    let text = layout::CONFIRM_TEXT;
    let (cx, _) = text.center();
    let lh = font::line_height(Font::Title);
    font::draw(band, Font::Title, cx, text.top() + 32, ui.tr(TITLE), theme::TEXT_PRIMARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    font::draw(band, Font::Row, cx, text.top() + 32 + lh, ui.tr(HELP), theme::TEXT_SECONDARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    kit::bar(band, BAR, percent, theme::AMBER);
}

/// The card and hint bar are constant while the dialog is up; only the bar
/// moves. Opening and closing change the frame mode (`overlay::signature`).
pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    let Some(percent) = ui.power_hold() else {
        return;
    };
    sig.region(layout::HINT_BAR, 0);
    sig.region(layout::CONFIRM_BOX, 0);
    sig.region_with(BAR, |h| {
        h.u8(percent);
    });
}
