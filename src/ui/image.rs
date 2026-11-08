//! A single on-demand preview, using 16.5 KiB rather than a full framebuffer.
//! The device pulls one chunk at a time; PTT pauses the next request.
use super::{
    font::{self, Align, Font},
    kit,
    layout::Rect,
    state::{Target, TouchAct, UiState, Update},
    theme,
};
use crate::{
    control::{Button, Control, Edge},
    deck::msg::{Event, HostMsg},
    display::Band,
};
use embedded_graphics::{
    pixelcolor::{raw::RawU16, Rgb565},
    prelude::*,
};

const CAP: usize = 512 + 128 * 128;
const CLOSE: Rect = Rect::new(24, 410, 432, 48);
#[derive(Clone, Copy)]
pub struct View {
    pub open: bool,
    agent: u8,
    sid: u32,
    rev: u16,
    index: u8,
    received: u16,
    total: u16,
    width: u16,
    height: u16,
    asked_at: Option<u32>,
    retries: u8,
    failed: bool,
    pixels: [u8; CAP],
}
impl View {
    pub const fn new() -> Self {
        Self {
            open: false,
            agent: 0,
            sid: 0,
            rev: 0,
            index: 0,
            received: 0,
            total: 0,
            width: 0,
            height: 0,
            asked_at: None,
            retries: 0,
            failed: false,
            pixels: [0; CAP],
        }
    }
}
pub fn open(ui: &mut UiState, index: u8, rev: u16, at: u32, out: &mut Update) {
    let v = &mut ui.image;
    v.open = true;
    v.agent = ui.agents.agent;
    v.sid = ui.deck.sid;
    v.rev = rev;
    v.index = index;
    v.received = 0;
    v.total = 0;
    v.width = 0;
    v.height = 0;
    v.asked_at = None;
    v.retries = 0;
    v.failed = false;
    tick(ui, at, out);
    out.dirty = true;
}
pub fn tick(ui: &mut UiState, at: u32, out: &mut Update) {
    if !ui.image.open {
        return;
    }
    // The request identifies the image the user opened, not the latest chat
    // window. The host pins that preview while messages continue arriving.
    if ui.image.agent != ui.agents.agent
        || ui.deck.sid != ui.image.sid
        || ui.deck.agent(ui.image.agent).is_none()
    {
        ui.image.open = false;
        out.dirty = true;
        return;
    }
    if !ui.deck.host_present {
        if !ui.image.failed {
            ui.image.failed = true;
            out.dirty = true;
        }
        return;
    }
    if ui.global.ptt.is_some() {
        return;
    }
    let v = &mut ui.image;
    if v.failed || (v.total > 0 && v.received == v.total) {
        return;
    }
    if v.asked_at.is_some_and(|t| at.wrapping_sub(t) < 6000) {
        return;
    }
    if v.retries >= 3 {
        v.failed = true;
        out.dirty = true;
        return;
    }
    out.push_deck(Event::Image {
        agent: v.agent,
        rev: v.rev,
        index: v.index,
        offset: v.received,
    });
    v.asked_at = Some(at);
    v.retries += 1;
}
pub fn host(ui: &mut UiState, msg: &HostMsg, out: &mut Update) {
    // Hello is also the normal five-second heartbeat. Only a different host
    // session invalidates the transfer; a same-session sync is harmless too.
    if ui.image.open && matches!(msg, HostMsg::Hello { sid, .. } if *sid != ui.image.sid) {
        ui.image.open = false;
        out.dirty = true;
        return;
    }
    let HostMsg::Image {
        agent,
        rev,
        index,
        offset,
        total,
        width,
        height,
        len,
        data,
    } = msg
    else {
        return;
    };
    let v = &mut ui.image;
    if !v.open || *agent != v.agent || *rev != v.rev || *index != v.index || *offset != v.received {
        return;
    }
    let expected = 512usize + usize::from(*width) * usize::from(*height);
    let end = usize::from(*offset) + usize::from(*len);
    if *width == 0
        || *height == 0
        || *width > 128
        || *height > 128
        || usize::from(*total) != expected
        || end > expected
        || *len == 0
        || *len > 384
        || (*offset != 0 && (*total != v.total || *width != v.width || *height != v.height))
        || (end < expected && *len != 384)
    {
        v.failed = true;
        out.dirty = true;
        return;
    }
    v.pixels[usize::from(*offset)..end].copy_from_slice(&data[..usize::from(*len)]);
    v.received = end as u16;
    v.total = *total;
    v.width = *width;
    v.height = *height;
    v.asked_at = None;
    v.retries = 0;
    out.dirty = true;
}
pub fn control(ui: &mut UiState, control: Control, out: &mut Update) -> bool {
    if !ui.image.open {
        return false;
    }
    if matches!(control, Control::Button(Button::Cancel, Edge::Press)) {
        ui.image.open = false;
        out.dirty = true;
    }
    true
}
pub fn hit(x: i32, y: i32) -> Option<Target> {
    CLOSE.contains(x, y).then(|| Target::tap(CLOSE, 0))
}
pub fn touch(ui: &mut UiState, act: TouchAct, out: &mut Update) {
    if matches!(act, TouchAct::Activate) {
        ui.image.open = false;
        out.dirty = true;
    }
}
fn text(band: &mut Band, y: i32, value: &str) {
    font::draw(
        band,
        Font::Body,
        240,
        y,
        value,
        theme::TEXT_PRIMARY,
        Some(theme::CANVAS),
        416,
        Align::Center,
    );
}
pub fn draw(band: &mut Band, ui: &UiState) {
    let v = &ui.image;
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("Image"), kit::header_right(ui, kit::HeaderRight::None));
    if v.total > 0 && v.received == v.total && !v.failed {
        let scale = (384 / i32::from(v.width))
            .min(320 / i32::from(v.height))
            .max(1);
        let w = i32::from(v.width) * scale;
        let h = i32::from(v.height) * scale;
        let x0 = (480 - w) / 2;
        let y0 = 72 + (320 - h) / 2;
        let rect = embedded_graphics::primitives::Rectangle::new(
            Point::new(x0, y0),
            Size::new(w as u32, h as u32),
        );
        // Only visit this display band's pixels, not the entire image per band.
        let rect = rect.intersection(&band.bounding_box());
        let _ = band.draw_iter(rect.points().map(|p| {
            let i = ((p.y - y0) / scale) as usize * usize::from(v.width)
                + ((p.x - x0) / scale) as usize;
            let palette = usize::from(v.pixels[512 + i]) * 2;
            let rgb = u16::from_le_bytes([v.pixels[palette], v.pixels[palette + 1]]);
            Pixel(p, Rgb565::from(RawU16::new(rgb)))
        }));
    } else if v.failed {
        text(band, 220, ui.tr("Preview unavailable"));
        text(band, 258, ui.tr("Close and tap the image to retry."));
    } else {
        use core::fmt::Write;
        let mut label = crate::deck::text::Text::<48>::new();
        if v.total > 0 {
            let _ = write!(
                label,
                "{} {}%",
                ui.tr("Loading image…"),
                u32::from(v.received) * 100 / u32::from(v.total)
            );
        } else {
            label.set(ui.tr("Loading image…"));
        }
        text(band, 240, label.as_str());
    }
    text(band, 442, ui.tr("Close · CANCEL"));
}
pub fn signature(ui: &UiState, sig: &mut super::signature::SigBuilder) {
    sig.mode(0x494d47);
    sig.region_with(crate::ui::faces::agents::HEADER_LANE, |h| {
        h.header_right(kit::header_right(ui, kit::HeaderRight::None));
    });
    sig.region_with(Rect::new(0, 60, 480, 420), |h| {
        let v = &ui.image;
        h.u8(v.agent)
            .u16(v.rev)
            .u8(v.index)
            .u16(v.received)
            .u16(v.total)
            .bool(v.failed);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.host(&HostMsg::Hello { v: 1, sid: 7, minute_of_day: None }, 0, &mut Update::new());
        ui.agents.view = crate::ui::state::AgentsView::Channel;
        ui.agents.agent = 1;
        ui.deck.agents[0].id = 1;
        ui.deck.agents[0].present = true;
        ui.deck.agents[0].reply_rev = 3;
        ui
    }

    #[test]
    fn full_transfer_and_preview_survive_heartbeats_sync_and_chat_updates() {
        for update_chat in [false, true] {
            let mut ui = hosted();
            let mut out = Update::new();
            open(&mut ui, 0, 3, 1, &mut out);
            let pixels: std::vec::Vec<u8> = (0..CAP).map(|i| i as u8).collect();
            // Deliberately stretch a full-size transfer across several normal
            // heartbeats. Exercise UiState's real message/timer dispatch.
            for (i, chunk) in pixels.chunks(384).enumerate() {
                let at = (i as u32 + 1) * 500;
                if at % 5000 == 0 {
                    ui.host(&HostMsg::Hello { v: 1, sid: 7, minute_of_day: None }, at, &mut Update::new());
                }
                if at == 10000 {
                    ui.host(&HostMsg::Synced, at, &mut Update::new());
                }
                if update_chat && at == 3000 {
                    ui.host(&HostMsg::Reply { agent: 1, text: crate::deck::text::Text::new(), rev: 4 }, at, &mut Update::new());
                }
                assert!(out.deck.iter().flatten().any(|event| matches!(event,
                    Event::Image { agent: 1, rev: 3, index: 0, offset } if usize::from(*offset) == i * 384)));
                let mut data = [0; 384];
                data[..chunk.len()].copy_from_slice(chunk);
                ui.host(&HostMsg::Image {
                    agent: 1, rev: 3, index: 0, offset: (i * 384) as u16,
                    total: CAP as u16, width: 128, height: 128,
                    len: chunk.len() as u16, data,
                }, at, &mut Update::new());
                out = Update::new();
                ui.tick(at + 1, 0, true, &mut out);
                assert!(ui.image.open, "preview closed at {at} ms; chat updates: {update_chat}");
                assert!(!ui.image.failed);
            }
            assert_eq!(usize::from(ui.image.received), CAP);
            assert_eq!(ui.image.pixels.as_slice(), pixels.as_slice());
            assert!(!out.deck.iter().flatten().any(|e| matches!(e, Event::Image { .. })));
            ui.host(&HostMsg::Hello { v: 1, sid: 7, minute_of_day: None }, 25000, &mut Update::new());
            ui.tick(25001, 0, true, &mut Update::new());
            assert!(ui.image.open, "a completed preview stays visible too");
        }
    }

    #[test]
    fn a_new_host_session_or_a_different_channel_closes_the_preview() {
        let mut ui = hosted();
        open(&mut ui, 0, 3, 1, &mut Update::new());
        let mut out = Update::new();
        ui.host(&HostMsg::Hello { v: 1, sid: 8, minute_of_day: None }, 2, &mut out);
        assert!(!ui.image.open);
        assert!(out.dirty);

        let mut ui = hosted();
        open(&mut ui, 0, 3, 1, &mut Update::new());
        ui.agents.agent = 2;
        ui.tick(2, 0, true, &mut Update::new());
        assert!(!ui.image.open);
    }

    #[test]
    fn unavailable_image_keeps_the_preview_open_with_an_error() {
        let mut ui = hosted();
        open(&mut ui, 0, 3, 1, &mut Update::new());
        ui.host(&HostMsg::Image {
            agent: 1, rev: 3, index: 0, offset: 0, total: 0,
            width: 0, height: 0, len: 0, data: [0; 384],
        }, 2, &mut Update::new());
        ui.host(&HostMsg::Hello { v: 1, sid: 7, minute_of_day: None }, 5000, &mut Update::new());
        let mut out = Update::new();
        ui.tick(5001, 0, true, &mut out);
        assert!(ui.image.open && ui.image.failed);
        assert!(!out.deck.iter().flatten().any(|e| matches!(e, Event::Image { .. })));
    }

    #[test]
    fn chunks_are_ordered_and_bound_to_the_view() {
        let mut ui = UiState::new();
        let mut out = Update::new();
        ui.image.open = true;
        ui.image.agent = 1;
        ui.image.rev = 3;
        let chunk = |rev, offset, len| HostMsg::Image {
            agent: 1,
            rev,
            index: 0,
            offset,
            total: 513,
            width: 1,
            height: 1,
            len,
            data: [0; 384],
        };
        host(&mut ui, &chunk(2, 0, 384), &mut out);
        assert_eq!(ui.image.received, 0);
        host(&mut ui, &chunk(3, 384, 129), &mut out);
        assert_eq!(ui.image.received, 0);
        host(&mut ui, &chunk(3, 0, 384), &mut out);
        assert_eq!(ui.image.received, 384);
        host(&mut ui, &chunk(3, 384, 129), &mut out);
        assert_eq!(ui.image.received, 513);
        assert!(!ui.image.failed);
    }
    #[test]
    fn transfer_times_out_and_cancel_stops_requests() {
        let mut ui = UiState::new();
        ui.reset();
        ui.deck.host_present = true;
        ui.agents.agent = 1;
        ui.deck.agents[0].present = true;
        ui.deck.agents[0].reply_rev = 3;
        let mut out = Update::new();
        open(&mut ui, 0, 3, 0, &mut out);
        assert_eq!(ui.image.retries, 1);
        tick(&mut ui, 5999, &mut out);
        assert_eq!(ui.image.retries, 1);
        tick(&mut ui, 6000, &mut out);
        tick(&mut ui, 12000, &mut out);
        tick(&mut ui, 18000, &mut out);
        assert!(ui.image.failed);
        assert!(control(
            &mut ui,
            Control::Button(Button::Cancel, Edge::Press),
            &mut out
        ));
        assert!(!ui.image.open);
        let retries = ui.image.retries;
        tick(&mut ui, 30000, &mut out);
        assert_eq!(ui.image.retries, retries);
    }
}
