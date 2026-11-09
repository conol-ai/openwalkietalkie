//! The Macropad key editor (`Overlay::MacroEdit`), opaque: EDIT KEY header
//! with the slot number, the key previewed as it will look on the face,
//! then its rows: Label (spoken over PTT), Modifiers and Key (SCROLL
//! through them while adjusting: the keystroke the host posts), Save, and
//! Remove key for a slot that had one. The grammar lives in
//! `state::macropad`; this file only draws what `View::edit` holds.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::faces::macropad::draw_key_in_localized;
use crate::ui::kit::{self, HeaderRight, Hint};
use crate::ui::overlay::notify::header_inputs;
use crate::ui::signature::SigBuilder;
use crate::deck::msg::Keystroke;
use crate::ui::state::macropad::{edit_rows, Edit, ROW_KEY, ROW_LABEL, ROW_MODS, ROW_REMOVE, ROW_SAVE};
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, layout, layout::Rect, theme};

const EMPTY_LABEL_HINT: &str = "hold PTT, say it";
const NEEDS_LABEL: &str = "say a label first";

/// Label text and value text of a row.
fn row_texts(edit: &Edit, row: u8, language: crate::ui::i18n::Language) -> (&'static str, Text<48>) {
    let mut value = Text::new();
    let label = match row {
        ROW_LABEL => {
            value.set(if edit.label.is_empty() {
                language.text(if edit.needs_label { NEEDS_LABEL } else { EMPTY_LABEL_HINT })
            } else {
                edit.label.as_str()
            });
            "Label"
        }
        ROW_MODS => {
            Keystroke { mods: edit.keys.mods, key: 0 }.shown(&mut value);
            if value.is_empty() {
                value.set(language.text("none"));
            }
            "Modifiers"
        }
        ROW_KEY => {
            value.set(if edit.keys.is_some() { edit.keys.shown_key() } else { language.text("none") });
            "Key"
        }
        ROW_SAVE => "Save",
        _ => "Remove key",
    };
    (language.text(label), value)
}

fn hints(edit: &Edit) -> [Hint<'static>; 3] {
    if edit.adjusting {
        [
            Hint { key: "SCROLL", label: if edit.row == ROW_KEY { "key \u{b7} LEFT/RIGHT by ten" } else { "modifiers" } },
            Hint { key: "ENTER", label: "done" },
            Hint { key: "", label: "" },
        ]
    } else {
        [
            Hint { key: "PTT", label: "say the label" },
            Hint { key: "ENTER", label: "act" },
            Hint { key: "CANCEL", label: "back" },
        ]
    }
}

fn hint_bar(band: &mut Band, hints: &[Hint], language: crate::ui::i18n::Language) {
    let count = hints.iter().take_while(|hint| !hint.key.is_empty() || !hint.label.is_empty()).count();
    kit::hint_bar_localized(band, hints.get(..count).unwrap_or(&[]), language);
}

fn slot_tag(edit: &Edit, out: &mut Text<16>, language: crate::ui::i18n::Language) {
    out.set(language.text("key "));
    out.push_char((b'1' + edit.index.min(8)) as char);
}

fn draw_row(band: &mut Band, rect: Rect, edit: &Edit, row: u8, language: crate::ui::i18n::Language) {
    let focused = row == edit.row;
    let adjusting = focused && edit.adjusting;
    let pressed = edit.pressed == Some(row);
    let (fill, label_color) = if adjusting {
        kit::round(band, rect, theme::CARD_R, theme::CARD_FILL);
        kit::outline(band, rect, theme::CARD_R, theme::BLUE, 2);
        (theme::CARD_FILL, theme::TEXT_PRIMARY)
    } else if pressed {
        kit::round(band, rect, theme::CARD_R, theme::CHIP_FILL);
        kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_SELECTED, 1);
        (theme::CHIP_FILL, theme::TEXT_PRIMARY)
    } else if focused {
        kit::card(band, rect, true, false);
        (theme::CARD_FILL, theme::TEXT_PRIMARY)
    } else {
        kit::card(band, rect, false, false);
        (theme::CANVAS, theme::TEXT_ROW)
    };
    let (label, value) = row_texts(edit, row, language);
    let baseline = rect.top() + 32;
    let label_color = if row == ROW_REMOVE { theme::RED } else { label_color };
    font::draw(band, Font::Row, rect.left() + layout::SETTING_LABEL_X, baseline, label, label_color, Some(fill), 180, Align::Left);
    let value_color = match row {
        ROW_LABEL if edit.label.is_empty() && edit.needs_label => theme::AMBER,
        ROW_LABEL if edit.label.is_empty() => theme::TEXT_FAINT,
        _ => theme::TEXT_ROW,
    };
    font::draw(band, Font::Row, rect.left() + layout::SETTING_VALUE_RIGHT, baseline, value.as_str(), value_color, Some(fill), 210, Align::Right);
}

