//! Timer face: TIMER header, three tabs (Stopwatch, Timer, Pomodoro), a big
//! seven-segment readout, then per mode either the stopwatch's newest laps or
//! the timer's progress bar and its length presets (a status line while it
//! runs), and the two action buttons over the hint bar.
//!
//! The readout is drawn from rounded segments, not the type ramp: its 96 px
//! digits are more than twice the largest atlas, and ten digits of segments
//! cost no flash. Unlit segments are kept as a faint ghost, as on a real
//! segment display. A tab whose clock runs while the other is shown carries a
//! dot.
//!
//! Signature: header, tabs, readout, the two detail lanes, each button, hint
//! bar. The readout hashes what it shows (tenths or seconds), never `now_ms`.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::layout::{self, Rect};
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::timer::{Action, Mode, Phase, PRESETS_S, PRESET_ID, PRIMARY_ID, SECONDARY_ID, TAB_ID};
use crate::ui::state::UiState;
use crate::ui::{font, font::Align, font::Font, kit, theme};

const TABS: Rect = Rect::new(theme::MARGIN, 68, theme::SCREEN - 2 * theme::MARGIN, 34);
/// The detail lanes: bar / first lap row, then presets / status / laps.
const DETAIL_TOP: Rect = Rect::new(theme::MARGIN, 240, theme::SCREEN - 2 * theme::MARGIN, 18);
const DETAIL_REST: Rect = Rect::new(theme::MARGIN, 258, theme::SCREEN - 2 * theme::MARGIN, 82);
/// A faint segment that is off.
const GHOST: u32 = 0x0E0F12;
/// The timer turns amber for its last seconds.
const WARN_MS: u32 = 10_000;

// ---- seven-segment readout ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Glyph {
    Digit(u8),
    Colon,
    Point,
    /// The stopwatch's tenths, smaller and on the baseline.
    Small(u8),
}

/// Segments a..g as bits 0..6, for 0..9.
const SEGMENTS: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Readout {
    glyphs: [Glyph; 10],
    len: usize,
}

impl Readout {
    fn new() -> Self {
        Self { glyphs: [Glyph::Colon; 10], len: 0 }
    }
    fn push(&mut self, glyph: Glyph) {
        if self.len < self.glyphs.len() {
            self.glyphs[self.len] = glyph;
            self.len += 1;
        }
    }
    fn two(&mut self, value: u32) {
        self.push(Glyph::Digit((value / 10 % 10) as u8));
        self.push(Glyph::Digit((value % 10) as u8));
    }
    fn glyphs(&self) -> &[Glyph] {
        &self.glyphs[..self.len]
    }
}

/// "H:MM:SS" from an hour, else "M:SS" ("MM:SS" with `pad`); then ".t" when
/// `tenths` is given.
fn readout(seconds: u32, pad: bool, tenths: Option<u32>) -> Readout {
    let mut out = Readout::new();
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        if h >= 10 {
            out.push(Glyph::Digit((h / 10 % 10) as u8));
        }
        out.push(Glyph::Digit((h % 10) as u8));
        out.push(Glyph::Colon);
        out.two(m);
    } else if pad || m >= 10 {
        out.two(m);
    } else {
        out.push(Glyph::Digit(m as u8));
    }
    out.push(Glyph::Colon);
    out.two(s);
    if let Some(tenths) = tenths {
        out.push(Glyph::Point);
        out.push(Glyph::Small((tenths % 10) as u8));
    }
    out
}

/// The readout of the current mode at `now_ms`.
fn current(ui: &UiState) -> Readout {
    let view = &ui.timer;
    match view.mode {
        Mode::Stopwatch => {
            let ms = view.stopwatch_ms(ui.now_ms);
            let seconds = ms / 1000;
            // Tenths only while they fit beside the hours.
            readout(seconds, true, (seconds < 10 * 3600).then_some(ms / 100))
        }
        Mode::Countdown => readout(view.remaining_ms(ui.now_ms).div_ceil(1000), false, None),
        Mode::Pomodoro => readout(view.pomodoro.remaining_ms(ui.now_ms).div_ceil(1000), false, None),
    }
}

