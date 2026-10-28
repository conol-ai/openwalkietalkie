//! Agents face and the LISTENING overlay (`ui::faces::agents`,
//! `ui::overlay::ptt`): partial == full through scripted scenarios of every
//! view, lane-sized damage, the corner mask, hint bars that fit, no ellipsis
//! on the mockup scenes, layout rects tied to drawn outlines, and the origin
//! sweep.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    deck::msg::*,
    deck::text::Text,
    ui::{
        faces::agents as face,
        font::{self, Font},
        kit,
        overlay::ptt,
        state::{AgentsView, PttSession, Toast},
        theme, Rect, Update,
    },
};

const FULL: usize = 480 * 480;
const LONG_REPLY: &str = "The staging database is three migrations behind main, so the integration suite fails on every branch that touches the accounts table. I can run the pending migrations there first and rerun the suite, or skip them and mark the failing cases as known.";
const LONG_QUESTION: &str = "Tests fail on main because the staging database is three migrations behind. Run them there first?";

/// Demo data with a host pretending to be there, as the previews do.
fn scene(setup: impl FnOnce(&mut UiState)) -> Box<UiState> {
    let mut ui = ui();
    ui.deck.host_present = true;
    ui.connected = true;
    setup(&mut ui);
    ui.invalidate();
    ui
}

fn card(ui: &mut UiState, agent: u8, choice: Option<u8>) {
    ui.agents.view = AgentsView::Card;
    ui.agents.agent = agent;
    ui.agents.highlight = agent;
    ui.agents.choice = choice;
    ui.global.active_channel = agent;
}

fn channel(ui: &mut UiState, agent: u8) {
    ui.agents.view = AgentsView::Channel;
    ui.agents.agent = agent;
    ui.agents.highlight = agent;
    ui.global.active_channel = agent;
}

fn listening(ui: &mut UiState, agent: u8, ctx: PttCtx) {
    ui.global.active_channel = agent;
    ui.global.ptt = Some(PttSession::new(agent, ctx, 0, ui.now_ms));
    ui.deck.transcript.set(crate::deck::demo::TRANSCRIPT);
}

fn host(ui: &mut UiState, msg: HostMsg) {
    let mut out = Update::new();
    let msg = Box::new(msg);
    ui.host(&msg, ui.now_ms, &mut out);
}

/// An `agent` push that changes only `state`, `title` and/or `since`.
fn agent_msg(id: u8, state: Option<AgentState>, title: Option<&str>, since_s: Option<u32>) -> HostMsg {
    HostMsg::Agent { id, gone: false, kind: None, state, name: None, project: None, title: title.map(Text::from_str), since_s }
}

fn gone_msg(id: u8) -> HostMsg {
    HostMsg::Agent { id, gone: true, kind: None, state: None, name: None, project: None, title: None, since_s: None }
}

fn clear_msg(agent: u8, id: u16, kind: CardKind) -> HostMsg {
    HostMsg::Card { agent, id, clear: true, kind, text: Text::new(), choices: [Text::new(); 4], choice_count: 0, recommended: None }
}

/// A window of the host's wrapped lines: the 12 from `first` of `total`,
/// in the font the host measured them with.
fn window_msg(agent: u8, id: u16, first: u16, total: u16) -> HostMsg {
    let mut lines = [Text::new(); DOC_LINES];
    for (slot, line) in lines.iter_mut().enumerate() {
        line.set(&std::format!("wrapped body line {}", first as usize + slot));
    }
    HostMsg::DocText { agent, doc: DocKind::Card, id, first, total, font: DocFont::Name, lines, line_count: DOC_LINES as u8 }
}

fn reply_window_msg(agent: u8, rev: u16, first: u16, total: u16) -> HostMsg {
    let mut lines = [Text::new(); DOC_LINES];
    for (slot, line) in lines.iter_mut().enumerate() {
        line.set(&std::format!("wrapped reply line {}", first as usize + slot));
    }
    HostMsg::DocText { agent, doc: DocKind::Reply, id: rev, first, total, font: DocFont::Row, lines, line_count: DOC_LINES as u8 }
}

fn card_msg(agent: u8, id: u16, kind: CardKind, choices: &[&str], recommended: Option<u8>) -> HostMsg {
    let mut texts = [Text::new(); 4];
    for (slot, choice) in choices.iter().enumerate().take(4) {
        texts[slot].set(choice);
    }
    HostMsg::Card {
        agent,
        id,
        clear: false,
        kind,
        text: Text::from_str(LONG_QUESTION),
        choices: texts,
        choice_count: choices.len().min(4) as u8,
        recommended,
    }
}

/// The HID link dropped: the deck goes stale and every hold, the PTT
/// session included, vanishes.
fn offline(ui: &mut UiState) {
    ui.deck.settings.demo = false;
    let mut out = Update::new();
    ui.set_connected(false, &mut out);
    ui.deck.link_lost();
}

/// The deck daemon fell silent while the link stayed up: what a PTT session
/// without a host looks like.
fn host_lost(ui: &mut UiState) {
    ui.deck.settings.demo = false;
    ui.deck.link_lost();
}

/// Apply each step to a copy of `start`, replaying the damage between the
/// states; returns the pixels each step transferred.
fn walk(start: &UiState, steps: &[(&str, &dyn Fn(&mut UiState))]) -> Vec<usize> {
    let mut previous = Box::new(*start);
    let mut next = Box::new(*start);
    let mut screen = Screens::new(&previous);
    let mut transferred = Vec::new();
    for (name, step) in steps {
        step(&mut next);
        next.invalidate();
        eprintln!("step: {name}");
        transferred.push(update(&mut screen, &previous, &next));
        next.copy_into(&mut previous);
    }
    transferred
}

fn pixel(frame: &[u8], x: i32, y: i32) -> u16 {
    let at = (y as usize * 480 + x as usize) * 2;
    u16::from_le_bytes([frame[at], frame[at + 1]])
}

fn raw(color: u32) -> u16 {
    kit::rgb(color).into_storage()
}

fn count_in(frame: &[u8], rect: Rect, color: u32) -> usize {
    let wanted = raw(color);
    (rect.top()..rect.bottom())
        .flat_map(|y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(frame, x, y) == wanted)
        .count()
}

