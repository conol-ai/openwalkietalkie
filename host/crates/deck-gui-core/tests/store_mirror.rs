//! The mirror against the real store. A `deck_model::Store` is driven
//! through a deck's morning (linked, its own reports, drops, a link that
//! never answers, the keys set from the app and edited on the deck) and
//! every event it emits is folded into a `HostState` the way the app folds
//! what the socket delivers. After each step the mirror must hold what the
//! store's own snapshot holds.
//!
//! The reducer suite writes its events by hand from reading the store, and
//! the hand and the reading can be wrong together. Here the events are the
//! store's own, so a fold that misreads one, or a change the store makes
//! without saying so, is a difference this file names.
use deck_gui_core::{ApiEvent, AppEvent, HostState, UiAction};
use deck_model::{
    Binding, Command, DeviceLine, DeviceReport, DeviceState, Event, EventKind, FaceEntry,
    LinkEvent, MacroKey, Principal, Request, SessionId, Snapshot, Store, StoreConfig, Template,
    Transport, Ts, LINE_MAX_U16,
};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// 2026-11-11T09:00:00.000Z, when the service started.
const NINE: Ts = Ts(1_794_387_600_000);
const FW: &str = "0.13.0-openwalkietalkie";
const SID: u32 = 314_159;

fn strip() -> Vec<FaceEntry> {
    vec![
        FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "Agents".into(),
        },
        FaceEntry {
            f: 3,
            t: Template::Macropad,
            n: "Macropad".into(),
        },
        FaceEntry {
            f: 5,
            t: Template::Settings,
            n: "Settings".into(),
        },
    ]
}

/// The store, the app's mirror of it, and the clock they share.
struct Morning {
    store: Store,
    state: HostState,
    started: Instant,
    now: Instant,
    /// Every event the store emitted, in order.
    events: Vec<Event>,
}

impl Morning {
    fn new() -> Morning {
        let started = Instant::now();
        let store = Store::new(StoreConfig::new(started, NINE, SID));
        let mut state = HostState::detached();
        let hello = AppEvent::Api(ApiEvent::Connected {
            snapshot: store.snapshot().clone(),
        });
        assert!(state.handle_event(hello).is_empty());
        Morning {
            store,
            state,
            started,
            now: started,
            events: Vec::new(),
        }
    }

    /// The clock at `minutes` past nine.
    fn at(&mut self, minutes: u64) {
        self.now = self.started + Duration::from_secs(60 * minutes);
    }

    /// What the hid thread reports.
    fn apply(&mut self, command: Command) {
        let (events, _) = self.store.apply(command, self.now);
        self.fold(events);
    }

    /// What the app sends, parsed the way the service parses it: its
    /// `{method, params}` into the store's `Request` (`api::dispatch`).
    fn send(&mut self, action: &UiAction) {
        let (method, params) = action.request();
        let request = serde_json::from_value::<Request>(
            serde_json::json!({"method": method, "params": params}),
        )
        .unwrap_or_else(|e| panic!("{method}: {e}"));
        let (result, events, _) =
            self.store
                .call(Principal::Uid(501), SessionId(1), request, self.now);
        result.unwrap_or_else(|e| panic!("{method}: {e}"));
        self.fold(events);
    }

    /// The events as the socket delivers them, then the check that the
    /// mirror agrees with the store.
    fn fold(&mut self, events: Vec<Event>) {
        for event in events {
            let effects = self
                .state
                .handle_event(AppEvent::Api(ApiEvent::Event(event.clone())));
            assert!(effects.is_empty(), "{:?} asked for {effects:?}", event.kind);
            self.events.push(event);
        }
        let (store, mirror) = (self.store.snapshot(), self.state.mirror());
        assert_eq!(mirror.rev, store.rev, "an event was not folded");
        assert_eq!(self.state.gaps(), 0);
        assert_eq!(mirror.device, as_events_tell_it(store, mirror));
        assert_eq!(mirror.macros, store.macros);
        assert_eq!(mirror.faces, store.faces);
    }

    /// A deck opened on `transport` that says hello and goes live, in the
    /// order the hid thread reports them.
    fn link_up(&mut self, transport: Transport) {
        self.apply(Command::Link(LinkEvent::Linked));
        self.apply(Command::DeviceReport(DeviceReport::Transport(Some(
            transport,
        ))));
        self.apply(Command::Device(DeviceLine::Hello {
            v: 1,
            fw: FW.into(),
            sid: SID,
            boot: false,
            rx: LINE_MAX_U16,
            agents: None,
            lang: None,
            lang_rev: None,
            faces: strip(),
        }));
        self.apply(Command::Link(LinkEvent::Live { sid: SID }));
    }

    /// The link lost, the way the hid thread says it: why, then `Down`.
    fn lose(&mut self, why: &str) {
        self.apply(Command::DeviceReport(DeviceReport::Dropped {
            why: why.into(),
        }));
        self.apply(Command::Link(LinkEvent::Down));
    }

    /// The data of the last `device.status` the store emitted.
    fn last_status(&self) -> &serde_json::Value {
        &self
            .events
            .iter()
            .rev()
            .find(|event| event.kind == EventKind::DeviceStatus)
            .expect("a device.status")
            .data
    }
}

