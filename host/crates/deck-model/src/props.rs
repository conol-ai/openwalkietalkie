//! The §14 property tests that belong to the model (the outbox's own —
//! unsent card + clear, title bucket, line sizes — live with the outbox).
use crate::testkit::{card_set, claude_adapter, register, Sim, UID};
use crate::{
    AgentId, AgentState, AgentUpdate, AnswerCap, CardKind, Command, Control, Deliver, DeviceLine,
    DeviceOp, Effect, FocusReport, FocusSource, Group, HoldId, HostLine, Key, LinkEvent, OrderCap,
    Origin, OriginKind, Request, SessionId, StatePatch, StoreConfig,
};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::time::Duration;

fn update(id: &AgentId, state: AgentState) -> Request {
    Request::AgentUpdate(AgentUpdate {
        agent: id.clone(),
        state: Some(state),
        title: None,
        title_source: None,
        since: None,
        name: None,
        meta: None, task: None, subagents: None,
    })
}

fn states() -> impl Strategy<Value = AgentState> {
    prop_oneof![
        Just(AgentState::Idle),
        Just(AgentState::Working),
        Just(AgentState::Question),
        Just(AgentState::Ready),
        Just(AgentState::Error),
    ]
}

/// One step of a random session: what a client or the device might do next.
#[derive(Clone, Debug)]
enum Step {
    Register(u8),
    State(u8, AgentState),
    Card(u8, bool),
    Answer(u8, u8),
    Snooze(u8),
    Release(u8),
    Channel(u8, u16),
    Focus(u8),
    Tick(u64),
    Link(bool),
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    let step = prop_oneof![
        (0..12u8).prop_map(Step::Register),
        (0..12u8, states()).prop_map(|(a, s)| Step::State(a, s)),
        (0..12u8, any::<bool>()).prop_map(|(a, r)| Step::Card(a, r)),
        (0..12u8, 0..3u8).prop_map(|(a, c)| Step::Answer(a, c)),
        (0..12u8).prop_map(Step::Snooze),
        (0..12u8).prop_map(Step::Release),
        (1..=9u8, any::<u16>()).prop_map(|(s, q)| Step::Channel(s, q)),
        (0..12u8).prop_map(Step::Focus),
        (0..2000u64).prop_map(Step::Tick),
        any::<bool>().prop_map(Step::Link),
    ];
    proptest::collection::vec(step, 1..40)
}

/// Drive a store through `steps`, collecting every effect.
fn run(steps: &[Step]) -> (Sim, Vec<Effect>) {
    let mut sim = Sim::new();
    let mut ids: BTreeMap<u8, AgentId> = BTreeMap::new();
    let mut all = Vec::new();
    for step in steps {
        match step {
            Step::Register(n) => {
                if !ids.contains_key(n) {
                    let id =
                        sim.register(UID, SessionId(1), &format!("a{n}"), Some(&format!("k:{n}")));
                    ids.insert(*n, id);
                }
            }
            Step::State(n, state) => {
                if let Some(id) = ids.get(n) {
                    let _ = sim.api(UID, SessionId(1), update(id, *state));
                }
            }
            Step::Card(n, ready) => {
                if let Some(id) = ids.get(n) {
                    let kind = if *ready { CardKind::R } else { CardKind::Q };
                    let _ = sim.api(
                        UID,
                        SessionId(1),
                        Request::CardSet(card_set(id, kind, Deliver::Poll, &["a", "b", "c"])),
                    );
                }
            }
            Step::Answer(n, choice) => {
                if let Some(slot) = ids.get(n).and_then(|id| sim.slot_of(id)) {
                    let card = sim.store.snapshot().agents[&ids[n]]
                        .card
                        .as_ref()
                        .map(|c| c.id.0)
                        .unwrap_or(1);
                    sim.apply(Command::Device(DeviceLine::Answer {
                        slot: slot.0,
                        card,
                        choice: *choice,
                    }));
                }
            }
            Step::Snooze(n) => {
                if let Some(slot) = ids.get(n).and_then(|id| sim.slot_of(id)) {
                    sim.apply(Command::Device(DeviceLine::Snooze {
                        slot: slot.0,
                        card: 1,
                    }));
                }
            }
            Step::Release(n) => {
                if let Some(id) = ids.remove(n) {
                    let _ = sim.api(
                        UID,
                        SessionId(1),
                        Request::AgentRelease {
                            agent: id,
                            reason: None,
                        },
                    );
                }
            }
            Step::Channel(slot, seq) => sim.apply(Command::Device(DeviceLine::Channel {
                slot: *slot,
                seq: *seq,
            })),
            Step::Focus(n) => {
                if let Some(id) = ids.get(n) {
                    let _ = sim.api(UID, SessionId(2), Request::OpFocus { agent: id.clone() });
                }
            }
            Step::Tick(ms) => sim.advance(Duration::from_millis(*ms)),
            Step::Link(up) => sim.apply(Command::Link(if *up {
                LinkEvent::Live { sid: 1 }
            } else {
                LinkEvent::Down
            })),
        }
        all.extend(sim.effects.iter().cloned());
    }
    (sim, all)
}

