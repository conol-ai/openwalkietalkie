//! Face manager (mockup 2), template gallery, naming and the remove-confirm
//! dialog: whichever `ui.global.overlay` names.
//!
//! Manager: FACES header, a four-slot row of thumbnail cards
//! (`layout::manager_card`) starting at `View::first_visible`, the selected
//! one lifted with a white outline, a grabbed one lifted and brighter; after
//! the strip a dashed "+" card while the strip has room. Each card carries a
//! schematic sketch of its template and the instance name in caps (the
//! keycap face: the tracked label face does not fit "MACROPAD" in 75 px).
//! Confirm: the same row under a dialog with two large buttons. Gallery: one
//! row per `GALLERY` template. Naming: the template, the name field and the
//! hold-PTT help.
//!
//! Signature: header, four slots, then the overlay's own regions and the hint
//! bar; the overlay kind is in the mode, so each has its own list shape.
use crate::deck::msg::{FaceEntry, Template};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::overlay::notify::header_inputs;
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::manager::GALLERY;
use crate::ui::state::{Overlay, UiState, MAX_FACES};
use crate::ui::{font, font::Align, font::Font, kit, layout, layout::Rect, theme};

const THUMB_R: u32 = 22;
/// A Macropad instance limit the gallery enforces (`manager` grammar).
const MAX_MACROPADS: usize = 3;

/// Cards in the row: the strip, plus "+" while a face can still be added.
fn card_count(ui: &UiState) -> u8 {
    let faces = ui.global.strip_len.min(MAX_FACES as u8);
    faces + u8::from((faces as usize) < MAX_FACES)
}

fn viewport(ui: &UiState) -> kit::list::Viewport {
    kit::list::Viewport::new(ui.manager.first_visible, layout::MANAGER_VISIBLE, card_count(ui))
}

/// What slot `slot` holds: `Some(None)` is the "+" card.
fn card_at(ui: &UiState, slot: u8) -> Option<Option<FaceEntry>> {
    let index = viewport(ui).index_at(slot)?;
    Some(ui.face_at(index))
}

fn caps<const N: usize>(name: &str, out: &mut Text<N>) {
    out.clear();
    for c in name.chars() {
        if !out.push_char(c.to_ascii_uppercase()) {
            break;
        }
    }
}

fn pill(band: &mut Band, card: Rect, x: i32, y: i32, w: i32, h: i32, color: u32) {
    kit::round(band, Rect::new(card.left() + x, card.top() + y, w, h), (h / 2) as u32, color);
}

fn key(band: &mut Band, card: Rect, x: i32, y: i32, w: i32, h: i32, color: u32) {
    kit::round(band, Rect::new(card.left() + x, card.top() + y, w, h), 4, color);
}

