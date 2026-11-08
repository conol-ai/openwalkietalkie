//! Usage Report face (mockup 5): a period tag in the header and one block per
//! provider (name and percent on one line, a toned bar, the reset line) in a
//! four-row viewport over `deck.usage`. "updating…" replaces the period while
//! `usage.updating`. When the list is longer than the viewport, the reset
//! line of the first / last visible block carries an "↑ N more" / "N more ↓"
//! marker on its right.
//!
//! Signature: header, one region per visible block, hint bar.
use crate::deck::msg::UsageItem;
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::layout::Rect;
use crate::ui::signature::SigBuilder;
use crate::ui::state::UiState;
use crate::ui::i18n::Language;
use crate::ui::{font, font::Align, font::Font, kit, layout, theme};

/// `layout::HEADER` with room for glyph overhang.
const HEADER_LANE: Rect = Rect::new(34, 30, 412, 32);

fn tag<'a>(ui: &'a UiState) -> kit::HeaderRight<'a> {
    let own = if ui.usage.updating {
        kit::HeaderRight::Text(ui.tr("updating\u{2026}"))
    } else if ui.deck.usage.period.is_empty() {
        kit::HeaderRight::None
    } else {
        kit::HeaderRight::Text(ui.deck.usage.period.as_str())
    };
    kit::face_tag(ui, own)
}

fn viewport(ui: &UiState) -> kit::list::Viewport {
    kit::list::Viewport::new(ui.usage.top_row, layout::USAGE_VISIBLE, ui.deck.usage.count.min(6))
}

/// The marker on the reset line of `slot`, if any: rows hidden above on the
/// first slot, hidden below on the last.
fn hidden(viewport: &kit::list::Viewport, slot: u8) -> Option<(u8, bool)> {
    if slot == 0 && viewport.more_above() {
        Some((viewport.top, false))
    } else if slot + 1 == viewport.visible && viewport.more_below() {
        Some((viewport.count - viewport.top - viewport.visible, true))
    } else {
        None
    }
}

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

fn percent_label(percent: u8, out: &mut Text<8>) {
    let percent = percent.min(100);
    out.clear();
    if percent >= 100 {
        out.push_char('1');
    }
    if percent >= 10 {
        out.push_char((b'0' + percent / 10 % 10) as char);
    }
    out.push_char((b'0' + percent % 10) as char);
    out.push_char('%');
}

fn block(band: &mut Band, rect: Rect, item: &UsageItem, marker: Option<(u8, bool)>, connected: bool, language: Language) {
    let mut text = Text::<24>::new();
    let baseline = rect.top() + layout::USAGE_NAME_BASELINE;
    let mut percent = Text::<8>::new();
    percent_label(item.percent, &mut percent);
    let percent_w = font::measure(Font::Name, percent.as_str());
    font::draw(band, Font::Name, theme::TEXT_X, baseline, item.name.as_str(), theme::TEXT_PRIMARY, Some(theme::CANVAS),
        theme::TEXT_RIGHT - percent_w - 12 - theme::TEXT_X, Align::Left);
    font::draw(band, Font::Name, theme::TEXT_RIGHT, baseline, percent.as_str(), theme::TEXT_PRIMARY, Some(theme::CANVAS), 100, Align::Right);
    let tip = connected && !item.reset_tip.is_empty();
    let account_offset = if item.account.is_empty() { 0 } else { 12 };
    if !item.account.is_empty() {
        font::draw(band, if tip { Font::Hint } else { Font::Small }, theme::TEXT_X, rect.top() + if tip { 38 } else { 40 }, item.account.as_str(), theme::TEXT_LABEL,
            Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Left);
    }
    let color = item.tone.map_or(theme::usage_tone(item.percent), theme::tone);
    let bar = if tip {
        Rect::new(layout::USAGE_BAR.left(), if account_offset == 0 { 35 } else { 45 }, layout::USAGE_BAR.width(), 5)
    } else { layout::USAGE_BAR.offset(0, account_offset) };
    kit::bar(band, bar.offset(rect.left(), rect.top()), item.percent, color);
    let reset_baseline = rect.top() + if tip { 64 } else { layout::USAGE_RESET_BASELINE + account_offset };
    let mut reset_w = theme::TEXT_RIGHT - theme::TEXT_X;
    if let Some((count, below)) = marker {
        more(count, below, language, &mut text);
        reset_w -= font::draw(band, Font::Small, theme::TEXT_RIGHT, reset_baseline, text.as_str(), theme::TEXT_FAINT, Some(theme::CANVAS), 120, Align::Right) + 12;
    }
    font::draw(band, if tip { Font::Hint } else { Font::Body }, theme::TEXT_X, reset_baseline, item.reset.as_str(), theme::TEXT_LABEL, Some(theme::CANVAS), reset_w, Align::Left);
    if tip {
        font::draw(band, Font::Hint, theme::TEXT_X, rect.top() + 79, item.reset_tip.as_str(), theme::AMBER,
            Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Left);
    }
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    let _ = instance;
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("USAGE"), tag(ui));
    let viewport = viewport(ui);
    if viewport.count == 0 {
        let rect = layout::usage_row(0);
        if kit::visible(band, rect) {
            font::draw(band, Font::Row, theme::TEXT_X, rect.top() + layout::USAGE_NAME_BASELINE, ui.tr("no usage data"), theme::TEXT_FAINT,
                Some(theme::CANVAS), 300, Align::Left);
        }
    }
    for (slot, row) in viewport.slots() {
        let rect = layout::usage_row(slot);
        let Some(item) = ui.deck.usage.items.get(row as usize) else {
            continue;
        };
        if kit::visible(band, rect) {
            block(band, rect, item, hidden(&viewport, slot), ui.deck.host_present, ui.language);
        }
    }
    kit::hint_bar_localized(band, HINTS, ui.language);
}

