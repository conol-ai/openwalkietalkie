//! Global layer, touch recogniser, Agents face and notification grammar
//! (`ui::state`), driven end to end through the public `UiState` API. The
//! rule-by-rule tests live next to the code; these cover what only shows
//! across files: balanced presses under random input, overlay capture, PTT
//! from every face, the event budget, a whole session from hello to answer,
//! and today's Codex-only use.
use crate::control::{Button, Control, Edge, Touch, TouchPhase, Wheel};
use crate::deck::msg::{AgentState, CardKind, Event, HostMsg, PttCtx, SettingKey, Template};
use crate::deck::text::Text;
use crate::ui::state::{AgentsView, Overlay};
use crate::ui::{UiState, Update};

fn fresh() -> Box<UiState> {
    let mut ui = Box::new(UiState::new());
    ui.reset();
    // These tests start from the demo session (off at power-on).
    ui.load_demo();
    ui
}

/// A real host (hello at 0, sid 1) with the demo roster: agents 1 (Question,
/// card 1) .. 5 (Error), and an empty drawer.
fn hosted() -> Box<UiState> {
    let mut ui = fresh();
    ui.host(&hello(1, 0), 0, &mut Update::new());
    crate::deck::demo::load(&mut ui.deck);
    ui.deck.settings.demo = false;
    for notice in &mut ui.deck.notices {
        notice.used = false;
    }
    ui.agents.highlight = 1;
    ui.invalidate();
    ui
}

fn hello(sid: u32, minute: u16) -> HostMsg {
    HostMsg::Hello {
        v: 1,
        sid,
        minute_of_day: Some(minute),
    }
}

fn agent(id: u8, state: AgentState, name: &str) -> HostMsg {
    HostMsg::Agent {
        id,
        gone: false,
        kind: Some(crate::deck::msg::AgentKind::Claude),
        state: Some(state),
        name: Some(Text::from_str(name)),
        project: None,
        title: Some(Text::from_str("working on it")),
        since_s: Some(120), task: None, subs: None,
    }
}

fn card(agent: u8, id: u16, kind: CardKind, choices: u8) -> HostMsg {
    let mut texts = [Text::new(); 4];
    for (index, text) in texts.iter_mut().enumerate().take(choices as usize) {
        text.set(["yes", "no", "later", "never"][index]);
    }
    HostMsg::Card {
        agent,
        id,
        clear: false,
        kind,
        text: Text::from_str("Ship it?"),
        choices: texts,
        choice_count: choices,
        recommended: (choices > 0).then_some(0),
    }
}

fn clear(agent: u8, id: u16) -> HostMsg {
    let mut msg = card(agent, id, CardKind::Question, 0);
    if let HostMsg::Card { clear, .. } = &mut msg {
        *clear = true;
    }
    msg
}

fn apply(ui: &mut UiState, control: Control, at: u32) -> Update {
    let mut out = Update::new();
    ui.apply(control, at, &mut out);
    out
}

fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
    let out = apply(ui, Control::Button(button, Edge::Press), at);
    apply(ui, Control::Button(button, Edge::Release), at + 1);
    out
}

fn host(ui: &mut UiState, msg: &HostMsg, at: u32) -> Update {
    let mut out = Update::new();
    ui.host(msg, at, &mut out);
    out
}

fn tick(ui: &mut UiState, at: u32) -> Update {
    let mut out = Update::new();
    ui.tick(at, 0, true, &mut out);
    out
}

fn events(out: &Update) -> Vec<Event> {
    out.deck_events().collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u32) as usize]
    }
}

struct Tracker {
    seed: u64,
    ptt_live: Option<(u8, PttCtx, u16)>,
    /// Host keys down, by (key, face) with the shift latched at the press.
    keys_down: Vec<(crate::deck::msg::KeyId, u8, bool)>,
    /// Macro keys down, by (face, index).
    macros_down: Vec<(u8, u8)>,
}

impl Tracker {
    fn clear(&mut self) {
        self.ptt_live = None;
        self.keys_down.clear();
        self.macros_down.clear();
    }

