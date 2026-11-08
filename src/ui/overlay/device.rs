//! Schematic front view of the deck for the tutorial, with controls lit
//! and labelled.
//!
//! Geometry comes from the BodyV6.13 case (hardware/rev-a/mechanical-source/
//! case/BodyV6.13.step: its eight solids are the case, the wheel and the six
//! key caps; X across, Z up, Y toward the viewer) and the r6 main board.
//! Seen from the front: USB-C in the top edge; the thumb wheel is a disc in
//! the top-left corner, its rim flush with both edges; PTT is the long key
//! below it on the left edge; ENTER, CANCEL and SHIFT run down the right edge
//! beside the display; LEFT and RIGHT are the two bars under the display; the
//! speaker grille sits bottom right and the microphone hole bottom left.
//! Positions are tenths of a millimetre from the body's top-left corner,
//! drawn at `SCALE` hundredths of a pixel per tenth.
use crate::control::Button;
use crate::display::Band;
use crate::ui::kit;
use crate::ui::{font, font::Align, font::Font, layout::Rect, theme};

/// Which controls are lit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Highlight {
    None,
    Button(Button),
    Chord(Button, Button),
    Wheel,
}

/// Hundredths of a pixel per tenth of a millimetre: 1.9 px/mm.
const SCALE: i32 = 19;
/// The case: 56 x 95 mm, 5 mm corners.
const BODY: (i32, i32) = (560, 950);
const CORNER: i32 = 50;
/// Wheel disc in the top-left corner: centre and radius.
const WHEEL: (i32, i32, i32) = (85, 85, 85);
/// The round active area of the display.
const DISPLAY: (i32, i32, i32) = (280, 272, 190);
/// USB-C opening in the top edge: centre x, width, depth.
const USB: (i32, i32, i32) = (280, 90, 30);
const SPEAKER: (i32, i32, i32) = (409, 765, 115);
const MIC: (i32, i32) = (80, 697);
/// How far a side key stands proud of the edge, and how far it is drawn
/// into the body so it reads as a key, not a hairline.
const KEY_OUT: i32 = 35;
const KEY_IN: i32 = 25;
/// Leader line and label of the lit control.
const LEAD_GAP: i32 = 4;
const LEAD_LEN: i32 = 18;

const fn px(tenths: i32) -> i32 {
    (tenths * SCALE + 50) / 100
}

/// The drawing's size in pixels, body only (labels reach beyond it).
pub const WIDTH: i32 = px(BODY.0);
pub const HEIGHT: i32 = px(BODY.1);
/// Room the labels need on either side and below.
pub const LABEL_ROOM: i32 = 90;

/// A key cap in tenths of a millimetre (x0, y0, x1, y1) from the body's
/// top-left corner; side keys straddle their edge.
const fn cap(button: Button) -> (i32, i32, i32, i32) {
    match button {
        Button::Enter => (BODY.0 - KEY_IN, 136, BODY.0 + KEY_OUT, 234),
        Button::Cancel => (BODY.0 - KEY_IN, 256, BODY.0 + KEY_OUT, 354),
        Button::Shift => (BODY.0 - KEY_IN, 376, BODY.0 + KEY_OUT, 474),
        Button::PushToTalk => (-KEY_OUT, 277, KEY_IN, 437),
        Button::Left => (98, 521, 278, 621),
        Button::Right => (282, 521, 462, 621),
    }
}

fn cap_rect(left: i32, top: i32, button: Button) -> Rect {
    let (x0, y0, x1, y1) = cap(button);
    Rect::new(left + px(x0), top + px(y0), px(x1) - px(x0), px(y1) - px(y0))
}

fn centred_baseline(font: Font, cy: i32) -> i32 {
    cy + font::ascent(font) * 9 / 20
}

/// Label the lit control: a leader line away from the body and its name.
fn callout(band: &mut Band, left: i32, top: i32, highlight: Highlight, language: crate::ui::i18n::Language) {
    let (name, from, dir): (&str, (i32, i32), (i32, i32)) = match highlight {
        Highlight::None => return,
        Highlight::Chord(first, second) => {
            callout(band, left, top, Highlight::Button(first), language);
            callout(band, left, top, Highlight::Button(second), language);
            return;
        }
        Highlight::Wheel => {
            let (cx, cy, r) = WHEEL;
            (language.text("WHEEL"), (left + px(cx - r), top + px(cy)), (-1, 0))
        }
        Highlight::Button(button) => {
            let rect = cap_rect(left, top, button);
            let (cx, cy) = rect.center();
            match button {
                Button::Enter | Button::Cancel | Button::Shift => {
                    (crate::ui::state::tutorial::name(button), (rect.right(), cy), (1, 0))
                }
                Button::PushToTalk => (crate::ui::state::tutorial::name(button), (rect.left(), cy), (-1, 0)),
                Button::Left | Button::Right => {
                    (crate::ui::state::tutorial::name(button), (cx, rect.bottom()), (0, 1))
                }
            }
        }
    };
    let (x, y) = from;
    match dir {
        (1, 0) => {
            kit::fill(band, Rect::new(x + LEAD_GAP, y - 1, LEAD_LEN, 2), theme::GREEN);
            font::draw(band, Font::Small, x + LEAD_GAP + LEAD_LEN + 6, centred_baseline(Font::Small, y), name, theme::GREEN, Some(theme::CANVAS), LABEL_ROOM, Align::Left);
        }
        (-1, 0) => {
            kit::fill(band, Rect::new(x - LEAD_GAP - LEAD_LEN, y - 1, LEAD_LEN, 2), theme::GREEN);
            font::draw(band, Font::Small, x - LEAD_GAP - LEAD_LEN - 6, centred_baseline(Font::Small, y), name, theme::GREEN, Some(theme::CANVAS), LABEL_ROOM, Align::Right);
        }
        _ => {
            kit::fill(band, Rect::new(x - 1, y + LEAD_GAP, 2, LEAD_LEN), theme::GREEN);
            font::draw(band, Font::Small, x, y + LEAD_GAP + LEAD_LEN + 4 + font::ascent(Font::Small), name, theme::GREEN, Some(theme::CANVAS), LABEL_ROOM, Align::Center);
        }
    }
}

