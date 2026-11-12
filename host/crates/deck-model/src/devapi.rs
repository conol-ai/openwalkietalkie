//! `device.*` API methods (§4.10): settings pushed and persisted (never
//! echoed), the strip, the Usage face, Macropad labels, Omarchy content, the
//! spoken name of a new face.
use crate::api::{ApiError, ApiResult};
use crate::{
    Control, DeviceOp, Effect, EventKind, FaceEntry, FaceId, HostLine, Key, MacroKey, MacroLayout,
    Principal, SetKey, StatePatch, Store, Usage, UsageItem, Window,
};

/// The `macro` line that gives the deck face `f`'s keys: always the whole
/// set, which is all the deck accepts.
pub(crate) fn macro_line(f: u8, macros: &MacroLayout) -> HostLine {
    HostLine::Macro {
        face: f,
        src: macros.src.clone(),
        keys: macros.keys.clone(),
    }
}

impl Store {
    /// §6.4: `set speaker` and `set mic` **are** the mixer's master gain
    /// and the capture scale, so a change to either is an audio command
    /// and not only a stored number (B4).
    ///
    /// It is pushed beside the `Persist` at every site that writes a
    /// setting — the API method here and the deck's own `set` line in
    /// `Store::on_device` — because those two are the same knob turned from
    /// two ends, and a fix applied to one of them would leave the deck's
    /// own volume buttons inert. The other six keys are host policy
    /// (`announce`, `chime`) or device-local, and the audio side has
    /// nothing to do about them.
    pub(crate) fn push_gain(&mut self, key: SetKey, value: u16) {
        if matches!(key, SetKey::Speaker | SetKey::Mic) {
            self.effects
                .push(Effect::Audio(crate::AudioCmd::Gain { key, value }));
        }
    }

    pub(crate) fn device_set(
        &mut self,
        key: SetKey,
        value: u16,
        principal: Principal,
    ) -> ApiResult {
        if !key.accepts(value) {
            return Err(ApiError::invalid("v", "range"));
        }
        if key == SetKey::Language {
            let revision = self.snapshot.settings.language_revision.saturating_add(1);
            self.snapshot.settings.language = value;
            self.snapshot.settings.language_revision = revision;
            self.snapshot.device.settings.language = value;
            self.snapshot.device.settings.language_revision = revision;
            self.effects.push(Effect::Persist(StatePatch::Language { value, revision }));
            self.device_op(DeviceOp::Put(Key::Set(key), HostLine::Language { value: value as u8, revision }));
            self.emit(EventKind::DeviceSet, principal, serde_json::json!({"k": key, "v": value, "rev": revision}));
            return Ok(self.rev());
        }
        let changed = self.snapshot.settings.set(key, value);
        let dev_changed = self.snapshot.device.settings.set(key, value);
        if changed || dev_changed {
            self.effects
                .push(Effect::Persist(StatePatch::Setting { key, value }));
            self.push_gain(key, value);
            self.device_op(DeviceOp::Put(Key::Set(key), HostLine::Set { key, value }));
            self.emit(
                EventKind::DeviceSet,
                principal,
                serde_json::json!({"k": key, "v": value}),
            );
        }
        Ok(self.rev())
    }

    /// Newer deck choices win; equal-revision conflicts resolve to the app.
    /// The desired line also acknowledges support without a mirrored event.
    pub(crate) fn receive_language(&mut self, value: u8, revision: u32) {
        if value > 3 || !self.snapshot.device.language_supported { return; }
        self.snapshot.device.settings.language = value as u16;
        self.snapshot.device.settings.language_revision = revision;
        if revision > self.snapshot.settings.language_revision {
            self.snapshot.settings.language = value as u16;
            self.snapshot.settings.language_revision = revision;
            self.effects.push(Effect::Persist(StatePatch::Language { value: value as u16, revision }));
            self.emit(EventKind::DeviceSet, Principal::Device, serde_json::json!({"k": SetKey::Language, "v": value, "rev": revision}));
        }
        self.device_op(DeviceOp::Put(Key::Set(SetKey::Language), HostLine::Language {
            value: self.snapshot.settings.language as u8,
            revision: self.snapshot.settings.language_revision,
        }));
    }

    /// Sent, not adopted: the next device `faces` event is the truth.
    pub(crate) fn device_faces_set(&mut self, list: Vec<FaceEntry>) -> ApiResult {
        if list.len() > 8 {
            return Err(ApiError::invalid("list", "max 8"));
        }
        let mut seen = [false; 9];
        for entry in &list {
            if !(1..=8).contains(&entry.f) {
                return Err(ApiError::invalid("list", "f 1..=8"));
            }
            if std::mem::replace(&mut seen[usize::from(entry.f)], true) {
                return Err(ApiError::invalid("list", "duplicate f"));
            }
            if entry.n.trim().is_empty() {
                return Err(ApiError::invalid("list", "empty name"));
            }
        }
        self.device_op(DeviceOp::Put(Key::Faces, HostLine::Faces { list }));
        Ok(serde_json::json!({"rev": self.snapshot.rev, "sent": true}))
    }