pub fn draw(band: &mut Band, ui: &UiState) {
    band.fill(kit::rgb(theme::CANVAS));
    let edit = &ui.macropad.edit;
    let mut tag = Text::new();
    slot_tag(edit, &mut tag, ui.language);
    kit::header(band, ui.tr("EDIT KEY"), kit::header_right(ui, HeaderRight::Text(tag.as_str())));
    // The key as the face will draw it: present once it has a label.
    let key = crate::deck::msg::MacroKey {
        label: edit.label,
        binding: edit.binding,
        keys: edit.keys,
        present: !edit.label.is_empty(),
    };
    draw_key_in_localized(band, layout::MACRO_EDIT_PREVIEW, &key, false, false, ui.language);
    for row in 0..edit_rows(edit) {
        let rect = layout::macro_edit_row(row);
        if kit::visible(band, rect) {
            draw_row(band, rect, edit, row, ui.language);
        }
    }
    hint_bar(band, &hints(edit), ui.language);
}

pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    let edit = &ui.macropad.edit;
    sig.mode(u32::from(edit_rows(edit)));
    sig.region_with(layout::HEADER, |h| {
        header_inputs(ui, h);
        h.u8(edit.index);
    });
    sig.region_with(layout::MACRO_EDIT_PREVIEW, |h| {
        h.text(&edit.label).u8(edit.binding as u8).u8(edit.keys.mods).u8(edit.keys.key);
    });
    for row in 0..edit_rows(edit) {
        sig.region_with(layout::macro_edit_row(row), |h| {
            let (label, value) = row_texts(edit, row, ui.language);
            h.str(label)
                .text(&value)
                .bool(row == edit.row)
                .bool(row == edit.row && edit.adjusting)
                .bool(edit.pressed == Some(row))
                .bool(edit.needs_label);
        });
    }
    // The adjusting hint names the row (the Key row adds LEFT/RIGHT).
    sig.region_with(layout::HINT_BAR, |h| {
        h.bool(edit.adjusting).u8(if edit.adjusting { edit.row } else { 0 });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::msg::KEYS;
    use crate::ui::state::macropad::MOD_COMBOS;

    #[test]
    fn every_row_string_fits_and_the_rows_clear_the_hint_bar() {
        let mut value = Text::<24>::new();
        for mods in MOD_COMBOS {
            Keystroke { mods, key: 0 }.shown(&mut value);
            assert!(font::measure(Font::Row, value.as_str()) <= 210, "{mods:#x}");
        }
        for (_, shown) in KEYS {
            assert!(font::measure(Font::Row, shown) <= 210, "{shown}");
        }
        for text in [EMPTY_LABEL_HINT, NEEDS_LABEL, "none"] {
            assert!(font::measure(Font::Row, text) <= 210, "{text}");
        }
        for label in ["Label", "Modifiers", "Key", "Save", "Remove key"] {
            assert!(font::measure(Font::Row, label) <= 180, "{label}");
        }
        assert!(layout::macro_edit_row(layout::MACRO_EDIT_ROWS - 1).bottom() <= layout::HINT_BAR.top() - 1);
        assert!(layout::MACRO_EDIT_PREVIEW.bottom() <= layout::macro_edit_row(0).top());
        let mut edit = Edit::new();
        edit.index = 8;
        let mut tag = Text::new();
        slot_tag(&edit, &mut tag, crate::ui::i18n::Language::English);
        assert_eq!(tag.as_str(), "key 9");
        assert_eq!(kit::hints_that_fit(&hints(&edit)).0, 3);
        edit.adjusting = true;
        for row in [ROW_MODS, ROW_KEY] {
            edit.row = row;
            assert_eq!(kit::hints_that_fit(&hints(&edit)[..2]).0, 2);
        }
    }
}
