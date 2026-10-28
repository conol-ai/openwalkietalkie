//! Agents face: roster (mockup 6), channel, question card (mockup 8), ready
//! card. Draws only from the pub view fields and the deck model.
//!
//! Every view has a fixed list of lanes (header, rows, choices, the "more rows"
//! line, the hint bar). `signature` registers the same lanes hashed by exactly
//! what their pixels read, so a highlight move or a title push repaints one
//! row, not the frame; the lane list of a view never changes shape, and the
//! view kind is folded into the mode. Time reaches `draw` only as the displayed
//! age step (`kit::age_step`), which the signatures hash as such.
//!
//! Rules made visible here: the roster is one project's page, its header the
//! project's name and its place among the pages ("COHDL · 1/2"), and a row
//! named after its project (a hook-registered session is named after its
//! directory) shows its kind alone; the active channel is a 3 px bar left of
//! its row, distinct from the highlight outline; a row's title gets " · {age}" for
//! Working/Question agents when it fits without truncating the title (never
//! for an age under a minute); the question body is the host's own wrapped
//! lines, drawn two at a time in Title or three in Name as the host measured
//! them (`layout::CARD_QUESTION` holds two Title lines and the choices start
//! at y 156), scrolled by `ui.agents.question_top` with a bar in the gutter
//! when more of them exist than fit — a card that arrived without a window is
//! wrapped here instead, exactly as it always was; the answered choice shows
//! "sent" and the others dim; without a deck host (and not demo) the tag reads
//! "offline" and the hint bar says so.
use crate::deck::model::{Agent, Card, MAX_AGENTS};
use crate::deck::msg::{AgentKind, AgentState, DocFont, DocKind, CardKind};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::kit::list::Viewport;
use crate::ui::kit::{Hint, HeaderRight};
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::{AgentsView, UiState};
use crate::ui::{font, font::Align, font::Font, kit, layout, layout::Rect, theme};

// ---- lanes (geometry the signatures and the damage share) --------------------
/// Header text, dot and tag.
pub const HEADER_LANE: Rect = Rect::new(24, 20, 432, 40);
/// Hairline and hint bar.
pub const HINT_LANE: Rect = Rect::new(0, theme::HINT_Y - 1, theme::SCREEN, theme::SCREEN - theme::HINT_Y + 1);
/// "↑ 1 more · 2 more ↓" under the roster rows / the card rows.
pub const ROSTER_MORE: Rect = Rect::new(32, 378, 416, 48);
pub const CARD_MORE: Rect = Rect::new(32, 392, 416, 34);
/// The question: two Title lines in `CARD_QUESTION`, or three Name lines
/// ending just above the first choice row.
pub const QUESTION_LANE: Rect = Rect::new(32, 60, 416, 112);
/// The question's scroll bar, in the 5 px gutter `CARD_QUESTION` leaves at
/// the right of the lane: 3 px wide, like the roster's active-channel bar.
pub const QUESTION_BAR: Rect = Rect::new(444, 74, 3, 78);
/// The firmware face a `DocFont` names. The host measured with it; the deck
/// draws with the same one and never re-wraps.
pub const fn doc_face(font: DocFont) -> Font {
    match font {
        DocFont::Title => Font::Title,
        DocFont::Name => Font::Name,
        DocFont::Row => Font::Row,
        DocFont::Text => Font::Text,
        DocFont::RichRow => Font::Row,
        DocFont::RichText => Font::Text,
        DocFont::RichTitle => Font::Reading,
    }
}

/// The reply takes the space left by the currently visible card controls.
pub fn reply_rect(ui: &UiState, agent: &Agent) -> Rect {
    layout::channel_reply(ui.deck.card(agent.id).map(Card::open))
}

/// The scroll bar stays in the gutter, over the full reading height.
fn reply_bar(ui: &UiState, agent: &Agent) -> Rect {
    let rect = reply_rect(ui, agent);
    Rect::new(444, rect.top() + 4, 3, rect.height() - 8)
}

/// Include the text and scroll bar in the same damage region.
fn reply_lane(ui: &UiState, agent: &Agent) -> Rect {
    let rect = reply_rect(ui, agent);
    // CJK/accents can rise above the nominal line box. Include those pixels
    // in both culling and damage so changing channels erases them too.
    let overhang = font::top_overhang(doc_face(reply_face(ui, agent)));
    Rect::new(32, rect.top() - overhang, 416, rect.height() + overhang)
}
/// Ready summary, ending just above the first step row.
pub const SUMMARY_LANE: Rect = Rect::new(32, 60, 416, 157);
pub const BUTTON_LANE: Rect = layout::CHANNEL_BUTTON;
/// Mode switch shares the state line, leaving the entire reading lane intact.
pub const WORKFLOW_TOGGLE: Rect = Rect::new(316, 68, 128, 30);
/// A roster row and the active-channel bar beside it.
pub const fn roster_lane(slot: u8) -> Rect {
    let row = layout::roster_row(slot);
    Rect::new(row.left() - 8, row.top(), row.width() + 8, row.height())
}

/// Text stops this far from a row's right edge.
const ROW_PAD: i32 = 12;
/// A scroll thumb never shrinks below this, however long the body is.
const QUESTION_THUMB_MIN: i32 = 8;
/// Room the header keeps for its right tag ("blocked 12d", the battery).
const TAG_ROOM: i32 = 100;
const CHANNEL_STATE_BASELINE: i32 = 89;

