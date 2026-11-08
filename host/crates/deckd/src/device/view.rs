//! `DeviceView` (design §6.5): the hid thread's copy of what the device
//! should show, one entry per `Key`, kept across link drops and sids; a
//! barrier is its ordered dump.
use deck_model::{DeviceOp, FaceEntry, FaceId, HostLine, Key, SetKey, Slot};
use deck_proto::Template;
use std::collections::BTreeMap;

/// Whether a `ws` line may go out (§6.4, §6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsGate {
    /// An Omarchy face is on screen.
    Visible,
    /// The visible face is unknown and the strip holds an Omarchy face: ≤1 per 2 s.
    Unknown,
    No,
}

/// One step of a barrier, resolved against the view when it is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierItem {
    /// The merged `agent` record (state + title) of a slot.
    Agent(Slot),
    Key(Key),
    /// `boot:1` only: a `set` key the view holds no entry for.
    SetDefault(SetKey, u16),
    /// A `gone` the outbox still owed when the barrier began (never planned by the view).
    Gone(Slot),
    Synced,
}

#[derive(Debug, Default)]
pub struct DeviceView {
    language_supported: bool,
    entries: BTreeMap<Key, HostLine>,
    /// The face on screen, unknown until the first `face` event.
    visible_face: Option<u8>,
    /// The strip as the device last reported it (`hello`/`faces`).
    faces: Vec<FaceEntry>,
}

/// Chat and workflow share the firmware's one reading buffer. Older windows
/// (including the other mode) must not accumulate or reappear on reconnect.
pub(super) fn same_document(slot: Slot, doc: deck_model::DocKind, key: Key) -> bool {
    let Key::DocText(other, kind, _) = key else { return false; };
    let reading = |kind| matches!(kind, deck_model::DocKind::Reply | deck_model::DocKind::Workflow);
    other == slot && (kind == doc || reading(kind) && reading(doc))
}

