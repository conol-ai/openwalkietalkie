//! Layout study: candidate roster designs with larger type, drawn with the
//! firmware's own fonts, marks and kit primitives at the panel's 480×480.
//! Nothing here is the firmware's roster; the chosen design is ported to
//! `src/ui/faces/agents.rs` afterwards.
//!
//!     cargo run --example roster-candidates -- <marks dir> <out dir>
//!
//! `<marks dir>` holds `tools/generate-agent-marks.py --size N` output in
//! `<dir>/<N>/<kind>.alpha`.
#![allow(dead_code)]
#[path = "../../../src/battery/state.rs"]
mod battery;
#[path = "../../../src/control/types.rs"]
mod control;
#[path = "../../../src/deck/mod.rs"]
mod deck;
#[path = "../../../src/display/framebuffer.rs"]
mod display;
#[path = "../../../src/display/orientation.rs"]
mod orientation;
#[path = "../../../src/ui/mod.rs"]
mod ui;

use deck::msg::AgentKind;
use display::Band;
use std::io::Write;
use ui::font::{self, Align, Font};
use ui::layout::Rect;
use ui::{kit, theme, UiState};

#[derive(Clone, Copy, PartialEq)]
enum Dot {
    Running,
    Waiting,
    Failed,
    Stopped,
}

struct Session {
    kind: AgentKind,
    task: &'static str,
    dot: Dot,
    subs: u8,
    age: &'static str,
}

const SESSIONS: [Session; 5] = [
    Session { kind: AgentKind::Codex, task: "Migrate the billing schema", dot: Dot::Waiting, subs: 0, age: "2m" },
    Session { kind: AgentKind::Claude, task: "Refactor the site header", dot: Dot::Stopped, subs: 0, age: "" },
    Session { kind: AgentKind::Codex, task: "Build both board images", dot: Dot::Running, subs: 3, age: "6m" },
    Session { kind: AgentKind::Claude, task: "Write the 0.12 release notes and blog post", dot: Dot::Running, subs: 1, age: "1m" },
    Session { kind: AgentKind::Conol, task: "Plan the Kyoto trip", dot: Dot::Stopped, subs: 0, age: "" },
];

struct Marks {
    dir: std::path::PathBuf,
}

impl Marks {
    fn draw(&self, band: &mut Band, kind: AgentKind, size: i32, x: i32, y: i32) {
        let name = match kind {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Conol => "conol",
            AgentKind::Other => "other",
        };
        let mask = std::fs::read(self.dir.join(size.to_string()).join(format!("{name}.alpha"))).expect("mark");
        let tint = ui::marks::tint(kind);
        for row in 0..size {
            for col in 0..size {
                let a = mask[(row * size + col) as usize];
                if a != 0 {
                    band.blend_px(x + col, y + row, tint, a);
                }
            }
        }
    }
}

fn dot(band: &mut Band, cx: i32, cy: i32, r: i32, state: Dot, bg: u32) {
    match state {
        Dot::Running => kit::dot(band, cx, cy, r, theme::GREEN),
        Dot::Waiting => kit::dot(band, cx, cy, r, theme::AMBER),
        Dot::Failed => kit::dot(band, cx, cy, r, theme::RED),
        Dot::Stopped => {
            kit::dot(band, cx, cy, r, theme::TEXT_FAINT);
            kit::dot(band, cx, cy, r - 2, bg);
        }
    }
}

/// The agent mark with its status as a presence dot on the lower right,
/// ringed in the background so it reads against the mark.
fn mark_dot(band: &mut Band, marks: &Marks, kind: AgentKind, size: i32, x: i32, y: i32, state: Dot, bg: u32) {
    marks.draw(band, kind, size, x, y);
    let r = (size / 6).max(5);
    let (cx, cy) = (x + size - r + 2, y + size - r + 2);
    kit::dot(band, cx, cy, r + 3, bg);
    dot(band, cx, cy, r, state, bg);
}