/// Schematic sketch of a template inside `card` (geometry from mockup 2).
fn sketch(band: &mut Band, card: Rect, template: Template, bar: u32) {
    let (ox, oy) = (layout::THUMB_SKETCH.left(), layout::THUMB_SKETCH.top());
    match template {
        Template::Agents => {
            for (row, color) in [theme::RED, theme::GREEN, theme::BLUE].into_iter().enumerate() {
                let y = oy + 1 + row as i32 * 16;
                kit::dot(band, card.left() + ox + 4, card.top() + y + 4, 4, color);
                pill(band, card, ox + 13, y + 1, [55, 47, 51][row], 6, bar);
            }
        }
        Template::Omarchy => {
            pill(band, card, ox, oy + 1, 40, 6, bar);
            for chip in 0..3 {
                let color = if chip == 1 { theme::TEXT_BRIGHT } else { bar };
                key(band, card, ox + chip * 21, oy + 15, 16, 16, color);
            }
            pill(band, card, ox, oy + 39, 64, 6, bar);
            pill(band, card, ox, oy + 53, 56, 6, bar);
        }
        Template::Arcade => {
            key(band, card, ox + 25, oy + 16, 12, 36, theme::BLUE);
            key(band, card, ox + 13, oy + 28, 36, 12, theme::BLUE);
            key(band, card, ox + 57, oy + 28, 10, 10, theme::AMBER);
        }
        Template::Macropad => {
            for index in 0..6 {
                key(band, card, ox + (index % 3) * 27, oy + 1 + (index / 3) * 21, 22, 16, bar);
            }
        }
        Template::Usage | Template::System => {
            for (row, (label, percent, color)) in [(30, 60, theme::BLUE), (24, 35, theme::GREEN), (36, 85, theme::AMBER)].into_iter().enumerate() {
                let y = oy + 1 + row as i32 * 25;
                pill(band, card, ox, y, label, 5, bar);
                pill(band, card, ox, y + 9, 64, 6, bar);
                pill(band, card, ox, y + 9, 64 * percent / 100, 6, color);
            }
        }
        Template::Timer => {
            // A dial with a hand, and the start button under it.
            let ring = Rect::new(card.left() + ox + 11, card.top() + oy + 2, 53, 53);
            kit::outline(band, ring, 26, bar, 4);
            pill(band, card, ox + 35, oy + 12, 5, 19, theme::TEXT_SECONDARY);
            pill(band, card, ox + 35, oy + 26, 15, 5, theme::TEXT_SECONDARY);
            pill(band, card, ox + 12, oy + 68, 51, 14, theme::GREEN);
        }
        Template::Settings => {
            for (row, knob) in [40, 18, 52].into_iter().enumerate() {
                let y = oy + 6 + row as i32 * 20;
                pill(band, card, ox, y, 64, 4, bar);
                kit::dot(band, card.left() + ox + knob, card.top() + y + 2, 4, theme::TEXT_SECONDARY);
            }
        }
    }
}

/// Cards off the row's left and right edges.
fn more(ui: &UiState) -> (u8, u8) {
    let view = viewport(ui);
    (view.top, card_count(ui).saturating_sub(view.top.saturating_add(view.visible)))
}

/// "\u{2039} 1 more" at the row's left edge, "2 more \u{203a}" at its right.
fn draw_more(band: &mut Band, ui: &UiState) {
    let lane = layout::MANAGER_MORE;
    let (before, after) = more(ui);
    if (before == 0 && after == 0) || !kit::visible(band, lane) {
        return;
    }
    let baseline = lane.top() + 20;
    let mut line = Text::<24>::new();
    if before > 0 {
        line.push_str("\u{2039} ");
        line.push_char((b'0' + before % 10) as char);
        line.push_str(ui.tr(" more"));
        font::draw(band, Font::Small, lane.left(), baseline, line.as_str(), theme::TEXT_FAINT, Some(theme::CANVAS), lane.width() / 2, Align::Left);
    }
    if after > 0 {
        line.clear();
        line.push_char((b'0' + after % 10) as char);
        line.push_str(ui.tr(" more \u{203a}"));
        font::draw(band, Font::Small, lane.right(), baseline, line.as_str(), theme::TEXT_FAINT, Some(theme::CANVAS), lane.width() / 2, Align::Right);
    }
}

fn draw_card(band: &mut Band, ui: &UiState, slot: u8) {
    let Some(card) = card_at(ui, slot) else {
        return;
    };
    let index = ui.manager.first_visible.saturating_add(slot);
    let selected = index == ui.manager.selected;
    let grabbed = selected && ui.manager.grabbed;
    let lift = if selected { layout::MANAGER_LIFT } else { 0 };
    let rect = layout::manager_card(slot).offset(0, -lift);
    if !kit::visible(band, rect) {
        return;
    }
    let Some(face) = card else {
        kit::dashed(band, rect, THUMB_R, if selected { theme::TEXT_SECONDARY } else { theme::OUTLINE_KEY }, theme::CANVAS);
        let (cx, cy) = rect.center();
        kit::fill(band, Rect::new(cx - 6, cy - 1, 13, 2), theme::PLUS);
        kit::fill(band, Rect::new(cx - 1, cy - 6, 2, 13), theme::PLUS);
        return;
    };
    let (fill, bar, label) = if grabbed {
        (theme::CHIP_FILL, theme::OUTLINE_THUMB, theme::TEXT_BRIGHT)
    } else {
        (theme::THUMB_FILL, theme::THUMB_BAR, theme::TEXT_SECONDARY)
    };
    kit::round(band, rect, THUMB_R, fill);
    if selected {
        kit::outline(band, rect, THUMB_R, theme::TEXT_PRIMARY, 2);
    } else {
        kit::outline(band, rect, THUMB_R, theme::OUTLINE_THUMB, 1);
    }
    let mut name = Text::<48>::new();
    caps(ui.face_name(&face), &mut name);
    let (lx, ly) = layout::THUMB_LABEL;
    // Six Japanese glyphs need 78 px at the smallest face. The label may
    // use the space up to the card's 8 px right inset, beyond the sketch.
    let label_width = rect.width() - lx - 8;
    font::draw(band, Font::Keycap, rect.left() + lx, rect.top() + ly, name.as_str(), label, Some(fill), label_width, Align::Left);
    sketch(band, rect, face.template, bar);
}