fn readout_color(ui: &UiState) -> u32 {
    let view = &ui.timer;
    match view.mode {
        Mode::Stopwatch if view.stopwatch.running() => theme::TEXT_PRIMARY,
        Mode::Stopwatch if view.stopwatch.banked_ms > 0 => theme::TEXT_SECONDARY,
        Mode::Stopwatch => theme::TEXT_LABEL,
        Mode::Countdown if view.done => theme::RED,
        Mode::Countdown if view.countdown.running() && view.remaining_ms(ui.now_ms) <= WARN_MS => theme::AMBER,
        Mode::Countdown if view.countdown.running() || view.countdown_idle() => theme::TEXT_PRIMARY,
        Mode::Countdown => theme::TEXT_SECONDARY,
        Mode::Pomodoro if view.pomodoro.done => theme::GREEN,
        Mode::Pomodoro if view.pomodoro.run.running() && view.pomodoro.remaining_ms(ui.now_ms) <= WARN_MS => theme::AMBER,
        Mode::Pomodoro if view.pomodoro.run.running() || view.pomodoro.idle() => phase_color(view.pomodoro.phase),
        Mode::Pomodoro => theme::TEXT_SECONDARY,
    }
}

fn phase_color(phase: Phase) -> u32 {
    match phase {
        Phase::Focus => theme::TEXT_PRIMARY,
        Phase::ShortBreak | Phase::LongBreak => theme::GREEN,
    }
}

/// Glyph sizes for digits `h` px tall: (width, gap after).
fn width(glyph: Glyph, h: i32) -> i32 {
    match glyph {
        Glyph::Digit(_) => h * 9 / 16,
        Glyph::Colon => h / 5,
        Glyph::Point => h / 6,
        Glyph::Small(_) => small_h(h) * 9 / 16,
    }
}

fn small_h(h: i32) -> i32 {
    h * 5 / 8
}

fn gap(h: i32) -> i32 {
    h / 10
}

fn total_width(glyphs: &[Glyph], h: i32) -> i32 {
    let widths: i32 = glyphs.iter().map(|&glyph| width(glyph, h)).sum();
    widths + gap(h) * (glyphs.len() as i32 - 1).max(0)
}

/// The digit height that fits `glyphs` across the readout.
fn fitted_height(glyphs: &[Glyph]) -> i32 {
    let room = layout::TIMER_READOUT.width() - 12;
    let mut h = layout::TIMER_DIGIT_H;
    // The glyph metrics round per glyph: shrink until the sum fits.
    while h > 16 && total_width(glyphs, h) > room {
        h -= 1;
    }
    h
}

fn digit(band: &mut Band, x: i32, y: i32, h: i32, value: u8, color: u32) {
    let w = h * 9 / 16;
    let t = (h / 8).max(3);
    let g = (t / 6).max(1);
    let r = (t / 2) as u32;
    let across = w - t - 2 * g;
    let down = (h - t) / 2 - 2 * g;
    let bits = SEGMENTS[(value % 10) as usize];
    let segments = [
        Rect::new(x + t / 2 + g, y, across, t),                 // a
        Rect::new(x + w - t, y + t / 2 + g, t, down),           // b
        Rect::new(x + w - t, y + h / 2 + g, t, down),           // c
        Rect::new(x + t / 2 + g, y + h - t, across, t),         // d
        Rect::new(x, y + h / 2 + g, t, down),                   // e
        Rect::new(x, y + t / 2 + g, t, down),                   // f
        Rect::new(x + t / 2 + g, y + h / 2 - t / 2, across, t), // g
    ];
    for (index, rect) in segments.into_iter().enumerate() {
        let lit = bits & (1 << index) != 0;
        kit::round(band, rect, r, if lit { color } else { GHOST });
    }
}

fn draw_readout(band: &mut Band, ui: &UiState) {
    let rect = layout::TIMER_READOUT;
    if !kit::visible(band, rect) {
        return;
    }
    let readout = current(ui);
    let glyphs = readout.glyphs();
    let color = readout_color(ui);
    let h = fitted_height(glyphs);
    let t = (h / 8).max(3);
    let (cx, cy) = rect.center();
    let top = cy - h / 2;
    let mut x = cx - total_width(glyphs, h) / 2;
    for &glyph in glyphs {
        let w = width(glyph, h);
        match glyph {
            Glyph::Digit(value) => digit(band, x, top, h, value, color),
            Glyph::Small(value) => {
                let small = small_h(h);
                digit(band, x, top + h - small, small, value, color);
            }
            Glyph::Colon => {
                for dy in [h * 3 / 10, h * 7 / 10] {
                    kit::dot(band, x + w / 2, top + dy, t / 2, color);
                }
            }
            Glyph::Point => kit::dot(band, x + w / 2, top + h - t / 2 - 1, t / 2, color),
        }
        x += w + gap(h);
    }
}