/// The count pill, right-aligned at `right`, centred on `cy`; returns its left edge.
fn badge(band: &mut Band, subs: u8, right: i32, cy: i32, h: i32, font: Font, fill: u32) -> i32 {
    if subs == 0 {
        return right;
    }
    let label = subs.to_string();
    let w = (font::measure(font, &label) + h * 2 / 3).max(h);
    let pill = Rect::new(right - w, cy - h / 2, w, h);
    kit::round(band, pill, (h / 2) as u32, fill);
    let baseline = cy + font::ascent(font) * 9 / 20;
    font::draw(band, font, pill.center().0, baseline, &label, theme::TEXT_BRIGHT, Some(fill), w, Align::Center);
    pill.left()
}

fn status(s: &Session) -> String {
    let word = match s.dot {
        Dot::Running => "Working",
        Dot::Waiting => "Waiting for you",
        Dot::Failed => "Failed",
        Dot::Stopped => "Ready",
    };
    let mut out = String::from(word);
    if !s.age.is_empty() {
        out.push_str(" \u{b7} ");
        out.push_str(s.age);
    }
    if s.subs > 0 {
        out.push_str(&format!(" \u{b7} {} running", s.subs));
    }
    out
}

/// Wrapped lines of `text` in `font` at `width`, at most `max`.
fn lines(font: Font, text: &str, width: i32, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    font::wrap(font, text, width, max, &mut |_, line, _| out.push(line.to_string()));
    out
}

fn chrome(band: &mut Band, ui: &UiState, label: &str) {
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, label, kit::header_right(ui, kit::status_tag(ui)));
    kit::hint_bar(band, &ui::faces::agents::ROSTER_HINTS);
}

// ---- A: large single-line rows, 4 a page ----------------------------------------

fn a_large_rows(band: &mut Band, ui: &UiState, marks: &Marks) {
    chrome(band, ui, "COHDL \u{b7} 1/2");
    for (i, s) in SESSIONS.iter().take(4).enumerate() {
        let rect = Rect::new(32, 70 + i as i32 * 88, 416, 82);
        let selected = i == 0;
        if selected {
            kit::card(band, rect, true, false);
        }
        let bg = if selected { theme::CARD_FILL } else { theme::CANVAS };
        let cy = rect.top() + rect.height() / 2;
        mark_dot(band, marks, s.kind, 40, rect.left() + 16, cy - 20, s.dot, bg);
        let right = badge(band, s.subs, rect.right() - 14, cy, 32, Font::Label, if selected { theme::OUTLINE } else { theme::CHIP_FILL });
        let x = rect.left() + 72;
        let w = right - 12 - x;
        font::draw(band, Font::TextStrong, x, cy + font::ascent(Font::TextStrong) * 9 / 20, s.task, theme::TEXT_PRIMARY, Some(bg), w, Align::Left);
    }
}

// ---- B: two-line rows, 3 a page ---------------------------------------------------

fn b_two_lines(band: &mut Band, ui: &UiState, marks: &Marks) {
    chrome(band, ui, "COHDL \u{b7} 1/2");
    let order = [3usize, 0, 2];
    for (i, &k) in order.iter().enumerate() {
        let s = &SESSIONS[k];
        let rect = Rect::new(32, 70 + i as i32 * 118, 416, 112);
        let selected = i == 0;
        if selected {
            kit::card(band, rect, true, false);
        }
        let bg = if selected { theme::CARD_FILL } else { theme::CANVAS };
        let lh = font::line_height(Font::Title) - 4;
        let block = 2 * lh;
        let first = rect.top() + (rect.height() - block) / 2 + font::ascent(Font::Title) - 2;
        let mid = first - font::ascent(Font::Title) * 9 / 20;
        mark_dot(band, marks, s.kind, 40, rect.left() + 16, mid - 20, s.dot, bg);
        let right = badge(band, s.subs, rect.right() - 14, mid, 34, Font::Label, if selected { theme::OUTLINE } else { theme::CHIP_FILL });
        let x = rect.left() + 72;
        let width = right - 10 - x;
        let shown = lines(Font::Title, s.task, width, 2);
        for (n, line) in shown.iter().enumerate() {
            let last = n + 1 == shown.len();
            let text = if last && n == 1 && font::measure(Font::Title, s.task) > font::measure(Font::Title, &shown.concat()) + 20 {
                format!("{line}")
            } else {
                line.clone()
            };
            font::draw(band, Font::Title, x, first + n as i32 * lh, &text, theme::TEXT_PRIMARY, Some(bg), right - 10 - x, Align::Left);
        }
        if shown.len() == 1 {
            font::draw(band, Font::Body, x, first + lh, &status(s), theme::TEXT_SECONDARY, Some(bg), rect.right() - 14 - x, Align::Left);
        }
    }
}

