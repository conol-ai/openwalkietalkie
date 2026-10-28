//! Grammar of the Agents face: roster, channel, question and ready cards.
//!
//! Roster: one page per project (`Deck::page_rows`), the page being the
//! highlighted agent's own (`View::project`). SCROLL moves the highlight down
//! the page (an agent id, displayed order, hard ends); LEFT/RIGHT turn to the
//! neighbouring project's page and highlight its first row (hard ends, no
//! `channel`: turning pages is looking, not talking); ENTER opens the
//! highlighted channel. The active channel's marker is host truth (`focus`)
//! and moves the highlight — and so the page — only when the user is not
//! looking (Roster, nothing held, 2 s quiet); a re-sort waits 1.5 s of quiet.
//! Channel: ENTER descends into the pending card, LEFT/RIGHT siblings, CANCEL
//! up. Card: SCROLL walks the question body's hidden lines and then the
//! choices — one wheel, the body first, because the body is above them; a
//! body that hides nothing leaves the wheel to the choices exactly as before,
//! and a body whose next window has not landed holds the wheel rather than
//! jumping into them. ENTER/tap confirms under the guard (`answer`:
//! the card locks as "sent" until the host clears it, or reverts after 5 s
//! with a "not delivered" toast), PTT answers by voice, LEFT/RIGHT walks the
//! pending cards (Question first), CANCEL snoozes a Question. A Ready card's
//! steps are answers; ENTER with none acknowledges it into the channel.
//! Without a host (and not the demo) cards are inert: nothing is emitted.
use super::{global, notify, Target, TouchAct, UiState, Update};
use crate::control::{Button, Control, Edge};
use crate::deck::model::{Effects, MAX_AGENTS};
use crate::deck::msg::{AgentKind, CardKind, DocKind, Event, HostMsg, PttCtx, Template};
use crate::deck::text::Text;
use crate::ui::kit::{age_step, list::Viewport};
use crate::ui::{faces, layout};

/// An answer the host has not cleared by then was not delivered.
pub const ANSWER_ACK_MS: u32 = 5000;
/// The roster re-sorts only after this much quiet.
pub const RESORT_QUIET_MS: u32 = 1500;
/// A host `focus` moves the highlight or the channel only after this.
pub const FOLLOW_QUIET_MS: u32 = 2000;
/// Ask for the next window of a question body this many lines before the
/// scroll reaches the edge of the one held, so an ordinary detent draws lines
/// that are already on the device.
/// How few lines may be left ahead of the reader before the next chunk is
/// asked for.
///
/// A chunk took **2.0 to 2.5 s** on the real link (measured 2026-09-23) and a
/// brisk scroll is about a line every 200 ms, so the buffer has to have
/// thirteen lines of road left when it asks or the wheel stalls anyway.
/// Fourteen gives 2.8 s, and `DOC_HELD` is deep enough to keep that much
/// ahead.
const WINDOW_MARGIN: usize = 14;

/// The chunk that extends what the deck holds, in the direction it is being
/// read, or `None` when the buffer already reaches the end it is heading for.
///
/// The deck asks for the lines it is **missing**, not for the line it is
/// looking at: a chunk centred on the reader would be half lines it already
/// has, and the buffer would creep forward a few lines at a time instead of
/// filling.
fn next_chunk(want: usize, visible: usize, first: usize, end: usize, total: usize) -> Option<u16> {
    if want + visible + WINDOW_MARGIN > end && end < total {
        return Some(end as u16);
    }
    if want < first + WINDOW_MARGIN && first > 0 {
        return Some(first.saturating_sub(crate::deck::msg::DOC_LINES) as u16);
    }
    None
}

/// A window the deck has asked for and is waiting on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Asked {
    doc: DocKind,
    agent: u8,
    id: u16,
    first: u16,
    at_ms: u32,
}

/// How long a request is allowed to be in flight before the deck asks again.
///
/// Measured against the real link on 2026-09-23: a window is 8-11 reports and
/// took **2.0 to 2.5 s** end to end, about 0.25 s a report. Five seconds is
/// clear of that, so a window actually on its way is never asked for twice,
/// while one that was lost — dropped by the host as stale, or cut by a link
/// blip — stops wedging the wheel for good.
const ASK_RETRY_MS: u32 = 5_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentsView {
    Roster,
    Channel,
    Card,
}

#[derive(Clone, Copy)]
pub struct View {
    pub settings: crate::ui::session::View,
    pub view: AgentsView,
    /// Agent id under the roster highlight; 0 = none.
    pub highlight: u8,
    /// Agent id of the Channel / Card view.
    pub agent: u8,
    /// Explicit companion focus waiting for the current held input to end.
    pub(super) deferred_focus: u8,
    /// Highlighted choice of the card; `None` on a Ready card = "open channel".
    pub choice: Option<u8>,
    /// When the on-screen (agent, card) last changed: the confirm guard.
    pub shown_at_ms: u32,
    /// First visible roster row.
    pub top_row: u8,
    /// First visible choice row.
    pub choice_top: u8,
    /// What a latched touch shows pressed; the meaning follows `view` (it is
    /// cleared on every view change), and it equals the latched `Target::id`:
    /// Roster = agent id (1..=8, identity, not the row); Channel = button
    /// index as in `layout::channel_button_at` (0, or 0/1 of the two choice
    /// buttons = the choice); Card = choice index 0..4 (identity in
    /// `card.choices`, not the visible slot) for Question and Ready alike.
    pub pressed: Option<u8>,
    /// First visible line of the channel's wrapped reply.
    pub reply_top: u16,
    pub reply_follow: bool,
    /// Revision latched at touch-down; a relayout must not toggle another tool.
    tool_pressed_rev: u16,
    /// A disclosure relayout is pending; reevaluate whether its view ends at the tail.
    tool_pending: Option<u16>,
    /// First visible line of the card's question body, counted in the whole
    /// text and never outside the window the card holds.
    pub question_top: u16,
    /// The window last asked for on the shown card: a detent inside the
    /// margin costs one line on the wire, not one per detent.
    /// The window last asked for: **which body**, not just which line.
    ///
    /// Keyed by the agent and the body's id as well as the line, because the
    /// same line number means different things for different agents and for a
    /// reply that has since been replaced. Keyed only by `(doc, first)` it
    /// swallowed a sibling's request as a duplicate, and at the edge of a held
    /// window — where the offset stops advancing, so the same line is asked
    /// for every detent — that swallowed request never came again and the
    /// wheel went dead.
    lines_asked: Option<Asked>,
    /// The project whose page the roster shows: the highlighted agent's.
    /// Empty = the agents without one (or an empty roster).
    pub project: Text<24>,
    /// Row the highlight was on its page, so a gone agent leaves it in place.
    highlight_row: u8,
    /// Which page `project` was, so a project that empties leaves the roster
    /// on its neighbour.
    project_row: u8,
    /// The displayed ages, so their rollover redraws.
    age_hash: u32,
    /// Id of the card the Card view shows (latched by `show_card`), so a
    /// clear of the card it replaced in the same message does not evict it.
    card: u16,
}

impl View {
    pub const fn new() -> Self {
        Self {
            settings: crate::ui::session::View::new(),
            view: AgentsView::Roster,
            highlight: 0,
            agent: 0,
            deferred_focus: 0,
            choice: None,
            shown_at_ms: 0,
            top_row: 0,
            choice_top: 0,
            pressed: None,
            reply_top: 0,
            reply_follow: true,
            tool_pressed_rev: 0,
            tool_pending: None,
            question_top: 0,
            lines_asked: None,
            project: Text::new(),
            highlight_row: 0,
            project_row: 0,
            age_hash: 0,
            card: 0,
        }
    }
}

fn order(ui: &UiState) -> &[u8] {
    &ui.deck.order[..(ui.deck.order_len as usize).min(MAX_AGENTS)]
}

fn roster_viewport(ui: &UiState) -> Viewport {
    let rows = ui.deck.page_len(ui.agents.project.as_str());
    Viewport::new(ui.agents.top_row, layout::ROSTER_VISIBLE, rows)
}

/// The first agent of every project, in roster order: the pages.
fn project_heads(ui: &UiState) -> ([u8; MAX_AGENTS], usize) {
    let mut heads = [0; MAX_AGENTS];
    let len = ui.deck.project_heads(&mut heads);
    (heads, len)
}

/// Which page `label` is, if there is one.
fn page_of(ui: &UiState, label: &str) -> Option<u8> {
    let (heads, len) = project_heads(ui);
    heads[..len]
        .iter()
        .position(|&head| ui.deck.project_of(head) == label)
        .map(|index| index as u8)
}

/// Highlight `agent` and turn to its project's page.
fn set_highlight(ui: &mut UiState, agent: u8) {
    ui.agents.highlight = agent;
    let Some(row) = ui.deck.page_row_of(agent) else {
        return;
    };
    ui.agents.project = ui.deck.agent(agent).map_or(Text::new(), |agent| agent.project);
    if let Some(page) = page_of(ui, ui.agents.project.as_str()) {
        ui.agents.project_row = page;
    }
    ui.agents.highlight_row = row;
    let mut viewport = roster_viewport(ui);
    viewport.follow(row);
    ui.agents.top_row = viewport.top;
}

/// The roster neighbour `step` rows from `agent`, across every page: what
/// the channel view's LEFT/RIGHT walk.
fn neighbour(ui: &UiState, agent: u8, step: i32) -> Option<u8> {
    let row = ui.deck.row_of(agent)? as i32 + step;
    usize::try_from(row).ok().and_then(|row| order(ui).get(row).copied())
}

/// The neighbour `step` rows from `agent` on its own page: what the roster's
/// wheel walks.
fn page_neighbour(ui: &UiState, agent: u8, step: i32) -> Option<u8> {
    let row = ui.deck.page_row_of(agent)? as i32 + step;
    let row = u8::try_from(row).ok()?;
    ui.deck
        .page_agent(ui.deck.project_of(agent), row)
        .map(|agent| agent.id)
}

/// The first agent of the page `step` pages from the one shown, if any.
fn neighbour_project(ui: &UiState, step: i32) -> Option<u8> {
    let (heads, len) = project_heads(ui);
    let page = page_of(ui, ui.agents.project.as_str())? as i32 + step;
    usize::try_from(page).ok().and_then(|page| heads[..len].get(page).copied())
}

/// The agent at `row` of the page shown.
fn page_agent(ui: &UiState, row: u8) -> Option<u8> {
    ui.deck
        .page_agent(ui.agents.project.as_str(), row)
        .map(|agent| agent.id)
}

/// After `UiState::reset` loaded its data: put the highlight on the first row.
pub(super) fn on_reset(ui: &mut UiState) {
    ui.image.open = false;
    ui.agents.settings = crate::ui::session::View::new();
    let first = order(ui).first().copied().unwrap_or(0);
    set_highlight(ui, first);
    ui.agents.age_hash = age_hash(ui, ui.now_ms);
}

/// Back to the roster (home jump, idle-home, the viewed agent left).
pub(super) fn show_roster(ui: &mut UiState, _at_ms: u32) {
    ui.image.open = false;
    ui.agents.settings.open = false;
    ui.agents.view = AgentsView::Roster;
    ui.agents.pressed = None;
}

pub fn reading_doc(ui: &UiState) -> DocKind {
    ui.deck.agent(ui.agents.agent).map_or(DocKind::Reply, |agent| agent.reply_kind)
}

fn toggle_workflow(ui: &mut UiState, at_ms: u32, out: &mut Update) {
    if !online(ui) { return; }
    let Some(agent) = ui.deck.agent_mut(ui.agents.agent) else { return; };
    agent.reply_kind = if agent.reply_kind == DocKind::Workflow { DocKind::Reply } else { DocKind::Workflow };
    agent.reply.forget();
    let (slot, doc, id) = (agent.id, agent.reply_kind, agent.reply_rev);
    ui.agents.reply_follow = doc == DocKind::Reply;
    ui.agents.reply_top = if ui.agents.reply_follow { u16::MAX } else { 0 };
    ui.agents.tool_pending = None;
    ui.agents.lines_asked = None;
    ui.agents.pressed = None;
    ask_doc_lines(ui, slot, doc, id, ui.agents.reply_top, at_ms, out);
    out.dirty = true;
}

/// `agent`'s channel (ENTER, a sibling, a jump whose card is gone, focus).
pub(super) fn show_channel(ui: &mut UiState, agent: u8, at_ms: u32) {
    // Slots can be reused after a session ends. Fetch settings on every entry.
    ui.image.open = false;
    ui.agents.settings = crate::ui::session::View::new();
    ui.agents.view = AgentsView::Channel;
    ui.agents.agent = agent;
    let workflow = reading_doc(ui) == DocKind::Workflow;
    ui.agents.reply_top = if !workflow && ui.deck.agent(agent).is_some_and(|a| a.reply.font.rich()) { u16::MAX } else { 0 };
    ui.agents.reply_follow = !workflow;
    ui.agents.tool_pending = None;
    ui.agents.pressed = None;
    set_highlight(ui, agent);
    global::arm_guard(ui, at_ms);
}

/// Show `agent`'s card (toast tap, drawer jump, auto-jump, ENTER).
pub(super) fn show_card(ui: &mut UiState, agent: u8, at_ms: u32) {
    let Some(card) = ui.deck.card(agent) else {
        return;
    };
    ui.image.open = false;
    ui.agents.choice = match card.kind {
        CardKind::Question => Some(card.recommended.unwrap_or(0)),
        CardKind::Ready => None,
    };
    ui.agents.view = AgentsView::Card;
    ui.agents.agent = agent;
    ui.agents.card = card.id;
    ui.agents.choice_top = 0;
    // The body opens at the top of the window the card holds — line 0 for
    // every card the host pushed a window with, and never a line the deck
    // does not have, which would open the card on an empty lane.
    ui.agents.question_top = card.body.first;
    ui.agents.lines_asked = None;
    ui.agents.pressed = None;
    ui.agents.shown_at_ms = at_ms;
    set_highlight(ui, agent);
    global::arm_guard(ui, at_ms);
}

