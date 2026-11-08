//! The tutorial pages (`Overlay::Tutorial`), opaque: TUTORIAL header with a
//! "2 / 6" tag, a title, one or two lines of explanation, the step's own
//! content, a caption above the hint bar, and a hint bar that names the
//! control the step waits for. The Buttons, Wheel, Latest and PTT steps draw the
//! deck (`device`) with the control being asked for lit and labelled; the
//! Panels step shows the strip as chips; Done is text.
//!
//! `state::tutorial::caption` is the one source of the caption; the
//! signature hashes the same inputs `draw` reads, in three lanes (title,
//! content, caption) with the step folded into the frame mode.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::kit::{self, HeaderRight, Hint};
use crate::ui::overlay::device::{self, Highlight};
use crate::ui::overlay::notify::header_inputs;
use crate::ui::signature::SigBuilder;
use crate::ui::state::tutorial::{caption_localized, name, role, Step, PANEL_SWITCHES, WHEEL_DETENTS};
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, layout, layout::Rect, theme};

/// Everything between the header and the hint bar, and its three lanes:
/// the title, the step's content, the caption.
const BODY: Rect = layout::BODY;
const TITLE_LANE: Rect = Rect::new(0, 60, theme::SCREEN, 64);
const CONTENT: Rect = Rect::new(0, 124, theme::SCREEN, 262);
const CAPTION_LANE: Rect = Rect::new(0, 386, theme::SCREEN, theme::HINT_Y - 1 - 386);
const TITLE_BASELINE: i32 = 104;
const LINE_1_BASELINE: i32 = 150;
/// The deck drawing, centred, under the explanation lines.
const DEVICE_LEFT: i32 = (theme::SCREEN - device::WIDTH) / 2;
const DEVICE_TOP: i32 = 196;
const CHIPS_Y: i32 = 224;
const CHIPS_H: i32 = 36;
const CHIP_GAP: i32 = 6;
const FOOTNOTE_BASELINE: i32 = 300;
const CAPTION_BASELINE: i32 = 414;

const fn title(step: Step) -> &'static str {
    match step {
        Step::Buttons => "Press ",
        Step::Wheel => "Turn the wheel",
        Step::Panels => "Switch panels",
        Step::Latest => "Follow latest",
        Step::Ptt => "Push to talk",
        Step::Done => "You're ready",
    }
}

/// The explanation lines of a step (the Buttons step shows the key's role).
const fn lines(step: Step) -> [&'static str; 2] {
    match step {
        Step::Buttons => ["", ""],
        Step::Wheel => ["It moves through lists,", "choices and values."],
        Step::Panels => ["Hold SHIFT and turn the wheel", "to move along the strip."],
        Step::Latest => ["In chat, jump to the latest message", "and resume following new text."],
        Step::Ptt => ["Hold it and speak to the agent.", "Let go to send."],
        Step::Done => ["Hold PTT 2 s to switch on,", "hold CANCEL 3 s to switch off."],
    }
}

const DONE_LINES: [&str; 2] = ["Replay this any time from", "Settings \u{203a} Display \u{203a} Tutorial."];
const PANELS_FOOTNOTE: &str = "Two quick SHIFT taps open the face manager.";

/// "Press ENTER" on the Buttons step, the step's title otherwise.
fn title_text(ui: &UiState, out: &mut Text<64>) {
    out.set(ui.tr(title(ui.tutorial.step)));
    if ui.tutorial.step == Step::Buttons {
        out.push_str(ui.tutorial.current().map_or("", name));
    }
}

fn highlight(ui: &UiState) -> Highlight {
    match ui.tutorial.step {
        Step::Buttons => ui.tutorial.current().map_or(Highlight::None, Highlight::Button),
        Step::Wheel => Highlight::Wheel,
        Step::Latest => Highlight::Chord(crate::control::Button::Shift, crate::control::Button::Enter),
        Step::Ptt => Highlight::Button(crate::control::Button::PushToTalk),
        Step::Panels | Step::Done => Highlight::None,
    }
}

fn hints(ui: &UiState) -> [Hint<'static>; 2] {
    let none = Hint { key: "", label: "" };
    let leave = Hint { key: "CANCEL", label: if ui.tutorial.replay { "back" } else { "skip" } };
    match ui.tutorial.step {
        Step::Buttons => [Hint { key: "", label: "press the lit key" }, none],
        Step::Wheel => [Hint { key: "SCROLL", label: "turn" }, leave],
        Step::Panels => [Hint { key: "SHIFT + SCROLL", label: "switch" }, leave],
        Step::Latest => [Hint { key: "SHIFT + ENTER", label: "latest" }, leave],
        Step::Ptt => [Hint { key: "PTT", label: "hold, then let go" }, leave],
        Step::Done => [Hint { key: "ENTER", label: if ui.tutorial.replay { "done" } else { "start" } }, none],
    }
}