// ---- C: the highlighted session expanded, the rest compact -------------------------

fn c_focus(band: &mut Band, ui: &UiState, marks: &Marks) {
    chrome(band, ui, "COHDL \u{b7} 1/2");
    let s = &SESSIONS[3];
    let card = Rect::new(32, 70, 416, 160);
    kit::card(band, card, true, false);
    let bg = theme::CARD_FILL;
    mark_dot(band, marks, s.kind, 40, card.left() + 18, card.top() + 18, s.dot, bg);
    let status_line = status(s);
    font::draw(band, Font::Body, card.left() + 72, card.top() + 38 + font::ascent(Font::Body) * 9 / 20, &status_line, theme::TEXT_SECONDARY, Some(bg), card.width() - 86, Align::Left);
    let x = card.left() + 18;
    let first = card.top() + 74 + font::ascent(Font::Title) - 6;
    let lh = font::line_height(Font::Title) - 4;
    for (n, line) in lines(Font::Title, s.task, card.width() - 36, 2).iter().enumerate() {
        font::draw(band, Font::Title, x, first + n as i32 * lh, line, theme::TEXT_PRIMARY, Some(bg), card.width() - 36, Align::Left);
    }
    for (i, &k) in [0usize, 2, 1].iter().enumerate() {
        let s = &SESSIONS[k];
        let rect = Rect::new(32, 240 + i as i32 * 62, 416, 58);
        let cy = rect.top() + rect.height() / 2;
        mark_dot(band, marks, s.kind, 32, rect.left() + 20, cy - 16, s.dot, theme::CANVAS);
        let right = badge(band, s.subs, rect.right() - 14, cy, 30, Font::Label, theme::CHIP_FILL);
        font::draw(band, Font::TextStrong, rect.left() + 72, cy + font::ascent(Font::TextStrong) * 9 / 20, s.task, theme::TEXT_PRIMARY, Some(theme::CANVAS), right - 10 - rect.left() - 72, Align::Left);
    }
}

// ---- D: carousel — one session large, its neighbours peeking ------------------------

fn d_carousel(band: &mut Band, ui: &UiState, marks: &Marks) {
    chrome(band, ui, "COHDL \u{b7} 2 OF 5");
    let peek = |band: &mut Band, s: &Session, top: i32| {
        let rect = Rect::new(48, top, 384, 42);
        let cy = rect.top() + rect.height() / 2;
        mark_dot(band, marks, s.kind, 24, rect.left(), cy - 12, s.dot, theme::CANVAS);
        font::draw(band, Font::Row, rect.left() + 38, cy + font::ascent(Font::Row) * 9 / 20, s.task, theme::TEXT_SECONDARY, Some(theme::CANVAS), rect.width() - 38, Align::Left);
    };
    peek(band, &SESSIONS[0], 64);
    peek(band, &SESSIONS[2], 382);
    let s = &SESSIONS[3];
    let card = Rect::new(32, 110, 416, 256);
    kit::card(band, card, true, false);
    let bg = theme::CARD_FILL;
    mark_dot(band, marks, s.kind, 40, card.left() + 22, card.top() + 20, s.dot, bg);
    let right = badge(band, s.subs, card.right() - 22, card.top() + 40, 34, Font::Label, theme::OUTLINE);
    font::draw(band, Font::Body, card.left() + 76, card.top() + 40 + font::ascent(Font::Body) * 9 / 20, &status(s), theme::TEXT_SECONDARY, Some(bg), right - 12 - card.left() - 76, Align::Left);
    let x = card.left() + 22;
    let lh = font::line_height(Font::Display) - 10;
    let first = card.top() + 74 + font::ascent(Font::Display) - 6;
    for (n, line) in lines(Font::Display, s.task, card.width() - 44, 3).iter().enumerate() {
        font::draw(band, Font::Display, x, first + n as i32 * lh, line, theme::TEXT_PRIMARY, Some(bg), card.width() - 44, Align::Left);
    }
}

