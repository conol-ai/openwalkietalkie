//! Integer geometry of every hit and draw target, in logical 480x480 face
//! coordinates. The state machines hit-test against these and the faces draw
//! to them, so a tap always lands on what was drawn.
//!
//! Measured from `mock/*.png` with Pillow (960 px mockups / 2; edges are the
//! centre of each 1.5 px outline stroke). Measurements, as x0..x1 / y0..y1:
//!   header      label cap-height 41..50.5, text from x 38..40, tags end 440..442
//!   hint bar    hairline y 427, fill 428..480, keycaps y 443.5..463.5
//!   indicator   pill x 223..257, y 10..15
//!   roster      row 32..448 x 70..144, pitch 77.5; dot centre x 53, d 11;
//!               name x 72.5 baseline +33, title baseline +53
//!   card        dot 39..50 x 41..52, name x 60.5, question 37.. x 84..132 (2
//!               lines); choice rows 32..448 x 156..207, pitch 61; number chip
//!               46.5..68.5 (22x22), choice text x 81.5, "recommended" ends 433
//!   ptt         name x 39.5 baseline 56, pill 333..441 x 37..64, waveform
//!               39..242 x 95..157 (5 px bars, 9 px pitch), transcript from
//!               40,188 (28 px line pitch), last reply baseline 410
//!   omarchy     title baseline 103; chips 37..71 square, pitch 42, y 125.5..
//!               159.5; WINDOWS label baseline 193.5; window rows 32..448 x
//!               204.5..248.5, pitch 48; 9 px square at x 44.5, text x 63.5
//!   macropad    keys x 31..163.5 / 173.5..306.5 / 316.5..449, y 70..177.5 /
//!               187.5..295.5 / 305.5..413
//!   usage       name cap-top 80 baseline 92.5, bar 37..443 x 107..117, reset
//!               baseline 137, pitch 90.5
//!   manager     cards 97x142 at x 25.5 + 111 n, y 166.5 (selected: 156.5)
//! Channel, ready card, settings, gallery, naming, confirm, toast and drawer
//! have no mockup; they reuse the measured row and card metrics.
use super::theme::{CORNER_R, HINT_Y, MARGIN, SCREEN};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
}

impl Rect {
    pub const EMPTY: Rect = Rect::new(0, 0, 0, 0);
    pub const SCREEN: Rect = Rect::new(0, 0, SCREEN, SCREEN);

    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            x: x as i16,
            y: y as i16,
            w: w as i16,
            h: h as i16,
        }
    }
    pub const fn left(&self) -> i32 {
        self.x as i32
    }
    pub const fn top(&self) -> i32 {
        self.y as i32
    }
    /// Exclusive.
    pub const fn right(&self) -> i32 {
        self.x as i32 + self.w as i32
    }
    /// Exclusive.
    pub const fn bottom(&self) -> i32 {
        self.y as i32 + self.h as i32
    }
    pub const fn width(&self) -> i32 {
        self.w as i32
    }
    pub const fn height(&self) -> i32 {
        self.h as i32
    }
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left() && x < self.right() && y >= self.top() && y < self.bottom()
    }
    pub const fn center(&self) -> (i32, i32) {
        (self.left() + self.width() / 2, self.top() + self.height() / 2)
    }
    /// Shrink (positive) or grow (negative) on every side.
    pub const fn inset(&self, by: i32) -> Rect {
        Rect::new(
            self.left() + by,
            self.top() + by,
            self.width() - 2 * by,
            self.height() - 2 * by,
        )
    }
    pub const fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.left() + dx, self.top() + dy, self.width(), self.height())
    }
    pub const fn intersects(&self, other: &Rect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.left() < other.right()
            && other.left() < self.right()
            && self.top() < other.bottom()
            && other.top() < self.bottom()
    }
    /// `EMPTY` when they do not overlap.
    pub const fn intersection(&self, other: &Rect) -> Rect {
        if !self.intersects(other) {
            return Rect::EMPTY;
        }
        let x = max(self.left(), other.left());
        let y = max(self.top(), other.top());
        Rect::new(
            x,
            y,
            min(self.right(), other.right()) - x,
            min(self.bottom(), other.bottom()) - y,
        )
    }
    /// Bounding box of both; an empty rect contributes nothing.
    pub const fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = min(self.left(), other.left());
        let y = min(self.top(), other.top());
        Rect::new(
            x,
            y,
            max(self.right(), other.right()) - x,
            max(self.bottom(), other.bottom()) - y,
        )
    }
}

