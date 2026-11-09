//! Drawing primitives and the widgets every deck face shares: header, hint
//! bar, keycap, state dot, row card, progress bar, chip, pill, list viewport.
//!
//! Rounded shapes are span fills with coverage anti-aliasing on the corner
//! arcs only (a signed-distance ramp one pixel wide, blended over what is
//! there). Everything culls against `band.bounding_box()` first: `draw` runs
//! once per 48-row band, ten times per frame.
use super::font::{self, Align, Font};
use super::layout::{self, Rect};
use super::state::UiState;
use super::theme;
use crate::deck::text::Text;
use crate::display::Band;
use embedded_graphics::{
    pixelcolor::{Rgb565, Rgb888},
    prelude::*,
    primitives::Rectangle,
};

pub fn rgb(color: u32) -> Rgb565 {
    Rgb888::new((color >> 16) as u8, (color >> 8) as u8, color as u8).into()
}

fn rectangle(rect: Rect) -> Rectangle {
    Rectangle::new(
        Point::new(rect.left(), rect.top()),
        Size::new(rect.width().max(0) as u32, rect.height().max(0) as u32),
    )
}

/// Does any of `rect` fall inside what this band pass draws?
pub fn visible(band: &Band, rect: Rect) -> bool {
    let clip = band.bounding_box();
    !rect.is_empty()
        && rect.left() < clip.top_left.x + clip.size.width as i32
        && rect.right() > clip.top_left.x
        && rect.top() < clip.top_left.y + clip.size.height as i32
        && rect.bottom() > clip.top_left.y
}

pub fn fill(band: &mut Band, rect: Rect, color: u32) {
    if !rect.is_empty() {
        let _ = band.fill_solid(&rectangle(rect), rgb(color));
    }
}

/// The rows of `rect` this band shows.
fn rows(band: &Band, rect: Rect) -> core::ops::Range<i32> {
    let clip = band.bounding_box();
    rect.top().max(clip.top_left.y)..rect.bottom().min(clip.top_left.y + clip.size.height as i32)
}

fn isqrt(n: u32) -> u32 {
    let mut rest = n;
    let mut result = 0;
    let mut bit = 1 << 30;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if rest >= result + bit {
            rest -= result + bit;
            result = (result >> 1) + bit;
        } else {
            result >>= 1;
        }
        bit >>= 2;
    }
    result
}

/// Coverage in sixteenths of pixel (x, y) by `rect` with corner radius `r`:
/// 16 inside, a one-pixel ramp across the corner arcs, 0 outside.
fn coverage(rect: Rect, r: i32, x: i32, y: i32) -> i32 {
    if !rect.contains(x, y) {
        return 0;
    }
    // Pixel centre relative to the nearest arc centre, in half pixels; 0 when
    // the pixel is beside a straight edge.
    let offset = |low: i32, high: i32, v: i32| {
        let from_low = 2 * (v - low) + 1 - 2 * r;
        let from_high = 2 * (v - high) + 1 + 2 * r;
        if from_low < 0 {
            from_low
        } else if from_high > 0 {
            from_high
        } else {
            0
        }
    };
    let dx = offset(rect.left(), rect.right(), x);
    let dy = offset(rect.top(), rect.bottom(), y);
    if dx == 0 || dy == 0 {
        return 16;
    }
    let d2 = dx * dx + dy * dy;
    if d2 <= (2 * r - 2) * (2 * r - 2) {
        return 16;
    }
    if d2 >= (2 * r + 2) * (2 * r + 2) {
        return 0;
    }
    (16 * r + 8 - isqrt(64 * d2 as u32) as i32).clamp(0, 16)
}

fn alpha(coverage: i32) -> u8 {
    (coverage.clamp(0, 16) * 255 / 16) as u8
}

/// A pixel and its horizontal mirror inside `rect`, solid or blended.
fn pair(band: &mut Band, rect: Rect, x: i32, y: i32, coverage: i32, color: u32) {
    let mirror = rect.right() - 1 - (x - rect.left());
    if coverage >= 16 {
        fill(band, Rect::new(x, y, 1, 1), color);
        fill(band, Rect::new(mirror, y, 1, 1), color);
    } else if coverage > 0 {
        band.blend_px(x, y, color, alpha(coverage));
        band.blend_px(mirror, y, color, alpha(coverage));
    }
}

