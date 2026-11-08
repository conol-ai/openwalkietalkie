//! Render harness: complete redraws, the independent panel-mounting oracle and
//! the partial-refresh replay that every face's tests compare against. Tests
//! live in the child modules, one owner per file.
//!
//! Harness API for every face's tests:
//!   `ui()`                         power-on `UiState` (demo data, Agents face)
//!   `on(template)`                 the same, jump-cut to a face
//!   `full(&UiState)`               complete logical frame, RGB565 bytes
//!   `Screens::new(&UiState)`       retained logical + panel images
//!   `update(&mut screens, &previous, &next) -> pixels transferred`
//!                                  replays the damage between two states and
//!                                  asserts both images equal a full redraw
//!   `damage(previous, next)`       the `Damage` the firmware would compute
//!   `lit_outside_safe_area(&frame)` first pixel brighter than the hint bar
//!                                  that lies outside the 74 px corner mask
use crate::{
    deck::msg::Template,
    display, orientation,
    ui::{damage::Damage, layout, scene, signature::Signature, UiState},
};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*, primitives::Rectangle};

mod agents;
mod arcade;
mod bluetooth;
mod fuzz;
mod kit;
mod i18n;
mod macropad;
mod omarchy;
mod overlay;
mod settings;
mod system;
mod timer;
mod slide;
mod usage;

/// States are several kilobytes: tests keep them boxed and copy with `*`.
fn ui() -> Box<UiState> {
    let mut ui = Box::new(UiState::new());
    ui.reset();
    ui.load_demo();
    ui
}

fn on(template: Template) -> Box<UiState> {
    let mut ui = ui();
    ui.on_face(template);
    ui
}

fn full(state: &UiState) -> Vec<u8> {
    full_draw(|band| scene::draw(band, state))
}

fn signature(state: &UiState) -> Box<Signature> {
    let mut sig = Box::new(Signature::new());
    scene::signature(state, &mut sig);
    sig
}

fn damage(previous: Option<&UiState>, next: &UiState) -> Damage {
    let previous = previous.map(signature);
    Damage::between(previous.as_deref(), &signature(next))
}

/// Objective corner test: nothing brighter than the hint-bar fill may be lit
/// where the panel's rounded corners hide it.
fn lit_outside_safe_area(frame: &[u8]) -> Option<(i32, i32)> {
    frame.chunks_exact(2).enumerate().find_map(|(index, pixel)| {
        let (x, y) = ((index % 480) as i32, (index / 480) as i32);
        let raw = u16::from_le_bytes([pixel[0], pixel[1]]);
        // The hint bar and its hairline (RGB565 3, 5, 3) run under the arcs,
        // as in the mockups: anything that dim counts as dark.
        let dark = (raw >> 11) <= 3 && ((raw >> 5) & 63) <= 6 && (raw & 31) <= 3;
        (!dark && !layout::point_in_safe_area(x, y)).then_some((x, y))
    })
}

fn full_draw(mut draw: impl FnMut(&mut display::Band)) -> Vec<u8> {
    let mut pixels = vec![0; 480 * 480 * 2];
    let mut band = display::Band::new();
    for index in 0..display::BANDS {
        band.set_row(index);
        draw(&mut band);
        let start = index as usize * display::BAND_BYTES;
        pixels[start..start + display::BAND_BYTES].copy_from_slice(band.data());
    }
    pixels
}

// Independent full-image oracle: reverse pixels, never the two bytes in RGB565.
fn panel_image(logical: &[u8]) -> Vec<u8> {
    if orientation::ROTATE_180 {
        logical
            .chunks_exact(2)
            .rev()
            .flat_map(|pixel| pixel.iter().copied())
            .collect()
    } else {
        logical.to_vec()
    }
}

struct Screens {
    logical: Vec<u8>,
    panel: Vec<u8>,
}

impl Screens {
    fn new(state: &UiState) -> Self {
        let logical = full(state);
        let panel = panel_image(&logical);
        Self { logical, panel }
    }
}