const fn min(a: i32, b: i32) -> i32 {
    if a < b {
        a
    } else {
        b
    }
}
const fn max(a: i32, b: i32) -> i32 {
    if a > b {
        a
    } else {
        b
    }
}

/// Inside the panel and clear of its four 74 px corner arcs.
pub const fn in_safe_area(rect: &Rect) -> bool {
    if rect.is_empty()
        || rect.left() < 0
        || rect.top() < 0
        || rect.right() > SCREEN
        || rect.bottom() > SCREEN
    {
        return false;
    }
    point_in_safe_area(rect.left(), rect.top())
        && point_in_safe_area(rect.right() - 1, rect.top())
        && point_in_safe_area(rect.left(), rect.bottom() - 1)
        && point_in_safe_area(rect.right() - 1, rect.bottom() - 1)
}

pub const fn point_in_safe_area(x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= SCREEN || y >= SCREEN {
        return false;
    }
    let dx = if x < CORNER_R {
        CORNER_R - x
    } else if x >= SCREEN - CORNER_R {
        x - (SCREEN - CORNER_R - 1)
    } else {
        0
    };
    let dy = if y < CORNER_R {
        CORNER_R - y
    } else if y >= SCREEN - CORNER_R {
        y - (SCREEN - CORNER_R - 1)
    } else {
        0
    };
    dx == 0 || dy == 0 || dx * dx + dy * dy <= CORNER_R * CORNER_R
}

/// Full-width row: x 32..448.
const fn row(y: i32, h: i32) -> Rect {
    Rect::new(MARGIN, y, SCREEN - 2 * MARGIN, h)
}

fn index_at(count: u8, rect: impl Fn(u8) -> Rect, x: i32, y: i32) -> Option<u8> {
    (0..count).find(|&index| rect(index).contains(x, y))
}

// ---- chrome shared by every face ---------------------------------------------
/// Header text band: label at `TEXT_X`, tag right-aligned at `TEXT_RIGHT`,
/// both on `HEADER_BASELINE`.
pub const HEADER: Rect = Rect::new(38, 34, 404, 24);
/// State dot of the channel/card header, and where the name starts after it.
pub const HEADER_DOT: (i32, i32, i32) = (44, 46, 5);
pub const HEADER_NAME_X: i32 = 60;
/// A touch that goes Down above this row belongs to the edge gesture.
pub const EDGE_ZONE_H: i32 = 48;
/// Top-edge pill: a notice is waiting in the drawer.
pub const INDICATOR: Rect = Rect::new(223, 10, 34, 5);
/// Bluetooth rune and four RSSI bars; separate from the notification marker.
pub const BLUETOOTH: Rect = Rect::new(374, 10, 52, 20);
/// The toast's whole lane, including its slide-down travel from y = -60 and
/// the header row it blanks on either side of the card.
pub const TOAST_LANE: Rect = Rect::new(32, 0, 416, 68);
pub const TOAST: Rect = Rect::new(48, 8, 384, 60);
pub const HINT_BAR: Rect = Rect::new(0, HINT_Y, SCREEN, SCREEN - HINT_Y);
/// Keycaps and hint text must stay inside this.
pub const HINT_CONTENT: Rect = Rect::new(32, 442, 416, 24);
/// Everything a face may draw between header and hint bar.
pub const BODY: Rect = Rect::new(0, 60, SCREEN, HINT_Y - 1 - 60);