// ---- E: 2×2 tiles ---------------------------------------------------------------------

fn e_tiles(band: &mut Band, ui: &UiState, marks: &Marks) {
    chrome(band, ui, "COHDL \u{b7} 1/2");
    for (i, s) in SESSIONS.iter().take(4).enumerate() {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        let rect = Rect::new(32 + col * 212, 70 + row * 178, 204, 170);
        let selected = i == 0;
        kit::card(band, rect, selected, false);
        let bg = if selected { theme::CARD_FILL } else { theme::CANVAS };
        mark_dot(band, marks, s.kind, 40, rect.left() + 16, rect.top() + 16, s.dot, bg);
        badge(band, s.subs, rect.right() - 16, rect.top() + 36, 30, Font::Label, if selected { theme::OUTLINE } else { theme::CHIP_FILL });
        let first = rect.top() + 72 + font::ascent(Font::TextStrong) - 4;
        let lh = font::line_height(Font::TextStrong) - 4;
        for (n, line) in lines(Font::TextStrong, s.task, rect.width() - 32, 3).iter().enumerate() {
            font::draw(band, Font::TextStrong, rect.left() + 16, first + n as i32 * lh, line, theme::TEXT_PRIMARY, Some(bg), rect.width() - 32, Align::Left);
        }
    }
}


// ---- D2: the chosen carousel, with the latest message ---------------------------------

/// One carousel scene: the project's sessions in order, the highlighted one,
/// and for it the line under its task (a question, the current activity or
/// the last reply) and its age.
struct Shot {
    project: &'static str,
    sessions: &'static [Card],
    at: usize,
}

#[derive(Clone, Copy)]
enum Latest {
    Question,
    Activity,
    Reply,
    Error,
}

struct Card {
    kind: AgentKind,
    task: &'static str,
    dot: Dot,
    subs: u8,
    age: &'static str,
    latest: Latest,
    message: &'static str,
}

const PAGE: [Card; 5] = [
    Card { kind: AgentKind::Codex, task: "Migrate the billing schema", dot: Dot::Waiting, subs: 0, age: "2m",
        latest: Latest::Question, message: "Tests fail on main. Run the 3 pending migrations on staging first?" },
    Card { kind: AgentKind::Claude, task: "Write the 0.12 release notes and blog post", dot: Dot::Running, subs: 1, age: "1m",
        latest: Latest::Activity, message: "Running a command \u{b7} cargo test -p deckd" },
    Card { kind: AgentKind::Codex, task: "Build both board images", dot: Dot::Running, subs: 3, age: "6m",
        latest: Latest::Activity, message: "Linking the Rev B image" },
    Card { kind: AgentKind::Claude, task: "Refactor the site header", dot: Dot::Stopped, subs: 0, age: "",
        latest: Latest::Reply, message: "Header refactor done, 34 files changed. Want me to run the test suite?" },
    Card { kind: AgentKind::Claude, task: "Deploy the staging stack", dot: Dot::Failed, subs: 0, age: "4m",
        latest: Latest::Error, message: "terraform apply exited with status 1: the state lock is held." },
];
const CONOL: [Card; 3] = [
    Card { kind: AgentKind::Conol, task: "+ New chat", dot: Dot::Stopped, subs: 0, age: "",
        latest: Latest::Reply, message: "Hold PTT and say your first message." },
    Card { kind: AgentKind::Conol, task: "Plan the Kyoto trip", dot: Dot::Running, subs: 2, age: "30s",
        latest: Latest::Activity, message: "Searching flights \u{b7} Tokyo to Osaka, 12 Dec" },
    Card { kind: AgentKind::Conol, task: "Weekly metrics review", dot: Dot::Stopped, subs: 0, age: "",
        latest: Latest::Reply, message: "Signups are up 14% week over week; churn is flat." },
];

