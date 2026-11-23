//! Exercise device protocols and UI interaction without MCU peripherals.
//! This file only mounts modules; every test lives in the shared sources or in
//! the sibling files declared at the bottom, one owner per file.
#![allow(dead_code)]

// Shared roots, mounted under the same names by every consumer.
#[path = "../../src/battery/state.rs"]
mod battery;
#[path = "../../src/control/types.rs"]
mod control;
#[path = "../../src/deck/mod.rs"]
mod deck;
#[path = "../../src/display/framebuffer.rs"]
mod display;
#[path = "../../src/display/orientation.rs"]
mod orientation;
#[path = "../../src/ui/mod.rs"]
mod ui;

// Runtime glue and drivers with host-runnable tests.
mod audio;
#[path = "../../src/link/bond_record.rs"]
mod bond_record;
#[path = "../../src/link/deck_gatt.rs"]
mod deck_gatt;
#[path = "../../src/battery/charger.rs"]
mod charger;
#[path = "../../src/link/wire.rs"]
mod link_wire;
#[path = "../../src/link/nor_flash.rs"]
mod nor_flash;
#[path = "../../src/input/cst9220.rs"]
mod cst9220;
#[path = "../../src/input/direct.rs"]
mod direct;
#[path = "../../src/input/keymap.rs"]
mod keymap;
#[path = "../../src/power.rs"]
mod power;
#[path = "../../src/input/touch_state.rs"]
mod touch_state;

#[cfg(test)]
mod deck_gatt_tests;
#[cfg(test)]
mod deck_protocol;
#[cfg(test)]
mod demo;
#[cfg(test)]
mod grammar;
#[cfg(test)]
mod grammar_faces;
#[cfg(test)]
mod touch_integration;
