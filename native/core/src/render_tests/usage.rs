//! Usage face (`ui::faces::usage`): the mockup scene is objective-clean with
//! the mockup's bar tones, scrolling and the "updating…" tag refresh
//! partially to the full redraw, blocks are separate lanes, and no origin
//! panics.
use super::*;
use crate::{
    battery::{BatteryState, ChargeState},
    control::{Button, Control, Edge, Touch, TouchPhase, Wheel},
    deck::msg::{HostMsg, Tone, UsageItem},
    ui::{
        faces,
        font::{self, Font},
        kit,
        signature::SigBuilder,
        theme, Rect, Update,
    },
};

fn pixel(frame: &[u8], x: i32, y: i32) -> u16 {
    let at = (y as usize * 480 + x as usize) * 2;
    u16::from_le_bytes([frame[at], frame[at + 1]])
}

fn lit_in(frame: &[u8], rect: Rect) -> impl Iterator<Item = (i32, i32)> + '_ {
    (rect.top()..rect.bottom())
        .flat_map(move |y| (rect.left()..rect.right()).map(move |x| (x, y)))
        .filter(move |&(x, y)| pixel(frame, x, y) != 0)
}

fn color(c: u32) -> u16 {
    kit::rgb(c).into_storage()
}

/// The mockup: demo data with a host present, so the tag reads "SEP 2026".
fn usage() -> Box<UiState> {
    let mut ui = on(Template::Usage);
    ui.deck.host_present = true;
    ui
}

fn items(count: u8, period: &str) -> HostMsg {
    let mut items = [UsageItem::EMPTY; 6];
    for (index, item) in items.iter_mut().enumerate().take(count as usize) {
        item.name.set(&format!("Provider {index}"));
        item.percent = (index as u8) * 25;
        item.reset.set(&format!("resets in {} days", 6 - index));
        item.tone = (index == 2).then_some(Tone::Red);
    }
    let mut msg = HostMsg::Usage { period: crate::deck::text::Text::new(), items, count };
    if let HostMsg::Usage { period: p, .. } = &mut msg {
        p.set(period);
    }
    msg
}

struct Session {
    previous: Box<UiState>,
    next: Box<UiState>,
    screen: Screens,
}

impl Session {
    fn new(ui: Box<UiState>) -> Self {
        let screen = Screens::new(&ui);
        Self { previous: Box::new((*ui).clone()), next: ui, screen }
    }
    fn step(&mut self, f: impl FnOnce(&mut UiState)) -> usize {
        f(&mut self.next);
        let transferred = update(&mut self.screen, &self.previous, &self.next);
        self.next.copy_into(&mut self.previous);
        transferred
    }
    fn host(&mut self, msg: HostMsg) -> usize {
        self.step(|ui| {
            let mut out = Update::new();
            ui.host(&msg, ui.now_ms, &mut out);
        })
    }
    fn control(&mut self, control: Control) -> usize {
        self.step(|ui| {
            let mut out = Update::new();
            ui.apply(control, ui.now_ms, &mut out);
        })
    }
    fn touch(&mut self, phase: TouchPhase, x: i32, y: i32) -> usize {
        let (x, y) = orientation::point(x as u16, y as u16).unwrap();
        self.control(Control::Touch(Touch { phase, x, y }))
    }
}

#[test]
fn the_mockup_scene_is_clean_fits_and_shows_the_mockup_tones() {
    let ui = usage();
    let frame = full(&ui);
    assert_eq!(lit_outside_safe_area(&frame), None);
    let bar = color(theme::HINT_FILL);
    let content: Vec<_> = lit_in(&frame, layout::HINT_BAR).filter(|&(x, y)| pixel(&frame, x, y) != bar).collect();
    assert!(content.len() > 300);
    assert!(content.iter().all(|&(x, _)| (32..448).contains(&x)));
    let reset_w = theme::TEXT_RIGHT - theme::TEXT_X;
    for item in &ui.deck.usage.items[..3] {
        let percent_w = font::measure(Font::Name, "100%");
        let name_w = theme::TEXT_RIGHT - percent_w - 12 - theme::TEXT_X;
        assert!(font::measure(Font::Name, item.name.as_str()) <= name_w, "{}", item.name.as_str());
        assert!(font::measure(Font::Body, item.reset.as_str()) <= reset_w, "{}", item.reset.as_str());
    }
    assert!(font::measure(Font::Small, "SEP 2026") <= 170);
    assert!(font::measure(Font::Small, "updating\u{2026}") <= 170);
    // Bars: 62 % green, 81 % amber, 12 % blue (mockup 5), on the chip track.
    for (slot, (percent, tone)) in [(62, theme::GREEN), (81, theme::AMBER), (12, theme::BLUE)].into_iter().enumerate() {
        let rect = layout::usage_row(slot as u8);
        let (cx, cy) = rect.center();
        assert_eq!(layout::usage_row_at(cx, cy), Some(slot as u8));
        let bar = layout::USAGE_BAR.offset(rect.left(), rect.top());
        let y = bar.center().1;
        assert_eq!(pixel(&frame, bar.left() + 8, y), color(tone), "slot {slot} bar start");
        let filled = bar.width() * percent / 100;
        assert_eq!(pixel(&frame, bar.left() + filled - 6, y), color(tone), "slot {slot} bar end");
        assert_eq!(pixel(&frame, bar.left() + filled + 6, y), color(theme::CHIP_FILL), "slot {slot} track");
        assert_eq!(pixel(&frame, bar.right() - 8, y), color(theme::CHIP_FILL));
        // Name on the left, percent on the right of the same baseline.
        let baseline = rect.top() + layout::USAGE_NAME_BASELINE;
        let line = Rect::new(32, baseline - 18, 416, 24);
        assert!(lit_in(&frame, line).any(|(x, _)| x < 120));
        assert!(lit_in(&frame, line).any(|(x, _)| x > 400));
        assert!(lit_in(&frame, line).all(|(x, _)| x < theme::TEXT_RIGHT + 1));
        assert!(lit_in(&frame, Rect::new(32, rect.top() + 50, 416, 24)).count() > 100, "reset line");
    }
    assert_eq!(lit_in(&frame, layout::usage_row(3)).count(), 0);
}