fn clamp_radius(rect: Rect, radius: u32) -> i32 {
    (radius.min(i16::MAX as u32) as i32).min(rect.width() / 2).min(rect.height() / 2).max(0)
}

/// Filled rounded rectangle.
pub fn round(band: &mut Band, rect: Rect, radius: u32, color: u32) {
    if !visible(band, rect) {
        return;
    }
    let r = clamp_radius(rect, radius);
    fill(band, Rect::new(rect.left(), rect.top() + r, rect.width(), rect.height() - 2 * r), color);
    for y in rows(band, rect) {
        if y >= rect.top() + r && y < rect.bottom() - r {
            continue;
        }
        // Left arc up to the first solid pixel, mirrored; one span between.
        let mut solid = rect.left() + r;
        for x in rect.left()..rect.left() + r {
            let c = coverage(rect, r, x, y);
            if c >= 16 {
                solid = x;
                break;
            }
            pair(band, rect, x, y, c, color);
        }
        fill(band, Rect::new(solid, y, rect.width() - 2 * (solid - rect.left()), 1), color);
    }
}

/// Rounded outline, `width` px, inside `rect`.
pub fn outline(band: &mut Band, rect: Rect, radius: u32, color: u32, width: u32) {
    if !visible(band, rect) || width == 0 {
        return;
    }
    let w = width.min(i16::MAX as u32) as i32;
    if 2 * w >= rect.width().min(rect.height()) {
        round(band, rect, radius, color);
        return;
    }
    let r = clamp_radius(rect, radius);
    let inner = rect.inset(w);
    let ri = (r - w).max(0);
    for y in rows(band, rect) {
        if y >= rect.top() + r && y < rect.bottom() - r {
            fill(band, Rect::new(rect.left(), y, w, 1), color);
            fill(band, Rect::new(rect.right() - w, y, w, 1), color);
            continue;
        }
        for x in rect.left()..rect.left() + r {
            pair(band, rect, x, y, coverage(rect, r, x, y) - coverage(inner, ri, x, y), color);
        }
        if y < rect.top() + w || y >= rect.bottom() - w {
            fill(band, Rect::new(rect.left() + r, y, rect.width() - 2 * r, 1), color);
        }
    }
}

/// Dashed 1 px rounded outline over a `fill`-coloured surface.
pub fn dashed(band: &mut Band, rect: Rect, radius: u32, color: u32, fill_color: u32) {
    if !visible(band, rect) {
        return;
    }
    outline(band, rect, radius, color, 1);
    // Knock 2 px gaps out of every 6 px of the straight runs; corners stay solid.
    let r = clamp_radius(rect, radius);
    let mut x = rect.left() + r + 4;
    while x + 2 <= rect.right() - r {
        fill(band, Rect::new(x, rect.top(), 2, 1), fill_color);
        fill(band, Rect::new(x, rect.bottom() - 1, 2, 1), fill_color);
        x += 6;
    }
    let mut y = rect.top() + r + 4;
    while y + 2 <= rect.bottom() - r {
        fill(band, Rect::new(rect.left(), y, 1, 2), fill_color);
        fill(band, Rect::new(rect.right() - 1, y, 1, 2), fill_color);
        y += 6;
    }
}

/// Filled circle, centre (cx, cy), diameter `2 r + 1`, soft edge.
pub fn dot(band: &mut Band, cx: i32, cy: i32, r: i32, color: u32) {
    let (cx, cy, r) = (cx.clamp(-4096, 4096), cy.clamp(-4096, 4096), r.clamp(0, 4096));
    let rect = Rect::new(cx - r, cy - r, 2 * r + 1, 2 * r + 1);
    if !visible(band, rect) {
        return;
    }
    for y in rows(band, rect) {
        let dy = 2 * (y - cy);
        let mut solid = cx + 1;
        for x in cx - r..=cx {
            let dx = 2 * (x - cx);
            let d2 = dx * dx + dy * dy;
            let c = if d2 <= (2 * r - 1) * (2 * r - 1) {
                16
            } else if d2 >= (2 * r + 3) * (2 * r + 3) {
                0
            } else {
                (16 * r + 16 - isqrt(64 * d2 as u32) as i32).clamp(0, 16)
            };
            if c >= 16 {
                solid = x;
                break;
            }
            pair(band, rect, x, y, c, color);
        }
        fill(band, Rect::new(solid, y, 2 * (cx - solid) + 1, 1), color);
    }
}

