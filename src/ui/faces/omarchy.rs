//! Omarchy Remote face (mockup 3): "Workspace N" over the workspace chips,
//! then the window list in a four-row viewport. Everything comes from
//! `deck.workspaces` and the pub fields of `state::omarchy::View`; the pressed
//! looks are the recogniser's latched chip / window row.
//!
//! Signature: one region per fixed lane (header, title, chip row, WINDOWS
//! line, each visible window row, the "more" line, hint bar), each hashing
//! exactly what its pixels show, so a wheel step repaints two rows.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::layout::Rect;
use crate::ui::signature::SigBuilder;
use crate::ui::state::UiState;
use crate::ui::i18n::Language;
use crate::ui::{font, font::Align, font::Font, kit, layout, theme};

/// Chip row: every chip rect, chips 0..9.
const CHIPS: Rect = Rect::new(37, 125, 9 * 42 - 8, 34);
/// "N more ↓" under the last row; "↑" shares the WINDOWS line.
const MORE_BELOW: Rect = Rect::new(32, 396, 416, 30);
const MORE_BELOW_BASELINE: i32 = 416;
/// Descenders of the Display title reach past `OMARCHY_TITLE`.
const TITLE: Rect = Rect::new(37, 66, 406, 54);
/// `layout::HEADER` with room for glyph overhang.
const HEADER_LANE: Rect = Rect::new(34, 30, 412, 32);
const WINDOWS_LINE: Rect = Rect::new(32, 176, 416, 24);

/// The header tag: the deck-wide status ("demo", "offline") displaces
/// "remote", and a low battery displaces everything.
fn tag(ui: &UiState) -> kit::HeaderRight<'static> {
    kit::face_tag(ui, kit::HeaderRight::Text(ui.tr("remote")))
}

fn title(ui: &UiState, out: &mut Text<32>) {
    out.set(ui.tr("Workspace "));
    match ui.deck.workspaces.active {
        0 => {
            out.push_char('\u{2014}');
        }
        n => {
            if n >= 100 {
                out.push_char((b'0' + n / 100) as char);
            }
            if n >= 10 {
                out.push_char((b'0' + n / 10 % 10) as char);
            }
            out.push_char((b'0' + n % 10) as char);
        }
    }
}

fn viewport(ui: &UiState) -> kit::list::Viewport {
    let ws = &ui.deck.workspaces;
    kit::list::Viewport::new(ui.omarchy.top_row, layout::OMARCHY_VISIBLE, ws.window_count.min(6))
}

/// "2 more ↓" / "↑ 1 more".
fn more(count: u8, below: bool, language: Language, out: &mut Text<24>) {
    out.clear();
    if !below {
        out.push_str("\u{2191} ");
    }
    if count >= 10 {
        out.push_char((b'0' + count / 10 % 10) as char);
    }
    out.push_char((b'0' + count % 10) as char);
    out.push_str(language.text(" more"));
    if below {
        out.push_str(" \u{2193}");
    }
}

fn chip(band: &mut Band, index: u8, selected: bool, pressed: bool) {
    let rect = layout::omarchy_chip(index);
    if !kit::visible(band, rect) {
        return;
    }
    let digit = [b'1' + index];
    let text = core::str::from_utf8(&digit).unwrap_or("");
    if !pressed {
        kit::chip(band, rect, text, selected);
        return;
    }
    // Pressed: the lighter fill under the chip's own outline.
    kit::round(band, rect, theme::CHIP_R, theme::CHIP_FILL);
    let outline = if selected { theme::OUTLINE_SELECTED } else { theme::OUTLINE };
    kit::outline(band, rect, theme::CHIP_R, outline, 1);
    let (cx, cy) = rect.center();
    let baseline = cy + font::ascent(Font::Row) * 9 / 20;
    font::draw(band, Font::Row, cx, baseline, text, theme::TEXT_PRIMARY, Some(theme::CHIP_FILL), rect.width() - 8, Align::Center);
}

