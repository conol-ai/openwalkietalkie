//! Channel-5 deck traffic end to end through `owt_core_hid_report`: the
//! runtime's gating and transport rules (docs/deck-protocol.md), not the grammar.
use super::*;
use crate::deck::msg::Template;
use std::{string::String, vec::Vec};

static DISCONNECTS: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
extern "C" fn owt_hid_disconnect() {
    DISCONNECTS.fetch_add(1, Ordering::Relaxed);
}

const AGENT: &[u8] = br#"{"m":"agent","id":1,"kind":"codex","name":"cohdl-api","state":"working"}"#;

#[test]
fn bluetooth_rssi_resets_on_reconnect_and_never_wakes_the_panel() {
    use ui::bluetooth::{Level, Status};
    let _guard = lock(&CORE_LOCK);
    reset();
    unsafe {
        owt_core_hid_connected(true);
        let core = &mut *core::ptr::addr_of_mut!(CORE);
        core.power.tick(power::OFF_AFTER_MS + 1);
        assert_eq!(core.power.screen(), power::Screen::Off);
        assert_eq!(core.ui.bluetooth, Status::Unknown);
        owt_core_bluetooth_rssi(-40);
        assert_eq!(core.ui.bluetooth, Status::Signal(Level::Strong));
        assert_eq!(core.power.screen(), power::Screen::Off);
        owt_core_bluetooth_rssi(127);
        assert_eq!(core.ui.bluetooth, Status::Unknown);
        owt_core_bluetooth_rssi(-95);
        assert_eq!(core.ui.bluetooth, Status::Signal(Level::Weak));
        owt_core_hid_connected(false);
        owt_core_bluetooth_rssi(-40);
        assert_eq!(core.ui.bluetooth, Status::Disconnected);
        owt_core_hid_connected(true);
        assert_eq!(core.ui.bluetooth, Status::Unknown);
        owt_core_bluetooth_rssi(-40);
        assert_eq!(core.ui.bluetooth, Status::Signal(Level::Strong));
    }
}

/// Fresh core on the Agents face, link up, keypad present, screen awake.
fn connected() -> &'static mut Core {
    reset();
    DISCONNECTS.store(0, Ordering::Relaxed);
    unsafe {
        let core = &mut *core::ptr::addr_of_mut!(CORE);
        core.devices = 2;
        core.ui.on_face(Template::Agents);
        owt_core_hid_connected(true);
        owt_core_poll(100);
        core
    }
}

