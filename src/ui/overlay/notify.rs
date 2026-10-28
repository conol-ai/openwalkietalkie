//! Top-edge indicator, toast and the notice drawer.
//!
//! The toast slides down inside `layout::TOAST_LANE` over `SLIDE_MS` from
//! `Toast::shown_ms` (ease-out, whole pixels) and rests at `layout::TOAST`. A
//! card toast (`ToastKind::Card`) shows the state dot, "Kind · name" and the
//! card text; a message toast shows an amber dot, `ToastKind::message()` and
//! the agent underneath (nothing for "No channel", which has no agent).
//!
//! The drawer lists `drawer_rows` (the filed notices in slot order) through a
//! `DRAWER_VISIBLE`-row viewport starting at `Notify::drawer_top`; the row
//! whose (agent, card) equals `Notify::drawer_sel` is the selected one.
//!
//! Signature: two fixed lanes (indicator, toast) for the overlay, and header,
//! five rows, empty-state and hint regions for the drawer.
use crate::deck::model::{Deck, Notice};
use crate::deck::msg::{AgentKind, CardKind};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::{Toast, ToastKind, UiState};
use crate::ui::{font, font::Align, font::Font, kit, layout, layout::Rect, theme};

/// The toast's slide-down, in ms.
pub const SLIDE_MS: u32 = 220;
/// Travel of the slide: from fully hidden above the panel to `layout::TOAST`.
const TRAVEL: i32 = layout::TOAST.bottom();

pub const fn kind_label(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
        AgentKind::Gemini => "Gemini",
        AgentKind::Other => "Agent",
        AgentKind::Conol => "Conol",
    }
}

const fn card_label(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Question => "Question",
        CardKind::Ready => "Ready",
    }
}

const fn card_color(kind: CardKind) -> u32 {
    match kind {
        CardKind::Question => theme::RED,
        CardKind::Ready => theme::GREEN,
    }
}

/// "Kind · name" of an agent; empty when it is not on the roster.
pub fn agent_line(ui: &UiState, agent: u8, out: &mut Text<40>) {
    out.clear();
    if let Some(agent) = ui.deck.agent(agent) {
        out.push_str(kind_label(agent.kind));
        out.push_str(" \u{b7} ");
        out.push_str(agent.name.as_str());
    }
}

/// What a notice's card says now, or the kind when that card is gone.
fn card_text(ui: &UiState, agent: u8, card: u16, kind: CardKind) -> &str {
    match ui.deck.card(agent) {
        Some(pending) if pending.id == card && !pending.text.is_empty() => pending.text.as_str(),
        _ => card_label(kind),
    }
}

/// Pixels the toast has travelled: 0 = hidden above the panel, `TRAVEL` = at
/// rest. Ease-out over `SLIDE_MS`; a `shown_ms` still ahead of `now_ms`
/// (a control stamped later than the last tick) has not started.
pub fn slide(toast: &Toast, now_ms: u32) -> i32 {
    let elapsed = now_ms.wrapping_sub(toast.shown_ms);
    if elapsed >= u32::MAX / 2 {
        return 0;
    }
    let t = (elapsed.min(SLIDE_MS) * 256 / SLIDE_MS) as i32;
    let remaining = 256 - t;
    TRAVEL - TRAVEL * remaining * remaining / 65_536
}

/// Where the toast is drawn right now.
pub fn toast_rect(toast: &Toast, now_ms: u32) -> Rect {
    layout::TOAST.offset(0, slide(toast, now_ms) - TRAVEL)
}

fn indicator_color(deck: &Deck) -> Option<u32> {
    (deck.notice_count() != 0).then(|| if deck.drawer_has_question() { theme::RED } else { theme::GREEN })
}

/// Indicator and toast; called for every frame, draws only what is showing.
fn visible_indicator(ui: &UiState) -> Option<u32> {
    if crate::ui::state::arcade::fullscreen(ui) { None } else { indicator_color(&ui.deck) }
}