/// The mockups' selected outline is 1.5 px: one bright ring and a half-tone
/// ring inside it over the card fill.
fn selected_outline(band: &mut Band, rect: Rect, radius: u32) {
    outline(band, rect, radius, theme::OUTLINE_SELECTED, 1);
    let half = (theme::OUTLINE_SELECTED & 0xFEFEFE) / 2 + (theme::CARD_FILL & 0xFEFEFE) / 2;
    outline(band, rect.inset(1), radius.saturating_sub(1), half, 1);
}

/// The row/card surface: selected = filled with the bright outline, idle =
/// thin outline on canvas, dashed = touch-only / placeholder.
pub fn card(band: &mut Band, rect: Rect, selected: bool, is_dashed: bool) {
    if !visible(band, rect) {
        return;
    }
    if is_dashed {
        dashed(band, rect, theme::CARD_R, theme::OUTLINE_KEY, theme::CANVAS);
    } else if selected {
        round(band, rect, theme::CARD_R, theme::CARD_FILL);
        selected_outline(band, rect, theme::CARD_R);
    } else {
        outline(band, rect, theme::CARD_R, theme::OUTLINE, 1);
    }
}

/// Progress bar: `percent` of `rect` in `color` over the track.
pub fn bar(band: &mut Band, rect: Rect, percent: u8, color: u32) {
    if !visible(band, rect) {
        return;
    }
    let radius = (rect.height() / 2) as u32;
    round(band, rect, radius, theme::CHIP_FILL);
    let width = rect.width() * percent.min(100) as i32 / 100;
    if width > 0 {
        let filled = Rect::new(rect.left(), rect.top(), width.max(rect.height()), rect.height());
        round(band, filled, radius, color);
    }
}

/// Baseline that centres capitals and digits of `font` on `cy`.
fn centred_baseline(font: Font, cy: i32) -> i32 {
    cy + font::ascent(font) * 9 / 20
}

/// Small square button with centred text (workspace chips, settings tabs).
/// Squares get the row face; wider chips the small face, or the hint face
/// when the text would not fit otherwise ("Announce" in a 66 px tab).
pub fn chip(band: &mut Band, rect: Rect, text: &str, selected: bool) {
    let max_w = rect.width() - 8;
    let font = if rect.width() < 48 {
        Font::Row
    } else if font::measure(Font::Small, text) <= max_w {
        Font::Small
    } else {
        Font::Hint
    };
    chip_in(band, rect, text, selected, font);
}

/// `chip` in a face the caller picked, for a strip whose chips must agree.
pub fn chip_in(band: &mut Band, rect: Rect, text: &str, selected: bool, font: Font) {
    if !visible(band, rect) {
        return;
    }
    let (fg, bg) = if selected {
        round(band, rect, theme::CHIP_R, theme::CARD_FILL);
        selected_outline(band, rect, theme::CHIP_R);
        (theme::TEXT_PRIMARY, theme::CARD_FILL)
    } else {
        outline(band, rect, theme::CHIP_R, theme::OUTLINE, 1);
        (theme::TEXT_SECONDARY, theme::CANVAS)
    };
    let (cx, cy) = rect.center();
    let max_w = rect.width() - 8;
    font::draw(band, font, cx, centred_baseline(font, cy), text, fg, Some(bg), max_w, Align::Center);
}

/// Fully rounded tag with a leading dot (LISTENING). The label is tracked
/// caps; when that does not fit the pill it drops to the keycap face.
pub fn pill(band: &mut Band, rect: Rect, text: &str, fg: u32, fill_color: u32, outline_color: u32) {
    if !visible(band, rect) {
        return;
    }
    let radius = (rect.height() / 2) as u32;
    round(band, rect, radius, fill_color);
    outline(band, rect, radius, outline_color, 1);
    let (_, cy) = rect.center();
    dot(band, rect.left() + 17, cy, 4, fg);
    let x = rect.left() + 28;
    let max_w = rect.right() - 12 - x;
    let font = if font::measure(Font::Label, text) <= max_w { Font::Label } else { Font::Keycap };
    font::draw(band, font, x, centred_baseline(font, cy), text, fg, Some(fill_color), max_w, Align::Left);
}