    fn check(&mut self, out: &Update, what: &str) {
        let seed = self.seed;
        for event in out.deck_events() {
            match event {
                Event::Ptt { act, agent, ctx, card, .. } => match (act, self.ptt_live) {
                    (1, None) => self.ptt_live = Some((agent, ctx, card)),
                    (0, Some(latched)) => {
                        assert_eq!(latched, (agent, ctx, card), "seed {seed}: release differs ({what})");
                        self.ptt_live = None;
                    }
                    other => panic!("seed {seed}: unbalanced ptt {other:?} ({what})"),
                },
                // Steps (act 2) have no release; a release repeats the latched shift.
                Event::Key { key, act, shift, face } => {
                    let at = self.keys_down.iter().position(|d| (d.0, d.1) == (key, face));
                    match (act, at) {
                        (1, None) => self.keys_down.push((key, face, shift)),
                        (0, Some(i)) => {
                            assert_eq!(self.keys_down[i].2, shift, "seed {seed}: key shift differs ({what})");
                            self.keys_down.swap_remove(i);
                        }
                        (2, _) => {}
                        other => panic!("seed {seed}: unbalanced key {key:?}/{face} {other:?} ({what})"),
                    }
                }
                Event::Macro { face, index, act } => {
                    let at = self.macros_down.iter().position(|d| *d == (face, index));
                    match (act, at) {
                        (1, None) => self.macros_down.push((face, index)),
                        (0, Some(i)) => {
                            self.macros_down.swap_remove(i);
                        }
                        (2, _) => {}
                        other => panic!("seed {seed}: unbalanced macro {face}/{index} {other:?} ({what})"),
                    }
                }
                _ => {}
            }
        }
    }
}

/// Every act 1 for an identity (a Codex key, a host key, a macro key, the PTT
/// session) is followed by exactly one act 0 for the same identity before the
/// next act 1, whatever random controls, host messages, jumps and time do in
/// between; and once every physical control is up, nothing is left pressed
/// anywhere.
#[test]
fn presses_are_balanced_per_identity_under_random_input() {
    for seed in [1u64, 2, 3, 4, 5] {
        let mut rng = Rng(seed | 0x9E37_79B9_7F4A_7C15);
        let mut ui = hosted();
        let mut now = 100u32;
        let mut tracker = Tracker {
            seed,
            ptt_live: None,
            keys_down: Vec::new(),
            macros_down: Vec::new(),
        };
        let mut physical = [false; 6];
        let mut finger = false;
        for step in 0..1500 {
            now = now.wrapping_add(rng.below(400));
            let what;
            let out = match rng.below(20) {
                0..=9 => {
                    let button = rng.pick(&Button::ALL);
                    let edge = if physical[button as usize] { Edge::Release } else { Edge::Press };
                    physical[button as usize] = edge == Edge::Press;
                    what = format!("{button:?} {edge:?}");
                    apply(&mut ui, Control::Button(button, edge), now)
                }
                10..=12 => {
                    what = String::from("wheel");
                    apply(&mut ui, Control::Wheel(rng.pick(&[Wheel::Cw, Wheel::Ccw])), now)
                }
                13..=15 => {
                    let phase = if finger {
                        rng.pick(&[TouchPhase::Move, TouchPhase::Up, TouchPhase::Cancel])
                    } else {
                        TouchPhase::Down
                    };
                    finger = !matches!(phase, TouchPhase::Up | TouchPhase::Cancel);
                    let touch = Touch { phase, x: rng.below(480) as u16, y: rng.below(480) as u16 };
                    what = format!("{touch:?}");
                    apply(&mut ui, Control::Touch(touch), now)
                }
                16 => {
                    let msg = match rng.below(6) {
                        0 => hello(1, 0),
                        1 => agent(1 + rng.below(6) as u8, AgentState::Question, "x"),
                        2 => card(1 + rng.below(6) as u8, rng.below(4) as u16, CardKind::Question, 3),
                        3 => clear(1 + rng.below(6) as u8, rng.below(4) as u16),
                        4 => HostMsg::Focus { agent: 1 + rng.below(6) as u8, ack: None },
                        _ => HostMsg::Set { key: SettingKey::AutoJump, value: rng.below(2) as u16 },
                    };
                    what = format!("{msg:?}");
                    host(&mut ui, &msg, now)
                }
                17 => {
                    let template = rng.pick(&Template::ALL);
                    what = format!("on_face {template:?}");
                    // A jump-cut with nothing held anywhere afterwards.
                    let mut out = Update::new();
                    ui.set_connected(false, &mut out);
                    ui.on_face(template);
                    ui.set_connected(true, &mut out);
                    tracker.clear();
                    out
                }
                18 => {
                    what = String::from("tick");
                    let mut out = Update::new();
                    ui.tick(now, rng.below(200_000), true, &mut out);
                    out
                }
                _ => {
                    what = String::from("heartbeat");
                    host(&mut ui, &HostMsg::None, now)
                }
            };
            assert!(!out.overflow, "seed {seed} step {step}: overflow ({what})");
            tracker.check(&out, &what);
        }
        // Lift everything: a stuck press would show here.
        for button in Button::ALL {
            if physical[button as usize] {
                let out = apply(&mut ui, Control::Button(button, Edge::Release), now);
                tracker.check(&out, "final release");
            }
        }
        if finger {
            let out = apply(&mut ui, Control::Touch(Touch { phase: TouchPhase::Up, x: 1, y: 1 }), now);
            tracker.check(&out, "final up");
        }
        assert!(ui.global.ptt.is_none() && !ui.touch.active() && !ui.global.shift, "seed {seed}");
        assert!(tracker.keys_down.is_empty() && tracker.macros_down.is_empty(), "seed {seed}");
        assert_eq!(ui.macropad.down, 0, "seed {seed}");
    }
}

