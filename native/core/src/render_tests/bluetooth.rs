//! The radio indicator shares the retained-frame renderer with every face.
use super::*;
use crate::ui::{
    bluetooth::{Level, Status},
    state::Overlay,
    Update,
};

const STATES: [Status; 6] = [
    Status::Unknown,
    Status::Signal(Level::Strong),
    Status::Signal(Level::Good),
    Status::Signal(Level::Fair),
    Status::Signal(Level::Weak),
    Status::Disconnected,
];

fn exercise(state: &mut UiState) {
    let mut screens = Screens::new(state);
    for status in STATES {
        let before = Box::new((*state).clone());
        assert!(state.update_bluetooth(status));
        let pixels = update(&mut screens, &before, state);
        assert!(
            pixels > 0 && pixels < 2000,
            "only the status region should transfer: {pixels}"
        );
        let old = full(&before);
        for (index, (a, b)) in old
            .chunks_exact(2)
            .zip(screens.logical.chunks_exact(2))
            .enumerate()
        {
            if a != b {
                assert!(layout::BLUETOOTH.contains((index % 480) as i32, (index / 480) as i32));
                assert!(layout::point_in_safe_area((index % 480) as i32, (index / 480) as i32));
            }
        }
        assert!(!state.update_bluetooth(status));
        assert!(damage(Some(state), state).next().is_none());
    }
}

#[test]
fn signal_changes_refresh_cleanly_on_every_face() {
    for template in [
        Template::Agents,
        Template::Omarchy,
        Template::Macropad,
        Template::Usage,
        Template::Settings,
    ] {
        exercise(&mut on(template));
    }
}

#[test]
fn signal_changes_refresh_over_overlays_and_during_a_swipe() {
    for overlay in [Overlay::Drawer, Overlay::Manager, Overlay::Tutorial] {
        let mut state = ui();
        state.global.overlay = overlay;
        exercise(&mut state);
    }
    let mut state = ui();
    state.global.strip_x = 200 * crate::ui::state::STRIP_ONE;
    exercise(&mut state);
}

#[test]
fn usb_and_host_updates_do_not_change_bluetooth_quality() {
    let mut state = ui();
    let mut update = Update::new();
    state.set_connected(true, &mut update);
    assert_eq!(state.bluetooth, Status::Disconnected);
    state.update_bluetooth(Status::Signal(Level::Strong));
    state.update_bluetooth(Status::Disconnected);
    assert!(state.connected, "USB can remain up after BLE drops");
    state.update_bluetooth(Status::Signal(Level::Weak));
    state.set_connected(true, &mut update);
    assert_eq!(state.bluetooth, Status::Signal(Level::Weak));
    state.reset();
    assert_eq!(state.bluetooth, Status::Disconnected);
}
