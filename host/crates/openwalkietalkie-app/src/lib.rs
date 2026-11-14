//! `OpenWalkieTalkie.app` — the deck's companion (design §8).
//!
//! A pure client. It opens no HID link, no audio device and no hotkey, and it
//! writes no third-party config file: `deckd` owns all of that and this app
//! talks to it over one UDS session. What is here is the window.
//!
//! The split (§8.1, contract §4): everything testable is in
//! [`deck_gui_core`] — the event bus, the tokio bridge, the `ApiClient`, the
//! `HostState` mirror with its reducer, the reply parsers, the attention list
//! and the status item's model — and this crate is the `gpui` layer on top:
//! [`present`] turns those into strings and tones, [`style`], [`ui`] and
//! [`art`] are the look, the parts and the deck's own pictures every page is
//! built from, [`shell`] is the window that owns the one mirror and pumps the
//! bus, [`frame`] and [`menu`] are the sidebar, the strips and the status
//! item, [`deck`], [`faces`], [`integrations`], [`voice`], [`service`] and
//! [`firstrun`] are the pages, [`login_item`] is `SMAppService`, [`prefs`] is
//! what this Mac remembers for the window, and [`app`] boots it.
//!
//! **Every page keeps one contract with the shell**: a pane the shell owns,
//! and `refresh_`, `render_`, `_dialog`, `_dialog_closed`, `localize_`,
//! `on_…_event`, `tick_` and `stage_` functions on [`shell::Shell`], which
//! is the only way the shell reaches it. Three pages have modules of their
//! own beside them: [`keys`] is the Macropad's keystrokes (Faces & Keys),
//! [`voice_actions`] the engines, keys and permissions behind the Voice
//! page, and [`updates`], [`footprint`] and [`local_api`] the firmware flow
//! and the two sub-pages of Service.
//!
//! The one threading rule, which the shape keeps rather than the discipline:
//! **nothing calls `block_on` on the gpui thread.** `deck-gui-core` hands out
//! a spawn and never a runtime `Handle`, so I/O goes to
//! [`deck_gui_core::TokioBridge::spawn`] and anything awaiting a reply goes
//! to `cx.spawn`, which runs a plain future on gpui's own foreground
//! executor.
pub mod app;
pub mod art;
pub mod assets;
pub mod i18n;
pub mod kitboard;
pub mod login_item;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod prefs;
pub mod present;
#[cfg(windows)]
pub(crate) mod run_key;
#[cfg(windows)]
pub mod service_host;
pub mod shell;
pub mod shots;
#[cfg(target_os = "macos")]
pub mod snapshot;
pub mod style;
pub mod ui;
pub mod updater;

// ---- the frame, and the pages with the modules that are theirs alone ------

pub mod frame;
pub mod menu;

pub mod deck;

pub mod faces;
pub mod keys;

pub mod integrations;

pub mod voice;
pub mod voice_actions;

pub mod footprint;
pub mod local_api;
pub mod service;
pub mod updates;

pub mod firstrun;

pub use app::run;
