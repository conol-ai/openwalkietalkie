//! Device session controls. The host owns catalogs and confirms every change.
use crate::control::{Button, Control, Edge};
use crate::deck::msg::{Event, HostMsg, SessionMenu, Template};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::signature::{Hasher, SigBuilder};
use crate::ui::state::{AgentsView, Target, TouchAct, UiState, Update};
use crate::ui::{
    font,
    font::{Align, Font},
    kit,
    layout::Rect,
    theme,
};
use core::fmt::Write;

pub const ENTRY: Rect = Rect::new(32, 66, 278, 34);
const BODY: Rect = Rect::new(24, 66, 432, 360);
pub const HINTS: [kit::Hint<'static>; 3] = [
    kit::Hint {
        key: "SCROLL",
        label: "choose",
    },
    kit::Hint {
        key: "ENTER",
        label: "select",
    },
    kit::Hint {
        key: "CANCEL",
        label: "back",
    },
];
const READ_ONLY_HINTS: [kit::Hint<'static>; 1] = [kit::Hint {
    key: "CANCEL",
    label: "back",
}];

#[derive(Clone, Copy)]
pub struct View {
    pub open: bool,
    pub field: u8,
    pub page: u8,
    pub highlight: u8,
    pub menu: SessionMenu,
    pub waiting: bool,
    asked_at: Option<u32>,
    pressed_rev: u16,
}
impl View {
    pub const fn new() -> Self {
        Self {
            open: false,
            field: 0,
            page: 0,
            highlight: 0,
            menu: SessionMenu::EMPTY,
            waiting: false,
            asked_at: None,
            pressed_rev: 0,
        }
    }
}

fn online(ui: &UiState) -> bool {
    ui.deck.host_present
}

fn request(ui: &mut UiState, at: u32, out: &mut Update) {
    if !online(ui) || ui.deck.agent(ui.agents.agent).is_none() {
        return;
    }
    let v = &mut ui.agents.settings;
    v.waiting = true;
    v.asked_at = Some(at);
    out.push_deck(Event::Session {
        agent: ui.agents.agent,
        field: v.field,
        page: v.page,
        rev: 0,
        choice: 255,
    });
    out.dirty = true;
}

pub fn open(ui: &mut UiState, at: u32, out: &mut Update) {
    let v = &mut ui.agents.settings;
    v.open = true;
    v.field = 0;
    v.page = 0;
    v.highlight = 0;
    ui.agents.pressed = None;
    request(ui, at, out);
    out.dirty = true;
}

pub fn tick(ui: &mut UiState, at: u32, out: &mut Update) {
    if ui.agents.view != AgentsView::Channel || ui.current_face().template != Template::Agents {
        return;
    }
    if !online(ui) {
        return;
    }
    let v = &mut ui.agents.settings;
    if v.waiting
        && v.asked_at
            .is_some_and(|start| at.wrapping_sub(start) >= 6000)
    {
        v.waiting = false;
        v.menu.status = 0;
        v.menu
            .note
            .set("No settings response. Update the Mac app, then reopen this menu.");
        out.dirty = true;
    }
    if v.menu.agent != ui.agents.agent && v.asked_at.is_none() {
        request(ui, at, out);
    }
}

pub fn host(ui: &mut UiState, msg: &HostMsg, out: &mut Update) {
    let v = &mut ui.agents.settings;
    match msg {
        HostMsg::Synced => {
            v.menu = SessionMenu::EMPTY;
            v.asked_at = None;
            v.waiting = false;
        }
        HostMsg::SessionMenu(menu)
            if menu.agent == ui.agents.agent
                && menu.field == v.field
                && (menu.page == v.page
                    || (v.page > menu.total.saturating_sub(1) / 4
                        && menu.page == menu.total.saturating_sub(1) / 4)) =>
        {
            if v.waiting && menu.status != 2 {
                v.highlight = menu
                    .selected
                    .filter(|i| i / 4 == menu.page)
                    .map_or(0, |i| i % 4);
            }
            v.menu = *menu;
            v.page = menu.page;
            v.waiting = false;
            v.highlight = v.highlight.min(if v.field == 0 {
                1
            } else {
                menu.count.saturating_sub(1)
            });
            out.dirty = true;
        }
        _ => {}
    }
}