fn key(band: &mut Band, left: i32, top: i32, button: Button, lit: bool) {
    let rect = cap_rect(left, top, button);
    if lit {
        kit::round(band, rect, 2, theme::GREEN);
        kit::outline(band, rect.inset(-3), 4, theme::GREEN, 1);
    } else {
        kit::round(band, rect, 2, theme::KEYCAP_FILL);
        kit::outline(band, rect, 2, theme::OUTLINE_KEY, 1);
    }
}

fn wheel(band: &mut Band, left: i32, top: i32, lit: bool) {
    let (cx, cy, r) = (left + px(WHEEL.0), top + px(WHEEL.1), px(WHEEL.2));
    if lit {
        kit::dot(band, cx, cy, r + 1, theme::GREEN);
        kit::dot(band, cx, cy, r - 2, theme::THUMB_BAR);
    } else {
        kit::dot(band, cx, cy, r + 1, theme::OUTLINE_THUMB);
        kit::dot(band, cx, cy, r, theme::THUMB_BAR);
    }
    // Knurl marks on the rim, then the hub.
    for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let m = r - 4;
        kit::fill(band, Rect::new(cx + dx * m - 1, cy + dy * m - 1, 3, 3), theme::OUTLINE_THUMB);
    }
    kit::dot(band, cx, cy, r / 3, if lit { theme::GREEN } else { theme::OUTLINE_KEY });
}

/// Draw the deck with its body's top-left corner at (`left`, `top`).
pub fn draw(band: &mut Band, left: i32, top: i32, highlight: Highlight) {
    draw_localized(band, left, top, highlight, crate::ui::i18n::Language::English);
}

/// Localize the wheel callout; key names match the physical keycaps.
pub fn draw_localized(band: &mut Band, left: i32, top: i32, highlight: Highlight, language: crate::ui::i18n::Language) {
    let reach = Rect::new(left - LABEL_ROOM, top - 8, WIDTH + 2 * LABEL_ROOM, HEIGHT + 8 + LABEL_ROOM);
    if !kit::visible(band, reach) {
        return;
    }
    let body = Rect::new(left, top, WIDTH, HEIGHT);
    kit::round(band, body, px(CORNER) as u32, theme::CHIP_FILL);
    kit::outline(band, body, px(CORNER) as u32, theme::OUTLINE_THUMB, 1);
    // USB-C: an opening in the top edge.
    let usb = Rect::new(left + px(USB.0 - USB.1 / 2), top - 1, px(USB.1), px(USB.2) + 1);
    kit::fill(band, usb, theme::CANVAS);
    kit::outline(band, usb, 1, theme::OUTLINE_KEY, 1);
    // The round display, the speaker grille and the microphone hole.
    let (dx, dy, dr) = (left + px(DISPLAY.0), top + px(DISPLAY.1), px(DISPLAY.2));
    kit::dot(band, dx, dy, dr, theme::OUTLINE_KEY);
    kit::dot(band, dx, dy, dr - 1, theme::CANVAS);
    let (sx, sy, sr) = (left + px(SPEAKER.0), top + px(SPEAKER.1), px(SPEAKER.2));
    kit::dot(band, sx, sy, sr, theme::OUTLINE_KEY);
    kit::dot(band, sx, sy, sr - 1, theme::CHIP_FILL);
    for (ox, oy) in [(0, 0), (-8, 0), (8, 0), (0, -8), (0, 8)] {
        kit::dot(band, sx + ox, sy + oy, 2, theme::OUTLINE_THUMB);
    }
    kit::dot(band, left + px(MIC.0), top + px(MIC.1), 2, theme::OUTLINE_THUMB);
    for button in Button::ALL {
        let lit = match highlight {
            Highlight::Button(asked) => button == asked,
            Highlight::Chord(first, second) => button == first || button == second,
            _ => false,
        };
        key(band, left, top, button, lit);
    }
    // The wheel last: it sits over the corner.
    wheel(band, left, top, highlight == Highlight::Wheel);
    callout(band, left, top, highlight, language);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caps_sit_where_the_case_puts_them() {
        // Three keys down the right edge, top to bottom ENTER, CANCEL, SHIFT;
        // PTT on the left edge below the wheel; two bars under the display.
        let (enter, cancel, shift, ptt, left, right) = (
            cap(Button::Enter),
            cap(Button::Cancel),
            cap(Button::Shift),
            cap(Button::PushToTalk),
            cap(Button::Left),
            cap(Button::Right),
        );
        assert!(enter.1 < cancel.1 && cancel.1 < shift.1);
        assert!(enter.2 > BODY.0 && ptt.0 < 0);
        assert!(ptt.1 > WHEEL.1 + WHEEL.2);
        assert!(left.1 > DISPLAY.1 + DISPLAY.2 && left.2 < right.0);
        assert!(WHEEL.0 == WHEEL.2 && WHEEL.1 == WHEEL.2, "the wheel's rim meets both edges");
        assert_eq!((WIDTH, HEIGHT), (106, 181));
        for button in Button::ALL {
            assert!(font::measure(Font::Small, crate::ui::state::tutorial::name(button)) <= LABEL_ROOM);
        }
    }
}