fn assert_frame(actual: &[u8], expected: &[u8], message: &str) {
    assert_eq!(actual.len(), expected.len());
    let mismatch = actual.iter().zip(expected).position(|(a, b)| a != b);
    assert_eq!(
        mismatch.map(|byte| (byte / 2 % 480, byte / 2 / 480)),
        None,
        "{message}"
    );
}

fn update(screen: &mut Screens, previous: &UiState, next: &UiState) -> usize {
    let mut dirty = damage(Some(previous), next);
    let mut band = display::Band::new();
    // Retained buffer contents outside the active clip must never reach the panel.
    band.fill(Rgb565::MAGENTA);
    let mut transferred = 0;
    let mut panel_bottom = 0;
    while let Some(rect) = dirty.next() {
        assert_eq!((rect.x0 | rect.y0 | rect.x1 | rect.y1) & 1, 0);
        assert!(rect.x0 < rect.x1 && rect.x1 <= 480);
        assert!(rect.y0 < rect.y1 && rect.y1 <= 480);
        assert_eq!(rect.y0 / display::BAND_H, (rect.y1 - 1) / display::BAND_H);
        band.set_row(rect.y0 / display::BAND_H);
        band.clip_to(Rectangle::new(
            Point::new(rect.x0 as i32, rect.y0 as i32),
            Size::new((rect.x1 - rect.x0) as u32, (rect.y1 - rect.y0) as u32),
        ));
        scene::draw(&mut band, next);
        let width = (rect.x1 - rect.x0) as usize * 2;
        for y in rect.y0..rect.y1 {
            let src = ((y - band.y0()) as usize * 480 + rect.x0 as usize) * 2;
            let dst = (y as usize * 480 + rect.x0 as usize) * 2;
            screen.logical[dst..dst + width].copy_from_slice(&band.data()[src..src + width]);
            transferred += width / 2;
        }
        let panel = rect.panel();
        assert_eq!((panel.x0 | panel.y0 | panel.x1 | panel.y1) & 1, 0);
        assert_eq!(panel.x1 - panel.x0, rect.x1 - rect.x0);
        assert_eq!(panel.y1 - panel.y0, rect.y1 - rect.y0);
        assert!(panel.y0 >= panel_bottom, "DMA must follow panel scan order");
        panel_bottom = panel.y1;
        let clip = band.bounding_box();
        let row = band.y0();
        let address = band.data().as_ptr();
        band.orient_for_panel();
        assert_eq!(band.data().as_ptr(), address);
        assert_eq!(band.y0(), row);
        assert_eq!(band.bounding_box(), clip);
        // The DMA starts at the original logical clip with a 480-pixel stride,
        // while its destination is the independently transformed panel window.
        let source = ((rect.y0 - row) as usize * 480 + rect.x0 as usize) * 2;
        assert_eq!((address as usize + source) & 3, 0);
        for y in 0..panel.y1 - panel.y0 {
            let src = source + y as usize * 480 * 2;
            let dst = ((panel.y0 + y) as usize * 480 + panel.x0 as usize) * 2;
            screen.panel[dst..dst + width].copy_from_slice(&band.data()[src..src + width]);
        }
        // Embassy uses a tightly packed DMA source. Check the actual in-place
        // compaction against the independent strided path for every crop,
        // including narrow windows, nonzero offsets and both mountings.
        let packed = band.pack_for_dma();
        assert_eq!(packed.as_ptr() as usize & 3, 0);
        assert_eq!(packed.len(), width * (panel.y1 - panel.y0) as usize);
        for (row, pixels) in packed.chunks_exact(width).enumerate() {
            let dst = ((panel.y0 as usize + row) * 480 + panel.x0 as usize) * 2;
            assert_eq!(pixels, &screen.panel[dst..dst + width]);
        }
    }
    let expected = full(next);
    assert_frame(
        &screen.logical,
        &expected,
        "logical partial refresh must match a complete redraw, including erased glows",
    );
    assert_frame(
        &screen.panel,
        &panel_image(&expected),
        "physical partial refresh must match an independently rotated complete redraw",
    );
    transferred
}
