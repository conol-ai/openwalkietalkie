//! The standalone charging screen bypasses normal deck scenes and controls.
use super::*;
use crate::{battery::{BatteryState, ChargeState}, ui::{charging, i18n::Language, signature::SigBuilder}};

fn battery(percent: Option<u8>, charge: ChargeState) -> BatteryState {
    BatteryState { estimated_percent: percent, charge, ..BatteryState::UNKNOWN }
}

fn frame(battery: BatteryState, language: Language) -> Vec<u8> {
    full_draw(|band| charging::draw(band, battery, language))
}

fn charging_signature(battery: BatteryState, language: Language) -> Signature {
    let mut signature = Signature::new();
    charging::signature(battery, language, &mut SigBuilder::new(&mut signature));
    signature
}

#[test]
fn every_charge_state_and_language_keeps_ink_inside_the_round_display() {
    for charge in [ChargeState::Charging, ChargeState::Full, ChargeState::Usb, ChargeState::Battery, ChargeState::Unknown] {
        for percent in [None, Some(0), Some(1), Some(9), Some(10), Some(72), Some(99), Some(100), Some(255)] {
            for language in Language::ALL {
                assert_eq!(lit_outside_safe_area(&frame(battery(percent, charge), language)), None, "{charge:?} {percent:?} {}", language.code());
            }
        }
    }
}

#[test]
fn the_standalone_charging_page_has_no_normal_deck_chrome() {
    for language in Language::ALL {
        let frame = frame(battery(Some(72), ChargeState::Charging), language);
        assert!(frame[..60 * 480 * 2].iter().all(|&byte| byte == 0), "no deck header or Bluetooth indicator");
        assert!(frame[428 * 480 * 2..].iter().all(|&byte| byte == 0), "no face hint bar or controls");
    }
}

#[test]
fn only_visible_battery_values_or_language_change_the_charging_signature() {
    let base = battery(Some(72), ChargeState::Charging);
    let before = charging_signature(base, Language::English);
    let mut metadata = base;
    metadata.millivolts = Some(4100);
    metadata.full_mv = 4350;
    assert_eq!(before, charging_signature(metadata, Language::English));
    assert!(Damage::between(Some(&before), &charging_signature(metadata, Language::English)).next().is_none());
    assert_eq!(
        charging_signature(battery(Some(100), ChargeState::Charging), Language::English),
        charging_signature(battery(Some(255), ChargeState::Charging), Language::English),
        "both show 100%",
    );
    assert_eq!(
        charging_signature(battery(Some(72), ChargeState::Usb), Language::English),
        charging_signature(battery(Some(72), ChargeState::Battery), Language::English),
        "both show not charging",
    );
    for next in [battery(Some(73), ChargeState::Charging), battery(None, ChargeState::Charging), battery(Some(72), ChargeState::Full), battery(Some(72), ChargeState::Usb), battery(Some(72), ChargeState::Unknown)] {
        assert_ne!(before, charging_signature(next, Language::English));
    }
    for language in Language::ALL.into_iter().filter(|&language| language != Language::English) {
        let next = charging_signature(base, language);
        assert_ne!(before.mode(), next.mode());
        let mut damage = Damage::between(Some(&before), &next);
        let mut pixels = 0;
        while let Some(rect) = damage.next() {
            pixels += usize::from(rect.x1 - rect.x0) * usize::from(rect.y1 - rect.y0);
        }
        assert_eq!(pixels, 480 * 480);
    }
    assert_ne!(before.mode(), signature(&ui()).mode(), "entering or leaving the charging scene repaints the page");
}

#[test]
fn charging_render_follows_the_panel_mounting_without_normal_scene_state() {
    for language in Language::ALL {
        let battery = battery(Some(72), ChargeState::Charging);
        let expected = panel_image(&frame(battery, language));
        let mut panel = vec![0; 480 * 480 * 2];
        let mut band = display::Band::new();
        for index in 0..display::BANDS {
            band.set_row(index);
            charging::draw(&mut band, battery, language);
            band.orient_for_panel();
            let panel_index = if orientation::ROTATE_180 { display::BANDS - 1 - index } else { index };
            let start = usize::from(panel_index) * display::BAND_BYTES;
            panel[start..start + display::BAND_BYTES].copy_from_slice(band.data());
        }
        assert_frame(&panel, &expected, language.code());
    }
}