fn hint_bar(band: &mut Band, hints: &[Hint], language: crate::ui::i18n::Language) {
    let count = hints.iter().take_while(|hint| !hint.key.is_empty() || !hint.label.is_empty()).count();
    kit::hint_bar_localized(band, hints.get(..count).unwrap_or(&[]), language);
}

fn step_tag(step: Step, out: &mut Text<8>) {
    out.clear();
    out.push_char((b'0' + step.number()) as char);
    out.push_str(" / ");
    out.push_char((b'0' + Step::ALL.len() as u8) as char);
}

fn draw_lines(band: &mut Band, lines: [&str; 2], first_baseline: i32, color: u32, language: crate::ui::i18n::Language) {
    let lh = font::line_height(Font::Body);
    for (index, line) in lines.iter().enumerate() {
        font::draw(band, Font::Body, theme::TEXT_X, first_baseline + index as i32 * lh, language.text(line), color, Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Left);
    }
}

/// The strip's faces as chips, the tutorial's own selection outlined.
fn draw_panels(band: &mut Band, ui: &UiState) {
    let count = (ui.global.strip_len as i32).clamp(1, crate::ui::state::MAX_FACES as i32);
    let width = (theme::SCREEN - 2 * theme::MARGIN - (count - 1) * CHIP_GAP) / count;
    let left = (theme::SCREEN - (count * width + (count - 1) * CHIP_GAP)) / 2;
    for index in 0..count {
        let rect = Rect::new(left + index * (width + CHIP_GAP), CHIPS_Y, width, CHIPS_H);
        if let Some(face) = ui.face_at(index as u8) {
            kit::chip(band, rect, ui.face_name(&face), index as u8 == ui.tutorial.panel);
        }
    }
    font::draw(band, Font::Small, theme::SCREEN / 2, FOOTNOTE_BASELINE, ui.tr(PANELS_FOOTNOTE), theme::TEXT_LABEL, Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Center);
    let _ = (PANEL_SWITCHES, WHEEL_DETENTS);
}

pub fn draw(band: &mut Band, ui: &UiState) {
    band.fill(kit::rgb(theme::CANVAS));
    let step = ui.tutorial.step;
    let mut tag = Text::new();
    step_tag(step, &mut tag);
    kit::header(band, ui.tr("TUTORIAL"), kit::header_right(ui, HeaderRight::Text(tag.as_str())));
    if kit::visible(band, BODY) {
        let mut title = Text::new();
        title_text(ui, &mut title);
        font::draw(band, Font::Title, theme::TEXT_X, TITLE_BASELINE, title.as_str(), theme::TEXT_PRIMARY, Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Left);
        match step {
            Step::Buttons => {
                let role = ui.tutorial.current().map_or("", role);
                draw_lines(band, [role, ""], LINE_1_BASELINE, theme::TEXT_ROW, ui.language);
                device::draw_localized(band, DEVICE_LEFT, DEVICE_TOP, highlight(ui), ui.language);
            }
            Step::Wheel | Step::Latest | Step::Ptt => {
                draw_lines(band, lines(step), LINE_1_BASELINE, theme::TEXT_ROW, ui.language);
                device::draw_localized(band, DEVICE_LEFT, DEVICE_TOP, highlight(ui), ui.language);
            }
            Step::Panels => {
                draw_lines(band, lines(step), LINE_1_BASELINE, theme::TEXT_ROW, ui.language);
                draw_panels(band, ui);
            }
            Step::Done => {
                draw_lines(band, lines(step), LINE_1_BASELINE, theme::TEXT_ROW, ui.language);
                draw_lines(band, DONE_LINES, LINE_1_BASELINE + 3 * font::line_height(Font::Body), theme::TEXT_SECONDARY, ui.language);
            }
        }
        let mut caption_text = Text::<96>::new();
        caption_localized(&ui.tutorial, &mut caption_text, ui.language);
        let color = if step == Step::Buttons && ui.tutorial.wrong { theme::AMBER } else { theme::TEXT_SECONDARY };
        font::draw(band, Font::Row, theme::SCREEN / 2, CAPTION_BASELINE, caption_text.as_str(), color, Some(theme::CANVAS), theme::TEXT_RIGHT - theme::TEXT_X, Align::Center);
    }
    hint_bar(band, &hints(ui), ui.language);
}

pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    let view = &ui.tutorial;
    sig.mode(view.step as u32 | (view.replay as u32) << 8);
    sig.region_with(layout::HEADER, |h| {
        header_inputs(ui, h);
        h.u8(view.step as u8);
    });
    sig.region_with(TITLE_LANE, |h| {
        let mut title = Text::new();
        title_text(ui, &mut title);
        h.text(&title);
    });
    match view.step {
        Step::Buttons => sig.region_with(CONTENT, |h| {
            h.u8(view.index);
        }),
        Step::Wheel => sig.region_with(CONTENT, |h| {
            h.u8(view.detents);
        }),
        Step::Panels => sig.region_with(CONTENT, |h| {
            h.u8(view.panel).u8(ui.global.strip_len);
            for index in 0..ui.global.strip_len {
                if let Some(face) = ui.face_at(index) {
                    h.text(&face.name);
                }
            }
        }),
        Step::Ptt => sig.region_with(CONTENT, |h| {
            h.bool(view.ptt_down);
        }),
        Step::Latest | Step::Done => sig.region(CONTENT, 0),
    }
    sig.region_with(CAPTION_LANE, |h| {
        let mut caption_text = Text::<96>::new();
        caption_localized(view, &mut caption_text, ui.language);
        h.text(&caption_text).bool(view.wrong);
    });
    sig.region_with(layout::HINT_BAR, |h| {
        for hint in hints(ui) {
            h.str(hint.key).str(hint.label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::tutorial::BUTTONS;

    #[test]
    fn every_language_fits_the_tutorial_title_and_instructions() {
        use crate::ui::i18n::Language;
        let room = theme::TEXT_RIGHT - theme::TEXT_X;
        let mut ui = std::boxed::Box::new(UiState::new());
        for language in Language::ALL {
            ui.language = language;
            for step in Step::ALL {
                ui.tutorial.step = step;
                let mut title = Text::new();
                title_text(&ui, &mut title);
                assert!(font::measure(Font::Title, title.as_str()) <= room, "{} {step:?}: {}", language.code(), title.as_str());
                for line in lines(step) {
                    assert!(font::measure(Font::Body, language.text(line)) <= room, "{} {line}", language.code());
                }
            }
            for line in DONE_LINES {
                assert!(font::measure(Font::Body, language.text(line)) <= room, "{} {line}", language.code());
            }
            assert!(font::measure(Font::Small, language.text(PANELS_FOOTNOTE)) <= room);
            for button in crate::control::Button::ALL {
                assert!(font::measure(Font::Body, language.text(role(button))) <= room, "{} {button:?}", language.code());
            }
        }
    }

    #[test]
    fn every_page_string_fits_its_line_and_the_lanes_tile_the_body() {
        let room = theme::TEXT_RIGHT - theme::TEXT_X;
        for step in Step::ALL {
            assert!(font::measure(Font::Title, title(step)) <= room, "{step:?}");
            for line in lines(step) {
                assert!(font::measure(Font::Body, line) <= room, "{line}");
            }
        }
        for line in DONE_LINES {
            assert!(font::measure(Font::Body, line) <= room, "{line}");
        }
        assert!(font::measure(Font::Small, PANELS_FOOTNOTE) <= room);
        for button in BUTTONS {
            let mut title = Text::<24>::new();
            title.set("Press ");
            title.push_str(name(button));
            assert!(font::measure(Font::Title, title.as_str()) <= room, "{button:?}");
            assert!(font::measure(Font::Body, role(button)) <= room, "{button:?}");
        }
        assert!(font::measure(Font::Body, role(crate::control::Button::PushToTalk)) <= room);
        let mut tag = Text::new();
        step_tag(Step::Panels, &mut tag);
        assert_eq!(tag.as_str(), "3 / 6");
        // The three lanes tile the body; the drawing and its labels stay in
        // the content lane, clear of the explanation lines above it.
        assert_eq!((TITLE_LANE.top(), TITLE_LANE.bottom()), (BODY.top(), CONTENT.top()));
        assert_eq!((CONTENT.bottom(), CAPTION_LANE.bottom()), (CAPTION_LANE.top(), BODY.bottom()));
        assert!(DEVICE_TOP >= LINE_1_BASELINE + font::line_height(Font::Body) + 6);
        assert!(DEVICE_TOP + device::HEIGHT <= CONTENT.bottom());
        assert!(DEVICE_LEFT - device::LABEL_ROOM >= 0 && DEVICE_LEFT + device::WIDTH + device::LABEL_ROOM <= theme::SCREEN);
        assert!(CAPTION_BASELINE - font::ascent(Font::Row) >= CAPTION_LANE.top());
    }
}
