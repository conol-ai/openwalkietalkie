//! Seeded random walk over everything that can change the screen: controls
//! (touches included), valid host messages, time, link and battery changes,
//! face jumps and direct pokes at overlay state. After EVERY step the partial
//! refresh computed from the two signatures must equal a complete redraw, for
//! the logical and the panel image. This is what catches a `signature` that
//! forgot one of its `draw`'s inputs.
//!
//!   OWT_FUZZ_STEPS=20000 OWT_FUZZ_SEED=7 cargo test ... fuzz
//!
//! A failure prints the seed, the step number and the step itself.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    control::{Button, Control, Edge, Touch, TouchPhase, Wheel},
    deck::msg::*,
    deck::text::Text,
    ui::state::{AgentsView, Overlay, PttSession, Toast, ToastKind},
    ui::Update,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u32) as usize]
    }
    fn text<const N: usize>(&mut self) -> Text<N> {
        const WORDS: [&str; 12] = [
            "staging", "refactor", "m\u{fc}nchen", "\u{2014}", "\u{b7}", "deploy", "tests",
            "a", "WWWWWWWWWWWW", "\u{2026}", "ok?", "\u{65e5}\u{672c}",
        ];
        let mut text = Text::new();
        for _ in 0..self.below(9) {
            text.push_str(self.pick(&WORDS));
            text.push_char(' ');
        }
        text
    }
}

fn control(rng: &mut Rng, last_touch: &mut (u16, u16)) -> Control {
    match rng.below(10) {
        0..=2 => Control::Wheel(rng.pick(&[Wheel::Cw, Wheel::Ccw])),
        3..=6 => Control::Button(
            rng.pick(&Button::ALL),
            rng.pick(&[Edge::Press, Edge::Release]),
        ),
        _ => {
            let phase = rng.pick(&[
                TouchPhase::Down,
                TouchPhase::Move,
                TouchPhase::Move,
                TouchPhase::Up,
                TouchPhase::Up,
                TouchPhase::Cancel,
            ]);
            if phase == TouchPhase::Down || rng.chance(30) {
                *last_touch = (rng.below(480) as u16, rng.below(480) as u16);
            } else if phase == TouchPhase::Move {
                last_touch.0 = (last_touch.0 + rng.below(40) as u16).min(479);
                last_touch.1 = (last_touch.1 + rng.below(60) as u16).min(479);
            }
            Control::Touch(Touch {
                phase,
                x: last_touch.0,
                y: last_touch.1,
            })
        }
    }
}