#[test]
fn the_tag_and_the_markers_follow_updating_and_scrolling() {
    let mut ui = usage();
    let plain = full(&ui);
    ui.usage.updating = true;
    ui.invalidate();
    let updating = full(&ui);
    let header = Rect::new(300, 30, 150, 30);
    assert!(lit_in(&plain, header).zip(lit_in(&updating, header)).any(|(a, b)| a != b));
    for y in 62..480 {
        for x in 0..480 {
            assert_eq!(pixel(&plain, x, y), pixel(&updating, x, y), "({x}, {y}) is not the header");
        }
    }
    let mut out = Update::new();
    ui.usage.updating = false;
    ui.host(&items(6, "OCT 2026"), 0, &mut out);
    let marker = |slot: u8| {
        let rect = layout::usage_row(slot);
        Rect::new(300, rect.top() + 50, 148, 24)
    };
    let frame = full(&ui);
    assert_eq!(lit_in(&frame, marker(0)).count(), 0);
    assert!(lit_in(&frame, marker(3)).count() > 40, "2 more \u{2193}");
    ui.usage.top_row = 2;
    ui.invalidate();
    let frame = full(&ui);
    assert!(lit_in(&frame, marker(0)).count() > 40, "\u{2191} 2 more");
    assert_eq!(lit_in(&frame, marker(3)).count(), 0);
    assert_eq!(lit_outside_safe_area(&frame), None);
    // The host's red tone wins over the derived one; slot 0 shows item 2.
    let bar = layout::USAGE_BAR.offset(layout::usage_row(0).left(), layout::usage_row(0).top());
    assert_eq!(pixel(&frame, bar.left() + 8, bar.center().1), color(theme::RED));
    ui.usage.top_row = 1;
    ui.invalidate();
    let frame = full(&ui);
    assert!(lit_in(&frame, marker(0)).count() > 40 && lit_in(&frame, marker(3)).count() > 40);
    ui.host(&items(0, ""), 0, &mut out);
    let frame = full(&ui);
    assert!(lit_in(&frame, layout::usage_row(0)).count() > 100, "no usage data");
    assert_eq!(lit_in(&frame, layout::usage_row(1)).count(), 0);
    assert_eq!(lit_in(&frame, header).count(), 0, "no period, no tag");
}

#[test]
fn the_signature_is_one_lane_per_block() {
    fn face_signature(ui: &UiState) -> Box<Signature> {
        let mut sig = Box::new(Signature::new());
        let mut builder = SigBuilder::new(&mut sig);
        faces::usage::signature(ui, ui.current_face().id, &mut builder);
        sig
    }
    fn differing(a: &Signature, b: &Signature) -> Vec<usize> {
        assert_eq!(a.regions().len(), b.regions().len());
        (0..a.regions().len()).filter(|&i| a.regions()[i] != b.regions()[i]).collect()
    }
    let ui = usage();
    let base = face_signature(&ui);
    assert_eq!(base.regions().len(), 6);
    for slot in 0..4u8 {
        assert_eq!(base.regions()[1 + slot as usize].0, layout::usage_row(slot));
    }
    let mut updating = Box::new((*ui).clone());
    updating.usage.updating = true;
    updating.invalidate();
    assert_eq!(differing(&base, &face_signature(&updating)), [0]);
    let mut nudged = Box::new((*ui).clone());
    nudged.deck.usage.items[1].percent = 82;
    nudged.invalidate();
    assert_eq!(differing(&base, &face_signature(&nudged)), [2]);
    let mut switched = Box::new((*ui).clone());
    switched.deck.usage.items[1].account.set("work@example.com");
    assert_eq!(differing(&base, &face_signature(&switched)), [2]);
    let mut expiring = Box::new((*ui).clone());
    expiring.deck.usage.items[1].reset_tip.set("2 resets expire in 1d 6h");
    assert_eq!(differing(&base, &face_signature(&expiring)), [2]);
    let mut scrolled = Box::new((*ui).clone());
    let mut out = Update::new();
    scrolled.host(&items(6, "SEP 2026"), 0, &mut out);
    scrolled.usage.top_row = 2;
    assert_eq!(differing(&base, &face_signature(&scrolled)), [1, 2, 3, 4]);
    let mut same = Box::new((*ui).clone());
    same.invalidate();
    assert_eq!(face_signature(&same).regions(), base.regions());
}