const KEYCAP_PAD: i32 = 4;

/// Width a keycap with this text takes.
pub fn keycap_width(text: &str) -> i32 {
    font::measure(Font::Keycap, text) + 2 * KEYCAP_PAD
}

/// One keycap of the hint bar, left edge at `x`; returns its width.
pub fn keycap(band: &mut Band, x: i32, text: &str) -> i32 {
    let width = keycap_width(text);
    let x = x.clamp(-4096, 4096);
    let rect = Rect::new(x, theme::KEYCAP_Y, width, theme::KEYCAP_H);
    if visible(band, rect) {
        round(band, rect, theme::KEYCAP_R, theme::KEYCAP_FILL);
        outline(band, rect, theme::KEYCAP_R, theme::OUTLINE_KEY, 1);
        font::draw(
            band,
            Font::Keycap,
            x + KEYCAP_PAD,
            theme::HINT_BASELINE,
            text,
            theme::KEYCAP_TEXT,
            Some(theme::KEYCAP_FILL),
            width,
            Align::Left,
        );
    }
    width
}

/// One entry of the hint bar: a keycap (empty = none) and what it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hint<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

// Tight enough for the four-hint question card bar (413 of 416 px).
const HINT_KEY_GAP: i32 = 4;
const HINT_GAP: i32 = 7;

fn hint_width(hint: &Hint) -> i32 {
    let key = if hint.key.is_empty() {
        0
    } else {
        keycap_width(hint.key)
    };
    let label = font::measure(Font::Hint, hint.label);
    key + label + if key != 0 && label != 0 { HINT_KEY_GAP } else { 0 }
}

/// How many leading hints fit x in [32, 448], and their total width.
pub fn hints_that_fit(hints: &[Hint]) -> (usize, i32) {
    let mut total = 0;
    for (count, hint) in hints.iter().enumerate() {
        let next = total + hint_width(hint) + if count == 0 { 0 } else { HINT_GAP };
        if next > layout::HINT_CONTENT.width() {
            return (count, total);
        }
        total = next;
    }
    (hints.len(), total)
}

/// The bottom bar: fill, hairline, and the hints centred. Trailing hints that
/// do not fit are dropped, never squeezed or clipped; when not even the first
/// fits, it alone is drawn from the left and its label ends in "…".
pub fn hint_bar(band: &mut Band, hints: &[Hint]) {
    let lane = Rect::new(0, theme::HINT_Y - 1, theme::SCREEN, theme::SCREEN - theme::HINT_Y + 1);
    if !visible(band, lane) {
        return;
    }
    fill(band, Rect::new(0, theme::HINT_Y - 1, theme::SCREEN, 1), theme::HAIRLINE);
    fill(band, layout::HINT_BAR, theme::HINT_FILL);
    let (count, total) = hints_that_fit(hints);
    let (count, mut x) = if count == 0 && !hints.is_empty() {
        (1, layout::HINT_CONTENT.left())
    } else {
        (count, theme::SCREEN / 2 - total / 2)
    };
    for hint in hints.iter().take(count) {
        if !hint.key.is_empty() {
            x += keycap(band, x, hint.key);
            if !hint.label.is_empty() {
                x += HINT_KEY_GAP;
            }
        }
        x += font::draw(
            band,
            Font::Hint,
            x,
            theme::HINT_BASELINE,
            hint.label,
            theme::TEXT_LABEL,
            Some(theme::HINT_FILL),
            layout::HINT_CONTENT.right() - x,
            Align::Left,
        );
        x += HINT_GAP;
    }
}

/// Localize interface actions while retaining the physical keycap names.
pub fn hint_bar_localized(band: &mut Band, hints: &[Hint], language: super::i18n::Language) {
    // The deck's bars have at most five hints. Eight also bounds stack use
    // when the clipped bar is drawn once per display band.
    let mut translated = [Hint { key: "", label: "" }; 8];
    let count = hints.len().min(translated.len());
    for (out, hint) in translated.iter_mut().zip(hints) {
        *out = Hint { key: hint.key, label: language.text(hint.label) };
    }
    hint_bar(band, &translated[..count]);
}