// ---- agents: roster -----------------------------------------------------------
pub const ROSTER_VISIBLE: u8 = 4;
/// Finger travel that scrolls a list by one row (`Target::pitch`): the
/// distance between row tops, rounded.
pub const ROSTER_PITCH: u8 = 78;
/// Visible row 0..4: 74 px tall, pitch 77.5.
pub const fn roster_row(row_index: u8) -> Rect {
    row(70 + row_index as i32 * 155 / 2, 74)
}
pub fn roster_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(ROSTER_VISIBLE, roster_row, x, y)
}
/// Inside a roster row, relative to its origin: dot centre and radius, text
/// left edge, name and title baselines, the active-channel bar.
pub const ROSTER_DOT: (i32, i32, i32) = (21, 37, 5);
pub const ROSTER_TEXT_X: i32 = 40;
pub const ROSTER_NAME_BASELINE: i32 = 33;
pub const ROSTER_TITLE_BASELINE: i32 = 56;
/// 3 px white bar left of the active channel's row.
pub const fn roster_active_bar(row_index: u8) -> Rect {
    let r = roster_row(row_index);
    Rect::new(r.left() - 8, r.top() + 14, 3, r.height() - 28)
}

// ---- agents: channel ------------------------------------------------------------
/// "Question · blocked 2m" under the header.
pub const CHANNEL_STATE: Rect = Rect::new(38, 70, 404, 26);
/// Wrapped reply when an open card needs the summary and action rows below it.
pub const CHANNEL_REPLY: Rect = Rect::new(38, 104, 404, 196);
/// One-line summary of the pending card.
pub const CHANNEL_CARD_TEXT: Rect = Rect::new(38, 310, 404, 28);
/// The reading area ends just above the footer.
pub const CHANNEL_CONTENT_BOTTOM: i32 = 422;

/// A closed card keeps only its summary, against the bottom of the content.
pub const fn channel_card_text(open: bool) -> Rect {
    if open {
        CHANNEL_CARD_TEXT
    } else {
        Rect::new(38, CHANNEL_CONTENT_BOTTOM - 28, 404, 28)
    }
}

/// Use all the space that the current card actually leaves for the reply.
pub const fn channel_reply(card_open: Option<bool>) -> Rect {
    let bottom = match card_open {
        Some(true) => CHANNEL_REPLY.bottom(),
        Some(false) => channel_card_text(false).top() - 10,
        None => CHANNEL_CONTENT_BOTTOM,
    };
    Rect::new(CHANNEL_REPLY.left(), CHANNEL_REPLY.top(), CHANNEL_REPLY.width(), bottom - CHANNEL_REPLY.top())
}
/// One wide button ("Answer · N choices", "Ready summary") ...
pub const CHANNEL_BUTTON: Rect = Rect::new(32, 350, 416, 60);
/// ... or two, labelled with the two choices of a binary question.
pub const fn channel_choice_button(index: u8) -> Rect {
    Rect::new(32 + index as i32 * 213, 350, 203, 60)
}
/// `two`: the binary-question layout is showing. Returns 0 or 1.
pub fn channel_button_at(x: i32, y: i32, two: bool) -> Option<u8> {
    if two {
        index_at(2, channel_choice_button, x, y)
    } else {
        CHANNEL_BUTTON.contains(x, y).then_some(0)
    }
}

// ---- agents: question and ready cards -------------------------------------------
/// Question text, up to two lines of Title.
pub const CARD_QUESTION: Rect = Rect::new(37, 70, 406, 86);
pub const CARD_VISIBLE: u8 = 4;
pub const CARD_PITCH: u8 = 56;
/// Visible choice row 0..4: 48 px tall, pitch 56, the last ending at 388.
pub const fn card_choice(row_index: u8) -> Rect {
    row(172 + row_index as i32 * 56, 48)
}
pub fn card_choice_at(x: i32, y: i32) -> Option<u8> {
    index_at(CARD_VISIBLE, card_choice, x, y)
}
/// Inside a choice row: number chip, text left edge and baseline, where the
/// "recommended" / "sent" tag ends.
pub const CHOICE_CHIP: Rect = Rect::new(14, 13, 22, 22);
pub const CHOICE_TEXT_X: i32 = 49;
pub const CHOICE_BASELINE: i32 = 33;
pub const CHOICE_TAG_RIGHT: i32 = 401;
/// Ready card: title + last reply, then the suggested steps as choice rows.
pub const READY_SUMMARY: Rect = Rect::new(37, 74, 406, 128);
pub const READY_VISIBLE: u8 = 3;
pub const READY_PITCH: u8 = 61;
pub const fn ready_row(row_index: u8) -> Rect {
    row(217 + row_index as i32 * 61, 51)
}
pub fn ready_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(READY_VISIBLE, ready_row, x, y)
}

