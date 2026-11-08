//! Four live computer meters, independent of account Usage.
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::{
    font::{self, Align, Font},
    kit,
    layout::Rect,
    signature::SigBuilder,
    state::UiState,
    theme,
};
use core::fmt::Write;

fn row(index: usize) -> Rect {
    Rect::new(36, 76 + index as i32 * 82, 408, 78)
}
fn state(ui: &UiState) -> &'static str {
    if ui.deck.system_at.is_none() {
        "WAITING"
    } else if !ui.deck.host_present && !ui.deck.demo_active() {
        "OFFLINE"
    } else if ui.deck.system_stale {
        "STALE"
    } else {
        "LIVE"
    }
}
fn percent(part: u32, total: u32) -> Option<u8> {
    (total > 0).then(|| ((u64::from(part) * 100 / u64::from(total)).min(100)) as u8)
}
fn gib(mib: u32, text: &mut Text<64>) {
    let tenths = (u64::from(mib) * 10 + 512) / 1024;
    let _ = write!(text, "{}.{:01}", tenths / 10, tenths % 10);
}
fn metric(ui: &UiState, index: usize) -> (&'static str, Option<u8>, Text<64>) {
    let s = &ui.deck.system;
    let mut detail = Text::new();
    let (name, value) = match index {
        0 => {
            detail.push_str(ui.tr("Total processor load"));
            ("CPU", s.cpu)
        }
        1 => {
            gib(s.memory_used_mib, &mut detail);
            detail.push_str(" / ");
            gib(s.memory_total_mib, &mut detail);
            detail.push_str(ui.tr(" GiB used"));
            (ui.tr("Memory"), percent(s.memory_used_mib, s.memory_total_mib))
        }
        2 => {
            detail.push_str(ui.tr("Graphics processor load"));
            ("GPU", s.gpu)
        }
        _ => {
            gib(s.disk_free_mib, &mut detail);
            detail.push_str(" / ");
            gib(s.disk_total_mib, &mut detail);
            detail.push_str(ui.tr(" GiB free"));
            (ui.tr("Storage"), percent(s.disk_free_mib, s.disk_total_mib))
        }
    };
    if value.is_none() {
        detail.clear();
        detail.push_str(ui.tr(if ui.deck.system_at.is_none() {
            "Waiting for computer"
        } else {
            "Not available"
        }));
    }
    (name, value, detail)
}
pub fn draw(band: &mut Band, ui: &UiState, _: u8) {
    band.fill(kit::rgb(theme::CANVAS));
    kit::header(band, ui.tr("SYSTEM STATUS"), kit::HeaderRight::Text(ui.tr(state(ui))));
    for i in 0..4 {
        let r = row(i);
        if !kit::visible(band, r) {
            continue;
        }
        let (label, value, detail) = metric(ui, i);
        let mut text = Text::<16>::new();
        if let Some(p) = value {
            let _ = write!(text, "{p}%");
        } else {
            text.push_str("\u{2014}");
        }
        let live = state(ui) == "LIVE";
        let color = if !live {
            theme::TEXT_FAINT
        } else if i == 3 {
            match value {
                Some(0..=10) => theme::RED,
                Some(11..=20) => theme::AMBER,
                _ => theme::GREEN,
            }
        } else {
            theme::usage_tone(value.unwrap_or(0))
        };
        font::draw(
            band,
            Font::Name,
            44,
            r.top() + 28,
            label,
            theme::TEXT_PRIMARY,
            Some(theme::CANVAS),
            250,
            Align::Left,
        );
        font::draw(
            band,
            Font::Name,
            436,
            r.top() + 28,
            text.as_str(),
            color,
            Some(theme::CANVAS),
            120,
            Align::Right,
        );
        kit::bar(
            band,
            Rect::new(44, r.top() + 42, 392, 7),
            value.unwrap_or(0),
            color,
        );
        font::draw(
            band,
            Font::Small,
            44,
            r.top() + 70,
            detail.as_str(),
            theme::TEXT_LABEL,
            Some(theme::CANVAS),
            392,
            Align::Left,
        );
    }
    kit::hint_bar_localized(
        band,
        &[kit::Hint {
            key: ui.tr("2 SEC"),
            label: "live refresh",
        }],
        ui.language,
    );
}
pub fn signature(ui: &UiState, _: u8, sig: &mut SigBuilder) {
    sig.region_with(Rect::new(34, 30, 412, 32), |h| {
        h.str(state(ui));
    });
    for i in 0..4 {
        let (label, value, detail) = metric(ui, i);
        sig.region_with(row(i), |h| {
            h.str(label)
                .u8(value.unwrap_or(255))
                .text(&detail)
                .str(state(ui));
        });
    }
    sig.region(crate::ui::layout::HINT_BAR, 0);
}