// ---- text helpers ---------------------------------------------------------------------------

/// "1:05.3", "12:00.0", "1:02:03.4"; without tenths "5:00", "1:00:00".
fn clock_text(ms: u32, tenths: bool, out: &mut Text<16>) {
    let seconds = if tenths { ms / 1000 } else { ms.div_ceil(1000) };
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let digits = |value: u32, pad: bool, out: &mut Text<16>| {
        if pad || value >= 10 {
            out.push_char((b'0' + (value / 10 % 10) as u8) as char);
        }
        out.push_char((b'0' + (value % 10) as u8) as char);
    };
    out.clear();
    if h > 0 {
        digits(h, false, out);
        out.push_char(':');
    }
    digits(m, h > 0, out);
    out.push_char(':');
    digits(s, true, out);
    if tenths {
        out.push_char('.');
        out.push_char((b'0' + (ms / 100 % 10) as u8) as char);
    }
}

fn preset_label(seconds: u32, out: &mut Text<16>) {
    let minutes = seconds / 60;
    out.clear();
    if minutes >= 10 {
        out.push_char((b'0' + (minutes / 10 % 10) as u8) as char);
    }
    out.push_char((b'0' + (minutes % 10) as u8) as char);
    out.push_str(" min");
}

/// The status line that replaces the presets once the timer has started.
fn status(ui: &UiState, out: &mut Text<16>) -> (&'static str, u32) {
    let view = &ui.timer;
    if view.done {
        out.clear();
        return ("Time\u{2019}s up", theme::RED);
    }
    clock_text(view.length_ms, false, out);
    if view.countdown.running() {
        ("of ", theme::TEXT_LABEL)
    } else {
        ("Paused \u{00B7} of ", theme::TEXT_LABEL)
    }
}

/// The timer's bar: what is left, full while idle; red and full once done.
fn bar(ui: &UiState) -> (u8, u32) {
    let view = &ui.timer;
    if view.done {
        return (100, theme::RED);
    }
    let left = view.remaining_ms(ui.now_ms) as u64 * 100 / view.length_ms.max(1) as u64;
    let color = if view.countdown.running() { theme::BLUE } else { theme::GRAY };
    (left.min(100) as u8, color)
}

// ---- pieces -----------------------------------------------------------------------------------

fn draw_chip(band: &mut Band, rect: Rect, name: &str, selected: bool, pressed: bool) {
    if pressed && !selected {
        kit::round(band, rect, theme::CHIP_R, theme::CHIP_FILL);
        kit::outline(band, rect, theme::CHIP_R, theme::OUTLINE, 1);
        let (cx, cy) = rect.center();
        font::draw(band, Font::Small, cx, cy + font::ascent(Font::Small) * 9 / 20, name, theme::TEXT_PRIMARY, Some(theme::CHIP_FILL), rect.width() - 8, Align::Center);
    } else {
        kit::chip(band, rect, name, selected);
    }
}

/// Dot on a tab: that mode's clock is running (or the timer is done).
fn tab_dot(ui: &UiState, mode: Mode) -> Option<u32> {
    let view = &ui.timer;
    match mode {
        Mode::Stopwatch => view.stopwatch.running().then_some(theme::GREEN),
        Mode::Countdown if view.done => Some(theme::RED),
        Mode::Countdown => view.countdown.running().then_some(theme::GREEN),
        Mode::Pomodoro if view.pomodoro.done => Some(theme::AMBER),
        Mode::Pomodoro => view.pomodoro.run.running().then_some(theme::GREEN),
    }
}

fn draw_tabs(band: &mut Band, ui: &UiState) {
    if !kit::visible(band, TABS) {
        return;
    }
    for (index, mode) in Mode::ALL.into_iter().enumerate() {
        let rect = layout::timer_tab(index as u8);
        let selected = mode == ui.timer.mode;
        draw_chip(band, rect, mode.label(), selected, ui.timer.pressed == Some((TAB_ID + index as u16) as u8));
        if let Some(color) = tab_dot(ui, mode).filter(|_| !selected) {
            let (_, cy) = rect.center();
            kit::dot(band, rect.right() - 10, cy, 3, color);
        }
    }
}