fn card_status(c: &Card) -> String {
    let word = match c.dot {
        Dot::Running => "Working",
        Dot::Waiting => "Waiting for you",
        Dot::Failed => "Failed",
        Dot::Stopped => "Ready",
    };
    if c.age.is_empty() { word.into() } else { format!("{word} \u{b7} {}", c.age) }
}

/// `text` wrapped to `max` lines, the last one cut with an ellipsis when
/// there is more.
fn clipped(font: Font, text: &str, width: i32, max: usize) -> Vec<String> {
    let mut all = lines(font, text, width, max + 1);
    if all.len() > max {
        all.truncate(max);
        let last = all.last_mut().unwrap();
        while !last.is_empty() && font::measure(font, &format!("{last}\u{2026}")) > width {
            last.pop();
        }
        *last = format!("{}\u{2026}", last.trim_end());
    }
    all
}

fn carousel(band: &mut Band, ui: &UiState, marks: &Marks, shot: &Shot) {
    let count = shot.sessions.len();
    chrome(band, ui, &format!("{} \u{b7} {} OF {}", shot.project, shot.at + 1, count));
    let peek = |band: &mut Band, c: &Card, top: i32| {
        let rect = Rect::new(48, top, 384, 40);
        let cy = rect.top() + rect.height() / 2;
        mark_dot(band, marks, c.kind, 24, rect.left(), cy - 12, c.dot, theme::CANVAS);
        let right = badge(band, c.subs, rect.right(), cy, 24, Font::Small, theme::CHIP_FILL);
        font::draw(band, Font::Row, rect.left() + 38, cy + font::ascent(Font::Row) * 9 / 20, c.task,
            theme::TEXT_SECONDARY, Some(theme::CANVAS), right - 10 - rect.left() - 38, Align::Left);
    };
    if shot.at > 0 {
        peek(band, &shot.sessions[shot.at - 1], 64);
    }
    if shot.at + 1 < count {
        peek(band, &shot.sessions[shot.at + 1], 386);
    }
    let c = &shot.sessions[shot.at];
    let card = Rect::new(32, 108, 416, 272);
    kit::card(band, card, true, false);
    let bg = theme::CARD_FILL;
    let inner = card.width() - 44;
    let x = card.left() + 22;
    // Status: mark and dot, state and age, running count.
    let row_cy = card.top() + 38;
    mark_dot(band, marks, c.kind, 40, x, row_cy - 20, c.dot, bg);
    let right = badge(band, c.subs, card.right() - 22, row_cy, 34, Font::Label, theme::OUTLINE);
    let status_color = match c.dot {
        Dot::Waiting => theme::AMBER,
        Dot::Failed => theme::RED,
        _ => theme::TEXT_SECONDARY,
    };
    font::draw(band, Font::Body, x + 54, row_cy + font::ascent(Font::Body) * 9 / 20, &card_status(c),
        status_color, Some(bg), right - 12 - x - 54, Align::Left);
    // The task, large, at most two lines.
    let lh = font::line_height(Font::Display) - 10;
    let first = card.top() + 74 + font::ascent(Font::Display) - 8;
    let task = clipped(Font::Display, c.task, inner, 2);
    for (n, line) in task.iter().enumerate() {
        font::draw(band, Font::Display, x, first + n as i32 * lh, line, theme::TEXT_PRIMARY, Some(bg), inner, Align::Left);
    }
    // The latest message, under a hairline: the question in full white, the
    // activity and the reply in grey, an error in red.
    let rule_y = first + (task.len() as i32 - 1) * lh + 22;
    kit::fill(band, Rect::new(x, rule_y, inner, 1), theme::OUTLINE);
    let (font, color) = match c.latest {
        Latest::Question => (Font::Row, theme::TEXT_PRIMARY),
        Latest::Error => (Font::Row, theme::RED),
        Latest::Activity | Latest::Reply => (Font::Row, theme::TEXT_SECONDARY),
    };
    let mlh = font::line_height(font) - 2;
    let room = ((card.bottom() - 16 - rule_y - 8) / mlh).clamp(1, 3) as usize;
    let base = rule_y + 10 + font::ascent(font);
    for (n, line) in clipped(font, c.message, inner, room).iter().enumerate() {
        font::draw(band, font, x, base + n as i32 * mlh, line, color, Some(bg), inner, Align::Left);
    }
}