fn window_row(band: &mut Band, ui: &UiState, slot: u8, row: u8) {
    let rect = layout::omarchy_window(slot);
    if !kit::visible(band, rect) {
        return;
    }
    let Some(window) = ui.deck.workspaces.windows.get(row as usize) else {
        return;
    };
    let selected = row == ui.omarchy.selected_window;
    let pressed = ui.omarchy.pressed_row == Some(row);
    let (fg, bg) = if pressed {
        kit::round(band, rect, theme::CARD_R, theme::CHIP_FILL);
        let outline = if selected { theme::OUTLINE_SELECTED } else { theme::OUTLINE };
        kit::outline(band, rect, theme::CARD_R, outline, 1);
        (theme::TEXT_PRIMARY, theme::CHIP_FILL)
    } else if selected {
        kit::card(band, rect, true, false);
        (theme::TEXT_PRIMARY, theme::CARD_FILL)
    } else {
        (theme::TEXT_ROW, theme::CANVAS)
    };
    kit::round(band, layout::WINDOW_SQUARE.offset(rect.left(), rect.top()), 2, theme::tone(window.tone));
    let x = rect.left() + layout::WINDOW_TEXT_X;
    font::draw(band, Font::Row, x, rect.top() + layout::WINDOW_BASELINE, window.title.as_str(), fg, Some(bg), rect.right() - 10 - x, Align::Left);
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    let _ = instance;
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("OMARCHY"), tag(ui));
    let ws = &ui.deck.workspaces;
    if kit::visible(band, TITLE) {
        let mut text = Text::<32>::new();
        title(ui, &mut text);
        let size = if font::measure(Font::Display, text.as_str()) > layout::OMARCHY_TITLE.width() { Font::Title } else { Font::Display };
        font::draw(band, size, layout::OMARCHY_TITLE.left(), layout::OMARCHY_TITLE_BASELINE, text.as_str(),
            theme::TEXT_PRIMARY, Some(theme::CANVAS), layout::OMARCHY_TITLE.width(), Align::Left);
    }
    if kit::visible(band, CHIPS) {
        for index in 0..ws.count.min(layout::OMARCHY_CHIPS) {
            chip(band, index, index + 1 == ws.active, ui.omarchy.pressed_chip == Some(index));
        }
    }
    let viewport = viewport(ui);
    if kit::visible(band, WINDOWS_LINE) {
        let x = layout::OMARCHY_WINDOWS_LABEL.left();
        font::draw(band, Font::Label, x, layout::OMARCHY_WINDOWS_BASELINE, ui.tr("WINDOWS"), theme::TEXT_FAINT, Some(theme::CANVAS),
            layout::OMARCHY_WINDOWS_LABEL.width(), Align::Left);
        if viewport.more_above() {
            let mut text = Text::<24>::new();
            more(viewport.top, false, ui.language, &mut text);
            font::draw(band, Font::Small, theme::TEXT_RIGHT, layout::OMARCHY_WINDOWS_BASELINE, text.as_str(), theme::TEXT_FAINT,
                Some(theme::CANVAS), 120, Align::Right);
        }
    }
    if viewport.count == 0 {
        let rect = layout::omarchy_window(0);
        if kit::visible(band, rect) {
            font::draw(band, Font::Row, rect.left() + layout::WINDOW_TEXT_X, rect.top() + layout::WINDOW_BASELINE, ui.tr("no windows"),
                theme::TEXT_FAINT, Some(theme::CANVAS), 300, Align::Left);
        }
    }
    for (slot, row) in viewport.slots() {
        window_row(band, ui, slot, row);
    }
    if viewport.more_below() && kit::visible(band, MORE_BELOW) {
        let mut text = Text::<24>::new();
        more(viewport.count - viewport.top - viewport.visible, true, ui.language, &mut text);
        font::draw(band, Font::Small, theme::TEXT_RIGHT, MORE_BELOW_BASELINE, text.as_str(), theme::TEXT_FAINT, Some(theme::CANVAS), 120, Align::Right);
    }
    kit::hint_bar_localized(band, HINTS, ui.language);
}

const HINTS: &[kit::Hint] = &[
    kit::Hint { key: "\u{2039} \u{203a}", label: "workspace" },
    kit::Hint { key: "SCROLL", label: "windows" },
    kit::Hint { key: "ENTER", label: "terminal" },
];

