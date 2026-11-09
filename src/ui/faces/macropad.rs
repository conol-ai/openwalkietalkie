//! Macropad face (mockup 4): the 3x3 key grid of the `MacroSet` keyed by this
//! strip entry. A key with a keystroke (or a hardware binding) is a solid
//! card with its label and a tiny glyph in its top-right corner: the Mac
//! modifiers and key it sends (`\u{2318}\u{21e7}4`), else the hardware
//! control that presses it; a key with neither is dashed with a dimmer
//! label (touch-only); an absent slot is the dashed "+ assign". A key in
//! `macropad.down` (held or flashing) draws with the lighter fill and its
//! label sunk two pixels.
//!
//! The grid cursor of the deck's own key editor (`state::macropad`,
//! SHIFT+CANCEL) outlines one key and swaps the hint bar.
//!
//! Signature: one region per key hashing that key's label, binding, presence,
//! pressed bit and cursor, so a flash repaints one key.
use crate::deck::msg::{Binding, MacroKey};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::layout::Rect;
use crate::ui::signature::SigBuilder;
use crate::ui::state::UiState;
use crate::ui::i18n::Language;
use crate::ui::{font, font::Align, font::Font, kit, layout, theme};

/// `layout::HEADER` with room for glyph overhang.
const HEADER_LANE: Rect = Rect::new(34, 30, 412, 32);
/// The pressed label sinks this much.
const SINK: i32 = 2;
/// Binding glyph: right edge and baseline inside the key.
const GLYPH_RIGHT: i32 = 10;
const GLYPH_BASELINE: i32 = 19;

/// What the corner glyph says for each hardware binding.
pub const fn binding_glyph(binding: Binding) -> &'static str {
    match binding {
        Binding::None => "",
        Binding::Left => "\u{2039}",
        Binding::Right => "\u{203a}",
        Binding::Enter => "\u{21b5}",
        Binding::Cw => "CW",
        Binding::Ccw => "CCW",
        Binding::ShiftLeft => "SHIFT \u{2039}",
        Binding::ShiftRight => "SHIFT \u{203a}",
        Binding::ShiftEnter => "SHIFT \u{21b5}",
    }
}

fn tag<'a>(ui: &'a UiState, instance: u8) -> kit::HeaderRight<'a> {
    let own = match ui.deck.macro_set(instance) {
        Some(set) if !set.source.is_empty() => kit::HeaderRight::Text(set.source.as_str()),
        _ => kit::HeaderRight::None,
    };
    kit::face_tag(ui, own)
}

/// The key in slot `index` of this face's set; absent slots read as `EMPTY`.
fn key(ui: &UiState, instance: u8, index: u8) -> MacroKey {
    ui.deck
        .macro_set(instance)
        .and_then(|set| set.keys.get(index as usize).copied())
        .filter(|key| key.present)
        .unwrap_or(MacroKey::EMPTY)
}

fn pressed(ui: &UiState, instance: u8, index: u8) -> bool {
    ui.macropad.face == instance && ui.macropad.down & (1 << (index & 15)) != 0
}

fn draw_key(band: &mut Band, index: u8, key: &MacroKey, pressed: bool, cursor: bool, language: Language) {
    draw_key_in_localized(band, layout::macro_key(index), key, pressed, cursor, language);
}

/// One key card in `rect`, as the grid draws it; the editor previews a
/// draft with it.
pub fn draw_key_in(band: &mut Band, rect: Rect, key: &MacroKey, pressed: bool, cursor: bool) {
    draw_key_in_localized(band, rect, key, pressed, cursor, Language::English);
}

pub fn draw_key_in_localized(band: &mut Band, rect: Rect, key: &MacroKey, pressed: bool, cursor: bool, language: Language) {
    if !kit::visible(band, rect) {
        return;
    }
    let bound = key.present && (key.keys.is_some() || key.binding != Binding::None);
    let bg = if pressed {
        kit::round(band, rect, theme::KEY_R, theme::CHIP_FILL);
        theme::CHIP_FILL
    } else if bound {
        kit::round(band, rect, theme::KEY_R, theme::CARD_FILL);
        theme::CARD_FILL
    } else {
        theme::CANVAS
    };
    if bound {
        kit::outline(band, rect, theme::KEY_R, theme::OUTLINE_KEY, 1);
    } else {
        kit::dashed(band, rect, theme::KEY_R, theme::OUTLINE_KEY, bg);
    }
    let sink = if pressed { SINK } else { 0 };
    let (cx, cy) = rect.center();
    let label = if key.present { key.label.as_str() } else { language.text("+ assign") };
    let fg = if bound { theme::TEXT_BRIGHT } else { theme::TEXT_LABEL };
    let baseline = cy + font::ascent(Font::Macro) * 9 / 20 + sink;
    font::draw(band, Font::Macro, cx, baseline, label, fg, Some(bg), rect.width() - 16, Align::Center);
    if bound {
        let mut combo = Text::<16>::new();
        let glyph = if key.keys.is_some() {
            key.keys.shown(&mut combo);
            combo.as_str()
        } else {
            binding_glyph(key.binding)
        };
        font::draw(band, Font::Keycap, rect.right() - GLYPH_RIGHT, rect.top() + GLYPH_BASELINE + sink, glyph,
            theme::TEXT_FAINT, Some(bg), rect.width() - 20, Align::Right);
    }
    if cursor {
        kit::outline(band, rect, theme::KEY_R, theme::OUTLINE_SELECTED, 2);
    }
}