fn lit_in(frame: &[u8], rect: Rect) -> usize {
    (rect.top()..rect.bottom())
        .flat_map(|y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(frame, x, y) != 0)
        .count()
}

/// Which of r, g, b leads, and by how much (8-bit scale), of an RGB565 pixel.
fn hue(p: u16) -> (usize, i32) {
    let rgb = [((p >> 11) & 31) as i32 * 255 / 31, ((p >> 5) & 63) as i32 * 255 / 63, (p & 31) as i32 * 255 / 31];
    let (max, min) = (*rgb.iter().max().unwrap(), *rgb.iter().min().unwrap());
    (rgb.iter().position(|&c| c == max).unwrap(), max - min)
}

/// Pixels of `rect` visibly tinted like `color`: anti-aliased text has few
/// pixels of exactly its colour, but every stem leans the same way.
fn tinted(frame: &[u8], rect: Rect, color: u32) -> usize {
    let (lead, _) = hue(raw(color));
    (rect.top()..rect.bottom())
        .flat_map(|y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let (l, spread) = hue(pixel(frame, x, y));
            l == lead && spread > 40
        })
        .count()
}

/// Brightest channel value in `rect`.
fn peak(frame: &[u8], rect: Rect) -> i32 {
    (rect.top()..rect.bottom())
        .flat_map(|y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .map(|(x, y)| {
            let p = pixel(frame, x, y);
            (((p >> 11) & 31) as i32 * 255 / 31).max(((p >> 5) & 63) as i32 * 255 / 63).max((p & 31) as i32 * 255 / 31)
        })
        .max()
        .unwrap_or(0)
}

/// Every Agents view and LISTENING overlay variant, as (name, state).
fn scenes() -> Vec<(&'static str, Box<UiState>)> {
    vec![
        ("roster", scene(|_| {})),
        ("roster-active-pressed", scene(|ui| {
            ui.global.active_channel = 3;
            ui.agents.pressed = Some(2);
            ui.agents.top_row = 1;
        })),
        // The second project's page: RIGHT from the mockup's roster.
        ("roster-page2", scene(|ui| {
            use crate::control::{Button, Control, Edge};
            let mut out = Update::new();
            ui.apply(Control::Button(Button::Right, Edge::Press), 100, &mut out);
            ui.apply(Control::Button(Button::Right, Edge::Release), 110, &mut out);
        })),
        ("roster-offline", scene(offline)),
        ("channel", scene(|ui| channel(ui, 3))),
        ("channel-question2", scene(|ui| {
            ui.deck.agents[0].card.choice_count = 2;
            channel(ui, 1);
        })),
        ("channel-question3", scene(|ui| channel(ui, 1))),
        ("channel-ready", scene(|ui| channel(ui, 2))),
        ("channel-long-reply", scene(|ui| {
            channel(ui, 3);
            ui.deck.agents[2].last_reply.set(LONG_REPLY);
            ui.agents.reply_top = 2;
        })),
        ("card-question", scene(|ui| card(ui, 1, Some(0)))),
        ("card-question-sent", scene(|ui| {
            card(ui, 1, Some(0));
            ui.deck.agents[0].card.pending_answer = Some(0);
        })),
        ("card-question-offline", scene(|ui| {
            card(ui, 1, Some(1));
            offline(ui);
        })),
        ("card-question-long", scene(|ui| {
            host(ui, card_msg(1, 9, CardKind::Question, &["Run on staging", "Skip migrations", "Abort task", "Ask me later"], Some(2)));
            card(ui, 1, Some(3));
            ui.agents.choice_top = 1;
            ui.agents.pressed = Some(3);
        })),
        ("card-question-window", scene(|ui| {
            host(ui, card_msg(1, 9, CardKind::Question, &["Run on staging", "Skip migrations", "Abort task"], Some(0)));
            host(ui, window_msg(1, 9, 4, 40));
            card(ui, 1, Some(0));
            ui.agents.question_top = 6;
        })),
        ("card-ready", scene(|ui| card(ui, 2, None))),
        ("card-ready-step", scene(|ui| {
            card(ui, 2, Some(1));
            ui.deck.agents[1].last_reply.set(LONG_REPLY);
        })),
        ("card-none", scene(|ui| {
            card(ui, 3, None);
        })),
        ("channel-gone", scene(|ui| {
            channel(ui, 7);
        })),
        ("ptt-roster", scene(|ui| listening(ui, 2, PttCtx::Talk))),
        ("ptt-channel-tip", scene(|ui| {
            listening(ui, 1, PttCtx::Talk);
            ui.deck.transcript_channel_tip = true;
        })),
        ("ptt-channel-tip-pending", scene(|ui| {
            listening(ui, 1, PttCtx::Talk);
            ui.deck.transcript_channel_tip = true;
            ui.global.ptt_pending = ui.global.ptt.take();
        })),
        ("ptt-answer", scene(|ui| {
            card(ui, 1, Some(0));
            listening(ui, 1, PttCtx::Answer);
            ui.now_ms = 1234;
        })),
        ("ptt-naming", scene(|ui| {
            ui.manager.naming_template = Template::Macropad;
            listening(ui, 0, PttCtx::Name);
            ui.deck.transcript.clear();
        })),
        ("ptt-no-host", scene(|ui| {
            host_lost(ui);
            listening(ui, 2, PttCtx::Talk);
        })),
        ("ptt-omarchy-final", scene(|ui| {
            ui.on_face(Template::Omarchy);
            listening(ui, 3, PttCtx::Talk);
            ui.deck.transcript_final = true;
        })),
        ("ptt-long-transcript", scene(|ui| {
            listening(ui, 2, PttCtx::Talk);
            ui.deck.transcript.set(LONG_REPLY);
        })),
    ]
}

#[test]
fn roster_partial_refresh_matches_full_and_repaints_by_row() {
    let start = scene(|_| {});
    let moved = walk(&start, &[
        ("highlight down", &|ui| ui.agents.highlight = 2),
        ("active channel", &|ui| ui.global.active_channel = 3),
        ("pressed", &|ui| ui.agents.pressed = Some(2)),
        ("released", &|ui| ui.agents.pressed = None),
        ("title push", &|ui| {
            host(ui, agent_msg(3, None, Some("Linking main.elf"), None))
        }),
        ("state push", &|ui| host(ui, agent_msg(3, Some(AgentState::Ready), None, None))),
        ("age rolls over", &|ui| {
            let mut out = Update::new();
            ui.tick(ui.now_ms + 60_000, 0, true, &mut out);
        }),
        ("scroll the viewport", &|ui| ui.agents.top_row = 1),
        ("agent gone", &|ui| host(ui, gone_msg(4))),
        ("new agent", &|ui| {
            host(ui, HostMsg::Agent {
                id: 6,
                gone: false,
                kind: Some(AgentKind::Other),
                state: Some(AgentState::Working),
                name: Some(Text::from_str("m\u{fc}nchen")),
                project: None,
                title: Some(Text::from_str("Compiling")),
                since_s: Some(3700),
            })
        }),
        ("low battery", &|ui| {
            ui.update_battery(BatteryState {
                estimated_percent: Some(9),
                charge: ChargeState::Battery,
                ..BatteryState::UNKNOWN
            });
        }),
        ("clock", &|ui| {
            ui.update_battery(BatteryState::UNKNOWN);
            ui.deck.clock_valid = true;
            ui.deck.clock_minute = 100;
        }),
        ("offline", &|ui| offline(ui)),
        ("demo again", &|ui| ui.deck.settings.demo = true),
    ]);
    // A highlight move or an active-channel change repaints two rows, not the frame.
    assert!(moved[0] < FULL / 2, "highlight: {}", moved[0]);
    assert!(moved[1] < FULL / 3, "active bar: {}", moved[1]);
    assert!(moved[4] < FULL / 3, "one title: {}", moved[4]);
}

#[test]
fn a_reply_uses_the_lower_screen_and_reflows_when_card_controls_change() {
    let start = scene(|ui| {
        channel(ui, 3);
        host(ui, HostMsg::Reply { agent: 3, text: Text::from_str(LONG_REPLY), rev: 1 });
        host(ui, reply_window_msg(3, 1, 0, 40));
    });
    let frame = full(&start);
    let bottom = Rect::new(38, 300, 404, layout::CHANNEL_CONTENT_BOTTOM - 300);
    assert!(lit_in(&frame, bottom) > 300, "the bottom of the reading area contains reply text");
    walk(&start, &[
        ("scroll to the end of the held window", &|ui| ui.agents.reply_top = u16::MAX),
        ("question takes its action space", &|ui| host(ui, card_msg(3, 5, CardKind::Question, &["Allow", "Deny"], None))),
        ("ready card replaces the question", &|ui| host(ui, card_msg(3, 6, CardKind::Ready, &[], None))),
        ("acknowledge frees the button area", &|ui| ui.deck.agents[2].card.acknowledged = true),
        ("clear frees the summary area", &|ui| host(ui, clear_msg(3, 6, CardKind::Ready))),
        ("scroll back in the expanded reply", &|ui| ui.agents.reply_top = 0),
    ]);
}

#[test]
fn channel_partial_refresh_matches_full_through_cards_replies_and_touches() {
    let start = scene(|ui| channel(ui, 3));
    let moved = walk(&start, &[
        ("reply push", &|ui| host(ui, HostMsg::Reply { agent: 3, text: Text::from_str(LONG_REPLY) , rev: 1 })),
        // The host laid the reply out and sent a window: the lane draws the
        // lines it was handed and the bar appears in the gutter. Scrolling it
        // is what catches a bar drawn outside the region its signature hashes
        // — damage tracking would never mark those columns dirty, so the thumb
        // would stand still while the text moved under it.
        ("a reply window arrives", &|ui| host(ui, reply_window_msg(3, 1, 0, 40))),
        ("scroll the reply", &|ui| ui.agents.reply_top = 4),
        ("the window moves under the offset", &|ui| host(ui, reply_window_msg(3, 1, 4, 40))),
        ("scroll to the end of what is held", &|ui| ui.agents.reply_top = 9),
        ("a new reply forgets the window", &|ui| host(ui, HostMsg::Reply { agent: 3, text: Text::from_str("Short."), rev: 2 })),
        ("scroll reply", &|ui| ui.agents.reply_top = 1),
        ("scroll past the end", &|ui| ui.agents.reply_top = 40),
        ("binary question arrives", &|ui| {
            ui.agents.reply_top = 0;
            host(ui, card_msg(3, 5, CardKind::Question, &["Run on staging", "Skip migrations"], Some(0)))
        }),
        ("press the first button", &|ui| ui.agents.pressed = Some(0)),
        ("press the second", &|ui| ui.agents.pressed = Some(1)),
        ("release", &|ui| ui.agents.pressed = None),
        ("three choices", &|ui| host(ui, card_msg(3, 6, CardKind::Question, &["a", "b", "c"], None))),
        ("answer sent", &|ui| ui.deck.agents[2].card.pending_answer = Some(1)),
        ("ready card", &|ui| host(ui, card_msg(3, 7, CardKind::Ready, &["Open a pull request"], None))),
        ("acknowledged", &|ui| ui.deck.agents[2].card.acknowledged = true),
        ("card cleared", &|ui| host(ui, clear_msg(3, 7, CardKind::Ready))),
        ("state and age", &|ui| {
            host(ui, agent_msg(3, Some(AgentState::Question), None, Some(7200)))
        }),
        ("sibling", &|ui| channel(ui, 1)),
        ("stale", &|ui| offline(ui)),
        ("agent gone", &|ui| {
            ui.deck.settings.demo = true;
            host(ui, gone_msg(1));
            channel(ui, 1);
        }),
    ]);
    // Indices follow the walk above. Scrolling a reply the host laid out must
    // repaint the lane, not the frame — and it must repaint *something*, or the
    // thumb is not moving with the text.
    assert!(moved[2] > 0, "windowed reply scroll repainted nothing");
    assert!(moved[2] <= 416 * (layout::channel_reply(None).height() + font::top_overhang(Font::Row) + 1) as usize, "windowed reply scroll: {}", moved[2]);
    assert!(moved[6] < FULL / 2, "reply scroll: {}", moved[6]);
    assert!(moved[9] < FULL / 4, "button press: {}", moved[9]);
}

#[test]
fn question_card_partial_refresh_matches_full_through_choices_sent_offline_and_scroll() {
    let start = scene(|ui| card(ui, 1, Some(0)));
    let moved = walk(&start, &[
        ("choice down", &|ui| ui.agents.choice = Some(1)),
        ("choice down again", &|ui| ui.agents.choice = Some(2)),
        ("choice out of range", &|ui| ui.agents.choice = Some(9)),
        ("pressed", &|ui| {
            ui.agents.choice = Some(1);
            ui.agents.pressed = Some(2);
        }),
        ("released", &|ui| ui.agents.pressed = None),
        ("sent", &|ui| ui.deck.agents[0].card.pending_answer = Some(1)),
        ("reverted", &|ui| ui.deck.agents[0].card.pending_answer = None),
        ("offline", &|ui| offline(ui)),
        ("back online", &|ui| ui.deck.settings.demo = true),
        ("long question with four choices", &|ui| {
            host(ui, card_msg(1, 9, CardKind::Question, &["Run on staging", "Skip migrations", "Abort task", "Ask me later"], Some(2)));
            ui.agents.choice = Some(2);
        }),
        ("scroll the choices", &|ui| {
            ui.agents.choice = Some(3);
            ui.agents.choice_top = 1;
        }),
        ("a window of wrapped lines arrives", &|ui| {
            host(ui, card_msg(1, 11, CardKind::Question, &["Run on staging", "Skip migrations"], Some(0)));
            host(ui, window_msg(1, 11, 0, 40));
            ui.agents.view = AgentsView::Card;
            ui.agents.agent = 1;
        }),
        ("scroll the body", &|ui| ui.agents.question_top = 5),
        ("the window moves under the offset", &|ui| host(ui, window_msg(1, 11, 4, 40))),
        ("scroll to the end of what is held", &|ui| ui.agents.question_top = 13),
        ("no choices", &|ui| host(ui, card_msg(1, 10, CardKind::Question, &[], None))),
        ("age", &|ui| {
            let mut out = Update::new();
            ui.tick(ui.now_ms + 61_000, 0, true, &mut out);
        }),
        ("card cleared", &|ui| host(ui, clear_msg(1, 10, CardKind::Question))),
        ("agent gone", &|ui| {
            host(ui, gone_msg(1));
            ui.agents.view = AgentsView::Card;
            ui.agents.agent = 1;
        }),
    ]);
    assert!(moved[0] < FULL / 2, "choice move: {}", moved[0]);
    assert!(moved[3] < FULL / 3, "press: {}", moved[3]);
}

#[test]
fn ready_card_partial_refresh_matches_full_through_steps_and_summary_changes() {
    let start = scene(|ui| card(ui, 2, None));
    let moved = walk(&start, &[
        ("step highlighted", &|ui| ui.agents.choice = Some(0)),
        ("next step", &|ui| ui.agents.choice = Some(1)),
        ("pressed", &|ui| ui.agents.pressed = Some(1)),
        ("released", &|ui| ui.agents.pressed = None),
        ("reply push", &|ui| host(ui, HostMsg::Reply { agent: 2, text: Text::from_str(LONG_REPLY) , rev: 1 })),
        ("empty summary falls back to the title", &|ui| ui.deck.agents[1].card.text.clear()),
        ("four steps", &|ui| {
            host(ui, card_msg(2, 3, CardKind::Ready, &["Run the test suite", "Open a pull request", "Deploy to staging", "Write the changelog"], None));
            ui.agents.choice = Some(3);
            ui.agents.choice_top = 1;
        }),
        ("no steps", &|ui| host(ui, card_msg(2, 4, CardKind::Ready, &[], None))),
        ("offline", &|ui| offline(ui)),
        ("age", &|ui| {
            ui.deck.settings.demo = true;
            let mut out = Update::new();
            ui.tick(ui.now_ms + 120_000, 0, true, &mut out);
        }),
    ]);
    assert!(moved[0] < FULL / 2, "step highlight: {}", moved[0]);
}

#[test]
fn channel_tip_appears_and_clears_with_correct_partial_damage() {
    let start = scene(|ui| listening(ui, 1, PttCtx::Talk));
    walk(&start, &[
        ("inbox hint", &|ui| host(ui, HostMsg::Transcript { agent: 1,
            text: Text::new(), last: false, press: None, failed: false, channel_tip: true })),
        ("transcribing with hint", &|ui| ui.global.ptt_pending = ui.global.ptt.take()),
        ("channel connected", &|ui| host(ui, HostMsg::Transcript { agent: 1,
            text: Text::from_str("Please continue"), last: false, press: None, failed: false, channel_tip: false })),
    ]);
}

#[test]
fn listening_overlay_partial_refresh_matches_full_and_only_the_waveform_ticks() {
    let start = scene(|ui| listening(ui, 2, PttCtx::Talk));
    let moved = walk(&start, &[
        ("phase", &|ui| ui.now_ms += ptt::WAVE_STEP_MS),
        ("phase again", &|ui| ui.now_ms += ptt::WAVE_STEP_MS),
        ("sub-phase", &|ui| ui.now_ms += 1),
        ("transcript grows", &|ui| {
            host(ui, HostMsg::Transcript { agent: 2, text: Text::from_str(LONG_REPLY), last: false, press: None, failed: false, channel_tip: false, })
        }),
        ("final", &|ui| host(ui, HostMsg::Transcript { agent: 2, text: Text::from_str(LONG_REPLY), last: true, press: None, failed: false, channel_tip: false, })),
        ("empty", &|ui| host(ui, HostMsg::Transcript { agent: 2, text: Text::new(), last: false, press: None, failed: false, channel_tip: false, })),
        ("reply changes the context line", &|ui| host(ui, HostMsg::Reply { agent: 2, text: Text::from_str("Done.") , rev: 1 })),
        ("answering", &|ui| listening(ui, 1, PttCtx::Answer)),
        ("naming", &|ui| {
            ui.manager.naming_template = Template::Usage;
            listening(ui, 0, PttCtx::Name);
        }),
        ("agent gone under the session", &|ui| {
            listening(ui, 3, PttCtx::Talk);
            host(ui, gone_msg(3));
            ui.global.ptt = Some(PttSession::new(3, PttCtx::Talk, 0, ui.now_ms));
        }),
        ("host lost", &|ui| host_lost(ui)),
        ("host back", &|ui| host(ui, HostMsg::Hello { v: 1, sid: 7, minute_of_day: Some(5) })),
        ("link lost takes the session", &|ui| offline(ui)),
        ("link back, demo standing in", &|ui| {
            let mut out = Update::new();
            ui.set_connected(true, &mut out);
            ui.deck.settings.demo = true;
        }),
        ("toast under the overlay", &|ui| {
            ui.global.ptt = Some(PttSession::new(1, PttCtx::Talk, 0, ui.now_ms));
            ui.notify.toast = Some(Toast::card(1, 1, CardKind::Question, ui.now_ms));
        }),
        ("over another face", &|ui| ui.on_face(Template::Usage)),
        ("released", &|ui| ui.global.ptt = None),
        ("pressed again on the roster", &|ui| {
            ui.on_face(Template::Agents);
            listening(ui, 2, PttCtx::Talk);
        }),
    ]);
    // A phase step repaints the waveform's bands only; a sub-phase step nothing.
    assert!(moved[0] < FULL / 4, "phase: {}", moved[0]);
    assert_eq!(moved[2], 0, "sub-phase");
    assert!(moved[3] < FULL / 2, "transcript: {}", moved[3]);
}

#[test]
fn every_view_stays_inside_the_corner_mask_and_its_hint_bar_between_32_and_448() {
    let bar = raw(theme::HINT_FILL);
    for (name, ui) in scenes() {
        let frame = full(&ui);
        assert_eq!(lit_outside_safe_area(&frame), None, "{name}");
        let outside = (layout::HINT_BAR.top()..480)
            .flat_map(|y| (0..480).map(move |x| (x, y)))
            .find(|&(x, y)| {
                let p = pixel(&frame, x, y);
                p != 0 && p != bar && p != raw(theme::HAIRLINE) && !(32..448).contains(&x)
            });
        assert_eq!(outside, None, "{name}: hint content outside x in [32, 448)");
        assert!(lit_in(&frame, layout::HINT_CONTENT) > 200, "{name}: an empty hint bar");
    }
    for hints in face::ALL_HINTS {
        assert_eq!(kit::hints_that_fit(hints).0, hints.len(), "{:?}", hints);
    }
}

#[test]
fn a_question_with_hidden_lines_draws_the_hosts_lines_and_a_scroll_bar() {
    let ui = scene(|ui| {
        host(ui, card_msg(1, 9, CardKind::Question, &["Run on staging", "Skip migrations", "Abort task"], Some(0)));
        host(ui, window_msg(1, 9, 4, 40));
        card(ui, 1, Some(0));
        ui.agents.question_top = 6;
    });
    let bar = face::QUESTION_BAR;
    face::ELLIPSES.with(|count| count.set(0));
    let frame = full(&ui);
    // The host measured the lines for this lane, so none of them is cut.
    assert_eq!(face::ELLIPSES.with(|count| count.get()), 0);
    // The whole track is lit, with the thumb a brighter run inside it: six
    // lines of forty are above the top, so it is neither at the top nor the
    // bottom of the track.
    assert_eq!(lit_in(&frame, bar), (bar.width() * bar.height()) as usize);
    let lit_thumb = count_in(&frame, bar, theme::TEXT_FAINT);
    assert_eq!(lit_thumb, (bar.width() * 8) as usize, "a minimum-height thumb");
    assert_ne!(pixel(&frame, bar.left(), bar.top()), raw(theme::TEXT_FAINT), "not at the top");
    assert_ne!(pixel(&frame, bar.left(), bar.bottom() - 1), raw(theme::TEXT_FAINT), "nor at the end");
    // A card whose body fits draws no bar at all: nothing about it changes.
    let plain = scene(|ui| card(ui, 1, Some(0)));
    assert_eq!(lit_in(&full(&plain), bar), 0);
}

#[test]
fn the_mockup_scenes_draw_no_ellipsis() {
    for (name, ui) in [
        ("roster", scene(|_| {})),
        ("card-question", scene(|ui| card(ui, 1, Some(0)))),
        // Not "ptt-roster": its one-line "Last reply · …" context truncates by
        // design, and at the larger type the demo reply no longer fits whole.
        ("channel-ready", scene(|ui| channel(ui, 2))),
        ("card-ready", scene(|ui| card(ui, 2, None))),
    ] {
        face::ELLIPSES.with(|count| count.set(0));
        let _ = full(&ui);
        assert_eq!(face::ELLIPSES.with(|count| count.get()), 0, "{name} truncated a run");
    }
    // The counter does see a truncation: the channel's one-line card summary.
    let ui = scene(|ui| channel(ui, 1));
    face::ELLIPSES.with(|count| count.set(0));
    let _ = full(&ui);
    assert!(face::ELLIPSES.with(|count| count.get()) > 0);
}

#[test]
fn the_centre_of_every_row_hits_its_target_where_its_outline_is_drawn() {
    let outline = |frame: &[u8], rect: Rect| {
        let (cx, _) = rect.center();
        let p = pixel(frame, cx, rect.top());
        p == raw(theme::OUTLINE) || p == raw(theme::OUTLINE_SELECTED)
    };
    // Roster: the highlighted row's outline and every row's dot sit in
    // `roster_row(slot)`. One page of five rows, scrolled by one.
    let ui = scene(|ui| {
        for slot in 3..5 {
            ui.deck.agents[slot].project.set(crate::deck::demo::PROJECTS[0]);
        }
        ui.agents.top_row = 1;
    });
    let frame = full(&ui);
    for slot in 0..layout::ROSTER_VISIBLE {
        let rect = layout::roster_row(slot);
        let (cx, cy) = rect.center();
        assert_eq!(layout::roster_row_at(cx, cy), Some(slot));
        let (dx, dy, _) = layout::ROSTER_DOT;
        assert_ne!(pixel(&frame, rect.left() + dx, rect.top() + dy), 0, "dot of slot {slot}");
        assert_eq!(outline(&frame, rect), ui.deck.order[1 + slot as usize] == ui.agents.highlight, "slot {slot}");
    }
    assert_eq!(count_in(&frame, layout::roster_active_bar(0), theme::TEXT_PRIMARY), 0);
    let ui = scene(|ui| ui.global.active_channel = 3);
    let frame = full(&ui);
    let bar = layout::roster_active_bar(2);
    assert_eq!(count_in(&frame, bar, theme::TEXT_PRIMARY), (bar.width() * bar.height()) as usize);
    // Question choices.
    let ui = scene(|ui| card(ui, 1, Some(1)));
    let frame = full(&ui);
    for slot in 0..3 {
        let rect = layout::card_choice(slot);
        let (cx, cy) = rect.center();
        assert_eq!(layout::card_choice_at(cx, cy), Some(slot));
        assert!(outline(&frame, rect), "choice {slot}");
    }
    assert!(!outline(&frame, layout::card_choice(3)), "no fourth choice");
    // Ready steps.
    let ui = scene(|ui| card(ui, 2, Some(0)));
    let frame = full(&ui);
    for slot in 0..2 {
        let rect = layout::ready_row(slot);
        let (cx, cy) = rect.center();
        assert_eq!(layout::ready_row_at(cx, cy), Some(slot));
        assert!(outline(&frame, rect), "step {slot}");
    }
    // Channel buttons, one and two.
    let ui = scene(|ui| channel(ui, 1));
    let frame = full(&ui);
    let (cx, cy) = layout::CHANNEL_BUTTON.center();
    assert_eq!(layout::channel_button_at(cx, cy, false), Some(0));
    assert!(outline(&frame, layout::CHANNEL_BUTTON));
    let ui = scene(|ui| {
        ui.deck.agents[0].card.choice_count = 2;
        channel(ui, 1);
    });
    let frame = full(&ui);
    for index in 0..2 {
        let rect = layout::channel_choice_button(index);
        let (cx, cy) = rect.center();
        assert_eq!(layout::channel_button_at(cx, cy, true), Some(index));
        assert!(outline(&frame, rect), "button {index}");
    }
    assert!(!outline(&frame, layout::CHANNEL_BUTTON), "the wide button is not drawn with two");
    // An acknowledged Ready card has nothing to open: no button, no target.
    let ui = scene(|ui| channel(ui, 2));
    assert!(outline(&full(&ui), layout::CHANNEL_BUTTON));
    let ui = scene(|ui| {
        ui.deck.agents[1].card.acknowledged = true;
        channel(ui, 2);
    });
    assert!(!outline(&full(&ui), layout::CHANNEL_BUTTON));
    assert!(lit_in(&full(&ui), layout::channel_card_text(false)) > 0, "the summary moves to the bottom");
}

#[test]
fn drawing_at_every_origin_never_panics_and_is_a_pure_shift() {
    for (name, ui) in scenes() {
        let reference = full(&ui);
        let sweep_all = matches!(name, "roster" | "card-question-long" | "ptt-roster");
        for dx in -480..=480 {
            if !sweep_all && dx % 7 != 0 && ![-479, -241, -1, 1, 33, 479].contains(&dx) {
                continue;
            }
            let frame = full_draw(|band| {
                band.set_origin_x(dx);
                scene::draw(band, &ui);
            });
            if ![-480, -479, -241, -1, 0, 1, 33, 240, 479, 480].contains(&dx) {
                continue;
            }
            for y in (0..480).step_by(3) {
                for x in 0..480 {
                    let expected = if (0..480).contains(&(x - dx)) { pixel(&reference, x - dx, y) } else { 0 };
                    assert_eq!(pixel(&frame, x, y), expected, "{name} origin {dx} at ({x}, {y})");
                }
            }
        }
    }
}

#[test]
fn the_listening_overlay_owns_every_pixel_and_registers_five_lanes() {
    let ui = scene(|ui| {
        ui.on_face(Template::Omarchy);
        listening(ui, 2, PttCtx::Talk);
    });
    let clean = full(&ui);
    let stale = full_draw(|band| {
        band.fill(embedded_graphics::pixelcolor::Rgb565::MAGENTA);
        scene::draw(band, &ui);
    });
    assert_frame(&stale, &clean, "an opaque overlay must not let stale pixels through");
    let sig = signature(&ui);
    // Notice lanes, title/waveform/transcript/context/hint, then Bluetooth.
    assert_eq!(sig.regions().len(), 2 + 5 + 1);
    assert_eq!(sig.regions().last().unwrap().0, layout::BLUETOOTH);
    assert_eq!(sig.regions()[3].0, layout::PTT_WAVE);
    // The face beneath contributes nothing, so switching it changes no pixel.
    let mut other = Box::new(*ui);
    other.on_face(Template::Usage);
    other.global.ptt = ui.global.ptt;
    assert_frame(&full(&other), &clean, "the overlay looks the same over any face");
}

#[test]
fn signature_region_counts_are_fixed_per_view() {
    let counts = [
        ("roster", scene(|_| {}), 7),
        ("channel", scene(|ui| channel(ui, 3)), 6),
        ("question", scene(|ui| card(ui, 1, Some(0))), 8),
        ("ready", scene(|ui| card(ui, 2, None)), 7),
        ("nothing pending", scene(|ui| card(ui, 3, None)), 3),
        ("no agent", scene(|ui| channel(ui, 7)), 3),
    ];
    for (name, ui, lanes) in counts {
        let sig = signature(&ui);
        assert_eq!(sig.regions().len(), lanes + 2 + 1, "{name}");
        assert!(!sig.overflowed());
        assert!(sig.regions().iter().all(|(rect, _)| Rect::SCREEN.intersection(rect) == *rect));
    }
}

#[test]
fn the_age_suffix_yields_before_the_title_is_truncated() {
    let lane = |ui: &UiState| {
        let row = layout::roster_row(2);
        let x = row.left() + layout::ROSTER_TEXT_X;
        (full(ui), Rect::new(x, row.top() + 40, row.right() - 12 - x, 24))
    };
    let working = scene(|_| {});
    let idle = scene(|ui| ui.deck.agents[2].state = AgentState::Idle);
    let (a, rect) = lane(&working);
    let (b, _) = lane(&idle);
    assert!(lit_in(&a, rect) > lit_in(&b, rect), "the short title carries \u{b7} 2m");
    // A title that fits alone but not beside " · 2m".
    let suffix = font::measure(Font::Body, " \u{b7} 2m");
    let mut long = String::from("Compiling the native image");
    while font::measure(Font::Body, &long) + suffix <= rect.width() {
        long.push_str(" and linking");
    }
    while font::measure(Font::Body, &long) > rect.width() {
        long.pop();
    }
    assert!(long.len() < 64 && font::measure(Font::Body, &long) + suffix > rect.width());
    let working = scene(|ui| ui.deck.agents[2].title.set(&long));
    let idle = scene(|ui| {
        ui.deck.agents[2].title.set(&long);
        ui.deck.agents[2].state = AgentState::Idle;
    });
    let (a, rect) = lane(&working);
    let (b, _) = lane(&idle);
    assert_eq!(lit_in(&a, rect), lit_in(&b, rect), "no room: the age is dropped, the title kept whole");
    assert!(lit_in(&a, Rect::new(rect.right() - 16, rect.top(), 16, rect.height())) > 0, "the title runs to the edge whole");
}

#[test]
fn sent_and_offline_cards_tag_the_chosen_row_and_dim_the_rest() {
    let tag_area = |slot: u8| {
        let rect = layout::card_choice(slot);
        Rect::new(rect.left() + 300, rect.top(), layout::CHOICE_TAG_RIGHT - 300, rect.height())
    };
    let live = full(&scene(|ui| card(ui, 1, Some(0))));
    assert!(tinted(&live, tag_area(0), theme::GREEN) > 40, "recommended");
    assert_eq!(tinted(&live, tag_area(1), theme::GREEN), 0);
    let sent = full(&scene(|ui| {
        card(ui, 1, Some(0));
        ui.deck.agents[0].card.pending_answer = Some(1);
    }));
    assert!(tinted(&sent, tag_area(1), theme::GREEN) > 20, "sent");
    assert_eq!(tinted(&sent, tag_area(0), theme::GREEN), 0, "recommended gives way to sent");
    let text_area = |slot: u8| {
        let rect = layout::card_choice(slot);
        Rect::new(rect.left() + layout::CHOICE_TEXT_X, rect.top(), 200, rect.height())
    };
    assert!(count_in(&sent, text_area(0), theme::TEXT_LABEL) > 20, "the others dim");
    assert_eq!(count_in(&live, text_area(0), theme::TEXT_LABEL), 0);
    let offline = full(&scene(|ui| {
        card(ui, 1, Some(0));
        offline(ui);
    }));
    assert!(tinted(&offline, tag_area(0), theme::AMBER) > 20, "offline tag");
    assert_eq!(tinted(&offline, Rect::new(38, 34, 20, 24), theme::RED), 0, "the dot goes gray");
    assert!(tinted(&live, Rect::new(38, 34, 20, 24), theme::RED) > 20, "the live dot is red");
}

#[test]
fn the_waveform_has_23_bars_that_move_with_time_and_flatten_without_a_host() {
    let bars = |frame: &[u8], color: u32| {
        let (_, cy) = layout::PTT_WAVE.center();
        let mut runs = 0;
        let mut inside = false;
        for x in layout::PTT_WAVE.left()..layout::PTT_WAVE.right() {
            let lit = pixel(frame, x, cy) == raw(color);
            runs += (lit && !inside) as usize;
            inside = lit;
        }
        runs
    };
    let first = scene(|ui| listening(ui, 2, PttCtx::Talk));
    let a = full(&first);
    assert_eq!(bars(&a, theme::GREEN), 23);
    let later = scene(|ui| {
        listening(ui, 2, PttCtx::Talk);
        ui.now_ms = 700;
    });
    let b = full(&later);
    assert_eq!(bars(&b, theme::GREEN), 23);
    assert_ne!(count_in(&a, layout::PTT_WAVE, theme::GREEN), count_in(&b, layout::PTT_WAVE, theme::GREEN));
    let outside = Rect::new(0, 60, 480, 116);
    assert_eq!(count_in(&b, outside, theme::GREEN) - count_in(&b, layout::PTT_WAVE, theme::GREEN), 0, "bars stay in the lane");
    let quiet = full(&scene(|ui| {
        host_lost(ui);
        listening(ui, 2, PttCtx::Talk);
    }));
    assert_eq!(count_in(&quiet, layout::PTT_WAVE, theme::GREEN), 0);
    assert_eq!(bars(&quiet, theme::GRAY), 23);
    assert_eq!(count_in(&quiet, ptt::TRANSCRIPT_LANE, theme::GREEN), 0, "no caret without a host");
    assert!(tinted(&quiet, layout::PTT_PILL, theme::AMBER) > 20, "NO HOST pill");
    assert_eq!(tinted(&quiet, layout::PTT_PILL, theme::GREEN), 0);
    assert!(tinted(&a, layout::PTT_PILL, theme::GREEN) > 20, "LISTENING pill");
}

#[test]
fn the_transcript_wraps_in_quotes_with_a_caret_that_final_removes() {
    let live = full(&scene(|ui| listening(ui, 2, PttCtx::Talk)));
    assert!(count_in(&live, ptt::TRANSCRIPT_LANE, theme::GREEN) >= 2 * font::ascent(Font::Text) as usize, "caret");
    let done = full(&scene(|ui| {
        listening(ui, 2, PttCtx::Talk);
        ui.deck.transcript_final = true;
    }));
    assert_eq!(count_in(&done, ptt::TRANSCRIPT_LANE, theme::GREEN), 0);
    let empty = full(&scene(|ui| {
        listening(ui, 2, PttCtx::Talk);
        ui.deck.transcript.clear();
    }));
    assert!(count_in(&empty, Rect::new(38, 176, 12, 30), theme::GREEN) > 0, "an empty transcript is just the caret");
    assert_eq!(lit_in(&empty, Rect::new(50, 176, 380, 200)), 0);
    let long = full(&scene(|ui| {
        listening(ui, 2, PttCtx::Talk);
        ui.deck.transcript.set(LONG_REPLY);
    }));
    let lines = ptt::TRANSCRIPT_LINES as i32;
    let lh = font::line_height(Font::Text);
    let last = layout::PTT_TRANSCRIPT.top() + lines * lh;
    assert!(lit_in(&long, Rect::new(38, last - lh, 404, lh)) > 0, "every line is used");
    assert_eq!(lit_in(&long, Rect::new(38, last + 8, 404, layout::PTT_LAST_REPLY.top() - last - 8)), 0, "never one more");
    // Once it outgrows the box the TAIL shows, behind a faint "…": a longer
    // tail changes the frame, and the first shown line starts dim.
    let mut tail = String::new();
    while ptt::transcript_lines(&format!("\"{tail}\"")).0 < 8 {
        tail.push_str("WWWWW ");
    }
    assert!(tail.len() < 190, "wide words overflow the box well inside Text<200>");
    let with = |text: String| {
        full(&scene(|ui| {
            listening(ui, 2, PttCtx::Talk);
            ui.deck.transcript.set(&text);
        }))
    };
    let (total, skip) = ptt::transcript_lines(&format!("\"{tail}\""));
    assert!(skip > 0 && total == ptt::TRANSCRIPT_LINES + skip, "{total} {skip}");
    let a = with(tail.clone());
    let b = with(format!("{tail} end"));
    assert_ne!(a, b, "the tail is what changes");
    let first_line = Rect::new(38, layout::PTT_TRANSCRIPT.top(), 14, font::line_height(Font::Text));
    assert!(lit_in(&a, first_line) > 0, "a … leads");
    assert!(peak(&a, first_line) <= 0x80, "a faint … leads: {:#x}", peak(&a, first_line));
    assert!(peak(&live, first_line) > 0xE0, "an unscrolled transcript opens with its bright quote");
}

#[test]
fn a_three_line_question_keeps_clear_of_the_first_choice() {
    let ui = scene(|ui| {
        host(ui, card_msg(1, 9, CardKind::Question, &["Run on staging", "Skip migrations"], Some(0)));
        card(ui, 1, Some(0));
    });
    let frame = full(&ui);
    let third = layout::CARD_QUESTION.top() + 2 * font::line_height(Font::Name);
    assert!(lit_in(&frame, Rect::new(37, third, 406, font::ascent(Font::Name))) > 0, "a third line");
    let gap = Rect::new(37, face::QUESTION_LANE.bottom() - 2, 406, 2);
    assert_eq!(lit_in(&frame, gap), 0, "nothing touches the first choice row");
    assert!(lit_in(&frame, layout::card_choice(0)) > 0);
}

#[test]
fn rich_code_and_emphasis_refresh_cleanly_across_scroll_and_card_changes() {
    let start = scene(|ui| {
        channel(ui, 3);
        let a = ui.deck.agent_mut(3).unwrap();
        a.has_card = false;
        a.last_reply.set("Rich chat");
        a.reply_rev = 1;
        a.reply.id = 1;
        a.reply.font = DocFont::RichText;
        a.reply.first = 0;
        a.reply.held = 17;
        a.reply.total = 40;
        for (i, line) in a.reply.lines.iter_mut().enumerate().take(17) {
            line.set(if i % 3 == 0 { "c0\u{e000}C    let x = 42;" } else { "p0Text \u{e000}Bbold\u{e000}A and \u{e000}Iemphasis" });
        }
    });
    walk(&start, &[
        ("scroll", &|ui| { ui.agents.reply_top = 2; }),
        ("tool update", &|ui| { ui.deck.agent_mut(3).unwrap().reply.lines[4].set("t002Command · done"); }),
        ("tool pressed", &|ui| { ui.agents.pressed = Some(18); }),
        ("tool expands", &|ui| { ui.agents.pressed = None; ui.deck.agent_mut(3).unwrap().reply.lines[4].set("T002Command · done"); }),
        ("card opens", &|ui| host(ui, card_msg(3, 9, CardKind::Question, &["Yes", "No"], Some(0)))),
        ("card clears", &|ui| host(ui, clear_msg(3, 9, CardKind::Question))),
        ("shift hints", &|ui| { ui.global.shift = true; }),
    ]);
}

#[test]
fn session_settings_partial_refresh_matches_full_across_pages_and_connection_changes() {
    let start = scene(|ui| channel(ui, 2));
    walk(&start, &[
        ("open settings", &|ui| crate::ui::session::open(ui, 1, &mut Update::new())),
        ("overview", &|ui| {
            let mut menu = crate::deck::msg::SessionMenu::EMPTY;
            menu.agent = 2; menu.rev = 1; menu.status = 1;
            menu.model.set("Model A"); menu.effort.set("high");
            host(ui, HostMsg::SessionMenu(menu));
        }),
        ("model picker", &|ui| {
            ui.agents.settings.field = 1;
            let mut menu = ui.agents.settings.menu;
            menu.field = 1; menu.rev = 2; menu.count = 2; menu.total = 2; menu.selected = Some(0);
            menu.choices[0].set("Model A"); menu.choices[1].set("Model B");
            host(ui, HostMsg::SessionMenu(menu));
        }),
        ("highlight", &|ui| ui.agents.settings.highlight = 1),
        ("pending", &|ui| { ui.agents.settings.menu.status = 2; ui.agents.settings.menu.note.set("Applying for the next turn..."); }),
        ("confirmed", &|ui| { ui.agents.settings.menu.status = 1; ui.agents.settings.menu.selected = Some(1); ui.agents.settings.menu.model.set("Model B"); }),
        ("offline", &|ui| ui.deck.host_present = false),
        ("close", &|ui| ui.agents.settings.open = false),
    ]);
}

#[test]
fn cjk_reply_overhang_is_erased_when_the_reply_changes() {
    let start = scene(|ui| {
        channel(ui, 3);
        host(ui, HostMsg::Reply { agent: 3, text: Text::from_str("日本語 München"), rev: 1 });
    });
    walk(&start, &[
        ("replace tall glyphs", &|ui| host(ui, HostMsg::Reply { agent: 3, text: Text::from_str("ok"), rev: 2 })),
        ("another channel", &|ui| channel(ui, 1)),
    ]);
}

#[test]
fn conol_mosaic_header_redraws_when_a_native_chat_joins_the_project() {
    let mut state = scene(|ui| {
        ui.agents.view = AgentsView::Roster;
        ui.agents.project.set("Conol");
        for agent in &mut ui.deck.agents {
            agent.project.set("Conol");
            agent.kind = AgentKind::Other;
        }
    });
    let mut screens = Screens::new(&state);
    let before = state.clone();
    for agent in &mut state.deck.agents { agent.kind = AgentKind::Conol; }
    update(&mut screens, &before, &state);
    let frame = full(&state);
    assert_ne!(full(&before), frame);
    assert!(lit_outside_safe_area(&frame).is_none());
    if let Ok(path) = std::env::var("CONOL_DECK_PREVIEW") {
        std::fs::write(path, frame).unwrap();
    }
}