#[test]
fn keys_under_an_overlay_are_not_forwarded_to_the_host_and_ptt_still_talks() {
    // Probes that keep each overlay's family open: ENTER grabs in the manager
    // but picks a template in the gallery and removes in the confirm dialog.
    let probes = [
        (Overlay::Drawer, [Button::Enter, Button::Left, Button::Right]),
        (Overlay::Manager, [Button::Enter, Button::Left, Button::Right]),
        (Overlay::Gallery, [Button::Left, Button::Right, Button::Cancel]),
        (Overlay::ConfirmRemove, [Button::Left, Button::Right, Button::Cancel]),
    ];
    for (overlay, buttons) in probes {
        let mut ui = hosted();
        ui.on_face(Template::Omarchy);
        ui.global.active_channel = 3;
        if overlay == Overlay::Drawer {
            ui.global.overlay = overlay;
        } else {
            // Double-SHIFT opens the manager on the current (movable) face.
            press(&mut ui, Button::Shift, 10);
            press(&mut ui, Button::Shift, 100);
            assert_eq!(ui.global.overlay, Overlay::Manager);
            assert_eq!(ui.manager.selected, 1);
            ui.manager.confirm_face = ui.current_face().id;
            ui.global.overlay = overlay;
        }
        for button in buttons {
            let out = press(&mut ui, button, 100);
            assert!(events(&out).iter().all(|e| !matches!(e, Event::Key { .. } | Event::Channel { .. })), "{overlay:?} {button:?}");
        }
        let out = apply(&mut ui, Control::Wheel(Wheel::Cw), 200);
        assert_eq!(out.deck_events().count(), 0, "{overlay:?}");
        assert_eq!(ui.current_face().template, Template::Omarchy, "{overlay:?}: still the same face");
        assert_ne!(ui.global.overlay, Overlay::None, "{overlay:?}: still captured");
        let out = apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Press), 300);
        assert_eq!(
            events(&out),
            [Event::Ptt { act: 1, agent: 3, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }],
            "{overlay:?}"
        );
        apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Release), 400);
    }
    // Naming owns PTT.
    let mut ui = hosted();
    ui.global.overlay = Overlay::Naming;
    let out = apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Press), 300);
    assert_eq!(events(&out), [Event::Ptt { act: 1, agent: 0, ctx: PttCtx::Name, card: 0, abort: false, press: 1, }]);
    assert!(crate::ui::overlay::ptt_visible(&ui));
}

#[test]
fn ptt_from_every_face_talks_to_the_active_channel() {
    for template in [Template::Agents, Template::Omarchy, Template::Macropad, Template::Usage, Template::Settings] {
        let mut ui = hosted();
        ui.on_face(template);
        ui.global.active_channel = 2;
        ui.agents.highlight = 4; // Only the Agents roster looks at this.
        let out = apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Press), 100);
        let expected = if template == Template::Agents { 4 } else { 2 };
        assert!(
            events(&out).contains(&Event::Ptt { act: 1, agent: expected, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }),
            "{template:?}: {:?}",
            events(&out)
        );
        assert!(crate::ui::overlay::ptt_visible(&ui) && crate::ui::overlay::opaque(&ui), "{template:?}");
        let out = apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Release), 900);
        assert_eq!(
            events(&out),
            [Event::Ptt { act: 0, agent: expected, ctx: PttCtx::Talk, card: 0, abort: false, press: 1, }],
            "{template:?}"
        );
        assert_eq!(ui.current_face().template, template, "{template:?}: the face is back");
    }
}