const HINTS: &[kit::Hint] = &[
    kit::Hint { key: "SCROLL", label: "providers" },
    kit::Hint { key: "ENTER", label: "refresh" },
];

/// Regions of this face and the inputs of their pixels; see `signature.rs`.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    let _ = instance;
    sig.region_with(HEADER_LANE, |h| {
        h.header_right(tag(ui));
    });
    let viewport = viewport(ui);
    for slot in 0..layout::USAGE_VISIBLE {
        sig.region_with(layout::usage_row(slot), |h| {
            let item = viewport.index_at(slot).and_then(|row| ui.deck.usage.items.get(row as usize));
            let Some(item) = item else {
                h.u8(0).bool(slot == 0 && viewport.count == 0);
                return;
            };
            h.u8(1)
                .text(&item.name)
                .text(&item.account)
                .u8(item.percent)
                .text(&item.reset)
                .str(if ui.deck.host_present { item.reset_tip.as_str() } else { "" })
                .u8(item.tone.map_or(255, |tone| tone as u8));
            let (count, below) = hidden(&viewport, slot).unwrap_or((0, false));
            h.u8(count).bool(below);
        });
    }
    sig.region(layout::HINT_BAR, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kit::list::Viewport;

    #[test]
    fn percent_labels_clamp_at_100() {
        let label = |p: u8| {
            let mut text = Text::<8>::new();
            percent_label(p, &mut text);
            std::string::String::from(text.as_str())
        };
        assert_eq!(label(0), "0%");
        assert_eq!(label(7), "7%");
        assert_eq!(label(62), "62%");
        assert_eq!(label(100), "100%");
        assert_eq!(label(255), "100%");
    }

    #[test]
    fn markers_sit_on_the_first_and_last_visible_blocks_only() {
        let view = Viewport::new(1, 4, 6);
        assert_eq!(hidden(&view, 0), Some((1, false)));
        assert_eq!(hidden(&view, 1), None);
        assert_eq!(hidden(&view, 3), Some((1, true)));
        let top = Viewport::new(0, 4, 6);
        assert_eq!(hidden(&top, 0), None);
        assert_eq!(hidden(&top, 3), Some((2, true)));
        let short = Viewport::new(0, 4, 3);
        assert!((0..4).all(|slot| hidden(&short, slot).is_none()));
        let mut text = Text::<24>::new();
        more(2, true, Language::English, &mut text);
        assert_eq!(text.as_str(), "2 more \u{2193}");
        more(1, false, Language::English, &mut text);
        assert_eq!(text.as_str(), "\u{2191} 1 more");
    }

    #[test]
    fn the_tag_prefers_status_then_updating_then_the_period() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.host_present = true;
        assert_eq!(tag(&ui), kit::HeaderRight::Text("SEP 2026"));
        ui.usage.updating = true;
        assert_eq!(tag(&ui), kit::HeaderRight::Text("updating\u{2026}"));
        ui.deck.host_present = false;
        assert_eq!(tag(&ui), kit::status_tag(&ui));
        ui.deck.settings.demo = false;
        ui.usage.updating = false;
        assert_eq!(tag(&ui), kit::HeaderRight::Text("offline"));
        ui.deck.host_present = true;
        ui.deck.usage.period.clear();
        assert_eq!(tag(&ui), kit::HeaderRight::None);
    }
}