/// The manager hint depends on what is selected.
fn manager_hints(ui: &UiState) -> [kit::Hint<'static>; 4] {
    let done = kit::Hint { key: "SHIFT SHIFT", label: "done" };
    let none = kit::Hint { key: "", label: "" };
    if ui.manager.grabbed {
        return [
            kit::Hint { key: "SCROLL", label: "move" },
            kit::Hint { key: "ENTER", label: "drop" },
            kit::Hint { key: "CANCEL", label: "revert" },
            done,
        ];
    }
    match ui.face_at(ui.manager.selected).map(|face| face.template) {
        None => [kit::Hint { key: "ENTER", label: "add a face" }, kit::Hint { key: "SCROLL", label: "select" }, done, none],
        Some(Template::Agents | Template::Settings) => [
            kit::Hint { key: "ENTER", label: "go" },
            kit::Hint { key: "SCROLL", label: "select" },
            done,
            none,
        ],
        Some(_) => [
            kit::Hint { key: "ENTER", label: "grab / drop" },
            kit::Hint { key: "SCROLL", label: "move" },
            kit::Hint { key: "CANCEL", label: "remove" },
            done,
        ],
    }
}

fn hint_bar(band: &mut Band, hints: &[kit::Hint], language: crate::ui::i18n::Language) {
    let count = hints.iter().take_while(|hint| !hint.key.is_empty() || !hint.label.is_empty()).count();
    kit::hint_bar_localized(band, hints.get(..count).unwrap_or(&[]), language);
}

fn macropads(ui: &UiState) -> usize {
    (0..ui.global.strip_len)
        .filter_map(|index| ui.face_at(index))
        .filter(|face| face.template == Template::Macropad)
        .count()
}

/// A gallery row that the grammar will refuse: the third Macropad, or a
/// second Timer.
fn gallery_refused(ui: &UiState, template: Template) -> bool {
    match template {
        Template::Macropad => macropads(ui) >= MAX_MACROPADS,
        Template::Timer => crate::ui::state::manager::has_timer(ui),
        _ => false,
    }
}

/// What a refused row says instead of its blurb.
const fn refused_blurb(template: Template) -> &'static str {
    match template {
        Template::Timer => "one already",
        _ => "three already",
    }
}

const fn gallery_blurb(template: Template) -> &'static str {
    match template {
        Template::Omarchy => "workspaces and windows",
        Template::Macropad => "nine keys from your dotfile",
        Template::Usage => "subscription limits",
        Template::Arcade => "three pocket games",
        Template::System => "computer resources",
        Template::Timer => "stopwatch and countdown",
        Template::Agents | Template::Settings => "",
    }
}