fn draw_laps(band: &mut Band, ui: &UiState) {
    let view = &ui.timer;
    let mut text = Text::<16>::new();
    for slot in 0..layout::TIMER_LAPS {
        let rect = layout::timer_lap(slot);
        if (slot as u16) >= view.lap_count || !kit::visible(band, rect) {
            continue;
        }
        let baseline = rect.top() + 22;
        text.clear();
        text.push_str("Lap ");
        let number = view.lap_count - slot as u16;
        let mut digits = [0u8; 5];
        let mut n = number;
        let mut len = 0;
        while n > 0 || len == 0 {
            digits[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        for &d in digits[..len].iter().rev() {
            text.push_char(d as char);
        }
        let label = if slot == 0 { theme::TEXT_ROW } else { theme::TEXT_LABEL };
        font::draw(band, Font::Row, theme::TEXT_X, baseline, text.as_str(), label, Some(theme::CANVAS), 160, Align::Left);
        clock_text(view.laps[slot as usize], true, &mut text);
        font::draw(band, Font::Row, theme::TEXT_RIGHT, baseline, text.as_str(), label, Some(theme::CANVAS), 200, Align::Right);
    }
}

fn draw_countdown_detail(band: &mut Band, ui: &UiState) {
    let (percent, color) = bar(ui);
    kit::bar(band, layout::TIMER_BAR, percent, color);
    let view = &ui.timer;
    let mut text = Text::<16>::new();
    if view.countdown_idle() {
        for (index, &seconds) in PRESETS_S.iter().enumerate() {
            let rect = layout::timer_preset(index as u8);
            if !kit::visible(band, rect) {
                continue;
            }
            preset_label(seconds, &mut text);
            let pressed = view.pressed == Some((PRESET_ID + index as u16) as u8);
            draw_chip(band, rect, text.as_str(), view.length_ms == seconds * 1000, pressed);
        }
        return;
    }
    let lane = layout::TIMER_STATUS;
    if !kit::visible(band, lane) {
        return;
    }
    let (lead, color) = status(ui, &mut text);
    let baseline = lane.top() + 29;
    let lead_w = font::measure(Font::Row, lead);
    let total = lead_w + font::measure(Font::Row, text.as_str());
    let x = lane.center().0 - total / 2;
    font::draw(band, Font::Row, x, baseline, lead, color, Some(theme::CANVAS), lane.width(), Align::Left);
    font::draw(band, Font::Row, x + lead_w, baseline, text.as_str(), theme::TEXT_ROW, Some(theme::CANVAS), lane.width() - lead_w, Align::Left);
}

/// Four completed-focus markers and the next stage of the cycle.
fn draw_pomodoro_detail(band: &mut Band, ui: &UiState) {
    let view = &ui.timer.pomodoro;
    let (percent, color) = pomodoro_bar(ui);
    kit::bar(band, layout::TIMER_BAR, percent, color);
    if !kit::visible(band, DETAIL_REST) {
        return;
    }
    let mut text = Text::<48>::new();
    text.push_str(view.phase.label());
    if view.phase == Phase::Focus {
        text.push_char(' ');
        let session = if view.done { view.completed } else { view.completed + 1 };
        text.push_char((b'0' + session.min(4)) as char);
        text.push_str(" of 4");
    }
    font::draw(band, Font::Row, 38, 285, text.as_str(), theme::TEXT_ROW, Some(theme::CANVAS), 250, Align::Left);
    for index in 0..4 {
        let rect = Rect::new(326 + index * 28, 272, 12, 12);
        if index < view.completed as i32 {
            kit::round(band, rect, 6, theme::GREEN);
        } else {
            let active = view.phase == Phase::Focus && index == view.completed as i32;
            kit::outline(band, rect, 6, if active { theme::TEXT_ROW } else { theme::OUTLINE }, 1);
        }
    }
    text.clear();
    if view.done {
        text.push_str("Next: ");
        let next = view.next_phase();
        text.push_str(next.label());
        text.push_str(" \u{00B7} ");
        let mut length = Text::<16>::new();
        preset_label(next.duration_ms() / 1000, &mut length);
        text.push_str(length.as_str());
    } else if view.idle() {
        text.push_str("25 min focus \u{00B7} 5 min break");
    } else {
        if !view.run.running() {
            text.push_str("Paused \u{00B7} ");
        }
        text.push_str("Next: ");
        text.push_str(view.next_phase().label());
    }
    font::draw(band, Font::Small, DETAIL_REST.center().0, 321, text.as_str(), if view.done { theme::GREEN } else { theme::TEXT_LABEL }, Some(theme::CANVAS), DETAIL_REST.width() - 12, Align::Center);
}

fn pomodoro_bar(ui: &UiState) -> (u8, u32) {
    let view = &ui.timer.pomodoro;
    if view.done {
        return (100, theme::GREEN);
    }
    let left = view.remaining_ms(ui.now_ms) as u64 * 100 / view.phase.duration_ms() as u64;
    let color = if !view.run.running() {
        theme::GRAY
    } else if view.phase == Phase::Focus {
        theme::BLUE
    } else {
        theme::GREEN
    };
    (left.min(100) as u8, color)
}

/// (fill, label colour) of the right button.
fn primary_colors(action: Action) -> (u32, u32) {
    match action {
        Action::Pause => (theme::AMBER, theme::CANVAS),
        Action::Start | Action::Resume | Action::Restart | Action::Next => (theme::GREEN, theme::CANVAS),
        Action::Lap | Action::Reset => (theme::CHIP_FILL, theme::TEXT_PRIMARY),
    }
}

fn draw_buttons(band: &mut Band, ui: &UiState) {
    let view = &ui.timer;
    let radius = (layout::TIMER_PRIMARY.height() / 2) as u32;
    let rect = layout::TIMER_SECONDARY;
    if kit::visible(band, rect) {
        let enabled = view.secondary().is_some();
        let pressed = enabled && view.pressed == Some(SECONDARY_ID as u8);
        let fill = if pressed { theme::KEYCAP_FILL } else if enabled { theme::CHIP_FILL } else { theme::CANVAS };
        kit::round(band, rect, radius, fill);
        kit::outline(band, rect, radius, if pressed { theme::OUTLINE_SELECTED } else { theme::OUTLINE_KEY }, 1);
        let fg = if enabled { theme::TEXT_PRIMARY } else { theme::TEXT_FAINT };
        let (cx, cy) = rect.center();
        font::draw(band, Font::Name, cx, cy + font::ascent(Font::Name) * 9 / 20, view.secondary_label(), fg, Some(fill), rect.width() - 24, Align::Center);
    }
    let rect = layout::TIMER_PRIMARY;
    if kit::visible(band, rect) {
        let action = view.primary();
        let (fill, fg) = primary_colors(action);
        kit::round(band, rect, radius, fill);
        if view.pressed == Some(PRIMARY_ID as u8) {
            kit::outline(band, rect, radius, theme::OUTLINE_SELECTED, 2);
        }
        let (cx, cy) = rect.center();
        font::draw(band, Font::Name, cx, cy + font::ascent(Font::Name) * 9 / 20, action.label(), fg, Some(fill), rect.width() - 24, Align::Center);
    }
}

fn hints(ui: &UiState) -> ([kit::Hint<'static>; 4], usize) {
    const fn hint(key: &'static str, label: &'static str) -> kit::Hint<'static> {
        kit::Hint { key, label }
    }
    let view = &ui.timer;
    let mut out = [hint("", ""); 4];
    let mut len = 0;
    let mut push = |h: kit::Hint<'static>| {
        out[len] = h;
        len += 1;
    };
    push(hint("ENTER", action_hint(view.primary())));
    if let Some(action) = view.secondary() {
        push(hint("CANCEL", action_hint(action)));
    }
    if view.mode == Mode::Countdown && view.countdown_idle() {
        push(hint("SCROLL", "length"));
    }
    push(hint("\u{2039} \u{203a}", "mode"));
    (out, len)
}

const fn action_hint(action: Action) -> &'static str {
    match action {
        Action::Start => "start",
        Action::Pause => "pause",
        Action::Resume => "resume",
        Action::Restart => "restart",
        Action::Lap => "lap",
        Action::Reset => "reset",
        Action::Next => "next",
    }
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    let _ = instance;
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, "TIMER", kit::header_right(ui, kit::status_tag(ui)));
    draw_tabs(band, ui);
    draw_readout(band, ui);
    match ui.timer.mode {
        Mode::Stopwatch => draw_laps(band, ui),
        Mode::Countdown => draw_countdown_detail(band, ui),
        Mode::Pomodoro => draw_pomodoro_detail(band, ui),
    }
    draw_buttons(band, ui);
    let (hints, len) = hints(ui);
    kit::hint_bar(band, &hints[..len]);
}