/// Who a PTT press would talk to right now: (agent or 0, ctx, card or 0).
pub(super) fn ptt_target(ui: &UiState) -> (u8, PttCtx, u16) {
    let live = |agent: u8| ui.deck.agent(agent).map_or(0, |agent| agent.id);
    let on_agents = ui.global.overlay == global::Overlay::None
        && ui.current_face().template == Template::Agents;
    match ui.global.overlay {
        global::Overlay::Naming | global::Overlay::MacroEdit => return (0, PttCtx::Name, 0),
        global::Overlay::None => {}
        _ => return (live(ui.global.active_channel), PttCtx::Talk, 0),
    }
    match ui.agents.view {
        AgentsView::Card if on_agents => (
            live(ui.agents.agent),
            PttCtx::Answer,
            ui.deck.card(ui.agents.agent).map_or(0, |card| card.id),
        ),
        AgentsView::Channel if on_agents => (live(ui.agents.agent), PttCtx::Talk, 0),
        AgentsView::Roster if on_agents && live(ui.agents.highlight) != 0 => {
            (ui.agents.highlight, PttCtx::Talk, 0)
        }
        _ => (live(ui.global.active_channel), PttCtx::Talk, 0),
    }
}

/// Companion focus completes an explicit chat action (including creation just
/// after PTT release). It must not be lost to the terminal-follow quiet
/// period. Held inputs defer it; a new navigation cancels it. Cards stay put.
pub(super) fn on_focus(ui: &mut UiState, agent: u8, at_ms: u32, out: &mut Update) {
    ui.agents.deferred_focus = 0;
    if ui.agents.view == AgentsView::Card {
        return;
    }
    let native = ui.deck.agent(agent).is_some_and(|a| matches!(a.kind, AgentKind::Conol | AgentKind::Other));
    if global::anything_held(ui) || ui.touch.active() || (native && ui.global.shift) {
        if native {
            ui.agents.deferred_focus = agent;
        }
        return;
    }
    if !native && !global::quiet_for(ui, at_ms, FOLLOW_QUIET_MS) {
        return;
    }
    match ui.agents.view {
        AgentsView::Roster if native => {
            show_channel(ui, agent, at_ms);
            out.dirty = true;
        }
        AgentsView::Roster if ui.agents.highlight != agent => {
            set_highlight(ui, agent);
            out.dirty = true;
        }
        AgentsView::Channel if ui.agents.agent != agent => {
            show_channel(ui, agent, at_ms);
            out.dirty = true;
        }
        _ => {}
    }
}

/// Answers may be sent (a host, or the demo answering for one).
fn online(ui: &UiState) -> bool {
    ui.deck.host_present || ui.deck.demo_active()
}

/// Send `choice` for `agent`'s card: it locks as "sent" and the view returns
/// to the channel. Nothing happens offline, on a locked card or out of range.
fn answer(ui: &mut UiState, agent: u8, choice: u8, at_ms: u32, out: &mut Update) {
    if !online(ui) {
        return;
    }
    let Some(card) = ui.deck.card(agent) else {
        return;
    };
    if card.pending_answer.is_some() || choice >= card.choice_count {
        return;
    }
    let id = card.id;
    if let Some(owner) = ui.deck.agent_mut(agent) {
        owner.card.pending_answer = Some(choice);
        owner.card.sent_at_ms = at_ms;
    }
    out.push_deck(Event::Answer {
        agent,
        card: id,
        choice,
    });
    notify::remove(ui, agent, id, out);
    show_channel(ui, agent, at_ms);
    out.dirty = true;
}

/// Agents with an open card, Questions first, each in roster order: what
/// LEFT/RIGHT on a card walks.
fn pending_cards(ui: &UiState, out: &mut [u8; MAX_AGENTS]) -> usize {
    let mut len = 0;
    for kind in [CardKind::Question, CardKind::Ready] {
        for &agent in order(ui) {
            if ui.deck.card(agent).is_some_and(|card| card.kind == kind && card.open()) && len < MAX_AGENTS {
                out[len] = agent;
                len += 1;
            }
        }
    }
    len
}

fn choice_count(ui: &UiState) -> u8 {
    ui.deck
        .card(ui.agents.agent)
        .map_or(0, |card| card.choice_count.min(4))
}

/// The choice cannot step down any further, so the wheel's backwards travel
/// belongs to the question body above it.
fn choice_at_floor(ui: &UiState) -> bool {
    let ready = ui.deck.card(ui.agents.agent).is_some_and(|card| card.kind == CardKind::Ready);
    let floor = if ready { -1 } else { 0 };
    ui.agents.choice.map_or(-1, |choice| choice as i32) <= floor
}

/// Ask the host for the window of `doc` that holds line `first`. Nothing is
/// asked offline: what the deck holds is what it shows.
fn ask_doc_lines(
    ui: &mut UiState,
    agent: u8,
    doc: DocKind,
    id: u16,
    first: u16,
    at_ms: u32,
    out: &mut Update,
) {
    if !online(ui) {
        return;
    }
    let pending = ui.agents.lines_asked.is_some_and(|asked| {
        (asked.doc, asked.agent, asked.id, asked.first) == (doc, agent, id, first)
            && at_ms.wrapping_sub(asked.at_ms) < ASK_RETRY_MS
    });
    if pending {
        return;
    }
    ui.agents.lines_asked = Some(Asked {
        doc,
        agent,
        id,
        first,
        at_ms,
    });
    out.push_deck(Event::DocLines {
        agent,
        doc,
        id,
        first,
    });
}

/// Walk the question body by `step`, asking for the window it is walking
/// into while there is still time to send it. True when the wheel belonged to
/// the body: it moved, or it is waiting on lines the host has not sent yet —
/// a wheel stalled at the edge of the held window must hold, not jump into
/// the choices. A body the host sent no window for never scrolls.
fn scroll_question(ui: &mut UiState, step: i32, at_ms: u32, out: &mut Update) -> bool {
    let Some(card) = ui.deck.card(ui.agents.agent) else {
        return false;
    };
    if card.kind != CardKind::Question || !card.body.windowed() {
        return false;
    }
    let visible = faces::agents::question_visible(card);
    let (id, first, held, total) =
        (card.id, card.body.first as usize, card.body.held as usize, card.body.total as usize);
    let want = (ui.agents.question_top as i32 + step).clamp(0, total.saturating_sub(visible) as i32);
    if want == ui.agents.question_top as i32 {
        // The body is at the end the wheel is turning towards: the rest of
        // this turn is the choices'.
        return false;
    }
    let want = want as u16;
    let shown = faces::agents::question_clamp(card, want);
    if shown != ui.agents.question_top {
        ui.agents.question_top = shown;
        out.dirty = true;
    }
    let agent = ui.agents.agent;
    if let Some(chunk) = next_chunk(want as usize, visible, first, first + held, total) {
        ask_doc_lines(ui, agent, DocKind::Card, id, chunk, at_ms, out);
    }
    true
}

/// Walk the reply by `step`, asking for the window it is walking into while
/// there is still time to send it. The mirror of `scroll_question`: a reply the
/// host laid out is scrolled inside the window the deck holds, and the deck
/// stops at that window's edge rather than blanking the lane when the host is
/// slow or away.
fn scroll_reply(ui: &mut UiState, step: i32, at_ms: u32, out: &mut Update) {
    let agent_slot = ui.agents.agent;
    let Some(agent) = ui.deck.agent(agent_slot) else {
        return;
    };
    let doc = agent.reply_kind;
    let visible = faces::agents::reply_visible(ui, agent);
    let total = faces::agents::reply_lines(ui, agent);
    let (windowed, id, first, end) = (
        agent.reply.windowed(),
        agent.reply_rev,
        faces::agents::reply_first(agent),
        faces::agents::reply_end(ui, agent),
    );
    let current = faces::agents::reply_clamp(ui, agent, ui.agents.reply_top);
    let want = (current as i32 + step).clamp(0, total.saturating_sub(visible) as i32);
    // Reaching the actual document end resumes following, including when
    // the last window still needs to arrive from the host.
    ui.agents.reply_follow = agent.reply_kind == DocKind::Reply && agent.reply.font.rich() && want as usize + visible >= total;
    let shown = faces::agents::reply_clamp(ui, agent, want.max(0) as u16);
    if shown != ui.agents.reply_top {
        ui.agents.reply_top = shown;
        out.dirty = true;
    }
    if !windowed {
        return;
    }
    let rich = agent.reply.font.rich();
    let want = want.max(0) as usize;
    if ui.agents.reply_follow {
        ui.agents.reply_top = u16::MAX;
        ask_doc_lines(ui, agent_slot, doc, id, u16::MAX, at_ms, out);
    } else if let Some(chunk) = next_chunk(want, visible, first, end, total) {
        ask_doc_lines(ui, agent_slot, doc, id, chunk, at_ms, out);
    } else if rich {
        // Tell the host where we are reading, even inside the held buffer.
        ask_doc_lines(ui, agent_slot, doc, id, want as u16, at_ms, out);
    }
}

fn open_card(ui: &mut UiState, agent: u8, at_ms: u32, out: &mut Update) {
    let Some(card) = ui.deck.card(agent) else {
        return;
    };
    if !card.open() {
        return;
    }
    let id = card.id;
    show_card(ui, agent, at_ms);
    notify::dismiss_toast_for(ui, agent, id, out);
    out.dirty = true;
}

pub(super) fn on_control(
    ui: &mut UiState,
    _instance: u8,
    control: Control,
    at_ms: u32,
    out: &mut Update,
) -> bool {
    if crate::ui::image::control(ui, control, out) { return true; }
    if crate::ui::session::control(ui, control, at_ms, out) { return true; }
    if ui.global.shift {
        if ui.agents.view == AgentsView::Channel && control == Control::Button(Button::Enter, Edge::Press) {
            let slot = ui.agents.agent;
            if let Some(agent) = ui.deck.agent(slot) {
                let (id, doc) = (agent.reply_rev, agent.reply_kind);
                ui.agents.reply_follow = doc == DocKind::Reply;
                ui.agents.reply_top = if ui.agents.reply_follow { u16::MAX } else { 0 };
                ask_doc_lines(ui, slot, doc, id, ui.agents.reply_top, at_ms, out);
                out.dirty = true;
            }
        }
        return true;
    }
    let step = super::selection_step;
    match (ui.agents.view, control) {
        (AgentsView::Roster, Control::Wheel(wheel)) => {
            if let Some(next) = page_neighbour(ui, ui.agents.highlight, step(wheel)) {
                set_highlight(ui, next);
                out.dirty = true;
            }
        }
        (AgentsView::Roster, Control::Button(button @ (Button::Left | Button::Right), Edge::Press)) => {
            // Turn the page: the neighbouring project, its first row
            // highlighted. Nothing is said to the host — the active channel
            // changes when a channel is opened, not when pages are turned.
            let step = if button == Button::Right { 1 } else { -1 };
            if let Some(next) = neighbour_project(ui, step) {
                set_highlight(ui, next);
                out.dirty = true;
            }
        }
        (AgentsView::Roster, Control::Button(Button::Enter, Edge::Press)) => {
            let agent = ui.agents.highlight;
            if ui.deck.agent(agent).is_some() {
                show_channel(ui, agent, at_ms);
                global::emit_channel(ui, agent, out);
                out.dirty = true;
            }
        }
        (AgentsView::Channel, Control::Button(Button::Enter, Edge::Press)) => {
            if ui.deck.card(ui.agents.agent).is_some_and(|card| card.open()) {
                open_card(ui, ui.agents.agent, at_ms, out);
            } else {
                toggle_workflow(ui, at_ms, out);
            }
        }
        (AgentsView::Channel, Control::Button(button @ (Button::Left | Button::Right), Edge::Press)) => {
            let step = if button == Button::Right { 1 } else { -1 };
            if let Some(next) = neighbour(ui, ui.agents.agent, step) {
                show_channel(ui, next, at_ms);
                global::emit_channel(ui, next, out);
                out.dirty = true;
            }
        }
        (AgentsView::Channel, Control::Button(Button::Cancel, Edge::Press)) => {
            if reading_doc(ui) == DocKind::Workflow && online(ui) {
                toggle_workflow(ui, at_ms, out);
            } else {
                show_roster(ui, at_ms);
                out.dirty = true;
            }
        }
        (AgentsView::Channel, Control::Wheel(wheel)) => {
            scroll_reply(ui, step(wheel), at_ms, out);
        }
        (AgentsView::Card, Control::Wheel(wheel)) => {
            // One wheel for the whole card: the question body's hidden lines
            // first, the choices after them. Forwards the body is walked
            // before the choices are; backwards the choices are walked back
            // to their first before the body is, because the body is above
            // them. A body that hides nothing takes no part in either, so the
            // wheel still lands straight on the choices.
            let step = step(wheel);
            let body = if step > 0 || choice_at_floor(ui) {
                scroll_question(ui, step, at_ms, out)
            } else {
                false
            };
            let count = choice_count(ui) as i32;
            let ready = ui.deck.card(ui.agents.agent).is_some_and(|c| c.kind == CardKind::Ready);
            // A Ready card's "none" sits before its first step.
            let current = ui.agents.choice.map_or(-1, |choice| choice as i32);
            let next = current + step;
            let floor = if ready { -1 } else { 0 };
            if !body && (floor..count).contains(&next) && next != current {
                ui.agents.choice = u8::try_from(next).ok();
                let mut viewport = Viewport::new(ui.agents.choice_top, layout::CARD_VISIBLE, count as u8);
                if let Some(choice) = ui.agents.choice {
                    viewport.follow(choice);
                }
                ui.agents.choice_top = viewport.top;
                out.dirty = true;
            }
        }
        (AgentsView::Card, Control::Button(Button::Enter, Edge::Press)) => {
            let agent = ui.agents.agent;
            match (ui.deck.card(agent).map(|c| c.kind), ui.agents.choice) {
                (Some(_), Some(choice)) => {
                    if global::guard_passed(ui, at_ms, at_ms) {
                        answer(ui, agent, choice, at_ms, out);
                    }
                }
                (Some(CardKind::Ready), None) => {
                    let id = ui.deck.card(agent).map_or(0, |c| c.id);
                    if let Some(owner) = ui.deck.agent_mut(agent) {
                        owner.card.acknowledged = true;
                    }
                    notify::remove(ui, agent, id, out);
                    show_channel(ui, agent, at_ms);
                    out.dirty = true;
                }
                _ => {}
            }
        }
        (AgentsView::Card, Control::Button(button @ (Button::Left | Button::Right), Edge::Press)) => {
            let mut cards = [0; MAX_AGENTS];
            let len = pending_cards(ui, &mut cards);
            let step = if button == Button::Right { 1 } else { -1 };
            let next = cards[..len]
                .iter()
                .position(|&agent| agent == ui.agents.agent)
                .and_then(|index| usize::try_from(index as i32 + step).ok())
                .and_then(|index| cards[..len].get(index).copied());
            if let Some(next) = next {
                show_card(ui, next, at_ms);
                global::emit_channel(ui, next, out);
                out.dirty = true;
            }
        }
        (AgentsView::Card, Control::Button(Button::Cancel, Edge::Press)) => {
            let agent = ui.agents.agent;
            if let Some(card) = ui.deck.card(agent) {
                let (id, snooze) =
                    (card.id, card.kind == CardKind::Question && card.pending_answer.is_none() && online(ui));
                if snooze {
                    if let Some(owner) = ui.deck.agent_mut(agent) {
                        owner.card.snoozed = true;
                    }
                    out.push_deck(Event::Snooze { agent, card: id });
                    notify::file(ui, agent, id, at_ms, out);
                }
            }
            if ui.deck.agent(agent).is_some() {
                show_channel(ui, agent, at_ms);
            } else {
                show_roster(ui, at_ms);
            }
            out.dirty = true;
        }
        _ => {}
    }
    true
}

