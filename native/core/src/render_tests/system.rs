//! Live telemetry must repaint only changed meters and age honestly.
use super::*;
use crate::{
    deck::msg::{HostMsg, SystemStatus},
    ui::Update,
};

fn sample(seq: u32) -> SystemStatus {
    SystemStatus {
        cpu: Some(23),
        gpu: Some(67),
        memory_used_mib: 23_552,
        memory_total_mib: 65_536,
        disk_free_mib: 421_888,
        disk_total_mib: 976_000,
        sample: seq,
    }
}

#[test]
fn live_meters_fit_and_only_changed_readings_repaint() {
    let mut previous = on(Template::System);
    previous.deck.host_present = true;
    previous.host(&HostMsg::System(sample(1)), 100, &mut Update::new());
    assert_eq!(lit_outside_safe_area(&full(&previous)), None);
    let mut next = Box::new((*previous).clone());
    let mut screen = Screens::new(&previous);
    next.host(&HostMsg::System(sample(2)), 2100, &mut Update::new());
    assert_eq!(
        update(&mut screen, &previous, &next),
        0,
        "heartbeat does not redraw unchanged pixels"
    );
    next.copy_into(&mut previous);
    for index in 0..4 {
        let mut s = next.deck.system;
        match index {
            0 => s.cpu = Some(91),
            1 => s.memory_used_mib = 51_000,
            2 => s.gpu = None,
            _ => s.disk_free_mib = 1000,
        }
        next.host(&HostMsg::System(s), 4100, &mut Update::new());
        let pixels = update(&mut screen, &previous, &next);
        assert!(
            (1..480 * 100).contains(&pixels),
            "only meter {index}: {pixels}"
        );
        next.copy_into(&mut previous);
    }
}

#[test]
fn waiting_stale_offline_and_unavailable_refresh_completely() {
    let mut previous = on(Template::System);
    previous.deck.settings.demo = false;
    previous.deck.host_present = true;
    let mut next = Box::new((*previous).clone());
    let mut screen = Screens::new(&previous);
    let start = u32::MAX - 4000;
    next.host(&HostMsg::System(sample(1)), start, &mut Update::new());
    update(&mut screen, &previous, &next);
    assert!(!next.deck.system_stale);
    next.copy_into(&mut previous);
    next.tick(start.wrapping_add(9999), 0, true, &mut Update::new());
    assert!(!next.deck.system_stale);
    next.tick(start.wrapping_add(10_000), 0, true, &mut Update::new());
    assert!(next.deck.system_stale);
    update(&mut screen, &previous, &next);
    next.copy_into(&mut previous);
    next.host(
        &HostMsg::System(SystemStatus::EMPTY),
        6000,
        &mut Update::new(),
    );
    assert!(!next.deck.system_stale);
    update(&mut screen, &previous, &next);
    assert_eq!(lit_outside_safe_area(&full(&next)), None);
    next.copy_into(&mut previous);
    next.deck.host_present = false;
    next.invalidate();
    update(&mut screen, &previous, &next);
}