    pub(crate) fn device_usage_set(
        &mut self,
        period: String,
        items: Vec<UsageItem>,
        principal: Principal,
    ) -> ApiResult {
        if period.trim().is_empty() {
            return Err(ApiError::invalid("period", "empty"));
        }
        if items.len() > 6 {
            return Err(ApiError::invalid("items", "max 6"));
        }
        if items.iter().any(|i| i.percent > 100) {
            return Err(ApiError::invalid("items", "percent 0..=100"));
        }
        let changed = self
            .snapshot
            .usage
            .as_ref()
            .is_none_or(|u| u.period != period || u.items != items);
        let usage = Usage {
            period: period.clone(),
            items: items.clone(),
            at: Some(self.ts_now()),
        };
        self.usage_refresh_at = None;
        self.snapshot.usage = Some(usage.clone());
        // Re-pushed even when unchanged: it ends the device's "updating…".
        self.device_op(DeviceOp::Put(Key::Usage, HostLine::Usage { period, items }));
        if changed {
            self.effects
                .push(Effect::Persist(StatePatch::Usage(usage.clone())));
            self.emit(
                EventKind::UsageUpdated,
                principal,
                serde_json::json!({"usage": usage}),
            );
        }
        Ok(self.rev())
    }

    /// The cached `usage` re-sent unchanged when no feeder answered a `refresh`.
    pub(crate) fn resend_usage(&mut self) {
        if let Some(usage) = &self.snapshot.usage {
            let line = HostLine::Usage {
                period: usage.period.clone(),
                items: usage.items.clone(),
            };
            self.device_op(DeviceOp::Put(Key::Usage, line));
        }
    }

    /// `device.macro.set`: face `f`'s keys, whole, persisted with the name
    /// of the layout they came from and sent to the deck. A repeated set is
    /// persisted and sent like any other (the outbox drops a line the deck
    /// already has) but changes nothing a client can see, so it says
    /// nothing.
    pub(crate) fn device_macro_set(
        &mut self,
        f: u8,
        layout: String,
        src: Option<String>,
        keys: Vec<MacroKey>,
        principal: Principal,
    ) -> ApiResult {
        if !(1..=8).contains(&f) {
            return Err(ApiError::invalid("f", "1..=8"));
        }
        if keys.len() > 9 {
            return Err(ApiError::invalid("keys", "max 9"));
        }
        if layout.trim().is_empty() {
            return Err(ApiError::invalid("layout", "empty"));
        }
        if keys.iter().any(|key| key.keys.as_deref().is_some_and(|k| !deck_proto::keystroke_valid(k))) {
            return Err(ApiError::invalid("keys", "unknown keystroke"));
        }
        self.effects.push(Effect::Persist(StatePatch::Macro {
            f,
            src: src.clone(),
            keys: keys.clone(),
        }));
        let macros = MacroLayout { src, keys };
        self.effects
            .push(Effect::Persist(StatePatch::MacropadBinding { f, layout }));
        self.device_op(DeviceOp::Put(Key::Macro(FaceId(f)), macro_line(f, &macros)));
        self.set_macros(f, macros, principal);
        Ok(self.rev())
    }

    /// Face `f`'s keys are now `macros`, from the API or the deck's own
    /// editor. The snapshot holds them (every `macro` line is built from
    /// it), and a change is said once, as `device.macros` with the whole
    /// layout: a reader replaces what it had for `f` and never has to
    /// merge single edits. Persisting and sending stay with the caller.
    pub(crate) fn set_macros(&mut self, f: u8, macros: MacroLayout, principal: Principal) {
        if self.snapshot.macros.get(&f) == Some(&macros) {
            return;
        }
        let data = serde_json::json!({"f": f, "src": macros.src, "keys": macros.keys});
        self.snapshot.macros.insert(f, macros);
        self.emit(EventKind::DeviceMacros, principal, data);
    }

    pub(crate) fn device_ws_set(
        &mut self,
        active: u8,
        count: u8,
        sel: u8,
        windows: Vec<Window>,
    ) -> ApiResult {
        if count > 9 {
            return Err(ApiError::invalid("count", "max 9"));
        }
        if active > count {
            return Err(ApiError::invalid("active", "active <= count"));
        }
        if windows.len() > 6 {
            return Err(ApiError::invalid("windows", "max 6"));
        }
        if usize::from(sel) >= windows.len().max(1) {
            return Err(ApiError::invalid("sel", "sel < windows.length"));
        }
        let line = HostLine::Ws {
            active,
            count,
            sel,
            windows,
        };
        self.ws = Some(line.clone());
        self.device_op(DeviceOp::Put(Key::Ws, line));
        Ok(self.rev())
    }

