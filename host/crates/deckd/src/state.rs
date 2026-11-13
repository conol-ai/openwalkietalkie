//! `state.json` (design §9): daemon-written, 0600, atomic rename. Corrupt →
//! renamed `state.json.corrupt` and started empty.
//!
//! Each deck keeps its own strip, settings and Macropad keys (`decks`, by
//! the deck's id, `device::identity`). The top level is the current deck's
//! — the one linked, else the last one identified — so everything that
//! reads it reads that deck, and a daemon that predates `decks` keeps
//! working on it. The language, the sessions, the voices and the usage are
//! the Mac's, whichever deck is in the hand.
use crate::device::identity;
use deck_model::{DeckProfile, FaceEntry, LastLinked, MacroLayout, Settings, SlotMemory, Usage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The current deck's strip, as last adopted.
    pub faces: Vec<FaceEntry>,
    /// The current deck's `set` keys, restored on `boot:1`, and the Mac's
    /// language.
    pub settings: Settings,
    /// key → slot memory.
    pub slots: BTreeMap<String, SlotMemory>,
    /// slot → last card id.
    pub card_ids: BTreeMap<u8, u16>,
    /// face id → layout name, for the current deck.
    pub macropad_bindings: BTreeMap<u8, String>,
    /// face id → the current deck's keys as last set (API or the deck's
    /// editor).
    pub macros: BTreeMap<u8, MacroLayout>,
    /// agent key → voice id.
    pub voices: BTreeMap<String, String>,
    // `terminal_map` (the Ghostty tab pins) went with the tab path: a file
    // that still has it loads, since unknown keys are ignored, and the key
    // is gone from the next save.
    pub usage: Option<Usage>,
    /// Recent daemon session ids, newest last.
    pub sids: Vec<u32>,
    /// The deck as it was last linked (`device.last`), so a restart still
    /// knows when it was last seen and what it ran.
    pub last_linked: Option<LastLinked>,
    /// What became of every card and how the deck's reader was used
    /// (`crate::deliveries`): ids, times, counts and closed words, never
    /// content. One this build cannot read starts again empty; the rest of
    /// the file still loads.
    #[serde(deserialize_with = "crate::deliveries::lenient")]
    pub deliveries: crate::deliveries::Deliveries,
    /// Every deck identified here, by its id (the serial number its cable
    /// reports): its own strip, settings and keys, and its last link.
    pub decks: BTreeMap<String, DeckState>,
    /// The id of the deck the top level is. `None` until a deck is first
    /// identified; until then the top level is the one setup the Mac had.
    pub deck: Option<String>,
}

/// What `state.json` keeps for one deck.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckState {
    pub faces: Vec<FaceEntry>,
    /// Its `set` keys. The language in it is not read back: the Mac's is.
    pub settings: Settings,
    pub macros: BTreeMap<u8, MacroLayout>,
    pub macropad_bindings: BTreeMap<u8, String>,
    /// This deck as it was last linked.
    pub last_linked: Option<LastLinked>,
}

/// What [`State::select`] made of the deck that linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selected {
    /// It named no deck: the current deck stays the current deck.
    Unidentified,
    /// It is the current deck.
    Same,
    /// The first deck identified here: the setup the Mac had is its own.
    Claimed,
    /// A deck seen here before: its own setup is the current one again.
    Known,
    /// A deck never seen here: it starts from the firmware's defaults.
    New,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// What `load` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Read,
    Absent,
    /// The file was unparsable and moved aside.
    Corrupt,
}

