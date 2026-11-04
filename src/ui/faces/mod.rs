//! One module per face template. A face draws into a `display::Band` in logical
//! 480x480 coordinates and never touches hardware:
//!
//!   pub fn draw(band: &mut Band, ui: &UiState, instance: u8)
//!   pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder)
//!
//! `instance` is the strip entry's stable id. `scene.rs` sets the band origin
//! and clip per face, so a face neither knows nor cares where on the strip it
//! is.

pub mod agents;
pub mod arcade;
pub mod macropad;
pub mod omarchy;
pub mod settings;
pub mod timer;
pub mod usage;
pub mod system;