// ---- PTT overlay (mockup 7) -------------------------------------------------------
pub const PTT_NAME: Rect = Rect::new(38, 36, 243, 28);
pub const PTT_NAME_BASELINE: i32 = 56;
/// LISTENING / "no host" pill.
pub const PTT_PILL: Rect = Rect::new(289, 37, 152, 27);
/// 23 bars, 5 px wide on a 9 px pitch, centred on y = 126.
pub const PTT_WAVE: Rect = Rect::new(39, 95, 203, 62);
pub const PTT_WAVE_BARS: u8 = 23;
pub const PTT_TRANSCRIPT: Rect = Rect::new(38, 176, 404, 204);
/// "Last reply · …", one line.
pub const PTT_LAST_REPLY: Rect = Rect::new(38, 394, 404, 22);
pub const PTT_LAST_REPLY_BASELINE: i32 = 410;

// ---- omarchy (mockup 3) --------------------------------------------------------------
/// "Workspace 2", Display, baseline 103.
pub const OMARCHY_TITLE: Rect = Rect::new(37, 70, 406, 44);
pub const OMARCHY_TITLE_BASELINE: i32 = 103;
pub const OMARCHY_CHIPS: u8 = 9;
/// Workspace chip 0..9 (workspace n+1): 34 px square, pitch 42.
pub const fn omarchy_chip(index: u8) -> Rect {
    Rect::new(37 + index as i32 * 42, 125, 34, 34)
}
pub fn omarchy_chip_at(x: i32, y: i32) -> Option<u8> {
    index_at(OMARCHY_CHIPS, omarchy_chip, x, y)
}
pub const OMARCHY_WINDOWS_LABEL: Rect = Rect::new(37, 180, 200, 16);
pub const OMARCHY_WINDOWS_BASELINE: i32 = 194;
pub const OMARCHY_VISIBLE: u8 = 4;
pub const OMARCHY_PITCH: u8 = 48;
/// Visible window row 0..4: 44 px tall, pitch 48.
pub const fn omarchy_window(row_index: u8) -> Rect {
    row(204 + row_index as i32 * 48, 44)
}
pub fn omarchy_window_at(x: i32, y: i32) -> Option<u8> {
    index_at(OMARCHY_VISIBLE, omarchy_window, x, y)
}
/// Inside a window row: the 9 px tone square, text left edge and baseline.
pub const WINDOW_SQUARE: Rect = Rect::new(12, 18, 9, 9);
pub const WINDOW_TEXT_X: i32 = 31;
pub const WINDOW_BASELINE: i32 = 29;

// ---- macropad (mockup 4) ---------------------------------------------------------------
/// Key 0..9, row-major 3x3: 131x107, pitch 142.5 x 118.
pub const fn macro_key(index: u8) -> Rect {
    let (column, line) = (index as i32 % 3, index as i32 / 3);
    Rect::new(32 + column * 285 / 2, 70 + line * 118, 131, 107)
}
pub fn macro_key_at(x: i32, y: i32) -> Option<u8> {
    index_at(9, macro_key, x, y)
}
/// The key editor: the key previewed at the top (a shorter card), then its
/// rows (label, modifiers, key, save, remove): 46 px tall, pitch 52.
pub const MACRO_EDIT_PREVIEW: Rect = Rect::new((SCREEN - 131) / 2, 68, 131, 72);
pub const MACRO_EDIT_ROWS: u8 = 5;
pub const fn macro_edit_row(row_index: u8) -> Rect {
    row(150 + row_index as i32 * 52, 46)
}
pub fn macro_edit_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(MACRO_EDIT_ROWS, macro_edit_row, x, y)
}

// ---- usage (mockup 5) ----------------------------------------------------------------------
pub const USAGE_VISIBLE: u8 = 4;
pub const USAGE_PITCH: u8 = 91;
/// Visible provider block 0..4: 84 px tall, pitch 90.5.
pub const fn usage_row(row_index: u8) -> Rect {
    row(72 + row_index as i32 * 181 / 2, 84)
}
pub fn usage_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(USAGE_VISIBLE, usage_row, x, y)
}
/// Inside a usage block: name/percent baseline, the bar, the reset baseline.
pub const USAGE_NAME_BASELINE: i32 = 21;
pub const USAGE_BAR: Rect = Rect::new(5, 35, 406, 10);
pub const USAGE_RESET_BASELINE: i32 = 66;