    /// `device.name{text}` (§4.10) — the answer to `ptt ctx:name`, which is
    /// the one place a *spoken* string becomes a face's **name**.
    ///
    /// §7.4: cut to 16 bytes on a character boundary (five CJK characters)
    /// and title-cased for Latin. B15: none of that happened here, and the
    /// wire encoder's own cut appends `…` (`CAPS.spoken_name`), so a
    /// too-long dictated name reached the deck as `Deploy sta…` — an
    /// ellipsis is right for a card's text and wrong for a name, where it
    /// reads as a device fault rather than as a quotation. The cut belongs
    /// on this side, before the op, with [`sanitize::cut_bare`]: by the time
    /// the encoder sees the line there is nothing left to cut.
    pub(crate) fn device_name(&mut self, text: String) -> ApiResult {
        // Collapse first: a dictated name arrives with the recogniser's own
        // spacing, and leading space would spend the budget on nothing.
        let text = crate::sanitize::collapse(&text);
        if text.is_empty() {
            return Err(ApiError::invalid("text", "empty"));
        }
        // Title-case **before** the cut: upper-casing can grow a string
        // (`ß` → `SS`), so cutting first would not bound the bytes.
        // The budget is the firmware's own, read from `deck_proto` rather
        // than copied, so the two cannot drift.
        let text = crate::sanitize::cut_bare(
            &crate::sanitize::title_case(&text),
            deck_proto::caps().spoken_name,
        );
        // The cut is on a character boundary, not a word one (§7.4's own
        // rule, and the budget is the firmware's line), but a name must not
        // end in the space the cut happened to land on.
        let text = text.trim_end().to_string();
        if text.is_empty() {
            return Err(ApiError::invalid("text", "empty"));
        }
        self.device_op(DeviceOp::Control(Control::Name(text)));
        Ok(self.rev())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Sim, UID};
    use crate::{Command, DeviceLine, DeviceSet, Request, SessionId, Template, Tone};
    use std::time::Duration;

    fn face(f: u8) -> FaceEntry {
        FaceEntry {
            f,
            t: Template::Macropad,
            n: format!("Face {f}"),
        }
    }