fn draw_gallery(band: &mut Band, ui: &UiState) {
    font::draw(band, Font::Text, layout::GALLERY_TITLE.left(), layout::GALLERY_TITLE.top() + 28, ui.tr("Pick a template"), theme::TEXT_ROW, Some(theme::CANVAS), layout::GALLERY_TITLE.width(), Align::Left);
    for (row, template) in GALLERY.iter().enumerate() {
        let rect = layout::gallery_row(row as u8);
        if !kit::visible(band, rect) {
            continue;
        }
        let selected = row as u8 == ui.manager.gallery_sel;
        let pressed = ui.manager.pressed == Some(row as u8);
        let refused = gallery_refused(ui, *template);
        let fill = if pressed {
            kit::round(band, rect, theme::CARD_R, theme::CHIP_FILL);
            kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_SELECTED, 1);
            theme::CHIP_FILL
        } else {
            kit::card(band, rect, selected, false);
            if selected { theme::CARD_FILL } else { theme::CANVAS }
        };
        let label = if refused { theme::TEXT_FAINT } else if selected || pressed { theme::TEXT_PRIMARY } else { theme::TEXT_ROW };
        let baseline = rect.top() + layout::GALLERY_BASELINE;
        font::draw(band, Font::Row, rect.left() + layout::SETTING_LABEL_X, baseline, ui.tr(template.label()), label, Some(fill), 160, Align::Left);
        let blurb = if refused { refused_blurb(*template) } else { gallery_blurb(*template) };
        font::draw(band, Font::Small, rect.left() + layout::SETTING_VALUE_RIGHT, baseline, ui.tr(blurb), theme::TEXT_LABEL, Some(fill), 220, Align::Right);
    }
    kit::hint_bar_localized(band, &[
        kit::Hint { key: "SCROLL", label: "pick" },
        kit::Hint { key: "ENTER", label: "add" },
        kit::Hint { key: "CANCEL", label: "back" },
    ], ui.language);
}

fn draw_naming(band: &mut Band, ui: &UiState) {
    let template = ui.tr(ui.manager.naming_template.label());
    let title = layout::NAMING_TITLE;
    let used = font::draw(band, Font::Text, title.left(), title.top() + 28, ui.tr("Name this "), theme::TEXT_ROW, Some(theme::CANVAS), title.width(), Align::Left);
    font::draw(band, Font::Text, title.left() + used, title.top() + 28, template, theme::TEXT_PRIMARY, Some(theme::CANVAS), title.width() - used, Align::Left);
    let field = layout::NAMING_FIELD;
    kit::card(band, field, true, false);
    let (cx, cy) = field.center();
    let typed = !ui.manager.naming_text.is_empty();
    let (name, color) = if typed { (ui.manager.naming_text.as_str(), theme::TEXT_PRIMARY) } else { (template, theme::TEXT_FAINT) };
    font::draw(band, Font::Title, cx, cy + 9, name, color, Some(theme::CARD_FILL), field.width() - 32, Align::Center);
    let help = layout::NAMING_HELP;
    let lh = font::line_height(Font::Body);
    let (hx, _) = help.center();
    font::draw(band, Font::Body, hx, help.top() + 22, ui.tr("Hold PTT and say a name"), theme::TEXT_SECONDARY, Some(theme::CANVAS), help.width(), Align::Center);
    let keep = if typed { "ENTER keeps what you said" } else { "ENTER keeps the template name" };
    font::draw(band, Font::Body, hx, help.top() + 22 + lh, ui.tr(keep), theme::TEXT_LABEL, Some(theme::CANVAS), help.width(), Align::Center);
    kit::hint_bar_localized(band, &[
        kit::Hint { key: "PTT", label: "say it" },
        kit::Hint { key: "ENTER", label: "accept" },
        kit::Hint { key: "CANCEL", label: "back" },
    ], ui.language);
}

fn confirm_face(ui: &UiState) -> Option<FaceEntry> {
    ui.index_of_face(ui.manager.confirm_face).and_then(|index| ui.face_at(index))
}

/// "Remove <name>?" and "<Template> face"; a face that is already gone
/// (a host `faces` push under the dialog) asks about "this face".
fn confirm_lines(ui: &UiState, question: &mut Text<80>, what: &mut Text<48>) {
    question.set(ui.tr("Remove "));
    match confirm_face(ui) {
        Some(face) => {
            question.push_str(ui.face_name(&face));
            what.set(ui.tr(face.template.label()));
            what.push_str(ui.tr(" face"));
        }
        None => {
            question.push_str(ui.tr("this face"));
            what.clear();
        }
    }
    question.push_char('?');
}

const CONFIRM_LABELS: [&str; 2] = ["Keep", "Remove"];

