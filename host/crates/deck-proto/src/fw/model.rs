//! Stand-in for the firmware's `model.rs`, which cannot be mounted (it loads
//! the demo script, which needs the whole UI). `proto.rs` takes `MAX_AGENTS`
//! from it; the `ui` shim takes the session bookkeeping behind `hello.sid`
//! and `hello.boot`, reduced to what `proto::encode` and its tests read: the
//! held sid is 0 until the host's `synced`, and a host `set` ends `boot`.
use super::msg::{HostMsg, MacroKey};
use super::text::Text;

pub const MAX_AGENTS: usize = 255;

/// The stand-in's session table: only its size is read (`hello`'s `agents`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sessions(pub usize);

impl Sessions {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0
    }

    #[must_use]
    #[allow(dead_code)] // clippy::len_without_is_empty wants it; nothing reads it
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }
}
pub const MAX_MACRO_SETS: usize = 3;

/// One Macropad's keys, as the firmware's `MacroSet`: the encoder reads a
/// `macrokey` event's key from here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacroSet {
    pub face: u8,
    pub source: Text<16>,
    pub keys: [MacroKey; 9],
}

impl MacroSet {
    pub const EMPTY: Self = Self {
        face: 0,
        source: Text::new(),
        keys: [MacroKey::EMPTY; 9],
    };
}

/// Only whether a host changed anything matters to `still_default`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    pub changed: bool,
}

impl Settings {
    pub const fn new() -> Self {
        Self { changed: false }
    }
}

#[derive(Default)]
pub struct Effects;

impl Effects {
    pub const fn new() -> Self {
        Self
    }
}

pub struct Deck {
    sid: u32,
    synced_sid: u32,
    host_present: bool,
    syncing: bool,
    pub settings: Settings,
    pub macros: [MacroSet; MAX_MACRO_SETS],
    /// As many as a Rev A or Rev B deck holds.
    pub agents: Sessions,
}

impl Default for Deck {
    fn default() -> Self {
        Self::new()
    }
}

impl Deck {
    pub const fn new() -> Self {
        Self {
            sid: 0,
            synced_sid: 0,
            host_present: false,
            syncing: false,
            settings: Settings::new(),
            macros: [MacroSet::EMPTY; MAX_MACRO_SETS],
            agents: Sessions(MAX_AGENTS),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn macro_set(&self, face: u8) -> Option<&MacroSet> {
        self.macros.iter().find(|set| set.face == face && face != 0)
    }

    /// As the firmware's: the face's set, or a free slot made its own.
    pub fn macro_set_mut(&mut self, face: u8) -> Option<&mut MacroSet> {
        if face == 0 {
            return None;
        }
        let slot = self
            .macros
            .iter()
            .position(|set| set.face == face)
            .or_else(|| self.macros.iter().position(|set| set.face == 0))?;
        let set = &mut self.macros[slot];
        if set.face != face {
            *set = MacroSet { face, ..MacroSet::EMPTY };
        }
        Some(set)
    }

    /// What `hello` reports as `sid`: the session a complete state is held
    /// for, 0 until the host's `synced`.
    pub fn held_sid(&self) -> u32 {
        if self.host_present && !self.syncing && self.synced_sid == self.sid {
            self.sid
        } else {
            0
        }
    }

    pub fn apply(&mut self, msg: &HostMsg, _now_ms: u32, _fx: &mut Effects) {
        match msg {
            HostMsg::Hello { sid, .. } => {
                if !self.host_present || *sid != self.sid {
                    self.syncing = true;
                }
                self.host_present = true;
                self.sid = *sid;
            }
            HostMsg::Synced => {
                self.syncing = false;
                self.synced_sid = self.sid;
            }
            HostMsg::Set { .. } => self.settings.changed = true,
            _ => {}
        }
    }
}
