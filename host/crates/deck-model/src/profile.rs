//! A deck's own strip, settings and Macropad keys (`Command::DeckProfile`).
//!
//! The deck keeps none of them across a power-off: it says `boot:1` and the
//! host restores them, so the host's copy is the only one there is. The
//! service keeps one copy per deck it has identified (by the serial number
//! its cable reports), and when a deck links the store takes that deck's:
//! the snapshot's strip, settings and keys are replaced, and the hid
//! thread's view follows before the link's first hello goes out, so the
//! sync that hello starts restores this deck and not the one before it.
//!
//! The language is not a deck's. It is the Mac's ("the app and the deck
//! change together"), and every deck follows it.
use crate::devapi::macro_line;
use crate::{
    DeviceOp, EventKind, FaceEntry, FaceId, HostLine, Key, MacroLayout, Principal, SetKey,
    Settings, Store,
};
use std::collections::BTreeMap;

/// What the host keeps for one deck.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeckProfile {
    /// The strip, as the deck last reported it.
    pub faces: Vec<FaceEntry>,
    /// Its `set` keys. The language in it is not read: the Mac's stays.
    pub settings: Settings,
    /// Each Macropad face's keys, by face id.
    pub macros: BTreeMap<u8, MacroLayout>,
}

impl Store {
    /// The deck that linked is `deck` (its label, `device.deck`), and
    /// `profile` is what the service keeps for it.
    ///
    /// Each part that differs from the snapshot's goes to the view as the
    /// lines a sync would send: the strip (taken away when this deck has
    /// none, so a `boot:1` sync leaves the deck its own), every `set` key,
    /// and each Macropad face's keys (those of a face this deck has no keys
    /// for taken away). The view sends nothing it already sent, and gates
    /// what the deck cannot take (`arcadesound`, a `macro` for a face not on
    /// its strip) the way it gates every other line. The Mac's mixer follows
    /// the speaker and microphone, and every document is wrapped again when
    /// the text size moved. One `device.profile` says it all; a profile the
    /// snapshot already holds says and sends nothing.
    ///
    /// Nothing is persisted: `state.json` already holds this profile, which
    /// is where it came from.
    pub(crate) fn on_deck_profile(&mut self, deck: Option<String>, profile: DeckProfile) {
        let DeckProfile {
            faces,
            mut settings,
            macros,
        } = profile;
        let was = self.snapshot.settings;
        settings.language = was.language;
        settings.language_revision = was.language_revision;
        let faces_changed = self.snapshot.faces != faces;
        let settings_changed = was != settings;
        let macros_changed = self.snapshot.macros != macros;
        if !faces_changed
            && !settings_changed
            && !macros_changed
            && self.snapshot.device.deck == deck
        {
            return;
        }
        // The slot the last deck showed (`Store::reader`) is not this one's:
        // the lazy reader feed waits for this deck to name its own. The
        // first deck identified here claims the Mac's setup, view included.
        if self.snapshot.device.deck.is_some() && self.snapshot.device.deck != deck {
            self.deck_view = None;
        }
        if faces_changed {
            self.device_op(if faces.is_empty() {
                DeviceOp::Remove(Key::Faces)
            } else {
                DeviceOp::Put(
                    Key::Faces,
                    HostLine::Faces {
                        list: faces.clone(),
                    },
                )
            });
        }
        if settings_changed {
            for (key, value) in settings.iter() {
                self.device_op(DeviceOp::Put(Key::Set(key), HostLine::Set { key, value }));
            }
        }
        if macros_changed {
            let gone: Vec<u8> = self
                .snapshot
                .macros
                .keys()
                .filter(|f| !macros.contains_key(f))
                .copied()
                .collect();
            for f in gone {
                self.device_op(DeviceOp::Remove(Key::Macro(FaceId(f))));
            }
            for (f, layout) in &macros {
                self.device_op(DeviceOp::Put(
                    Key::Macro(FaceId(*f)),
                    macro_line(*f, layout),
                ));
            }
        }
        let data = serde_json::json!({
            "deck": deck, "faces": faces, "settings": settings, "macros": macros,
        });
        self.snapshot.faces = faces;
        self.snapshot.settings = settings;
        self.snapshot.macros = macros;
        let device = &mut self.snapshot.device;
        device.settings = Settings {
            language: device.settings.language,
            language_revision: device.settings.language_revision,
            ..settings
        };
        device.deck = deck;
        for key in [SetKey::Speaker, SetKey::Mic] {
            if was.get(key) != settings.get(key) {
                self.push_gain(key, settings.get(key));
            }
        }
        if was.textsize != settings.textsize {
            self.relayout_documents();
        }
        self.emit(EventKind::DeviceProfile, Principal::Device, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sim;
    use crate::{AudioCmd, Binding, Command, DeviceLine, Effect, MacroKey, StatePatch, Template};

    fn face(f: u8, t: Template) -> FaceEntry {
        FaceEntry {
            f,
            t,
            n: format!("{t:?}"),
        }
    }

    /// Agents, a Macropad at `pad`, Settings.
    fn strip(pad: u8) -> Vec<FaceEntry> {
        vec![
            face(1, Template::Agents),
            face(pad, Template::Macropad),
            face(5, Template::Settings),
        ]
    }

    fn keys(label: &str) -> MacroLayout {
        MacroLayout {
            src: Some("app".into()),
            keys: vec![MacroKey {
                label: label.into(),
                binding: Some(Binding::Left),
                keys: Some("cmd+b".into()),
            }],
        }
    }

    /// Deck A as the snapshot holds it: its strip with the Macropad at 3,
    /// the speaker at 3, keys on face 3, the Mac in Japanese.
    fn deck_a() -> Sim {
        let mut sim = Sim::with(|cfg| {
            cfg.persisted.faces = strip(3);
            cfg.persisted.settings.speaker = 3;
            cfg.persisted.settings.language = 3;
            cfg.persisted.settings.language_revision = 4;
            cfg.persisted.macros.insert(3, keys("build"));
            cfg.persisted.deck = Some("aaaaaaaa".into());
        });
        sim.drain();
        sim
    }

    fn removes(sim: &Sim) -> Vec<Key> {
        sim.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Device(DeviceOp::Remove(k)) => Some(*k),
                _ => None,
            })
            .collect()
    }