/// Regions of this face and the inputs of their pixels; see `signature.rs`.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    let _ = instance;
    let ws = &ui.deck.workspaces;
    sig.region_with(HEADER_LANE, |h| {
        h.header_right(tag(ui));
    });
    sig.region_with(TITLE, |h| {
        h.u8(ws.active);
    });
    sig.region_with(CHIPS, |h| {
        h.u8(ws.count.min(layout::OMARCHY_CHIPS)).u8(ws.active).u8(ui.omarchy.pressed_chip.map_or(255, |c| c.min(254)));
    });
    let viewport = viewport(ui);
    sig.region_with(WINDOWS_LINE, |h| {
        h.u8(if viewport.more_above() { viewport.top } else { 0 });
    });
    for slot in 0..layout::OMARCHY_VISIBLE {
        sig.region_with(layout::omarchy_window(slot), |h| {
            let Some(row) = viewport.index_at(slot) else {
                h.u8(0).bool(slot == 0 && viewport.count == 0);
                return;
            };
            let Some(window) = ws.windows.get(row as usize) else {
                h.u8(0);
                return;
            };
            h.u8(1)
                .text(&window.title)
                .u8(window.tone as u8)
                .bool(row == ui.omarchy.selected_window)
                .bool(ui.omarchy.pressed_row == Some(row));
        });
    }
    sig.region_with(MORE_BELOW, |h| {
        h.u8(if viewport.more_below() { viewport.count - viewport.top - viewport.visible } else { 0 });
    });
    sig.region(layout::HINT_BAR, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_active_workspace_or_none() {
        let mut ui = std::boxed::Box::new(UiState::new());
        let mut text = Text::<32>::new();
        ui.deck.workspaces.active = 2;
        title(&ui, &mut text);
        assert_eq!(text.as_str(), "Workspace 2");
        ui.deck.workspaces.active = 0;
        title(&ui, &mut text);
        assert_eq!(text.as_str(), "Workspace \u{2014}");
        ui.deck.workspaces.active = 12;
        title(&ui, &mut text);
        assert_eq!(text.as_str(), "Workspace 12");
        ui.deck.workspaces.active = 255;
        title(&ui, &mut text);
        assert_eq!(text.as_str(), "Workspace 255");
    }

    #[test]
    fn more_markers_count_the_hidden_rows() {
        let mut text = Text::<24>::new();
        more(2, true, Language::English, &mut text);
        assert_eq!(text.as_str(), "2 more \u{2193}");
        more(1, false, Language::English, &mut text);
        assert_eq!(text.as_str(), "\u{2191} 1 more");
        more(12, true, Language::English, &mut text);
        assert_eq!(text.as_str(), "12 more \u{2193}");
    }

    #[test]
    fn translated_workspace_titles_and_markers_keep_numbers_and_suffixes() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.deck.workspaces.active = 255;
        for language in Language::ALL {
            ui.language = language;
            let mut text = Text::<32>::new();
            title(&ui, &mut text);
            assert!(text.as_str().ends_with("255"), "{language:?}: {}", text.as_str());
            assert!(font::measure(Font::Title, text.as_str()) <= layout::OMARCHY_TITLE.width());
            let mut marker = Text::<24>::new();
            more(12, true, language, &mut marker);
            assert!(marker.as_str().starts_with("12") && marker.as_str().ends_with(" \u{2193}"));
            assert!(marker.as_str().contains(language.text(" more")));
        }
    }

    #[test]
    fn the_lanes_cover_their_layout_rects_and_stay_in_the_safe_area() {
        for rect in [HEADER_LANE, TITLE, CHIPS, WINDOWS_LINE, MORE_BELOW] {
            assert!(layout::in_safe_area(&rect), "{rect:?}");
        }
        assert_eq!(layout::HEADER.intersection(&HEADER_LANE), layout::HEADER);
        assert_eq!(layout::OMARCHY_TITLE.intersection(&TITLE), layout::OMARCHY_TITLE);
        assert_eq!(layout::OMARCHY_WINDOWS_LABEL.intersection(&WINDOWS_LINE), layout::OMARCHY_WINDOWS_LABEL);
        for index in 0..layout::OMARCHY_CHIPS {
            let chip = layout::omarchy_chip(index);
            assert_eq!(chip.intersection(&CHIPS), chip);
        }
        assert!(!MORE_BELOW.intersects(&layout::omarchy_window(3)) && !MORE_BELOW.intersects(&layout::HINT_BAR));
    }
}
