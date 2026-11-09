use crate::*;
use crate::testkit::{Sim, UID};

fn hello(language: Option<(u8, u32)>, boot: bool) -> DeviceLine {
    DeviceLine::Hello { v: 1, fw: "sync".into(), sid: 314159, boot, rx: 1024,
        agents: None, lang: language.map(|v| v.0), lang_rev: language.map(|v| v.1), faces: vec![] }
}

fn set(sim: &mut Sim, value: u16) {
    sim.api(UID, SessionId(1), Request::DeviceSet(DeviceSet { key: SetKey::Language, value })).unwrap();
}

fn pushed(sim: &Sim) -> Vec<(u8, u32)> {
    sim.effects.iter().filter_map(|effect| match effect {
        Effect::Device(DeviceOp::Put(Key::Set(SetKey::Language), HostLine::Language { value, revision })) => Some((*value, *revision)),
        _ => None,
    }).collect()
}

#[test]
fn app_language_changes_persist_and_publish_while_the_deck_is_absent() {
    let mut sim = Sim::new();
    set(&mut sim, 3);
    assert_eq!((sim.store.snapshot().settings.language, sim.store.snapshot().settings.language_revision), (3, 1));
    assert!(sim.effects.contains(&Effect::Persist(StatePatch::Language { value: 3, revision: 1 })));
    assert_eq!(pushed(&sim), [(3, 1)]);
    assert_eq!(sim.events.last().unwrap().kind, EventKind::DeviceSet);
    set(&mut sim, 0);
    assert_eq!((sim.store.snapshot().settings.language, sim.store.snapshot().settings.language_revision), (0, 2));
    assert_eq!(pushed(&sim), [(0, 2)]);
    assert!(sim.api(UID, SessionId(1), Request::DeviceSet(DeviceSet { key: SetKey::Language, value: 4 })).is_err());
    assert_eq!(sim.store.snapshot().settings.language_revision, 2);
}

#[test]
fn a_newer_deck_preference_is_adopted_and_acknowledged_without_repeated_persistence() {
    let mut sim = Sim::new();
    sim.apply(Command::Device(hello(Some((2, 7)), false)));
    assert!(sim.store.snapshot().device.language_supported);
    assert_eq!((sim.store.snapshot().settings.language, sim.store.snapshot().settings.language_revision), (2, 7));
    assert!(sim.effects.contains(&Effect::Persist(StatePatch::Language { value: 2, revision: 7 })));
    assert_eq!(pushed(&sim), [(2, 7)]);
    sim.apply(Command::Device(hello(Some((2, 7)), true)));
    assert_eq!(pushed(&sim), [(2, 7)], "each hello negotiates support even when values already match");
    assert!(!sim.effects.iter().any(|effect| matches!(effect, Effect::Persist(StatePatch::Language { .. }))));
}

#[test]
fn app_wins_equal_revision_conflicts_and_delayed_deck_events_cannot_undo_it() {
    let mut sim = Sim::new();
    set(&mut sim, 3);
    sim.apply(Command::Device(hello(Some((1, 1)), false)));
    assert_eq!(sim.store.snapshot().settings.language, 3);
    assert_eq!(pushed(&sim), [(3, 1)]);
    set(&mut sim, 0);
    sim.apply(Command::Device(DeviceLine::Language { value: 1, revision: 1 }));
    assert_eq!(sim.store.snapshot().settings.language, 0);
    assert_eq!(pushed(&sim), [(0, 2)]);
    sim.apply(Command::Device(DeviceLine::Language { value: 2, revision: 3 }));
    assert_eq!(sim.store.snapshot().settings.language, 2);
    assert_eq!(pushed(&sim), [(2, 3)]);
}

#[test]
fn legacy_and_partial_capability_hellos_do_not_change_or_receive_language() {
    let mut sim = Sim::new();
    set(&mut sim, 3);
    sim.apply(Command::Device(hello(None, true)));
    assert!(!sim.store.snapshot().device.language_supported);
    assert_eq!(sim.store.snapshot().settings.language, 3);
    assert!(pushed(&sim).is_empty());
    sim.apply(Command::Device(DeviceLine::Language { value: 1, revision: 9 }));
    assert!(pushed(&sim).is_empty());
    assert_eq!(sim.store.snapshot().settings.language, 3);
    let mut partial = hello(None, false);
    if let DeviceLine::Hello { lang, .. } = &mut partial { *lang = Some(1); }
    sim.apply(Command::Device(partial));
    assert!(!sim.store.snapshot().device.language_supported);
    assert!(pushed(&sim).is_empty());
}

#[test]
fn reconnects_reconcile_offline_choices_in_either_direction_and_migrate_legacy_preferences() {
    let mut sim = Sim::with(|config| {
        config.persisted.settings.language = 2;
        config.persisted.settings.language_revision = 0;
    });
    assert_eq!(sim.store.snapshot().settings.language_revision, 1);
    sim.apply(Command::Device(hello(Some((0, 0)), true)));
    assert_eq!(pushed(&sim), [(2, 1)]);
    set(&mut sim, 3);
    sim.apply(Command::Device(hello(Some((2, 1)), false)));
    assert_eq!(pushed(&sim), [(3, 2)]);
    sim.apply(Command::Device(hello(Some((1, 3)), false)));
    assert_eq!((sim.store.snapshot().settings.language, sim.store.snapshot().settings.language_revision), (1, 3));
}