/// A message the parser could have produced: every id and count in range.
fn host_msg(rng: &mut Rng, out: &mut HostMsg) {
    let agent = 1 + rng.below(8) as u8;
    *out = match rng.below(14) {
        0 => HostMsg::Hello {
            v: 1,
            sid: rng.below(2),
            minute_of_day: rng.chance(70).then(|| rng.below(1440) as u16),
        },
        1 => HostMsg::Synced,
        2..=4 => fuzz_agent(HostMsg::Agent {
            id: agent,
            gone: rng.chance(15),
            kind: rng.chance(60).then(|| {
                rng.pick(&[AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini, AgentKind::Other])
            }),
            state: rng.chance(70).then(|| {
                rng.pick(&[
                    AgentState::Working,
                    AgentState::Question,
                    AgentState::Ready,
                    AgentState::Error,
                    AgentState::Idle,
                ])
            }),
            name: rng.chance(60).then(|| rng.text()),
            project: rng.chance(40).then(|| rng.pick(&[Text::from_str("cohdl"), Text::from_str("zeta"), Text::new()])),
            title: rng.chance(60).then(|| rng.text()),
            since_s: rng.chance(40).then(|| rng.below(100_000)),
            // Derived, not drawn: the seeded sequence stays what it was.
            task: None,
            subs: None,
        }),
        5 | 6 => {
            let choice_count = rng.below(5) as u8;
            let mut choices = [Text::new(); 4];
            for choice in choices.iter_mut().take(choice_count as usize) {
                *choice = rng.text();
            }
            HostMsg::Card {
                agent,
                id: rng.below(5) as u16,
                clear: rng.chance(25),
                kind: rng.pick(&[CardKind::Question, CardKind::Ready]),
                text: rng.text(),
                choices,
                choice_count,
                recommended: (choice_count > 0 && rng.chance(50))
                    .then(|| rng.below(choice_count as u32) as u8),
            }
        }
        7 => HostMsg::Reply {
            agent,
            text: rng.text(),
                    rev: 1,
        },
        8 => HostMsg::Transcript {
            agent,
            text: rng.text(),
            last: rng.chance(30), press: None, failed: false, channel_tip: false, rec: false,
        },
        9 => HostMsg::Focus {
            agent,
            ack: rng.chance(50).then(|| rng.below(8) as u16),
        },
        10 => {
            let window_count = rng.below(7) as u8;
            let mut windows = [WindowRow::EMPTY; 6];
            for window in windows.iter_mut().take(window_count as usize) {
                window.tone = rng.pick(&[Tone::Blue, Tone::Amber, Tone::Green, Tone::Red, Tone::Gray]);
                window.title = rng.text();
            }
            let count = rng.below(10) as u8;
            HostMsg::Workspaces {
                active: rng.below(count as u32 + 1) as u8,
                count,
                selected: rng.below(window_count.max(1) as u32) as u8,
                windows,
                window_count,
            }
        }
        11 => {
            let mut keys = [MacroKey::EMPTY; 9];
            for key in &mut keys {
                key.present = rng.chance(80);
                key.label = rng.text();
                key.binding = rng.pick(&[
                    Binding::None,
                    Binding::Left,
                    Binding::Right,
                    Binding::Enter,
                    Binding::Cw,
                    Binding::Ccw,
                    Binding::ShiftLeft,
                    Binding::ShiftRight,
                    Binding::ShiftEnter,
                ]);
            }
            HostMsg::Macro {
                face: 1 + rng.below(8) as u8,
                source: rng.chance(70).then(|| rng.text()),
                keys,
            }
        }
        12 => {
            let count = rng.below(7) as u8;
            let mut items = [UsageItem::EMPTY; 6];
            for item in items.iter_mut().take(count as usize) {
                item.name = rng.text();
                item.percent = rng.below(131) as u8;
                item.reset = rng.text();
                item.tone = rng.chance(30).then(|| rng.pick(&[Tone::Blue, Tone::Red, Tone::Gray]));
            }
            HostMsg::Usage {
                period: rng.text(),
                items,
                count,
            }
        }
        _ => match rng.below(3) {
            0 => {
                // Agents first, Settings last, unique ids, at most 3 macropads.
                let middle = rng.below(7) as usize;
                let mut list = [FaceEntry::EMPTY; 8];
                list[0] = FaceEntry::new(1, Template::Agents, "Agents");
                let mut macropads = 0;
                for slot in 1..=middle {
                    let mut template =
                        rng.pick(&[Template::Omarchy, Template::Macropad, Template::Usage, Template::Timer]);
                    if template == Template::Macropad {
                        macropads += 1;
                        if macropads > 3 {
                            template = Template::Usage;
                        }
                    }
                    list[slot] = FaceEntry {
                        id: slot as u8 + 1,
                        template,
                        name: rng.text(),
                    };
                }
                list[middle + 1] = FaceEntry::new(8, Template::Settings, "Settings");
                HostMsg::Faces {
                    list,
                    count: middle as u8 + 2,
                }
            }
            1 => HostMsg::Name { text: rng.text() },
            _ => HostMsg::Set {
                key: rng.pick(&[
                    SettingKey::Speaker,
                    SettingKey::Mic,
                    SettingKey::Announce,
                    SettingKey::Chime,
                    SettingKey::AutoJump,
                    SettingKey::Brightness,
                    SettingKey::IdleHome,
                    SettingKey::Demo,
                ]),
                value: rng.below(200) as u16,
            },
        },
    };
}