// ---- settings (no mockup) --------------------------------------------------------------------
pub const SETTINGS_PANELS: u8 = 6;
/// Panel tab 0..6 (Volume, Speech, Alerts, Display, Host, About).
pub const fn settings_tab(index: u8) -> Rect {
    Rect::new(32 + index as i32 * 70, 68, 66, 34)
}
pub fn settings_tab_at(x: i32, y: i32) -> Option<u8> {
    index_at(SETTINGS_PANELS, settings_tab, x, y)
}
pub const SETTINGS_ROWS: u8 = 4;
/// Setting row 0..4: 58 px tall, pitch 66.
pub const fn settings_row(row_index: u8) -> Rect {
    row(120 + row_index as i32 * 66, 58)
}
pub fn settings_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(SETTINGS_ROWS, settings_row, x, y)
}
/// Inside a settings row: label left edge, baseline, where the value ends,
/// and the level bar of a 0..=10 / 1..=5 setting.
pub const SETTING_LABEL_X: i32 = 16;
pub const SETTING_BASELINE: i32 = 36;
pub const SETTING_VALUE_RIGHT: i32 = 400;
pub const SETTING_BAR: Rect = Rect::new(214, 24, 132, 10);

// ---- timer (no mockup) -----------------------------------------------------------------------
/// Mode tabs (Stopwatch, Timer, Pomodoro) under the header.
pub const TIMER_TABS: u8 = 3;
pub const fn timer_tab(index: u8) -> Rect {
    Rect::new(32 + index as i32 * 140, 68, 136, 34)
}
pub fn timer_tab_at(x: i32, y: i32) -> Option<u8> {
    index_at(TIMER_TABS, timer_tab, x, y)
}
/// The big readout; its digits are `TIMER_DIGIT_H` tall, centred in it.
pub const TIMER_READOUT: Rect = Rect::new(32, 116, 416, 116);
pub const TIMER_DIGIT_H: i32 = 96;
/// Timer: the progress bar under the readout.
pub const TIMER_BAR: Rect = Rect::new(38, 244, 404, 10);
/// Timer: the length presets (idle), or the status line in their place.
pub const TIMER_PRESETS: u8 = 5;
pub const fn timer_preset(index: u8) -> Rect {
    Rect::new(32 + index as i32 * 84, 278, 80, 44)
}
pub fn timer_preset_at(x: i32, y: i32) -> Option<u8> {
    index_at(TIMER_PRESETS, timer_preset, x, y)
}
pub const TIMER_STATUS: Rect = row(278, 44);
/// Stopwatch: the newest laps, one line each.
pub const TIMER_LAPS: u8 = 3;
pub const fn timer_lap(index: u8) -> Rect {
    row(242 + index as i32 * 32, 30)
}
/// Left (Lap / Reset) and right (Start / Pause / Resume / Restart / Next) buttons.
pub const TIMER_SECONDARY: Rect = Rect::new(32, 348, 202, 62);
pub const TIMER_PRIMARY: Rect = Rect::new(246, 348, 202, 62);