#[test]
fn partial_refresh_matches_full_through_a_scripted_session() {
    let mut session = Session::new(usage());
    for account in ["personal@example.com", "work@company.example", "a.very.long.email.address.for.overflow@example.com", ""] {
        session.step(|ui| {
            for item in &mut ui.deck.usage.items {
                item.account.set(account);
            }
            ui.invalidate();
        });
    }
    for account in ["work@example.com", ""] {
        for tip in ["1 reset expires in <1m", "99+ resets expire in 2d 23h", ""] {
            session.step(|ui| {
                ui.deck.usage.items[1].account.set(account);
                ui.deck.usage.items[1].reset_tip.set(tip);
                ui.invalidate();
            });
        }
    }
    let tag = session.step(|ui| {
        ui.usage.updating = true;
        ui.invalidate();
    });
    assert!(tag <= 480 * 48, "{tag} px for the tag");
    session.step(|ui| {
        ui.usage.updating = false;
        ui.invalidate();
    });
    session.control(Control::Button(Button::Enter, Edge::Press));
    session.control(Control::Button(Button::Enter, Edge::Release));
    session.step(|ui| {
        let mut out = Update::new();
        ui.tick(ui.now_ms + 2000, 0, true, &mut out);
    });
    session.host(items(6, "OCT 2026"));
    let scrolled = session.step(|ui| {
        ui.usage.top_row = 1;
        ui.invalidate();
    });
    assert!(scrolled <= 4 * 416 * 92 + 480 * 48, "{scrolled} px for a scroll");
    session.control(Control::Wheel(Wheel::Cw));
    session.control(Control::Wheel(Wheel::Ccw));
    session.step(|ui| {
        ui.usage.top_row = 2;
        ui.invalidate();
    });
    for slot in 0..layout::USAGE_VISIBLE {
        let (x, y) = layout::usage_row(slot).center();
        session.touch(TouchPhase::Down, x, y);
        session.touch(TouchPhase::Move, x, y - 30);
        session.touch(TouchPhase::Up, x, y - 30);
    }
    session.host(items(2, "NOV"));
    session.step(|ui| {
        ui.deck.usage.items[0].percent = 100;
        ui.deck.usage.items[1].percent = 255;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.deck.usage.items[0].percent = 0;
        ui.deck.usage.items[0].tone = Some(Tone::Gray);
        ui.invalidate();
    });
    session.host(items(0, ""));
    session.host(items(3, "SEP 2026"));
    session.step(|ui| {
        ui.update_battery(BatteryState { estimated_percent: Some(15), charge: ChargeState::Battery, ..BatteryState::UNKNOWN });
        ui.invalidate();
    });
    session.step(|ui| {
        ui.deck.settings.demo = false;
        ui.deck.host_present = false;
        ui.invalidate();
    });
    session.step(|ui| {
        ui.usage.updating = true;
        ui.deck.host_present = true;
        ui.invalidate();
    });
}

#[test]
fn every_origin_draws_the_face_shifted_and_never_panics() {
    let mut ui = usage();
    let mut out = Update::new();
    ui.host(&items(6, "a period tag that is too long"), 0, &mut out);
    ui.usage.top_row = 1;
    ui.deck.usage.items[2].name.set("A provider name that overflows the line");
    ui.deck.usage.items[3].reset.set("resets at some point far in the future");
    let id = ui.current_face().id;
    let reference = full_draw(|band| faces::usage::draw(band, &ui, id));
    for dx in -480..=480 {
        let frame = full_draw(|band| {
            band.set_origin_x(dx);
            faces::usage::draw(band, &ui, id);
        });
        if ![-480, -333, -1, 0, 1, 77, 479, 480].contains(&dx) {
            continue;
        }
        for y in (0..480).step_by(3) {
            for x in 0..480 {
                let expected = if (0..480).contains(&(x - dx)) { pixel(&reference, x - dx, y) } else { 0 };
                assert_eq!(pixel(&frame, x, y), expected, "origin {dx} at ({x}, {y})");
            }
        }
    }
}