#[test]
fn a_session_from_hello_to_answer() {
    let mut ui = fresh();
    assert!(ui.deck.demo_active());
    let out = host(&mut ui, &hello(42, 19 * 60 + 42), 1000);
    assert_eq!(events(&out), [Event::Hello]);
    assert!(!ui.deck.demo_active() && ui.deck.host_present && ui.deck.order_len == 0);
    assert_eq!(ui.deck.clock_minute, 1182);
    host(&mut ui, &agent(2, AgentState::Working, "web-ui"), 1100);
    host(&mut ui, &agent(5, AgentState::Idle, "research"), 1200);
    assert_eq!(ui.agents.highlight, 2, "the first agent is highlighted");
    // A question arrives while the user looks at another face: a toast.
    ui.on_face(Template::Usage);
    host(&mut ui, &agent(5, AgentState::Question, "research"), 5000);
    let out = host(&mut ui, &card(5, 7, CardKind::Question, 2), 5001);
    assert!(out.wake && ui.notify.toast.is_some());
    assert_eq!(ui.deck.order[..2], [2, 5], "not re-sorted yet");
    tick(&mut ui, 7000);
    assert_eq!(ui.deck.order[..2], [5, 2], "re-sorted once quiet");
    // The toast is tapped: to the card, guarded, no `channel`.
    let touch = |ui: &mut UiState, phase, at| apply(ui, Control::Touch(Touch { phase, x: 240, y: 40 }), at);
    touch(&mut ui, TouchPhase::Down, 7100);
    let out = touch(&mut ui, TouchPhase::Up, 7150);
    assert_eq!(events(&out), [Event::Face { id: 1 }]);
    assert_eq!((ui.current_face().template, ui.agents.view, ui.agents.agent), (Template::Agents, AgentsView::Card, 5));
    assert_eq!(ui.agents.choice, Some(0));
    assert_eq!(press(&mut ui, Button::Enter, 7200).deck_events().count(), 0, "guarded");
    apply(&mut ui, Control::Wheel(Wheel::Ccw), 7700);
    let out = press(&mut ui, Button::Enter, 7800);
    assert_eq!(events(&out), [Event::Answer { agent: 5, card: 7, choice: 1 }]);
    assert_eq!(ui.agents.view, AgentsView::Channel);
    assert_eq!(ui.deck.card(5).unwrap().pending_answer, Some(1));
    // The host resolves it.
    host(&mut ui, &clear(5, 7), 8000);
    host(&mut ui, &agent(5, AgentState::Working, "research"), 8001);
    assert!(ui.deck.card(5).is_none() && ui.deck.notice_count() == 0);
    tick(&mut ui, 14_000);
    assert!(ui.notify.toast.is_none(), "acknowledged in time");
    assert_eq!(ui.agents.view, AgentsView::Channel);
    // Heartbeats keep it alive; silence does not.
    host(&mut ui, &hello(42, 1183), 15_000);
    assert!(ui.deck.host_present);
    let out = tick(&mut ui, 15_000 + crate::deck::model::HEARTBEAT_MS);
    assert!(!ui.deck.host_present && out.dirty);
    assert!(ui.deck.agent(5).unwrap().stale && !ui.deck.demo_active());
    let out = press(&mut ui, Button::Enter, 31_000);
    assert_eq!(out.deck_events().count(), 0, "offline: nothing goes out");
}

#[test]
fn link_drop_and_reconnect_resume_the_same_daemon_without_a_resync() {
    let mut ui = hosted();
    press(&mut ui, Button::Enter, 100);
    assert_eq!(ui.agents.view, AgentsView::Channel);
    ui.set_connected(false, &mut Update::new());
    assert!(ui.deck.agent(1).unwrap().stale && !ui.deck.host_present);
    assert_eq!(ui.agents.view, AgentsView::Channel, "the view survives");
    ui.set_connected(true, &mut Update::new());
    let out = host(&mut ui, &hello(1, 0), 5000);
    assert_eq!(events(&out), [Event::Hello]);
    assert!(!ui.deck.syncing && !ui.deck.agent(1).unwrap().stale);
    assert_eq!(ui.agents.view, AgentsView::Channel);
    // A different daemon: stale until it confirms, and the old view falls
    // back once its agent is purged.
    host(&mut ui, &hello(2, 0), 6000);
    assert!(ui.deck.syncing && ui.deck.agent(1).unwrap().stale);
    host(&mut ui, &agent(2, AgentState::Working, "web-ui"), 6100);
    host(&mut ui, &HostMsg::Synced, 6200);
    assert!(ui.deck.agent(1).is_none() && ui.deck.agent(2).is_some());
    assert_eq!((ui.agents.view, ui.agents.highlight), (AgentsView::Roster, 2));
}

#[test]
fn a_teardown_never_needs_more_than_eight_deck_events() {
    // The busiest transition: a PTT press with the finger down on a face,
    // three keys held and a channel change: channel + ptt + releases.
    let mut ui = hosted();
    ui.on_face(Template::Macropad);
    ui.global.active_channel = 3;
    apply(&mut ui, Control::Touch(Touch { phase: TouchPhase::Down, x: 100, y: 100 }), 100);
    for button in [Button::Enter, Button::Left, Button::Right, Button::Cancel] {
        apply(&mut ui, Control::Button(button, Edge::Press), 110);
    }
    let out = apply(&mut ui, Control::Button(Button::PushToTalk, Edge::Press), 200);
    assert!(!out.overflow);
    assert!(out.deck_events().count() <= 8);
    let mut out = Update::new();
    ui.set_connected(false, &mut out);
    assert_eq!(out.deck_events().count(), 0);
}