/// Right side of the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderRight<'a> {
    None,
    /// A tag: "remote", "demo", "offline", "SEP 2026", "blocked 2m".
    Text(&'a str),
    /// Minute of day, drawn "19:42".
    Clock(u16),
    /// Amber "12%": the battery is low.
    Battery(u8),
}

/// What a face's header shows on the right: a low battery (<= 15 %) displaces
/// everything, otherwise the face's own tag.
pub fn header_right<'a>(ui: &UiState, own: HeaderRight<'a>) -> HeaderRight<'a> {
    match ui.battery.percentage() {
        Some(percent) if percent <= 15 => HeaderRight::Battery(percent),
        _ => own,
    }
}

/// A face's own header tag ("remote", the usage period, the macro source):
/// the deck status ("demo"/"offline") displaces it and a low battery displaces
/// both, so every face agrees on what the right corner means.
pub fn face_tag<'a>(ui: &UiState, own: HeaderRight<'a>) -> HeaderRight<'a> {
    let own = match status_tag(ui) {
        HeaderRight::Text(status) => HeaderRight::Text(status),
        _ => own,
    };
    header_right(ui, own)
}

/// The deck-wide status tag: "demo", "offline", else the clock.
pub fn status_tag(ui: &UiState) -> HeaderRight<'static> {
    if ui.deck.demo_active() {
        HeaderRight::Text(ui.tr("demo"))
    } else if !ui.deck.host_present {
        HeaderRight::Text(ui.tr("offline"))
    } else if ui.deck.clock_valid {
        HeaderRight::Clock(ui.deck.clock_minute)
    } else {
        HeaderRight::None
    }
}

/// The state age as displayed ("· 2m", "blocked 2m"), in the unit that is
/// shown: 0 = "now" (under a minute), 1..=59 minutes, then hours, then days.
/// `draw` formats with `age_label`; `tick` reports dirty exactly when
/// `age_step` of a visible agent changes, so both sides agree on granularity
/// (at most one repaint a minute).
pub fn age_step(now_ms: u32, since_ms: u32) -> u32 {
    let age = now_ms.wrapping_sub(since_ms);
    // A stamp "ahead" of now (queued controls, host `since` rounding) is new.
    if age >= u32::MAX / 2 {
        0
    } else {
        age / 60_000
    }
}

/// "now", "2m", "3h", "12d". The 32-bit clock wraps after 49 days, so an age
/// past 24 days reads as a stamp from the future: "now".
pub fn age_label(now_ms: u32, since_ms: u32, out: &mut Text<8>) {
    age_label_localized(now_ms, since_ms, out, super::i18n::Language::English);
}

pub fn age_label_localized<const N: usize>(now_ms: u32, since_ms: u32, out: &mut Text<N>, language: super::i18n::Language) {
    let minutes = age_step(now_ms, since_ms);
    let (value, unit) = match minutes {
        0 => {
            out.set(language.text("now"));
            return;
        }
        1..=59 => (minutes, "m"),
        60..=1439 => (minutes / 60, "h"),
        _ => (minutes / 1440, "d"),
    };
    out.clear();
    if value >= 10 {
        out.push_char((b'0' + (value / 10 % 10) as u8) as char);
    }
    out.push_char((b'0' + (value % 10) as u8) as char);
    out.push_str(language.text(unit));
}

/// Header: tracked caps label on the left, tag on the right. Does not clear.
pub fn header(band: &mut Band, left_label: &str, right: HeaderRight) {
    if !visible(band, layout::HEADER) {
        return;
    }
    font::draw(
        band,
        Font::Label,
        theme::TEXT_X,
        theme::HEADER_BASELINE,
        left_label,
        theme::TEXT_LABEL,
        Some(theme::CANVAS),
        260,
        Align::Left,
    );
    header_tag(band, right);
}

