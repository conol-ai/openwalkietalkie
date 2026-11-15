//! Grok Bot on a deck: the kind is sent only to a deck that draws it, and
//! the bots share one roster page.
use crate::testkit::{register, Sim};
use crate::*;

fn hello(kinds: Option<u8>) -> DeviceLine {
    DeviceLine::Hello {
        v: 1,
        fw: "0.14.0".into(),
        sid: 314159,
        boot: false,
        rx: 1024,
        agents: None,
        lang: None,
        lang_rev: None,
        kinds,
        faces: vec![],
    }
}

/// The kind the deck is told for `slot`, in a full projection of `snapshot`.
fn kind_sent(snapshot: &Snapshot, slot: Slot) -> Option<Kind> {
    project(snapshot, Ts(0)).into_iter().find_map(|op| match op {
        DeviceOp::Put(Key::AgentState(s), HostLine::Agent { kind, .. }) if s == slot => kind,
        _ => None,
    })
}

#[test]
fn a_grok_bot_is_sent_as_grok_only_to_a_deck_that_draws_it() {
    let mut sim = Sim::new();
    let principal = Principal::Adapter(AdapterKind::Grok);
    let mut bot = register("Research", Some("grokbot:research"));
    bot.kind = Kind::Grok;
    let res = sim
        .api(principal, SessionId(9), Request::AgentRegister(bot))
        .expect("register");
    let id = AgentId(res["agent"].as_str().unwrap().to_string());
    let agent = sim.store.snapshot().agents[&id].clone();
    // Its own roster page, and Grok in the API whatever the deck draws.
    assert_eq!(agent.project().as_deref(), Some(GROK_BOT_PROJECT));
    assert_eq!(agent.kind, Kind::Grok);
    let slot = agent.slot.expect("on the deck");

    // A deck whose hello predates `kinds` gets `other`; a deck that counts
    // grok gets it.
    let mut snapshot = sim.store.snapshot().clone();
    assert_eq!(snapshot.device.kinds, None);
    assert_eq!(kind_sent(&snapshot, slot), Some(Kind::Other));
    snapshot.device.kinds = Some(6);
    assert_eq!(kind_sent(&snapshot, slot), Some(Kind::Grok));

    // The store keeps what the linked deck said, and forgets it with the link.
    sim.apply(Command::Link(LinkEvent::Linked));
    sim.apply(Command::Device(hello(Some(6))));
    assert_eq!(sim.store.snapshot().device.kinds, Some(6));
    sim.apply(Command::Link(LinkEvent::Down));
    assert_eq!(sim.store.snapshot().device.kinds, None);
    sim.apply(Command::Device(hello(None)));
    assert_eq!(sim.store.snapshot().device.kinds, None);
}

#[test]
fn a_grok_session_that_runs_somewhere_keeps_its_own_project() {
    let mut sim = Sim::new();
    let mut session = register("cli", Some("grok:abc"));
    session.kind = Kind::Grok;
    session.cwd = Some("/Users/owner/projects/openmicrodial".into());
    let res = sim
        .api(Principal::Uid(501), SessionId(3), Request::AgentRegister(session))
        .expect("register");
    let id = AgentId(res["agent"].as_str().unwrap().to_string());
    assert_eq!(
        sim.store.snapshot().agents[&id].project().as_deref(),
        Some("openmicrodial")
    );
}