fn draw_confirm(band: &mut Band, ui: &UiState) {
    kit::hint_bar_localized(band, &[
        kit::Hint { key: "ENTER", label: "remove" },
        kit::Hint { key: "CANCEL", label: "keep" },
    ], ui.language);
    let bx = layout::CONFIRM_BOX;
    if !kit::visible(band, bx) {
        return;
    }
    kit::round(band, bx, theme::CARD_R, theme::CARD_FILL);
    kit::outline(band, bx, theme::CARD_R, theme::OUTLINE_THUMB, 1);
    let (mut question, mut what) = (Text::new(), Text::new());
    confirm_lines(ui, &mut question, &mut what);
    let text = layout::CONFIRM_TEXT;
    let (cx, _) = text.center();
    let lh = font::line_height(Font::Name);
    font::draw(band, Font::Name, cx, text.top() + 32, question.as_str(), theme::TEXT_PRIMARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    font::draw(band, Font::Row, cx, text.top() + 32 + lh, what.as_str(), theme::TEXT_SECONDARY, Some(theme::CARD_FILL), text.width(), Align::Center);
    for (index, label) in CONFIRM_LABELS.iter().enumerate() {
        let rect = layout::confirm_button(index as u8);
        let pressed = ui.manager.pressed == Some(index as u8);
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

pub fn draw(band: &mut Band, ui: &UiState) {
    band.fill(kit::rgb(theme::CANVAS));
    let (label, right) = match ui.global.overlay {
        Overlay::Gallery => ("ADD FACE", "gallery"),
        Overlay::Naming => ("NAME FACE", ui.manager.naming_template.label()),
        Overlay::ConfirmRemove => ("FACES", "remove"),
        _ => ("FACES", "manage"),
    };
    kit::header(band, ui.tr(label), kit::header_right(ui, kit::HeaderRight::Text(ui.tr(right))));
    match ui.global.overlay {
        Overlay::Gallery => draw_gallery(band, ui),
        Overlay::Naming => draw_naming(band, ui),
        overlay => {
            for slot in 0..layout::MANAGER_VISIBLE {
                draw_card(band, ui, slot);
            }
            if overlay == Overlay::ConfirmRemove {
                draw_confirm(band, ui);
            } else {
                draw_more(band, ui);
                hint_bar(band, &manager_hints(ui), ui.language);
            }
        }
    }
}

fn hash_hints(hints: &[kit::Hint], h: &mut Hasher) {
    for hint in hints {
        h.str(hint.key).str(hint.label);
    }
}

pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    sig.region_with(layout::HEADER, |h| {
        header_inputs(ui, h);
        h.u8(ui.manager.naming_template as u8);
    });
    match ui.global.overlay {
        Overlay::Gallery => {
            sig.region(layout::GALLERY_TITLE, 0);
            for (row, template) in GALLERY.iter().enumerate() {
                sig.region_with(layout::gallery_row(row as u8), |h| {
                    h.bool(row as u8 == ui.manager.gallery_sel)
                        .bool(ui.manager.pressed == Some(row as u8))
                        .bool(gallery_refused(ui, *template));
                });
            }
            sig.region(layout::HINT_BAR, 0);
        }
        Overlay::Naming => {
            sig.region_with(layout::NAMING_TITLE, |h| {
                h.u8(ui.manager.naming_template as u8);
            });
            sig.region_with(layout::NAMING_FIELD, |h| {
                h.u8(ui.manager.naming_template as u8).text(&ui.manager.naming_text);
            });
            sig.region_with(layout::NAMING_HELP, |h| {
                h.bool(ui.manager.naming_text.is_empty());
            });
            sig.region(layout::HINT_BAR, 0);
        }
        overlay => {
            sig.region_with(layout::MANAGER_MORE, |h| {
                // Under the confirm dialog the row's edges are not marked.
                let (before, after) = if overlay == Overlay::ConfirmRemove { (0, 0) } else { more(ui) };
                h.u8(before).u8(after);
            });
            for slot in 0..layout::MANAGER_VISIBLE {
                sig.region_with(layout::manager_slot(slot), |h| {
                    let index = ui.manager.first_visible.saturating_add(slot);
                    h.bool(index == ui.manager.selected).bool(ui.manager.grabbed);
                    match card_at(ui, slot) {
                        None => h.u8(0),
                        Some(None) => h.u8(1),
                        Some(Some(face)) => h.u8(2).u8(face.id).u8(face.template as u8).text(&face.name),
                    };
                });
            }
            if overlay == Overlay::ConfirmRemove {
                sig.region_with(layout::CONFIRM_BOX, |h| {
                    let (mut question, mut what) = (Text::new(), Text::new());
                    confirm_lines(ui, &mut question, &mut what);
                    h.text(&question).text(&what).opt_u32(ui.manager.pressed.map(u32::from));
                });
                sig.region(layout::HINT_BAR, 1);
            } else {
                sig.region_with(layout::HINT_BAR, |h| hash_hints(&manager_hints(ui), h));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_face_names_fit_thumbnails_in_every_language() {
        let width = layout::manager_card(0).width() - layout::THUMB_LABEL.0 - 8;
        for language in crate::ui::i18n::Language::ALL {
            for template in Template::ALL {
                let mut name = Text::<48>::new();
                caps(language.text(template.label()), &mut name);
                assert!(font::measure(Font::Keycap, name.as_str()) <= width, "{} {:?}: {}", language.code(), template, name.as_str());
            }
        }
    }

    #[test]
    fn the_plus_card_follows_the_strip_and_hides_at_eight_faces() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        assert_eq!(card_count(&ui), 8);
        assert_eq!(card_at(&ui, 0).map(|c| c.map(|f| f.template)), Some(Some(Template::Agents)));
        ui.manager.first_visible = 4;
        assert_eq!(card_at(&ui, 3), Some(None), "slot 3 shows the + card");
        ui.manager.first_visible = 9;
        assert_eq!(viewport(&ui).top, 4, "the viewport clamps");
        assert_eq!(more(&ui), (4, 0), "four cards off the left edge");
        ui.manager.first_visible = 0;
        assert_eq!(more(&ui), (0, 4), "System, Arcade, Settings and + off the right edge");
        ui.manager.first_visible = 1;
        assert_eq!(more(&ui), (1, 3));
        ui.global.strip_len = 8;
        assert_eq!(card_count(&ui), 8);
        ui.global.strip_len = 200;
        assert_eq!(card_count(&ui), 8);
        assert_eq!(card_at(&ui, 3).map(|c| c.is_none()), Some(false));
    }

    #[test]
    fn a_third_macropad_refuses_a_fourth_and_names_are_capitalised() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        assert!(!gallery_refused(&ui, Template::Macropad));
        // The default strip already has one Macropad (index 2).
        for index in [1, 3] {
            ui.global.strip[index].template = Template::Macropad;
        }
        assert_eq!(macropads(&ui), 3);
        assert!(gallery_refused(&ui, Template::Macropad) && !gallery_refused(&ui, Template::Usage));
        let mut name = Text::<16>::new();
        caps("cohdl \u{e9}x", &mut name);
        assert_eq!(name.as_str(), "COHDL \u{e9}X");
        caps("a very long face name indeed", &mut name);
        assert_eq!(name.as_str(), "A VERY LONG FACE");
    }

    #[test]
    fn the_confirm_dialog_names_the_face_on_one_line_or_asks_about_this_face() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        let (mut question, mut what) = (Text::new(), Text::new());
        ui.manager.confirm_face = 2;
        confirm_lines(&ui, &mut question, &mut what);
        assert_eq!((question.as_str(), what.as_str()), ("Remove Omarchy?", "Omarchy face"));
        ui.global.strip[1].name.set("A very long face");
        confirm_lines(&ui, &mut question, &mut what);
        assert_eq!(question.as_str(), "Remove A very long face?");
        assert!(font::measure(Font::Name, question.as_str()) <= layout::CONFIRM_TEXT.width(), "a full name fits");
        ui.manager.confirm_face = 200;
        confirm_lines(&ui, &mut question, &mut what);
        assert_eq!((question.as_str(), what.as_str()), ("Remove this face?", ""));
    }
}
