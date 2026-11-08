//! The frame: one or two faces, overlays, then Bluetooth status. `draw` and
//! `signature` walk the same structure, so what is described is what is drawn.
//!
//! `draw` runs once per band and once per damage rectangle; the band's clip
//! says which. Time comes from `ui.now_ms` only.
use super::faces;
use super::layout::Rect;
use super::overlay;
use super::signature::{SigBuilder, Signature};
use super::state::UiState;
use super::theme;
use crate::deck::msg::{FaceEntry, Template};
use crate::display::Band;
use embedded_graphics::{prelude::*, primitives::Rectangle};

pub fn draw(band: &mut Band, ui: &UiState) {
    // An opaque overlay fills the band itself: skip the faces it hides.
    let faces = if overlay::opaque(ui) { [None; 2] } else { ui.visible_faces() };
    for visible in faces.into_iter().flatten() {
        // The face sees its own 480x480 and nothing of its neighbour.
        let window = band.window();
        band.set_origin_x(window_origin(band, visible.origin_x));
        band.clip_to(Rectangle::new(Point::zero(), Size::new(480, 480)));
        match visible.face {
            Some(face) => draw_face(band, ui, face),
            None => band.fill(super::kit::rgb(theme::CANVAS)),
        }
        band.restore(window);
    }
    overlay::draw(band, ui);
    if !super::state::arcade::fullscreen(ui) {
        super::bluetooth::draw(band, ui.bluetooth);
    }
}

// A caller may already have moved the origin (tests sweep it); slide inside.
fn window_origin(band: &Band, origin_x: i32) -> i32 {
    band.origin_x() + origin_x
}

pub fn draw_face(band: &mut Band, ui: &UiState, face: FaceEntry) {
    match face.template {
        Template::Agents => faces::agents::draw(band, ui, face.id),
        Template::Omarchy => faces::omarchy::draw(band, ui, face.id),
        Template::Macropad => faces::macropad::draw(band, ui, face.id),
        Template::Arcade => faces::arcade::draw(band, ui, face.id),
        Template::System => faces::system::draw(band, ui, face.id),
        Template::Timer => faces::timer::draw(band, ui, face.id),
        Template::Usage => faces::usage::draw(band, ui, face.id),
        Template::Settings => faces::settings::draw(band, ui, face.id),
    }
}

pub fn face_signature(ui: &UiState, face: FaceEntry, sig: &mut SigBuilder) {
    match face.template {
        Template::Agents => faces::agents::signature(ui, face.id, sig),
        Template::Omarchy => faces::omarchy::signature(ui, face.id, sig),
        Template::Macropad => faces::macropad::signature(ui, face.id, sig),
        Template::Arcade => faces::arcade::signature(ui, face.id, sig),
        Template::System => faces::system::signature(ui, face.id, sig),
        Template::Timer => faces::timer::signature(ui, face.id, sig),
        Template::Usage => faces::usage::signature(ui, face.id, sig),
        Template::Settings => faces::settings::signature(ui, face.id, sig),
    }
}

/// Fill `sig` for the frame `draw` would produce from `ui`.
pub fn signature(ui: &UiState, sig: &mut Signature) {
    let mut builder = SigBuilder::new(sig);
    // Language changes affect every built-in label, including opaque overlays.
    builder.mode(ui.language as u32);
    // Whole pixels: a sub-pixel step of the slide changes nothing on screen.
    builder.mode((ui.global.strip_x >> 8) as u32);
    // Under an opaque overlay the faces keep their mode (what is beneath
    // decides the frame once the overlay closes) but register no regions:
    // `draw` skips them, so nothing they hash reaches a pixel.
    let hidden = overlay::opaque(ui);
    for visible in ui.visible_faces().into_iter().flatten() {
        builder.set_origin_x(visible.origin_x);
        match visible.face {
            Some(face) => {
                builder.mode(face.id as u32 | (face.template as u32) << 8);
                if !hidden {
                    face_signature(ui, face, &mut builder);
                }
            }
            None if hidden => {}
            None => builder.region(Rect::SCREEN, 0),
        }
    }
    builder.set_origin_x(0);
    overlay::signature(ui, &mut builder);
    let fullscreen = super::state::arcade::fullscreen(ui);
    builder.mode(fullscreen as u32);
    if !fullscreen {
        super::bluetooth::signature(ui.bluetooth, &mut builder);
    }
}