/// The store's device as its events tell it. Five fields travel in no event
/// (`language_supported`, `pace_ms`, `outbox_depth`, `err_counts`,
/// `sessions`): a mirror learns them from a whole snapshot only, so they
/// are taken as the mirror has them, and every other field must agree.
fn as_events_tell_it(store: &Snapshot, mirror: &Snapshot) -> DeviceState {
    let known = &mirror.device;
    DeviceState {
        language_supported: known.language_supported,
        pace_ms: known.pace_ms,
        outbox_depth: known.outbox_depth,
        err_counts: known.err_counts,
        sessions: known.sessions,
        ..store.device.clone()
    }
}

#[test]
fn the_mirror_follows_the_real_store_through_a_link_its_reports_the_drops_and_the_keys() {
    let mut day = Morning::new();
    day.link_up(Transport::Usb);
    // What the deck says about itself, a charge it cannot read (0) among it.
    day.at(1);
    for report in [
        DeviceReport::Power {
            battery: Some(85),
            charging: Some(false),
        },
        DeviceReport::Board(Some("Rev B".into())),
        DeviceReport::Power {
            battery: Some(0),
            charging: Some(true),
        },
        DeviceReport::Transport(Some(Transport::Bluetooth)),
    ] {
        day.apply(Command::DeviceReport(report));
    }
    let device = &day.state.mirror().device;
    assert_eq!((device.battery, device.charging), (None, Some(true)));
    assert_eq!(device.board.as_deref(), Some("Rev B"));
    assert_eq!(
        device.last.as_ref().and_then(|last| last.transport),
        Some(Transport::Bluetooth)
    );

    // The keys, set from the app…
    day.at(2);
    let build = MacroKey {
        label: "build".into(),
        binding: Some(Binding::Left),
        keys: Some("cmd+b".into()),
    };
    day.send(&UiAction::DeviceMacroSet {
        f: 3,
        layout: "app".into(),
        src: None,
        keys: vec![build.clone()],
    });
    // …and one more added on the deck's own editor, a slot further on.
    day.apply(Command::Device(DeviceLine::MacroKey {
        f: 3,
        i: 2,
        label: Some("lock".into()),
        binding: Some(Binding::Enter),
        keys: Some("ctrl+cmd+q".into()),
    }));
    let keys = &day.state.mirror().macros[&3].keys;
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0], build);
    assert!(keys[1].label.is_empty(), "the empty slot keeps `lock` third");

    // A drop: the stall, then the link down, which clears what the deck
    // said and stamps when it was lost.
    day.at(36);
    day.lose("write_stall");
    let device = &day.state.mirror().device;
    assert_eq!(device.fw, None);
    assert_eq!(
        (device.transport, device.battery, device.board.as_deref()),
        (None, None, None)
    );
    assert_eq!(device.drops.len(), 1);
    assert_eq!(
        device.last.as_ref().map(|last| last.at),
        Some(NINE.plus(Duration::from_secs(36 * 60)))
    );
    // A late report about the deck that is gone changes nothing anywhere.
    day.apply(Command::DeviceReport(DeviceReport::Power {
        battery: Some(50),
        charging: None,
    }));

    // A link that opens and never answers, then one that does, then one
    // closed on purpose for an install, which is not a drop.
    day.at(40);
    day.apply(Command::Link(LinkEvent::Linked));
    day.lose("misses");
    day.at(41);
    day.link_up(Transport::Bluetooth);
    day.at(50);
    day.lose("firmware_update");
    let whys: Vec<&str> = day
        .state
        .mirror()
        .device
        .drops
        .iter()
        .map(|drop| drop.why.as_str())
        .collect();
    assert_eq!(whys, vec!["write_stall", "misses"]);

    // The morning took every kind the device fields are told by.
    let kinds: BTreeSet<&str> = day.events.iter().map(|e| e.kind.as_str()).collect();
    for kind in [
        "device.link",
        "device.session",
        "device.faces",
        "device.status",
        "device.macros",
        "device.macro.edit",
    ] {
        assert!(kinds.contains(kind), "the morning never emitted {kind}");
    }
}

/// `tests/fixtures/daemon/device_status.json` is not a hand's idea of the
/// event: it is what this store emits for a deck linked over Bluetooth at
/// 09:41 after two drops, byte for byte as JSON. The service's own tests
/// check their `device.status` against its keys and types.
#[test]
fn the_device_status_fixture_is_exactly_what_the_store_emits() {
    let mut day = Morning::new();
    day.link_up(Transport::Usb);
    day.at(12);
    day.lose("read");
    day.at(13);
    day.link_up(Transport::Bluetooth);
    day.at(36);
    day.lose("write_stall");
    day.at(41);
    day.link_up(Transport::Bluetooth);
    day.now += Duration::from_secs(2);
    day.apply(Command::DeviceReport(DeviceReport::Power {
        battery: Some(85),
        charging: Some(false),
    }));
    day.apply(Command::DeviceReport(DeviceReport::Board(Some(
        "Rev B".into(),
    ))));
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/daemon/device_status.json"))
            .expect("the fixture is json");
    assert_eq!(day.last_status(), &fixture);
}