// ---- face manager (mockup 2), gallery, naming, confirm ---------------------------------------
pub const MANAGER_VISIBLE: u8 = 4;
/// The selected card is drawn this much higher.
pub const MANAGER_LIFT: i32 = 10;
/// Viewport slot 0..4: 97x141, pitch 111.
pub const fn manager_card(slot: u8) -> Rect {
    Rect::new(26 + slot as i32 * 111, 167, 97, 141)
}
/// Hit area of a slot: the card in both its resting and lifted position.
pub const fn manager_slot(slot: u8) -> Rect {
    let card = manager_card(slot);
    Rect::new(
        card.left(),
        card.top() - MANAGER_LIFT,
        card.width(),
        card.height() + MANAGER_LIFT,
    )
}
pub fn manager_card_at(x: i32, y: i32) -> Option<u8> {
    index_at(MANAGER_VISIBLE, manager_slot, x, y)
}
/// "\u{2039} 1 more" / "2 more \u{203a}" under the row's edges.
pub const MANAGER_MORE: Rect = Rect::new(26, 322, 428, 30);
/// Inside a manager card: label baseline and left edge, thumbnail sketch area.
pub const THUMB_LABEL: (i32, i32) = (11, 24);
pub const THUMB_SKETCH: Rect = Rect::new(11, 34, 75, 96);
/// "Add a face" and the template rows under it.
pub const GALLERY_TITLE: Rect = Rect::new(37, 70, 406, 40);
pub const GALLERY_ROWS: u8 = 6;
/// Template row 0..6: 48 px tall, pitch 52.
pub const fn gallery_row(row_index: u8) -> Rect {
    row(112 + row_index as i32 * 52, 48)
}
/// Inside a gallery row: the label and blurb baseline.
pub const GALLERY_BASELINE: i32 = 32;
pub fn gallery_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(GALLERY_ROWS, gallery_row, x, y)
}
/// "Name this face", the name field, and the hold-PTT help text.
pub const NAMING_TITLE: Rect = Rect::new(37, 70, 406, 40);
pub const NAMING_FIELD: Rect = row(140, 64);
pub const NAMING_HELP: Rect = Rect::new(38, 228, 404, 72);
/// Remove-confirm dialog over the dimmed manager.
pub const CONFIRM_BOX: Rect = Rect::new(48, 140, 384, 196);
pub const CONFIRM_TEXT: Rect = Rect::new(68, 160, 344, 84);
/// Button 0 = keep (CANCEL), 1 = remove (ENTER).
pub const fn confirm_button(index: u8) -> Rect {
    Rect::new(68 + index as i32 * 180, 264, 164, 52)
}
pub fn confirm_button_at(x: i32, y: i32) -> Option<u8> {
    index_at(2, confirm_button, x, y)
}

