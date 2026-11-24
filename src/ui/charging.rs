//! The powered-off charging screen. The runtime owns its lifetime and input
//! policy; this renderer reads only the battery and saved interface language.
//! No deck controls, host state, overlays or animation reach this screen.
use crate::battery::{BatteryState, ChargeState};
use crate::deck::text::Text;
use crate::display::Band;
use crate::ui::i18n::Language;
use crate::ui::signature::SigBuilder;
use crate::ui::{font, font::Align, font::Font, kit, layout::Rect, theme};

const MODE: u32 = 0x4348_5247; // CHRG, distinct from the normal deck scene.
const STATUS: Rect = Rect::new(40, 76, 400, 44);
const BATTERY: Rect = Rect::new(164, 150, 144, 76);
const CAP: Rect = Rect::new(308, 174, 9, 28);
const FILL: Rect = Rect::new(172, 158, 128, 60);
const PERCENT: Rect = Rect::new(80, 244, 320, 62);
const HINT: Rect = Rect::new(40, 338, 400, 44);
const POWER_HINT: &str = "Hold PTT 2 s to power on";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Charging,
    Full,
    NotCharging,
    Unavailable,
}

impl Status {
    #[cfg(test)]
    const ALL: [Self; 4] = [Self::Charging, Self::Full, Self::NotCharging, Self::Unavailable];

    fn of(battery: BatteryState) -> Self {
        match battery.charge {
            ChargeState::Charging => Self::Charging,
            ChargeState::Full => Self::Full,
            ChargeState::Usb | ChargeState::Battery => Self::NotCharging,
            ChargeState::Unknown => Self::Unavailable,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Charging => "Charging",
            Self::Full => "Fully charged",
            Self::NotCharging => "Not charging",
            Self::Unavailable => "Charge status unavailable",
        }
    }

    const fn color(self) -> u32 {
        match self {
            Self::Charging | Self::Full => theme::GREEN,
            Self::NotCharging => theme::AMBER,
            Self::Unavailable => theme::GRAY,
        }
    }
}

fn percentage(battery: BatteryState) -> Option<u8> {
    battery.percentage().map(|value| value.min(100))
}

fn percent_text(percent: Option<u8>) -> Text<8> {
    let mut text = Text::new();
    let Some(percent) = percent else {
        text.set("\u{2014}");
        return text;
    };
    if percent >= 100 {
        text.push_char('1');
    }
    if percent >= 10 {
        text.push_char((b'0' + percent / 10 % 10) as char);
    }
    text.push_char((b'0' + percent % 10) as char);
    text.push_char('%');
    text
}

pub fn draw(band: &mut Band, battery: BatteryState, language: Language) {
    band.fill(kit::rgb(theme::CANVAS));
    let status = Status::of(battery);
    let percent = percentage(battery);
    let color = status.color();
    font::draw(band, Font::Name, STATUS.center().0, 108, language.text(status.label()), theme::TEXT_PRIMARY, Some(theme::CANVAS), STATUS.width(), Align::Center);

    kit::outline(band, BATTERY, 12, color, 3);
    kit::round(band, CAP, 3, color);
    kit::round(band, FILL, 6, theme::CHIP_FILL);
    if let Some(percent) = percent.filter(|&value| value > 0) {
        let width = (FILL.width() * i32::from(percent) + 99) / 100;
        let fill = Rect::new(FILL.left(), FILL.top(), width, FILL.height());
        kit::round(band, fill, (width / 2).min(6) as u32, color);
    }

    let text = percent_text(percent);
    font::draw(band, Font::Display, PERCENT.center().0, 292, text.as_str(), theme::TEXT_PRIMARY, Some(theme::CANVAS), PERCENT.width(), Align::Center);
    font::draw(band, Font::Body, HINT.center().0, 370, language.text(POWER_HINT), theme::TEXT_SECONDARY, Some(theme::CANVAS), HINT.width(), Align::Center);
}

/// Charging is static: ADC noise and metadata changes cannot repaint it.
/// A changed percentage/status redraws once; language changes replace the page.
pub fn signature(battery: BatteryState, language: Language, sig: &mut SigBuilder) {
    sig.mode(MODE);
    sig.mode(language as u32);
    sig.region_with(Rect::SCREEN, |hash| {
        hash.u8(Status::of(battery) as u8).opt_u32(percentage(battery).map(u32::from));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_estimated_hundred_percent_is_not_a_confirmed_full_charge() {
        let mut battery = BatteryState { estimated_percent: Some(100), ..BatteryState::UNKNOWN };
        assert_eq!(Status::of(battery), Status::Unavailable);
        battery.charge = ChargeState::Charging;
        assert_eq!(Status::of(battery), Status::Charging);
        battery.charge = ChargeState::Full;
        assert_eq!(Status::of(battery), Status::Full);
        battery.charge = ChargeState::Usb;
        assert_eq!(Status::of(battery), Status::NotCharging);
    }

    #[test]
    fn charging_copy_and_numbers_fit_in_all_four_languages() {
        for language in Language::ALL {
            for status in Status::ALL {
                assert!(font::measure(Font::Name, language.text(status.label())) <= STATUS.width(), "{}: {}", language.code(), status.label());
            }
            assert!(font::measure(Font::Body, language.text(POWER_HINT)) <= HINT.width(), "{}", language.code());
        }
        for (percent, expected) in [(None, "\u{2014}"), (Some(0), "0%"), (Some(9), "9%"), (Some(10), "10%"), (Some(99), "99%"), (Some(100), "100%")] {
            let text = percent_text(percent);
            assert_eq!(text.as_str(), expected);
            assert!(font::measure(Font::Display, text.as_str()) <= PERCENT.width());
        }
        for rect in [STATUS, BATTERY, CAP, PERCENT, HINT] {
            assert!(crate::ui::layout::in_safe_area(&rect), "{rect:?}");
        }
    }
}