pub fn draw(band: &mut Band, ui: &UiState) {
    if let Some(color) = visible_indicator(ui) {
        kit::round(band, layout::INDICATOR, 2, color);
    }
    let Some(toast) = ui.notify.toast else {
        return;
    };
    let rect = toast_rect(&toast, ui.now_ms);
    if !kit::visible(band, rect) {
        return;
    }
    // The card floats over a blank header row: no half letters beside it.
    let lane = layout::TOAST_LANE;
    let top = rect.top().max(0);
    kit::fill(band, Rect::new(lane.left(), top, lane.width(), rect.bottom() - top), theme::CANVAS);
    let fill = if ui.notify.toast_pressed { theme::CHIP_FILL } else { theme::CARD_FILL };
    kit::round(band, rect, theme::CARD_R, fill);
    kit::outline(band, rect, theme::CARD_R, theme::OUTLINE_KEY, 1);
    let (dx, dy, r) = layout::TOAST_DOT;
    let color = match toast.kind {
        ToastKind::Card(kind) => card_color(kind),
        ToastKind::NoChannel | ToastKind::NotDelivered | ToastKind::TranscriptionFailed | ToastKind::TranscriptionUnavailable => theme::AMBER,
    };
    kit::dot(band, rect.left() + dx, rect.top() + dy, r, color);
    let mut line = Text::<40>::new();
    agent_line(ui, toast.agent, &mut line);
    let (name, text): (&str, &str) = match toast.kind {
        ToastKind::Card(kind) => (line.as_str(), card_text(ui, toast.agent, toast.card, kind)),
        ToastKind::NoChannel => ("No channel", "pick an agent on the roster first"),
        ToastKind::NotDelivered => ("Not delivered", line.as_str()),
        ToastKind::TranscriptionFailed => ("Transcription failed", "Nothing was sent. Try again."),
        ToastKind::TranscriptionUnavailable => ("Transcription unavailable", "Check the host and try again."),
    };
    let x = rect.left() + layout::TOAST_TEXT_X;
    let max_w = rect.right() - 12 - x;
    font::draw(band, Font::Body, x, rect.top() + layout::TOAST_NAME_BASELINE, name, theme::TEXT_PRIMARY, Some(fill), max_w, Align::Left);
    font::draw(band, Font::Small, x, rect.top() + layout::TOAST_TEXT_BASELINE, text, theme::TEXT_SECONDARY, Some(fill), max_w, Align::Left);
}

/// The notices the drawer lists, in row order.
pub fn drawer_rows(deck: &Deck) -> impl Iterator<Item = &Notice> + '_ {
    deck.notices.iter().filter(|n| n.used && n.in_drawer)
}

fn drawer_viewport(ui: &UiState) -> kit::list::Viewport {
    kit::list::Viewport::new(ui.notify.drawer_top, layout::DRAWER_VISIBLE, ui.deck.notice_count())
}

const DRAWER_DOT: (i32, i32, i32) = (21, 27, 5);
const DRAWER_NAME_BASELINE: i32 = 23;
const DRAWER_TEXT_BASELINE: i32 = 43;
/// The empty state, centred in the body.
const DRAWER_EMPTY: Rect = Rect::new(60, 200, 360, 80);

const DRAWER_HINTS: [kit::Hint<'static>; 4] = [
    kit::Hint { key: "SCROLL", label: "select" },
    kit::Hint { key: "ENTER", label: "open" },
    kit::Hint { key: "\u{2039} \u{203a}", label: "dismiss" },
    kit::Hint { key: "CANCEL", label: "close" },
];

/// The drawer, when `ui.global.overlay == Overlay::Drawer`. Opaque: fills
/// the band first.
pub fn draw_drawer(band: &mut Band, ui: &UiState) {
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, "NOTICES", kit::header_right(ui, kit::status_tag(ui)));
    let viewport = drawer_viewport(ui);
    let top = viewport.top as usize;
    for (slot, notice) in drawer_rows(&ui.deck).skip(top).take(layout::DRAWER_VISIBLE as usize).enumerate() {
        let rect = layout::drawer_row(slot as u8);
        if !kit::visible(band, rect) {
            continue;
        }
        let selected = (notice.agent, notice.card) == ui.notify.drawer_sel;
        kit::card(band, rect, selected, false);
        let bg = Some(if selected { theme::CARD_FILL } else { theme::CANVAS });
        let (dx, dy, r) = DRAWER_DOT;
        kit::dot(band, rect.left() + dx, rect.top() + dy, r, card_color(notice.kind));
        let mut line = Text::<40>::new();
        agent_line(ui, notice.agent, &mut line);
        let name = if line.is_empty() { card_label(notice.kind) } else { line.as_str() };
        let x = rect.left() + layout::TOAST_TEXT_X;
        let max_w = rect.right() - 12 - x;
        font::draw(band, Font::Body, x, rect.top() + DRAWER_NAME_BASELINE, name, theme::TEXT_PRIMARY, bg, max_w, Align::Left);
        let text = card_text(ui, notice.agent, notice.card, notice.kind);
        font::draw(band, Font::Small, x, rect.top() + DRAWER_TEXT_BASELINE, text, theme::TEXT_SECONDARY, bg, max_w, Align::Left);
    }
    if viewport.count == 0 && kit::visible(band, DRAWER_EMPTY) {
        let (cx, _) = DRAWER_EMPTY.center();
        font::draw(band, Font::Text, cx, DRAWER_EMPTY.top() + 30, "No notices", theme::TEXT_ROW, Some(theme::CANVAS), DRAWER_EMPTY.width(), Align::Center);
        font::draw(band, Font::Small, cx, DRAWER_EMPTY.top() + 62, "missed questions and ready cards collect here", theme::TEXT_LABEL, Some(theme::CANVAS), DRAWER_EMPTY.width(), Align::Center);
    }
    kit::hint_bar(band, &DRAWER_HINTS);
}