proptest! {
    #[test]
    fn a_card_is_never_put_before_its_agent_in_project(steps in steps()) {
        let (sim, _) = run(&steps);
        let ops = sim.store.project();
        let mut seen_agent = Vec::new();
        for op in &ops {
            match op {
                DeviceOp::Put(Key::AgentState(slot), _) => seen_agent.push(*slot),
                DeviceOp::Put(Key::Card(slot, _), _) => {
                    prop_assert!(seen_agent.contains(slot), "card before agent in {ops:?}");
                }
                DeviceOp::Put(_, _) => {}
                other => prop_assert!(false, "project() is Put only, got {other:?}"),
            }
        }
        let keys: Vec<Key> = ops.iter().filter_map(|op| match op {
            DeviceOp::Put(k, _) => Some(*k),
            _ => None,
        }).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        prop_assert_eq!(&keys, &sorted, "barrier order");
        prop_assert!(keys.iter().all(|k| k.in_barrier()));
    }

    #[test]
    fn synced_is_never_a_lane_entry_and_puts_are_barrier_keys(steps in steps()) {
        let (_, effects) = run(&steps);
        for e in &effects {
            match e {
                Effect::Device(DeviceOp::Put(key, line)) => {
                    let control = matches!(line, HostLine::Synced | HostLine::Hello { .. });
                    prop_assert!(!control, "a control line as desired state: {line:?}");
                    let lane_key = key.in_barrier() || matches!(key, Key::AgentGone(_));
                    prop_assert!(lane_key, "{key:?}");
                }
                Effect::Device(DeviceOp::Control(c)) => {
                    prop_assert!(!matches!(c, Control::Synced | Control::Hello), "{c:?}");
                }
                Effect::Device(DeviceOp::Sync { .. }) => prop_assert!(false, "no barrier from a session"),
                _ => {}
            }
        }
    }

    #[test]
    fn events_bump_rev_by_exactly_one_each(steps in steps()) {
        let mut sim = Sim::new();
        let mut last = 0u64;
        let mut ids: Vec<AgentId> = Vec::new();
        for step in &steps {
            sim.events.clear();
            match step {
                Step::Register(n) => ids.push(sim.register(UID, SessionId(1), &format!("a{n}"), None)),
                Step::State(n, s) => {
                    if let Some(id) = ids.get(usize::from(*n)) {
                        let _ = sim.api(UID, SessionId(1), update(id, *s));
                    }
                }
                Step::Tick(ms) => sim.advance(Duration::from_millis(*ms)),
                _ => sim.apply(Command::Device(DeviceLine::Up)),
            }
            for e in &sim.events {
                prop_assert_eq!(e.rev, last + 1);
                last = e.rev;
            }
            prop_assert_eq!(sim.store.snapshot().rev, last);
        }
    }

    #[test]
    fn no_card_id_repeats_on_a_slot_across_restarts(
        start in prop_oneof![Just(0u16), Just(u16::MAX - 3), any::<u16>()],
        batches in proptest::collection::vec(1..6usize, 1..5),
    ) {
        let mut persisted: BTreeMap<u8, u16> = BTreeMap::new();
        persisted.insert(1, start);
        let mut seen: Vec<u16> = Vec::new();
        let mut last = start;
        for batch in batches {
            // A restart: the store is rebuilt from what the last run persisted.
            let mut sim = Sim::with(|cfg: &mut StoreConfig| cfg.persisted.card_ids = persisted.clone());
            let id = sim.register(UID, SessionId(1), "bot", None);
            prop_assert_eq!(sim.slot_of(&id), Some(crate::Slot(1)));
            for _ in 0..batch {
                let cid = sim.set_card(&id, CardKind::Q, Deliver::Poll, vec!["a"]);
                prop_assert_ne!(cid.0, 0);
                prop_assert_eq!(cid, crate::CardId(last).next());
                prop_assert!(!seen.contains(&cid.0), "{} repeated", cid.0);
                seen.push(cid.0);
                last = cid.0;
                for e in &sim.effects {
                    if let Effect::Persist(StatePatch::CardId { slot, last }) = e {
                        persisted.insert(slot.0, last.0);
                    }
                }
            }
            prop_assert_eq!(persisted[&1], last, "persisted before the next run");
        }
    }

    #[test]
    fn every_intermediate_group_answer_is_cleared_within_4s(
        count in 2..=4u8,
        delays in proptest::collection::vec(0..20_000u64, 4),
        choices in proptest::collection::vec(0..2u8, 4),
    ) {
        let mut sim = Sim::new();
        let mut r = register("cohdl-api", Some("claude:1"));
        r.caps.answer = AnswerCap::Hold;
        r.caps.order = OrderCap::Inject;
        let res = sim
            .api(claude_adapter(), SessionId(100), Request::AgentRegister(r))
            .unwrap();
        let id = AgentId(res["agent"].as_str().unwrap().into());
        for index in 0..count {
            let mut c = card_set(&id, CardKind::Q, Deliver::Hold, &["Allow", "Deny"]);
            c.origin = Origin {
                kind: OriginKind::Question,
                tool: None,
                request_id: None,
                group: Some(Group { index, count }),
            };
            let cid = sim.hold(c, HoldId(1)).unwrap();
            sim.drain();
            sim.advance(Duration::from_millis(delays[usize::from(index)]));
            let t_answer = sim.t;
            sim.apply(Command::Device(DeviceLine::Answer {
                slot: 1,
                card: cid.0,
                choice: choices[usize::from(index)],
            }));
            let cleared = |sim: &Sim| sim.effects.iter().any(|e| matches!(
                e,
                Effect::Device(DeviceOp::Control(Control::CardClear(crate::Slot(1), n))) if *n == cid.0
            ));
            if index + 1 < count {
                prop_assert!(cleared(&sim), "intermediate card {index} not cleared at once");
                prop_assert!(sim.t.duration_since(t_answer) <= Duration::from_secs(4));
                prop_assert!(sim.store.snapshot().agents[&id].card.is_none());
            } else {
                prop_assert!(!cleared(&sim), "the last card waits for the hook response");
                sim.apply(Command::HoldWritten {
                    agent: id.clone(),
                    card: cid,
                });
                prop_assert!(cleared(&sim));
            }
        }
    }

    #[test]
    fn focus_is_in_the_barrier_only_after_a_pc_side_focus(
        moves in proptest::collection::vec((0..4u8, 1..=3u8), 1..20),
    ) {
        let mut sim = Sim::new();
        let ids: Vec<AgentId> = (0..3)
            .map(|i| sim.register(UID, SessionId(1), &format!("a{i}"), None))
            .collect();
        for (kind, n) in moves {
            let id = &ids[usize::from(n) - 1];
            let pc_focus = match kind {
                0 => {
                    sim.apply(Command::Device(DeviceLine::Channel { slot: n, seq: 1 }));
                    false
                }
                1 => {
                    sim.api(UID, SessionId(2), Request::OpFocus { agent: id.clone() }).unwrap();
                    true
                }
                2 => {
                    sim.api(
                        UID,
                        SessionId(2),
                        Request::FocusReport(FocusReport {
                            agent: Some(id.clone()),
                            source: FocusSource::Desktop,
                        }),
                    )
                    .unwrap();
                    true
                }
                _ => {
                    sim.api(
                        UID,
                        SessionId(2),
                        Request::FocusReport(FocusReport {
                            agent: None,
                            source: FocusSource::Api,
                        }),
                    )
                    .unwrap();
                    false
                }
            };
            let in_barrier = sim
                .store
                .project()
                .iter()
                .any(|op| matches!(op, DeviceOp::Put(Key::Focus, _)));
            prop_assert_eq!(in_barrier, pc_focus);
        }
    }
}