fn cursor_at(ui: &UiState, instance: u8, index: u8) -> bool {
    ui.macropad.cursor == Some((instance, index))
}

fn hints(ui: &UiState, instance: u8) -> &'static [kit::Hint<'static>] {
    if ui.macropad.cursor.is_some_and(|(face, _)| face == instance) {
        CURSOR_HINTS
    } else {
        HINTS
    }
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id
/// and selects the `MacroSet`.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("MACROPAD"), tag(ui, instance));
    for index in 0..9u8 {
        draw_key(band, index, &key(ui, instance, index), pressed(ui, instance, index), cursor_at(ui, instance, index), ui.language);
    }
    kit::hint_bar_localized(band, hints(ui, instance), ui.language);
}

// Plain text, as the mockup's bar: no keycaps.
const HINTS: &[kit::Hint] = &[kit::Hint { key: "", label: "tap or press \u{b7} SHIFT+CANCEL edits the keys" }];
const CURSOR_HINTS: &[kit::Hint] = &[
    kit::Hint { key: "SCROLL", label: "pick" },
    kit::Hint { key: "ENTER", label: "edit" },
    kit::Hint { key: "CANCEL", label: "done" },
];

/// Regions of this face and the inputs of their pixels; see `signature.rs`.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    sig.region_with(HEADER_LANE, |h| {
        h.header_right(tag(ui, instance));
    });
    for index in 0..9u8 {
        let key = key(ui, instance, index);
        sig.region_with(layout::macro_key(index), |h| {
            h.bool(key.present)
                .u8(key.binding as u8)
                .u8(key.keys.mods)
                .u8(key.keys.key)
                .text(&key.label)
                .bool(pressed(ui, instance, index))
                .bool(cursor_at(ui, instance, index));
        });
    }
    sig.region(layout::HINT_BAR, u32::from(ui.macropad.cursor.is_some_and(|(face, _)| face == instance)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_has_a_short_glyph_and_none_has_none() {
        let all = [
            Binding::Left,
            Binding::Right,
            Binding::Enter,
            Binding::Cw,
            Binding::Ccw,
            Binding::ShiftLeft,
            Binding::ShiftRight,
            Binding::ShiftEnter,
        ];
        for binding in all {
            let glyph = binding_glyph(binding);
            assert!(!glyph.is_empty());
            // Fits beside the label in the key's top corner, inside the arc.
            assert!(font::measure(Font::Keycap, glyph) <= layout::macro_key(0).width() / 2, "{glyph}");
        }
        assert_eq!(binding_glyph(Binding::None), "");
        // The widest keystroke fits beside the label too.
        let mut combo = Text::<16>::new();
        crate::deck::msg::Keystroke::parse("ctrl+opt+shift+cmd+pagedown").unwrap().shown(&mut combo);
        assert!(font::measure(Font::Keycap, combo.as_str()) <= layout::macro_key(0).width() - 20, "{}", combo.as_str());
        let glyphs: std::vec::Vec<_> = all.iter().map(|&b| binding_glyph(b)).collect();
        for (i, a) in glyphs.iter().enumerate() {
            assert!(!glyphs[i + 1..].contains(a), "{a} twice");
        }
    }

    #[test]
    fn a_key_reads_from_the_set_of_its_own_face_only() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        let face = crate::deck::demo::MACRO_FACE;
        assert_eq!(key(&ui, face, 0).label.as_str(), "build");
        assert!(!key(&ui, face, 8).present);
        assert!(!key(&ui, face + 1, 0).present);
        assert!(!key(&ui, face, 9).present);
        ui.macropad.face = face;
        ui.macropad.down = 1 << 4;
        assert!(pressed(&ui, face, 4) && !pressed(&ui, face, 3) && !pressed(&ui, face + 1, 4));
    }
}
