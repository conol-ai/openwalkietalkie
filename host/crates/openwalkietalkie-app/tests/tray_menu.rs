//! What the **tray** shows and what its six items do (§8.2's tray row),
//! driven through the real reducer.
//!
//! The status item itself needs a menu bar and is proved by running the app;
//! everything it *says* is a pure function of the mirror and is proved here.
//! The two halves meet at `deck_gui_core::tray::Summary`, which is the only
//! thing either of them reads — so a dot in the menu bar and a roster in the
//! window cannot disagree.
use deck_gui_core::tray::{Summary, TrayCommand, QUIT_TOOLTIP};
use deck_gui_core::{ApiEvent, AppEvent, ConfigValue, HostState, UiAction};
use deck_model::{
    AdapterKind, Agent, AgentId, AgentState, AnswerCap, Caps, Event, EventKind, Kind, OrderCap,
    Owner, Principal, SessionId, SessionInfo, SetKey, Snapshot, Source, TitleSource, Tone, Ts,
};
use openwalkietalkie_app::present;
use serde_json::{json, Value};

const TS: Ts = Ts::from_millis(1_789_760_523_120);

fn event(rev: u64, kind: EventKind, data: Value) -> AppEvent {
    AppEvent::Api(ApiEvent::Event(Event {
        rev,
        ts: TS,
        kind,
        principal: Principal::Uid(501),
        data,
    }))
}

fn agent(id: &str, state: AgentState) -> Agent {
    Agent {
        id: AgentId(id.into()),
        slot: None,
        kind: Kind::Claude,
        name: id.into(),
        state,
        title: Some("Compiling openmicro".into()),
        title_source: TitleSource::Prompt,
        since: TS,
        source: Source::Hook,
        owner: Owner {
            principal: Principal::Adapter(AdapterKind::Claude),
            session: SessionId(7),
            lease_expires: None,
        },
        session: SessionInfo::default(),
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: true,
        },
        companion: None,
        card: None,
        reply: None,
        messages: Vec::new(),
        workflow_runs: Vec::new(), workflow: None,
        chat_rev: 0,
        reply_rev: 0,
        voice: None,
        pinned: false,
        meta: Value::Null,
        redacted: false,
    }
}

fn live(agents: &[(&str, AgentState)]) -> HostState {
    let mut snapshot = Snapshot {
        rev: 10,
        ..Snapshot::default()
    };
    snapshot.service.port = 47_201;
    for (id, state) in agents {
        snapshot
            .agents
            .insert(AgentId((*id).into()), agent(id, *state));
    }
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    state
}

#[test]
fn the_dot_follows_the_roster_as_the_daemons_own_events_move_it() {
    let mut state = live(&[("a", AgentState::Idle)]);
    assert_eq!(Summary::of(state.mirror(), true, None).dot, Tone::Gray);
    // An agent starts working…
    state.handle_event(event(
        11,
        EventKind::AgentChanged,
        json!({"agent": "a", "fields": {"state": "working"}}),
    ));
    assert_eq!(Summary::of(state.mirror(), true, None).dot, Tone::Blue);
    // …asks a question…
    state.handle_event(event(
        12,
        EventKind::AgentChanged,
        json!({"agent": "a", "fields": {"state": "question"}}),
    ));
    let summary = Summary::of(state.mirror(), true, None);
    assert_eq!(summary.dot, Tone::Red);
    assert_eq!(summary.headline, "1 question");
    // …and is answered.
    state.handle_event(event(
        13,
        EventKind::AgentChanged,
        json!({"agent": "a", "fields": {"state": "ready"}}),
    ));
    assert_eq!(Summary::of(state.mirror(), true, None).dot, Tone::Green);
    // The window draws the same tones from the same states, so the menu bar
    // and the roster cannot say two different things about one agent.
    assert_eq!(
        present::state_tone(AgentState::Question, false),
        Summary::of(live(&[("q", AgentState::Question)]).mirror(), true, None).dot
    );
}

#[test]
fn a_degraded_service_takes_the_dot_amber_and_names_the_squatter_once() {
    let mut state = live(&[("a", AgentState::Ready)]);
    assert_eq!(Summary::of(state.mirror(), true, None).dot, Tone::Green);
    // §4.1: a stranger answers on the port. Every held Claude hook is
    // POSTing prompts and the `hooks` token to it while this is true, which
    // is why it outranks anything on the roster.
    state.handle_event(event(
        11,
        EventKind::ServiceDegraded,
        json!({
            "reason": "hooks_hijack_risk",
            "port": 47201,
            "listener": {"pid": 812, "command": "node /usr/local/bin/other"}
        }),
    ));
    let summary = Summary::of(state.mirror(), true, Some(true));
    assert_eq!(summary.dot, Tone::Amber);
    assert!(summary.wants_attention());
    let detail = summary.detail.clone().expect("a detail");
    assert!(detail.contains("node /usr/local/bin/other"), "{detail}");
    assert!(detail.contains("47201"), "{detail}");
    assert!(summary.tooltip().contains("OpenWalkieTalkie"));
    // The daemon re-probes every 30 s and re-emits the same reason. The
    // window raises its alert on a *change* of reason, so the second one is
    // the same summary and nothing new to announce.
    state.handle_event(event(
        12,
        EventKind::ServiceDegraded,
        json!({"reason": "hooks_hijack_risk", "port": 47201}),
    ));
    assert_eq!(
        Summary::of(state.mirror(), true, Some(true)).degraded,
        summary.degraded
    );
    // Recovered.
    state.handle_event(event(
        13,
        EventKind::ServiceDegraded,
        json!({"reason": null}),
    ));
    let recovered = Summary::of(state.mirror(), true, Some(true));
    assert_eq!(recovered.dot, Tone::Green);
    assert!(!recovered.wants_attention());
    assert_eq!(recovered.detail, None);
}