    fn gains(sim: &Sim) -> Vec<(SetKey, u16)> {
        sim.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Audio(AudioCmd::Gain { key, value }) => Some((*key, *value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_deck_profile_swaps_strip_settings_and_keys_and_says_so_once() {
        let mut sim = deck_a();
        assert_eq!(
            sim.store.snapshot().device.deck.as_deref(),
            Some("aaaaaaaa")
        );
        let b = DeckProfile {
            faces: strip(6),
            settings: Settings {
                mic: 2,
                ..Settings::default()
            },
            macros: BTreeMap::from([(6, keys("deploy"))]),
        };
        sim.apply(Command::DeckProfile {
            deck: Some("bbbbbbbb".into()),
            profile: b.clone(),
        });
        // The strip, the ten `set` keys (never the language, which is the
        // Mac's and negotiated on its own), face 3's keys taken away and
        // face 6's put.
        let mut puts = vec![Key::Faces];
        puts.extend(SetKey::ALL.map(Key::Set));
        puts.push(Key::Macro(FaceId(6)));
        assert_eq!(sim.puts(), puts);
        assert_eq!(removes(&sim), vec![Key::Macro(FaceId(3))]);
        assert_eq!(
            sim.line(Key::Faces),
            Some(HostLine::Faces { list: strip(6) })
        );
        assert_eq!(
            sim.line(Key::Set(SetKey::Speaker)),
            Some(HostLine::Set {
                key: SetKey::Speaker,
                value: 6
            })
        );
        assert_eq!(
            sim.line(Key::Macro(FaceId(6))),
            Some(macro_line(6, &keys("deploy")))
        );
        // The Mac's mixer follows the deck in the hand.
        assert_eq!(gains(&sim), vec![(SetKey::Speaker, 6), (SetKey::Mic, 2)]);
        // Nothing to persist: the service's record is where it came from.
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Persist(_))));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceProfile]);
        let data = &sim.events[0].data;
        assert_eq!(data["deck"], "bbbbbbbb");
        assert_eq!(data["faces"], serde_json::to_value(strip(6)).unwrap());
        assert_eq!(data["settings"]["mic"], 2);
        assert_eq!(data["macros"]["6"]["keys"][0]["label"], "deploy");
        let snap = sim.store.snapshot();
        assert_eq!(snap.faces, strip(6));
        assert_eq!(snap.macros, b.macros);
        assert_eq!(snap.device.deck.as_deref(), Some("bbbbbbbb"));
        assert_eq!((snap.settings.speaker, snap.settings.mic), (6, 2));
        assert_eq!(
            (snap.device.settings.speaker, snap.device.settings.mic),
            (6, 2)
        );
        // The language and its revision are the Mac's, whatever the profile
        // carried.
        assert_eq!(
            (snap.settings.language, snap.settings.language_revision),
            (3, 4)
        );
        assert_eq!(data["settings"]["language"], 3);
    }

    #[test]
    fn an_empty_strip_takes_the_faces_entry_away() {
        let mut sim = deck_a();
        sim.apply(Command::DeckProfile {
            deck: Some("cccccccc".into()),
            profile: DeckProfile::default(),
        });
        // A deck the host keeps no strip for keeps its own at a `boot:1`
        // sync: the view has no strip to send it.
        assert!(removes(&sim).contains(&Key::Faces));
        assert!(!sim.puts().contains(&Key::Faces));
        assert!(sim.store.snapshot().faces.is_empty());
        assert!(sim.store.snapshot().macros.is_empty());
        assert_eq!(sim.kinds(), vec![EventKind::DeviceProfile]);
    }

    #[test]
    fn the_same_profile_again_says_and_sends_nothing() {
        let mut sim = deck_a();
        let a = DeckProfile {
            faces: strip(3),
            settings: Settings {
                speaker: 3,
                ..Settings::default()
            },
            macros: BTreeMap::from([(3, keys("build"))]),
        };
        sim.apply(Command::DeckProfile {
            deck: Some("aaaaaaaa".into()),
            profile: a.clone(),
        });
        assert!(sim.events.is_empty(), "{:?}", sim.kinds());
        assert!(sim.effects.is_empty(), "{:?}", sim.effects);
        // The first deck identified after an upgrade: the profile is the
        // one the Mac had, and only the label is new. It is said, and
        // nothing goes to the deck.
        let mut sim = Sim::with(|cfg| {
            cfg.persisted.faces = strip(3);
            cfg.persisted.settings.speaker = 3;
            cfg.persisted.macros.insert(3, keys("build"));
        });
        sim.drain();
        assert_eq!(sim.store.snapshot().device.deck, None);
        sim.apply(Command::DeckProfile {
            deck: Some("aaaaaaaa".into()),
            profile: a,
        });
        assert_eq!(sim.kinds(), vec![EventKind::DeviceProfile]);
        assert!(sim.effects.is_empty(), "{:?}", sim.effects);
        assert_eq!(
            sim.store.snapshot().device.deck.as_deref(),
            Some("aaaaaaaa")
        );
    }

    #[test]
    fn a_deck_with_no_stored_strip_is_remembered_with_its_own_on_boot_1() {
        let mut sim = deck_a();
        sim.apply(Command::DeckProfile {
            deck: Some("cccccccc".into()),
            profile: DeckProfile::default(),
        });
        let hello = |faces: Vec<FaceEntry>| DeviceLine::Hello {
            v: 1,
            fw: "0.13.0".into(),
            sid: 0,
            boot: true,
            rx: crate::LINE_MAX_U16,
            agents: None,
            lang: None,
            lang_rev: None,
            faces,
        };
        sim.apply(Command::Device(hello(strip(4))));
        assert_eq!(sim.store.snapshot().faces, strip(4));
        assert!(sim
            .effects
            .contains(&Effect::Persist(StatePatch::Faces(strip(4)))));
        assert!(sim.kinds().contains(&EventKind::DeviceFaces));
        // Nothing is sent back: the deck is showing that strip already.
        assert!(sim.puts().is_empty(), "{:?}", sim.puts());
        // A deck that kept a strip here gets it back on its next `boot:1`.
        sim.apply(Command::Device(hello(strip(6))));
        assert_eq!(sim.store.snapshot().faces, strip(4));
        assert!(!sim.kinds().contains(&EventKind::DeviceFaces));
    }

    #[test]
    fn a_deck_with_another_text_size_gets_every_document_wrapped_for_it() {
        let mut sim = deck_a();
        let id = sim.register(crate::testkit::UID, crate::SessionId(1), "bot", None);
        let slot = sim.slot_of(&id).expect("a slot");
        let owner = sim.store.snapshot().agents[&id].owner.clone();
        sim.api(
            owner.principal,
            owner.session,
            crate::Request::Reply(crate::ReplySet {
                message: None,
                agent: id.clone(),
                text: "word ".repeat(300),
                speak: false,
                announce: None,
                lane: None,
            }),
        )
        .expect("agent.reply");
        sim.apply(Command::DeckProfile {
            deck: Some("dddddddd".into()),
            profile: DeckProfile {
                faces: strip(3),
                settings: Settings {
                    speaker: 3,
                    textsize: 2,
                    ..Settings::default()
                },
                macros: BTreeMap::from([(3, keys("build"))]),
            },
        });
        // Only the text size moved: the ten keys go (the view drops the
        // ones the deck has), the strip and keys do not, the mixer is left
        // alone, and the reply is laid out again for the larger face.
        assert_eq!(sim.store.snapshot().settings.textsize, 2);
        assert!(!sim.puts().contains(&Key::Faces));
        assert!(!sim.puts().contains(&Key::Macro(FaceId(3))));
        assert!(gains(&sim).is_empty());
        assert!(sim.puts().contains(&Key::Reply(slot)), "{:?}", sim.puts());
        assert!(
            sim.puts()
                .iter()
                .any(|key| matches!(key, Key::DocText(s, crate::DocKind::Reply, _) if *s == slot)),
            "{:?}",
            sim.puts()
        );
        assert_eq!(
            sim.store.snapshot().device.deck.as_deref(),
            Some("dddddddd")
        );
        let json = serde_json::to_value(&sim.store.snapshot().device).unwrap();
        assert_eq!(json["deck"], "dddddddd");
        // A service with no deck identified writes no `deck` at all.
        let fresh = Sim::new();
        let json = serde_json::to_value(&fresh.store.snapshot().device).unwrap();
        assert!(json.get("deck").is_none());
    }

    /// R6 and R7 together: the slot a deck last showed is that deck's view
    /// (`Store::reader`, the lazy reader feed). The same deck back after a
    /// drop keeps it; another deck in the hand has shown nothing yet, so the
    /// session on that slot is no longer read for it.
    #[test]
    fn another_deck_in_the_hand_does_not_inherit_the_last_ones_view() {
        use crate::testkit::UID;
        use crate::{DocKind, SessionId};
        use std::collections::BTreeSet;
        let mut sim = deck_a();
        let a = sim.register(UID, SessionId(1), "a", Some("k:a"));
        let slot = sim.slot_of(&a).expect("on the deck").0;
        sim.apply(Command::Device(DeviceLine::DocLines {
            slot,
            doc: DocKind::Reply,
            id: 0,
            first: 0,
        }));
        assert_eq!(sim.store.reader(), BTreeSet::from([a.clone()]));
        let own = DeckProfile {
            faces: strip(3),
            settings: Settings {
                speaker: 3,
                ..Settings::default()
            },
            macros: BTreeMap::from([(3, keys("build"))]),
        };
        sim.apply(Command::DeckProfile {
            deck: Some("aaaaaaaa".into()),
            profile: own,
        });
        assert_eq!(
            sim.store.reader(),
            BTreeSet::from([a.clone()]),
            "deck A again"
        );
        sim.apply(Command::DeckProfile {
            deck: Some("bbbbbbbb".into()),
            profile: DeckProfile::default(),
        });
        assert!(sim.store.reader().is_empty(), "deck B named no slot yet");
        sim.apply(Command::Device(DeviceLine::DocLines {
            slot,
            doc: DocKind::Reply,
            id: 0,
            first: 0,
        }));
        assert_eq!(sim.store.reader(), BTreeSet::from([a]), "until it does");
    }
}