pub(super) fn hit(ui: &UiState, _instance: u8, x: i32, y: i32) -> Option<Target> {
    if ui.image.open { return crate::ui::image::hit(x, y); }
    if ui.agents.settings.open && ui.agents.view == AgentsView::Channel {
        return crate::ui::session::hit(ui, x, y);
    }
    match ui.agents.view {
        AgentsView::Roster => {
            let slot = layout::roster_row_at(x, y)?;
            let row = roster_viewport(ui).index_at(slot)?;
            let agent = page_agent(ui, row)?;
            Some(Target::list_row(layout::roster_row(slot), agent as u16, layout::ROSTER_PITCH))
        }
        AgentsView::Channel => {
            if crate::ui::session::ENTRY.contains(x, y) {
                return Some(Target::tap(crate::ui::session::ENTRY, 3));
            }
            if online(ui) {
                if faces::agents::WORKFLOW_TOGGLE.contains(x, y) {
                    return Some(Target::tap(faces::agents::WORKFLOW_TOGGLE, 2));
                }
                if let Some(agent) = ui.deck.agent(ui.agents.agent).filter(|a| a.reply.font.rich() && a.reply.id == a.reply_rev) {
                    let lane = faces::agents::reply_rect(ui, agent);
                    let lh = crate::ui::font::line_height(faces::agents::doc_face(agent.reply.font));
                    if lane.contains(x, y) {
                        let row = (y - lane.top()) / lh;
                        if (row as usize) < faces::agents::reply_visible(ui, agent) {
                            let top = faces::agents::reply_clamp(ui, agent, ui.agents.reply_top);
                            if let Some(index) = agent.reply.line(top as usize + row as usize).and_then(crate::ui::rich::image) {
                                return Some(Target::tap(crate::ui::layout::Rect::new(lane.left(), lane.top() + row * lh, lane.width(), lh), 256 + index as u16));
                            }
                            if let Some((index, _)) = agent.reply.line(top as usize + row as usize).and_then(crate::ui::rich::tool) {
                                return Some(Target::tap(crate::ui::layout::Rect::new(lane.left(), lane.top() + row * lh, lane.width(), lh), 16 + index as u16));
                            }
                        }
                    }
                }
            }
            let card = ui.deck.card(ui.agents.agent).filter(|card| card.open())?;
            let two = card.kind == CardKind::Question && card.choice_count == 2;
            let index = layout::channel_button_at(x, y, two)?;
            let rect = if two { layout::channel_choice_button(index) } else { layout::CHANNEL_BUTTON };
            Some(Target::tap(rect, index as u16))
        }
        AgentsView::Card => {
            let card = ui.deck.card(ui.agents.agent)?;
            let (slot, rect, visible) = match card.kind {
                CardKind::Question => {
                    let slot = layout::card_choice_at(x, y)?;
                    (slot, layout::card_choice(slot), layout::CARD_VISIBLE)
                }
                CardKind::Ready => {
                    let slot = layout::ready_row_at(x, y)?;
                    (slot, layout::ready_row(slot), layout::READY_VISIBLE)
                }
            };
            let index = Viewport::new(ui.agents.choice_top, visible, card.choice_count.min(4)).index_at(slot)?;
            Some(Target::tap(rect, index as u16))
        }
    }
}

pub(super) fn on_touch(
    ui: &mut UiState,
    _instance: u8,
    target: Target,
    act: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    if ui.agents.settings.open && ui.agents.view == AgentsView::Channel {
        crate::ui::session::touch(ui, target, act, at_ms, out);
        return;
    }
    if ui.image.open { crate::ui::image::touch(ui, act, out); return; }
    if target.id >= 256 {
        if matches!(act, TouchAct::Down) { ui.agents.tool_pressed_rev = ui.deck.agent(ui.agents.agent).map_or(0, |a| a.reply.id); }
        if matches!(act, TouchAct::Activate) {
            let (x, y) = target.rect.center();
            if hit(ui, 0, x, y).is_some_and(|t| t.id == target.id)
                && ui.deck.agent(ui.agents.agent).is_some_and(|a| a.reply.id == ui.agents.tool_pressed_rev) {
                crate::ui::image::open(ui, (target.id - 256) as u8, ui.agents.tool_pressed_rev, at_ms, out);
            }
        }
        return;
    }
    let id = target.id as u8;
    match (ui.agents.view, act) {
        (_, TouchAct::Down) => {
            ui.agents.pressed = Some(id);
            if ui.agents.view == AgentsView::Channel && id >= 16 {
                ui.agents.tool_pressed_rev = ui.deck.agent(ui.agents.agent).map_or(0, |a| a.reply.id);
            }
            if ui.agents.view == AgentsView::Card
                && ui.deck.card(ui.agents.agent).is_some_and(|c| c.kind == CardKind::Question)
                && id < choice_count(ui)
            {
                ui.agents.choice = Some(id);
            }
        }
        (_, TouchAct::Cancel | TouchAct::Swipe { .. }) => ui.agents.pressed = None,
        (AgentsView::Roster, TouchAct::Scroll(rows)) => {
            ui.agents.pressed = None;
            let mut viewport = roster_viewport(ui);
            viewport.scroll(rows as i32);
            ui.agents.top_row = viewport.top;
        }
        (AgentsView::Roster, TouchAct::Activate) => {
            ui.agents.pressed = None;
            // The same agent must still be under the finger.
            let (x, y) = target.rect.center();
            if hit(ui, 0, x, y).is_none_or(|now| now.id != target.id) || ui.deck.agent(id).is_none() {
                return;
            }
            if ui.agents.highlight == id {
                show_channel(ui, id, at_ms);
                global::emit_channel(ui, id, out);
            } else {
                set_highlight(ui, id);
            }
        }
        (AgentsView::Channel, TouchAct::Activate) => {
            ui.agents.pressed = None;
            let agent = ui.agents.agent;
            if id == 3 { crate::ui::session::open(ui, at_ms, out); return; }
            if id == 2 {
                toggle_workflow(ui, at_ms, out);
                return;
            }
            if id >= 16 {
                let (x, y) = target.rect.center();
                if hit(ui, 0, x, y).is_none_or(|now| now.id != target.id) { return; }
                let Some(owner) = ui.deck.agent(agent).filter(|a| a.reply.id == ui.agents.tool_pressed_rev) else { return; };
                let top = faces::agents::reply_clamp(ui, owner, ui.agents.reply_top);
                let lane = faces::agents::reply_rect(ui, owner);
                let lh = crate::ui::font::line_height(faces::agents::doc_face(owner.reply.font));
                ui.agents.reply_top = top + ((target.rect.top() - lane.top()) / lh) as u16;
                ui.agents.reply_follow = false;
                ui.agents.tool_pending = Some(owner.reply.id);
                ui.agents.lines_asked = None;
                out.push_deck(Event::Tool { agent, id: owner.reply.id, index: id - 16 });
                out.dirty = true;
                return;
            }
            let two = ui
                .deck
                .card(agent)
                .is_some_and(|c| c.kind == CardKind::Question && c.choice_count == 2);
            if two {
                if global::guard_passed(ui, ui.touch.down_ms(), at_ms) {
                    answer(ui, agent, id, at_ms, out);
                }
            } else {
                open_card(ui, agent, at_ms, out);
            }
        }
        (AgentsView::Card, TouchAct::Activate) => {
            ui.agents.pressed = None;
            if global::guard_passed(ui, ui.touch.down_ms(), at_ms) {
                answer(ui, ui.agents.agent, id, at_ms, out);
            }
        }
        _ => return,
    }
    out.dirty = true;
}

pub(super) fn on_host(
    ui: &mut UiState,
    msg: &HostMsg,
    fx: &Effects,
    at_ms: u32,
    out: &mut Update,
) {
    validate(ui, Some(fx), at_ms, out);
    crate::ui::image::host(ui, msg, out);
    crate::ui::session::host(ui, msg, out);
    if matches!(msg, HostMsg::Synced) && ui.agents.view == AgentsView::Channel && reading_doc(ui) == DocKind::Workflow {
        if let Some(agent) = ui.deck.agent(ui.agents.agent) {
            let (slot, id) = (agent.id, agent.reply_rev);
            ui.agents.lines_asked = None;
            ask_doc_lines(ui, slot, DocKind::Workflow, id, ui.agents.reply_top, at_ms, out);
        }
    }
    if let HostMsg::DocText { agent, doc, id, font, .. } = msg {
        if *agent == ui.agents.agent && *doc == reading_doc(ui) && matches!(doc, DocKind::Reply | DocKind::Workflow) && font.rich() && ui.deck.agent(*agent).is_some_and(|a| a.reply.id == *id) {
            if ui.agents.tool_pending.is_some_and(|previous| previous != *id) {
                ui.agents.tool_pending = None;
                if let Some(owner) = ui.deck.agent(*agent) {
                    let top = faces::agents::reply_clamp(ui, owner, ui.agents.reply_top) as usize;
                    ui.agents.reply_follow = *doc == DocKind::Reply && top + faces::agents::reply_visible(ui, owner) >= owner.reply.total as usize;
                    if ui.agents.reply_follow { ask_doc_lines(ui, *agent, DocKind::Reply, *id, u16::MAX, at_ms, out); }
                }
            }
            if ui.agents.reply_follow { ui.agents.reply_top = u16::MAX; }
        }
    }
    // The message itself reported dirty; the ages it set are not a rollover.
    ui.agents.age_hash = age_hash(ui, at_ms);
}