#[test]
fn hello_gets_a_hello_reply_without_any_channel2_request() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // Power-on: no host yet and no demo.
    assert!(!core.ui.deck.demo_active() && !core.ui.deck.host_present);
    host_line(HELLO);
    let reply = deck_lines();
    assert_eq!(reply.len(), 1);
    assert!(reply[0].starts_with(r#"{"m":"hello","v":1,"fw":""#), "{}", reply[0]);
    let agents = core.ui.deck.agents.len();
    assert!(reply[0].contains(&format!(r#""boot":1,"rx":1024,"agents":{agents},"lang":0,"lang_rev":0,"faces":[{{"f":1,"t":"agents","n":"Agents"}}"#)), "{}", reply[0]);
    assert!(core.ui.deck.host_present && !core.ui.deck.demo_active());
    assert_eq!(core.ui.deck.sid, 7);
    assert!(core.ui.deck.clock_valid);
    // A saved interface preference is advertised independently of boot's
    // host-settings freshness, including its revision on every heartbeat.
    core.ui.language = crate::ui::i18n::Language::Japanese;
    core.ui.language_revision = 7;
    // A heartbeat with the same sid is answered too and changes nothing.
    host_line(HELLO);
    let heartbeat = deck_lines();
    assert_eq!(heartbeat.len(), 1);
    assert!(heartbeat[0].contains(r#""boot":1"#), "{}", heartbeat[0]);
    assert!(heartbeat[0].contains(r#""lang":3,"lang_rev":7"#), "{}", heartbeat[0]);
    assert!(core.ui.deck.host_present);
}

#[test]
fn deck_events_flow_after_hello_without_any_channel2_request() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // Demo: SHIFT+SCROLL slides but nothing reaches the wire.
    key(43, true, 200);
    key(41, true, 210);
    key(41, false, 220);
    key(43, false, 230);
    assert_eq!(core.ui.current_face().template, Template::Omarchy);
    assert_eq!(drained(), (Vec::new(), Vec::new()));
    host_line(HELLO);
    deck_lines();
    // Past the 150 ms detent limit of the first slide.
    key(43, true, 400);
    key(41, true, 410);
    key(41, false, 420);
    key(43, false, 430);
    assert_eq!(core.ui.current_face().template, Template::Macropad);
    assert_eq!(deck_lines(), [r#"{"m":"face","f":3}"#]);
}

#[test]
fn ptt_round_trip_after_hello() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    deck_lines();
    assert!(core.ui.deck.agent(1).is_some_and(|agent| agent.present));
    key(44, true, 400);
    assert!(core.ui.global.ptt.is_some());
    assert_eq!(
        deck_lines(),
        [
            r#"{"m":"channel","agent":1,"seq":1}"#,
            r#"{"m":"ptt","act":1,"agent":1,"ctx":"talk","p":1}"#,
        ]
    );
    host_line(br#"{"m":"transcript","agent":1,"text":"run the tests"}"#);
    assert_eq!(core.ui.deck.transcript.as_str(), "run the tests");
    key(44, false, 900);
    assert_eq!(deck_lines(), [r#"{"m":"ptt","act":0,"agent":1,"ctx":"talk","p":1}"#]);
    assert!(core.ui.global.ptt.is_none());
    assert!(core.ui.global.ptt_pending.is_some());
    host_line(br#"{"m":"transcript","agent":1,"text":"run all the tests","final":1,"p":1}"#);
    assert!(core.ui.global.ptt_pending.is_none());
    assert_eq!(core.ui.deck.transcript.as_str(), "run all the tests");
}

/// The runtime half of an answer: the event reaches the wire encoded, and the
/// host's `card clear` comes back through the assembler into the model (what
/// the grammar then does with the notice is its own test).
#[test]
fn answer_round_trip_reaches_the_wire_and_the_clear_comes_back() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"card","agent":1,"id":40,"type":"q","text":"Ship it?","choices":["Yes","No"],"rec":0}"#);
    deck_lines();
    assert!(core.ui.deck.card(1).is_some_and(|card| card.id == 40));
    assert_eq!(core.ui.notify.toast.and_then(|t| t.notice()), Some((1, 40)));
    let mut update = Update::new();
    update.push_deck(Event::Answer { agent: 1, card: 40, choice: 1 });
    emit(core, &mut update);
    assert_eq!(deck_lines(), [r#"{"m":"answer","agent":1,"card":40,"choice":1}"#]);
    host_line(br#"{"m":"card","agent":1,"id":40,"clear":1}"#);
    assert!(core.ui.deck.card(1).is_none());
    assert!(deck_lines().is_empty());
    // Offline (link dropped after hello): the same event goes nowhere.
    unsafe { owt_core_hid_connected(false) };
    let mut update = Update::new();
    update.push_deck(Event::Answer { agent: 1, card: 40, choice: 1 });
    emit(core, &mut update);
    assert!(drained().1.is_empty());
}

#[test]
fn system_requests_are_answered_on_channel2_and_leave_the_deck_alone() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    deck_lines();
    rpc_request(br#"{"method":"device.status","id":3}"#);
    let (rpc, deck) = drained();
    assert!(deck.is_empty());
    assert_eq!(rpc.len(), 1);
    assert!(rpc[0].starts_with(r#"{"id":3,"result":{"version":"0.1.0-openwalkietalkie-hfp","battery":"#), "{}", rpc[0]);
    assert!(deck_online(core));
    // Keys still reach the deck host, never channel 2.
    core.ui.on_face(Template::Omarchy);
    key(31, true, 600);
    key(31, false, 610);
    assert_eq!(
        deck_lines(),
        [
            r#"{"m":"key","k":"enter","act":1,"shift":0,"f":2}"#,
            r#"{"m":"key","k":"enter","act":0,"shift":0,"f":2}"#,
        ]
    );
}

#[test]
fn several_lines_per_report_and_a_line_across_reports_are_both_applied() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // Two short messages in one body, then a 130-byte one over three bodies.
    let mut two = Vec::new();
    two.extend_from_slice(HELLO);
    two.push(b'\n');
    two.extend_from_slice(AGENT);
    assert!(two.len() + 1 > proto::PAYLOAD_MAX);
    host_line(&two);
    assert_eq!(deck_lines().len(), 1);
    assert!(core.ui.deck.agent(1).is_some_and(|agent| agent.present));
    let mut long = Vec::from(&br#"{"m":"agent","id":2,"kind":"claude","name":"web-ui","state":"ready","title":""#[..]);
    long.resize(128, b'x');
    long.extend_from_slice(br#""}"#);
    assert!(long.len() > 2 * proto::PAYLOAD_MAX);
    host_line(&long);
    let agent = core.ui.deck.agent(2).unwrap();
    assert!(agent.present && agent.title.as_str().starts_with("xxxx"));
    assert!(deck_lines().is_empty());
}

#[test]
fn bad_lines_are_answered_with_err_at_most_once_per_second() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    deck_lines();
    unsafe { owt_core_poll(1000) };
    host_line(b"{\"m\":\"agent\",\"id\":1");
    host_line(b"{\"m\":\"agent\",\"id\":9}");
    assert_eq!(deck_lines(), [r#"{"m":"err","why":"parse"}"#]);
    unsafe { owt_core_poll(1999) };
    host_line(b"{\"m\":\"nope\"}");
    assert!(deck_lines().is_empty());
    unsafe { owt_core_poll(2000) };
    host_line(b"{\"m\":\"nope\"}");
    assert_eq!(deck_lines(), [r#"{"m":"err","why":"unknown"}"#]);
    // An overlong line is discarded up to its LF and reported as size.
    unsafe { owt_core_poll(3000) };
    let mut huge = Vec::from(&b"{\"m\":\"reply\",\"agent\":1,\"text\":\""[..]);
    huge.resize(proto::LINE_MAX + 40, b'y');
    huge.extend_from_slice(b"\"}");
    host_line(&huge);
    assert_eq!(deck_lines(), [r#"{"m":"err","why":"size"}"#]);
    assert!(core.ui.deck.host_present);
    // The next line is whole again.
    unsafe { owt_core_poll(4000) };
    host_line(AGENT);
    assert!(core.ui.deck.agent(1).is_some_and(|agent| agent.present));
    assert!(deck_lines().is_empty());
}

#[test]
fn nothing_is_transmitted_on_channel5_before_the_host_spoke_there() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // A bad first line: the err has nowhere to go yet.
    host_line(b"{\"m\":\"agent\",\"id\":9}");
    assert!(drained().1.is_empty());
    // A stale half line must not join the next link's first line.
    host_line(&HELLO[..20]);
    unsafe {
        owt_core_hid_connected(false);
        owt_core_hid_connected(true);
    }
    assert!(!core.deck_rx);
    host_line(&HELLO[20..]);
    assert!(drained().1.is_empty());
    assert!(!core.ui.deck.host_present);
    host_line(HELLO);
    assert_eq!(deck_lines().len(), 1);
    assert!(core.deck_rx && core.ui.deck.host_present);
    // A link drop makes the deck offline again; nothing is sent until hello.
    unsafe { owt_core_hid_connected(false) };
    assert!(!core.ui.deck.host_present && !core.deck_rx);
    unsafe { owt_core_hid_connected(true) };
    key(43, true, 200);
    key(41, true, 210);
    key(41, false, 220);
    key(43, false, 230);
    assert!(drained().1.is_empty());
}

#[test]
fn overflow_sends_up_then_drops_the_link() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // Offline: no `up` for a host that never spoke, but the link still drops.
    let mut update = Update::new();
    update.overflow = true;
    emit(core, &mut update);
    assert!(drained().1.is_empty());
    assert_eq!(DISCONNECTS.load(Ordering::Relaxed), 1);
    host_line(HELLO);
    deck_lines();
    let mut update = Update::new();
    update.push_deck(Event::Face { id: 2 });
    update.overflow = true;
    emit(core, &mut update);
    assert_eq!(deck_lines(), [r#"{"m":"face","f":2}"#, r#"{"m":"up"}"#]);
    assert_eq!(DISCONNECTS.load(Ordering::Relaxed), 2);
}

#[test]
fn demo_events_take_the_loopback_and_never_the_wire() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // The demo is off at power-on; these follow a deck that has it on.
    core.ui.load_demo();
    // A system request is answered, but the deck stays in the demo until hello.
    rpc_request(br#"{"method":"device.status","id":3}"#);
    assert_eq!(drained().0.len(), 1);
    assert!(core.ui.deck.demo_active() && !deck_online(core));
    // PTT on the demo roster: channel + ptt go to the built-in host.
    assert_ne!(core.ui.agents.highlight, 0);
    key(44, true, 400);
    assert!(core.ui.global.ptt.is_some());
    key(44, false, 500);
    assert_eq!(drained(), (Vec::new(), Vec::new()));
    // The first real hello ends the demo: the same press now reaches the host.
    host_line(HELLO);
    host_line(AGENT);
    deck_lines();
    assert!(!core.ui.deck.demo_active() && deck_online(core));
    key(44, true, 600);
    key(44, false, 700);
    let deck = deck_lines();
    // The demo's channel died with the session: `channel` precedes the press.
    assert_eq!(deck.len(), 3, "{deck:?}");
    assert!(deck[0].starts_with(r#"{"m":"channel","agent":1"#));
    assert!(deck[1].starts_with(r#"{"m":"ptt","act":1,"agent":1"#));
    assert!(deck[2].starts_with(r#"{"m":"ptt","act":0,"agent":1"#));
}

#[test]
fn a_new_card_lights_the_dark_screen_but_host_chatter_does_not() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    unsafe { owt_core_poll(100 + power::OFF_AFTER_MS) };
    assert_eq!(core.power.screen(), power::Screen::Off);
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"reply","agent":1,"text":"done"}"#);
    deck_lines();
    assert_eq!(core.power.screen(), power::Screen::Off);
    host_line(br#"{"m":"card","agent":1,"id":40,"type":"q","text":"Ship it?","choices":["Yes","No"]}"#);
    assert_eq!(core.power.screen(), power::Screen::Active);
    assert!(core.redraw);
    // The lock is opaque: retain the notice in the drawer for after unlock.
    assert!(core.ui.notify.toast.is_none());
    assert!(core.ui.deck.notices.iter().any(|n| n.agent == 1 && n.card == 40 && n.in_drawer));
    assert_eq!(core.ui.screen_lock, Some(0));
    unsafe {
        owt_core_poll(100 + power::OFF_AFTER_MS + 300);
        owt_core_frame_ready();
    }
    assert_eq!(core.ui.screen_lock, Some(0));
    // Lit for the notice window, then dark again without input.
    unsafe {
        owt_core_poll(100 + power::OFF_AFTER_MS + power::NOTIFY_MS - 1);
        assert_eq!(owt_core_screen(), power::Screen::Active as u8);
        owt_core_poll(100 + power::OFF_AFTER_MS + power::NOTIFY_MS + 1);
        assert_eq!(owt_core_screen(), power::Screen::Off as u8);
    }
    // A repeated card is not a new notice: it does not wake.
    host_line(br#"{"m":"card","agent":1,"id":40,"type":"q","text":"Ship it now?","choices":["Yes","No"]}"#);
    assert_eq!(core.power.screen(), power::Screen::Off);
    deck_lines();
    // A blind ENTER only wakes: the roster is untouched and nothing is sent.
    let at = 100 + 2 * power::OFF_AFTER_MS;
    key(31, true, at);
    key(31, false, at + 10);
    assert_eq!(core.power.screen(), power::Screen::Active);
    assert_eq!(core.ui.agents.view, ui::state::AgentsView::Roster);
    assert!(deck_lines().is_empty());
}

/// A card showing under a dark screen is not answered by the
/// ENTER that wakes it; ENTER answers only after the wheel unlocks it.
#[test]
fn card_under_dark_screen_not_answered_by_wake_enter() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    // Idle-home off, so the card is still up when the screen goes dark.
    host_line(br#"{"m":"set","k":"idlehome","v":0}"#);
    host_line(br#"{"m":"card","agent":1,"id":40,"type":"q","text":"Ship it?","choices":["Yes","No"]}"#);
    // ENTER on the roster opens the channel, ENTER there opens the card.
    key(31, true, 200);
    key(31, false, 210);
    key(31, true, 1000);
    key(31, false, 1010);
    assert_eq!((core.ui.agents.view, core.ui.agents.agent), (ui::state::AgentsView::Card, 1));
    assert!(deck_lines().iter().all(|line| !line.contains("answer")));
    // Two idle minutes with the host's heartbeat: the screen goes dark.
    let dark = 1010 + power::OFF_AFTER_MS;
    for step in 1..=12 {
        unsafe { owt_core_poll(1010 + step * 10_000) };
        host_line(AGENT);
    }
    assert_eq!(core.power.screen(), power::Screen::Off);
    assert!(core.ui.deck.host_present);
    key(31, true, dark + 10);
    key(31, false, dark + 20);
    assert_eq!(core.power.screen(), power::Screen::Active);
    assert_eq!(core.ui.agents.view, ui::state::AgentsView::Card, "still the card");
    assert!(deck_lines().is_empty(), "the wake ENTER answers nothing");
    assert_eq!(core.ui.deck.card(1).unwrap().pending_answer, None);
    // A visible wake frame alone does not unlock the card.
    unsafe {
        owt_core_poll(dark + 300);
        owt_core_frame_ready();
    }
    key(31, true, dark + 350);
    key(31, false, dark + 360);
    assert!(deck_lines().is_empty());
    for tick in 0..4 {
        key(41, true, dark + 400 + tick * 20);
        key(41, false, dark + 410 + tick * 20);
        assert!(deck_lines().is_empty());
    }
    assert_eq!(core.ui.screen_lock, None);
    let at = dark + 300 + ui::state::global::GUARD_MS;
    key(31, true, at);
    key(31, false, at + 10);
    assert_eq!(deck_lines(), [r#"{"m":"answer","agent":1,"card":40,"choice":0}"#,
        r#"{"m":"session","agent":1,"f":0,"p":0,"r":0,"i":255}"#]);
    assert_eq!(core.ui.deck.card(1).unwrap().pending_answer, Some(0));
}

#[test]
fn brightness_follows_the_user_setting_and_the_screen_state() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    unsafe {
        assert_eq!(owt_core_brightness(), power::FULL_BRIGHTNESS);
        host_line(HELLO);
        host_line(br#"{"m":"set","k":"brightness","v":2}"#);
        deck_lines();
        assert_eq!(core.ui.user_brightness(), 2);
        assert_eq!(owt_core_brightness(), power::brightness(power::Screen::Active, 2));
        owt_core_poll(100 + power::DIM_AFTER_MS);
        assert_eq!(owt_core_brightness(), power::brightness(power::Screen::Dim, 2));
        owt_core_poll(100 + power::OFF_AFTER_MS);
        assert_eq!(owt_core_brightness(), 0);
    }
}

#[test]
fn wait_ms_is_the_dark_poll_while_off_even_while_the_ui_animates() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"card","agent":1,"id":41,"type":"r","text":"Header refactor done"}"#);
    deck_lines();
    assert!(core.ui.notify.toast.is_some() && core.ui.animating());
    unsafe {
        owt_core_frame_ready();
        assert_eq!(
            owt_core_wait_ms(),
            power::IDLE_POLL_MS.min(power::Screen::Active.animation_ms())
        );
        let dark = 100 + power::NOTIFY_MS + power::OFF_AFTER_MS;
        owt_core_poll(dark);
        assert_eq!(owt_core_screen(), power::Screen::Off as u8);
        // Something the UI would keep animating (a session nobody holds,
        // started now: the 60 s cap has not run).
        core.ui.global.ptt = Some(ui::state::PttSession::new(
            1,
            crate::deck::msg::PttCtx::Talk,
            0,
            dark,
        ));
        assert!(core.ui.animating());
        assert_eq!(owt_core_wait_ms(), power::DARK_POLL_MS);
    }
}

#[test]
fn idle_clock_reaches_the_ui_and_reads_zero_while_held() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // Polling is not input: idle since power-on.
    unsafe { owt_core_poll(5100) };
    assert_eq!(core.power.idle_ms(5100), 5100);
    key(44, true, 6000);
    assert_eq!(core.power.idle_ms(90_000), 0);
    key(44, false, 90_000);
    unsafe { owt_core_poll(90_500) };
    assert_eq!(core.power.idle_ms(90_500), 500);
}

/// One sitting with the device, through the C entry points only: the demo
/// from boot, then a real host taking over, then losing it.
#[test]
fn a_session_from_boot_through_the_demo_to_a_real_host_and_its_loss() {
    use crate::deck::msg::AgentState;
    use ui::kit;
    use ui::state::{AgentsView, Overlay};
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    // The demo is off at power-on; these follow a deck that has it on.
    core.ui.load_demo();
    let mut now = 200;
    let tap = |number: u8, at: &mut u32| {
        key(number, true, *at);
        key(number, false, *at + 10);
        *at += 700;
    };
    // Demo roster: the Question agent leads; ENTER opens its channel, ENTER
    // opens the card, ENTER (past the guard) answers it.
    assert!(core.ui.deck.demo_active());
    assert_eq!((core.ui.agents.view, core.ui.agents.highlight), (AgentsView::Roster, 1));
    tap(31, &mut now);
    assert_eq!((core.ui.agents.view, core.ui.global.active_channel), (AgentsView::Channel, 1));
    tap(31, &mut now);
    assert_eq!(core.ui.agents.view, AgentsView::Card);
    assert_eq!(core.ui.deck.card(1).map(|card| card.id), Some(1));
    tap(31, &mut now);
    // The built-in host cleared the card at once and the agent gets to work
    // on the next lit tick after its 400 ms.
    assert_eq!(core.ui.agents.view, AgentsView::Channel);
    assert!(core.ui.deck.card(1).is_none());
    unsafe { owt_core_poll(now) };
    assert_eq!(core.ui.deck.agent(1).map(|agent| agent.state), Some(AgentState::Working));
    assert_eq!(drained(), (Vec::new(), Vec::new()));
    tap(42, &mut now);
    assert_eq!(core.ui.agents.view, AgentsView::Roster);
    // SHIFT+SCROLL to the far end of the strip and back, one face per detent.
    let faces = core.ui.global.strip_len as usize;
    key(43, true, now);
    for (wheel, expect) in [(41, faces - 1), (21, 0)] {
        for _ in 0..faces + 1 {
            now += 170;
            key(wheel, true, now);
            key(wheel, false, now + 5);
        }
        assert_eq!(core.ui.global.current as usize, expect);
    }
    key(43, false, now + 20);
    now += 1000;
    assert_eq!(core.ui.current_face().template, Template::Agents);
    // Double-SHIFT opens the manager on the current face and closes it again.
    for overlay in [Overlay::Manager, Overlay::None] {
        key(43, true, now);
        key(43, false, now + 50);
        key(43, true, now + 200);
        key(43, false, now + 250);
        assert_eq!(core.ui.global.overlay, overlay);
        now += 1000;
    }
    // PTT on the roster: the LISTENING session types a transcript in.
    key(44, true, now);
    assert!(core.ui.global.ptt.is_some());
    for step in 1..40 {
        unsafe { owt_core_poll(now + step * 50) };
    }
    assert!(!core.ui.deck.transcript.is_empty() && !core.ui.deck.transcript_final);
    key(44, false, now + 2000);
    now += 3000;
    unsafe { owt_core_poll(now) };
    assert!(core.ui.global.ptt.is_none() && core.ui.deck.transcript_final);
    assert_eq!(drained(), (Vec::new(), Vec::new()));
    // A real host says hello: the demo is gone, the reply is on the wire.
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"card","agent":1,"id":40,"type":"q","text":"Ship it?","choices":["Yes","No"],"rec":0}"#);
    let lines = deck_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(r#"{"m":"hello","#));
    assert!(!core.ui.deck.demo_active() && deck_online(core));
    assert_eq!(core.ui.deck.order_len, 1);
    assert_eq!(kit::status_tag(&core.ui), kit::HeaderRight::Clock(19 * 60 + 42));
    assert_eq!(core.ui.notify.toast.and_then(|t| t.notice()), Some((1, 40)));
    unsafe { owt_core_poll(now + 100) };
    now += 5000;
    tap(31, &mut now);
    tap(31, &mut now);
    tap(31, &mut now);
    // The channel sequence counts from boot: the demo's ENTER was seq 1.
    assert_eq!(deck_lines(), [r#"{"m":"channel","agent":1,"seq":2}"#, r#"{"m":"session","agent":1,"f":0,"p":0,"r":0,"i":255}"#, r#"{"m":"answer","agent":1,"card":40,"choice":0}"#,
        r#"{"m":"session","agent":1,"f":0,"p":0,"r":0,"i":255}"#]);
    host_line(br#"{"m":"card","agent":1,"id":40,"clear":1}"#);
    host_line(br#"{"m":"agent","id":1,"state":"working"}"#);
    assert_eq!(core.ui.deck.agent(1).map(|agent| agent.state), Some(AgentState::Working));
    // The link drops: offline, never back to the demo, nothing sent.
    unsafe { owt_core_hid_connected(false) };
    assert!(!core.ui.deck.host_present && !core.ui.deck.demo_active());
    assert_eq!(kit::status_tag(&core.ui), kit::HeaderRight::Text("offline"));
    assert!(core.ui.deck.agent(1).is_some_and(|agent| agent.present && agent.stale));
    tap(31, &mut now);
    assert_eq!(drained(), (Vec::new(), Vec::new()));
}

/// A daemon that stalls past the heartbeat with the link up never saw the
/// releases: its returning hello is answered with `up`.
#[test]
fn heartbeat_loss_with_the_link_up_owes_the_returning_host_an_up() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    deck_lines();
    key(44, true, 400);
    assert_eq!(
        deck_lines(),
        [r#"{"m":"channel","agent":1,"seq":1}"#, r#"{"m":"ptt","act":1,"agent":1,"ctx":"talk","p":1}"#]
    );
    unsafe { owt_core_poll(400 + 15_500) };
    assert!(!core.ui.deck.host_present);
    key(44, false, 16_000);
    assert_eq!(deck_lines(), Vec::<String>::new());
    host_line(HELLO);
    let sent = deck_lines();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].starts_with(r#"{"m":"hello","#));
    assert_eq!(sent[1], r#"{"m":"up"}"#);
}

#[test]
fn heartbeat_loss_with_the_link_up_owes_a_held_host_key_an_up() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    core.ui.on_face(Template::Omarchy);
    host_line(HELLO);
    deck_lines();
    key(32, true, 400);
    assert!(deck_lines().iter().any(|l| l.contains(r#""m":"key","k":"left","act":1"#)));
    unsafe { owt_core_poll(400 + 15_500) };
    assert!(!core.ui.deck.host_present);
    key(32, false, 16_000);
    host_line(HELLO);
    let sent = deck_lines();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[1], r#"{"m":"up"}"#);
}

/// main.c: the Off tick lands while a frame is in flight, so the panel never
/// sleeps and no wake frame follows; the lock still needs all four detents.
#[test]
fn an_off_tick_locks_even_when_the_panel_is_busy() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"agent","id":2,"kind":"codex","name":"b","state":"working"}"#);
    let off = 100 + power::OFF_AFTER_MS;
    unsafe { owt_core_poll(off) };
    assert_eq!(core.power.screen(), power::Screen::Off);
    let highlight = core.ui.agents.highlight;
    key(21, true, off + 5);
    key(21, false, off + 10);
    assert_eq!(core.power.screen(), power::Screen::Active);
    assert_eq!(core.ui.agents.highlight, highlight, "the waking detent only wakes");
    key(21, true, off + 100);
    key(21, false, off + 110);
    assert_eq!(core.ui.agents.highlight, highlight);
    for tick in 0..4 {
        key(41, true, off + 200 + tick * 20);
        key(41, false, off + 210 + tick * 20);
        assert_eq!(core.ui.agents.highlight, highlight);
    }
    key(41, true, off + 10_000);
    key(41, false, off + 10_030);
    assert_ne!(core.ui.agents.highlight, highlight);
    // After a real sleep the wake frame gates input until it is shown.
    unsafe {
        owt_core_poll(off + 10_030 + power::OFF_AFTER_MS);
        owt_core_panel_slept();
    }
    let highlight = core.ui.agents.highlight;
    let at = off + 20_000 + power::OFF_AFTER_MS;
    for step in 0..2 {
        key(41, true, at + step * 100);
        key(41, false, at + step * 100 + 10);
    }
    assert_eq!(core.ui.agents.highlight, highlight);
    unsafe { owt_core_frame_ready() };
    for step in 2..4 {
        key(41, true, at + step * 100);
        key(41, false, at + step * 100 + 10);
        assert_eq!(core.ui.agents.highlight, highlight);
    }
    key(21, true, at + 400);
    key(21, false, at + 410);
    assert_ne!(core.ui.agents.highlight, highlight);
}

/// A keypad bus outage releases the buttons once, not on every poll: the UI
/// still goes quiet and follows a host focus.
#[test]
fn a_keypad_bus_outage_still_lets_a_host_focus_be_followed() {
    let _guard = lock(&CORE_LOCK);
    let core = connected();
    host_line(HELLO);
    host_line(AGENT);
    host_line(br#"{"m":"agent","id":2,"kind":"codex","name":"b","state":"working"}"#);
    deck_lines();
    assert_ne!(core.ui.agents.highlight, 2);
    KEYPAD_BUS_FAILS.store(true, Ordering::Relaxed);
    for now in (200..5000).step_by(50) {
        unsafe { owt_core_poll(now) };
    }
    host_line(br#"{"m":"focus","agent":2}"#);
    assert_eq!(core.ui.agents.highlight, 2);
    // The bus recovers and fails again: that outage releases again.
    KEYPAD_BUS_FAILS.store(false, Ordering::Relaxed);
    key(44, true, 5100);
    assert!(core.power.held());
    KEYPAD_BUS_FAILS.store(true, Ordering::Relaxed);
    unsafe { owt_core_poll(5200) };
    assert!(!core.power.held() && core.ui.global.ptt.is_none());
}