fn choose(ui: &mut UiState, at: u32, out: &mut Update) {
    if !online(ui) {
        return;
    }
    let v = &mut ui.agents.settings;
    if v.waiting || v.menu.agent != ui.agents.agent || matches!(v.menu.status, 0 | 2) {
        return;
    }
    if v.field == 0 {
        v.field = v.highlight + 1;
        v.page = 0;
        v.highlight = 0;
        request(ui, at, out);
    } else if v.highlight < v.menu.count {
        out.push_deck(Event::Session {
            agent: ui.agents.agent,
            field: v.field,
            page: v.page,
            rev: v.menu.rev,
            choice: v.highlight,
        });
        v.waiting = true;
        v.asked_at = Some(at);
        out.dirty = true;
    }
}

fn move_page(ui: &mut UiState, forward: bool, at: u32, out: &mut Update) {
    let v = &mut ui.agents.settings;
    if v.field == 0 || v.waiting || v.menu.status == 2 {
        return;
    }
    let next = if forward {
        v.page
            .saturating_add(1)
            .min(v.menu.total.saturating_sub(1) / 4)
    } else {
        v.page.saturating_sub(1)
    };
    if next != v.page {
        v.page = next;
        v.highlight = 0;
        request(ui, at, out);
    }
}

pub fn control(ui: &mut UiState, control: Control, at: u32, out: &mut Update) -> bool {
    if !ui.agents.settings.open || ui.agents.view != AgentsView::Channel {
        return false;
    }
    match control {
        Control::Button(Button::Cancel, Edge::Press) => {
            let v = &mut ui.agents.settings;
            if v.field == 0 {
                v.open = false;
            } else {
                v.field = 0;
                v.page = 0;
                v.highlight = 0;
                request(ui, at, out);
            }
            out.dirty = true;
        }
        Control::Button(Button::Enter, Edge::Press) => choose(ui, at, out),
        Control::Button(Button::Left, Edge::Press) => move_page(ui, false, at, out),
        Control::Button(Button::Right, Edge::Press) => move_page(ui, true, at, out),
        Control::Wheel(wheel) => {
            let forward = crate::ui::state::selection_step(wheel) > 0;
            let v = &mut ui.agents.settings;
            if !v.waiting && v.menu.status != 2 {
                let count = if v.field == 0 { 2 } else { v.menu.count };
                if forward && v.highlight + 1 < count {
                    v.highlight += 1;
                } else if !forward && v.highlight > 0 {
                    v.highlight -= 1;
                } else {
                    move_page(ui, forward, at, out);
                }
                out.dirty = true;
            }
        }
        _ => {}
    }
    true
}

pub const fn row(field: u8, i: u8) -> Rect {
    if field == 0 {
        Rect::new(32, 110 + i as i32 * 92, 416, 80)
    } else {
        Rect::new(32, 110 + i as i32 * 62, 416, 54)
    }
}

pub fn hit(ui: &UiState, x: i32, y: i32) -> Option<Target> {
    let v = &ui.agents.settings;
    let count = if v.field == 0 { 2 } else { v.menu.count };
    (0..count)
        .find(|i| row(v.field, *i).contains(x, y))
        .map(|i| Target::tap(row(v.field, i), i as u16))
}

pub fn touch(ui: &mut UiState, target: Target, act: TouchAct, at: u32, out: &mut Update) {
    match act {
        TouchAct::Down => {
            ui.agents.settings.pressed_rev = ui.agents.settings.menu.rev;
            ui.agents.settings.highlight = target.id as u8;
        }
        TouchAct::Activate => {
            if ui.agents.settings.menu.rev == ui.agents.settings.pressed_rev
                && hit(ui, target.rect.center().0, target.rect.center().1)
                    .is_some_and(|t| t.id == target.id)
            {
                choose(ui, at, out);
            }
        }
        _ => {}
    }
    out.dirty = true;
}

pub fn label(ui: &UiState) -> Text<80> {
    let mut text = Text::new();
    let menu = &ui.agents.settings.menu;
    if menu.agent == ui.agents.agent && !menu.model.is_empty() {
        text.push_str(menu.model.as_str());
        if !menu.effort.is_empty() {
            text.push_str(" · ");
            text.push_str(menu.effort.as_str());
        }
    } else {
        text.push_str(ui.tr("Model · Thinking effort"));
    }
    text
}

pub fn hash(ui: &UiState, h: &mut Hasher) {
    let v = &ui.agents.settings;
    h.text(&label(ui))
        .bool(v.waiting)
        .u8(v.menu.status)
        .bool(online(ui));
}