// ---- toast and drawer ---------------------------------------------------------------------------
/// Inside the toast: dot centre and radius, text left edge, the two baselines.
pub const TOAST_DOT: (i32, i32, i32) = (22, 30, 5);
pub const TOAST_TEXT_X: i32 = 40;
pub const TOAST_NAME_BASELINE: i32 = 25;
pub const TOAST_TEXT_BASELINE: i32 = 47;
pub fn toast_at(x: i32, y: i32) -> bool {
    TOAST.contains(x, y)
}
pub const DRAWER_VISIBLE: u8 = 5;
pub const DRAWER_PITCH: u8 = 62;
/// Visible notice row 0..5: 54 px tall, pitch 62.
pub const fn drawer_row(row_index: u8) -> Rect {
    row(70 + row_index as i32 * 62, 54)
}
pub fn drawer_row_at(x: i32, y: i32) -> Option<u8> {
    index_at(DRAWER_VISIBLE, drawer_row, x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn indexed(count: u8, rect: impl Fn(u8) -> Rect) -> Vec<Rect> {
        (0..count).map(rect).collect()
    }

    /// Every view: (name, rects that hold content and must not overlap).
    fn views() -> Vec<(&'static str, Vec<Rect>)> {
        let chrome = [HEADER, HINT_CONTENT];
        let with_chrome = |rects: Vec<Rect>| chrome.iter().copied().chain(rects).collect();
        std::vec![
            ("roster", with_chrome(indexed(ROSTER_VISIBLE, roster_row))),
            ("channel-reply", with_chrome(std::vec![CHANNEL_STATE, channel_reply(None)])),
            ("channel-closed", with_chrome(std::vec![CHANNEL_STATE, channel_reply(Some(false)), channel_card_text(false)])),
            (
                "channel",
                with_chrome(std::vec![
                    CHANNEL_STATE,
                    CHANNEL_REPLY,
                    CHANNEL_CARD_TEXT,
                    CHANNEL_BUTTON
                ])
            ),
            (
                "channel-binary",
                with_chrome(
                    [CHANNEL_STATE, CHANNEL_REPLY, CHANNEL_CARD_TEXT]
                        .into_iter()
                        .chain(indexed(2, channel_choice_button))
                        .collect()
                )
            ),
            (
                "question",
                with_chrome(
                    [CARD_QUESTION]
                        .into_iter()
                        .chain(indexed(CARD_VISIBLE, card_choice))
                        .collect()
                )
            ),
            (
                "ready",
                with_chrome(
                    [READY_SUMMARY]
                        .into_iter()
                        .chain(indexed(READY_VISIBLE, ready_row))
                        .collect()
                )
            ),
            (
                "ptt",
                std::vec![
                    PTT_NAME,
                    PTT_PILL,
                    PTT_WAVE,
                    PTT_TRANSCRIPT,
                    PTT_LAST_REPLY,
                    HINT_CONTENT
                ]
            ),
            (
                "omarchy",
                with_chrome(
                    [OMARCHY_TITLE, OMARCHY_WINDOWS_LABEL]
                        .into_iter()
                        .chain(indexed(OMARCHY_CHIPS, omarchy_chip))
                        .chain(indexed(OMARCHY_VISIBLE, omarchy_window))
                        .collect()
                )
            ),
            ("macropad", with_chrome(indexed(9, macro_key))),
            ("usage", with_chrome(indexed(USAGE_VISIBLE, usage_row))),
            (
                "settings",
                with_chrome(
                    indexed(SETTINGS_PANELS, settings_tab)
                        .into_iter()
                        .chain(indexed(SETTINGS_ROWS, settings_row))
                        .collect()
                )
            ),
            (
                "stopwatch",
                with_chrome(
                    [TIMER_READOUT, TIMER_SECONDARY, TIMER_PRIMARY]
                        .into_iter()
                        .chain(indexed(TIMER_TABS, timer_tab))
                        .chain(indexed(TIMER_LAPS, timer_lap))
                        .collect()
                )
            ),
            (
                "timer",
                with_chrome(
                    [TIMER_READOUT, TIMER_BAR, TIMER_SECONDARY, TIMER_PRIMARY]
                        .into_iter()
                        .chain(indexed(TIMER_TABS, timer_tab))
                        .chain(indexed(TIMER_PRESETS, timer_preset))
                        .collect()
                )
            ),
            ("timer-status", with_chrome(std::vec![TIMER_BAR, TIMER_STATUS])),
            (
                "manager",
                with_chrome(
                    [MANAGER_MORE]
                        .into_iter()
                        .chain(indexed(MANAGER_VISIBLE, manager_slot))
                        .collect()
                )
            ),
            (
                "gallery",
                with_chrome(
                    [GALLERY_TITLE]
                        .into_iter()
                        .chain(indexed(GALLERY_ROWS, gallery_row))
                        .collect()
                )
            ),
            (
                "naming",
                with_chrome(std::vec![NAMING_TITLE, NAMING_FIELD, NAMING_HELP])
            ),
            (
                "confirm",
                [CONFIRM_TEXT]
                    .into_iter()
                    .chain(indexed(2, confirm_button))
                    .collect()
            ),
            ("toast", std::vec![TOAST]),
            ("indicator", std::vec![INDICATOR]),
            ("drawer", with_chrome(indexed(DRAWER_VISIBLE, drawer_row))),
        ]
    }

    #[test]
    fn every_content_rect_is_inside_the_panel_and_its_corner_arcs() {
        for (view, rects) in views() {
            for rect in rects {
                assert!(in_safe_area(&rect), "{view}: {rect:?}");
            }
        }
        for rect in [HINT_BAR, TOAST_LANE, BODY, CONFIRM_BOX, Rect::SCREEN] {
            assert!(Rect::SCREEN.intersection(&rect) == rect, "{rect:?}");
        }
        for slot in 0..ROSTER_VISIBLE {
            assert!(in_safe_area(&roster_active_bar(slot)));
        }
    }

    #[test]
    fn rects_of_one_view_never_overlap() {
        for (view, rects) in views() {
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    assert!(!a.intersects(b), "{view}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_centre_of_every_target_hits_that_target_and_gaps_hit_nothing() {
        fn check(count: u8, rect: impl Fn(u8) -> Rect, at: impl Fn(i32, i32) -> Option<u8>) {
            for index in 0..count {
                let r = rect(index);
                let (x, y) = r.center();
                assert_eq!(at(x, y), Some(index), "{r:?}");
                assert_eq!(at(r.left(), r.top()), Some(index));
                assert_eq!(at(r.right() - 1, r.bottom() - 1), Some(index));
                assert_ne!(at(r.left() - 1, y), Some(index));
                assert_ne!(at(x, r.bottom()), Some(index));
            }
            assert_eq!(at(0, 0), None);
            assert_eq!(at(479, 479), None);
        }
        check(ROSTER_VISIBLE, roster_row, roster_row_at);
        check(CARD_VISIBLE, card_choice, card_choice_at);
        check(READY_VISIBLE, ready_row, ready_row_at);
        check(OMARCHY_CHIPS, omarchy_chip, omarchy_chip_at);
        check(OMARCHY_VISIBLE, omarchy_window, omarchy_window_at);
        check(9, macro_key, macro_key_at);
        check(USAGE_VISIBLE, usage_row, usage_row_at);
        check(SETTINGS_PANELS, settings_tab, settings_tab_at);
        check(SETTINGS_ROWS, settings_row, settings_row_at);
        check(MANAGER_VISIBLE, manager_slot, manager_card_at);
        check(GALLERY_ROWS, gallery_row, gallery_row_at);
        check(TIMER_TABS, timer_tab, timer_tab_at);
        check(TIMER_PRESETS, timer_preset, timer_preset_at);
        check(2, confirm_button, confirm_button_at);
        check(DRAWER_VISIBLE, drawer_row, drawer_row_at);
        check(2, channel_choice_button, |x, y| channel_button_at(x, y, true));
        check(1, |_| CHANNEL_BUTTON, |x, y| channel_button_at(x, y, false));
        assert!(toast_at(240, 38) && !toast_at(240, 69) && !toast_at(40, 38));
    }

    #[test]
    fn measured_geometry_matches_the_mockups() {
        assert_eq!(roster_row(0), Rect::new(32, 70, 416, 74));
        assert_eq!(roster_row(3).top(), 302);
        // Lower and tighter than mockup 8 (278, 51 tall): the larger type ramp
        // needs room for three question lines above the choices.
        assert_eq!(card_choice(2), Rect::new(32, 284, 416, 48));
        assert_eq!(omarchy_chip(1), Rect::new(79, 125, 34, 34));
        assert_eq!(omarchy_window(1).top(), 252);
        assert_eq!(macro_key(4), Rect::new(174, 188, 131, 107));
        assert_eq!(macro_key(8).right(), 448);
        assert_eq!(macro_key(8).bottom(), 413);
        assert_eq!(usage_row(2).top(), 253);
        assert_eq!(manager_card(3), Rect::new(359, 167, 97, 141));
        assert_eq!((TOAST.left(), TOAST.right(), TOAST.top(), TOAST.bottom()), (48, 432, 8, 68));
        assert_eq!((INDICATOR.left(), INDICATOR.right()), (223, 257));
    }

    #[test]
    fn nothing_tappable_starts_inside_the_edge_gesture_zone_except_the_toast() {
        for (view, rects) in views() {
            if matches!(view, "toast" | "indicator" | "ptt") {
                continue;
            }
            for rect in rects {
                assert!(rect == HEADER || rect.top() >= EDGE_ZONE_H, "{view}: {rect:?}");
            }
        }
    }

    #[test]
    fn rect_algebra() {
        let a = Rect::new(10, 10, 20, 20);
        let b = Rect::new(20, 25, 20, 20);
        assert_eq!(a.intersection(&b), Rect::new(20, 25, 10, 5));
        assert_eq!(a.union(&b), Rect::new(10, 10, 30, 35));
        assert_eq!(a.union(&Rect::EMPTY), a);
        assert_eq!(a.intersection(&Rect::new(30, 10, 5, 5)), Rect::EMPTY);
        assert_eq!(a.inset(2), Rect::new(12, 12, 16, 16));
        assert_eq!(a.offset(-10, 5), Rect::new(0, 15, 20, 20));
        assert!(a.contains(10, 10) && !a.contains(30, 10) && !a.contains(10, 30));
        assert!(!point_in_safe_area(0, 0) && point_in_safe_area(22, 22) && !point_in_safe_area(21, 21));
        assert!(point_in_safe_area(240, 0) && point_in_safe_area(0, 240));
    }
}
