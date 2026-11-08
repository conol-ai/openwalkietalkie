//! A test-only stand-in for the firmware's `crate::ui`, which `proto.rs`
//! names for its event encoder (`ui.global.strip`, `ui.deck.held_sid()`,
//! `ui.still_default()`) and its inline tests (`reset`, `host`, `Update`).
//! The deck model is the firmware's real one, so `held_sid` and `boot`
//! behave as on the device; the UI layers around it play no part in encoding
//! a line.
use super::model::{Deck, Effects, Settings};
use super::msg::{FaceEntry, HostMsg, Template};

pub const MAX_FACES: usize = 8;

/// The power-on strip (`src/ui/state/mod.rs`, `DEFAULT_STRIP`).
pub const DEFAULT_STRIP: [Template; 7] = [
    Template::Agents,
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::System,
    Template::Arcade,
    Template::Settings,
];

pub struct Global {
    pub strip: [FaceEntry; MAX_FACES],
    pub strip_len: u8,
}

pub struct UiState {
    pub language: u8,
    pub language_revision: u32,
    pub global: Global,
    pub deck: Deck,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiState {
    pub fn new() -> Self {
        Self {
            language: 0,
            language_revision: 0,
            global: Global {
                strip: [FaceEntry::EMPTY; MAX_FACES],
                strip_len: 0,
            },
            deck: Deck::new(),
        }
    }

    /// Power-on state: the default strip and a reset deck.
    pub fn reset(&mut self) {
        self.deck.reset();
        for (index, template) in DEFAULT_STRIP.into_iter().enumerate() {
            let entry = &mut self.global.strip[index];
            entry.id = template.default_id();
            entry.template = template;
            entry.name.set(template.label());
        }
        self.global.strip_len = DEFAULT_STRIP.len() as u8;
    }

    pub fn host(&mut self, msg: &HostMsg, at_ms: u32, _out: &mut Update) {
        let mut fx = Effects::new();
        self.deck.apply(msg, at_ms, &mut fx);
    }

    /// Strip and settings are still the power-on defaults (`hello.boot`).
    pub fn still_default(&self) -> bool {
        self.deck.settings == Settings::new()
            && usize::from(self.global.strip_len) == DEFAULT_STRIP.len()
            && DEFAULT_STRIP.iter().enumerate().all(|(index, template)| {
                let entry = &self.global.strip[index];
                entry.id == template.default_id()
                    && entry.template == *template
                    && entry.name == template.label()
            })
    }
}

#[derive(Default)]
pub struct Update;

impl Update {
    pub fn new() -> Self {
        Self
    }
}