fn text(band: &mut Band, face: Font, x: i32, y: i32, value: &str, color: u32, width: i32) {
    font::draw(
        band,
        face,
        x,
        y,
        value,
        color,
        Some(theme::CANVAS),
        width,
        Align::Left,
    );
}

pub fn draw(band: &mut Band, ui: &UiState) {
    let v = &ui.agents.settings;
    let menu = &v.menu;
    let title = match v.field {
        1 => "Choose model",
        2 => "Thinking effort",
        _ => "Session settings",
    };
    kit::header(band, ui.tr(title), kit::header_right(ui, kit::HeaderRight::None));
    if let Some(agent) = ui.deck.agent(ui.agents.agent) {
        text(
            band,
            Font::Hint,
            38,
            87,
            crate::ui::faces::agents::kind_name_localized(agent, ui.language).as_str(),
            theme::TEXT_SECONDARY,
            404,
        );
    }
    if v.field == 0 {
        for (i, (heading, value)) in [
            ("Model", menu.model.as_str()),
            ("Thinking effort", menu.effort.as_str()),
        ]
        .iter()
        .enumerate()
        {
            let rect = row(0, i as u8);
            kit::outline(
                band,
                rect,
                theme::CARD_R,
                if v.highlight == i as u8 && menu.status != 0 && online(ui) {
                    theme::TEXT_PRIMARY
                } else {
                    theme::OUTLINE
                },
                1,
            );
            text(
                band,
                Font::Hint,
                46,
                rect.top() + 25,
                ui.tr(heading),
                theme::TEXT_SECONDARY,
                388,
            );
            text(
                band,
                Font::Body,
                46,
                rect.top() + 58,
                if value.is_empty() {
                    ui.tr("Not reported")
                } else {
                    value
                },
                theme::TEXT_PRIMARY,
                388,
            );
        }
    } else {
        for i in 0..menu.count {
            let rect = row(v.field, i);
            kit::outline(
                band,
                rect,
                theme::CARD_R,
                if v.highlight == i {
                    theme::TEXT_PRIMARY
                } else {
                    theme::OUTLINE
                },
                1,
            );
            let selected = menu.selected == Some(v.page * 4 + i);
            if selected {
                kit::dot(band, 429, rect.center().1, 3, theme::TEXT_PRIMARY);
            }
            text(
                band,
                Font::Body,
                46,
                rect.top() + 35,
                menu.choices[i as usize].as_str(),
                theme::TEXT_PRIMARY,
                369,
            );
        }
        if menu.total > 0 {
            let mut page = Text::<40>::new();
            let _ = write!(
                page,
                "{} {} / {}   ‹  ›",
                ui.tr("Page"),
                u32::from(v.page) + 1,
                u32::from(menu.total.saturating_sub(1) / 4) + 1
            );
            text(
                band,
                Font::Hint,
                38,
                378,
                page.as_str(),
                theme::TEXT_SECONDARY,
                404,
            );
        }
    }
    let note = if !online(ui) {
        ui.tr("Reconnect the Mac app to change settings.")
    } else if v.waiting {
        ui.tr("Loading session settings...")
    } else if menu.note == "No settings response. Update the Mac app, then reopen this menu." {
        ui.tr("No settings response. Update the Mac app, then reopen this menu.")
    } else {
        menu.note.as_str()
    };
    let empty = v.field != 0 && menu.total == 0;
    let y = if empty {
        145
    } else if v.field == 0 {
        317
    } else {
        397
    };
    font::wrap(
        Font::Hint,
        note,
        404,
        if v.field == 0 || empty { 5 } else { 2 },
        &mut |i, line, _| {
            text(
                band,
                Font::Hint,
                38,
                y + i as i32 * 20,
                line,
                theme::TEXT_SECONDARY,
                404,
            );
        },
    );
    kit::hint_bar_localized(
        band,
        if matches!(menu.status, 0 | 2) || v.waiting || !online(ui) {
            &READ_ONLY_HINTS
        } else {
            &HINTS
        },
        ui.language,
    );
}