impl DeviceView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one op in (`Put`/`Remove`); `Sync`/`Control` do not touch the view.
    pub fn apply(&mut self, op: &DeviceOp) {
        match op {
            DeviceOp::Put(key, line) => {
                if key.in_barrier() {
                    if let Key::DocText(slot, doc, _) = key {
                        self.entries.retain(|old, _| !same_document(*slot, *doc, *old) || old == key);
                    }
                    self.entries.insert(*key, line.clone());
                }
            }
            DeviceOp::Remove(key) => {
                self.entries.remove(key);
                if let Key::AgentState(slot) | Key::AgentGone(slot) = key {
                    self.remove_slot(*slot);
                }
            }
            DeviceOp::Sync { .. } | DeviceOp::Control(_) => {}
        }
    }

    /// Forget everything about `slot` (agent gone).
    pub fn remove_slot(&mut self, slot: Slot) {
        self.entries.retain(|k, _| k.slot() != Some(slot));
    }

    pub fn get(&self, key: &Key) -> Option<&HostLine> {
        self.entries.get(key)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Key, &HostLine)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Slots with an agent entry (state or title), ascending.
    pub fn slots(&self) -> Vec<Slot> {
        self.entries
            .keys()
            .filter_map(|k| match k {
                Key::AgentState(s) | Key::AgentTitle(s) => Some(*s),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<Slot>>()
            .into_iter()
            .collect()
    }

    pub fn visible_face(&self) -> Option<u8> {
        self.visible_face
    }

    pub fn set_visible_face(&mut self, face: Option<u8>) {
        self.visible_face = face;
    }

    pub fn faces(&self) -> &[FaceEntry] {
        &self.faces
    }

    pub fn set_faces(&mut self, faces: Vec<FaceEntry>) {
        self.faces = faces;
    }

    pub fn set_language_supported(&mut self, supported: bool) {
        self.language_supported = supported;
    }

    /// New settings require a template advertised by the device, not merely
    /// one in the host's saved layout. Legacy firmware rejects unknown keys.
    pub fn set_allowed(&self, key: SetKey) -> bool {
        if key == SetKey::Language { return self.language_supported; }
        key != SetKey::Arcadesound || self.faces.iter().any(|f| f.t == Template::Arcade)
    }

    /// The strip that gates `macro`/`ws`: the `faces` line the host pushes
    /// (the persisted strip on `boot:1`, which the device never echoes,
    /// P:102-104), else the one the device reported.
    fn strip(&self) -> &[FaceEntry] {
        match self.entries.get(&Key::Faces) {
            Some(HostLine::Faces { list }) if !list.is_empty() => list,
            _ => &self.faces,
        }
    }

    /// True when the strip has a face of template `t`.
    fn strip_has(&self, t: Template) -> bool {
        self.strip().iter().any(|f| f.t == t)
    }

    pub fn system_allowed(&self) -> bool {
        self.faces.iter().any(|f| f.t == Template::System)
            && self.strip_has(Template::System)
    }

    pub fn ws_gate(&self) -> WsGate {
        match self.visible_face {
            Some(f) => {
                if self
                    .strip()
                    .iter()
                    .any(|e| e.f == f && e.t == Template::Omarchy)
                {
                    WsGate::Visible
                } else {
                    WsGate::No
                }
            }
            None if self.strip_has(Template::Omarchy) => WsGate::Unknown,
            None => WsGate::No,
        }
    }

    /// `macro` for face `f` only when the strip has a Macropad there.
    pub fn macro_allowed(&self, f: FaceId) -> bool {
        self.strip()
            .iter()
            .any(|e| e.f == f.0 && e.t == Template::Macropad)
    }

    /// The full `agent` record of `slot`: the state entry with the title
    /// entry merged in, or the title alone for a title-only agent.
    pub fn agent_line(&self, slot: Slot) -> Option<HostLine> {
        let title = match self.entries.get(&Key::AgentTitle(slot)) {
            Some(HostLine::Agent { title, .. }) => title.clone(),
            _ => None,
        };
        match self.entries.get(&Key::AgentState(slot)) {
            Some(HostLine::Agent {
                slot,
                kind,
                name,
                project,
                state,
                since_s,
                ..
            }) => Some(HostLine::Agent {
                slot: *slot,
                kind: *kind,
                name: name.clone(),
                project: project.clone(),
                state: *state,
                title,
                since_s: *since_s,
                task: None,
                subs: None,
            }),
            _ => self.entries.get(&Key::AgentTitle(slot)).cloned(),
        }
    }

    /// The sync barrier (§6.5), in order: `agent` (full record, ascending
    /// slot) → `card` → `reply` → `focus` (PC-sourced only) → `macro` (faces
    /// in the strip) → `usage` → `system` (supported and enabled) → `ws` (Omarchy visible, or face unknown and
    /// in the strip) → (`boot` only) `faces` then every `set` → `synced`.
    pub fn barrier_plan(&self, boot: bool) -> Vec<BarrierItem> {
        let mut out = Vec::with_capacity(self.entries.len() + 1);
        out.extend(self.slots().into_iter().map(BarrierItem::Agent));
        for key in self.entries.keys() {
            let keep = match key {
                Key::AgentState(_) | Key::AgentTitle(_) => false,
                Key::Card(..) | Key::DocText(..) | Key::Reply(_) | Key::Focus | Key::Usage => true,
                Key::Macro(face) => self.macro_allowed(*face),
                Key::System => self.system_allowed(),
                Key::Ws => self.ws_gate() != WsGate::No,
                Key::Faces => boot,
                Key::Set(key) => (boot || *key == SetKey::Language) && self.set_allowed(*key),
                _ => false,
            };
            if keep {
                out.push(BarrierItem::Key(*key));
            }
        }
        if boot {
            // Every supported set key, even those the view has no entry for,
            // keeps the device's defaults explicit (P:95-97).
            let defaults = deck_model::Settings::default();
            for key in SetKey::ALL {
                if self.set_allowed(key) && !self.entries.contains_key(&Key::Set(key)) {
                    out.push(BarrierItem::SetDefault(key, defaults.get(key)));
                }
            }
        }
        out.push(BarrierItem::Synced);
        out
    }

    /// The barrier's lines as the view stands now (`barrier_plan` resolved).
    pub fn barrier(&self, boot: bool) -> Vec<HostLine> {
        self.barrier_plan(boot)
            .into_iter()
            .filter_map(|item| match item {
                BarrierItem::Agent(slot) => self.agent_line(slot),
                BarrierItem::Key(key) => self.entries.get(&key).cloned(),
                BarrierItem::SetDefault(key, value) => Some(HostLine::Set { key, value }),
                BarrierItem::Gone(slot) => Some(HostLine::AgentGone { slot: slot.0 }),
                BarrierItem::Synced => Some(HostLine::Synced),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_proto::{AgentState, CardKind, Kind};

    fn agent(slot: u8, title: Option<&str>) -> HostLine {
        HostLine::Agent {
            slot,
            kind: Some(Kind::Claude),
            name: Some("n".into()),
            project: None,
            state: Some(AgentState::Working),
            title: title.map(String::from),
            since_s: Some(1),
            task: None,
            subs: None,
        }
    }

    fn title(slot: u8, t: &str) -> HostLine {
        HostLine::Agent {
            slot,
            kind: None,
            name: None,
            project: None,
            state: None,
            title: Some(t.into()),
            since_s: None,
            task: None,
            subs: None,
        }
    }

    fn m(line: &HostLine) -> String {
        match line {
            HostLine::Agent { slot, title, .. } => format!(
                "agent{slot}{}",
                title
                    .as_deref()
                    .map(|t| format!(":{t}"))
                    .unwrap_or_default()
            ),
            HostLine::Card { slot, id, .. } => format!("card{slot}.{id}"),
            HostLine::Reply { slot, .. } => format!("reply{slot}"),
            HostLine::Focus { .. } => "focus".into(),
            HostLine::Macro { face, .. } => format!("macro{face}"),
            HostLine::Set { key, .. } => format!("set:{}", key.name()),
            other => other.m().to_string(),
        }
    }

    fn view() -> DeviceView {
        let mut v = DeviceView::new();
        v.set_faces(vec![
            FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "A".into(),
            },
            FaceEntry {
                f: 2,
                t: Template::Omarchy,
                n: "O".into(),
            },
            FaceEntry {
                f: 3,
                t: Template::Macropad,
                n: "M".into(),
            },
        ]);
        for op in [
            DeviceOp::Put(
                Key::Usage,
                HostLine::Usage {
                    period: "p".into(),
                    items: vec![],
                },
            ),
            DeviceOp::Put(
                Key::Reply(Slot(2)),
                HostLine::Reply {
                    slot: 2,
                    text: "r".into(),
                    rev: 1,
                },
            ),
            DeviceOp::Put(
                Key::Card(Slot(2), 7),
                HostLine::Card {
                    slot: 2,
                    id: 7,
                    kind: CardKind::Q,
                    text: "t".into(),
                    choices: vec![],
                    rec: None,
                },
            ),
            DeviceOp::Put(Key::AgentTitle(Slot(2)), title(2, "T2")),
            DeviceOp::Put(Key::AgentState(Slot(2)), agent(2, None)),
            DeviceOp::Put(Key::AgentState(Slot(1)), agent(1, None)),
            DeviceOp::Put(
                Key::Ws,
                HostLine::Ws {
                    active: 1,
                    count: 1,
                    sel: 0,
                    windows: vec![],
                },
            ),
            DeviceOp::Put(
                Key::Macro(FaceId(3)),
                HostLine::Macro {
                    face: 3,
                    src: None,
                    keys: vec![],
                },
            ),
            DeviceOp::Put(
                Key::Macro(FaceId(5)),
                HostLine::Macro {
                    face: 5,
                    src: None,
                    keys: vec![],
                },
            ),
            DeviceOp::Put(Key::Focus, HostLine::Focus { slot: 1, ack: None }),
            DeviceOp::Put(
                Key::Set(SetKey::Brightness),
                HostLine::Set {
                    key: SetKey::Brightness,
                    value: 2,
                },
            ),
            DeviceOp::Put(Key::Faces, HostLine::Faces { list: vec![] }),
            DeviceOp::Put(Key::Hello, HostLine::Hello { sid: 1, hm: None }),
            DeviceOp::Control(deck_model::Control::Hello),
            DeviceOp::Sync {
                boot: false,
                reason: deck_model::SyncReason::Start,
            },
        ] {
            v.apply(&op);
        }
        v
    }

    #[test]
    fn the_barrier_is_the_ordered_view_with_synced_last() {
        let v = view();
        assert!(
            v.get(&Key::Hello).is_none(),
            "controls never enter the view"
        );
        let names: Vec<String> = v.barrier(false).iter().map(m).collect();
        assert_eq!(
            names,
            [
                "agent1",
                "agent2:T2",
                "card2.7",
                "reply2",
                "focus",
                "macro3",
                "usage",
                "ws",
                "synced"
            ]
        );
        let names: Vec<String> = v.barrier(true).iter().map(m).collect();
        assert_eq!(
            &names[8..],
            [
                "faces",
                "set:brightness",
                "set:speaker",
                "set:mic",
                "set:announce",
                "set:chime",
                "set:autojump",
                "set:idlehome",
                "set:demo",
                "set:text",
                "synced"
            ]
        );
        assert_eq!(names.iter().filter(|n| n.starts_with("set:")).count(), 9);
        let plan = v.barrier_plan(true);
        assert_eq!(plan[0], BarrierItem::Agent(Slot(1)));
        assert_eq!(plan.last(), Some(&BarrierItem::Synced));
        assert!(plan.contains(&BarrierItem::SetDefault(SetKey::Demo, 1)));
        assert_eq!(v.slots(), vec![Slot(1), Slot(2)]);
    }

    #[test]
    fn arcade_defaults_and_saved_values_require_device_advertisement() {
        let mut v = view();
        let has_sound = |v: &DeviceView| v.barrier(true).iter().any(|line|
            matches!(line, HostLine::Set { key: SetKey::Arcadesound, .. }));
        assert!(!has_sound(&v));
        let arcade = vec![FaceEntry { f: 6, t: Template::Arcade, n: "Arcade".into() }];
        // A saved layout alone must not make an older firmware accept it.
        v.apply(&DeviceOp::Put(Key::Faces, HostLine::Faces { list: arcade.clone() }));
        assert!(!has_sound(&v));
        v.set_faces(arcade);
        assert!(v.barrier_plan(true).contains(&BarrierItem::SetDefault(SetKey::Arcadesound, 0)));
        v.apply(&DeviceOp::Put(Key::Set(SetKey::Arcadesound), HostLine::Set { key: SetKey::Arcadesound, value: 2 }));
        assert!(v.barrier(true).contains(&HostLine::Set { key: SetKey::Arcadesound, value: 2 }));
        // A retained setting must not leak when a legacy deck is connected.
        v.set_faces(vec![]);
        assert!(!has_sound(&v));
    }

    #[test]
    fn system_telemetry_requires_device_support_and_an_enabled_face() {
        let mut v = view();
        let status = HostLine::System(deck_proto::SystemStatus::EMPTY);
        v.apply(&DeviceOp::Put(Key::System, status.clone()));
        assert!(!v.barrier(true).contains(&status));
        let system = vec![FaceEntry { f: 7, t: Template::System, n: "System".into() }];
        v.apply(&DeviceOp::Put(Key::Faces, HostLine::Faces { list: system.clone() }));
        assert!(!v.barrier(true).contains(&status), "saved layout cannot enable legacy firmware");
        v.set_faces(system);
        assert!(v.barrier(true).contains(&status));
        assert!(v.barrier(false).contains(&status));
        v.apply(&DeviceOp::Put(Key::Faces, HostLine::Faces { list: vec![FaceEntry {
            f: 1, t: Template::Agents, n: "Agents".into(),
        }] }));
        assert!(!v.barrier(true).contains(&status), "removed face receives no updates");
        v.set_faces(vec![]);
        assert!(!v.system_allowed(), "another connected deck must advertise support again");
    }

    #[test]
    fn ws_follows_the_visible_face_and_macro_the_strip() {
        let mut v = view();
        assert_eq!(v.ws_gate(), WsGate::Unknown);
        v.set_visible_face(Some(1));
        assert_eq!(v.ws_gate(), WsGate::No);
        assert!(!v
            .barrier(false)
            .iter()
            .any(|l| matches!(l, HostLine::Ws { .. })));
        v.set_visible_face(Some(2));
        assert_eq!(v.ws_gate(), WsGate::Visible);
        assert!(v
            .barrier(false)
            .iter()
            .any(|l| matches!(l, HostLine::Ws { .. })));
        assert!(v.macro_allowed(FaceId(3)));
        assert!(!v.macro_allowed(FaceId(5)));
        v.set_visible_face(None);
        v.set_faces(vec![FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "A".into(),
        }]);
        assert_eq!(v.ws_gate(), WsGate::No);
        let b = v.barrier(false);
        assert!(!b.iter().any(|l| matches!(l, HostLine::Ws { .. })));
        assert!(!b.iter().any(|l| matches!(l, HostLine::Macro { .. })));
        // A strip the host pushes (boot:1 restore, PC edit) gates before the
        // device reports it; the device's own report is the fallback.
        v.apply(&DeviceOp::Put(
            Key::Faces,
            HostLine::Faces {
                list: vec![
                    FaceEntry {
                        f: 2,
                        t: Template::Omarchy,
                        n: "O".into(),
                    },
                    FaceEntry {
                        f: 5,
                        t: Template::Macropad,
                        n: "M".into(),
                    },
                ],
            },
        ));
        assert_eq!(v.ws_gate(), WsGate::Unknown);
        assert!(v.macro_allowed(FaceId(5)));
        assert!(!v.macro_allowed(FaceId(3)));
        let names: Vec<String> = v.barrier(false).iter().map(m).collect();
        assert!(names.contains(&"macro5".to_string()) && names.contains(&"ws".to_string()));
        v.apply(&DeviceOp::Remove(Key::Faces));
        assert_eq!(v.ws_gate(), WsGate::No);
        assert!(!v.macro_allowed(FaceId(5)));
    }

    #[test]
    fn remove_forgets_a_slot_and_a_title_only_agent_is_still_dumped() {
        let mut v = view();
        v.apply(&DeviceOp::Remove(Key::AgentState(Slot(2))));
        assert!(v.get(&Key::Card(Slot(2), 7)).is_none());
        assert!(v.get(&Key::Reply(Slot(2))).is_none());
        assert!(v.get(&Key::AgentTitle(Slot(2))).is_none());
        assert_eq!(
            v.barrier(false).iter().map(m).collect::<Vec<_>>()[..1],
            ["agent1"]
        );
        v.apply(&DeviceOp::Put(Key::AgentTitle(Slot(4)), title(4, "only")));
        let names: Vec<String> = v.barrier(false).iter().map(m).collect();
        assert_eq!(&names[..2], ["agent1", "agent4:only"]);
        assert_eq!(v.agent_line(Slot(4)), Some(title(4, "only")));
        assert_eq!(v.agent_line(Slot(1)), Some(agent(1, None)));
        assert_eq!(v.agent_line(Slot(6)), None);
        v.apply(&DeviceOp::Remove(Key::AgentGone(Slot(4))));
        assert!(v.get(&Key::AgentTitle(Slot(4))).is_none());
        assert_eq!(v.len(), 8);
        assert!(!v.is_empty());
        assert_eq!(v.entries().count(), 8);
    }
}
