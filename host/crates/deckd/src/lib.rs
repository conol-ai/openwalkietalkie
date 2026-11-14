//! `deckd`: the OpenWalkieTalkie host service and its CLI (design §2, §3).
//! The library holds every module so the binary is one line and the
//! modules are unit-tested in place.
pub mod adapters;
pub mod api;
pub mod apidoc;
pub mod audio;
pub mod ble;
pub mod bus;
pub mod care;
mod chat;
mod chat_images;
mod claude_workflows;
pub mod claude_channel;
pub mod cli;
pub mod codexd;
pub mod config;
pub mod core;
pub mod deliveries;
pub mod demo;
pub mod desktop;
pub mod device;
pub mod diagnostics;
pub mod dormancy;
pub mod download;
pub mod hid;
pub mod hook;
pub mod inbox;
pub mod integrations;
pub mod kokoro;
pub mod launchd;
pub mod liveness;
pub mod metrics;
pub mod reader;
pub mod secrets;
pub mod service;
pub mod state;
pub mod stt;
pub mod tokens;
pub mod procs;
pub mod trust;
#[cfg(windows)]
pub mod winproc;
pub mod uninstall;
pub mod usage;
pub mod system_status;
pub mod verbs;

pub mod firmware;