#[test]
fn mute_announcements_writes_the_one_device_key_and_reads_its_tick_from_the_mirror() {
    let mut state = live(&[("a", AgentState::Ready)]);
    // §8.2: `device.set announce`. The deck speaks by default.
    let speaking = Summary::of(state.mirror(), true, None);
    assert!(!speaking.muted);
    let action = speaking.mute_action();
    assert_eq!(
        action,
        UiAction::DeviceSet {
            key: SetKey::Announce,
            value: 0
        }
    );
    let (method, params) = action.request();
    assert_eq!(method, "device.set");
    assert_eq!(params["k"], "announce");
    assert_eq!(params["v"], 0);
    // The click does not set the tick: the event the daemon sends back does.
    state.handle_event(event(
        11,
        EventKind::DeviceSet,
        json!({"k": "announce", "v": 0}),
    ));
    assert_eq!(state.mirror().settings.announce, 0);
    let muted = Summary::of(state.mirror(), true, None);
    assert!(muted.muted, "the tick follows the mirror");
    assert_eq!(
        muted.mute_action(),
        UiAction::DeviceSet {
            key: SetKey::Announce,
            value: 1
        },
        "and the item now offers the other direction"
    );
}

#[test]
fn pause_hooks_is_a_config_write_and_stays_disabled_until_the_daemon_has_said_what_it_is() {
    let state = live(&[]);
    // Nothing read yet: the item can neither tick nor act, because a guess
    // would re-enable hooks the operator had paused in `config.toml`.
    assert_eq!(
        Summary::of(state.mirror(), true, None).pause_hooks_action(),
        None
    );
    let on = Summary::of(state.mirror(), true, Some(true));
    let action = on.pause_hooks_action().expect("an action");
    assert_eq!(
        action,
        UiAction::ConfigSet {
            key: "hooks.enabled".into(),
            value: ConfigValue::Bool(false)
        }
    );
    let (method, params) = action.request();
    assert_eq!(method, "config.set");
    // §4.11's shape is a patch: the daemon refuses a flat `{key, value}`, so a
    // pause sent that way would be `-32602` and the hooks would keep firing.
    assert_eq!(params["patch"]["hooks.enabled"], false);
    assert!(params.get("key").is_none());
    // `hooks.enabled` is a hot key (§3.7), so this is a real pause and not a
    // restart — which is why the tray offers it at all.
    let off = Summary::of(state.mirror(), true, Some(false));
    assert_eq!(
        off.pause_hooks_action(),
        Some(UiAction::ConfigSet {
            key: "hooks.enabled".into(),
            value: ConfigValue::Bool(true)
        })
    );
}

#[test]
fn quit_is_the_gui_alone_and_stop_service_is_the_one_that_stops_the_deck() {
    // §8.2 asks for both items and for the tooltip that tells them apart.
    assert!(QUIT_TOOLTIP.contains("Quits this window only"));
    assert!(QUIT_TOOLTIP.contains("service keeps running"));
    assert!(QUIT_TOOLTIP.contains("Stop service"));
    assert_eq!(UiAction::ServiceStop.request().0, "service.stop");
    // Six items, each with its own id, each recognised on the way back.
    assert_eq!(TrayCommand::ALL.len(), 6);
    for command in TrayCommand::ALL {
        assert_eq!(TrayCommand::parse(command.id()), Some(command));
    }
    // An id from a build that is not this one is not a command.
    assert_eq!(TrayCommand::parse("tray-per-agent-row"), None);
}

#[test]
fn a_service_that_is_not_answering_greys_the_tray_whatever_the_last_roster_said() {
    let mut state = live(&[("a", AgentState::Question)]);
    assert_eq!(Summary::of(state.mirror(), true, None).dot, Tone::Red);
    state.handle_event(AppEvent::Api(ApiEvent::Disconnected));
    // §8.2: the last mirror is kept and greyed. The tray follows: a green or
    // red dot for a daemon that has gone would be reporting a service that
    // is not there.
    assert!(!state.connected());
    assert!(!state.mirror().agents.is_empty(), "the roster is kept");
    let summary = Summary::of(state.mirror(), state.connected(), Some(true));
    assert_eq!(summary.dot, Tone::Gray);
    assert_eq!(summary.headline, "Service not running");
}

#[test]
fn the_tray_icon_is_a_real_bitmap_whose_dot_is_the_only_thing_that_changes() {
    let px = present::TRAY_ICON_PX as usize;
    let grey = present::tray_icon_rgba(Tone::Gray);
    assert_eq!(grey.len(), px * px * 4, "RGBA, one byte a channel");
    // Something is drawn: an icon that was entirely transparent would be a
    // status item nobody can click.
    let opaque = grey.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
    assert!(opaque > px * 4, "{opaque} opaque pixels is not a glyph");
    // Template images carry state in their alpha mask, so AppKit can tint
    // every state consistently with the menu bar's appearance.
    let mut seen = Vec::new();
    for tone in [Tone::Red, Tone::Amber, Tone::Green, Tone::Blue, Tone::Gray] {
        let bytes = present::tray_icon_rgba(tone);
        assert_eq!(bytes.len(), grey.len());
        assert!(!seen.contains(&bytes), "{tone:?} shares a status symbol");
        assert!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3] == [0, 0, 0]),
            "{tone:?} has a fixed color instead of a template mask"
        );
        seen.push(bytes);
    }
    // Only the dot moves: the glyph is the same drawing whatever the state.
    let red = present::tray_icon_rgba(Tone::Red);
    let changed = grey
        .as_chunks::<4>()
        .0
        .iter()
        .zip(red.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed > 0 && changed < px * px / 4,
        "{changed} pixels moved"
    );
}