// ---- hint bars (pub so the fit test can measure every one) --------------------
const fn h(key: &'static str, label: &'static str) -> Hint<'static> {
    Hint { key, label }
}
const PROJECT: Hint<'static> = h("\u{2039} \u{203a}", "project");
const AGENT: Hint<'static> = h("\u{2039} \u{203a}", "agent");
const TALK: Hint<'static> = h("PTT", "talk");
const BACK: Hint<'static> = h("CANCEL", "back");
pub const ROSTER_HINTS: [Hint<'static>; 4] = [PROJECT, h("SCROLL", "select"), h("ENTER", "open"), TALK];
pub const CHANNEL_CARD_HINTS: [Hint<'static>; 4] = [AGENT, h("ENTER", "card"), TALK, BACK];
pub const CHANNEL_SHIFT_HINTS: [Hint<'static>; 2] = [h("ENTER", "latest message"), h("SCROLL", "face")];
pub const CHANNEL_HINTS: [Hint<'static>; 4] = [h("ENTER", "workflow"), h("SCROLL", "chat"), TALK, BACK];
pub const WORKFLOW_HINTS: [Hint<'static>; 4] = [h("ENTER", "chat"), h("SCROLL", "steps"), TALK, BACK];
pub const WORKFLOW_SHIFT_HINTS: [Hint<'static>; 2] = [h("ENTER", "current step"), h("SCROLL", "face")];
pub const QUESTION_HINTS: [Hint<'static>; 4] =
    [h("SCROLL", "choose"), h("ENTER", "confirm"), h("PTT", "reply"), h("CANCEL", "snooze")];
pub const QUESTION_VOICE_HINTS: [Hint<'static>; 2] = [h("PTT", "reply"), h("CANCEL", "snooze")];
pub const QUESTION_SENT_HINTS: [Hint<'static>; 1] = [h("", "answer sent \u{b7} waiting for the host")];
pub const QUESTION_OFFLINE_HINTS: [Hint<'static>; 2] = [h("", "offline \u{b7} answers need the host"), BACK];
pub const READY_OPEN_HINTS: [Hint<'static>; 4] = [h("SCROLL", "steps"), h("ENTER", "open"), TALK, BACK];
pub const READY_SEND_HINTS: [Hint<'static>; 4] = [h("SCROLL", "steps"), h("ENTER", "send"), TALK, BACK];
pub const READY_PLAIN_HINTS: [Hint<'static>; 3] = [h("ENTER", "open"), TALK, BACK];
pub const READY_OFFLINE_HINTS: [Hint<'static>; 3] = [h("", "offline \u{b7} steps need the host"), h("ENTER", "open"), BACK];
pub const EMPTY_HINTS: [Hint<'static>; 1] = [BACK];
pub const ALL_HINTS: [&[Hint<'static>]; 17] = [
    &ROSTER_HINTS,
    &CHANNEL_CARD_HINTS,
    &CHANNEL_HINTS,
    &CHANNEL_SHIFT_HINTS,
    &WORKFLOW_HINTS,
    &WORKFLOW_SHIFT_HINTS,
    &QUESTION_HINTS,
    &QUESTION_VOICE_HINTS,
    &QUESTION_SENT_HINTS,
    &QUESTION_OFFLINE_HINTS,
    &READY_OPEN_HINTS,
    &READY_SEND_HINTS,
    &READY_PLAIN_HINTS,
    &READY_OFFLINE_HINTS,
    &EMPTY_HINTS,
    &crate::ui::overlay::ptt::HINTS,
    &crate::ui::overlay::ptt::NO_HOST_HINTS,
];

#[cfg(test)]
std::thread_local! {
    /// Runs this thread drew that ended in "…" (counted per band pass): the
    /// mockup scenes must draw none.
    pub static ELLIPSES: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// `font::draw` on a flat fill, counting truncations under test.
#[allow(clippy::too_many_arguments)]
pub fn text(band: &mut Band, font: Font, x: i32, baseline: i32, s: &str, fg: u32, bg: u32, max_w: i32, align: Align) -> i32 {
    #[cfg(test)]
    if font::measure(font, s) > max_w {
        ELLIPSES.with(|count| count.set(count.get() + 1));
    }
    font::draw(band, font, x, baseline, s, fg, Some(bg), max_w, align)
}

/// Baseline that centres capitals of `font` on `cy`.
fn centred(font: Font, cy: i32) -> i32 {
    cy + font::ascent(font) * 9 / 20
}

/// Which of `ALL_HINTS` a bar shows, for the hint lane's hash.
fn hint_variant(hints: &'static [Hint<'static>]) -> u32 {
    ALL_HINTS.iter().position(|set| core::ptr::eq(*set, hints)).map_or(99, |index| index as u32)
}

/// No deck host and no demo standing in for one: dots gray, cards inert.
pub fn offline(ui: &UiState) -> bool {
    !ui.deck.host_present && !ui.deck.demo_active()
}

pub const fn kind(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
        AgentKind::Gemini => "Gemini",
        AgentKind::Other => "Agent",
        AgentKind::Conol => "Conol",
    }
}

/// "Codex · cohdl-api".
pub fn kind_name(agent: &Agent) -> Text<40> {
    let mut out = Text::new();
    out.push_str(kind(agent.kind));
    out.push_str(" \u{b7} ");
    out.push_str(agent.name.as_str());
    out
}

/// A roster row's first line: `kind_name`, or the kind alone when the name
/// is the project's own — a hook-registered session is named after its
/// directory, and the page header already says that.
pub fn row_name(agent: &Agent) -> Text<40> {
    if agent.kind == AgentKind::Conol { return Text::from_str(agent.name.as_str()); }
    if !agent.project.is_empty() && agent.name == agent.project {
        Text::from_str(kind(agent.kind))
    } else {
        kind_name(agent)
    }
}

/// A roster row's second line: the title, unless it is the project's own
/// name — a session with no prompt title yet is titled after its directory,
/// which the page header already says.
pub fn row_title(agent: &Agent) -> &str {
    if !agent.project.is_empty() && agent.title == agent.project {
        ""
    } else {
        agent.title.as_str()
    }
}

/// The roster's header: the page's project in the header's capitals and its
/// place among the pages when there is more than one ("COHDL · 1/2"); "OTHER"
/// is the page of the agents without a project; "AGENTS" with nothing on the
/// roster at all.
pub fn roster_header(ui: &UiState, out: &mut Text<40>) {
    out.clear();
    if ui.deck.order_len == 0 {
        out.push_str("AGENTS");
        return;
    }
    let label = ui.agents.project.as_str();
    if label.is_empty() {
        out.push_str("OTHER");
    } else {
        for c in label.chars() {
            out.push_char(c.to_ascii_uppercase());
        }
    }
    let mut heads = [0; MAX_AGENTS];
    let count = ui.deck.project_heads(&mut heads);
    if count > 1 {
        let page = heads[..count]
            .iter()
            .position(|&head| ui.deck.project_of(head) == label)
            .unwrap_or(0);
        out.push_str(" \u{b7} ");
        push_count(out, page as u8 + 1);
        out.push_char('/');
        push_count(out, count as u8);
    }
}

fn dot_color(ui: &UiState, agent: &Agent) -> u32 {
    theme::agent_state(agent.state, agent.stale || offline(ui))
}

/// The age an agent's row, state line or tag shows: 0 = under a minute (shown
/// as nothing), never shown for Idle or while offline.
fn age_step(ui: &UiState, agent: &Agent) -> u32 {
    if agent.state == AgentState::Idle || agent.stale || offline(ui) {
        0
    } else {
        kit::age_step(ui.now_ms, agent.state_since_ms)
    }
}

/// `prefix` then the age ("blocked 2m", " · 2m"); nothing when the age is 0.
fn age_text<const N: usize>(ui: &UiState, agent: &Agent, prefix: &str, out: &mut Text<N>) {
    out.clear();
    if age_step(ui, agent) == 0 {
        return;
    }
    let mut label = Text::<8>::new();
    kit::age_label(ui.now_ms, agent.state_since_ms, &mut label);
    out.push_str(prefix);
    out.push_str(label.as_str());
}

fn push_count<const N: usize>(out: &mut Text<N>, n: u8) {
    if n >= 10 {
        out.push_char((b'0' + n / 10 % 10) as char);
    }
    out.push_char((b'0' + n % 10) as char);
}

/// Header tag of a card: "blocked 2m" / "ready 5m" (just the word under a
/// minute) built in `out`, or the deck's own tag when it is not live.
fn card_tag<'a>(ui: &UiState, agent: &Agent, card: &Card, out: &'a mut Text<16>) -> HeaderRight<'a> {
    if offline(ui) {
        return kit::status_tag(ui);
    }
    let word = match card.kind {
        CardKind::Question => "blocked",
        CardKind::Ready => "ready",
    };
    out.set(word);
    let mut age = Text::<12>::new();
    age_text(ui, agent, " ", &mut age);
    out.push_str(age.as_str());
    HeaderRight::Text(out.as_str())
}

/// The row/button surface, returning the fill its text sits on. Selected =
/// the bright card; idle = a thin outline (choices, buttons) or nothing
/// (roster rows, as in mockup 6); pressed rows show the chip fill inside.
fn surface(band: &mut Band, rect: Rect, selected: bool, pressed: bool, outlined: bool) -> u32 {
    if selected || outlined {
        kit::card(band, rect, selected, false);
    }
    if pressed {
        kit::round(band, rect.inset(2), theme::CARD_R.saturating_sub(2), theme::CHIP_FILL);
        theme::CHIP_FILL
    } else if selected {
        theme::CARD_FILL
    } else {
        theme::CANVAS
    }
}

/// Dot, "Kind · name" and the right tag (a low battery displaces it).
fn header_with_dot(band: &mut Band, ui: &UiState, agent: &Agent, right: HeaderRight) {
    if !kit::visible(band, HEADER_LANE) {
        return;
    }
    let (cx, cy, r) = layout::HEADER_DOT;
    kit::dot(band, cx, cy, r, dot_color(ui, agent));
    let max_w = theme::TEXT_RIGHT - TAG_ROOM - layout::HEADER_NAME_X;
    let name = kind_name(agent);
    text(band, Font::Name, layout::HEADER_NAME_X, theme::HEADER_BASELINE, name.as_str(), theme::TEXT_PRIMARY, theme::CANVAS, max_w, Align::Left);
    kit::header_tag(band, kit::header_right(ui, right));
}

fn hash_header(h: &mut Hasher, ui: &UiState, agent: &Agent, right: HeaderRight) {
    h.u32(dot_color(ui, agent)).u8(agent.kind as u8).text(&agent.name);
    h.header_right(kit::header_right(ui, right));
}

/// "↑ 1 more · 2 more ↓" centred in `lane`.
fn draw_more(band: &mut Band, lane: Rect, viewport: &Viewport) {
    let (above, below) = more(viewport);
    if (above == 0 && below == 0) || !kit::visible(band, lane) {
        return;
    }
    let mut line = Text::<24>::new();
    if above > 0 {
        line.push_str("\u{2191} ");
        push_count(&mut line, above);
        line.push_str(" more");
    }
    if above > 0 && below > 0 {
        line.push_str(" \u{b7} ");
    }
    if below > 0 {
        push_count(&mut line, below);
        line.push_str(" more \u{2193}");
    }
    let (cx, _) = lane.center();
    text(band, Font::Small, cx, lane.top() + 22, line.as_str(), theme::TEXT_FAINT, theme::CANVAS, lane.width(), Align::Center);
}

fn more(viewport: &Viewport) -> (u8, u8) {
    let shown = viewport.top.saturating_add(viewport.visible);
    (viewport.top, viewport.count.saturating_sub(shown))
}

/// The rows of the page shown: the highlighted agent's project.
fn roster_viewport(ui: &UiState) -> Viewport {
    let rows = ui.deck.page_len(ui.agents.project.as_str());
    Viewport::new(ui.agents.top_row, layout::ROSTER_VISIBLE, rows)
}

fn roster_agent<'a>(ui: &'a UiState, viewport: &Viewport, slot: u8) -> Option<&'a Agent> {
    let row = viewport.index_at(slot)?;
    ui.deck.page_agent(ui.agents.project.as_str(), row)
}

fn choices_viewport(ui: &UiState, card: &Card, visible: u8) -> Viewport {
    Viewport::new(ui.agents.choice_top, visible, card.choice_count.min(4))
}

/// Draw the face in logical coordinates; `instance` is the strip entry's id.
pub fn draw(band: &mut Band, ui: &UiState, instance: u8) {
    let _ = instance;
    band.fill(kit::rgb(theme::CANVAS));
    if ui.image.open && ui.agents.view == AgentsView::Channel { crate::ui::image::draw(band, ui); return; }
    if ui.agents.settings.open && ui.agents.view == AgentsView::Channel {
        crate::ui::session::draw(band, ui); return;
    }
    let agent = ui.deck.agent(ui.agents.agent);
    match (ui.agents.view, agent, ui.deck.card(ui.agents.agent)) {
        (AgentsView::Roster, _, _) => draw_roster(band, ui),
        (AgentsView::Channel, Some(agent), _) => draw_channel(band, ui, agent),
        (AgentsView::Card, Some(agent), Some(card)) => match card.kind {
            CardKind::Question => draw_question(band, ui, agent, card),
            CardKind::Ready => draw_ready(band, ui, agent, card),
        },
        (AgentsView::Card, Some(agent), None) => draw_empty(band, ui, Some(agent), "Nothing pending"),
        (_, None, _) => draw_empty(band, ui, None, "No agent"),
    }
}

/// Regions of this face and the inputs of their pixels; see `signature.rs`.
pub fn signature(ui: &UiState, instance: u8, sig: &mut SigBuilder) {
    let _ = instance;
    if ui.image.open && ui.agents.view == AgentsView::Channel { crate::ui::image::signature(ui, sig); return; }
    if ui.agents.settings.open && ui.agents.view == AgentsView::Channel {
        crate::ui::session::signature(ui, sig); return;
    }
    let agent = ui.deck.agent(ui.agents.agent);
    match (ui.agents.view, agent, ui.deck.card(ui.agents.agent)) {
        (AgentsView::Roster, _, _) => {
            sig.mode(1);
            roster_signature(ui, sig);
        }
        (AgentsView::Channel, Some(agent), _) => {
            sig.mode(2);
            channel_signature(ui, agent, sig);
        }
        (AgentsView::Card, Some(agent), Some(card)) => {
            sig.mode(3 + card.kind as u32);
            card_signature(ui, agent, card, sig);
        }
        (AgentsView::Card, Some(agent), None) => {
            sig.mode(5);
            empty_signature(ui, Some(agent), sig);
        }
        (_, None, _) => {
            sig.mode(6);
            empty_signature(ui, None, sig);
        }
    }
}

// ---- roster -------------------------------------------------------------------


/// Conol's robot mosaic, the same 16×16 mark as the desktop asset.
const CONOL_MARK: [u16; 16] = [
    0b0000000000000000,
    0b0000000110000000,
    0b0001100110011000,
    0b0001100110011000,
    0b0000100110010000,
    0b0000011111100000,
    0b0001111111111000,
    0b0011100000011100,
    0b0011000000001100,
    0b0011001101101100,
    0b0011001101101100,
    0b0011000000001100,
    0b0011001001001100,
    0b0011100110011100,
    0b0001111111111000,
    0b0000000000000000
];
fn conol_mark(band: &mut Band, x: i32, y: i32) {
    for (row, bits) in CONOL_MARK.iter().enumerate() {
        for col in 0..16 {
            if bits & (1 << (15 - col)) != 0 {
                kit::fill(band, Rect::new(x + col * 2, y + row as i32 * 2, 2, 2), 0xFFE238);
            }
        }
    }
}

fn conol_project(ui: &UiState) -> bool {
    ui.deck.agents.iter().any(|a| a.kind == AgentKind::Conol && a.project.as_str() == ui.agents.project.as_str())
}

fn draw_roster(band: &mut Band, ui: &UiState) {
    let mut label = Text::<40>::new();
    roster_header(ui, &mut label);
    let is_conol = conol_project(ui);
    if is_conol {
        conol_mark(band, theme::TEXT_X, 21);
        font::draw(band, Font::Label, theme::TEXT_X + 40, theme::HEADER_BASELINE,
            label.as_str(), theme::TEXT_LABEL, Some(theme::CANVAS), 220, Align::Left);
        kit::header_tag(band, kit::header_right(ui, kit::status_tag(ui)));
    } else {
        kit::header(band, label.as_str(), kit::header_right(ui, kit::status_tag(ui)));
    }
    let viewport = roster_viewport(ui);
    for slot in 0..layout::ROSTER_VISIBLE {
        if let Some(agent) = roster_agent(ui, &viewport, slot) {
            draw_roster_row(band, ui, slot, agent);
        }
    }
    draw_more(band, ROSTER_MORE, &viewport);
    kit::hint_bar(band, &ROSTER_HINTS);
}

fn draw_roster_row(band: &mut Band, ui: &UiState, slot: u8, agent: &Agent) {
    if !kit::visible(band, roster_lane(slot)) {
        return;
    }
    let rect = layout::roster_row(slot);
    let selected = agent.id == ui.agents.highlight;
    let bg = surface(band, rect, selected, ui.agents.pressed == Some(agent.id), false);
    if agent.id == ui.global.active_channel {
        kit::fill(band, layout::roster_active_bar(slot), theme::TEXT_PRIMARY);
    }
    let (dx, dy, r) = layout::ROSTER_DOT;
    kit::dot(band, rect.left() + dx, rect.top() + dy, r, dot_color(ui, agent));
    let x = rect.left() + layout::ROSTER_TEXT_X;
    let max_w = rect.right() - ROW_PAD - x;
    let name = row_name(agent);
    text(band, Font::Name, x, rect.top() + layout::ROSTER_NAME_BASELINE, name.as_str(), theme::TEXT_PRIMARY, bg, max_w, Align::Left);
    let baseline = rect.top() + layout::ROSTER_TITLE_BASELINE;
    let title = row_title(agent);
    let mut suffix = Text::<12>::new();
    if matches!(agent.state, AgentState::Working | AgentState::Question) {
        age_text(ui, agent, " \u{b7} ", &mut suffix);
    }
    // The age is appended only when the whole title still fits beside it.
    let title_w = font::measure(Font::Body, title);
    if !suffix.is_empty() && title_w + font::measure(Font::Body, suffix.as_str()) <= max_w {
        let used = text(band, Font::Body, x, baseline, title, theme::TEXT_SECONDARY, bg, max_w, Align::Left);
        text(band, Font::Body, x + used, baseline, suffix.as_str(), theme::TEXT_SECONDARY, bg, max_w - used, Align::Left);
    } else {
        text(band, Font::Body, x, baseline, title, theme::TEXT_SECONDARY, bg, max_w, Align::Left);
    }
}

fn roster_signature(ui: &UiState, sig: &mut SigBuilder) {
    sig.region_with(HEADER_LANE, |h| {
        let mut label = Text::<40>::new();
        roster_header(ui, &mut label);
        h.text(&label).u8(conol_project(ui) as u8).header_right(kit::header_right(ui, kit::status_tag(ui)));
    });
    let viewport = roster_viewport(ui);
    for slot in 0..layout::ROSTER_VISIBLE {
        sig.region_with(roster_lane(slot), |h| match roster_agent(ui, &viewport, slot) {
            None => {
                h.u8(0);
            }
            Some(agent) => {
                h.u8(agent.id)
                    .u8(agent.kind as u8)
                    .text(&agent.name)
                    .text(&agent.project)
                    .text(&agent.title)
                    .u32(dot_color(ui, agent))
                    .bool(agent.id == ui.agents.highlight)
                    .bool(agent.id == ui.global.active_channel)
                    .bool(ui.agents.pressed == Some(agent.id))
                    .u32(if matches!(agent.state, AgentState::Working | AgentState::Question) {
                        age_step(ui, agent)
                    } else {
                        0
                    });
            }
        });
    }
    sig.region_with(ROSTER_MORE, |h| {
        let (above, below) = more(&viewport);
        h.u8(above).u8(below);
    });
    sig.region(HINT_LANE, hint_variant(&ROSTER_HINTS));
}

// ---- channel ------------------------------------------------------------------

fn channel_hints(card: Option<&Card>) -> &'static [Hint<'static>] {
    match card {
        Some(card) if card.open() => &CHANNEL_CARD_HINTS,
        _ => &CHANNEL_HINTS,
    }
}

fn reading_hints(ui: &UiState, agent: &Agent, card: Option<&Card>) -> &'static [Hint<'static>] {
    if agent.reply_kind == DocKind::Workflow {
        if ui.global.shift { &WORKFLOW_SHIFT_HINTS }
        else if card.is_some_and(Card::open) { &CHANNEL_CARD_HINTS }
        else { &WORKFLOW_HINTS }
    } else if ui.global.shift { &CHANNEL_SHIFT_HINTS } else { channel_hints(card) }
}

fn draw_channel(band: &mut Band, ui: &UiState, agent: &Agent) {
    header_with_dot(band, ui, agent, kit::status_tag(ui));
    draw_state_line(band, ui, agent);
    draw_reply(band, ui, agent);
    let card = ui.deck.card(agent.id);
    if let Some(card) = card {
        draw_card_block(band, ui, card);
    }
    kit::hint_bar(band, reading_hints(ui, agent, card));
}

/// "Question · blocked 2m" in the state colour, then secondary text.
fn draw_state_line(band: &mut Band, ui: &UiState, agent: &Agent) {
    let lane = Rect::new(layout::CHANNEL_STATE.left(), layout::CHANNEL_STATE.top(), WORKFLOW_TOGGLE.left() - layout::CHANNEL_STATE.left() - 8, layout::CHANNEL_STATE.height());
    if !kit::visible(band, Rect::new(32, 66, 416, 34)) {
        return;
    }
    let bg = surface(band, WORKFLOW_TOGGLE, false, ui.agents.pressed == Some(2), true);
    text(band, Font::Body, WORKFLOW_TOGGLE.center().0, CHANNEL_STATE_BASELINE,
        if agent.reply_kind == DocKind::Workflow { "Chat" } else { "Workflow" },
        theme::TEXT_PRIMARY, bg, WORKFLOW_TOGGLE.width() - 12, Align::Center);
    let label = crate::ui::session::label(ui);
    text(band, Font::Hint, lane.left(), CHANNEL_STATE_BASELINE, label.as_str(),
        theme::TEXT_SECONDARY, theme::CANVAS, lane.width() - 12, Align::Left);
    text(band, Font::Hint, lane.right() - 10, CHANNEL_STATE_BASELINE, "›",
        theme::TEXT_SECONDARY, theme::CANVAS, 12, Align::Left);
}

/// Lines the last reply occupies in the channel view; the grammar lets
/// `reply_top` scroll until the last visible lines of them show.
///
/// With a window from the host this is the whole reply's length, however long
/// that is. Without one it is what the capped `reply` line wraps to here, which
/// is what the deck did before the host laid anything out.
pub fn reply_lines(ui: &UiState, agent: &Agent) -> usize {
    if agent.reply.windowed() {
        return agent.reply.total as usize;
    }
    let face = doc_face(reply_face(ui, agent));
    let mut lines = 0;
    font::wrap(face, agent.last_reply.as_str(), layout::CHANNEL_REPLY.width(), 32, &mut |index, _, _| {
        lines = index + 1;
    });
    lines
}

/// The first line the reply can be scrolled to: the deck never scrolls out of
/// the window it holds, so a window the host has not answered for yet stops the
/// scroll instead of blanking the lane.
pub fn reply_first(agent: &Agent) -> usize {
    if agent.reply.windowed() {
        agent.reply.first as usize
    } else {
        0
    }
}

/// One past the last line the reply can be scrolled to.
pub fn reply_end(ui: &UiState, agent: &Agent) -> usize {
    if agent.reply.windowed() {
        agent.reply.first as usize + agent.reply.held as usize
    } else {
        reply_lines(ui, agent)
    }
}

/// `top` brought inside the window the reply holds.
pub fn reply_clamp(ui: &UiState, agent: &Agent, top: u16) -> u16 {
    let floor = reply_first(agent);
    let ceiling = reply_end(ui, agent)
        .saturating_sub(reply_visible(ui, agent))
        .max(floor);
    (top as usize).clamp(floor, ceiling) as u16
}

/// The reply's scroll thumb, or `None` when nothing is hidden.
fn reply_thumb(ui: &UiState, agent: &Agent, top: usize) -> Option<Rect> {
    let visible = reply_visible(ui, agent);
    let total = reply_lines(ui, agent);
    if total <= visible {
        return None;
    }
    let bar = reply_bar(ui, agent);
    let height = bar.height();
    let thumb = (height * visible as i32 / total as i32).max(QUESTION_THUMB_MIN);
    let offset = (height - thumb) * top.min(total - visible) as i32 / (total - visible) as i32;
    Some(Rect::new(bar.left(), bar.top() + offset, bar.width(), thumb))
}

/// Lines of the reply the channel view can show at once.
pub fn reply_face(ui: &UiState, agent: &Agent) -> DocFont {
    if agent.reply.windowed() {
        agent.reply.font
    } else {
        DocFont::for_size(ui.deck.settings.text_size)
    }
}

/// Lines of the reply the channel view shows at once, in whichever face the
/// size setting (or the host's window) selected.
pub fn reply_visible(ui: &UiState, agent: &Agent) -> usize {
    font::lines_in_height(doc_face(reply_face(ui, agent)), reply_rect(ui, agent).height())
}

/// The last reply, wrapped; `reply_top` lines are scrolled away.
fn draw_reply(band: &mut Band, ui: &UiState, agent: &Agent) {
    let lane = reply_rect(ui, agent);
    if !kit::visible(band, reply_lane(ui, agent)) {
        return;
    }
    // One face for the whole lane: whichever the host measured its window with,
    // or the one the `text` size setting picks when there is no window.
    let face = doc_face(reply_face(ui, agent));
    let visible = reply_visible(ui, agent);
    let lh = font::line_height(face);
    let first = lane.top() + font::ascent(face);
    if !agent.reply.windowed() && agent.reply_kind == DocKind::Workflow {
        text(band, face, lane.left(), first, "Waiting for workflow…", theme::TEXT_FAINT, theme::CANVAS, lane.width(), Align::Left);
        return;
    }
    if agent.last_reply.is_empty() && !agent.reply.windowed() {
        text(band, face, lane.left(), first, "No reply yet", theme::TEXT_FAINT, theme::CANVAS, lane.width(), Align::Left);
        return;
    }
    let top = reply_clamp(ui, agent, ui.agents.reply_top) as usize;
    if agent.reply.windowed() {
        // The host laid this out; re-wrapping it here would be a second
        // implementation that can disagree with the one that measured it.
        for slot in 0..visible {
            let Some(line) = agent.reply.line(top + slot) else {
                continue;
            };
            if agent.reply.font.rich() {
                crate::ui::rich::draw(band, face, lane.left(), first + slot as i32 * lh, lane.width(), line);
                if crate::ui::rich::tool(line).is_some_and(|(index, _)| ui.agents.pressed == Some(16 + index)) {
                    kit::outline(band, Rect::new(lane.left(), lane.top() + slot as i32 * lh, lane.width(), lh), 2, theme::TEXT_SECONDARY, 1);
                }
            } else {
                text(band, face, lane.left(), first + slot as i32 * lh, line, theme::TEXT_ROW, theme::CANVAS, lane.width(), Align::Left);
            }
        }
    } else {
        font::wrap(face, agent.last_reply.as_str(), lane.width(), top + visible, &mut |index, line, _| {
            if index >= top {
                let baseline = first + (index - top) as i32 * lh;
                text(band, face, lane.left(), baseline, line, theme::TEXT_ROW, theme::CANVAS, lane.width(), Align::Left);
            }
        });
    }
    if let Some(thumb) = reply_thumb(ui, agent, top) {
        kit::fill(band, reply_bar(ui, agent), theme::THUMB_BAR);
        kit::fill(band, thumb, theme::TEXT_FAINT);
    }
}

/// Which buttons a card block shows: the two choices of a binary question,
/// or one wide button.
fn two_buttons(card: &Card) -> bool {
    card.kind == CardKind::Question && card.choice_count == 2
}

fn button_label(card: &Card, out: &mut Text<32>) {
    out.clear();
    match card.kind {
        CardKind::Ready => out.push_str("Ready summary"),
        CardKind::Question if card.pending_answer.is_some() => out.push_str("Answer sent"),
        CardKind::Question => {
            out.push_str("Answer");
            if card.choice_count > 0 {
                out.push_str(" \u{b7} ");
                push_count(out, card.choice_count.min(4));
                out.push_str(if card.choice_count == 1 { " choice" } else { " choices" });
            }
        }
    }
}

fn draw_card_block(band: &mut Band, ui: &UiState, card: &Card) {
    let lane = layout::channel_card_text(card.open());
    if kit::visible(band, lane) {
        text(band, Font::Body, lane.left(), lane.top() + 20, card.text.as_str(), theme::TEXT_SECONDARY, theme::CANVAS, lane.width(), Align::Left);
    }
    // An acknowledged Ready card keeps its summary but has nothing to open.
    if !card.open() || !kit::visible(band, BUTTON_LANE) {
        return;
    }
    let baseline = centred(Font::Row, BUTTON_LANE.center().1);
    if two_buttons(card) {
        for index in 0..2u8 {
            let rect = layout::channel_choice_button(index);
            let bg = surface(band, rect, false, ui.agents.pressed == Some(index), true);
            let label = card.choices.get(index as usize).map_or("", |choice| choice.as_str());
            text(band, Font::Row, rect.center().0, baseline, label, theme::TEXT_PRIMARY, bg, rect.width() - 2 * ROW_PAD, Align::Center);
        }
    } else {
        let rect = layout::CHANNEL_BUTTON;
        let bg = surface(band, rect, false, ui.agents.pressed == Some(0), true);
        let mut label = Text::<32>::new();
        button_label(card, &mut label);
        text(band, Font::Row, rect.center().0, baseline, label.as_str(), theme::TEXT_PRIMARY, bg, rect.width() - 2 * ROW_PAD, Align::Center);
    }
}

fn channel_signature(ui: &UiState, agent: &Agent, sig: &mut SigBuilder) {
    sig.region_with(HEADER_LANE, |h| hash_header(h, ui, agent, kit::status_tag(ui)));
    sig.region_with(Rect::new(32, 66, 416, 34), |h| {
        crate::ui::session::hash(ui, h);
        h.u8(agent.reply_kind as u8).u8(ui.agents.pressed.unwrap_or(255));
    });
    sig.region_with(reply_lane(ui, agent), |h| {
        // The window's own lines, not just the capped text: a new window for
        // the same reply changes what is drawn and must repaint the lane.
        let top = reply_clamp(ui, agent, ui.agents.reply_top);
        h.u8(agent.reply_kind as u8).text(&agent.last_reply).u16(top).u16(agent.reply.total).u16(agent.reply.id).u8(reply_face(ui, agent) as u8);
        h.u8(ui.agents.pressed.filter(|i| *i >= 16).unwrap_or(u8::MAX));
        if agent.reply.windowed() {
            for slot in 0..reply_visible(ui, agent) {
                h.str(agent.reply.line(top as usize + slot).unwrap_or(""));
            }
        }
    });
    let card = ui.deck.card(agent.id);
    sig.region_with(card.map_or(Rect::EMPTY, |card| layout::channel_card_text(card.open())), |h| {
        match card {
            Some(card) => h.bool(true).text(&card.text),
            None => h.bool(false),
        };
    });
    sig.region_with(card.filter(|card| card.open()).map_or(Rect::EMPTY, |_| BUTTON_LANE), |h| {
        match card.filter(|card| card.open()) {
            Some(card) => {
                let mut label = Text::<32>::new();
                button_label(card, &mut label);
                h.bool(true)
                    .bool(two_buttons(card))
                    .text(&label)
                    .text(&card.choices[0])
                    .text(&card.choices[1])
                    .u8(ui.agents.pressed.unwrap_or(255))
            }
            None => h.bool(false),
        };
    });
    sig.region(HINT_LANE, hint_variant(reading_hints(ui, agent, card)));
}

// ---- cards ----------------------------------------------------------------------

/// The font a question body is drawn in: the one the host measured its window
/// with, or — for a card that arrived without one — two Title lines, dropping
/// to three Name lines when they overflow.
fn question_font(card: &Card) -> Font {
    if card.body.windowed() {
        return doc_face(card.body.font);
    }
    let width = layout::CARD_QUESTION.width();
    let mut truncated = false;
    font::wrap(Font::Title, card.text.as_str(), width, 2, &mut |_, _, cut| truncated |= cut);
    if truncated {
        Font::Name
    } else {
        Font::Title
    }
}

/// Lines of the question body the card view shows at once.
pub fn question_visible(card: &Card) -> usize {
    if card.body.windowed() {
        return card.body.font.question_lines();
    }
    if question_font(card) == Font::Title {
        2
    } else {
        3
    }
}

/// The first line the body can be scrolled to. The deck never scrolls out of
/// the window it holds, so a window the host has not answered for yet stops
/// the scroll instead of blanking the lane: a deck whose host is away stays
/// readable (§6.3).
pub fn question_first(card: &Card) -> usize {
    if card.body.windowed() {
        card.body.first as usize
    } else {
        0
    }
}

/// One past the last line the body can be scrolled to; with
/// `question_visible` this gives the hidden lines the way `reply_lines` does
/// for the channel's reply. A card the host sent no window for does not
/// scroll at all: it draws in the lines the lane has, as it always has.
pub fn question_lines(card: &Card) -> usize {
    if card.body.windowed() {
        card.body.first as usize + card.body.held as usize
    } else {
        question_visible(card)
    }
}

/// `top` brought inside the window `card` holds. The grammar clamps as it
/// scrolls, but a window landing between a scroll and the next frame must
/// never leave a row with no line behind it.
pub fn question_clamp(card: &Card, top: u16) -> u16 {
    let floor = question_first(card);
    let ceiling = question_lines(card).saturating_sub(question_visible(card)).max(floor);
    (top as usize).clamp(floor, ceiling) as u16
}

/// The question body: the host's wrapped lines from the scroll offset, or the
/// card's own text wrapped here when no window has arrived. A body the host
/// measured is never re-wrapped — two wraps can disagree.
fn draw_question_text(band: &mut Band, ui: &UiState, card: &Card) {
    if !kit::visible(band, QUESTION_LANE) {
        return;
    }
    let font = question_font(card);
    let visible = question_visible(card);
    let lane = layout::CARD_QUESTION;
    let lh = font::line_height(font);
    let first = lane.top() + font::ascent(font);
    let top = question_clamp(card, ui.agents.question_top) as usize;
    if card.body.windowed() {
        for slot in 0..visible {
            let Some(line) = card.body.line(top + slot) else {
                continue;
            };
            text(band, font, lane.left(), first + slot as i32 * lh, line, theme::TEXT_PRIMARY, theme::CANVAS, lane.width(), Align::Left);
        }
    } else {
        font::wrap(font, card.text.as_str(), lane.width(), visible, &mut |index, line, _| {
            text(band, font, lane.left(), first + index as i32 * lh, line, theme::TEXT_PRIMARY, theme::CANVAS, lane.width(), Align::Left);
        });
    }
    if let Some(thumb) = question_thumb(card, top) {
        kit::fill(band, QUESTION_BAR, theme::THUMB_BAR);
        kit::fill(band, thumb, theme::TEXT_FAINT);
    }
}

/// Where the scroll bar's thumb sits, or `None` when the whole body fits.
/// The thumb is the visible share of `total` — the lines the host says the
/// text has, not the ones the deck happens to hold — so the bar tells the
/// truth about the text from the first window on.
fn question_thumb(card: &Card, top: usize) -> Option<Rect> {
    let visible = question_visible(card);
    let total = card.body.total as usize;
    if !card.body.windowed() || total <= visible {
        return None;
    }
    let height = QUESTION_BAR.height();
    let thumb = (height * visible as i32 / total as i32).max(QUESTION_THUMB_MIN);
    let offset = (height - thumb) * top.min(total - visible) as i32 / (total - visible) as i32;
    Some(Rect::new(QUESTION_BAR.left(), QUESTION_BAR.top() + offset, QUESTION_BAR.width(), thumb))
}

/// What a choice row says at its right edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChoiceTag {
    None,
    Recommended,
    Sent,
    Offline,
}

/// How choice `index` of `card` is drawn: (selected, pressed, dimmed, tag).
fn choice_look(ui: &UiState, card: &Card, index: u8) -> (bool, bool, bool, ChoiceTag) {
    let sent = card.pending_answer;
    let selected = match sent {
        Some(sent) => sent == index,
        None => ui.agents.choice == Some(index),
    };
    let pressed = sent.is_none() && ui.agents.pressed == Some(index);
    let dimmed = sent.is_some() && !selected;
    // Once an answer is out only "sent" is worth a tag.
    let tag = if sent == Some(index) {
        ChoiceTag::Sent
    } else if sent.is_some() || card.kind != CardKind::Question {
        ChoiceTag::None
    } else if selected && offline(ui) {
        ChoiceTag::Offline
    } else if card.recommended == Some(index) {
        ChoiceTag::Recommended
    } else {
        ChoiceTag::None
    };
    (selected, pressed, dimmed, tag)
}

/// A numbered choice (question) or step (ready) row.
fn draw_choice(band: &mut Band, ui: &UiState, card: &Card, rect: Rect, index: u8) {
    if !kit::visible(band, rect) {
        return;
    }
    let Some(choice) = card.choices.get(index as usize) else {
        return;
    };
    let (selected, pressed, dimmed, tag) = choice_look(ui, card, index);
    let bg = surface(band, rect, selected, pressed, true);
    let chip = layout::CHOICE_CHIP.offset(rect.left(), rect.top());
    kit::round(band, chip, theme::CHIP_R, theme::CHIP_FILL);
    let digit = [b'1' + index % 9];
    let (cx, cy) = chip.center();
    text(band, Font::Small, cx, centred(Font::Small, cy), core::str::from_utf8(&digit).unwrap_or("?"), theme::TEXT_SECONDARY, theme::CHIP_FILL, chip.width(), Align::Center);
    let baseline = rect.top() + layout::CHOICE_BASELINE;
    let tag_right = rect.left() + layout::CHOICE_TAG_RIGHT;
    let (label, color) = match tag {
        ChoiceTag::None => ("", 0),
        ChoiceTag::Recommended => ("recommended", theme::GREEN),
        ChoiceTag::Sent => ("sent", theme::GREEN),
        ChoiceTag::Offline => ("offline", theme::AMBER),
    };
    let tag_w = if label.is_empty() {
        0
    } else {
        text(band, Font::Small, tag_right, baseline, label, color, bg, 140, Align::Right) + ROW_PAD
    };
    let x = rect.left() + layout::CHOICE_TEXT_X;
    let fg = if dimmed {
        theme::TEXT_LABEL
    } else if selected {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_ROW
    };
    text(band, Font::Row, x, baseline, choice.as_str(), fg, bg, tag_right - tag_w - x, Align::Left);
}

fn question_hints(ui: &UiState, card: &Card) -> &'static [Hint<'static>] {
    if card.pending_answer.is_some() {
        &QUESTION_SENT_HINTS
    } else if offline(ui) {
        &QUESTION_OFFLINE_HINTS
    } else if card.choice_count == 0 {
        &QUESTION_VOICE_HINTS
    } else {
        &QUESTION_HINTS
    }
}

fn draw_question(band: &mut Band, ui: &UiState, agent: &Agent, card: &Card) {
    let mut tag = Text::<16>::new();
    header_with_dot(band, ui, agent, card_tag(ui, agent, card, &mut tag));
    draw_question_text(band, ui, card);
    let viewport = choices_viewport(ui, card, layout::CARD_VISIBLE);
    for (slot, index) in viewport.slots() {
        draw_choice(band, ui, card, layout::card_choice(slot), index);
    }
    draw_more(band, CARD_MORE, &viewport);
    kit::hint_bar(band, question_hints(ui, card));
}

fn ready_hints(ui: &UiState, card: &Card) -> &'static [Hint<'static>] {
    if offline(ui) {
        &READY_OFFLINE_HINTS
    } else if card.choice_count == 0 {
        &READY_PLAIN_HINTS
    } else if ui.agents.choice.is_some() {
        &READY_SEND_HINTS
    } else {
        &READY_OPEN_HINTS
    }
}

/// Stage summary: the card text (or the title) in Title, then the last reply
/// in Body on the lines left above the first step row.
fn draw_summary(band: &mut Band, agent: &Agent, card: &Card) {
    if !kit::visible(band, SUMMARY_LANE) {
        return;
    }
    let lane = layout::READY_SUMMARY;
    let summary = if card.text.is_empty() { agent.title.as_str() } else { card.text.as_str() };
    let lh = font::line_height(Font::Title);
    let first = lane.top() + font::ascent(Font::Title);
    let mut used = 0;
    font::wrap(Font::Title, summary, lane.width(), 2, &mut |index, line, _| {
        text(band, Font::Title, lane.left(), first + index as i32 * lh, line, theme::TEXT_PRIMARY, theme::CANVAS, lane.width(), Align::Left);
        used = index + 1;
    });
    if agent.last_reply.is_empty() {
        return;
    }
    let top = lane.top() + used as i32 * lh + 6;
    let body_lh = font::line_height(Font::Body);
    let lines = ((lane.bottom() - top) / body_lh).max(0) as usize;
    let first = top + font::ascent(Font::Body);
    font::wrap(Font::Body, agent.last_reply.as_str(), lane.width(), lines, &mut |index, line, _| {
        text(band, Font::Body, lane.left(), first + index as i32 * body_lh, line, theme::TEXT_SECONDARY, theme::CANVAS, lane.width(), Align::Left);
    });
}

fn draw_ready(band: &mut Band, ui: &UiState, agent: &Agent, card: &Card) {
    let mut tag = Text::<16>::new();
    header_with_dot(band, ui, agent, card_tag(ui, agent, card, &mut tag));
    draw_summary(band, agent, card);
    let viewport = choices_viewport(ui, card, layout::READY_VISIBLE);
    for (slot, index) in viewport.slots() {
        draw_choice(band, ui, card, layout::ready_row(slot), index);
    }
    draw_more(band, CARD_MORE, &viewport);
    kit::hint_bar(band, ready_hints(ui, card));
}

fn card_signature(ui: &UiState, agent: &Agent, card: &Card, sig: &mut SigBuilder) {
    sig.region_with(HEADER_LANE, |h| {
        let mut tag = Text::<16>::new();
        hash_header(h, ui, agent, card_tag(ui, agent, card, &mut tag));
    });
    let (question, visible, hints) = match card.kind {
        CardKind::Question => (true, layout::CARD_VISIBLE, question_hints(ui, card)),
        CardKind::Ready => (false, layout::READY_VISIBLE, ready_hints(ui, card)),
    };
    if question {
        sig.region_with(QUESTION_LANE, |h| {
            let top = question_clamp(card, ui.agents.question_top);
            h.u16(top).u16(card.body.first).u16(card.body.total).u8(card.body.held).u8(card.body.font as u8);
            if card.body.windowed() {
                for slot in 0..question_visible(card) {
                    h.str(card.body.line(top as usize + slot).unwrap_or(""));
                }
            } else {
                h.text(&card.text);
            }
        });
    } else {
        sig.region_with(SUMMARY_LANE, |h| {
            h.text(&card.text).text(&agent.title).text(&agent.last_reply);
        });
    }
    let viewport = choices_viewport(ui, card, visible);
    for slot in 0..visible {
        let rect = if question { layout::card_choice(slot) } else { layout::ready_row(slot) };
        sig.region_with(rect, |h| match viewport.index_at(slot).and_then(|index| card.choices.get(index as usize).map(|choice| (index, choice))) {
            None => {
                h.bool(false);
            }
            Some((index, choice)) => {
                let (selected, pressed, dimmed, tag) = choice_look(ui, card, index);
                h.bool(true).u8(index).text(choice).bool(selected).bool(pressed).bool(dimmed).u8(tag as u8);
            }
        });
    }
    sig.region_with(CARD_MORE, |h| {
        let (above, below) = more(&viewport);
        h.u8(above).u8(below);
    });
    sig.region(HINT_LANE, hint_variant(hints));
}

// ---- nothing to show ----------------------------------------------------------------

fn draw_empty(band: &mut Band, ui: &UiState, agent: Option<&Agent>, message: &str) {
    match agent {
        Some(agent) => header_with_dot(band, ui, agent, kit::status_tag(ui)),
        None => kit::header(band, "AGENTS", kit::header_right(ui, kit::status_tag(ui))),
    }
    let (cx, cy) = layout::BODY.center();
    text(band, Font::Row, cx, centred(Font::Row, cy), message, theme::TEXT_FAINT, theme::CANVAS, layout::BODY.width() - 2 * theme::MARGIN, Align::Center);
    kit::hint_bar(band, &EMPTY_HINTS);
}

fn empty_signature(ui: &UiState, agent: Option<&Agent>, sig: &mut SigBuilder) {
    sig.region_with(HEADER_LANE, |h| match agent {
        Some(agent) => hash_header(h, ui, agent, kit::status_tag(ui)),
        None => {
            h.header_right(kit::header_right(ui, kit::status_tag(ui)));
        }
    });
    sig.region(layout::BODY, agent.is_some() as u32);
    sig.region(HINT_LANE, hint_variant(&EMPTY_HINTS));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.host_present = true;
        ui
    }

    #[test]
    fn kind_and_name_join_with_a_middle_dot() {
        let ui = demo();
        let codex = ui.deck.agent(1).unwrap();
        assert_eq!(kind_name(codex).as_str(), "Codex \u{b7} cohdl-api");
        assert_eq!(row_name(codex).as_str(), "Codex \u{b7} cohdl-api");
        let mut other = Agent::new();
        other.kind = AgentKind::Other;
        other.name.set("x");
        assert_eq!(kind_name(&other).as_str(), "Agent \u{b7} x");
        // A session named after its directory: the page says the name.
        other.project.set("x");
        assert_eq!(kind_name(&other).as_str(), "Agent \u{b7} x");
        assert_eq!(row_name(&other).as_str(), "Agent");
        other.project.clear();
        other.name.clear();
        assert_eq!(row_name(&other).as_str(), "Agent \u{b7} ", "no project: never the kind alone");
        // The same for a title that is just the directory's name.
        assert_eq!(row_title(codex), "Run migrations on staging?");
        other.title.set("x");
        assert_eq!(row_title(&other), "x");
        other.project.set("x");
        assert_eq!(row_title(&other), "");
    }

    #[test]
    fn the_roster_header_names_the_page_and_counts_the_pages() {
        let mut ui = demo();
        let mut label = Text::<40>::new();
        roster_header(&ui, &mut label);
        assert_eq!(label.as_str(), "COHDL \u{b7} 1/2");
        ui.agents.project.set(crate::deck::demo::PROJECTS[1]);
        roster_header(&ui, &mut label);
        assert_eq!(label.as_str(), "OPENMICRO \u{b7} 2/2");
        // One project: no count. None: the face's name. An unlabelled page
        // has a name of its own.
        for id in [4u8, 5] {
            ui.deck.agent_mut(id).unwrap().project.set(crate::deck::demo::PROJECTS[0]);
        }
        ui.agents.project.set(crate::deck::demo::PROJECTS[0]);
        roster_header(&ui, &mut label);
        assert_eq!(label.as_str(), "COHDL");
        ui.deck.agent_mut(5).unwrap().project.clear();
        ui.agents.project.clear();
        roster_header(&ui, &mut label);
        assert_eq!(label.as_str(), "OTHER \u{b7} 2/2");
        ui.deck.clear_host_data();
        roster_header(&ui, &mut label);
        assert_eq!(label.as_str(), "AGENTS");
    }

    #[test]
    fn ages_are_shown_only_when_live_and_over_a_minute() {
        let mut ui = demo();
        let mut tag = Text::<16>::new();
        let codex = ui.deck.agent(1).unwrap();
        let card = ui.deck.card(1).unwrap();
        assert_eq!(card_tag(&ui, codex, card, &mut tag), HeaderRight::Text("blocked 2m"));
        let mut suffix = Text::<12>::new();
        age_text(&ui, codex, " \u{b7} ", &mut suffix);
        assert_eq!(suffix.as_str(), " \u{b7} 2m");
        // Under a minute: just the word.
        let claude = ui.deck.agent(2).unwrap();
        let ready = ui.deck.card(2).unwrap();
        assert_eq!(card_tag(&ui, claude, ready, &mut tag), HeaderRight::Text("ready"));
        // Offline: the deck's tag wins and no age is shown anywhere.
        ui.deck.settings.demo = false;
        ui.deck.link_lost();
        let codex = ui.deck.agent(1).unwrap();
        let card = ui.deck.card(1).unwrap();
        assert_eq!(card_tag(&ui, codex, card, &mut tag), HeaderRight::Text("offline"));
        assert_eq!(age_step(&ui, codex), 0);
        assert!(offline(&ui) && dot_color(&ui, codex) == theme::GRAY);
    }

    #[test]
    fn button_labels_count_choices() {
        let ui = demo();
        let mut label = Text::<32>::new();
        let mut card = *ui.deck.card(1).unwrap();
        button_label(&card, &mut label);
        assert_eq!(label.as_str(), "Answer \u{b7} 3 choices");
        card.choice_count = 1;
        button_label(&card, &mut label);
        assert_eq!(label.as_str(), "Answer \u{b7} 1 choice");
        card.choice_count = 0;
        button_label(&card, &mut label);
        assert_eq!(label.as_str(), "Answer");
        card.pending_answer = Some(0);
        button_label(&card, &mut label);
        assert_eq!(label.as_str(), "Answer sent");
        assert!(!two_buttons(&card));
        card.pending_answer = None;
        card.choice_count = 2;
        assert!(two_buttons(&card));
        let ready = ui.deck.card(2).unwrap();
        button_label(ready, &mut label);
        assert_eq!(label.as_str(), "Ready summary");
        assert!(!two_buttons(ready));
    }

    /// The per-face line counts host and deck share (`DocFont`) are exactly
    /// what each lane holds: one more line would draw outside the region that
    /// hashes it, and the panel would keep stale pixels there.
    #[test]
    fn doc_font_line_counts_fill_their_lanes_exactly() {
        use crate::deck::msg::DocFont;
        let fits = |font: Font, top: i32, lines: usize, bottom: i32| {
            let face = &crate::ui::font_data::FONTS[font as usize];
            top + font::ascent(font) + (lines as i32 - 1) * font::line_height(font) + face.below as i32 <= bottom
        };
        for doc in [DocFont::Title, DocFont::Name, DocFont::Row, DocFont::Text] {
            let font = doc_face(doc);
            let reply = (layout::CHANNEL_REPLY.top(), layout::CHANNEL_REPLY.bottom());
            assert!(fits(font, reply.0, doc.reply_lines(), reply.1), "{doc:?} reply");
            assert!(!fits(font, reply.0, doc.reply_lines() + 1, reply.1), "{doc:?} reply holds one more");
            // Two clear rows above the first choice (`a_three_line_question_…`).
            let question = (layout::CARD_QUESTION.top(), QUESTION_LANE.bottom() - 2);
            assert!(fits(font, question.0, doc.question_lines(), question.1), "{doc:?} question");
            assert!(!fits(font, question.0, doc.question_lines() + 1, question.1), "{doc:?} question holds one more");
        }
    }

    #[test]
    fn replies_fill_the_available_height_for_every_font_and_card_state() {
        let mut ui = demo();
        for doc in [DocFont::Title, DocFont::Name, DocFont::Row, DocFont::Text] {
            let font = doc_face(doc);
            let metrics = &crate::ui::font_data::FONTS[font as usize];
            for card_open in [None, Some(false), Some(true)] {
                let agent = &mut ui.deck.agents[0];
                agent.has_card = card_open.is_some();
                agent.card.kind = CardKind::Ready;
                agent.card.acknowledged = card_open == Some(false);
                agent.reply.total = 40;
                agent.reply.held = 40;
                agent.reply.font = doc;
                let agent = ui.deck.agent(1).unwrap();
                let rect = reply_rect(&ui, agent);
                let visible = reply_visible(&ui, agent);
                let bottom = rect.top() + font::ascent(font) + (visible as i32 - 1) * font::line_height(font) + metrics.below as i32;
                assert!(bottom <= rect.bottom(), "{doc:?} / {card_open:?} clips a line");
                assert!(bottom + font::line_height(font) > rect.bottom(), "{doc:?} / {card_open:?} wastes a line");
                assert_eq!(reply_clamp(&ui, agent, u16::MAX) as usize, 40 - visible);
                if card_open.is_none() {
                    assert_eq!(rect.bottom(), layout::CHANNEL_CONTENT_BOTTOM);
                    assert!(visible > doc.reply_lines());
                }
            }
        }
    }

    #[test]
    fn the_question_drops_to_three_name_lines_only_when_two_title_lines_overflow() {
        let ui = demo();
        let mut card = *ui.deck.card(1).unwrap();
        card.text.set("Run the migrations on staging first?");
        assert_eq!(question_font(&card), Font::Title);
        card.text.set("Tests fail on main because the staging database is three migrations behind. Run them there first?");
        assert_eq!(question_font(&card), Font::Name);
        // Three Name lines: the last line's ink stays inside the lane, above the first choice row.
        let name = &crate::ui::font_data::FONTS[Font::Name as usize];
        let last_ink = layout::CARD_QUESTION.top() + 2 * font::line_height(Font::Name) + font::ascent(Font::Name) + name.below as i32;
        assert!(last_ink <= QUESTION_LANE.bottom(), "{last_ink}");
        assert!(layout::CARD_QUESTION.top() + font::ascent(Font::Title) - name.above as i32 >= QUESTION_LANE.top());
        assert_eq!(QUESTION_LANE.bottom(), layout::card_choice(0).top());
        assert_eq!(SUMMARY_LANE.bottom(), layout::ready_row(0).top());
    }

    #[test]
    fn a_windowed_question_shows_the_hosts_lines_and_sizes_its_bar_to_the_whole_text() {
        let ui = demo();
        let mut card = *ui.deck.card(1).unwrap();
        // No window: the lines are counted here, and nothing scrolls.
        assert!(!card.body.windowed());
        assert_eq!(question_lines(&card), question_visible(&card));
        assert_eq!(question_clamp(&card, 9), 0);
        assert_eq!(question_thumb(&card, 0), None);
        // Twelve lines of a forty-line body, from line 6, measured in Name.
        card.body.font = DocFont::Name;
        card.body.total = 40;
        card.body.first = 6;
        card.body.held = 12;
        for (index, line) in card.body.lines.iter_mut().enumerate() {
            line.set("wrapped");
            assert_eq!(line.len(), 7, "line {index}");
        }
        assert_eq!(question_font(&card), Font::Name);
        assert_eq!((question_visible(&card), question_first(&card), question_lines(&card)), (3, 6, 18));
        assert_eq!(card.body.line(5), None, "a line the window left behind");
        assert_eq!(card.body.line(17).map(str::len), Some(7));
        // The scroll never leaves the window: lines 6 to 15 can be the top.
        assert_eq!(question_clamp(&card, 0), 6);
        assert_eq!(question_clamp(&card, 15), 15);
        assert_eq!(question_clamp(&card, 999), 15);
        // The thumb is the visible share of `total` — the whole text, not
        // what the deck holds — so the bar is honest from the first window.
        let thumb = question_thumb(&card, 6).unwrap();
        assert_eq!(thumb.width(), QUESTION_BAR.width());
        assert_eq!(thumb.height(), (QUESTION_BAR.height() * 3 / 40).max(QUESTION_THUMB_MIN));
        assert!(thumb.top() > QUESTION_BAR.top(), "six lines are above it");
        assert_eq!(question_thumb(&card, 0).unwrap().top(), QUESTION_BAR.top());
        assert_eq!(question_thumb(&card, 37).unwrap().bottom(), QUESTION_BAR.bottom());
        // Wherever the offset is, every visible row has a line behind it:
        // the lane cannot go blank while a window request is outstanding.
        for top in 0..=60 {
            let top = question_clamp(&card, top) as usize;
            let rows = 0..question_visible(&card);
            assert!(rows.map(|slot| card.body.line(top + slot)).all(|l| l.is_some()), "top {top}");
        }
        // Title: two lines fit, and a body of two has nothing to scroll.
        card.body.font = DocFont::Title;
        assert_eq!((question_font(&card), question_visible(&card)), (Font::Title, 2));
        card.body.total = 2;
        assert_eq!(question_thumb(&card, 0), None);
        // A window with no lines in it is no window at all: the body falls
        // back to the card's own text instead of drawing nothing.
        card.body.total = 40;
        card.body.held = 0;
        assert!(!card.body.windowed());
        assert_eq!((question_first(&card), question_lines(&card)), (0, question_visible(&card)));
        assert_eq!(question_thumb(&card, 0), None);
    }

    #[test]
    fn choice_looks_follow_selection_sent_and_offline() {
        let mut ui = demo();
        ui.agents.choice = Some(1);
        let card = *ui.deck.card(1).unwrap();
        assert_eq!(choice_look(&ui, &card, 0), (false, false, false, ChoiceTag::Recommended));
        assert_eq!(choice_look(&ui, &card, 1), (true, false, false, ChoiceTag::None));
        let mut sent = card;
        sent.pending_answer = Some(2);
        assert_eq!(choice_look(&ui, &sent, 2), (true, false, false, ChoiceTag::Sent));
        assert_eq!(choice_look(&ui, &sent, 1), (false, false, true, ChoiceTag::None));
        assert_eq!(choice_look(&ui, &sent, 0), (false, false, true, ChoiceTag::None), "recommended gives way to sent");
        ui.agents.pressed = Some(1);
        assert!(choice_look(&ui, &card, 1).1);
        assert!(!choice_look(&ui, &sent, 1).1, "a sent card ignores touches");
        ui.deck.settings.demo = false;
        ui.deck.host_present = false;
        assert_eq!(choice_look(&ui, &card, 1).3, ChoiceTag::Offline);
        assert_eq!(question_hints(&ui, &card), &QUESTION_OFFLINE_HINTS);
        assert_eq!(question_hints(&ui, &sent), &QUESTION_SENT_HINTS);
    }

    #[test]
    fn hint_sets_follow_the_card_state() {
        let mut ui = demo();
        let question = *ui.deck.card(1).unwrap();
        let ready = *ui.deck.card(2).unwrap();
        assert_eq!(question_hints(&ui, &question), &QUESTION_HINTS);
        let mut voice = question;
        voice.choice_count = 0;
        assert_eq!(question_hints(&ui, &voice), &QUESTION_VOICE_HINTS);
        assert_eq!(ready_hints(&ui, &ready), &READY_OPEN_HINTS);
        ui.agents.choice = Some(0);
        assert_eq!(ready_hints(&ui, &ready), &READY_SEND_HINTS);
        let mut plain = ready;
        plain.choice_count = 0;
        assert_eq!(ready_hints(&ui, &plain), &READY_PLAIN_HINTS);
        assert_eq!(channel_hints(Some(&question)), &CHANNEL_CARD_HINTS);
        assert_eq!(channel_hints(None), &CHANNEL_HINTS);
        let mut seen = ready;
        seen.acknowledged = true;
        assert_eq!(channel_hints(Some(&seen)), &CHANNEL_HINTS);
        ui.deck.settings.demo = false;
        ui.deck.host_present = false;
        assert_eq!(ready_hints(&ui, &ready), &READY_OFFLINE_HINTS);
    }

    #[test]
    fn the_more_line_counts_rows_beyond_the_viewport() {
        assert_eq!(more(&Viewport::new(0, 4, 3)), (0, 0));
        assert_eq!(more(&Viewport::new(0, 4, 6)), (0, 2));
        assert_eq!(more(&Viewport::new(2, 4, 6)), (2, 0));
        assert_eq!(more(&Viewport::new(1, 4, 6)), (1, 1));
        assert_eq!(more(&Viewport::new(9, 4, 0)), (0, 0));
    }

    #[test]
    fn lanes_cover_what_they_name_and_never_cross() {
        assert_eq!(roster_lane(0), Rect::new(24, 70, 424, 74));
        assert!(roster_lane(0).intersection(&layout::roster_active_bar(0)) == layout::roster_active_bar(0));
        for slot in 1..layout::ROSTER_VISIBLE {
            assert!(!roster_lane(slot).intersects(&roster_lane(slot - 1)));
        }
        assert!(!roster_lane(3).intersects(&ROSTER_MORE) && !ROSTER_MORE.intersects(&HINT_LANE));
        assert!(!HEADER_LANE.intersects(&roster_lane(0)) && !HEADER_LANE.intersects(&QUESTION_LANE));
        // The question's scroll bar is inside its lane, clear of the text.
        assert_eq!(QUESTION_LANE.intersection(&QUESTION_BAR), QUESTION_BAR);
        assert!(QUESTION_BAR.left() >= layout::CARD_QUESTION.right());
        assert!(layout::in_safe_area(&QUESTION_BAR));
        assert!(!layout::card_choice(3).intersects(&CARD_MORE) && !CARD_MORE.intersects(&HINT_LANE));
        assert!(!layout::CHANNEL_CARD_TEXT.intersects(&BUTTON_LANE) && !BUTTON_LANE.intersects(&HINT_LANE));
        assert_eq!(HINT_LANE.top(), theme::HINT_Y - 1);
    }

    /// A scroll bar drawn outside the region its signature hashes is a bar that
    /// never repaints: damage tracking never marks those columns dirty, so the
    /// thumb stands still while the text scrolls under it. That was a real
    /// defect in the reply lane — `CHANNEL_REPLY` ends at x 442 and the bar is
    /// at 444.
    #[test]
    fn every_scroll_bar_is_inside_the_lane_its_signature_hashes() {
        assert_eq!(QUESTION_LANE.intersection(&QUESTION_BAR), QUESTION_BAR);
        assert_eq!(Rect::SCREEN.intersection(&QUESTION_LANE), QUESTION_LANE);
        let mut ui = demo();
        for card_open in [None, Some(false), Some(true)] {
            ui.deck.agents[0].has_card = card_open.is_some();
            ui.deck.agents[0].card.kind = CardKind::Ready;
            ui.deck.agents[0].card.acknowledged = card_open == Some(false);
            let agent = ui.deck.agent(1).unwrap();
            let lane = reply_lane(&ui, agent);
            let bar = reply_bar(&ui, agent);
            assert_eq!(lane.intersection(&bar), bar, "{card_open:?}");
            assert_eq!(Rect::SCREEN.intersection(&lane), lane);
            assert_eq!(lane.intersection(&reply_rect(&ui, agent)), reply_rect(&ui, agent));
        }
    }
}