/// Never show an agent or card that the model no longer has (a host message,
/// or a sync purged in `tick`); keep the highlight and the viewport sane.
fn validate(ui: &mut UiState, fx: Option<&Effects>, at_ms: u32, out: &mut Update) {
    let cleared = |agent: u8| fx.is_some_and(|fx| fx.card_cleared.is_some_and(|(a, _)| a == agent));
    let noticed = |agent: u8| fx.is_some_and(|fx| fx.notice.is_some_and(|n| n.agent == agent));
    // The shown card itself went (not the one a jump in this same message
    // already replaced it with).
    let shown_cleared = fx.is_some_and(|fx| fx.card_cleared == Some((ui.agents.agent, ui.agents.card)));
    // A gone highlight keeps its row on its page (clamped); a page that went
    // with it keeps its place among the pages; an empty roster has none.
    if ui.deck.agent(ui.agents.highlight).is_none() {
        let rows = ui.deck.page_len(ui.agents.project.as_str());
        let next = if rows > 0 {
            page_agent(ui, ui.agents.highlight_row.min(rows - 1)).unwrap_or(0)
        } else {
            let (heads, len) = project_heads(ui);
            heads[..len]
                .get((ui.agents.project_row as usize).min(len.saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        if next != ui.agents.highlight {
            set_highlight(ui, next);
            out.dirty = true;
        }
    } else if ui.deck.project_of(ui.agents.highlight) != ui.agents.project.as_str() {
        // The host moved the highlighted agent to another project: the page
        // follows it.
        let highlight = ui.agents.highlight;
        set_highlight(ui, highlight);
        out.dirty = true;
    }
    let viewed = ui.agents.agent;
    match ui.agents.view {
        // Never keep showing a card that is gone or was swapped under the
        // user: back to the channel (or the roster), under the guard, never
        // on to another card.
        AgentsView::Card if ui.deck.card(viewed).is_none() || shown_cleared => {
            if ui.deck.agent(viewed).is_some() {
                show_channel(ui, viewed, at_ms);
            } else {
                show_roster(ui, at_ms);
                global::arm_guard(ui, at_ms);
            }
            out.dirty = true;
        }
        AgentsView::Card => {
            // A re-push with fewer choices: the choice stays in range.
            let count = choice_count(ui);
            if ui.agents.choice.is_some_and(|choice| choice >= count) {
                ui.agents.choice = (count != 0).then_some(0);
                ui.agents.choice_top = 0;
                out.dirty = true;
            }
            // A window that moved (or an edit that took one away): the scroll
            // offset stays inside the lines the deck holds, so the lane never
            // draws a row with nothing behind it.
            let top = ui
                .deck
                .card(viewed)
                .map_or(0, |card| faces::agents::question_clamp(card, ui.agents.question_top));
            if top != ui.agents.question_top {
                ui.agents.question_top = top;
                out.dirty = true;
            }
        }
        AgentsView::Channel if ui.deck.agent(viewed).is_none() => {
            show_roster(ui, at_ms);
            global::arm_guard(ui, at_ms);
            out.dirty = true;
        }
        // The channel's card block changed under the user's finger.
        AgentsView::Channel if cleared(viewed) || noticed(viewed) => {
            global::arm_guard(ui, at_ms);
        }
        _ => {}
    }
    let mut viewport = roster_viewport(ui);
    if viewport.top != ui.agents.top_row {
        ui.agents.top_row = viewport.top;
        out.dirty = true;
    }
    if let Some(row) = ui.deck.page_row_of(ui.agents.highlight) {
        viewport.follow(row);
        if viewport.top != ui.agents.top_row {
            ui.agents.top_row = viewport.top;
            out.dirty = true;
        }
    }
}

/// Displayed ages roll over, the unacknowledged-answer timeout, the re-sort.
pub(super) fn on_tick(ui: &mut UiState, now_ms: u32, out: &mut Update) {
    validate(ui, None, now_ms, out);
    let deferred = ui.agents.deferred_focus;
    if deferred != 0 {
        if deferred != ui.global.active_channel || ui.deck.agent(deferred).is_none() {
            ui.agents.deferred_focus = 0;
        } else if !global::anything_held(ui) && !ui.touch.active() && !ui.global.shift {
            on_focus(ui, deferred, now_ms, out);
        }
    }
    crate::ui::image::tick(ui, now_ms, out);
    crate::ui::session::tick(ui, now_ms, out);
    // A dropped or revision-stale mode request must not leave an empty view.
    if ui.agents.view == AgentsView::Channel {
        if let Some(agent) = ui.deck.agent(ui.agents.agent) {
            if (!agent.reply.windowed() || agent.reply.id != agent.reply_rev)
                && (agent.reply_kind == DocKind::Workflow || ui.agents.lines_asked.is_some()) {
                let (slot, doc, id) = (agent.id, agent.reply_kind, agent.reply_rev);
                ask_doc_lines(ui, slot, doc, id, ui.agents.reply_top, now_ms, out);
            }
        }
    }
    for slot in 0..MAX_AGENTS {
        let agent = &ui.deck.agents[slot];
        if agent.present
            && agent.has_card
            && agent.card.pending_answer.is_some()
            && now_ms.wrapping_sub(agent.card.sent_at_ms) >= ANSWER_ACK_MS
        {
            let (id, card) = (agent.id, agent.card.id);
            ui.deck.agents[slot].card.pending_answer = None;
            notify::raise_message(ui, notify::ToastKind::NotDelivered, id, card, now_ms, out);
            out.dirty = true;
        }
    }
    if ui.deck.sort_pending()
        && global::quiet_for(ui, now_ms, RESORT_QUIET_MS)
        && !global::anything_held(ui)
        && !ui.touch.active()
    {
        ui.deck.sort();
        let highlight = ui.agents.highlight;
        set_highlight(ui, highlight);
        out.dirty = true;
    }
    // An age rolled over: only a visible Agents face repaints for it (the
    // roster lanes hash the step, so a hidden face catches up when shown).
    let hash = age_hash(ui, now_ms);
    if hash != ui.agents.age_hash {
        ui.agents.age_hash = hash;
        out.dirty |= global::lit(ui) && agents_visible(ui);
    }
}

fn age_hash(ui: &UiState, now_ms: u32) -> u32 {
    let mut hash = 0u32;
    for agent in ui.deck.agents.iter().filter(|agent| agent.present) {
        let age = age_step(now_ms, agent.state_since_ms);
        hash = hash.wrapping_mul(31).wrapping_add(age ^ (agent.id as u32) << 24);
    }
    hash
}

fn agents_visible(ui: &UiState) -> bool {
    ui.visible_faces()
        .iter()
        .flatten()
        .any(|visible| visible.face.is_some_and(|face| face.template == Template::Agents))
}

pub(super) fn animating(_ui: &UiState) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Wheel;
    use crate::control::{Touch, TouchPhase};
    use crate::deck::msg::AgentState;
    use crate::deck::text::Text;
    use crate::ui::state::ToastKind;

    /// A real host (hello at 0) with the demo roster: 1 Question (card 1,
    /// 3 choices, recommended 0), 2 Ready (card 2, 2 steps), 3 Working,
    /// 4 Idle, 5 Error; order [1, 2, 3, 4, 5], no drawer entries.
    fn hosted() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.host(&HostMsg::Hello { v: 1, sid: 1, minute_of_day: None }, 0, &mut Update::new());
        crate::deck::demo::load(&mut ui.deck);
        ui.deck.settings.demo = false;
        for notice in &mut ui.deck.notices {
            notice.used = false;
        }
        on_reset(&mut ui);
        ui.invalidate();
        ui
    }

    fn press(ui: &mut UiState, button: Button, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(Control::Button(button, Edge::Press), at, &mut out);
        ui.apply(Control::Button(button, Edge::Release), at + 1, &mut Update::new());
        out
    }
    fn wheel(ui: &mut UiState, wheel: Wheel, at: u32) -> Update {
        let mut out = Update::new();
        ui.apply(Control::Wheel(wheel), at, &mut out);
        out
    }
    fn tap(ui: &mut UiState, x: i32, y: i32, at: u32) -> Update {
        let mut out = Update::new();
        let touch = |phase| Control::Touch(Touch { phase, x: x as u16, y: y as u16 });
        ui.apply(touch(TouchPhase::Down), at, &mut Update::new());
        ui.apply(touch(TouchPhase::Up), at + 20, &mut out);
        out
    }
    fn events(out: &Update) -> std::vec::Vec<Event> {
        out.deck_events().collect()
    }
    fn agent(id: u8, state: AgentState) -> HostMsg {
        HostMsg::Agent {
            id,
            gone: false,
            kind: None,
            state: Some(state),
            name: None,
            project: None,
            title: None,
            since_s: None,
        }
    }
    fn gone(id: u8) -> HostMsg {
        let mut msg = agent(id, AgentState::Idle);
        if let HostMsg::Agent { gone, .. } = &mut msg {
            *gone = true;
        }
        msg
    }
    fn in_project(id: u8, state: AgentState, project: &str) -> HostMsg {
        let mut msg = agent(id, state);
        if let HostMsg::Agent { project: slot, .. } = &mut msg {
            *slot = Some(Text::from_str(project));
        }
        msg
    }
    fn card(agent: u8, id: u16, kind: CardKind, choices: u8) -> HostMsg {
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind,
            text: Text::from_str("Ship it?"),
            choices: [Text::from_str("x"); 4],
            choice_count: choices,
            recommended: None,
        }
    }
    /// `lines` of the `total` a card's body wraps to, starting at `first`,
    /// in the font the host measured them with (Name: three fit the lane).
    fn window(agent: u8, id: u16, first: u16, total: u16, lines: &[&str]) -> HostMsg {
        let mut held = [Text::new(); crate::deck::msg::DOC_LINES];
        for (slot, line) in lines.iter().enumerate().take(held.len()) {
            held[slot].set(line);
        }
        HostMsg::DocText {
            agent,
            doc: DocKind::Card,
            id,
            first,
            total,
            font: crate::deck::msg::DocFont::Name,
            lines: held,
            line_count: lines.len().min(held.len()) as u8,
        }
    }
    /// A body of `total` lines, of which the deck holds the 12 from `first`.
    fn long_window(agent: u8, id: u16, first: u16, total: u16) -> HostMsg {
        let text: std::vec::Vec<std::string::String> = (first
            ..first + crate::deck::msg::DOC_LINES as u16)
            .map(|line| std::format!("line {line}"))
            .collect();
        let lines: std::vec::Vec<&str> = text.iter().map(std::string::String::as_str).collect();
        window(agent, id, first, total, &lines)
    }
    fn clear(agent: u8, id: u16) -> HostMsg {
        let mut msg = card(agent, id, CardKind::Question, 0);
        if let HostMsg::Card { clear, .. } = &mut msg {
            *clear = true;
        }
        msg
    }
    fn host(ui: &mut UiState, msg: &HostMsg, at: u32) -> Update {
        let mut out = Update::new();
        ui.host(msg, at, &mut out);
        out
    }

    #[test]
    fn left_right_turn_the_project_pages_and_the_wheel_walks_one_page() {
        let mut ui = hosted();
        ui.global.active_channel = 1;
        let [cohdl, openmicro] = crate::deck::demo::PROJECTS;
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (1, cohdl));
        // The wheel walks the page's three rows, hard ends.
        wheel(&mut ui, Wheel::Ccw, 10);
        wheel(&mut ui, Wheel::Ccw, 20);
        assert_eq!((ui.agents.highlight, ui.agents.highlight_row), (3, 2));
        assert!(!wheel(&mut ui, Wheel::Ccw, 30).dirty, "the page ends at its last row");
        assert_eq!(ui.agents.highlight, 3);
        // RIGHT turns to the next project: its first row, and nothing is
        // said to the host — turning pages is looking, not talking.
        let out = press(&mut ui, Button::Right, 40);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (4, openmicro));
        assert_eq!((ui.agents.top_row, ui.agents.highlight_row, ui.agents.project_row), (0, 0, 1));
        assert!(out.dirty && events(&out).is_empty());
        assert_eq!(ui.global.active_channel, 1);
        wheel(&mut ui, Wheel::Ccw, 50);
        assert_eq!(ui.agents.highlight, 5);
        // Hard ends on both sides.
        press(&mut ui, Button::Right, 60);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (5, openmicro));
        press(&mut ui, Button::Left, 70);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (1, cohdl));
        assert_eq!(ui.agents.project_row, 0);
        press(&mut ui, Button::Left, 80);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (1, cohdl));
        // ENTER is what makes a channel: the highlighted agent's.
        press(&mut ui, Button::Right, 90);
        let out = press(&mut ui, Button::Enter, 100);
        assert_eq!((ui.agents.view, ui.global.active_channel), (AgentsView::Channel, 4));
        assert_eq!(events(&out), [Event::Channel { agent: 4, seq: 1 }]);
        // In the channel LEFT/RIGHT walk every page's agents in roster order.
        let out = press(&mut ui, Button::Left, 110);
        assert_eq!((ui.agents.agent, ui.agents.highlight, ui.agents.project.as_str()), (3, 3, cohdl));
        assert_eq!(events(&out), [Event::Channel { agent: 3, seq: 2 }]);
        press(&mut ui, Button::Cancel, 120);
        assert_eq!((ui.agents.view, ui.agents.highlight), (AgentsView::Roster, 3));
        // SHIFT chords never fall through.
        ui.apply(Control::Button(Button::Shift, Edge::Press), 160, &mut Update::new());
        let out = press(&mut ui, Button::Right, 170);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), out.deck_events().count()), (3, cohdl, 0));
        ui.apply(Control::Button(Button::Shift, Edge::Release), 180, &mut Update::new());
    }

    #[test]
    fn a_project_push_moves_an_agent_to_another_page_and_the_page_follows_the_highlight() {
        let mut ui = hosted();
        let [cohdl, openmicro] = crate::deck::demo::PROJECTS;
        wheel(&mut ui, Wheel::Ccw, 0);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (2, cohdl));
        // The highlighted agent moves house: the roster shows its new page
        // at once, re-sorted later.
        let out = host(&mut ui, &in_project(2, AgentState::Ready, openmicro), 100);
        assert!(out.dirty);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), ui.agents.project_row), (2, openmicro, 1));
        assert_eq!(ui.agents.highlight_row, 0, "the page is the roster order's: 2 before 4 and 5 until the re-sort");
        assert!(ui.deck.sort_pending());
        ui.tick(100 + RESORT_QUIET_MS, 0, true, &mut Update::new());
        assert_eq!(&ui.deck.order[..5], &[1, 3, 2, 5, 4]);
        assert_eq!((ui.agents.highlight, ui.agents.highlight_row), (2, 0));
        // A third project opens a page of its own, after the others by name.
        host(&mut ui, &in_project(6, AgentState::Working, "zeta"), 200);
        ui.tick(200 + RESORT_QUIET_MS, 0, true, &mut Update::new());
        press(&mut ui, Button::Right, 2000);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), ui.agents.project_row), (6, "zeta", 2));
        // An agent with no project sits on the last page, "Other".
        host(&mut ui, &agent(7, AgentState::Idle), 2100);
        ui.tick(2100 + RESORT_QUIET_MS, 0, true, &mut Update::new());
        press(&mut ui, Button::Right, 4000);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), ui.agents.project_row), (7, "", 3));
        assert_eq!(press(&mut ui, Button::Right, 4100).deck_events().count(), 0);
        assert_eq!(ui.agents.highlight, 7, "hard end");
    }

    #[test]
    fn enter_opens_the_channel_and_cancel_climbs_back() {
        let mut ui = hosted();
        wheel(&mut ui, Wheel::Ccw, 10);
        let out = press(&mut ui, Button::Enter, 20);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 2));
        assert_eq!(events(&out), [Event::Channel { agent: 2, seq: 1 }]);
        // Siblings from the channel, hard ends.
        let out = press(&mut ui, Button::Right, 30);
        assert_eq!((ui.agents.agent, ui.agents.highlight), (3, 3));
        assert_eq!(events(&out), [Event::Channel { agent: 3, seq: 2 }]);
        for at in [40, 50, 60] {
            press(&mut ui, Button::Right, at);
        }
        assert_eq!(ui.agents.agent, 5);
        press(&mut ui, Button::Cancel, 70);
        assert_eq!((ui.agents.view, ui.agents.highlight), (AgentsView::Roster, 5));
        assert_eq!(press(&mut ui, Button::Cancel, 80).deck_events().count(), 0);
        assert_eq!(ui.current_face().template, Template::Agents);
        // ENTER on a channel with a Question descends; without a card, nothing.
        press(&mut ui, Button::Enter, 90);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        press(&mut ui, Button::Enter, 100);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        for at in [110, 120, 130, 140] {
            press(&mut ui, Button::Left, at);
        }
        press(&mut ui, Button::Enter, 150);
        assert_eq!((ui.agents.view, ui.agents.agent, ui.agents.choice), (AgentsView::Card, 1, Some(0)));
        press(&mut ui, Button::Cancel, 700);
        assert_eq!(ui.agents.view, AgentsView::Channel);
    }

    #[test]
    fn the_reply_scrolls_exactly_as_far_as_its_wrapped_lines_overflow_the_lane() {
        let mut ui = hosted();
        press(&mut ui, Button::Enter, 10);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        // A one-line reply never scrolls.
        assert!(!wheel(&mut ui, Wheel::Ccw, 20).dirty);
        assert_eq!(ui.agents.reply_top, 0);
        // A long one (wide glyphs: 200 bytes of prose fit the lane) scrolls
        // until its last lines fill the lane, then stops.
        let mut reply = Text::<200>::new();
        for _ in 0..16 {
            reply.push_str("WWWWWWWWWWWW ");
        }
        ui.host(&HostMsg::Reply { agent: 1, text: reply, rev: 1 }, 30, &mut Update::new());
        let agent = ui.deck.agent(1).unwrap();
        let hidden = faces::agents::reply_lines(&ui, agent) - faces::agents::reply_visible(&ui, agent);
        assert!(hidden >= 1, "{}", faces::agents::reply_lines(&ui, agent));
        for step in 1..=hidden + 2 {
            let dirty = wheel(&mut ui, Wheel::Ccw, 40 + step as u32).dirty;
            assert_eq!(dirty, step <= hidden);
        }
        assert_eq!(ui.agents.reply_top as usize, hidden);
        wheel(&mut ui, Wheel::Cw, 100);
        assert_eq!(ui.agents.reply_top as usize, hidden - 1);
    }

    #[test]
    fn scrolling_after_a_card_clears_starts_from_the_visible_reply_offset() {
        let mut ui = hosted();
        host(&mut ui, &long_reply(1, 1), 1000);
        host(&mut ui, &reply_window(1, 1, 0, 40), 1010);
        show_channel(&mut ui, 1, 1100);
        ui.agents.reply_top = faces::agents::reply_clamp(&ui, ui.deck.agent(1).unwrap(), u16::MAX);
        let old_top = ui.agents.reply_top;
        ui.deck.agents[0].has_card = false;
        let expanded_top = faces::agents::reply_clamp(&ui, ui.deck.agent(1).unwrap(), old_top);
        assert!(expanded_top < old_top);
        wheel(&mut ui, Wheel::Cw, 1200);
        assert_eq!(ui.agents.reply_top, expanded_top - 1);
    }

    #[test]
    fn a_question_body_that_fits_leaves_the_whole_wheel_to_the_choices() {
        let mut ui = hosted();
        // The demo's card carries no window at all: it draws and behaves as
        // it always has.
        show_card(&mut ui, 1, 1000);
        assert_eq!((ui.agents.choice, ui.agents.question_top), (Some(0), 0));
        let out = wheel(&mut ui, Wheel::Ccw, 1100);
        assert_eq!((ui.agents.choice, ui.agents.question_top), (Some(1), 0));
        assert!(out.dirty && events(&out).is_empty());
        wheel(&mut ui, Wheel::Cw, 1200);
        assert_eq!(ui.agents.choice, Some(0));
        assert!(!wheel(&mut ui, Wheel::Cw, 1300).dirty, "nothing above the first choice");
        // A window whose whole body is on the screen is the same: three Name
        // lines fit, so the wheel never stops on the body.
        host(&mut ui, &window(1, 1, 0, 3, &["one", "two", "three"]), 1400);
        assert_eq!(ui.agents.question_top, 0);
        wheel(&mut ui, Wheel::Ccw, 1500);
        assert_eq!((ui.agents.choice, ui.agents.question_top), (Some(1), 0));
        assert!(!wheel(&mut ui, Wheel::Cw, 1600).dirty || ui.agents.choice == Some(0));
        assert!(!wheel(&mut ui, Wheel::Cw, 1700).dirty, "still nothing above the first choice");
    }

    #[test]
    fn the_wheel_walks_a_long_question_body_and_then_its_choices() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        // Nine lines held whole, three visible: six are hidden.
        let lines = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
        host(&mut ui, &window(1, 1, 0, 9, &lines), 1100);
        let card = ui.deck.card(1).unwrap();
        assert_eq!(faces::agents::question_visible(card), 3);
        assert_eq!(faces::agents::question_lines(card) - faces::agents::question_visible(card), 6);
        for step in 1..=6u16 {
            let out = wheel(&mut ui, Wheel::Ccw, 1100 + u32::from(step));
            assert!(out.dirty && events(&out).is_empty(), "line {step}");
            assert_eq!((ui.agents.question_top, ui.agents.choice), (step, Some(0)));
        }
        // The last line is on the screen: the wheel goes on into the choices.
        wheel(&mut ui, Wheel::Ccw, 1200);
        assert_eq!((ui.agents.question_top, ui.agents.choice), (6, Some(1)));
        wheel(&mut ui, Wheel::Ccw, 1300);
        assert_eq!((ui.agents.question_top, ui.agents.choice), (6, Some(2)));
        assert!(!wheel(&mut ui, Wheel::Ccw, 1400).dirty, "three choices, hard end");
        // Backwards: the choices first, because the body is above them.
        for choice in [1u8, 0] {
            wheel(&mut ui, Wheel::Cw, 1500 + u32::from(choice));
            assert_eq!((ui.agents.question_top, ui.agents.choice), (6, Some(choice)));
        }
        wheel(&mut ui, Wheel::Cw, 1600);
        assert_eq!((ui.agents.question_top, ui.agents.choice), (5, Some(0)));
        // The rest of the grammar is untouched: ENTER answers the choice the
        // body was scrolled past, CANCEL snoozes, LEFT/RIGHT walk the cards.
        let out = press(&mut ui, Button::Enter, 2000);
        assert_eq!(events(&out), [Event::Answer { agent: 1, card: 1, choice: 0 }]);
    }

    #[test]
    fn the_body_offset_resets_on_a_new_card_and_never_leaves_the_window_held() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        host(&mut ui, &window(1, 1, 0, 9, &["1", "2", "3", "4", "5", "6", "7", "8", "9"]), 1100);
        for at in 1200..1205 {
            wheel(&mut ui, Wheel::Ccw, at);
        }
        assert_eq!(ui.agents.question_top, 5);
        // Another card, and back: the body opens at the top of what is held.
        show_card(&mut ui, 2, 1300);
        show_card(&mut ui, 1, 1400);
        assert_eq!(ui.agents.question_top, 0);
        // A chunk that carries the body on joins what is held rather than
        // replacing it, so the offset stays where the reader left it: the
        // line it is on is still in the buffer.
        for at in 1500..1505 {
            wheel(&mut ui, Wheel::Ccw, at);
        }
        assert_eq!(ui.agents.question_top, 5);
        host(&mut ui, &window(1, 1, 6, 9, &["7", "8", "9"]), 1600);
        assert_eq!(ui.agents.question_top, 5, "the buffer grew under the offset");
        assert_eq!(ui.deck.card(1).unwrap().body.end(), 9, "and now reaches further");
        // A card the host replaces starts over with no window at all.
        host(&mut ui, &card(1, 7, CardKind::Question, 3), 1700);
        show_card(&mut ui, 1, 1800);
        assert_eq!(ui.agents.question_top, 0);
        assert!(!ui.deck.card(1).unwrap().body.windowed());
    }

    #[test]
    fn the_body_asks_for_the_next_window_and_waits_for_it_instead_of_falling_into_the_choices() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        // Forty lines, the first `DOC_LINES` of them held.
        let held = crate::deck::msg::DOC_LINES;
        host(&mut ui, &long_window(1, 1, 0, 40), 1100);
        let visible = faces::agents::question_visible(ui.deck.card(1).unwrap());
        // The next window is asked for `WINDOW_MARGIN` lines before the offset
        // would run out of held text, so the wheel is not left waiting on the
        // link. Derived, not written down: the geometry is the constants'.
        let first_ask = held.saturating_sub(visible + WINDOW_MARGIN - 1);

        let mut at = 1200;
        for top in 1..first_ask as u16 {
            let out = wheel(&mut ui, Wheel::Ccw, at);
            assert_eq!(
                (ui.agents.question_top, events(&out).len()),
                (top, 0),
                "nothing is asked for while the scroll is clear of the edge (top {top})"
            );
            at += 100;
        }
        let out = wheel(&mut ui, Wheel::Ccw, at);
        assert_eq!(ui.agents.question_top, first_ask as u16);
        // It asks for the lines it is **missing** — the chunk starting where
        // the buffer ends — not for the line it is looking at. A chunk centred
        // on the reader would be half lines already held.
        assert_eq!(
            events(&out),
            [Event::DocLines { agent: 1, doc: DocKind::Card, id: 1, first: held as u16 }]
        );

        // At the edge the wheel holds the body: it must not jump into the
        // choices while there is text the deck has not been sent.
        let edge = (held - visible) as u16;
        let mut guard = 0;
        while ui.agents.question_top < edge && guard < 40 {
            at += 100;
            wheel(&mut ui, Wheel::Ccw, at);
            guard += 1;
        }
        assert_eq!(ui.agents.question_top, edge, "pinned at the end of what is held");
        at += 100;
        let out = wheel(&mut ui, Wheel::Ccw, at);
        assert_eq!(
            (ui.agents.question_top, ui.agents.choice),
            (edge, Some(0)),
            "waiting on the host, not falling into the choices"
        );
        assert!(
            events(&out).is_empty() || matches!(events(&out)[0], Event::DocLines { .. }),
            "only ever asks for lines while it waits"
        );
        at += 100;
        assert!(
            events(&wheel(&mut ui, Wheel::Ccw, at)).is_empty(),
            "asked once, not once a detent"
        );

        // The window lands: the scroll goes on from where it stopped.
        host(&mut ui, &long_window(1, 1, 6, 40), at + 100);
        assert_eq!(ui.agents.question_top, edge);
        at += 200;
        wheel(&mut ui, Wheel::Ccw, at);
        assert_eq!(ui.agents.question_top, edge + 1);

        // Without a host nothing is asked for, and what is held still scrolls.
        ui.deck.link_lost();
        at += 100;
        let out = wheel(&mut ui, Wheel::Ccw, at);
        assert_eq!((ui.agents.question_top, events(&out).len()), (edge + 2, 0));
        assert!(out.dirty);
    }

    #[test]
    fn focus_during_scroll_moves_marker_not_highlight() {
        let mut ui = hosted();
        wheel(&mut ui, Wheel::Ccw, 1000);
        host(&mut ui, &HostMsg::Focus { agent: 4, ack: None }, 1500);
        assert_eq!((ui.global.active_channel, ui.agents.highlight), (4, 2));
        // Quiet for 2 s: the highlight follows, and the page turns with it.
        host(&mut ui, &HostMsg::Focus { agent: 5, ack: None }, 3000);
        assert_eq!((ui.global.active_channel, ui.agents.highlight), (5, 5));
        assert_eq!((ui.agents.project.as_str(), ui.agents.top_row), (crate::deck::demo::PROJECTS[1], 0));
        // Held: not even when quiet.
        ui.apply(Control::Button(Button::Enter, Edge::Press), 3100, &mut Update::new());
        ui.on_face(Template::Usage);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 9000);
        assert_eq!((ui.global.active_channel, ui.agents.highlight), (3, 5));
        ui.apply(Control::Button(Button::Enter, Edge::Release), 9100, &mut Update::new());
        // An idle channel follows; a card never does.
        let mut ui = hosted();
        press(&mut ui, Button::Enter, 0);
        assert_eq!(ui.agents.agent, 1);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1000);
        assert_eq!(ui.agents.agent, 1);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 2001);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 3));
        assert!(!global::guard_passed(&ui, 2100, 2200), "a view change the user did not make");
        show_card(&mut ui, 1, 3000);
        host(&mut ui, &HostMsg::Focus { agent: 2, ack: None }, 9000);
        assert_eq!((ui.agents.view, ui.agents.agent, ui.global.active_channel), (AgentsView::Card, 1, 2));
    }

    #[test]
    fn conol_creation_focus_immediately_follows_ptt_release() {
        let mut ui = hosted();
        ui.deck.agent_mut(3).unwrap().kind = AgentKind::Conol;
        show_channel(&mut ui, 2, 0);
        ui.apply(Control::Button(Button::PushToTalk, Edge::Press), 1000, &mut Update::new());
        ui.apply(Control::Button(Button::PushToTalk, Edge::Release), 1100, &mut Update::new());
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1345);
        assert_eq!((ui.agents.view, ui.agents.agent, ui.global.active_channel), (AgentsView::Channel, 3, 3));
        assert_eq!(ptt_target(&ui).0, 3, "the next dictation follows the newly created chat");
        // Even an explicit Conol focus cannot dismiss an unfinished form.
        show_card(&mut ui, 1, 1400);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1500);
        ui.tick(4000, 0, true, &mut Update::new());
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 1));
    }

    #[test]
    fn generic_companion_focus_opens_the_session_after_recent_input() {
        let mut ui = hosted();
        ui.deck.agent_mut(3).unwrap().kind = AgentKind::Other;
        wheel(&mut ui, Wheel::Ccw, 1000);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1100);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 3));
        assert_eq!(ptt_target(&ui).0, 3);
    }

    #[test]
    fn conol_creation_from_roster_opens_the_new_channel() {
        let mut ui = hosted();
        ui.deck.agent_mut(3).unwrap().kind = AgentKind::Conol;
        wheel(&mut ui, Wheel::Ccw, 1000);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1100);
        assert_eq!((ui.agents.view, ui.agents.agent, ui.agents.highlight), (AgentsView::Channel, 3, 3));
    }

    #[test]
    fn conol_focus_waits_for_held_input_but_not_another_host_event() {
        let mut ui = hosted();
        ui.deck.agent_mut(3).unwrap().kind = AgentKind::Conol;
        show_channel(&mut ui, 2, 0);
        ui.apply(Control::Button(Button::Shift, Edge::Press), 1000, &mut Update::new());
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1100);
        ui.tick(4000, 0, true, &mut Update::new());
        assert_eq!((ui.agents.agent, ui.agents.deferred_focus), (2, 3));
        ui.apply(Control::Button(Button::Shift, Edge::Release), 4100, &mut Update::new());
        ui.tick(4101, 0, true, &mut Update::new());
        assert_eq!((ui.agents.agent, ui.agents.deferred_focus), (3, 0));
    }

    #[test]
    fn conol_deferred_focus_yields_to_new_navigation_and_newer_focus() {
        let mut ui = hosted();
        ui.deck.agent_mut(3).unwrap().kind = AgentKind::Conol;
        show_channel(&mut ui, 2, 0);
        ui.apply(Control::Button(Button::Shift, Edge::Press), 1000, &mut Update::new());
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1100);
        wheel(&mut ui, Wheel::Ccw, 1200);
        assert_eq!(ui.agents.deferred_focus, 0);
        host(&mut ui, &HostMsg::Focus { agent: 3, ack: None }, 1300);
        host(&mut ui, &HostMsg::Focus { agent: 4, ack: None }, 1400);
        assert_eq!(ui.agents.deferred_focus, 0);
        ui.apply(Control::Button(Button::Shift, Edge::Release), 4100, &mut Update::new());
        ui.tick(4101, 0, true, &mut Update::new());
        assert_ne!(ui.agents.agent, 3);
    }

    #[test]
    fn resort_deferred_while_wheel_repeating() {
        let mut ui = hosted();
        assert_eq!(&ui.deck.order[..5], &[1, 2, 3, 4, 5]);
        wheel(&mut ui, Wheel::Ccw, 1000);
        wheel(&mut ui, Wheel::Ccw, 1090);
        assert_eq!(ui.agents.highlight, 3);
        host(&mut ui, &agent(3, AgentState::Question), 1100);
        assert_eq!(&ui.deck.order[..5], &[1, 2, 3, 4, 5], "in place while the wheel turns");
        assert!(ui.deck.sort_pending());
        ui.tick(2000, 0, true, &mut Update::new());
        wheel(&mut ui, Wheel::Cw, 2100);
        ui.tick(2100 + RESORT_QUIET_MS - 1, 0, true, &mut Update::new());
        assert_eq!(&ui.deck.order[..5], &[1, 2, 3, 4, 5], "1.5 s after the last detent");
        let mut out = Update::new();
        ui.tick(2100 + RESORT_QUIET_MS, 0, true, &mut out);
        // Each page sorted on its own: Questions first on the first, the
        // Error before the Idle on the second.
        assert_eq!(&ui.deck.order[..5], &[1, 3, 2, 5, 4]);
        assert!(out.dirty && !ui.deck.sort_pending());
        assert_eq!((ui.agents.highlight, ui.agents.top_row), (2, 0), "the highlight kept its agent");
        // Held: still deferred.
        host(&mut ui, &agent(5, AgentState::Idle), 5000);
        ui.apply(Control::Button(Button::Enter, Edge::Press), 5000, &mut Update::new());
        ui.tick(9000, 0, true, &mut Update::new());
        assert!(ui.deck.sort_pending());
        ui.apply(Control::Button(Button::Enter, Edge::Release), 9000, &mut Update::new());
        ui.tick(9000 + RESORT_QUIET_MS, 0, true, &mut Update::new());
        assert!(!ui.deck.sort_pending());
        assert_eq!(&ui.deck.order[..5], &[1, 3, 2, 4, 5]);
    }

    #[test]
    fn a_resort_keeps_the_highlighted_agent_in_view() {
        let mut ui = hosted();
        // Two more Questions on the first page: once sorted, the highlighted
        // Working agent is its fifth row, below the fold.
        let cohdl = crate::deck::demo::PROJECTS[0];
        host(&mut ui, &in_project(6, AgentState::Question, cohdl), 0);
        host(&mut ui, &in_project(7, AgentState::Question, cohdl), 0);
        for at in [0, 100] {
            wheel(&mut ui, Wheel::Ccw, at);
        }
        assert_eq!((ui.agents.highlight, ui.agents.top_row), (3, 0));
        let mut out = Update::new();
        ui.tick(100 + RESORT_QUIET_MS, 0, true, &mut out);
        assert_eq!(&ui.deck.order[..7], &[1, 6, 7, 2, 3, 5, 4]);
        assert!(out.dirty && !ui.deck.sort_pending());
        assert_eq!((ui.agents.highlight, ui.agents.highlight_row, ui.agents.top_row), (3, 4, 1), "the highlight kept its agent");
    }

    #[test]
    fn tap_cancelled_when_row_identity_changes_between_down_and_up() {
        let mut ui = hosted();
        let (x, y) = layout::roster_row(1).center();
        let touch = |ui: &mut UiState, phase, at| {
            ui.apply(Control::Touch(Touch { phase, x: x as u16, y: y as u16 }), at, &mut Update::new());
        };
        touch(&mut ui, TouchPhase::Down, 0);
        assert_eq!(ui.agents.pressed, Some(2));
        host(&mut ui, &gone(2), 10);
        touch(&mut ui, TouchPhase::Up, 20);
        assert_eq!((ui.agents.highlight, ui.agents.view), (1, AgentsView::Roster));
        // Tap = highlight, tap the highlighted row = open.
        touch(&mut ui, TouchPhase::Down, 100);
        touch(&mut ui, TouchPhase::Up, 110);
        assert_eq!((ui.agents.highlight, ui.agents.view), (3, AgentsView::Roster));
        touch(&mut ui, TouchPhase::Down, 200);
        let mut out = Update::new();
        ui.apply(Control::Touch(Touch { phase: TouchPhase::Up, x: x as u16, y: y as u16 }), 210, &mut out);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 3));
        assert_eq!(events(&out), [Event::Channel { agent: 3, seq: 1 }]);
    }

    #[test]
    fn gone_of_open_channel_falls_back_to_roster_and_a_gone_highlight_keeps_its_row() {
        let mut ui = hosted();
        let [cohdl, openmicro] = crate::deck::demo::PROJECTS;
        wheel(&mut ui, Wheel::Ccw, 0);
        press(&mut ui, Button::Enter, 10);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 2));
        host(&mut ui, &gone(2), 100);
        assert_eq!((ui.agents.view, ui.agents.highlight), (AgentsView::Roster, 3), "row 1 again");
        assert!(!global::guard_passed(&ui, 150, 400));
        host(&mut ui, &gone(3), 200);
        assert_eq!((ui.agents.highlight, ui.agents.highlight_row), (1, 0), "clamped to the last row of its page");
        assert_eq!(ui.agents.project.as_str(), cohdl);
        // The page emptied: the roster turns to the neighbouring page.
        host(&mut ui, &gone(1), 300);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), ui.agents.project_row), (4, openmicro, 0));
        host(&mut ui, &gone(4), 400);
        assert_eq!(ui.agents.highlight, 5);
        host(&mut ui, &gone(5), 500);
        assert_eq!(ui.agents.highlight, 0);
        host(&mut ui, &agent(6, AgentState::Working), 600);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str()), (6, ""), "the first agent of an empty roster is highlighted");
        // The last page emptying falls back to the one before it.
        let mut ui = hosted();
        press(&mut ui, Button::Right, 0);
        assert_eq!((ui.agents.highlight, ui.agents.project_row), (4, 1));
        host(&mut ui, &gone(4), 100);
        host(&mut ui, &gone(5), 200);
        assert_eq!((ui.agents.highlight, ui.agents.project.as_str(), ui.agents.project_row), (1, cohdl, 0));
        // A card whose agent leaves goes to the roster too.
        let mut ui = hosted();
        show_card(&mut ui, 1, 0);
        host(&mut ui, &gone(1), 100);
        assert_eq!(ui.agents.view, AgentsView::Roster);
    }

    #[test]
    fn a_sync_that_times_out_in_tick_purges_the_viewed_agent_too() {
        let mut ui = hosted();
        press(&mut ui, Button::Enter, 10);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        host(&mut ui, &HostMsg::Hello { v: 1, sid: 2, minute_of_day: None }, 1000);
        host(&mut ui, &agent(3, AgentState::Working), 1100);
        assert!(ui.deck.syncing);
        let mut out = Update::new();
        ui.tick(1000 + crate::deck::model::SYNC_TIMEOUT_MS, 0, true, &mut out);
        assert!(ui.deck.agent(1).is_none() && out.dirty);
        assert_eq!((ui.agents.view, ui.agents.highlight), (AgentsView::Roster, 3));
    }

    #[test]
    fn enter_50ms_after_a_jump_does_not_answer_but_500ms_after_does() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        let out = press(&mut ui, Button::Enter, 1050);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!(ui.agents.view, AgentsView::Card);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, None);
        let out = press(&mut ui, Button::Enter, 1500);
        assert_eq!(events(&out), [Event::Answer { agent: 1, card: 1, choice: 0 }]);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, Some(0));
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1), "no auto-advance");
    }

    #[test]
    fn tap_within_guard_after_toast_jump_is_ignored() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        let (x, y) = layout::card_choice(1).center();
        let out = tap(&mut ui, x, y, 1100);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!((ui.agents.view, ui.agents.choice), (AgentsView::Card, Some(1)), "the tap still highlights");
        // A finger that came down before the change never confirms either.
        let touch = |ui: &mut UiState, phase, at| {
            ui.apply(Control::Touch(Touch { phase, x: x as u16, y: y as u16 }), at, &mut Update::new());
        };
        show_card(&mut ui, 1, 2000);
        touch(&mut ui, TouchPhase::Down, 1900);
        let mut out = Update::new();
        ui.apply(Control::Touch(Touch { phase: TouchPhase::Up, x: x as u16, y: y as u16 }), 2600, &mut out);
        assert_eq!(out.deck_events().count(), 0);
        let out = tap(&mut ui, x, y, 2700);
        assert_eq!(events(&out), [Event::Answer { agent: 1, card: 1, choice: 1 }]);
    }

    #[test]
    fn card_replaced_under_cursor_falls_back_to_the_channel_under_guard() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        wheel(&mut ui, Wheel::Ccw, 1100);
        assert_eq!(ui.agents.choice, Some(1));
        host(&mut ui, &card(1, 9, CardKind::Question, 3), 1200);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        assert!(!global::guard_passed(&ui, 1250, 1400));
        press(&mut ui, Button::Enter, 1300);
        assert_eq!(ui.agents.view, AgentsView::Card, "ENTER in the channel is not guarded");
        assert_eq!(ui.agents.choice, Some(0), "the new card starts at its recommendation");
        // A re-push with fewer choices keeps the choice in range.
        wheel(&mut ui, Wheel::Ccw, 1400);
        wheel(&mut ui, Wheel::Ccw, 1500);
        assert_eq!(ui.agents.choice, Some(2));
        host(&mut ui, &card(1, 9, CardKind::Question, 2), 1600);
        assert_eq!((ui.agents.view, ui.agents.choice), (AgentsView::Card, Some(0)));
    }

    #[test]
    fn autojump_onto_a_replacing_card_lands_on_the_card() {
        let mut ui = hosted();
        ui.deck.settings.auto_jump = true;
        ui.on_face(Template::Omarchy);
        ui.tick(5000, 5000, true, &mut Update::new());
        host(&mut ui, &card(3, 7, CardKind::Question, 3), 5000);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        press(&mut ui, Button::Cancel, 6000);
        press(&mut ui, Button::Cancel, 7000);
        ui.on_face(Template::Omarchy);
        ui.tick(12000, 5000, true, &mut Update::new());
        // The new card clears the one it replaces in the same message: the
        // jump it made stays on the card, not the channel.
        host(&mut ui, &card(3, 8, CardKind::Question, 3), 12000);
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 3));
        assert!(!global::guard_passed(&ui, 12050, 12200));
    }

    #[test]
    fn host_clear_of_visible_card_goes_to_channel_not_next_card() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        host(&mut ui, &clear(1, 1), 1100);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        assert!(ui.deck.card(2).is_some(), "another card is pending, and not shown");
        assert!(!global::guard_passed(&ui, 1150, 1300));
    }

    #[test]
    fn double_enter_answers_only_one_card() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        let first = press(&mut ui, Button::Enter, 2000);
        assert_eq!(events(&first), [Event::Answer { agent: 1, card: 1, choice: 0 }]);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        // The bounce reopens the sent card, which is locked: nothing answers
        // until the host clears it.
        let second = press(&mut ui, Button::Enter, 2030);
        assert_eq!(second.deck_events().count(), 0);
        assert_eq!(ui.agents.view, AgentsView::Card);
        assert_eq!(press(&mut ui, Button::Enter, 4000).deck_events().count(), 0);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, Some(0));
        press(&mut ui, Button::Cancel, 4100);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        assert!(!ui.deck.card(1).unwrap().snoozed, "no snooze of a sent card");
    }

    #[test]
    fn answer_without_deck_host_keeps_card_and_emits_nothing() {
        let mut ui = hosted();
        ui.set_connected(false, &mut Update::new());
        assert!(!ui.deck.host_present && !ui.deck.demo_active());
        assert!(ui.deck.agent(1).unwrap().stale);
        show_card(&mut ui, 1, 1000);
        let out = press(&mut ui, Button::Enter, 2000);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, None);
        assert_eq!(ui.agents.view, AgentsView::Card);
        let out = press(&mut ui, Button::Cancel, 2100);
        assert_eq!(out.deck_events().count(), 0);
        assert!(!ui.deck.card(1).unwrap().snoozed);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        // The demo answers for a host.
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        assert!(ui.deck.demo_active());
        show_card(&mut ui, 1, 1000);
        let out = press(&mut ui, Button::Enter, 2000);
        assert_eq!(events(&out), [Event::Answer { agent: 1, card: 1, choice: 0 }]);
    }

    #[test]
    fn unacked_answer_reverts_after_timeout_with_a_not_delivered_toast() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        press(&mut ui, Button::Enter, 2000);
        ui.tick(2000 + ANSWER_ACK_MS - 1, 0, true, &mut Update::new());
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, Some(0));
        let mut out = Update::new();
        ui.tick(2000 + ANSWER_ACK_MS, 0, true, &mut out);
        assert!(out.dirty);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, None);
        let toast = ui.notify.toast.unwrap();
        assert_eq!((toast.kind, toast.agent, toast.card), (ToastKind::NotDelivered, 1, 1));
        // Acknowledged in time: the card is gone and nothing complains.
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        press(&mut ui, Button::Enter, 2000);
        host(&mut ui, &clear(1, 1), 3000);
        ui.tick(8000, 0, true, &mut Update::new());
        assert!(ui.notify.toast.is_none() && ui.deck.card(1).is_none());
    }

    #[test]
    fn cancel_snoozes_a_question_into_the_drawer_and_returns_to_the_channel() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 1000);
        let out = press(&mut ui, Button::Cancel, 1100);
        assert_eq!(events(&out), [Event::Snooze { agent: 1, card: 1 }]);
        assert!(ui.deck.card(1).unwrap().snoozed);
        assert_eq!((ui.deck.notice_count(), ui.deck.drawer_has_question()), (1, true));
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        // A Ready card: CANCEL just climbs.
        show_card(&mut ui, 2, 2000);
        let out = press(&mut ui, Button::Cancel, 2100);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!((ui.agents.view, ui.deck.notice_count()), (AgentsView::Channel, 1));
    }

    #[test]
    fn left_right_walks_pending_cards_questions_first_with_hard_ends() {
        let mut ui = hosted();
        host(&mut ui, &agent(4, AgentState::Question), 0);
        host(&mut ui, &card(4, 44, CardKind::Question, 2), 1);
        // Pending: Questions 1, 4 (roster order), then Ready 2.
        show_card(&mut ui, 2, 1000);
        assert_eq!(press(&mut ui, Button::Right, 1100).deck_events().count(), 0);
        assert_eq!(ui.agents.agent, 2);
        let out = press(&mut ui, Button::Left, 1200);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 4));
        assert_eq!(events(&out), [Event::Channel { agent: 4, seq: 1 }]);
        assert!(!global::guard_passed(&ui, 1250, 1400));
        press(&mut ui, Button::Left, 1300);
        assert_eq!((ui.agents.agent, ui.agents.highlight), (1, 1));
        assert_eq!(press(&mut ui, Button::Left, 1400).deck_events().count(), 0);
        assert_eq!(ui.agents.agent, 1);
    }

    #[test]
    fn an_empty_roster_settles_instead_of_repainting_every_tick() {
        for hello in [false, true] {
            let mut ui = std::boxed::Box::new(UiState::new());
            ui.reset();
            ui.load_demo();
            ui.deck.settings.demo = false;
            ui.deck.clear_host_data();
            if hello {
                ui.host(&HostMsg::Hello { v: 1, sid: 1, minute_of_day: None }, 0, &mut Update::new());
            }
            ui.on_face(Template::Usage);
            for at in [100, 150] {
                ui.tick(at, at, true, &mut Update::new());
            }
            let mut out = Update::new();
            ui.tick(200, 200, true, &mut out);
            assert!(!out.dirty, "hello: {hello}");
            assert_eq!(ui.agents.highlight, 0);
        }
    }

    #[test]
    fn workflow_switch_is_requested_and_ignores_in_flight_chat_windows() {
        let mut ui = hosted();
        show_channel(&mut ui, 3, 100);
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 1 }, 110);
        let out = tap(&mut ui, 380, 82, 200);
        assert!(matches!(events(&out).as_slice(), [Event::DocLines { doc: DocKind::Workflow, id: 1, first: 0, .. }]));
        let mut window = reply_window(3, 1, 0, 10);
        host(&mut ui, &window, 250);
        assert!(!ui.deck.agent(3).unwrap().reply.windowed(), "late chat must not paint the workflow");
        if let HostMsg::DocText { doc, .. } = &mut window { *doc = DocKind::Workflow; }
        host(&mut ui, &window, 260);
        assert!(ui.deck.agent(3).unwrap().reply.windowed());
        assert_eq!(ui.agents.reply_top, 0, "workflow stays at current steps, not the chat tail");
        assert!(!ui.agents.reply_follow);
        let out = press(&mut ui, Button::Cancel, 300);
        assert!(matches!(events(&out).as_slice(), [Event::DocLines { doc: DocKind::Reply, first: u16::MAX, .. }]));
        assert_eq!(ui.agents.view, AgentsView::Channel);
        host(&mut ui, &window, 350);
        assert!(!ui.deck.agent(3).unwrap().reply.windowed(), "late workflow must not paint chat");
    }

    #[test]
    fn workflow_refresh_keeps_the_page_until_its_replacement_arrives() {
        let mut ui = hosted();
        show_channel(&mut ui, 3, 100);
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Workflow"), rev: 1 }, 110);
        toggle_workflow(&mut ui, 200, &mut Update::new());
        let window = |rev| {
            let mut msg = reply_window(3, rev, 0, 20);
            if let HostMsg::DocText { doc, font, lines, .. } = &mut msg {
                *doc = DocKind::Workflow;
                *font = crate::deck::msg::DocFont::RichRow;
                lines[2].set("t040Full plan");
            }
            msg
        };
        host(&mut ui, &window(1), 250);
        ui.agents.reply_top = 2;
        for rev in 2..=4 {
            let at = u32::from(rev) * 3000;
            let old = ui.deck.agent(3).unwrap().reply;
            host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Workflow"), rev }, at);
            assert_eq!(ui.deck.agent(3).unwrap().reply, old, "an envelope must not blank the page");
            assert_eq!(ui.agents.reply_top, 2);
            let lane = faces::agents::reply_rect(&ui, ui.deck.agent(3).unwrap());
            assert!(hit(&ui, 0, lane.left() + 30, lane.top() + 10).is_none(), "old disclosure indices are disabled");
            host(&mut ui, &window(rev - 1), at + 50);
            assert_eq!(ui.deck.agent(3).unwrap().reply, old, "late old windows stay rejected");
            let mut out = Update::new();
            on_tick(&mut ui, at + 100, &mut out);
            assert!(events(&out).iter().any(|e| matches!(e, Event::DocLines { doc: DocKind::Workflow, id, .. } if *id == rev)), "retry uses the announced revision");
            assert_eq!(ui.deck.agent(3).unwrap().reply, old);
            host(&mut ui, &window(rev), at + 200);
            assert_eq!(ui.deck.agent(3).unwrap().reply.id, rev);
            assert!(ui.deck.agent(3).unwrap().reply.windowed());
            assert_eq!(ui.agents.reply_top, 2);
            assert!(hit(&ui, 0, lane.left() + 30, lane.top() + 10).is_some(), "current disclosures work again");
        }
    }

    #[test]
    fn workflow_keeps_permission_enter_and_retries_missing_windows() {
        let mut ui = hosted();
        show_channel(&mut ui, 1, 100);
        toggle_workflow(&mut ui, 200, &mut Update::new());
        let out = press(&mut ui, Button::Enter, 300);
        assert_eq!(ui.agents.view, AgentsView::Card);
        assert!(events(&out).is_empty(), "opening the card does not answer it");
        show_channel(&mut ui, 1, 400);
        let mut out = Update::new();
        on_tick(&mut ui, 6000, &mut out);
        assert!(events(&out).iter().any(|e| matches!(e, Event::DocLines { doc: DocKind::Workflow, .. })));
        let mut out = Update::new();
        host(&mut ui, &HostMsg::Synced, 6100);
        on_host(&mut ui, &HostMsg::Synced, &Effects::new(), 6101, &mut out);
        assert!(events(&out).iter().any(|e| matches!(e, Event::DocLines { doc: DocKind::Workflow, .. })));
    }

    #[test]
    fn ready_card_steps_answer_and_enter_with_none_acknowledges() {
        let mut ui = hosted();
        ui.deck.notices[0] = crate::deck::Notice::new(2, 2, CardKind::Ready, 0).filed();
        show_card(&mut ui, 2, 1000);
        assert_eq!(ui.agents.choice, None);
        wheel(&mut ui, Wheel::Ccw, 1100);
        wheel(&mut ui, Wheel::Ccw, 1200);
        wheel(&mut ui, Wheel::Ccw, 1300);
        assert_eq!(ui.agents.choice, Some(1), "two steps, hard end");
        wheel(&mut ui, Wheel::Cw, 1400);
        wheel(&mut ui, Wheel::Cw, 1500);
        assert_eq!(ui.agents.choice, None);
        wheel(&mut ui, Wheel::Cw, 1600);
        assert_eq!(ui.agents.choice, None);
        let out = press(&mut ui, Button::Enter, 1700);
        assert_eq!(out.deck_events().count(), 0);
        assert!(ui.deck.card(2).unwrap().acknowledged);
        assert_eq!((ui.agents.view, ui.deck.notice_count()), (AgentsView::Channel, 0));
        assert!(matches!(events(&press(&mut ui, Button::Enter, 1800)).as_slice(),
            [Event::DocLines { doc: DocKind::Workflow, .. }]));
        assert_eq!(ui.agents.view, AgentsView::Channel, "an acknowledged Ready card does not reopen");
        // Nor does a tap where its button was, or LEFT/RIGHT from another card.
        let (x, y) = layout::CHANNEL_BUTTON.center();
        assert!(hit(&ui, 0, x, y).is_none());
        assert!(tap(&mut ui, x, y, 1810).deck_events().count() == 0 && ui.agents.pressed.is_none());
        assert_eq!(ui.agents.view, AgentsView::Channel);
        show_card(&mut ui, 1, 1840);
        for (button, at) in [(Button::Right, 1850), (Button::Left, 1860)] {
            assert_eq!(press(&mut ui, button, at).deck_events().count(), 0);
            assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 1));
        }
        show_channel(&mut ui, 2, 1870);
        // A step is an answer.
        ui.deck.agent_mut(2).unwrap().card.acknowledged = false;
        press(&mut ui, Button::Enter, 1900);
        wheel(&mut ui, Wheel::Ccw, 2000);
        let out = press(&mut ui, Button::Enter, 2500);
        assert_eq!(events(&out), [Event::Answer { agent: 2, card: 2, choice: 0 }]);
    }

    #[test]
    fn channel_buttons_follow_the_choice_count() {
        let mut ui = hosted();
        press(&mut ui, Button::Enter, 0);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Channel, 1));
        // Three choices: one wide button that descends.
        let (x, y) = layout::CHANNEL_BUTTON.center();
        assert_eq!(hit(&ui, 0, x, y).map(|t| t.id), Some(0));
        tap(&mut ui, x, y, 1000);
        assert_eq!(ui.agents.view, AgentsView::Card);
        press(&mut ui, Button::Cancel, 2000);
        // Two choices: two buttons labelled with them, answers under the guard.
        host(&mut ui, &card(1, 5, CardKind::Question, 2), 3000);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        let (x, y) = layout::channel_choice_button(1).center();
        assert_eq!(hit(&ui, 0, x, y).map(|t| t.id), Some(1));
        assert_eq!(tap(&mut ui, x, y, 3100).deck_events().count(), 0, "guarded after the swap");
        let out = tap(&mut ui, x, y, 4000);
        assert_eq!(events(&out), [Event::Answer { agent: 1, card: 5, choice: 1 }]);
        assert_eq!(ui.agents.view, AgentsView::Channel);
        // Ready: one "summary" button that opens the card.
        show_channel(&mut ui, 2, 5000);
        let (x, y) = layout::CHANNEL_BUTTON.center();
        tap(&mut ui, x, y, 5100);
        assert_eq!((ui.agents.view, ui.agents.agent), (AgentsView::Card, 2));
        let (x, y) = layout::ready_row(1).center();
        let out = tap(&mut ui, x, y, 6000);
        assert_eq!(events(&out), [Event::Answer { agent: 2, card: 2, choice: 1 }]);
        // No card: no button.
        show_channel(&mut ui, 3, 7000);
        assert_eq!(hit(&ui, 0, x, y), None);
    }

    #[test]
    fn shift_enter_on_card_does_not_answer() {
        let mut ui = hosted();
        show_card(&mut ui, 1, 0);
        ui.apply(Control::Button(Button::Shift, Edge::Press), 1000, &mut Update::new());
        let out = press(&mut ui, Button::Enter, 1100);
        assert_eq!(out.deck_events().count(), 0);
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, None);
        let out = wheel(&mut ui, Wheel::Ccw, 1200);
        assert_eq!(ui.agents.choice, Some(0), "SHIFT+SCROLL slid, it did not choose");
        assert_ne!(ui.global.current, 0);
        let _ = out;
    }

    #[test]
    fn opening_a_card_dismisses_its_toast_without_filing_it() {
        let mut ui = hosted();
        host(&mut ui, &card(3, 33, CardKind::Question, 2), 1000);
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((3, 33)));
        show_channel(&mut ui, 3, 1100);
        press(&mut ui, Button::Enter, 1200);
        assert_eq!((ui.agents.view, ui.notify.toast), (AgentsView::Card, None));
        assert_eq!(ui.deck.notice_count(), 0);
    }

    #[test]
    fn displayed_ages_report_dirty_when_they_roll_over() {
        let mut ui = hosted();
        host(&mut ui, &agent(3, AgentState::Working), 1000);
        let mut out = Update::new();
        ui.tick(30_000, 0, true, &mut out);
        let mut out = Update::new();
        ui.tick(59_000, 0, true, &mut out);
        assert!(!out.dirty);
        let mut out = Update::new();
        ui.tick(61_000, 0, true, &mut out);
        assert!(out.dirty, "3 became '1m'");
    }

    #[test]
    fn disconnect_marks_roster_stale_and_disables_cards() {
        let mut ui = hosted();
        press(&mut ui, Button::Enter, 0);
        ui.set_connected(false, &mut Update::new());
        assert!(ui.deck.agents.iter().filter(|a| a.present).all(|a| a.stale));
        assert_eq!(ui.deck.order_len, 5, "the roster stays");
        let (x, y) = layout::CHANNEL_BUTTON.center();
        tap(&mut ui, x, y, 1000);
        assert_eq!(ui.agents.view, AgentsView::Card, "cards can be looked at");
        let (x, y) = layout::card_choice(0).center();
        assert_eq!(tap(&mut ui, x, y, 2000).deck_events().count(), 0, "but not answered");
        assert_eq!(ui.deck.card(1).unwrap().pending_answer, None);
    }

    fn reply_window(agent: u8, rev: u16, first: u16, total: u16) -> HostMsg {
        let text: std::vec::Vec<std::string::String> = (first
            ..first + crate::deck::msg::DOC_LINES as u16)
            .map(|line| std::format!("reply line {line}"))
            .collect();
        let mut held = [Text::new(); crate::deck::msg::DOC_LINES];
        for (slot, line) in text.iter().enumerate() {
            held[slot].set(line);
        }
        HostMsg::DocText {
            agent,
            doc: DocKind::Reply,
            id: rev,
            first,
            total,
            font: crate::deck::msg::DocFont::Text,
            lines: held,
            line_count: text.len() as u8,
        }
    }

    fn long_reply(agent: u8, rev: u16) -> HostMsg {
        HostMsg::Reply {
            agent,
            text: Text::from_str("a reply far longer than the line that carries it"),
            rev,
        }
    }

    /// Scrolling stops responding when the deck has already asked about the
    /// same line number for something else.
    ///
    /// `lines_asked` remembers only `(doc, first)`, and nothing resets it when
    /// the channel moves to a sibling. Agent 1's reply is scrolled until the
    /// deck asks for the window at some line; the channel then moves to agent
    /// 2 and is scrolled to the same line, and the request is swallowed as a
    /// duplicate. The window never comes, and because the deck stops at the
    /// edge of the window it holds rather than blanking the lane, the wheel
    /// simply does nothing from then on.
    #[test]
    fn a_sibling_reply_still_asks_for_a_window_at_a_line_already_asked_about() {
        let mut ui = hosted();
        for agent in [1u8, 2] {
            host(&mut ui, &long_reply(agent, 1), 1000);
            host(&mut ui, &reply_window(agent, 1, 0, 40), 1010);
        }

        show_channel(&mut ui, 1, 1100);
        let mut asked = None;
        for at in 1200..1220 {
            let out = wheel(&mut ui, Wheel::Ccw, at);
            if let Some(Event::DocLines { first, .. }) = events(&out).first() {
                asked = Some(*first);
                break;
            }
        }
        let asked = asked.expect("agent 1 asks for a window as it nears the edge");

        show_channel(&mut ui, 2, 1300);
        let mut sibling = None;
        for at in 1400..1420 {
            let out = wheel(&mut ui, Wheel::Ccw, at);
            if let Some(Event::DocLines { agent, first, .. }) = events(&out).first() {
                sibling = Some((*agent, *first));
                break;
            }
        }
        assert_eq!(
            sibling,
            Some((2, asked)),
            "the sibling's window was never asked for, so its wheel is dead"
        );
    }

    /// A window that never arrives must not wedge the wheel for good.
    ///
    /// At the edge of the window it holds the deck stops advancing the offset,
    /// so every further detent asks about the **same** line. With no expiry on
    /// the request, one lost window — dropped by the host as stale, or cut by
    /// a link blip — meant that line was never asked for again and scrolling
    /// was dead until the card or the channel was reopened.
    #[test]
    fn a_window_that_never_arrives_is_asked_for_again() {
        let mut ui = hosted();
        host(&mut ui, &long_reply(1, 1), 1000);
        host(&mut ui, &reply_window(1, 1, 0, 40), 1010);
        show_channel(&mut ui, 1, 1100);

        // Scroll to the edge of the held window and note what it asks for.
        let mut asked = None;
        for at in 1200..1240 {
            if let Some(Event::DocLines { first, .. }) = events(&wheel(&mut ui, Wheel::Ccw, at)).first()
            {
                asked = Some(*first);
            }
        }
        let asked = asked.expect("the edge of the window asks for the next one");
        let pinned = ui.agents.reply_top;

        // The host never answers. Further detents stay quiet: the request is
        // in flight and asking again would only spend the link.
        for at in 1300..1310 {
            assert!(
                events(&wheel(&mut ui, Wheel::Ccw, at)).is_empty(),
                "asked once while it is in flight, not once a detent"
            );
        }
        assert_eq!(ui.agents.reply_top, pinned, "and the offset holds at the edge");

        // Past the retry window it asks again rather than staying dead.
        let out = wheel(&mut ui, Wheel::Ccw, 1240 + ASK_RETRY_MS + 1);
        assert_eq!(
            events(&out),
            [Event::DocLines {
                agent: 1,
                doc: DocKind::Reply,
                id: 1,
                first: asked
            }],
            "a lost window is asked for again"
        );
    }

    /// The wheel keeps up with the link.
    ///
    /// Scrolls a long reply at a realistic rate against a host answering at
    /// the rate the real link was measured at, and counts the detents that
    /// move nothing. Holding a single chunk this was **66% dead, with runs of
    /// 2.2 s** — the operator reported it as "some scrolls are not responded",
    /// and they were right. Buffering `DOC_HELD` lines and asking for the
    /// chunk beyond the buffer takes it to about 1%.
    ///
    /// The numbers here are the measured ones: a chunk took 2.0-2.5 s on the
    /// real link and a brisk scroll is a line every 200 ms.
    #[test]
    fn the_wheel_keeps_up_with_the_link() {
        const DETENT_MS: u32 = 200;
        const HOST_MS: u32 = 2500;
        let mut ui = hosted();
        host(&mut ui, &long_reply(1, 1), 0);
        host(&mut ui, &reply_window(1, 1, 0, 400), 10);
        show_channel(&mut ui, 1, 20);

        let mut at = 100u32;
        let mut inflight: Option<(u16, u32)> = None;
        let (mut dead, mut run, mut worst) = (0usize, 0usize, 0usize);
        for _ in 0..120 {
            if let Some((first, due)) = inflight {
                if at >= due {
                    host(&mut ui, &reply_window(1, 1, first, 400), at);
                    inflight = None;
                }
            }
            let before = ui.agents.reply_top;
            let out = wheel(&mut ui, Wheel::Ccw, at);
            if ui.agents.reply_top == before {
                dead += 1;
                run += 1;
                worst = core::cmp::max(worst, run);
            } else {
                run = 0;
            }
            if let Some(Event::DocLines { first, .. }) = events(&out).first() {
                if inflight.is_none() {
                    // The host serves the chunk starting where it was asked.
                    inflight = Some((*first, at + HOST_MS));
                }
            }
            at += DETENT_MS;
        }
        assert!(
            dead * 10 < 120,
            "the wheel was dead for {dead} of 120 detents; a single chunk was 80"
        );
        assert!(
            worst * (DETENT_MS as usize) <= 1_000,
            "longest run of dead detents was {} ms",
            worst * DETENT_MS as usize
        );
    }
    #[test]
    fn rich_chat_follows_live_until_scrolled_and_shift_enter_resumes() {
        let mut ui = hosted();
        show_channel(&mut ui, 3, 100);
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 1 }, 101);
        let window = |first, total| {
            let mut lines = [Text::new(); crate::deck::msg::DOC_LINES];
            for line in &mut lines { line.set("p0Live text"); }
            HostMsg::DocText { agent: 3, doc: DocKind::Reply, id: 1, first, total, font: crate::deck::msg::DocFont::RichRow, lines, line_count: crate::deck::msg::DOC_LINES as u8 }
        };
        host(&mut ui, &window(0, 17), 102);
        let before = faces::agents::reply_clamp(&ui, ui.deck.agent(3).unwrap(), ui.agents.reply_top);
        assert!(before > 0);
        wheel(&mut ui, Wheel::Cw, 500);
        assert!(!ui.agents.reply_follow);
        let pinned = ui.agents.reply_top;
        host(&mut ui, &window(0, 25), 510);
        assert_eq!(ui.agents.reply_top, pinned);
        ui.apply(Control::Button(Button::Shift, Edge::Press), 600, &mut Update::new());
        let out = press(&mut ui, Button::Enter, 610);
        assert!(ui.agents.reply_follow);
        assert!(events(&out).iter().any(|e| matches!(e, Event::DocLines { agent: 3, doc: DocKind::Reply, first: u16::MAX, .. })));
        host(&mut ui, &window(8, 25), 620);
        assert_eq!(ui.deck.agent(3).unwrap().reply.total, 25);
        assert!(faces::agents::reply_clamp(&ui, ui.deck.agent(3).unwrap(), ui.agents.reply_top) > before);
        ui.apply(Control::Button(Button::Shift, Edge::Release), 630, &mut Update::new());
        wheel(&mut ui, Wheel::Cw, 3000);
        assert!(!ui.agents.reply_follow);
        let out = wheel(&mut ui, Wheel::Ccw, 3100);
        assert!(ui.agents.reply_follow, "scrolling back to the end resumes following");
        assert!(events(&out).iter().any(|e| matches!(e, Event::DocLines { first: u16::MAX, .. })));
        host(&mut ui, &window(16, 33), 3200);
        let a = ui.deck.agent(3).unwrap();
        assert_eq!(faces::agents::reply_clamp(&ui, a, ui.agents.reply_top) as usize + faces::agents::reply_visible(&ui, a), 33);
        show_roster(&mut ui, 3300);
        show_channel(&mut ui, 3, 3400);
        assert_eq!(ui.agents.reply_top, u16::MAX, "opening a conversation starts at the end");
    }

    #[test]
    fn tool_rows_toggle_only_on_a_deliberate_tap_of_the_same_entry_and_revision() {
        let mut ui = hosted();
        show_channel(&mut ui, 3, 100);
        let window = |id, label: &str| {
            let mut lines = [Text::new(); crate::deck::msg::DOC_LINES];
            lines[0].set(label);
            lines[1].set("p0Assistant text is not tappable");
            HostMsg::DocText { agent: 3, doc: DocKind::Reply, id, first: 0, total: 2, font: crate::deck::msg::DocFont::RichRow, lines, line_count: 2 }
        };
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 1 }, 101);
        host(&mut ui, &window(1, "t005Command · done"), 102);
        let lane = faces::agents::reply_rect(&ui, ui.deck.agent(3).unwrap());
        let (x, y) = (lane.left() + 80, lane.top() + 10);
        let out = tap(&mut ui, x, y, 200);
        assert_eq!(events(&out), [Event::Tool { agent: 3, id: 1, index: 5 }]);
        assert!(!ui.agents.reply_follow, "opening details holds the reading position");
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 2 }, 300);
        host(&mut ui, &window(2, "T005Command · done"), 301);
        assert!(ui.agents.reply_follow, "details that fit through the tail keep following");
        assert_eq!(events(&tap(&mut ui, x, y, 400)), [Event::Tool { agent: 3, id: 2, index: 5 }]);
        let touch = |phase, y| Control::Touch(Touch { phase, x: x as u16, y: y as u16 });
        ui.apply(touch(TouchPhase::Down, y), 500, &mut Update::new());
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 3 }, 501);
        host(&mut ui, &window(3, "t005Different tool · done"), 502);
        let mut out = Update::new();
        ui.apply(touch(TouchPhase::Up, y), 520, &mut out);
        assert!(events(&out).is_empty(), "a relayout under the finger cannot toggle a new entry");
        ui.apply(touch(TouchPhase::Down, y), 600, &mut Update::new());
        ui.apply(touch(TouchPhase::Move, y + 70), 610, &mut Update::new());
        ui.apply(touch(TouchPhase::Up, y), 620, &mut out);
        assert!(events(&out).is_empty(), "dragging off a row cancels the tap");
        assert!(events(&tap(&mut ui, x, y + 40, 700)).is_empty());
        tap(&mut ui, x, y, 800);
        host(&mut ui, &HostMsg::Reply { agent: 3, text: Text::from_str("Chat"), rev: 4 }, 900);
        let mut expanded = window(4, "T005Command · running");
        if let HostMsg::DocText { total, line_count, lines, .. } = &mut expanded {
            *total = 40;
            *line_count = crate::deck::msg::DOC_LINES as u8;
            for line in &mut lines[1..] { line.set("c0Tool output"); }
        }
        host(&mut ui, &expanded, 901);
        assert!(!ui.agents.reply_follow, "long tool details hold the touched header in view");
        assert_eq!(ui.agents.reply_top, 0);
    }

}