    #[test]
    fn device_set_pushes_persists_and_validates() {
        let mut sim = Sim::new();
        let res = sim
            .api(
                UID,
                SessionId(1),
                Request::DeviceSet(DeviceSet {
                    key: SetKey::Brightness,
                    value: 2,
                }),
            )
            .unwrap();
        assert_eq!(res["rev"], 1);
        assert_eq!(
            sim.effects,
            vec![
                Effect::Persist(StatePatch::Setting {
                    key: SetKey::Brightness,
                    value: 2
                }),
                Effect::Device(DeviceOp::Put(
                    Key::Set(SetKey::Brightness),
                    HostLine::Set {
                        key: SetKey::Brightness,
                        value: 2
                    }
                )),
            ]
        );
        assert_eq!(sim.events[0].kind, EventKind::DeviceSet);
        assert_eq!(sim.store.snapshot().settings.brightness, 2);
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceSet(DeviceSet {
                key: SetKey::Brightness,
                value: 2,
            }),
        )
        .unwrap();
        assert!(sim.effects.is_empty() && sim.events.is_empty());
        let err = sim
            .api(
                UID,
                SessionId(1),
                Request::DeviceSet(DeviceSet {
                    key: SetKey::Brightness,
                    value: 0,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        // The device's own change is not echoed, and a later API set of the
        // same value only persists the host copy.
        sim.apply(Command::Device(DeviceLine::Set {
            key: SetKey::Speaker,
            value: 3,
        }));
        assert!(sim.puts().is_empty());
        assert_eq!(sim.store.snapshot().device.settings.speaker, 3);
    }

    /// B4 / §6.4: `set speaker` and `set mic` **are** the mixer's master
    /// gain and the capture scale, so turning either has to reach the audio
    /// side. Before this they only persisted and emitted `device.set`: the
    /// number in `state.json` moved and the next announcement was exactly
    /// as loud as before.
    #[test]
    fn the_two_gain_settings_reach_the_mixer_from_both_ends_and_the_others_do_not() {
        let mut sim = Sim::new();
        let gains = |sim: &Sim| -> Vec<Effect> {
            sim.effects
                .iter()
                .filter(|e| matches!(e, Effect::Audio(crate::AudioCmd::Gain { .. })))
                .cloned()
                .collect()
        };
        // From the API…
        for (key, value) in [(SetKey::Speaker, 3), (SetKey::Mic, 7)] {
            sim.api(
                UID,
                SessionId(1),
                Request::DeviceSet(DeviceSet { key, value }),
            )
            .unwrap();
            assert_eq!(
                gains(&sim),
                vec![Effect::Audio(crate::AudioCmd::Gain { key, value })],
                "{key:?} from the API"
            );
            // Beside the `Persist`, never instead of it: the stored value
            // and the mixer's cannot be allowed to disagree.
            assert!(sim
                .effects
                .contains(&Effect::Persist(StatePatch::Setting { key, value })));
        }
        // …and from the deck's own buttons, which are the same knob.
        for (key, value) in [(SetKey::Speaker, 8), (SetKey::Mic, 0)] {
            sim.apply(Command::Device(DeviceLine::Set { key, value }));
            assert_eq!(
                gains(&sim),
                vec![Effect::Audio(crate::AudioCmd::Gain { key, value })],
                "{key:?} from the deck"
            );
        }
        // Host policy and device-local keys have nothing for the mixer.
        for (key, value) in [
            (SetKey::Announce, 0),
            (SetKey::Chime, 1),
            (SetKey::Brightness, 2),
            (SetKey::Autojump, 1),
        ] {
            sim.api(
                UID,
                SessionId(1),
                Request::DeviceSet(DeviceSet { key, value }),
            )
            .unwrap();
            assert!(gains(&sim).is_empty(), "{key:?} is not a gain");
        }
        // An unchanged value is not a change at either end, so the mixer is
        // not told about it twice.
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceSet(DeviceSet {
                key: SetKey::Speaker,
                value: 8,
            }),
        )
        .unwrap();
        assert!(gains(&sim).is_empty(), "already at 8");
        // …and a value the device would reject never reaches the mixer.
        assert!(sim
            .api(
                UID,
                SessionId(1),
                Request::DeviceSet(DeviceSet {
                    key: SetKey::Speaker,
                    value: 99
                })
            )
            .is_err());
        assert!(gains(&sim).is_empty(), "out of range");
    }

    #[test]
    fn faces_set_is_sent_not_adopted() {
        let mut sim = Sim::new();
        let res = sim
            .api(
                UID,
                SessionId(1),
                Request::DeviceFacesSet {
                    list: vec![face(1), face(2)],
                },
            )
            .unwrap();
        assert_eq!(res["sent"], true);
        assert_eq!(sim.puts(), vec![Key::Faces]);
        assert!(sim.store.snapshot().faces.is_empty());
        assert!(sim.events.is_empty());
        for (list, why) in [
            ((1..=9).map(face).collect::<Vec<_>>(), "max 8"),
            (vec![face(0)], "f 1..=8"),
            (vec![face(1), face(1)], "duplicate f"),
            (
                vec![FaceEntry {
                    f: 1,
                    t: Template::Agents,
                    n: " ".into(),
                }],
                "empty name",
            ),
        ] {
            let err = sim
                .api(UID, SessionId(1), Request::DeviceFacesSet { list })
                .unwrap_err();
            assert_eq!(err.data["why"], why);
        }
    }

    #[test]
    fn usage_set_updates_persists_and_answers_a_refresh() {
        let mut sim = Sim::new();
        let items = vec![UsageItem {
            reset_tip: None,
            name: "Claude 5h".into(),
            account: None,
            percent: 42,
            reset: "resets 21:00".into(),
            tone: Some(Tone::Green),
        }];
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceUsageSet {
                period: "5h / 7d".into(),
                items: items.clone(),
            },
        )
        .unwrap();
        assert_eq!(sim.events[0].kind, EventKind::UsageUpdated);
        assert!(sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Persist(StatePatch::Usage(_)))));
        assert_eq!(sim.puts(), vec![Key::Usage]);
        assert_eq!(
            sim.store.snapshot().usage.as_ref().unwrap().at,
            Some(sim.store.ts(sim.t))
        );
        // Same numbers: pushed again, no event, no persist.
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceUsageSet {
                period: "5h / 7d".into(),
                items: items.clone(),
            },
        )
        .unwrap();
        assert!(sim.events.is_empty());
        assert_eq!(sim.puts(), vec![Key::Usage]);
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Persist(_))));
        // refresh with no feeder → the cached usage re-sent at 1.5 s.
        sim.apply(Command::Device(DeviceLine::Refresh {
            what: "usage".into(),
        }));
        sim.advance(Duration::from_millis(1000));
        assert!(sim.puts().is_empty());
        sim.advance(Duration::from_millis(600));
        assert_eq!(sim.puts(), vec![Key::Usage]);
        sim.advance(Duration::from_secs(5));
        assert!(sim.puts().is_empty(), "once");
        // A feeder answering in time cancels the re-send.
        sim.apply(Command::Device(DeviceLine::Refresh {
            what: "usage".into(),
        }));
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceUsageSet {
                period: "5h / 7d".into(),
                items: vec![],
            },
        )
        .unwrap();
        sim.advance(Duration::from_secs(2));
        assert!(sim.puts().is_empty());
        for (period, items, why) in [
            (" ", vec![], "empty"),
            ("x", vec![items[0].clone(); 7], "max 6"),
            (
                "x",
                vec![UsageItem {
                    reset_tip: None,
                    name: "n".into(),
                    account: None,
                    percent: 101,
                    reset: String::new(),
                    tone: None,
                }],
                "percent 0..=100",
            ),
        ] {
            let err = sim
                .api(
                    UID,
                    SessionId(1),
                    Request::DeviceUsageSet {
                        period: period.into(),
                        items,
                    },
                )
                .unwrap_err();
            assert_eq!(err.data["why"], why);
        }
    }

    #[test]
    fn a_key_edited_on_the_deck_is_stored_persisted_sent_back_and_comes_back_after_a_restart() {
        use crate::{Command, DeviceLine, EventKind, MacroLayout};
        let edit = |i: u8, label: Option<&str>, binding: Option<crate::Binding>| {
            Command::Device(DeviceLine::MacroKey {
                f: 3,
                i,
                label: label.map(str::to_string),
                binding,
                keys: None,
            })
        };
        let key = |label: &str, binding: Option<crate::Binding>| MacroKey {
            label: label.into(),
            binding,
            keys: None,
        };
        let mut sim = Sim::new();
        sim.apply(edit(2, Some("shot"), Some(crate::Binding::Cw)));
        assert_eq!(sim.puts(), vec![Key::Macro(FaceId(3))]);
        // Slots before it are empty labels, so the keys keep their places.
        let keys = vec![key("", None), key("", None), key("shot", Some(crate::Binding::Cw))];
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::Macro {
            f: 3,
            src: None,
            keys: keys.clone()
        })));
        assert!(sim.events.iter().any(|e| e.kind == EventKind::DeviceMacroEdit && e.data["l"] == "shot"));
        assert!(sim
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Macro(FaceId(3)), HostLine::Macro { keys: k, .. }) if *k == keys)));
        // Clearing the last key trims the list; clearing a middle one does not.
        sim.apply(edit(0, Some("build"), Some(crate::Binding::Left)));
        sim.apply(edit(2, None, None));
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::Macro {
            f: 3,
            src: None,
            keys: vec![key("build", Some(crate::Binding::Left))]
        })));
        sim.apply(edit(2, Some("shot"), None));
        sim.apply(edit(0, None, None));
        let keys = vec![key("", None), key("", None), key("shot", None)];
        assert!(sim.effects.contains(&Effect::Persist(StatePatch::Macro { f: 3, src: None, keys: keys.clone() })));
        // A press (or a wheel step) of a key with a keystroke posts it; a
        // release and a key without one do not.
        sim.apply(edit(1, Some("copy"), None));
        sim.apply(Command::Device(DeviceLine::MacroKey { f: 3, i: 2, label: Some("shot".into()), binding: None, keys: Some("cmd+shift+4".into()) }));
        sim.apply(Command::Device(DeviceLine::Macro { f: 3, i: 2, act: 1 }));
        assert!(sim.effects.contains(&Effect::Desktop(crate::DesktopCmd::Keystroke { keys: "cmd+shift+4".into() })));
        sim.apply(Command::Device(DeviceLine::Macro { f: 3, i: 2, act: 0 }));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
        sim.apply(Command::Device(DeviceLine::Macro { f: 3, i: 1, act: 2 }));
        assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
        let keys = vec![key("", None), key("copy", None), MacroKey { label: "shot".into(), binding: None, keys: Some("cmd+shift+4".into()) }];
        // A restart restores the line from `state.json`.
        let restored = Sim::with(|cfg| {
            cfg.persisted.macros.insert(
                3,
                MacroLayout {
                    src: Some("deck".into()),
                    keys: keys.clone(),
                },
            );
        });
        assert!(restored
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Macro(FaceId(3)), HostLine::Macro { src: Some(s), keys: k, .. }) if s == "deck" && *k == keys)));
    }

    /// The keys a client sets are the snapshot's, and the one event that
    /// says so carries the whole layout and who set it. The same set again
    /// changes nothing a client can see, so it says nothing.
    #[test]
    fn a_macro_set_puts_the_keys_in_the_snapshot_and_says_so_once() {
        let keys = vec![
            MacroKey {
                label: "build".into(),
                binding: Some(crate::Binding::Left),
                keys: Some("cmd+b".into()),
            },
            MacroKey {
                label: String::new(),
                binding: None,
                keys: None,
            },
            MacroKey {
                label: "shot".into(),
                binding: None,
                keys: Some("cmd+shift+4".into()),
            },
        ];
        let set = |src: Option<&str>| Request::DeviceMacroSet {
            f: 3,
            layout: "app".into(),
            src: src.map(str::to_string),
            keys: keys.clone(),
        };
        let mut sim = Sim::new();
        let res = sim.api(UID, SessionId(1), set(None)).unwrap();
        assert_eq!(sim.kinds(), vec![EventKind::DeviceMacros]);
        let said = &sim.events[0];
        assert_eq!(said.principal, UID);
        assert_eq!(said.rev, sim.store.snapshot().rev);
        assert_eq!(
            res["rev"], said.rev,
            "the reply names the revision that holds them"
        );
        // The empty middle slot is said too, so `shot` keeps its place.
        assert_eq!(
            said.data,
            serde_json::json!({"f": 3, "src": null, "keys": [
                {"label": "build", "binding": "left", "keys": "cmd+b"},
                {"label": "", "binding": null},
                {"label": "shot", "binding": null, "keys": "cmd+shift+4"}
            ]})
        );
        assert_eq!(
            sim.store.snapshot().macros[&3],
            MacroLayout {
                src: None,
                keys: keys.clone()
            }
        );
        // The same keys again are still persisted and sent, and say nothing.
        let rev = sim.store.snapshot().rev;
        let res = sim.api(UID, SessionId(1), set(None)).unwrap();
        assert!(sim.events.is_empty());
        assert_eq!(res["rev"], rev);
        assert_eq!(sim.puts(), vec![Key::Macro(FaceId(3))]);
        // A new tag on the same keys is a change.
        sim.api(UID, SessionId(1), set(Some("deck.toml"))).unwrap();
        assert_eq!(sim.kinds(), vec![EventKind::DeviceMacros]);
        assert_eq!(sim.events[0].data["src"], "deck.toml");
        assert_eq!(
            sim.store.snapshot().macros[&3].src.as_deref(),
            Some("deck.toml")
        );
        // A refused set leaves the keys as they were.
        let before = sim.store.snapshot().clone();
        let unknown = Request::DeviceMacroSet {
            f: 3,
            layout: "app".into(),
            src: None,
            keys: vec![MacroKey {
                label: "x".into(),
                binding: None,
                keys: Some("cmd+nope".into()),
            }],
        };
        assert!(sim.api(UID, SessionId(1), unknown).is_err());
        assert_eq!(sim.store.snapshot(), &before);
        assert!(sim.events.is_empty() && sim.effects.is_empty());
    }

    /// The deck's own editor changes one slot. The snapshot folds the edit
    /// into the face's whole layout, keeping the tag a client gave it, and
    /// says the result after the edit itself.
    #[test]
    fn a_key_edited_on_the_deck_lands_in_the_snapshot_and_the_whole_face_is_said_after_the_edit() {
        let build = MacroKey {
            label: "build".into(),
            binding: Some(crate::Binding::Left),
            keys: Some("cmd+b".into()),
        };
        let mut sim = Sim::new();
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceMacroSet {
                f: 3,
                layout: "app".into(),
                src: Some("app".into()),
                keys: vec![build.clone()],
            },
        )
        .unwrap();
        let shot = Command::Device(DeviceLine::MacroKey {
            f: 3,
            i: 2,
            label: Some("shot".into()),
            binding: None,
            keys: Some("cmd+shift+4".into()),
        });
        sim.apply(shot.clone());
        assert_eq!(
            sim.kinds(),
            vec![EventKind::DeviceMacroEdit, EventKind::DeviceMacros]
        );
        let layout = MacroLayout {
            src: Some("app".into()),
            keys: vec![
                build.clone(),
                MacroKey {
                    label: String::new(),
                    binding: None,
                    keys: None,
                },
                MacroKey {
                    label: "shot".into(),
                    binding: None,
                    keys: Some("cmd+shift+4".into()),
                },
            ],
        };
        assert_eq!(sim.store.snapshot().macros[&3], layout);
        let said = &sim.events[1];
        assert_eq!(said.principal, Principal::Device);
        assert_eq!(said.rev, sim.store.snapshot().rev);
        assert_eq!(
            said.data,
            serde_json::json!({"f": 3, "src": "app", "keys": layout.keys})
        );
        // The editor saving the same slot again changes no key: only the
        // edit is said.
        sim.apply(shot);
        assert_eq!(sim.kinds(), vec![EventKind::DeviceMacroEdit]);
        // Clearing the last key trims the face, and that is a change.
        sim.apply(Command::Device(DeviceLine::MacroKey {
            f: 3,
            i: 2,
            label: None,
            binding: None,
            keys: None,
        }));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::DeviceMacroEdit, EventKind::DeviceMacros]
        );
        assert_eq!(sim.store.snapshot().macros[&3].keys, vec![build]);
        // A face the host had no keys for starts from none.
        sim.apply(Command::Device(DeviceLine::MacroKey {
            f: 6,
            i: 0,
            label: Some("mute".into()),
            binding: Some(crate::Binding::Cw),
            keys: None,
        }));
        assert_eq!(
            sim.store.snapshot().macros[&6],
            MacroLayout {
                src: None,
                keys: vec![MacroKey {
                    label: "mute".into(),
                    binding: Some(crate::Binding::Cw),
                    keys: None,
                }],
            }
        );
        assert_eq!(
            sim.kinds(),
            vec![EventKind::DeviceMacroEdit, EventKind::DeviceMacros]
        );
        assert_eq!(sim.events[1].data["f"], 6);
    }

    /// After a restart the keys on file are the snapshot's at once, with no
    /// event (nothing changed); the deck is sent them and a press posts from
    /// them, and setting the same keys from the API says nothing.
    #[test]
    fn the_keys_on_file_are_in_the_snapshot_from_the_start_and_a_press_posts_from_them() {
        let layout = MacroLayout {
            src: Some("deck".into()),
            keys: vec![MacroKey {
                label: "shot".into(),
                binding: None,
                keys: Some("cmd+shift+4".into()),
            }],
        };
        let mut sim = Sim::with(|cfg| {
            cfg.persisted.macros.insert(3, layout.clone());
        });
        let snapshot = sim.store.snapshot();
        assert_eq!(
            snapshot.macros,
            std::collections::BTreeMap::from([(3, layout.clone())])
        );
        assert_eq!(snapshot.rev, 0);
        assert!(sim.store.project().contains(&DeviceOp::Put(
            Key::Macro(FaceId(3)),
            macro_line(3, &layout)
        )));
        sim.apply(Command::Device(DeviceLine::Macro { f: 3, i: 0, act: 1 }));
        assert!(sim
            .effects
            .contains(&Effect::Desktop(crate::DesktopCmd::Keystroke {
                keys: "cmd+shift+4".into()
            })));
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceMacroSet {
                f: 3,
                layout: "deck".into(),
                src: layout.src.clone(),
                keys: layout.keys.clone(),
            },
        )
        .unwrap();
        assert!(sim.events.is_empty());
    }

    #[test]
    fn macro_ws_and_name_go_to_the_device_with_their_ranges_checked() {
        let mut sim = Sim::new();
        let window = Window {
            tone: None,
            title: "t".into(),
        };
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceWsSet {
                active: 2,
                count: 4,
                sel: 0,
                windows: vec![window.clone()],
            },
        )
        .unwrap();
        assert_eq!(sim.puts(), vec![Key::Ws]);
        assert!(sim
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Ws, _))));
        for (active, count, sel, n, field) in [
            (1, 10, 0, 0, "count"),
            (5, 4, 0, 0, "active"),
            (0, 0, 0, 7, "windows"),
            (0, 0, 1, 1, "sel"),
            (0, 0, 1, 0, "sel"),
        ] {
            let err = sim
                .api(
                    UID,
                    SessionId(1),
                    Request::DeviceWsSet {
                        active,
                        count,
                        sel,
                        windows: vec![window.clone(); n],
                    },
                )
                .unwrap_err();
            assert_eq!(err.data["field"], field);
        }
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceName {
                text: "Build".into(),
            },
        )
        .unwrap();
        assert_eq!(
            sim.effects,
            vec![Effect::Device(DeviceOp::Control(Control::Name(
                "Build".into()
            )))]
        );
        let err = sim
            .api(UID, SessionId(1), Request::DeviceName { text: " ".into() })
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        assert_eq!(
            sim.store.snapshot().rev,
            0,
            "device-only lines never bump rev"
        );
    }

    /// `device.macro.set` puts face `f`'s keys on the wire and persists the
    /// layout's name, and refuses a face, a name or a key count out of
    /// range. The keys are the snapshot's as well, so a set moves `rev` once
    /// and says so (`device.macros`), and a refusal moves nothing.
    #[test]
    fn a_macro_set_goes_to_the_device_with_its_ranges_checked() {
        let mut sim = Sim::new();
        let keys = vec![MacroKey {
            label: "build".into(),
            binding: Some(crate::Binding::Left),
            keys: None,
        }];
        sim.api(
            UID,
            SessionId(1),
            Request::DeviceMacroSet {
                f: 5,
                layout: "Build".into(),
                src: Some("build.toml".into()),
                keys: keys.clone(),
            },
        )
        .unwrap();
        assert_eq!(sim.puts(), vec![Key::Macro(FaceId(5))]);
        assert!(sim
            .effects
            .contains(&Effect::Persist(StatePatch::MacropadBinding {
                f: 5,
                layout: "Build".into()
            })));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceMacros]);
        let rev = sim.store.snapshot().rev;
        assert!(sim
            .store
            .project()
            .iter()
            .any(|op| matches!(op, DeviceOp::Put(Key::Macro(FaceId(5)), HostLine::Macro { keys: k, .. }) if *k == keys)));
        for (f, layout, keys, why) in [
            (0, "x", vec![], "1..=8"),
            (1, " ", vec![], "empty"),
            (1, "x", vec![keys[0].clone(); 10], "max 9"),
        ] {
            let err = sim
                .api(
                    UID,
                    SessionId(1),
                    Request::DeviceMacroSet {
                        f,
                        layout: layout.into(),
                        src: None,
                        keys,
                    },
                )
                .unwrap_err();
            assert_eq!(err.data["why"], why);
        }
        assert_eq!(sim.store.snapshot().rev, rev, "a refused set moves nothing");
    }

    /// B15 / §7.4: `ptt ctx:name` is the one place a dictated string becomes
    /// a **name**, so it is cut here — to the firmware's 16 bytes, on a
    /// character boundary, with no ellipsis — and title-cased for Latin.
    ///
    /// Before this the store passed the raw string through and the wire
    /// encoder cut it with `…` (`CAPS.spoken_name`), so a long dictated name
    /// arrived as a face called `Deploy staging…`, which reads as a device
    /// fault and not as a quotation.
    #[test]
    fn a_spoken_face_name_is_title_cased_and_cut_to_sixteen_bytes_without_an_ellipsis() {
        let mut sim = Sim::new();
        let named = |sim: &mut Sim, text: &str| {
            sim.api(UID, SessionId(1), Request::DeviceName { text: text.into() })
                .expect("a non-empty name");
            match sim.effects.first() {
                Some(Effect::Device(DeviceOp::Control(Control::Name(text)))) => text.clone(),
                other => panic!("{other:?}"),
            }
        };
        let cap = deck_proto::caps().spoken_name;
        assert_eq!(cap, 16, "the firmware's budget for a spoken name");
        for (said, want) in [
            ("build", "Build"),
            // §7.4 cuts on a character boundary, not a word boundary: the
            // budget is the firmware's line, and a name that loses its last
            // letters is still the name the operator will recognise.
            ("deploy staging now", "Deploy Staging N"),
            // The rest of a word is the operator's: an acronym survives.
            ("API deploy", "API Deploy"),
            // The recogniser's own spacing is collapsed before the budget
            // is spent on it.
            ("  kitchen   deck  ", "Kitchen Deck"),
        ] {
            let got = named(&mut sim, said);
            assert_eq!(got, want, "{said:?}");
            assert!(got.len() <= cap, "{got:?} is {} bytes", got.len());
        }
        // Exactly at the budget: nothing is dropped, nothing is marked.
        let exact = named(&mut sim, "sixteen bytes!!!");
        assert_eq!(exact, "Sixteen Bytes!!!");
        assert_eq!(exact.len(), cap);
        // Five CJK characters are fifteen bytes and fit whole; a sixth is
        // dropped on the boundary, never halved, and no `…` is appended (an
        // ellipsis would itself cost three of the sixteen bytes).
        let cjk = named(&mut sim, "厨房甲板控制台");
        assert_eq!(cjk, "厨房甲板控");
        assert_eq!(cjk.chars().count(), 5);
        assert_eq!(cjk.len(), 15);
        assert!(!cjk.contains('…'), "a face name is not a quotation");
        // The long Latin name the encoder used to ellipsise.
        let long = named(&mut sim, "deploy staging cluster");
        assert!(!long.contains('…'), "{long:?}");
        assert_eq!(long, "Deploy Staging C");
        // A string that is nothing but whitespace is still `-32009`.
        for blank in [" ", "\t\n", ""] {
            let err = sim
                .api(
                    UID,
                    SessionId(1),
                    Request::DeviceName { text: blank.into() },
                )
                .unwrap_err();
            assert_eq!(err.code, ApiError::INVALID, "{blank:?}");
            assert_eq!(err.data["field"], "text");
        }
    }
}