/// Just the right-hand tag (views whose left side is a dot + name).
pub fn header_tag(band: &mut Band, right: HeaderRight) {
    if !visible(band, layout::HEADER) {
        return;
    }
    let digits;
    let (text, color): (&str, u32) = match right {
        HeaderRight::None => return,
        HeaderRight::Text(text) => (text, theme::TEXT_LABEL),
        HeaderRight::Clock(minute) => {
            let (h, m) = ((minute / 60 % 24) as u8, (minute % 60) as u8);
            digits = [b'0' + h / 10, b'0' + h % 10, b':', b'0' + m / 10, b'0' + m % 10];
            (core::str::from_utf8(&digits).unwrap_or(""), theme::TEXT_LABEL)
        }
        HeaderRight::Battery(percent) => {
            let percent = percent.min(100);
            digits = [
                b'0' + percent / 100,
                b'0' + percent / 10 % 10,
                b'0' + percent % 10,
                b'%',
                b' ',
            ];
            let start = if percent >= 100 { 0 } else if percent >= 10 { 1 } else { 2 };
            (core::str::from_utf8(&digits[start..4]).unwrap_or(""), theme::AMBER)
        }
    };
    font::draw(
        band,
        Font::Small,
        theme::TEXT_RIGHT,
        theme::HEADER_BASELINE,
        text,
        color,
        Some(theme::CANVAS),
        170,
        Align::Right,
    );
}

pub mod list {
    //! Scrolling window over a list that is longer than its visible slots.

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Viewport {
        /// Index of the item in slot 0.
        pub top: u8,
        pub visible: u8,
        pub count: u8,
    }

    impl Viewport {
        pub const fn new(top: u8, visible: u8, count: u8) -> Self {
            let mut viewport = Self { top, visible, count };
            viewport.top = viewport.clamped(top as i32);
            viewport
        }
        const fn clamped(&self, top: i32) -> u8 {
            let max = self.count.saturating_sub(self.visible) as i32;
            if top < 0 {
                0
            } else if top > max {
                max as u8
            } else {
                top as u8
            }
        }
        /// Scroll the least that brings `index` into view.
        pub fn follow(&mut self, index: u8) {
            let index = index as i32;
            let mut top = self.top as i32;
            if index < top {
                top = index;
            } else if index >= top + self.visible as i32 {
                top = index - self.visible as i32 + 1;
            }
            self.top = self.clamped(top);
        }
        /// Scroll by rows (+ = later items). True when it moved.
        pub fn scroll(&mut self, rows: i32) -> bool {
            let top = self.clamped(self.top as i32 + rows);
            let moved = top != self.top;
            self.top = top;
            moved
        }
        /// Item index shown in `slot`, if any.
        pub fn index_at(&self, slot: u8) -> Option<u8> {
            let index = self.top.checked_add(slot)?;
            (slot < self.visible && index < self.count).then_some(index)
        }
        /// Slot that shows item `index`, if it is in view.
        pub fn slot_of(&self, index: u8) -> Option<u8> {
            let slot = index.checked_sub(self.top)?;
            (slot < self.visible && index < self.count).then_some(slot)
        }
        /// (slot, item index) of everything in view.
        pub fn slots(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
            (0..self.visible).filter_map(|slot| Some((slot, self.index_at(slot)?)))
        }
        pub fn more_above(&self) -> bool {
            self.top > 0
        }
        pub fn more_below(&self) -> bool {
            self.top as u16 + (self.visible as u16) < self.count as u16
        }
    }
}

#[cfg(test)]
mod tests {
    use super::list::Viewport;
    use super::*;

    #[test]
    fn age_labels_change_exactly_when_the_step_does() {
        let label = |now: u32, since: u32| {
            let mut text = Text::<8>::new();
            age_label(now, since, &mut text);
            std::string::String::from(text.as_str())
        };
        assert_eq!(label(59_999, 0), "now");
        assert_eq!(label(60_000, 0), "1m");
        assert_eq!(label(0, 0u32.wrapping_sub(125_000)), "2m");
        assert_eq!(label(59 * 60_000 + 59_999, 0), "59m");
        assert_eq!(label(60 * 60_000, 0), "1h");
        assert_eq!(label(23 * 3_600_000 + 3_599_999, 0), "23h");
        assert_eq!(label(36 * 3_600_000, 0), "1d");
        assert_eq!(label(20 * 86_400_000, 0), "20d");
        // A stamp ahead of now is brand new, not 49 days old.
        assert_eq!(label(1000, 5000), "now");
        assert_eq!(age_step(1000, 5000), 0);
        assert_eq!(age_step(119_999, 0), 1);
        assert_eq!(age_step(120_000, 0), 2);
    }

