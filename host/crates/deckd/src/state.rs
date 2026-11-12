//! `state.json` (design §9): daemon-written, 0600, atomic rename. Corrupt →
//! renamed `state.json.corrupt` and started empty.
use deck_model::{FaceEntry, LastLinked, MacroLayout, Settings, SlotMemory, Usage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The last adopted strip.
    pub faces: Vec<FaceEntry>,
    /// All eight `set` keys, restored on `boot:1`.
    pub settings: Settings,
    /// key → slot memory.
    pub slots: BTreeMap<String, SlotMemory>,
    /// slot → last card id.
    pub card_ids: BTreeMap<u8, u16>,
    /// face id → layout name.
    pub macropad_bindings: BTreeMap<u8, String>,
    /// face id → the keys as last set (API or the deck's editor).
    pub macros: BTreeMap<u8, MacroLayout>,
    /// agent key → voice id.
    pub voices: BTreeMap<String, String>,
    pub terminal_map: BTreeMap<String, serde_json::Value>,
    pub usage: Option<Usage>,
    /// Recent daemon session ids, newest last.
    pub sids: Vec<u32>,
    /// The deck as it was last linked (`device.last`), so a restart still
    /// knows when it was last seen and what it ran.
    pub last_linked: Option<LastLinked>,
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
        }
    }

    /// Fold a reducer `Persist` effect in; returns whether anything changed.
    pub fn apply(&mut self, patch: &deck_model::StatePatch) -> bool {
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