pub fn signature(ui: &UiState, sig: &mut SigBuilder) {
    let v = &ui.agents.settings;
    sig.mode(40 + u32::from(v.field));
    sig.region_with(crate::ui::faces::agents::HEADER_LANE, |h| {
        h.u8(v.field)
            .header_right(kit::header_right(ui, kit::HeaderRight::None));
    });
    sig.region_with(BODY, |h| {
        hash(ui, h);
        h.u8(v.highlight)
            .u8(v.page)
            .text(&v.menu.model)
            .text(&v.menu.effort)
            .text(&v.menu.note)
            .u8(v.menu.count)
            .u8(v.menu.total)
            .u8(v.menu.selected.unwrap_or(255));
        if let Some(agent) = ui.deck.agent(ui.agents.agent) {
            h.text(&crate::ui::faces::agents::kind_name(agent));
        }
        for choice in &v.menu.choices {
            h.text(choice);
        }
    });
    sig.region(
        crate::ui::faces::agents::HINT_LANE,
        u32::from(!matches!(v.menu.status, 0 | 2) && !v.waiting && online(ui)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Wheel;
    fn fixture() -> Box<UiState> {
        let mut ui = Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui.deck.host_present = true;
        ui.agents.view = AgentsView::Channel;
        ui.agents.agent = 2;
        ui
    }
    fn ready(ui: &mut UiState, field: u8, rev: u16) {
        let mut menu = SessionMenu::EMPTY;
        menu.agent = 2;
        menu.field = field;
        menu.rev = rev;
        menu.status = 1;
        menu.model.set("Model A");
        menu.effort.set("high");
        if field != 0 {
            menu.count = 2;
            menu.total = 2;
            menu.choices[0].set("low");
            menu.choices[1].set("high");
        }
        host(ui, &HostMsg::SessionMenu(menu), &mut Update::new());
    }

    #[test]
    fn wheel_selects_a_supported_effort_and_stale_touches_cannot_apply_it() {
        let mut ui = fixture();
        let mut out = Update::new();
        open(&mut ui, 1, &mut out);
        assert!(matches!(
            out.deck_events().next(),
            Some(Event::Session {
                field: 0,
                choice: 255,
                ..
            })
        ));
        ready(&mut ui, 0, 1);
        control(&mut ui, Control::Wheel(Wheel::Ccw), 2, &mut Update::new());
        let mut out = Update::new();
        control(
            &mut ui,
            Control::Button(Button::Enter, Edge::Press),
            3,
            &mut out,
        );
        assert!(matches!(
            out.deck_events().next(),
            Some(Event::Session {
                field: 2,
                choice: 255,
                ..
            })
        ));
        ready(&mut ui, 2, 2);
        let target = Target::tap(row(2, 1), 1);
        touch(&mut ui, target, TouchAct::Down, 4, &mut Update::new());
        ready(&mut ui, 2, 3);
        let mut out = Update::new();
        touch(&mut ui, target, TouchAct::Activate, 5, &mut out);
        assert_eq!(out.deck_events().count(), 0);
        touch(&mut ui, target, TouchAct::Down, 6, &mut Update::new());
        touch(&mut ui, target, TouchAct::Activate, 7, &mut out);
        assert!(matches!(
            out.deck_events().next(),
            Some(Event::Session {
                field: 2,
                rev: 3,
                choice: 1,
                ..
            })
        ));
        assert!(ui.agents.settings.waiting);
        assert_eq!(ui.agents.settings.menu.effort.as_str(), "high");
    }

    #[test]
    fn offline_read_only_pending_and_timed_out_controls_do_not_send_changes() {
        let mut ui = fixture();
        open(&mut ui, 1, &mut Update::new());
        ready(&mut ui, 0, 1);
        for (online, status) in [(false, 1), (true, 0), (true, 2)] {
            ui.deck.host_present = online;
            ui.agents.settings.menu.status = status;
            let mut out = Update::new();
            choose(&mut ui, 2, &mut out);
            assert_eq!(out.deck_events().count(), 0);
        }
        ui.deck.host_present = true;
        request(&mut ui, 10, &mut Update::new());
        tick(&mut ui, 6010, &mut Update::new());
        assert!(!ui.agents.settings.waiting);
        assert!(ui
            .agents
            .settings
            .menu
            .note
            .as_str()
            .contains("No settings response"));
        host(&mut ui, &HostMsg::Synced, &mut Update::new());
        let mut out = Update::new();
        tick(&mut ui, 6020, &mut out);
        assert!(matches!(
            out.deck_events().next(),
            Some(Event::Session { choice: 255, .. })
        ));
    }
}
