//! Render the exact embedded scene to PPM without a board or a window server.
//!
//!   ui-preview <out.ppm> [scene] [en|zh-Hans|zh-Hant|ja]
//!   ui-preview --list                 every scene name, one per line
//!
//! Scenes are built only through the public `UiState` / `Deck` API, `on_face`
//! and direct pub-field setup. `tools/preview-all.py`
//! renders all of them and lays the mockup scenes over `mock/*.png`.
#![allow(dead_code)]
#[path = "../../../src/battery/state.rs"]
mod battery;
#[path = "../../../src/control/types.rs"]
mod control;
#[path = "../../../src/deck/mod.rs"]
mod deck;
#[path = "../../../src/display/framebuffer.rs"]
mod display;
#[path = "../../../src/display/orientation.rs"]
mod orientation;
#[path = "../../../src/ui/mod.rs"]
mod ui;

use deck::msg::{CardKind, PttCtx, Template};
use ui::state::{AgentsView, Overlay, PttSession, Toast, ToastKind, FACE_W, STRIP_ONE};
use ui::{scene, splash, UiState};

use std::io::Write;

/// The power-on demo session as it stands, and the boot splash.
const BASE_SCENES: [&str; 2] = ["default", "splash"];

/// Deck scenes; the first eight are the mockups (`tools/preview-all.py` knows
/// which `mock/*.png` each one reproduces).
const DECK_SCENES: [&str; 115] = [
    "screen-locked",
    "screen-unlocking",
    "question-answer",
    "chinese-reply",
    "chinese-question",
    "roster",
    "roster-page2",
    "roster-offline",
    "session-settings",
    "session-models",
    "session-effort",
    "session-claude",
    "session-pending",
    "session-offline",
    "channel",
    "channel-question2",
    "channel-ready",
    "channel-long-reply",
    "channel-full-reply",
    "channel-rich-chat",
    "channel-images",
    "image-preview",
    "image-loading",
    "image-error",
    "channel-tools",
    "channel-tool-open",
    "workflow",
    "workflow-runs",
    "workflow-complete",
    "workflow-tool-open",
    "workflow-question",
    "channel-gone",
    "card-question",
    "card-question-sent",
    "card-question-long",
    "card-question-window",
    "card-question-offline",
    "card-ready",
    "card-ready-step",
    "card-none",
    "ptt-roster",
    "ptt-transcribing",
    "ptt-recording",
    "ptt-recording-long",
    "ptt-channel-tip",
    "ptt-channel-tip-pending",
    "ptt-omarchy",
    "ptt-no-host",
    "ptt-naming",
    "ptt-answer",
    "ptt-long-transcript",
    "omarchy",
    "omarchy-scrolled",
    "macropad",
    "macropad-pressed",
    "macropad-empty",
    "macropad-cursor",
    "macropad-edit",
    "arcade",
    "arcade-flappy",
    "arcade-snake",
    "arcade-raiden",
    "arcade-paused",
    "timer-stopwatch",
    "timer-countdown",
    "pomodoro",
    "pomodoro-running",
    "pomodoro-paused",
    "pomodoro-break",
    "pomodoro-long-break",
    "pomodoro-done",
    "pomodoro-cycle-done",
    "system",
    "system-stale",
    "system-unavailable",
    "usage",
    "usage-accounts",
    "usage-fable",
    "usage-reset-expiring",
    "usage-updating",
    "settings-volume",
    "settings-speech",
    "settings-alerts",
    "settings-display",
    "settings-language",
    "settings-tutorial",
    "settings-host",
    "settings-about",
    "settings-adjust",
    "settings-reset",
    "tutorial-buttons",
    "tutorial-wheel",
    "tutorial-panels",
    "tutorial-latest",
    "tutorial-ptt",
    "tutorial-done",
    "manager",
    "manager-grabbed",
    "manager-confirm",
    "gallery",
    "naming",
    "toast-question",
    "toast-ready",
    "toast-message",
    "toast-not-delivered",
    "drawer",
    "indicator",
    "slide-25",
    "slide-50",
    "slide-75",
    "battery-low-header",
    "bluetooth-strong",
    "bluetooth-weak",
    "bluetooth-unknown",
    "bluetooth-disconnected",
];

const LONG_QUESTION: &str = "The staging database has 3 pending migrations and the integration tests fail on main. Apply them before the deploy?";
const LONG_REPLY: &str = "Refactored the header into three components, moved the nav state into a hook, updated 34 files and the snapshot tests; the bundle is 4 kB smaller. Ready for a review whenever you are.";
const LONG_TRANSCRIPT: &str = "Refactor the auth module, then run the test suite, update the changelog, open a pull request against main and ask the reviewers to look at the session handling first because that is where the bug was.";

fn listening(ui: &mut UiState) {
    ui.global.active_channel = 2;
    ui.global.ptt = Some(PttSession::new(2, PttCtx::Talk, 0, 0));
    ui.deck.transcript.set(deck::demo::TRANSCRIPT);
}