/// What `kit::header_right(ui, kit::status_tag(ui))` and its label read.
pub fn header_inputs(ui: &UiState, h: &mut Hasher) {
    h.opt_u32(ui.battery.percentage().map(u32::from))
        .bool(ui.deck.demo_active())
        .bool(ui.deck.host_present)
        .bool(ui.deck.clock_valid)
        .u16(ui.deck.clock_minute);
}

/// Always registers the same two lanes, so the list keeps its shape.
pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    sig.region_with(layout::INDICATOR, |h| {
        h.opt_u32(visible_indicator(ui));
    });
    sig.region_with(layout::TOAST_LANE, |h| {
        let Some(toast) = ui.notify.toast else {
            h.bool(false);
            return;
        };
        let kind = match toast.kind {
            ToastKind::Card(CardKind::Question) => 0,
            ToastKind::Card(CardKind::Ready) => 1,
            ToastKind::NoChannel => 2,
            ToastKind::NotDelivered => 3,
            ToastKind::TranscriptionFailed => 4,
            ToastKind::TranscriptionUnavailable => 5,
        };
        h.bool(true)
            .i32(slide(&toast, ui.now_ms))
            .bool(ui.notify.toast_pressed)
            .u8(kind)
            .u8(toast.agent)
            .u16(toast.card);
        let mut line = Text::<40>::new();
        agent_line(ui, toast.agent, &mut line);
        h.text(&line);
        if let ToastKind::Card(kind) = toast.kind {
            h.str(card_text(ui, toast.agent, toast.card, kind));
        }
    });
}

pub fn signature_drawer(ui: &UiState, sig: &mut SigBuilder) {
    sig.region_with(layout::HEADER, |h| header_inputs(ui, h));
    let viewport = drawer_viewport(ui);
    let mut rows = drawer_rows(&ui.deck).skip(viewport.top as usize);
    for slot in 0..layout::DRAWER_VISIBLE {
        let notice = rows.next();
        sig.region_with(layout::drawer_row(slot), |h| {
            let Some(notice) = notice else {
                h.bool(false);
                return;
            };
            h.bool(true)
                .u8(notice.agent)
                .u16(notice.card)
                .u8(notice.kind as u8)
                .bool((notice.agent, notice.card) == ui.notify.drawer_sel);
            let mut line = Text::<40>::new();
            agent_line(ui, notice.agent, &mut line);
            h.text(&line).str(card_text(ui, notice.agent, notice.card, notice.kind));
        });
    }
    sig.region_with(DRAWER_EMPTY, |h| {
        h.bool(viewport.count == 0);
    });
    sig.region(layout::HINT_BAR, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slide_eases_from_hidden_to_rest_in_whole_pixels() {
        let toast = Toast::card(1, 1, CardKind::Question, 1000);
        assert_eq!(slide(&toast, 1000), 0);
        assert_eq!(toast_rect(&toast, 1000).bottom(), 0);
        let mut last = 0;
        for now in 1000..1000 + SLIDE_MS + 50 {
            let at = slide(&toast, now);
            assert!((last..=TRAVEL).contains(&at), "{now}: {at}");
            last = at;
        }
        assert_eq!(slide(&toast, 1000 + SLIDE_MS), TRAVEL);
        assert_eq!(toast_rect(&toast, 5000), layout::TOAST);
        // Ease-out: more than half the travel in the first half of the time.
        assert!(slide(&toast, 1000 + SLIDE_MS / 2) > TRAVEL / 2);
        // Not started when the stamp is ahead of the clock.
        assert_eq!(slide(&toast, 999), 0);
    }

    #[test]
    fn drawer_rows_are_the_filed_notices_in_slot_order() {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.clear_host_data();
        ui.deck.notices[3] = Notice::new(2, 9, CardKind::Ready, 0).filed();
        ui.deck.notices[1] = Notice::new(1, 4, CardKind::Question, 0).filed();
        ui.deck.notices[5] = Notice::new(3, 1, CardKind::Ready, 0);
        let rows: std::vec::Vec<_> = drawer_rows(&ui.deck).map(|n| (n.agent, n.card)).collect();
        assert_eq!(rows, [(1, 4), (2, 9)]);
        assert_eq!(indicator_color(&ui.deck), Some(theme::RED));
        ui.deck.notices[1].used = false;
        assert_eq!(indicator_color(&ui.deck), Some(theme::GREEN));
        ui.deck.notices[3].in_drawer = false;
        assert_eq!(indicator_color(&ui.deck), None);
    }
}