impl State {
    pub fn load(path: &Path) -> (State, Loaded) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return (State::default(), Loaded::Absent),
        };
        match serde_json::from_slice::<State>(&bytes) {
            Ok(mut state) => {
                state.settings.normalize_language();
                (state, Loaded::Read)
            }
            Err(_) => {
                let corrupt = path.with_extension("json.corrupt");
                // Best effort; a failed rename only costs the next start the same detour.
                let _ = std::fs::rename(path, &corrupt);
                (State::default(), Loaded::Corrupt)
            }
        }
    }

    /// Write to `<path>.tmp` (0600) and rename over `path`.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let tmp = path.with_extension("json.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The model's view of the persisted state.
    pub fn persisted(&self) -> deck_model::Persisted {
        deck_model::Persisted {
            faces: self.faces.clone(),
            settings: self.settings,
            slots: self.slots.clone(),
            card_ids: self.card_ids.clone(),
            voices: self.voices.clone(),
            usage: self.usage.clone(),
            macros: self.macros.clone(),
            last_linked: self.last_linked.clone(),
            deck: self.deck_label(),
        }
    }

    /// The deck that linked reported the serial number `serial`: it becomes
    /// the current deck, and the top level its strip, settings and keys.
    ///
    /// The deck is looked up by every id the serial can name, the cable's
    /// spelling first; a record a Bluetooth spelling made under the other
    /// byte order is moved to the cable's id once the cable names it. The
    /// deck that was current keeps what the top level held. The first deck
    /// identified here takes the setup the whole Mac had; any other deck
    /// never seen here starts from the firmware's defaults, nothing copied
    /// from another deck. The language stays the Mac's throughout. A serial
    /// that names no deck changes nothing: the last deck identified stays.
    pub fn select(&mut self, serial: Option<&str>) -> Selected {
        let candidates = serial.map(identity::candidates).unwrap_or_default();
        let Some(cable) = candidates.first().cloned() else {
            return Selected::Unidentified;
        };
        let found = candidates
            .iter()
            .find(|id| self.decks.contains_key(*id))
            .cloned();
        let id = match found {
            Some(other) if other != cable && serial.is_some_and(identity::from_cable) => {
                if let Some(record) = self.decks.remove(&other) {
                    self.decks.insert(cable.clone(), record);
                }
                if self.deck.as_deref() == Some(other.as_str()) {
                    self.deck = Some(cable.clone());
                }
                cable
            }
            Some(id) => id,
            None => cable,
        };
        if self.deck.as_deref() == Some(id.as_str()) {
            return Selected::Same;
        }
        if self.deck.is_none() && self.decks.is_empty() {
            self.keep_current(id.clone());
            self.deck = Some(id);
            return Selected::Claimed;
        }
        if let Some(old) = self.deck.take() {
            self.keep_current(old);
        }
        let selected = if self.decks.contains_key(&id) {
            Selected::Known
        } else {
            Selected::New
        };
        let record = self.decks.get(&id).cloned().unwrap_or_default();
        let (language, language_revision) =
            (self.settings.language, self.settings.language_revision);
        self.faces = record.faces;
        self.settings = Settings {
            language,
            language_revision,
            ..record.settings
        };
        self.macros = record.macros;
        self.macropad_bindings = record.macropad_bindings;
        self.keep_current(id.clone());
        self.deck = Some(id);
        selected
    }

    /// The top level into `decks[id]`. The deck's last link stays its own.
    fn keep_current(&mut self, id: String) {
        let record = self.decks.entry(id).or_default();
        record.faces.clone_from(&self.faces);
        record.settings = self.settings;
        record.macros.clone_from(&self.macros);
        record.macropad_bindings.clone_from(&self.macropad_bindings);
    }

    /// The current deck's strip, settings and keys, as the store takes them.
    pub fn profile(&self) -> DeckProfile {
        DeckProfile {
            faces: self.faces.clone(),
            settings: self.settings,
            macros: self.macros.clone(),
        }
    }

    /// The current deck's label (`device.deck`), `None` until a deck is
    /// identified.
    pub fn deck_label(&self) -> Option<String> {
        self.deck.as_deref().map(identity::label)
    }

    /// Fold a reducer `Persist` effect in; returns whether anything changed.
    /// What is the current deck's lands in its own record too.
    pub fn apply(&mut self, patch: &deck_model::StatePatch) -> bool {
        let top = self.apply_top(patch);
        let own = self.apply_own(patch);
        top || own
    }

    /// The current deck's part of `patch`, into its record.
    fn apply_own(&mut self, patch: &deck_model::StatePatch) -> bool {
        use deck_model::StatePatch;
        let Some(record) = self.deck.as_ref().and_then(|id| self.decks.get_mut(id)) else {
            return false;
        };
        match patch {
            StatePatch::Faces(list) => {
                let changed = record.faces != *list;
                record.faces.clone_from(list);
                changed
            }
            StatePatch::Setting { key, value } => record.settings.set(*key, *value),
            StatePatch::MacropadBinding { f, layout } => {
                record.macropad_bindings.insert(*f, layout.clone()).as_deref()
                    != Some(layout.as_str())
            }
            StatePatch::Macro { f, src, keys } => {
                let layout = MacroLayout {
                    src: src.clone(),
                    keys: keys.clone(),
                };
                record.macros.insert(*f, layout.clone()).as_ref() != Some(&layout)
            }
            StatePatch::LastLinked(last) => {
                let changed = record.last_linked.as_ref() != Some(last);
                record.last_linked = Some(last.clone());
                changed
            }
            // The Mac's, whichever deck is linked: one language for every
            // deck, and the sessions, voices and usage are no deck's.
            StatePatch::Language { .. }
            | StatePatch::CardId { .. }
            | StatePatch::Slot { .. }
            | StatePatch::Voice { .. }
            | StatePatch::Usage(_) => false,
        }
    }

    fn apply_top(&mut self, patch: &deck_model::StatePatch) -> bool {
        use deck_model::StatePatch;
        match patch {
            StatePatch::Language { value, revision } => {
                let changed = self.settings.language != *value || self.settings.language_revision != *revision;
                self.settings.language = *value;
                self.settings.language_revision = *revision;
                changed
            }
            StatePatch::Faces(list) => {
                let changed = self.faces != *list;
                self.faces = list.clone();
                changed
            }
            StatePatch::Setting { key, value } => self.settings.set(*key, *value),
            StatePatch::CardId { slot, last } => {
                self.card_ids.insert(slot.0, last.0) != Some(last.0)
            }
            StatePatch::Slot {
                key,
                slot,
                kind,
                cwd,
                last_seen,
            } => {
                let entry = SlotMemory {
                    slot: *slot,
                    last_seen: *last_seen,
                    kind: *kind,
                    cwd: cwd.clone(),
                };
                self.slots.insert(key.clone(), entry.clone()) != Some(entry)
            }
            StatePatch::MacropadBinding { f, layout } => {
                self.macropad_bindings.insert(*f, layout.clone()).as_deref()
                    != Some(layout.as_str())
            }
            StatePatch::Macro { f, src, keys } => {
                let layout = MacroLayout {
                    src: src.clone(),
                    keys: keys.clone(),
                };
                self.macros.insert(*f, layout.clone()).as_ref() != Some(&layout)
            }
            StatePatch::Voice { key, voice } => {
                self.voices.insert(key.clone(), voice.clone()).as_deref() != Some(voice.as_str())
            }
            StatePatch::Usage(usage) => {
                let changed = self.usage.as_ref() != Some(usage);
                self.usage = Some(usage.clone());
                changed
            }
            StatePatch::LastLinked(last) => {
                let changed = self.last_linked.as_ref() != Some(last);
                self.last_linked = Some(last.clone());
                changed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{Binding, CardId, MacroKey, SetKey, Slot, StatePatch, Template};

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("deckd-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trips_atomically_with_0600() {
        let d = dir("rt");
        let path = d.join("state.json");
        let (state, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Absent);
        assert_eq!(state, State::default());
        let mut state = State::default();
        state.faces.push(FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        });
        state.card_ids.insert(3, 17);
        state.sids.push(314_159);
        assert!(state.apply(&StatePatch::Macro {
            f: 3,
            src: None,
            keys: vec![MacroKey { label: "build".into(), binding: Some(Binding::Left), keys: None, }],
        }));
        assert_eq!(state.persisted().macros[&3].keys[0].label, "build");
        state.save(&path).unwrap();
        assert!(!d.join("state.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back, state);
        // A partial file (unknown keys, missing keys) still loads.
        std::fs::write(&path, r#"{"card_ids":{"1":5},"future":true}"#).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back.card_ids[&1], 5);
        assert_eq!(back.settings, Settings::default());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A file a daemon with the Ghostty tab path wrote still loads, and the
    /// pins it kept are gone from the next save.
    #[test]
    fn an_old_terminal_map_loads_and_is_dropped_on_save() {
        let d = dir("terminal-map");
        let path = d.join("state.json");
        std::fs::write(
            &path,
            r#"{"card_ids":{"2":9},"terminal_map":{"UUID-A":{"key":"claude:s","id4":"cd34"}}}"#,
        )
        .unwrap();
        let (state, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(state.card_ids[&2], 9);
        state.save(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.get("terminal_map").is_none(), "{saved}");
        assert_eq!(saved["card_ids"]["2"], 9);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_moved_aside() {
        let d = dir("corrupt");
        let path = d.join("state.json");
        std::fs::write(&path, "{not json").unwrap();
        let (state, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Corrupt);
        assert_eq!(state, State::default());
        assert!(!path.exists());
        assert!(d.join("state.json.corrupt").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn language_and_revision_are_persisted_together_and_old_state_migrates() {
        let d = dir("language");
        let path = d.join("state.json");
        let mut state = State::default();
        assert!(state.apply(&StatePatch::Language { value: 3, revision: 12 }));
        state.save(&path).unwrap();
        let (loaded, status) = State::load(&path);
        assert_eq!(status, Loaded::Read);
        assert_eq!((loaded.settings.language, loaded.settings.language_revision), (3, 12));
        std::fs::write(&path, r#"{"settings":{"language":2}}"#).unwrap();
        let (loaded, _) = State::load(&path);
        assert_eq!((loaded.settings.language, loaded.settings.language_revision), (2, 1));
        std::fs::write(&path, r#"{"settings":{"language":99}}"#).unwrap();
        let (loaded, _) = State::load(&path);
        assert_eq!((loaded.settings.language, loaded.settings.language_revision), (0, 0));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn patches_fold_in_and_report_changes() {
        let mut s = State::default();
        assert!(s.apply(&StatePatch::CardId {
            slot: Slot(2),
            last: CardId(4)
        }));
        assert!(!s.apply(&StatePatch::CardId {
            slot: Slot(2),
            last: CardId(4)
        }));
        assert!(s.apply(&StatePatch::Setting {
            key: SetKey::Mic,
            value: 2
        }));
        assert!(!s.apply(&StatePatch::Setting {
            key: SetKey::Mic,
            value: 2
        }));
        assert!(!s.apply(&StatePatch::Setting {
            key: SetKey::Mic,
            value: 200
        }));
        assert!(s.apply(&StatePatch::MacropadBinding {
            f: 3,
            layout: "Build".into()
        }));
        assert!(!s.apply(&StatePatch::MacropadBinding {
            f: 3,
            layout: "Build".into()
        }));
        assert!(s.apply(&StatePatch::Voice {
            key: "k".into(),
            voice: "v".into()
        }));
        assert!(s.apply(&StatePatch::Usage(Usage {
            period: "p".into(),
            items: vec![],
            at: None
        })));
        assert!(!s.apply(&StatePatch::Usage(Usage {
            period: "p".into(),
            items: vec![],
            at: None
        })));
        assert!(s.apply(&StatePatch::Slot {
            key: "claude:1".into(),
            slot: Slot(1),
            kind: deck_model::Kind::Claude,
            cwd: None,
            last_seen: deck_model::Ts(0),
        }));
        let p = s.persisted();
        assert_eq!(p.card_ids[&2], 4);
        assert_eq!(p.settings.mic, 2);
        assert_eq!(p.slots.len(), 1);
    }

    fn last_linked(at: u64) -> LastLinked {
        LastLinked {
            at: deck_model::Ts(at),
            fw: Some("0.13.0-openwalkietalkie".into()),
            board: Some("Rev B".into()),
            transport: Some(deck_model::Transport::Bluetooth),
        }
    }

    #[test]
    fn the_last_linked_deck_folds_in_once_and_a_file_without_it_reads_as_never_linked() {
        let d = dir("last-linked");
        let path = d.join("state.json");
        let mut state = State::default();
        assert!(state.apply(&StatePatch::LastLinked(last_linked(1_789_760_523_000))));
        assert!(!state.apply(&StatePatch::LastLinked(last_linked(1_789_760_523_000))));
        assert!(state.apply(&StatePatch::LastLinked(last_linked(1_789_760_583_000))));
        assert_eq!(state.persisted().last_linked, Some(last_linked(1_789_760_583_000)));
        state.save(&path).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back.last_linked, Some(last_linked(1_789_760_583_000)));
        // A state.json written before the field: never linked, nothing lost.
        std::fs::write(&path, r#"{"card_ids":{"1":5},"sids":[7]}"#).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!((back.last_linked, back.card_ids[&1]), (None, 5));
        std::fs::remove_dir_all(d).unwrap();
    }

    const A: &str = "A1B2C3D4E5F6";
    const B: &str = "C0FFEE15600D";

    fn agents_and(f: u8, t: Template) -> Vec<FaceEntry> {
        vec![
            FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "Agents".into(),
            },
            FaceEntry {
                f,
                t,
                n: format!("{t:?}"),
            },
        ]
    }

    fn keys(label: &str) -> MacroLayout {
        MacroLayout {
            src: None,
            keys: vec![MacroKey {
                label: label.into(),
                binding: Some(Binding::Left),
                keys: Some("cmd+b".into()),
            }],
        }
    }

    /// Deck A as the first deck identified here: a Macropad at 3 with
    /// keys, the speaker at 3, the Mac in Japanese.
    fn deck_a() -> State {
        let mut state = State {
            faces: agents_and(3, Template::Macropad),
            ..State::default()
        };
        state.settings.speaker = 3;
        assert!(state.apply(&StatePatch::Language {
            value: 3,
            revision: 4
        }));
        state.macros.insert(3, keys("build"));
        state.macropad_bindings.insert(3, "Build".into());
        assert_eq!(state.select(Some(A)), Selected::Claimed);
        state
    }

    #[test]
    fn a_file_from_before_decks_is_claimed_by_the_first_deck_identified() {
        let d = dir("claimed");
        let path = d.join("state.json");
        std::fs::write(
            &path,
            r#"{"faces":[{"f":1,"t":"agents","n":"Agents"},{"f":3,"t":"macropad","n":"Build"}],
                "settings":{"speaker":3,"language":2},"macros":{"3":{"src":null,"keys":[]}},
                "card_ids":{"1":5}}"#,
        )
        .unwrap();
        let (mut state, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!((state.deck.as_deref(), state.decks.len()), (None, 0));
        assert_eq!(state.persisted().deck, None);
        let before = state.clone();
        assert_eq!(state.select(Some(A)), Selected::Claimed);
        // Nothing moves: the setup the whole Mac had is this deck's now.
        assert_eq!(
            (&state.faces, state.settings, &state.macros),
            (&before.faces, before.settings, &before.macros)
        );
        assert_eq!(state.deck.as_deref(), Some(A));
        let record = &state.decks[A];
        assert_eq!(record.faces, before.faces);
        assert_eq!(record.settings.speaker, 3);
        assert_eq!(record.macros, before.macros);
        assert_eq!(state.persisted().deck, Some(identity::label(A)));
        assert_eq!(state.select(Some(A)), Selected::Same);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_new_deck_starts_from_the_firmware_defaults_in_the_macs_language() {
        let mut state = deck_a();
        assert_eq!(state.select(Some(B)), Selected::New);
        assert!(state.faces.is_empty());
        assert!(state.macros.is_empty() && state.macropad_bindings.is_empty());
        assert_eq!(
            state.settings,
            Settings {
                language: 3,
                language_revision: 4,
                ..Settings::default()
            }
        );
        assert_eq!(state.deck.as_deref(), Some(B));
        assert_eq!(
            state.profile().settings.speaker,
            Settings::default().speaker
        );
        assert_eq!(
            state.settings.demo, 0,
            "the firmware powers on with demo data off"
        );
        // A keeps what it had; nothing of it was copied to B.
        let a = &state.decks[A];
        assert_eq!(a.faces, agents_and(3, Template::Macropad));
        assert_eq!((a.settings.speaker, a.macros[&3].keys[0].label.as_str()), (3, "build"));
        assert_eq!(a.macropad_bindings[&3], "Build");
        assert!(state.decks[B].faces.is_empty());
    }

    #[test]
    fn switching_decks_swaps_strip_settings_keys_and_bindings_and_keeps_the_language() {
        let mut state = deck_a();
        assert_eq!(state.select(Some(B)), Selected::New);
        // B gets a strip, a speaker and keys of its own, and while it is in
        // the hand the Mac's language changes.
        for patch in [
            StatePatch::Faces(agents_and(4, Template::Usage)),
            StatePatch::Setting {
                key: SetKey::Speaker,
                value: 8,
            },
            StatePatch::Macro {
                f: 6,
                src: None,
                keys: keys("deploy").keys,
            },
            StatePatch::MacropadBinding {
                f: 6,
                layout: "Deploy".into(),
            },
            StatePatch::Language {
                value: 1,
                revision: 5,
            },
        ] {
            assert!(state.apply(&patch), "{patch:?}");
        }
        assert_eq!(state.select(Some(A)), Selected::Known);
        assert_eq!(state.faces, agents_and(3, Template::Macropad));
        assert_eq!(state.settings.speaker, 3);
        assert_eq!(state.macros.keys().copied().collect::<Vec<_>>(), vec![3]);
        assert_eq!(state.macropad_bindings[&3], "Build");
        assert_eq!(
            (state.settings.language, state.settings.language_revision),
            (1, 5),
            "the Mac's language, whichever deck"
        );
        assert_eq!(state.select(Some(B)), Selected::Known);
        assert_eq!(state.faces, agents_and(4, Template::Usage));
        assert_eq!(state.settings.speaker, 8);
        assert_eq!(state.macros[&6].keys[0].label, "deploy");
        assert_eq!(state.macropad_bindings[&6], "Deploy");
        assert_eq!((state.settings.language, state.settings.language_revision), (1, 5));
    }

    #[test]
    fn patches_land_in_the_current_decks_profile_and_the_top_level() {
        let mut state = deck_a();
        assert!(state.apply(&StatePatch::Setting {
            key: SetKey::Mic,
            value: 2
        }));
        assert!(state.apply(&StatePatch::Faces(agents_and(7, Template::System))));
        assert!(state.apply(&StatePatch::Macro {
            f: 3,
            src: Some("app".into()),
            keys: keys("ship").keys,
        }));
        assert!(state.apply(&StatePatch::LastLinked(last_linked(1_789_760_523_000))));
        let a = &state.decks[A];
        assert_eq!(a.settings.mic, 2);
        assert_eq!(a.faces, agents_and(7, Template::System));
        assert_eq!(a.macros[&3].keys[0].label, "ship");
        assert_eq!(a.last_linked, Some(last_linked(1_789_760_523_000)));
        assert_eq!((state.settings.mic, &state.faces), (2, &a.faces));
        // The Mac's, not the deck's: they reach the top level only.
        let record = state.decks[A].clone();
        assert!(state.apply(&StatePatch::Language {
            value: 2,
            revision: 9
        }));
        assert!(state.apply(&StatePatch::CardId {
            slot: Slot(1),
            last: CardId(4)
        }));
        assert!(state.apply(&StatePatch::Usage(Usage {
            period: "p".into(),
            items: vec![],
            at: None
        })));
        assert_eq!(state.decks[A], record);
        // A patch the deck's record already holds changes nothing.
        assert!(!state.apply(&StatePatch::Setting {
            key: SetKey::Mic,
            value: 2
        }));
    }

    #[test]
    fn an_unidentified_link_keeps_the_current_deck() {
        let mut state = deck_a();
        assert_eq!(state.select(Some(B)), Selected::New);
        let before = state.clone();
        for serial in [None, Some(""), Some("000000000000"), Some("OpenWalkieTalkie")] {
            assert_eq!(state.select(serial), Selected::Unidentified, "{serial:?}");
            assert_eq!(state, before, "{serial:?}");
        }
        // Before any deck was identified, too: the Mac's one setup stays.
        let mut fresh = State::default();
        fresh.settings.speaker = 4;
        let before = fresh.clone();
        assert_eq!(fresh.select(None), Selected::Unidentified);
        assert_eq!(fresh, before);
    }

    #[test]
    fn a_profile_made_from_a_reversed_bluetooth_spelling_is_found_and_rekeyed_by_the_cable() {
        let mut state = deck_a();
        // A Bluetooth stack that printed deck B's address least significant
        // byte first, as the cable does: read the usual way round, its
        // record is made under the other byte order.
        let reversed = "C0:FF:EE:15:60:0D";
        assert_eq!(state.select(Some(reversed)), Selected::New);
        assert!(state.apply(&StatePatch::Setting {
            key: SetKey::Brightness,
            value: 2
        }));
        assert!(state.decks.contains_key("0D6015EEFFC0") && !state.decks.contains_key(B));
        assert_eq!(state.select(Some(A)), Selected::Known);
        // The cable names it for good: found under its reversal, moved to
        // its own id, and the same deck from then on over either link.
        assert_eq!(state.select(Some(B)), Selected::Known);
        assert!(state.decks.contains_key(B) && !state.decks.contains_key("0D6015EEFFC0"));
        assert_eq!(state.settings.brightness, 2);
        assert_eq!(state.deck.as_deref(), Some(B));
        assert_eq!(state.select(Some(reversed)), Selected::Same);
        // The current deck re-keyed in place is still the current deck.
        let mut state = deck_a();
        assert_eq!(state.select(Some(reversed)), Selected::New);
        assert_eq!(state.select(Some(B)), Selected::Same);
        assert_eq!(state.deck.as_deref(), Some(B));
        assert_eq!(state.decks.len(), 2);
        // A Bluetooth spelling in the usual order is the cable's id at once.
        assert_eq!(state.select(Some("0d:60:15:ee:ff:c0")), Selected::Same);
    }

    #[test]
    fn decks_survive_save_and_load_and_the_top_level_is_the_current_deck() {
        let d = dir("decks");
        let path = d.join("state.json");
        let mut state = deck_a();
        assert_eq!(state.select(Some(B)), Selected::New);
        assert!(state.apply(&StatePatch::Faces(agents_and(4, Template::Usage))));
        state.save(&path).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back, state);
        assert_eq!(back.deck.as_deref(), Some(B));
        assert_eq!(back.faces, agents_and(4, Template::Usage), "B's, at the top");
        assert_eq!(back.decks[A].faces, agents_and(3, Template::Macropad));
        assert_eq!(back.persisted().deck, Some(identity::label(B)));
        assert_eq!(back.persisted().faces, back.faces);
        // The file names the decks by id and the current one by `deck`.
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["deck"], B);
        assert!(json["decks"].get(A).is_some() && json["decks"].get(B).is_some());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// R7 and R4 in one file: each deck's own setup, which deck is current,
    /// and the delivery record all come back whole from a save and a load.
    /// Another deck in the hand leaves the record as it is: it is the
    /// Mac's, whichever deck is linked.
    #[test]
    fn two_decks_and_the_delivery_record_survive_save_and_load_together() {
        use crate::deliveries::{Deliveries, Record, Totals};
        let d = dir("decks-deliveries");
        let path = d.join("state.json");
        let mut state = deck_a();
        let mut deliveries = Deliveries {
            since: Some(deck_model::Ts(1_789_760_000_000)),
            version: "0.13.0".into(),
            cards: Totals {
                total: 1,
                reached: 1,
                deck_used: 1,
                ..Totals::default()
            },
            ..Deliveries::default()
        };
        deliveries.cards.outcomes.insert("answered".into(), 1);
        deliveries.recent.push_back(Record {
            agent: deck_model::AgentId("ag_1".into()),
            card: 7,
            kind: "q".into(),
            origin: "permission".into(),
            deliver: "hold".into(),
            from: "claude".into(),
            set: deck_model::Ts(1_789_760_523_000),
            ended: deck_model::Ts(1_789_760_583_000),
            reached: true,
            deck_used: true,
            answered: Some("deck".into()),
            outcome: "answered".into(),
            ..Record::default()
        });
        state.deliveries = deliveries.clone();
        assert_eq!(state.select(Some(B)), Selected::New);
        assert!(state.apply(&StatePatch::Faces(agents_and(4, Template::Usage))));
        assert_eq!(state.deliveries, deliveries, "a new deck in the hand");
        state.save(&path).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back, state);
        assert_eq!(back.deck.as_deref(), Some(B));
        assert_eq!(back.decks[A].faces, agents_and(3, Template::Macropad));
        assert_eq!(back.decks[B].faces, agents_and(4, Template::Usage));
        assert_eq!(back.deliveries, deliveries);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for key in ["decks", "deck", "deliveries"] {
            assert!(json.get(key).is_some(), "{key}: {json}");
        }
        assert_eq!(json["deliveries"]["recent"][0]["outcome"], "answered");
        let mut again = back;
        assert_eq!(again.select(Some(A)), Selected::Known);
        assert_eq!(again.deliveries, deliveries, "deck A back in the hand");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The whole path a restart takes: the store's persist effects folded
    /// into `state.json`, the file read back, and a new store seeded from it.
    #[test]
    fn the_deck_a_store_remembers_is_still_remembered_after_a_restart() {
        use deck_model::{
            Command, DeviceLine, DeviceReport, Effect, LinkEvent, Store, StoreConfig, Transport,
        };
        use std::time::{Duration, Instant};
        let d = dir("restart");
        let path = d.join("state.json");
        let t0 = Instant::now();
        let mut store = Store::new(StoreConfig::new(t0, deck_model::Ts(1_789_760_523_000), 7));
        let mut state = State::default();
        let hello = DeviceLine::Hello {
            v: 1,
            fw: "0.13.0-openwalkietalkie".into(),
            sid: 7,
            boot: false,
            rx: deck_model::LINE_MAX_U16,
            agents: None,
            lang: None,
            lang_rev: None,
            faces: vec![],
        };
        let report = Command::DeviceReport;
        let lost = Duration::from_secs(90);
        let steps = [
            (Duration::ZERO, Command::Link(LinkEvent::Linked)),
            (Duration::ZERO, report(DeviceReport::Transport(Some(Transport::Usb)))),
            (Duration::ZERO, Command::Device(hello)),
            (Duration::ZERO, Command::Link(LinkEvent::Live { sid: 7 })),
            (Duration::ZERO, report(DeviceReport::Board(Some("Rev B".into())))),
            (lost, report(DeviceReport::Dropped { why: "removed".into() })),
            (lost, Command::Link(LinkEvent::Down)),
        ];
        for (after, command) in steps {
            let (_, effects) = store.apply(command, t0 + after);
            for effect in effects {
                if let Effect::Persist(patch) = effect {
                    state.apply(&patch);
                }
            }
        }
        let remembered = store.snapshot().device.last.clone().expect("a deck went live");
        assert_eq!(remembered.at, store.ts(t0 + lost), "when it was lost");
        assert_eq!(remembered.board.as_deref(), Some("Rev B"));
        state.save(&path).unwrap();
        let (back, _) = State::load(&path);
        let mut config = StoreConfig::new(Instant::now(), deck_model::Ts(1_789_760_700_000), 8);
        config.persisted = back.persisted();
        let restarted = Store::new(config);
        assert_eq!(restarted.snapshot().device.last, Some(remembered));
        // The drops are this run's alone.
        assert_eq!(store.snapshot().device.drops.len(), 1);
        assert!(restarted.snapshot().device.drops.is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