    #[test]
    fn viewport_follows_scrolls_and_clamps() {
        let mut view = Viewport::new(0, 4, 6);
        view.follow(5);
        assert_eq!(view.top, 2);
        assert_eq!(view.slot_of(5), Some(3));
        assert_eq!(view.index_at(0), Some(2));
        view.follow(1);
        assert_eq!(view.top, 1);
        assert!(view.scroll(9) && view.top == 2 && !view.scroll(1));
        assert!(view.more_above() && !view.more_below());
        let short = Viewport::new(7, 4, 3);
        assert_eq!(short.top, 0);
        assert_eq!(short.slots().collect::<std::vec::Vec<_>>(), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(short.index_at(3), None);
        assert_eq!(Viewport::new(0, 4, 0).slots().count(), 0);
        assert_eq!(Viewport::new(255, 255, 255).index_at(254), Some(254));
    }

    #[test]
    fn hint_bar_drops_trailing_hints_that_do_not_fit() {
        let hints = [
            Hint { key: "SCROLL", label: "choose" },
            Hint { key: "ENTER", label: "confirm" },
            Hint { key: "PTT", label: "reply" },
            Hint { key: "CANCEL", label: "snooze" },
            Hint { key: "SHIFT SHIFT", label: "a very long trailing explanation" },
        ];
        let (count, total) = hints_that_fit(&hints);
        assert!(count >= 1 && count < hints.len());
        assert!(total <= layout::HINT_CONTENT.width());
        assert_eq!(hints_that_fit(&[]), (0, 0));
        assert_eq!(hints_that_fit(&hints[..1]).0, 1);
        // The mockup bars fit whole (no hint of the eight scenes is dropped).
        assert_eq!(hints_that_fit(&hints[..4]).0, 4);
        let roster = [
            Hint { key: "\u{2039} \u{203a}", label: "switch" },
            Hint { key: "SCROLL", label: "select" },
            Hint { key: "ENTER", label: "open" },
            Hint { key: "PTT", label: "talk" },
        ];
        assert_eq!(hints_that_fit(&roster).0, 4);
        let bare = [Hint { key: "", label: "tap or press \u{b7} dashed = touch-only key" }];
        assert_eq!(hints_that_fit(&bare), (1, hint_width(&bare[0])));
    }

    #[test]
    fn corner_coverage_is_a_one_pixel_ramp_that_mirrors() {
        let rect = Rect::new(10, 20, 100, 60);
        for r in 0..=16 {
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    let c = coverage(rect, r, x, y);
                    assert!((0..=16).contains(&c));
                    assert_eq!(c, coverage(rect, r, rect.right() - 1 - (x - rect.left()), y));
                    assert_eq!(c, coverage(rect, r, x, rect.bottom() - 1 - (y - rect.top())));
                }
            }
            // Straight edges are solid, the very corner pixel is not (r > 1).
            assert_eq!(coverage(rect, r, rect.left() + 50, rect.top()), 16);
            assert_eq!(coverage(rect, r, rect.left(), rect.top() + 30), 16);
            assert_eq!(coverage(rect, r, rect.left(), rect.top()) == 16, r == 0);
        }
        assert_eq!(coverage(rect, 8, rect.left() - 1, rect.top()), 0);
        assert_eq!(coverage(rect, 8, rect.left(), rect.top() - 1), 0);
        // Pixels the r = 14 arc crosses are partial; either side of it solid / empty.
        let mid = coverage(rect, 14, rect.left() + 3, rect.top() + 4);
        assert!((1..=15).contains(&mid), "{mid}");
        assert_eq!(coverage(rect, 14, rect.left() + 4, rect.top() + 4), 16);
        assert_eq!(coverage(rect, 14, rect.left() + 3, rect.top() + 3), 0);
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(63), 7);
        assert_eq!(isqrt(64), 8);
        assert_eq!(isqrt(u32::MAX), 65535);
    }
}
