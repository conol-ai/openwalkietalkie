//! Shared UI root: state machines, faces, damage and the splash, without any
//! hardware I/O. Every consumer (the Embassy firmware, the native core, the
//! host tests and the preview) mounts this one file as `ui`, next to `battery`,
//! `control`, `display`, `orientation` and `deck` at its crate root. Nothing in
//! this tree may name any other crate path.
//!
//! Rendering is banded (see `display::Band`): a face is drawn against a full
//! 480x480 canvas, but only `BAND_H` rows exist in memory at a time, so it is
//! drawn once per band and each band shipped as it is finished. Ten passes
//! cost more CPU than one, and buy back 400KB of RAM we do not have.
//!
//! `state` decides, `scene` draws (`faces`, `overlay`, through `kit` and `font`
//! in `theme` colours at `layout` rectangles), `signature` + `damage` find what
//! to redraw.

pub mod bluetooth;
pub mod damage;
pub mod faces;
pub mod font;
pub mod font_cjk;
pub mod rich;
pub mod font_data;
pub mod icons;
pub mod kit;
pub mod layout;
pub mod meter;
pub mod overlay;
pub mod scene;
pub mod session;
pub mod image;
pub mod signature;
pub mod splash;
pub mod state;
pub mod theme;

// Not every consumer names every re-export.
#[allow(unused_imports)]
pub use layout::Rect;
#[allow(unused_imports)]
pub use signature::{Hasher, SigBuilder, Signature};
#[allow(unused_imports)]
pub use state::{UiState, Update};

/// Compile-time check of the consumer mount table: shared code may rely on
/// exactly these crate paths, so they must resolve in every consumer.
#[allow(unused_imports)]
mod mounts {
    use crate::battery::{BatteryState, ChargeState, Estimator};
    use crate::control::{Button, Control, Edge, Touch, TouchPhase, Wheel};
    use crate::display::{Band, BANDS, BAND_BYTES, BAND_H, HEIGHT, WIDTH};
    use crate::orientation::{point, point_with, ROTATE_180};
    use crate::{deck, ui};
}