fn card(ui: &mut UiState, agent: u8, choice: Option<u8>) {
    ui.agents.view = AgentsView::Card;
    ui.agents.agent = agent;
    ui.agents.highlight = agent;
    ui.agents.choice = choice;
    ui.global.active_channel = agent;
}

fn toast(ui: &mut UiState, agent: u8, card: u16, kind: CardKind) {
    ui.now_ms = 1000;
    ui.notify.toast = Some(Toast::card(agent, card, kind, 0));
}

/// Demo data with a host pretending to be there, so header tags read like the
/// mockups (clock, "remote", "deck.toml") instead of "demo".
fn deck_scene(ui: &mut UiState, scene: &str) {
    ui.deck.host_present = true;
    ui.connected = true;
    match scene {
        "screen-locked" | "screen-unlocking" => {
            ui.set_screen_lock(Some(if scene == "screen-locked" { 0 } else { 2 }));
        }
        "question-answer" => {
            let agent = &mut ui.deck.agents[0];
            agent.name.set("Project setup");
            agent.has_card = false;
            agent.state = deck::msg::AgentState::Working;
            agent.reply_rev = 1;
            agent.reply.id = 1;
            agent.reply.font = deck::msg::DocFont::RichRow;
            let lines = [
                "h0\u{e000}BYou · 4 · done", "b0",
                "h0\u{e000}BYour answers", "b0",
                "p0\u{e000}BWhich database?", "b0", "q1SQLite", "b0",
                "p0\u{e000}BProject name?", "b0", "q1中文项目",
            ];
            for (slot, text) in agent.reply.lines.iter_mut().zip(lines) { slot.set(text); }
            agent.reply.first = 0;
            agent.reply.held = lines.len() as u8;
            agent.reply.total = lines.len() as u16;
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 1;
            ui.agents.highlight = 1;
            ui.global.active_channel = 1;
        }
        "chinese-reply" | "chinese-question" => {
            let agent = &mut ui.deck.agents[0];
            agent.name.set("中文助手");
            agent.title.set("设备字体已更新");
            if scene == "chinese-question" {
                agent.card.text.set("是否更新设备？简体中文與繁體中文都能正常顯示。");
                agent.card.choices[0].set("立即更新");
                agent.card.choices[1].set("先运行测试");
                agent.card.choices[2].set("取消操作");
                card(ui, 1, Some(0));
            } else {
                agent.has_card = false;
                agent.state = deck::msg::AgentState::Ready;
                agent.reply_rev = 1;
                agent.reply.id = 1;
                agent.reply.font = deck::msg::DocFont::Title;
                agent.reply.first = 0;
                agent.reply.held = 0;
                ui::font::wrap(ui::font::Font::Title,
                    "你好，世界！中文字体已安装。\n简体中文与繁體中文：臺灣、香港。\nCodex 已完成 3 项测试。",
                    ui::layout::CHANNEL_REPLY.width(), deck::msg::DOC_HELD, &mut |index, line, _| {
                        agent.reply.lines[index].set(line);
                        agent.reply.held = (index + 1) as u8;
                    });
                agent.reply.total = agent.reply.held as u16;
                ui.agents.view = AgentsView::Channel;
                ui.agents.agent = 1;
                ui.agents.highlight = 1;
                ui.global.active_channel = 1;
            }
        }
        "bluetooth-strong" | "bluetooth-weak" | "bluetooth-unknown" | "bluetooth-disconnected" => {
            use ui::bluetooth::{Level, Status};
            ui.bluetooth = match scene {
                "bluetooth-strong" => Status::Signal(Level::Strong),
                "bluetooth-weak" => Status::Signal(Level::Weak),
                "bluetooth-unknown" => Status::Unknown,
                _ => Status::Disconnected,
            };
        }
        "session-settings" | "session-models" | "session-effort" | "session-claude" | "session-pending" | "session-offline" => {
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 2;
            ui.deck.agent_mut(2).unwrap().kind = deck::msg::AgentKind::Codex;
            let v = &mut ui.agents.settings;
            v.open = true;
            v.field = if scene == "session-models" || scene == "session-pending" { 1 } else if scene == "session-effort" { 2 } else { 0 };
            v.menu.agent = 2; v.menu.rev = 1; v.menu.field = v.field;
            v.menu.model.set("GPT-6.1 Sol"); v.menu.effort.set("high"); v.menu.status = 1;
            v.menu.note.set("Changes apply to the next turn.");
            if v.field != 0 {
                v.menu.count = 4; v.menu.total = if v.field == 1 { 6 } else { 7 }; v.menu.selected = Some(0);
                for (i, label) in if v.field == 1 { ["GPT-6.1 Sol", "GPT-6 Astra", "GPT-6 Sol", "GPT-6 Luna"] }
                    else { ["low", "medium", "high", "xhigh"] }.iter().enumerate() { v.menu.choices[i].set(label); }
            }
            if scene == "session-claude" {
                ui.deck.agent_mut(2).unwrap().kind = deck::msg::AgentKind::Claude;
                v.menu.model.set("Fable"); v.menu.effort.clear(); v.menu.status = 0;
                v.menu.note.set("Use /model or /effort in Claude Code. Channels does not enable these controls.");
            }
            if scene == "session-pending" { v.menu.status = 2; v.menu.note.set("Applying for the next turn..."); v.highlight = 1; }
            if scene == "session-offline" { ui.deck.host_present = false; }
        }
        "ptt-transcribing" => {
            listening(ui);
            ui.global.ptt_pending = ui.global.ptt.take();
            ui.now_ms = 600;
        }
        "ptt-recording" | "ptt-recording-long" => {
            // A companion's press: speech-like levels from the deck mic, 20 ms
            // frames, words and pauses, then the timeline at 6.3 s / 14.2 s.
            listening(ui);
            ui.deck.transcript.clear();
            ui.deck.transcript_rec = true;
            let secs = if scene == "ptt-recording" { 6_300 } else { 14_200 };
            let start = 10_000u32;
            let mut session = ui.global.ptt.unwrap();
            session.started_ms = start;
            session.meter_base = ui::meter::count();
            ui.global.ptt = Some(session);
            let mut seed = 0x2545_f491u32;
            let mut loud = 1.0f32;
            for t in (0..secs).step_by(20) {
                seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
                // Syllables of 180-260 ms with their own loudness, short gaps
                // between words, a breath every couple of seconds.
                if t % 220 == 0 { loud = 0.15 + (seed % 100) as f32 / 115.0; }
                let word = (t / 660) % 4 != 3 && (t / 2_300) % 3 != 2;
                let syllable = ((t % 220) as f32 / 220.0 * core::f32::consts::PI).sin().max(0.0);
                let noise = 40 + (seed % 50) as u16;
                let peak = if word { noise + (syllable * loud * 14_000.0) as u16 } else { noise };
                ui::meter::push(peak, start + t);
            }
            ui.now_ms = start + secs;
        }
        "ptt-channel-tip" | "ptt-channel-tip-pending" => {
            listening(ui);
            ui.deck.agent_mut(2).unwrap().kind = deck::msg::AgentKind::Claude;
            ui.deck.transcript_channel_tip = true;
            if scene == "ptt-channel-tip-pending" {
                ui.global.ptt_pending = ui.global.ptt.take();
                ui.now_ms = 600;
            }
        }
        "roster" => {}
        "roster-page2" => {
            // RIGHT from the mockup's roster: the second project's page.
            use control::{Button, Control, Edge};
            let mut out = ui::Update::new();
            ui.apply(Control::Button(Button::Right, Edge::Press), 100, &mut out);
            ui.apply(Control::Button(Button::Right, Edge::Release), 110, &mut out);
        }
        "roster-offline" => {
            ui.deck.settings.demo = false;
            ui.deck.link_lost();
            ui.connected = false;
        }
        "channel" | "channel-question2" | "channel-ready" => {
            let agent = match scene {
                "channel" => 3,
                "channel-question2" => 1,
                _ => 2,
            };
            if scene == "channel-question2" {
                ui.deck.agents[0].card.choice_count = 2;
            }
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = agent;
            ui.agents.highlight = agent;
            ui.global.active_channel = agent;
        }
        // A reply longer than the `reply` line can carry: the host wrapped it
        // and sent a window, and the deck draws the lines it was handed,
        // scrolled, with the bar in the gutter. Before this, `CAPS.reply` cut
        // the body at 200 bytes with an ellipsis and there was nothing to
        // scroll.
        "channel-long-reply" | "channel-full-reply" => {
            let agent = &mut ui.deck.agents[0];
            agent.last_reply.set(LONG_REPLY);
            agent.reply_rev = 1;
            agent.reply.id = 1;
            agent.reply.font = deck::msg::DocFont::Row;
            agent.reply.first = 4;
            agent.reply.held = deck::msg::DOC_LINES as u8;
            agent.reply.total = 40;
            for (slot, line) in agent.reply.lines.iter_mut().enumerate() {
                line.set(&format!("wrapped reply line {}", 4 + slot));
            }
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 1;
            ui.agents.highlight = 1;
            ui.agents.reply_top = 6;
            ui.global.active_channel = 1;
            if scene == "channel-full-reply" {
                agent.has_card = false;
                agent.state = deck::msg::AgentState::Ready;
                agent.reply.font = deck::msg::DocFont::Title;
                agent.reply.first = 0;
                agent.reply.held = 0;
                ui::font::wrap(ui::font::Font::Title, LONG_REPLY, ui::layout::CHANNEL_REPLY.width(), deck::msg::DOC_HELD, &mut |index, line, _| {
                    agent.reply.lines[index].set(line);
                    agent.reply.held = (index + 1) as u8;
                });
                agent.reply.total = agent.reply.held as u16;
                ui.agents.reply_top = 0;
            }
        }
        "workflow" | "workflow-runs" | "workflow-complete" | "workflow-tool-open" | "workflow-question" => {
            let agent = &mut ui.deck.agents[0];
            agent.has_card = scene == "workflow-question";
            agent.name.set("Workflow");
            if scene == "workflow-runs" { agent.kind = deck::msg::AgentKind::Claude; }
            agent.state = if scene == "workflow-complete" { deck::msg::AgentState::Ready }
                else if agent.has_card { deck::msg::AgentState::Question } else { deck::msg::AgentState::Working };
            agent.last_reply.set("Workflow");
            agent.reply_kind = deck::msg::DocKind::Workflow;
            agent.reply_rev = 1;
            agent.reply.id = 1;
            agent.reply.font = deck::msg::DocFont::RichRow;
            let lines: &[&str] = if scene == "workflow-runs" { &[
                "h0Workflow", "h0fixup-constraints-and-truth", "p00/9 · 1m16s · 488.1k tokens", "w00009",
                "h0design-navigation-panel", "p00/4 · 1m16s · 1.1m tokens", "w00004",
            ] } else if scene == "workflow-tool-open" { &[
                "T002Run cargo test · failed", "h0Bash", "c0\u{e000}Ccargo test workflow", "p0One test needs attention.",
                "c0\u{e000}Sassert_eq!\u{e000}C(done, total);", "p0Expected: 5", "p0Actual: 4", "t003Read workflow.rs · done",
            ] } else if scene == "workflow-complete" { &[
                "h0\u{e000}BPlan · 5/5 complete", "w00505", "t040Full plan · 5 steps", "p03. Done: Build the workflow UI", "p04. Done: Verify live updates", "p05. Done: Review the result", "h0\u{e000}BRecent activity", "t001Read agents.rs · done", "t002Run cargo test · done",
            ] } else { &[
                "h0\u{e000}BPlan · 3/5 complete", "w00305", "t040Full plan · 5 steps", "p03. Done: Build the workflow UI", "q04. Now: Verify live updates", "p05. Next: Review the result", "h0\u{e000}BRecent activity", "t001Read agents.rs · done", "t002Run cargo test · running",
            ] };
            for (slot, text) in agent.reply.lines.iter_mut().zip(lines) { slot.set(text); }
            if agent.has_card { agent.reply.lines[4].set("q04. Paused: Verify live updates"); }
            agent.reply.first = 0;
            agent.reply.held = lines.len() as u8;
            agent.reply.total = lines.len() as u16;
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 1;
            ui.agents.highlight = 1;
            ui.agents.reply_top = 0;
            ui.agents.reply_follow = false;
            ui.global.active_channel = 1;
        }
        "image-preview" | "image-loading" | "image-error" => {
            ui.deck.host_present = true;
            let agent = &mut ui.deck.agents[0];
            agent.has_card = false; agent.reply_rev = 1; agent.reply.id = 1;
            ui.agents.view = AgentsView::Channel; ui.agents.agent = 1;
            let mut out = ui::Update::new();
            ui::image::open(ui, 0, 1, 100, &mut out);
            let mut pixels = vec![0u8; 512+128*96];
            for i in 0..256 {
                let rgb = (((i / 32) as u16 * 31 / 7) << 11) | ((((i / 4) % 8) as u16 * 63 / 7) << 5) | ((i % 4) as u16 * 31 / 3);
                pixels[2*i..2*i+2].copy_from_slice(&rgb.to_le_bytes());
            }
            for y in 0..96 { for x in 0..128 { pixels[512+y*128+x] = ((x/16)*32+(y/12)*4+((x+y)/32)%4) as u8; } }
            for (i, chunk) in pixels.chunks(384).enumerate() {
                if scene == "image-loading" && i >= 13 { break; }
                let mut data = [0;384]; data[..chunk.len()].copy_from_slice(chunk);
                ui::image::host(ui, &deck::msg::HostMsg::Image { agent:1, rev:1, index:0, offset:(i*384) as u16, total:pixels.len() as u16, width:128, height:96, len:chunk.len() as u16, data }, &mut out);
            }
            if scene == "image-error" { ui.deck.host_present = false; ui::image::tick(ui, 200, &mut out); }
        }
        "channel-rich-chat" | "channel-tools" | "channel-tool-open" | "channel-images" => {
            let agent = &mut ui.deck.agents[0];
            agent.has_card = false;
            agent.name.set("Conversation");
            agent.state = deck::msg::AgentState::Working;
            agent.last_reply.set("The chat view now supports formatted messages.");
            agent.reply_rev = 1;
            agent.reply.id = 1;
            agent.reply.font = deck::msg::DocFont::RichRow;
            let lines: &[&str] = match scene {
                "channel-images" => &["h0You", "p0What do you think of this design?", "i000Image · 1024 × 768 · tap to view", "b0", "h0Activity", "p0Generated a preview", "t001Show details · 1", "i001Image · 1024 × 1024 · tap to view"],
                "channel-tools" => &[
                    "h0\u{e000}BYou · 1 · done",
                    "p0Check the transcription flow.",
                    "b0",
                    "h0\u{e000}BExplored",
                    "p1\u{e000}ERead\u{e000}A voice.rs",
                    "p1\u{e000}ERead\u{e000}A outbox.rs",
                    "p1\u{e000}ERead\u{e000}A contract.rs",
                    "t001Show details · 3",
                    "b0",
                    "h0\u{e000}BAssistant · 5 · live",
                    "p0The final transcript now waits.",
                ],
                "channel-tool-open" => &[
                    "T001Hide details · 3",
                    "h0\u{e000}BRead voice.rs",
                    "h0\u{e000}Brust",
                    "c0\u{e000}Sfn\u{e000}C finish() {",
                    "c0\u{e000}C  \u{e000}Slet\u{e000}C ready = \u{e000}Strue\u{e000}C;",
                    "c0\u{e000}C  send(\u{e000}c\"complete\"\u{e000}C);",
                    "c0\u{e000}C}",
                    "b0",
                    "h0\u{e000}BRead outbox.rs",
                    "p0Completed results stay queued.",
                ],
                _ => &[
                "h0\u{e000}BAssistant · 3 · live",
                "p0Here is the \u{e000}Bupdated\u{e000}A chat view.",
                "b0",
                "p0• Conversation history",
                "p0• Live text and tool progress",
                "p0• \u{e000}BBold\u{e000}A and \u{e000}Iemphasis",
                "b0",
                "c0\u{e000}Cfn main() {",
                "c0\u{e000}C    println!(\"Hello!\");",
                "c0\u{e000}C}",
                ],
            };
            for (slot, text) in agent.reply.lines.iter_mut().zip(lines) { slot.set(text); }
            if agent.has_card { agent.reply.lines[4].set("q04. Paused: Verify live updates"); }
            agent.reply.first = 0;
            agent.reply.held = lines.len() as u8;
            agent.reply.total = lines.len() as u16;
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 1;
            ui.agents.highlight = 1;
            ui.agents.reply_top = 0;
            ui.global.active_channel = 1;
        }
        "channel-gone" => {
            ui.agents.view = AgentsView::Channel;
            ui.agents.agent = 8;
        }
        "card-question" => card(ui, 1, Some(0)),
        "card-question-sent" => {
            card(ui, 1, Some(0));
            ui.deck.agents[0].card.pending_answer = Some(0);
        }
        "card-question-long" => {
            card(ui, 1, Some(2));
            let question = &mut ui.deck.agents[0].card;
            question.text.set(LONG_QUESTION);
            question.choice_count = 4;
            question.choices[3].set("Ask the team on Slack first");
            ui.agents.choice_top = 1;
            ui.agents.pressed = Some(2);
        }
        // The host wrapped the body and sent a window of it: the deck draws
        // the lines it was handed, scrolled, with the bar in the gutter.
        "card-question-window" => {
            card(ui, 1, Some(0));
            let question = &mut ui.deck.agents[0].card;
            question.body.font = deck::msg::DocFont::Name;
            question.body.first = 4;
            question.body.held = deck::msg::DOC_LINES as u8;
            question.body.total = 40;
            for (slot, line) in question.body.lines.iter_mut().enumerate() {
                line.set(&format!("wrapped body line {}", 4 + slot));
            }
            ui.agents.question_top = 6;
        }
        "card-question-offline" => {
            card(ui, 1, Some(0));
            ui.deck.settings.demo = false;
            ui.deck.link_lost();
        }
        "card-ready" => card(ui, 2, None),
        "card-ready-step" => {
            card(ui, 2, Some(1));
            ui.deck.agents[1].last_reply.set(LONG_REPLY);
        }
        "card-none" => card(ui, 4, None),
        "ptt-roster" => listening(ui),
        "ptt-omarchy" => {
            ui.on_face(Template::Omarchy);
            listening(ui);
        }
        "ptt-no-host" => {
            // The daemon went silent with the HID link up: the session is
            // latched but nobody listens.
            ui.deck.settings.demo = false;
            ui.deck.link_lost();
            ui.global.active_channel = 2;
            ui.global.ptt = Some(PttSession::new(2, PttCtx::Talk, 0, 0));
        }
        "ptt-naming" => {
            ui.global.overlay = Overlay::Naming;
            ui.manager.naming_template = Template::Macropad;
            ui.global.ptt = Some(PttSession::new(0, PttCtx::Name, 0, 0));
        }
        "ptt-answer" => {
            card(ui, 1, Some(0));
            ui.global.ptt = Some(PttSession::new(1, PttCtx::Answer, 1, 0));
            ui.deck.transcript.set(deck::demo::TRANSCRIPT);
        }
        "ptt-long-transcript" => {
            listening(ui);
            ui.deck.transcript.set(LONG_TRANSCRIPT);
        }
        "omarchy" => ui.on_face(Template::Omarchy),
        "omarchy-scrolled" => {
            ui.on_face(Template::Omarchy);
            let ws = &mut ui.deck.workspaces;
            let extra = ["term \u{b7} zsh", "slack \u{2014} #firmware", "nvim \u{b7} deck.rs"];
            for (row, title) in ws.windows.iter_mut().skip(3).zip(extra) {
                row.tone = deck::msg::Tone::Gray;
                row.title.set(title);
            }
            ws.window_count = 6;
            ws.selected = 4;
            ui.omarchy.selected_window = 4;
            ui.omarchy.top_row = 2;
            ui.omarchy.pressed_row = Some(4);
        }
        "macropad" => ui.on_face(Template::Macropad),
        "macropad-pressed" => {
            ui.on_face(Template::Macropad);
            ui.macropad.face = ui.current_face().id;
            ui.macropad.down = 1 << 1;
        }
        "macropad-empty" => {
            ui.on_face(Template::Macropad);
            for set in &mut ui.deck.macros {
                set.face = 0;
                set.source.clear();
            }
        }
        "macropad-cursor" => {
            ui.on_face(Template::Macropad);
            ui.macropad.cursor = Some((ui.current_face().id, 4));
        }
        "macropad-edit" => {
            ui.on_face(Template::Macropad);
            ui.global.overlay = Overlay::MacroEdit;
            ui.macropad.edit.face = ui.current_face().id;
            ui.macropad.edit.index = 4;
            ui.macropad.edit.present = true;
            ui.macropad.edit.label.set("screenshot");
            ui.macropad.edit.keys = deck::msg::Keystroke::parse("cmd+shift+4").unwrap_or(deck::msg::Keystroke::NONE);
            ui.macropad.edit.row = 2;
            ui.macropad.edit.adjusting = true;
        }
        "arcade" | "arcade-flappy" | "arcade-snake" | "arcade-raiden" | "arcade-paused" => {
            ui.deck.settings.demo = false;
            ui.on_face(Template::Arcade);
            if scene != "arcade" {
                use control::{Button, Control, Edge};
                ui.arcade.selection = match scene { "arcade-snake" => 1, "arcade-raiden" | "arcade-paused" => 2, _ => 0 };
                for _ in 0..2 { for edge in [Edge::Press,Edge::Release] { ui.apply(Control::Button(Button::Enter,edge),0,&mut ui::Update::new()); } }
                // Let the enemy formation enter the screen in Raiden previews.
                let end_ms = if ui.arcade.selection == 2 { 2000 } else { 1000 };
                for now in (20..=end_ms).step_by(20) {
                    if scene == "arcade-flappy" && now % 300 == 0 { for edge in [Edge::Press,Edge::Release] { ui.apply(Control::Button(Button::PushToTalk,edge),now,&mut ui::Update::new()); } }
                    ui.tick(now,0,true,&mut ui::Update::new());
                }
                if scene == "arcade-paused" { ui.apply(Control::Button(Button::Enter,Edge::Press),end_ms,&mut ui::Update::new()); }
            }
        }
        "timer-stopwatch" | "timer-countdown" | "pomodoro" | "pomodoro-running"
        | "pomodoro-paused" | "pomodoro-break" | "pomodoro-long-break"
        | "pomodoro-done" | "pomodoro-cycle-done" => {
            use ui::state::timer::{Mode, Phase, Run};
            let len = ui.global.strip_len as usize;
            ui.global.strip[len] = ui.global.strip[len - 1];
            ui.global.strip[len - 1] = deck::msg::FaceEntry::new(Template::Timer.default_id(), Template::Timer, "Timer");
            ui.global.strip_len += 1;
            ui.deck.settings.demo = false;
            ui.on_face(Template::Timer);
            ui.now_ms = 1000;
            ui.timer.mode = Mode::Pomodoro;
            match scene {
                "timer-stopwatch" => ui.timer.mode = Mode::Stopwatch,
                "timer-countdown" => ui.timer.mode = Mode::Countdown,
                "pomodoro" => {},
                "pomodoro-running" | "pomodoro-paused" => {
                    ui.timer.pomodoro.completed = 1;
                    ui.timer.pomodoro.run = Run { banked_ms: 312_000, since_ms: (scene == "pomodoro-running").then_some(1000) };
                }
                "pomodoro-break" | "pomodoro-long-break" => {
                    ui.timer.pomodoro.phase = if scene == "pomodoro-break" { Phase::ShortBreak } else { Phase::LongBreak };
                    ui.timer.pomodoro.completed = if scene == "pomodoro-break" { 1 } else { 4 };
                    ui.timer.pomodoro.run = Run { banked_ms: 42_000, since_ms: Some(1000) };
                }
                _ => {
                    ui.timer.pomodoro.completed = if scene == "pomodoro-cycle-done" { 4 } else { 1 };
                    ui.timer.pomodoro.run = Run { banked_ms: Phase::Focus.duration_ms(), since_ms: None };
                    ui.timer.pomodoro.done = true;
                }
            }
        }
        "system" | "system-stale" | "system-unavailable" => {
            ui.on_face(Template::System);
            ui.deck.settings.demo = false;
            ui.deck.host_present = true;
            ui.host(&deck::msg::HostMsg::System(deck::msg::SystemStatus {
                cpu: Some(23), gpu: Some(67), memory_used_mib: 23_552,
                memory_total_mib: 65_536, disk_free_mib: 421_888,
                disk_total_mib: 976_000, sample: 1,
            }), 100, &mut ui::Update::new());
            if scene == "system-stale" { ui.deck.system_stale = true; }
            if scene == "system-unavailable" { ui.deck.system = deck::msg::SystemStatus::EMPTY; }
        }
        "usage" => ui.on_face(Template::Usage),
        "usage-accounts" | "usage-fable" | "usage-reset-expiring" => {
            ui.on_face(Template::Usage);
            ui.deck.usage.period.set("5h / 7d");
            ui.deck.usage.count = 4;
            for (i, (name, account, percent, reset)) in [
                ("Claude session", "personal@example.com", 42, "resets in 2h 15m"),
                ("Claude week", "personal@example.com", 68, "resets in 4d 8h"),
                ("Codex session", "work@company.example", 23, "resets in 3h 10m"),
                ("Codex week", "work@company.example", 81, "resets in 2d 6h"),
            ].into_iter().enumerate() {
                let item = &mut ui.deck.usage.items[i];
                item.name.set(name);
                item.account.set(account);
                item.percent = percent;
                item.reset.set(reset);
                item.tone = None;
            }
            if scene == "usage-reset-expiring" {
                ui.deck.usage.items[1].reset_tip.set("2 resets expire in 1d 6h");
                ui.deck.usage.items[3].reset_tip.set("1 reset expires in 2h 10m");
                ui.deck.usage.items[4] = ui.deck.usage.items[0];
                ui.deck.usage.items[4].name.set("Claude Fable week");
                ui.deck.usage.count = 5;
            }
            if scene == "usage-fable" {
                ui.deck.usage.items[4] = ui.deck.usage.items[3];
                ui.deck.usage.items[3] = ui.deck.usage.items[2];
                ui.deck.usage.items[2].name.set("Claude Fable week");
                ui.deck.usage.items[2].account.set("personal@example.com");
                ui.deck.usage.items[2].reset.set("resets in 4d 8h");
                ui.deck.usage.items[2].percent = 100;
                ui.deck.usage.count = 5;
            }
        }
        "usage-updating" => {
            ui.on_face(Template::Usage);
            ui.usage.updating = true;
        }
        "settings-volume" | "settings-speech" | "settings-alerts" | "settings-display"
        | "settings-host" | "settings-about" => {
            ui.on_face(Template::Settings);
            ui.settings.panel = [
                "settings-volume",
                "settings-speech",
                "settings-alerts",
                "settings-display",
                "settings-host",
                "settings-about",
            ]
            .iter()
            .position(|name| *name == scene)
            .unwrap_or(0) as u8;
        }
        "settings-language" | "settings-tutorial" => {
            ui.on_face(Template::Settings);
            ui.settings.panel = 3;
            ui.settings.row = if scene == "settings-language" { 3 } else { 4 };
            ui.settings.adjusting = scene == "settings-language";
        }
        "settings-adjust" => {
            ui.on_face(Template::Settings);
            ui.settings.panel = 3;
            ui.settings.adjusting = true;
        }
        "settings-reset" => {
            ui.on_face(Template::Settings);
            let (panel, row) = ui::state::settings::FACTORY_RESET_ROW;
            ui.settings.panel = panel;
            ui.settings.row = row;
            ui.global.overlay = Overlay::ConfirmReset;
        }
        "tutorial-buttons" | "tutorial-wheel" | "tutorial-panels" | "tutorial-latest" | "tutorial-ptt" | "tutorial-done" => {
            use ui::state::tutorial::Step;
            ui.start_tutorial(false);
            ui.tutorial.step = match scene {
                "tutorial-wheel" => Step::Wheel,
                "tutorial-panels" => Step::Panels,
                "tutorial-latest" => Step::Latest,
                "tutorial-ptt" => Step::Ptt,
                "tutorial-done" => Step::Done,
                _ => Step::Buttons,
            };
            // Mid-way through each step, so the progress marks show.
            ui.tutorial.index = 1;
            ui.tutorial.named = Some(control::Button::Enter);
            ui.tutorial.detents = 1;
            ui.tutorial.panel = 1;
            ui.tutorial.ptt_down = true;
        }
        "manager" => ui.global.overlay = Overlay::Manager,
        "manager-grabbed" => {
            ui.global.overlay = Overlay::Manager;
            ui.manager.selected = 1;
            ui.manager.grabbed = true;
        }
        "manager-confirm" => {
            ui.global.overlay = Overlay::ConfirmRemove;
            ui.manager.selected = 1;
            ui.manager.confirm_face = 2;
        }
        "gallery" => {
            ui.global.overlay = Overlay::Gallery;
            ui.manager.gallery_sel = 2;
        }
        "naming" => {
            ui.global.overlay = Overlay::Naming;
            ui.manager.naming_template = Template::Macropad;
            ui.manager.naming_text.set(deck::demo::FACE_NAME);
        }
        "toast-question" => {
            ui.on_face(Template::Omarchy);
            ui.deck.notices[0].in_drawer = false;
            toast(ui, 1, 1, CardKind::Question);
        }
        "toast-ready" => {
            ui.on_face(Template::Usage);
            toast(ui, 2, 2, CardKind::Ready);
        }
        "toast-message" => {
            // PTT with no target agent.
            ui.now_ms = 1000;
            ui.notify.toast = Some(Toast::message(ToastKind::NoChannel, 0, 0, 0));
        }
        "toast-not-delivered" => {
            // The answer reverted after 5 s without a host clear.
            card(ui, 1, Some(1));
            ui.now_ms = 1000;
            ui.notify.toast = Some(Toast::message(ToastKind::NotDelivered, 1, 1, 0));
        }
        "drawer" => {
            ui.global.overlay = Overlay::Drawer;
            ui.deck.notices[1] = deck::Notice::new(2, 2, CardKind::Ready, 0).filed();
            ui.notify.drawer_sel = (1, 1);
        }
        "indicator" => ui.on_face(Template::Usage),
        "slide-25" | "slide-50" | "slide-75" => {
            let quarter = match scene {
                "slide-25" => 1,
                "slide-50" => 2,
                _ => 3,
            };
            ui.global.current = 1;
            ui.global.strip_x = FACE_W * STRIP_ONE * quarter / 4;
        }
        "battery-low-header" => {
            ui.update_battery(battery::BatteryState {
                millivolts: Some(3450),
                charge: battery::ChargeState::Battery,
                estimated_percent: Some(9),
                ..battery::BatteryState::UNKNOWN
            });
        }
        _ => unreachable!("scene table and builder disagree on '{scene}'"),
    }
    ui.invalidate();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--list") {
        for scene in BASE_SCENES.iter().chain(&DECK_SCENES) {
            println!("{scene}");
        }
        return;
    }
    let path = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("/tmp/openwalkietalkie-ui.ppm");
    let scene_name = args.get(2).map(String::as_str).unwrap_or("default");
    let language_code = args.get(3).map(String::as_str).unwrap_or("en");
    let language = ui::i18n::Language::ALL.into_iter().find(|language| language.code() == language_code)
        .unwrap_or_else(|| {
            eprintln!("ui-preview: unknown language '{language_code}'");
            std::process::exit(2);
        });
    // Boxed: a `UiState` is never moved by value, here as on the device.
    let mut ui = Box::new(UiState::new());
    ui.reset();
    // Every scene starts from the demo session (off at power-on).
    ui.load_demo();
    if BASE_SCENES.contains(&scene_name) {
        ui.invalidate();
    } else if DECK_SCENES.contains(&scene_name) {
        deck_scene(&mut ui, scene_name);
    } else {
        // A misspelt scene must not pass for the default one in golden checks.
        eprintln!("ui-preview: unknown scene '{scene_name}' (try --list)");
        std::process::exit(2);
    }
    ui.language = language;
    let mut output = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(output, "P6\n480 480\n255").unwrap();
    let mut band = display::Band::new();
    let start = std::time::Instant::now();
    for index in 0..display::BANDS {
        band.set_row(index);
        if scene_name == "splash" {
            splash::draw(&mut band);
        } else {
            scene::draw(&mut band, &ui);
        }
        for pixel in band.data().chunks_exact(2) {
            let raw = u16::from_le_bytes([pixel[0], pixel[1]]);
            output
                .write_all(&[
                    ((raw >> 11) * 255 / 31) as u8,
                    (((raw >> 5) & 63) * 255 / 63) as u8,
                    ((raw & 31) * 255 / 31) as u8,
                ])
                .unwrap();
        }
    }
    output.flush().unwrap();
    eprintln!("Rendered {path} in {:?}", start.elapsed());
}