// ---- signature --------------------------------------------------------------------------------

fn hash_detail(ui: &UiState, h: &mut Hasher) {
    let view = &ui.timer;
    h.u8(view.mode as u8);
    match view.mode {
        Mode::Stopwatch => {
            h.u16(view.lap_count);
            for lap in view.laps {
                h.u32(lap);
            }
        }
        Mode::Countdown => {
            let (percent, color) = bar(ui);
            h.u8(percent).u32(color).bool(view.countdown_idle()).bool(view.done).bool(view.countdown.running());
            h.u32(view.length_ms).opt_u32(view.pressed.map(u32::from));
        }
        Mode::Pomodoro => {
            let (percent, color) = pomodoro_bar(ui);
            let pomodoro = &view.pomodoro;
            h.u8(percent).u32(color).u8(pomodoro.phase as u8).u8(pomodoro.completed);
            h.bool(pomodoro.done).bool(pomodoro.run.running()).bool(pomodoro.idle());
        }
    }
}

/// Header, tabs, readout, the two detail lanes, the buttons and the hint bar.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    let _ = instance;
    let view = &ui.timer;
    sig.region_with(layout::HEADER, |h| crate::ui::overlay::notify::header_inputs(ui, h));
    sig.region_with(TABS, |h| {
        h.u8(view.mode as u8).opt_u32(view.pressed.map(u32::from));
        for mode in Mode::ALL {
            h.opt_u32(tab_dot(ui, mode));
        }
    });
    sig.region_with(layout::TIMER_READOUT, |h| {
        let readout = current(ui);
        h.u32(readout_color(ui)).u8(readout.len as u8);
        for glyph in readout.glyphs() {
            match *glyph {
                Glyph::Digit(value) => h.u8(value),
                Glyph::Small(value) => h.u8(16 + value),
                Glyph::Colon => h.u8(32),
                Glyph::Point => h.u8(33),
            };
        }
    });
    sig.region_with(DETAIL_TOP, |h| hash_detail(ui, h));
    sig.region_with(DETAIL_REST, |h| hash_detail(ui, h));
    sig.region_with(layout::TIMER_SECONDARY, |h| {
        h.str(view.secondary_label()).bool(view.secondary().is_some()).bool(view.pressed == Some(SECONDARY_ID as u8));
    });
    sig.region_with(layout::TIMER_PRIMARY, |h| {
        h.u8(view.primary() as u8).bool(view.pressed == Some(PRIMARY_ID as u8));
    });
    sig.region_with(layout::HINT_BAR, |h| {
        let (hints, len) = hints(ui);
        for hint in &hints[..len] {
            h.str(hint.key).str(hint.label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyphs(readout: &Readout) -> std::string::String {
        readout
            .glyphs()
            .iter()
            .map(|glyph| match *glyph {
                Glyph::Digit(d) | Glyph::Small(d) => (b'0' + d) as char,
                Glyph::Colon => ':',
                Glyph::Point => '.',
            })
            .collect()
    }

    #[test]
    fn readouts_pad_and_grow_hours() {
        assert_eq!(glyphs(&readout(0, true, Some(0))), "00:00.0");
        assert_eq!(glyphs(&readout(65, true, Some(653))), "01:05.3");
        assert_eq!(glyphs(&readout(300, false, None)), "5:00");
        assert_eq!(glyphs(&readout(659, false, None)), "10:59");
        assert_eq!(glyphs(&readout(3723, false, None)), "1:02:03");
        assert_eq!(glyphs(&readout(3723, true, Some(4))), "1:02:03.4");
        assert_eq!(glyphs(&readout(36_000, true, None)), "10:00:00");
    }

    #[test]
    fn every_readout_fits_the_lane() {
        for readout in [readout(5 * 3600, false, None), readout(9 * 3600 + 3599, true, Some(9)), readout(36_000 + 3599, true, None)] {
            let h = fitted_height(readout.glyphs());
            assert!(total_width(readout.glyphs(), h) <= layout::TIMER_READOUT.width() - 12, "{}", glyphs(&readout));
            assert!(h >= 60);
        }
        assert_eq!(fitted_height(readout(59 * 60, true, Some(0)).glyphs()), layout::TIMER_DIGIT_H);
    }

    #[test]
    fn clock_text_matches_the_readout() {
        let text = |ms: u32, tenths: bool| {
            let mut out = Text::<16>::new();
            clock_text(ms, tenths, &mut out);
            std::string::String::from(out.as_str())
        };
        assert_eq!(text(12_400, true), "0:12.4");
        assert_eq!(text(300_000, false), "5:00");
        assert_eq!(text(299_001, false), "5:00");
        assert_eq!(text(3_723_400, true), "1:02:03.4");
        let mut out = Text::<16>::new();
        preset_label(25 * 60, &mut out);
        assert_eq!(out.as_str(), "25 min");
    }
}