/// Set pub view fields the way previews and future grammars do, then
/// `invalidate`, which is the contract for direct pokes.
fn poke(rng: &mut Rng, ui: &mut UiState) {
    let agents: Vec<u8> = ui.deck.agents.iter().filter(|a| a.present).map(|a| a.id).collect();
    match rng.below(8) {
        0 => {
            ui.global.overlay = rng.pick(&[
                Overlay::None,
                Overlay::None,
                Overlay::Drawer,
                Overlay::Manager,
                Overlay::Gallery,
                Overlay::Naming,
                Overlay::ConfirmRemove,
            ])
        }
        1 if !agents.is_empty() => {
            let agent = rng.pick(&agents);
            ui.agents.view = rng.pick(&[AgentsView::Roster, AgentsView::Channel, AgentsView::Card]);
            ui.agents.agent = agent;
            ui.agents.highlight = rng.pick(&agents);
            ui.agents.choice = rng.chance(70).then(|| rng.below(4) as u8);
            ui.global.active_channel = rng.pick(&agents);
        }
        2 => {
            ui.global.ptt = (rng.chance(50) && !agents.is_empty()).then(|| {
                PttSession::new(
                    rng.pick(&agents),
                    rng.pick(&[PttCtx::Talk, PttCtx::Answer, PttCtx::Name]),
                    rng.below(4) as u16,
                    ui.now_ms,
                )
            })
        }
        3 => {
            ui.notify.toast = rng.chance(60).then(|| {
                let agent = 1 + rng.below(8) as u8;
                let card = rng.below(5) as u16;
                match rng.below(4) {
                    0 => Toast::message(ToastKind::NoChannel, 0, 0, ui.now_ms),
                    1 => Toast::message(ToastKind::NotDelivered, agent, card, ui.now_ms),
                    2 => Toast::card(agent, card, CardKind::Question, ui.now_ms),
                    _ => Toast::card(agent, card, CardKind::Ready, ui.now_ms),
                }
            })
        }
        4 => {
            ui.settings.panel = rng.below(6) as u8;
            ui.settings.row = rng.below(4) as u8;
        }
        5 => {
            ui.manager.selected = rng.below(ui.global.strip_len as u32 + 1) as u8;
            ui.manager.grabbed = rng.chance(30);
            ui.manager.first_visible = rng.below(5) as u8;
            ui.manager.gallery_sel = rng.below(4) as u8;
            ui.manager.confirm_face = 1 + rng.below(8) as u8;
            ui.manager.naming_text = rng.text();
        }
        6 => {
            ui.macropad.face = ui.current_face().id;
            ui.macropad.down = rng.below(512) as u16;
            ui.omarchy.selected_window = rng.below(6) as u8;
            ui.usage.top_row = rng.below(3) as u8;
            ui.usage.updating = rng.chance(50);
        }
        _ => {
            for notice in &mut ui.deck.notices {
                notice.used = rng.chance(40);
                notice.in_drawer = rng.chance(60);
                notice.kind = rng.pick(&[CardKind::Question, CardKind::Ready]);
                notice.agent = 1 + rng.below(8) as u8;
            }
        }
    }
    ui.invalidate();
}

/// The task and the count of a random agent line, from what it already
/// drew: the title as the task on even ids, the age as the count.
fn fuzz_agent(mut msg: HostMsg) -> HostMsg {
    if let HostMsg::Agent { id, title, since_s, task, subs, .. } = &mut msg {
        *task = title.filter(|_| *id % 2 == 0);
        *subs = since_s.map(|s| (s % 120) as u8);
    }
    msg
}

#[test]
fn partial_refresh_equals_a_full_redraw_after_every_random_step() {
    let env = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let steps = env("OWT_FUZZ_STEPS", 3000);
    let seed = env("OWT_FUZZ_SEED", 0x00C0_DEC5_EED5);
    let mut rng = Rng(seed | 1);
    let mut previous = ui();
    let mut next = ui();
    let mut screen = Screens::new(&previous);
    let mut msg = Box::new(HostMsg::None);
    let mut now = 0u32;
    let mut last_touch = (240, 240);
    for step in 0..steps {
        let mut out = Update::new();
        let what = match rng.below(100) {
            0..=54 => {
                let control = control(&mut rng, &mut last_touch);
                now = now.wrapping_add(rng.below(120));
                next.apply(control, now, &mut out);
                format!("{control:?}")
            }
            55..=74 => {
                host_msg(&mut rng, &mut msg);
                next.host(&msg, now, &mut out);
                format!("{msg:?}")
            }
            75..=89 => {
                now = now.wrapping_add(if rng.chance(10) {
                    rng.below(130_000)
                } else {
                    1 + rng.below(700)
                });
                let idle = if rng.chance(20) { rng.below(200_000) } else { 0 };
                next.tick(now, idle, rng.chance(90), &mut out);
                format!("tick {now} idle {idle}")
            }
            90..=92 => {
                let template = rng.pick(&Template::ALL);
                next.on_face(template);
                format!("on_face {template:?}")
            }
            93..=96 => {
                poke(&mut rng, &mut next);
                String::from("poke")
            }
            97 => {
                let connected = rng.chance(60);
                next.set_connected(connected, &mut out);
                format!("connected {connected}")
            }
            98 => {
                next.frame_shown(now);
                String::from("frame shown")
            }
            _ => {
                let percent = rng.below(101) as u8;
                next.update_battery(BatteryState {
                    estimated_percent: rng.chance(90).then_some(percent),
                    charge: rng.pick(&[ChargeState::Battery, ChargeState::Charging, ChargeState::Full]),
                    ..BatteryState::UNKNOWN
                });
                format!("battery {percent}")
            }
        };
        // Shown only when an assertion below fails.
        eprintln!("seed {seed} step {step}: {what}");
        assert!(!out.overflow || out.deck_events().count() == 8);
        update(&mut screen, &previous, &next);
        next.copy_into(&mut previous);
    }
}
