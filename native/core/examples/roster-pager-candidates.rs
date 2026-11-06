//! Layout study: how the roster tells that more projects lie LEFT or RIGHT.
//! The real roster face is drawn by the firmware (`scene::draw`); each
//! candidate is painted over it with the firmware's fonts and colours.
//! Nothing here is the firmware's roster; the chosen one is ported to
//! `src/ui/faces/agents.rs` afterwards.
//!
//!     cargo run --example roster-pager-candidates -- <out dir>
//!
//! Writes `<candidate>-<page>.ppm` for pages `first` (a project to the right
//! only) and `middle` (one on each side) of a three-project roster, and
//! `conol-marks` (the first page with Conol sessions, to check their marks).
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

use control::{Button, Control, Edge};
use display::Band;
use std::io::Write;
use ui::font::{self, Align, Font};
use ui::layout::Rect;
use ui::{kit, scene, theme, UiState};

const CANDIDATES: [&str; 6] = ["current", "chevrons", "edges", "dots", "names", "both"];

/// The roster's pages: every project label in page order, and the shown one.
fn pages(ui: &UiState) -> (Vec<String>, usize) {
    let mut heads = [0u8; deck::model::MAX_AGENTS];
    let len = ui.deck.project_heads(&mut heads);
    let labels: Vec<String> = heads[..len].iter().map(|&h| ui.deck.project_of(h).to_string()).collect();
    let at = labels.iter().position(|l| l == ui.agents.project.as_str()).unwrap_or(0);
    (labels, at)
}

/// An anti-aliased chevron pointing `dir` (-1 left, 1 right), centred on
/// (cx, cy), `h` tall, `stroke` px wide.
fn chevron(band: &mut Band, cx: f32, cy: f32, h: f32, stroke: f32, dir: f32, color: u32) {
    let w = h / 2.0;
    let tip = (cx + dir * w / 2.0, cy);
    let ends = [(cx - dir * w / 2.0, cy - h / 2.0), (cx - dir * w / 2.0, cy + h / 2.0)];
    let (x0, x1) = ((cx - w - stroke) as i32, (cx + w + stroke) as i32 + 1);
    let (y0, y1) = ((cy - h / 2.0 - stroke) as i32, (cy + h / 2.0 + stroke) as i32 + 1);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ends.iter().map(|&e| segment_distance(p, e, tip)).fold(f32::MAX, f32::min);
            let c = (stroke / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if c > 0.0 {
                band.blend_px(x, y, color, (c * 255.0) as u8);
            }
        }
    }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (qx, qy) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    (qx * qx + qy * qy).sqrt()
}

fn overlay(band: &mut Band, ui: &UiState, candidate: &str) {
    let (labels, at) = pages(ui);
    let left = at > 0;
    let right = at + 1 < labels.len();
    let card = ui::layout::ROSTER_CARD;
    let cy = (card.top() + card.bottom()) as f32 / 2.0;
    match candidate {
        // 1. A chevron in each side gutter, toward a neighbouring project.
        "chevrons" => {
            if left {
                chevron(band, 16.0, cy, 22.0, 3.0, -1.0, theme::TEXT_LABEL);
            }
            if right {
                chevron(band, 464.0, cy, 22.0, 3.0, 1.0, theme::TEXT_LABEL);
            }
        }
        // 2. The neighbouring page's card peeks in from the screen's edge,
        // the sideways twin of the peeks above and below.
        "edges" => {
            let (top, h) = (card.top() + 22, card.height() - 44);
            let sliver = 14;
            if left {
                let r = Rect::new(sliver - 404, top, 404, h);
                kit::round(band, r, theme::CARD_R, theme::CARD_FILL);
                kit::outline(band, r, theme::CARD_R, theme::OUTLINE_THUMB, 2);
            }
            if right {
                let r = Rect::new(theme::SCREEN - sliver, top, 404, h);
                kit::round(band, r, theme::CARD_R, theme::CARD_FILL);
                kit::outline(band, r, theme::CARD_R, theme::OUTLINE_THUMB, 2);
            }
        }
        // 3. One dot per project under the header; the shown one a pill.
        "dots" => {
            let (dot, pill, gap) = (6, 18, 8);
            let total = labels.len() as i32 * (dot + gap) - gap + (pill - dot);
            let mut x = theme::SCREEN / 2 - total / 2;
            for i in 0..labels.len() {
                let w = if i == at { pill } else { dot };
                let color = if i == at { theme::TEXT_BRIGHT } else { theme::GRAY };
                kit::round(band, Rect::new(x, 63, w, dot), 3, color);
                x += w + gap;
            }
        }
        // 4. The neighbours by name, in the header's capitals, on the line
        // between the carousel and the hint bar.
        "names" => {
            let baseline = 420;
            let name = |label: &str, dir: f32, band: &mut Band| {
                let text = if label.is_empty() { "OTHER".to_string() } else { label.to_uppercase() };
                let w = font::measure(Font::Label, &text);
                if dir < 0.0 {
                    chevron(band, 42.0, baseline as f32 - 4.5, 10.0, 2.0, -1.0, theme::TEXT_FAINT);
                    font::draw(band, Font::Label, 52, baseline, &text, theme::TEXT_FAINT, None, 180, Align::Left);
                } else {
                    let x = theme::SCREEN - 52 - w.min(180);
                    font::draw(band, Font::Label, x, baseline, &text, theme::TEXT_FAINT, None, 180, Align::Left);
                    chevron(band, 438.0, baseline as f32 - 4.5, 10.0, 2.0, 1.0, theme::TEXT_FAINT);
                }
            };
            if left {
                name(&labels[at - 1], -1.0, band);
            }
            if right {
                name(&labels[at + 1], 1.0, band);
            }
        }
        // 5. Chevrons and dots together.
        "both" => {
            overlay(band, ui, "chevrons");
            overlay(band, ui, "dots");
        }
        _ => {}
    }
}

fn press(ui: &mut UiState, button: Button, at: u32) {
    let mut out = ui::Update::new();
    ui.apply(Control::Button(button, Edge::Press), at, &mut out);
    ui.apply(Control::Button(button, Edge::Release), at + 10, &mut out);
}

fn main() {
    let out_dir = std::path::PathBuf::from(std::env::args().nth(1).expect("out dir"));
    std::fs::create_dir_all(&out_dir).unwrap();
    for page in ["first", "middle", "conol-marks"] {
        for candidate in CANDIDATES {
            let mut ui = Box::new(UiState::new());
            ui.reset();
            ui.load_demo();
            // Three projects: cohdl (agents 1-2), openmicro (3-4), battlefield (5).
            ui.deck.agents[2].project.set("openmicro");
            ui.deck.agents[4].project.set("battlefield");
            if page == "conol-marks" {
                for i in 0..2 {
                    ui.deck.agents[i].kind = deck::msg::AgentKind::Conol;
                }
            }
            if page == "middle" {
                press(&mut ui, Button::Right, 100);
            }
            ui.invalidate();
            let path = out_dir.join(format!("{candidate}-{page}.ppm"));
            let mut output = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
            writeln!(output, "P6\n480 480\n255").unwrap();
            let mut band = Band::new();
            for index in 0..display::BANDS {
                band.set_row(index);
                scene::draw(&mut band, &ui);
                overlay(&mut band, &ui, candidate);
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
        }
    }
    eprintln!("wrote {}", out_dir.display());
}