const SHOTS: [(&str, Shot); 5] = [
    ("carousel-working", Shot { project: "COHDL", sessions: &PAGE, at: 1 }),
    ("carousel-question", Shot { project: "COHDL", sessions: &PAGE, at: 0 }),
    ("carousel-ready", Shot { project: "COHDL", sessions: &PAGE, at: 3 }),
    ("carousel-failed", Shot { project: "COHDL", sessions: &PAGE, at: 4 }),
    ("carousel-conol", Shot { project: "CONOL", sessions: &CONOL, at: 1 }),
];

type Draw = fn(&mut Band, &UiState, &Marks);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let marks = Marks { dir: args.get(1).expect("marks dir").into() };
    let out = std::path::PathBuf::from(args.get(2).expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let mut ui = Box::new(UiState::new());
    ui.reset();
    ui.load_demo();
    let candidates: [(&str, Draw); 5] = [
        ("a-large-rows", a_large_rows),
        ("b-two-lines", b_two_lines),
        ("c-focus", c_focus),
        ("d-carousel", d_carousel),
        ("e-tiles", e_tiles),
    ];
    for (name, draw) in candidates {
        let path = out.join(format!("{name}.ppm"));
        let mut output = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(output, "P6\n480 480\n255").unwrap();
        let mut band = Band::new();
        for index in 0..display::BANDS {
            band.set_row(index);
            draw(&mut band, &ui, &marks);
            for pixel in band.data().chunks_exact(2) {
                let raw = u16::from_le_bytes([pixel[0], pixel[1]]);
                output
                    .write_all(&[
                        ((raw >> 11) * 255 / 31) as u8,
                        (((raw >> 5) & 63) * 255 / 63) as u8,
                        ((raw & 31) * 255 / 31) as u8,
                    ])
                    .unwrap();
            }
        }
        output.flush().unwrap();
        eprintln!("{}", path.display());
    }
    for (name, shot) in &SHOTS {
        let path = out.join(format!("{name}.ppm"));
        let mut output = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(output, "P6\n480 480\n255").unwrap();
        let mut band = Band::new();
        for index in 0..display::BANDS {
            band.set_row(index);
            carousel(&mut band, &ui, &marks, shot);
            for pixel in band.data().chunks_exact(2) {
                let raw = u16::from_le_bytes([pixel[0], pixel[1]]);
                output
                    .write_all(&[
                        ((raw >> 11) * 255 / 31) as u8,
                        (((raw >> 5) & 63) * 255 / 63) as u8,
                        ((raw & 31) * 255 / 31) as u8,
                    ])
                    .unwrap();
            }
        }
        output.flush().unwrap();
        eprintln!("{}", path.display());
    }
}
