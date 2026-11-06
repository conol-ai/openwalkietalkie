//! Product UI, controls and device protocols shared with the Embassy firmware.
//! Every entry point runs on the single SDK application thread. Bluetooth
//! callbacks enqueue reports to that thread; they never enter this module.
#![cfg_attr(not(test), no_std)]
#![allow(dead_code)]

// Shared roots, mounted under the same names by every consumer.
#[path = "../../../src/battery/state.rs"]
mod battery;
#[path = "../../../src/control/types.rs"]
mod control;
#[path = "../../../src/deck/mod.rs"]
mod deck;
#[path = "../../../src/display/framebuffer.rs"]
mod display;
#[path = "../../../src/display/orientation.rs"]
mod orientation;
#[path = "../../../src/ui/mod.rs"]
mod ui;

// Runtime glue and drivers.
#[path = "../../../src/battery/charger.rs"]
mod charger;
#[path = "../../../src/input/cst9220.rs"]
mod cst9220;
#[path = "../../../src/input/direct.rs"]
mod direct;
#[path = "../../../src/input/keymap.rs"]
mod keymap;
#[path = "../../../src/power.rs"]
mod power;
#[path = "../../../src/input/tca8418.rs"]
mod tca8418;
#[path = "../../../src/input/touch_state.rs"]
mod touch_state;
#[path = "../../../src/link/wire.rs"]
mod wire;
#[path = "../../../src/input/xl9555.rs"]
mod xl9555;

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod render_tests;

use control::{Button, Control, Edge, Wheel};
use deck::msg::{Event, HostMsg};
use deck::proto;
use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, Operation};
use ui::{damage, scene, signature::Signature, splash, UiState, Update};

extern "C" {
    fn owt_i2c_transfer(
        address: u8,
        tx: *const u8,
        tx_len: usize,
        rx: *mut u8,
        rx_len: usize,
    ) -> i32;
    fn owt_delay_ms(ms: u32);
    fn owt_hid_notify(data: *const u8, length: usize) -> i32;
    fn owt_hid_disconnect();
    fn owt_touch_asserted() -> bool;
    fn owt_keypad_asserted() -> bool;
    /// Rev A: an edge on one of the eight direct inputs since the last call,
    /// and their raw levels in `direct`'s bit order. The dev board's C side
    /// answers false and 0xFF.
    fn owt_inputs_asserted() -> bool;
    fn owt_inputs_read() -> u8;
    fn owt_keypad_retry();
    fn owt_watchdog_result(ok: bool);
    fn owt_touch_trace(phase: u8, x: u16, y: u16, ready: bool);
    fn owt_input_wake_trace(source: u8, now: u32);
    fn owt_battery_mv() -> u16;
    fn owt_battery_trace(mv: u16, voltage_percent: u8, estimated_percent: u8, charge: u8);
    /// Rev B: the BQ24073's CHG output on PA33 reads low (charging).
    #[cfg(feature = "hw-b")]
    fn owt_charger_charging() -> bool;
    fn owt_platform_panic() -> !;
}

#[derive(Clone, Copy)]
struct Bus;
impl ErrorType for Bus {
    type Error = ErrorKind;
}
impl I2c for Bus {
    fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
        let (tx, rx): (&[u8], &mut [u8]) = match ops {
            [Operation::Write(tx), Operation::Read(rx)] => (tx, rx),
            [Operation::Write(tx)] => (tx, &mut []),
            [Operation::Read(rx)] => (&[], rx),
            _ => return Err(ErrorKind::Other),
        };
        let result =
            unsafe { owt_i2c_transfer(address, tx.as_ptr(), tx.len(), rx.as_mut_ptr(), rx.len()) };
        if result == 0 {
            Ok(())
        } else {
            Err(ErrorKind::Bus)
        }
    }
}

struct Core {
    ui: UiState,
    rssi: ui::bluetooth::RssiFilter,
    contact: touch_state::Contact,
    rx: wire::Reassembler,
    devices: u32,
    /// Channel 5: a line was received on this link (the device never speaks
    /// first there).
    deck_rx: bool,
    redraw: bool,
    battery_at: u32,
    battery_status_at: u32,
    touch_at: u32,
    touch_poll_at: u32,
    /// Held wheel key: direction, hold start, next detent.
    repeat: Option<(Wheel, u32, u32)>,
    power: power::Policy,
    now: u32,
    keypad_poll_at: u32,
    /// The keypad bus is failing: its releases went out once for the outage.
    keypad_failed: bool,
    /// Rev A's six buttons and wheel, decoded from GPIO levels.
    inputs: Option<direct::Decoder>,
    inputs_poll_at: u32,
    battery_sampled: bool,
    battery_estimator: battery::Estimator,
}
impl Core {
    const fn new() -> Self {
        Self {
            ui: UiState::new(),
            rssi: ui::bluetooth::RssiFilter::new(),
            contact: touch_state::Contact::new(),
            rx: wire::Reassembler::new(),
            devices: 0,
            deck_rx: false,
            redraw: true,
            battery_at: 0,
            battery_status_at: 0,
            touch_at: 0,
            touch_poll_at: 0,
            repeat: None,
            power: power::Policy::new(0),
            now: 0,
            keypad_poll_at: 0,
            keypad_failed: false,
            inputs: None,
            inputs_poll_at: 0,
            battery_sampled: false,
            // Rev A's charger reports nothing: the estimate is voltage-only.
            // Rev B's CHG pin says when the cell charges, which is a status.
            battery_estimator: if cfg!(feature = "hw-a") && !cfg!(feature = "hw-b") {
                battery::Estimator::voltage_only()
            } else {
                battery::Estimator::new()
            },
        }
    }
}
static mut CORE: Core = Core::new();
static mut BANDS: [display::Band; 2] = [display::Band::new(), display::Band::new()];
// The renderer's snapshot and the two signatures around it. A `UiState` is
// several kilobytes and this thread's stack is small: these are only ever
// filled in place, never built or moved by value.
static mut FRAME: UiState = UiState::new();
// The native image keeps the old eight sessions in SRAM, one table for the
// live state and one for the renderer's snapshot (the Embassy firmware puts
// 255 in PSRAM instead).
static mut CORE_AGENTS: [deck::model::Agent; deck::model::DEFAULT_SESSIONS] =
    [deck::model::Agent::new(); deck::model::DEFAULT_SESSIONS];
static mut FRAME_AGENTS: [deck::model::Agent; deck::model::DEFAULT_SESSIONS] =
    [deck::model::Agent::new(); deck::model::DEFAULT_SESSIONS];
static mut PREV_SIG: Signature = Signature::new();
static mut NEXT_SIG: Signature = Signature::new();
static mut FRAME_VALID: bool = false;
static mut DAMAGE: damage::Damage = damage::Damage::empty();
// Channel 5: the line being assembled, the parse target and the TX line are
// each hundreds of bytes and live here rather than on the app thread's stack.
static mut LINE: proto::LineAssembler = proto::LineAssembler::new();
static mut HOST_MSG: HostMsg = HostMsg::None;
static mut TX: [u8; proto::TX_MAX] = [0; proto::TX_MAX];

fn delay(ms: u32) {
    unsafe { owt_delay_ms(ms) };
}

fn keypad_init() -> bool {
    let mut expander = xl9555::Xl9555::new(Bus, xl9555::ADDR);
    if expander.init().is_err() {
        return false;
    }
    delay(20);
    if expander.set(xl9555::WIFI_EN, true).is_err() {
        return false;
    }
    delay(20);
    // The on-screen replica supplies lighting. Leave the physical LED rail
    // off and the unused ESP32/GPS/haptic devices in reset, as init configured.
    delay(100);
    if expander.set(xl9555::KEY_RST, false).is_err() {
        return false;
    }
    delay(10);
    if expander.set(xl9555::KEY_RST, true).is_err() {
        return false;
    }
    delay(50);
    tca8418::Tca8418::new(Bus, 0x34).configure(5, 4).is_ok()
}

/// Called after the board has released the touch reset pin for at least 50ms.
#[no_mangle]
pub unsafe extern "C" fn owt_core_init() -> u32 {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    // SAFETY: the two tables are reached only through these two states.
    core.ui.deck.agents.attach(core::ptr::addr_of_mut!(CORE_AGENTS).cast(), deck::model::DEFAULT_SESSIONS);
    (*core::ptr::addr_of_mut!(FRAME)).deck.agents.attach(core::ptr::addr_of_mut!(FRAME_AGENTS).cast(), deck::model::DEFAULT_SESSIONS);
    core.ui.reset();
    if charger::initialize(&mut Bus).is_ok() {
        core.devices |= 1;
    }
    if keypad_init() {
        core.devices |= 2;
    }
    #[cfg(feature = "hw-a")]
    {
        core.inputs = Some(direct::Decoder::new(owt_inputs_read(), 0));
        core.devices |= 8;
    }
    let mut touch = cst9220::Cst9220::new(Bus);
    if touch.enter_info_mode().is_ok() {
        delay(40);
        if touch
            .identity()
            .map(|id| id.checkcode == 0xcaca4e20)
            .unwrap_or(false)
            && touch.enter_normal_mode().is_ok()
        {
            core.devices |= 4;
        }
    }
    core.devices
}

fn send_json(json: &[u8]) {
    let mut offset = 0;
    loop {
        let mut report = [0; wire::REPORT_LEN];
        let used = wire::frame(json, offset, &mut report);
        if used == 0 {
            break;
        }
        offset += used;
        if unsafe { owt_hid_notify(report[1..].as_ptr(), 63) } != 0 {
            break;
        }
        delay(4);
    }
}

/// One deck event on channel 5, paced like `send_json`. An event that does not
/// fit `TX` is dropped whole: a cut line must never reach the host.
fn send_deck(event: Event, ui: &UiState) {
    let tx = unsafe { &mut *core::ptr::addr_of_mut!(TX) };
    let n = proto::encode(event, ui, tx);
    if n == 0 {
        return;
    }
    let mut offset = 0;
    let mut report = [0; proto::REPORT_LEN];
    loop {
        let used = proto::frame(&tx[..n], offset, &mut report);
        if used == 0 {
            break;
        }
        offset += used;
        if unsafe { owt_hid_notify(report[1..].as_ptr(), 63) } != 0 {
            break;
        }
        delay(4);
    }
}

fn apply(core: &mut Core, input: Control) {
    let before = core.power.screen();
    let deliver = core.power.input(input, core.now);
    core.redraw |= core.ui.set_screen_lock(core.power.lock_progress());
    core.redraw |= before != core.power.screen();
    if before == power::Screen::Off && core.power.screen() != before {
        unsafe {
            owt_input_wake_trace(
                if matches!(input, Control::Touch(_)) {
                    0
                } else {
                    1
                },
                core.now,
            )
        };
    }
    if !deliver {
        return;
    }
    if let Control::Touch(touch) = input {
        unsafe {
            owt_touch_trace(touch.phase as u8, touch.x, touch.y, core.deck_rx);
        }
    }
    let mut update = Update::new();
    core.ui.apply(input, core.now, &mut update);
    emit(core, &mut update);
}

/// The deck host is on the wire: a `hello` was applied on this link.
fn deck_online(core: &Core) -> bool {
    core.ui.deck.host_present && core.deck_rx
}

/// Act on one update: a notice lights the screen, deck events go out on
/// channel 5 once the deck host said hello (or into the demo's built-in
/// host), and an overflow ends the link after `up` so no release can stay
/// held on the host.
fn emit(core: &mut Core, update: &mut Update) {
    if update.wake {
        let before = core.power.screen();
        core.power.notify(core.now);
        core.redraw |= before != core.power.screen();
        if before == power::Screen::Off {
            unsafe { owt_input_wake_trace(3, core.now) };
        }
    }
    let online = deck_online(core);
    if !online && core.ui.deck.demo_active() {
        // The built-in host's answers are applied into this same update.
        core.ui
            .demo_loopback(core.now, update, unsafe { &mut *core::ptr::addr_of_mut!(HOST_MSG) });
    }
    core.redraw |= update.dirty;
    if online {
        for event in update.deck_events() {
            send_deck(event, &core.ui);
        }
    }
    if update.overflow {
        if online {
            send_deck(Event::Up, &core.ui);
        }
        unsafe { owt_hid_disconnect() };
    }
    // A CANCEL hold ran its course. The native image has no soft power-off
    // yet (the Embassy image enters the PMU's hibernate here): the dialog
    // closes and the deck stays on.
    let _ = update.power_off;
}

fn key(core: &mut Core, key: u8, pressed: bool, now: u32) {
    // Every physical matrix key can wake, including keys with no binding.
    // A bound key wakes through `apply`, which sees the screen still Off.
    let input = match keymap::map(key) {
        keymap::Mapped::Wheel(wheel) => {
            if pressed {
                core.repeat = Some((wheel, now, now.wrapping_add(keymap::REPEAT_DELAY_MS)));
            } else if core.repeat.map(|held| held.0) == Some(wheel) {
                core.repeat = None;
            }
            if !pressed {
                return;
            }
            Control::Wheel(wheel)
        }
        keymap::Mapped::Button(button) => {
            Control::Button(button, if pressed { Edge::Press } else { Edge::Release })
        }
        keymap::Mapped::None => {
            if pressed && (1..=80).contains(&key) {
                wake(core, 1);
            }
            return;
        }
    };
    apply(core, input);
}

fn wake(core: &mut Core, source: u8) {
    let before = core.power.screen();
    core.power.wake(core.now);
    core.redraw |= before != core.power.screen();
    if before == power::Screen::Off {
        unsafe { owt_input_wake_trace(source, core.now) };
    }
}

/// Debug UART wake uses the application queue and never emits a HID action.
#[no_mangle]
pub unsafe extern "C" fn owt_core_wake(now: u32) {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    core.now = now;
    wake(core, 2);
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_poll(now: u32) -> bool {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    core.now = now;
    let mut update = Update::new();
    let screen_on = core.power.screen() != power::Screen::Off;
    core.ui.tick(now, core.power.idle_ms(now), screen_on, &mut update);
    emit(core, &mut update);
    let keypad_irq = owt_keypad_asserted();
    if core.devices & 2 != 0
        && (keypad_irq || now.wrapping_sub(core.keypad_poll_at) >= core.power.poll_ms())
    {
        core.keypad_poll_at = now;
        let mut keypad = tca8418::Tca8418::new(Bus, 0x34);
        let mut acknowledge = keypad_irq;
        for _ in 0..16 {
            match keypad.next_event() {
                Ok(Some(e)) => {
                    core.keypad_failed = false;
                    acknowledge = true;
                    key(core, e.key, e.pressed, now);
                }
                Ok(None) => {
                    core.keypad_failed = false;
                    break;
                }
                Err(_) => {
                    // A key-up lost with the bus must not strand any hold.
                    // Once per outage: each release counts as UI input, and
                    // at the poll rate they would keep the UI from going quiet.
                    if !core::mem::replace(&mut core.keypad_failed, true) {
                        core.repeat = None;
                        for button in Button::ALL {
                            apply(core, Control::Button(button, Edge::Release));
                        }
                    }
                    break;
                }
            }
        }
        if acknowledge {
            let _ = keypad.acknowledge();
            // An event can arrive between the empty-FIFO read and clearing
            // INT. Recheck after acknowledgement and latch another wake so it
            // cannot be hidden until the 500ms dark-screen recovery poll.
            if keypad.pending().is_ok_and(|count| count != 0) {
                owt_keypad_retry();
            }
        }
    }
    // Rev A's direct inputs: an edge latched by the ISR, the decoder asking
    // for fast samples while something settles, or the ordinary poll.
    if let Some(decoder) = core.inputs.as_mut() {
        let irq = owt_inputs_asserted();
        if irq
            || decoder.busy(now)
            || now.wrapping_sub(core.inputs_poll_at) >= core.power.poll_ms()
        {
            core.inputs_poll_at = now;
            let events = decoder.sample(owt_inputs_read(), now);
            for control in events.iter() {
                apply(core, control);
            }
        }
    }
    if let Some((wheel, since, at)) = core.repeat {
        if now.wrapping_sub(since) > keymap::REPEAT_MAX_MS {
            core.repeat = None;
        } else if now.wrapping_sub(at) < u32::MAX / 2 {
            core.repeat = Some((wheel, since, now.wrapping_add(keymap::REPEAT_PERIOD_MS)));
            apply(core, Control::Wheel(wheel));
        }
    }
    // INT can pulse entirely during rendering or a host reply. Its ISR latches
    // that edge, and periodic reads also recover a contact when no IRQ arrives.
    let touch_interval = if core.contact.active() {
        20
    } else {
        core.power.poll_ms()
    };
    let touch_irq = owt_touch_asserted();
    if core.devices & 4 != 0
        && (touch_irq || now.wrapping_sub(core.touch_poll_at) >= touch_interval)
    {
        core.touch_poll_at = now;
        let report = cst9220::Cst9220::new(Bus).read_report();
        // A short wake tap can end before the report is read. Its latched
        // interrupt plus a valid release is still local wake evidence. An
        // empty fallback poll, malformed frame or I2C error is not.
        if touch_irq && report == Ok(cst9220::Report::Released) {
            wake(core, 0);
        }
        let touch = match report {
            Ok(cst9220::Report::NotReady) => None,
            Ok(report) => {
                core.touch_at = now;
                core.contact.sample(report)
            }
            Err(_) => core.contact.cancel(),
        };
        if let Some(touch) = touch {
            apply(core, Control::Touch(touch));
        }
    }
    if core.contact.active() && now.wrapping_sub(core.touch_at) >= 120 {
        if let Some(touch) = core.contact.cancel() {
            apply(core, Control::Touch(touch));
        }
    }
    let before = core.power.screen();
    if ui::state::arcade::playing(&core.ui) && core.power.screen() != power::Screen::Off { core.power.notify(now); }
    let screen = core.power.tick(now);
    core.redraw |= core.ui.set_screen_lock(core.power.lock_progress());
    core.redraw |= before != screen;
    let battery_due =
        !core.battery_sampled || now.wrapping_sub(core.battery_at) >= power::BATTERY_INTERVAL_MS;
    let fast_status =
        core.ui.battery.near_full() || core.battery_estimator.completion_since().is_some();
    if battery_due || fast_status && now.wrapping_sub(core.battery_status_at) >= 1000 {
        core.battery_status_at = now;
        // Keep watchdog servicing tied to its own regular deadline. Extra ADC
        // samples on Done transitions must not postpone it indefinitely.
        if battery_due && core.devices & 1 != 0 {
            owt_watchdog_result(charger::feed_watchdog(&mut Bus).is_ok());
        }
        let status = if core.devices & 1 != 0 {
            charger::read_reg(&mut Bus, charger::REG_STATUS).ok()
        } else {
            // Rev B: the BQ24073's CHG pin. This image has no USB device, so
            // a charge that has ended reads as no input (the estimate holds).
            #[cfg(feature = "hw-b")]
            {
                Some(
                    battery::ChargeState::from_chg_pin(owt_charger_charging(), false, None, 4200)
                        .as_register(),
                )
            }
            #[cfg(not(feature = "hw-b"))]
            {
                None
            }
        };
        let done_edge = core.battery_estimator.observe_charger(status, now);
        // Fast status observations never reuse cached voltage as a fresh
        // estimator sample. A new Done edge gets an immediate real ADC read.
        if battery_due || done_edge {
            if battery_due {
                core.battery_sampled = true;
                core.battery_at = now;
            }
            let mut state = battery::BatteryState::UNKNOWN;
            let mv = owt_battery_mv();
            state.millivolts = if (2500..=4500).contains(&mv) {
                Some(mv)
            } else {
                None
            };
            if let Some(raw) = status {
                state.charge = battery::ChargeState::from_register(raw);
            }
            if core.devices & 1 != 0 {
                if let Ok(reg) = charger::read_reg(&mut Bus, charger::REG_VOLTAGE) {
                    state.full_mv = charger::full_voltage_mv(reg);
                } else {
                    // An unread endpoint cannot qualify a full-charge event.
                    core.battery_estimator.observe_charger(None, now);
                }
            }
            let state = core.battery_estimator.update(state, now);
            owt_battery_trace(
                state.millivolts.unwrap_or(0),
                state.voltage_percentage().unwrap_or(255),
                state.percentage().unwrap_or(255),
                state.charge as u8,
            );
            core.redraw |= core.ui.update_battery(state);
        }
    }
    // Retain hidden host changes for one complete refresh on wake.
    screen != power::Screen::Off && core::mem::replace(&mut core.redraw, false)
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_screen() -> u8 {
    (*core::ptr::addr_of!(CORE)).power.screen() as u8
}

/// Panel level for the current screen state at the user's brightness
/// setting; main.c re-applies it whenever the value changes.
#[no_mangle]
pub unsafe extern "C" fn owt_core_brightness() -> u8 {
    let core = &*core::ptr::addr_of!(CORE);
    power::brightness(core.power.screen(), core.ui.user_brightness())
}

/// The wake frame is on the glass: end the hidden-frame gate. The screen
/// lock remains until four counterclockwise detents; toast time starts now.
#[no_mangle]
pub unsafe extern "C" fn owt_core_frame_ready() {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    core.power.frame_ready(core.now);
    core.ui.frame_shown(core.now);
}

/// The panel went to sleep: its wake waits for a whole frame
/// (`owt_core_frame_ready`); local input stays locked until the wheel unlocks.
#[no_mangle]
pub unsafe extern "C" fn owt_core_panel_slept() {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    core.power.slept();
    core.redraw |= core.ui.set_screen_lock(core.power.lock_progress());
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_wait_ms() -> u32 {
    let core = &*core::ptr::addr_of!(CORE);
    let screen = core.power.screen();
    if core.repeat.is_some() || core.inputs.as_ref().is_some_and(|d| d.busy(core.now)) {
        power::ACTIVE_POLL_MS
    } else if screen != power::Screen::Off && core.ui.animating() {
        core.power.poll_ms().min(screen.animation_ms()).min(if ui::state::arcade::playing(&core.ui) { 33 } else { 500 })
    } else {
        core.power.poll_ms()
    }
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_battery_percent() -> u8 {
    (*core::ptr::addr_of!(CORE))
        .ui
        .battery
        .percentage()
        .unwrap_or(0)
}

/// Private diagnostic payload; application thread only, no fresh ADC reads.
#[no_mangle]
pub unsafe extern "C" fn owt_core_battery_diagnostic(data: *mut u8) {
    if data.is_null() {
        return;
    }
    let core = &*core::ptr::addr_of!(CORE);
    let out = core::slice::from_raw_parts_mut(data, 9);
    out[0] = core.ui.battery.percentage().unwrap_or(255);
    out[1] = core.ui.battery.charge as u8;
    out[2..4].copy_from_slice(&core.ui.battery.full_mv.to_le_bytes());
    let since = core.battery_estimator.completion_since();
    out[4] = u8::from(since.is_some());
    out[5..9].copy_from_slice(
        &since
            .map_or(0, |at| core.now.wrapping_sub(at))
            .to_le_bytes(),
    );
}

/// Panel mounting of this build in degrees (0 LilyGo board, 180 Rev A and
/// Rev B hardware), so the boot log identifies which image is running.
#[no_mangle]
pub extern "C" fn owt_core_rotation() -> u16 {
    if orientation::ROTATE_180 {
        180
    } else {
        0
    }
}

/// The board this core was built for (0 LilyGo, 1 Rev A, 2 Rev B): the boot
/// log names it and the C side checks it against its own board define.
#[no_mangle]
pub extern "C" fn owt_core_board() -> u8 {
    orientation::BOARD_CODE
}

#[no_mangle]
pub extern "C" fn owt_core_splash_duration() -> u32 {
    splash::DURATION_MS
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_draw_splash_band(index: u16) -> *const u8 {
    if index >= display::BANDS {
        return core::ptr::null();
    }
    let band = &mut (*core::ptr::addr_of_mut!(BANDS))[0];
    // The C caller supplies a physical panel band, in native scan order.
    band.set_row(if orientation::ROTATE_180 {
        display::BANDS - 1 - index
    } else {
        index
    });
    splash::draw(band);
    band.orient_for_panel();
    band.data().as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_begin_frame(force: bool) -> bool {
    let frame = &mut *core::ptr::addr_of_mut!(FRAME);
    (*core::ptr::addr_of!(CORE)).ui.copy_into(frame);
    let next = &mut *core::ptr::addr_of_mut!(NEXT_SIG);
    scene::signature(frame, next);
    let previous = if !force && *core::ptr::addr_of!(FRAME_VALID) {
        Some(&*core::ptr::addr_of!(PREV_SIG))
    } else {
        None
    };
    let damage = damage::Damage::between(previous, next);
    let changed = !damage.is_empty();
    *core::ptr::addr_of_mut!(DAMAGE) = damage;
    next.copy_into(&mut *core::ptr::addr_of_mut!(PREV_SIG));
    *core::ptr::addr_of_mut!(FRAME_VALID) = true;
    changed
}

/// The application alternates buffers, and never reuses one until DMA completes.
#[no_mangle]
pub unsafe extern "C" fn owt_core_draw_next(slot: u8, region: *mut damage::Region) -> *const u8 {
    if slot >= 2 || region.is_null() {
        return core::ptr::null();
    }
    let Some(rect) = (*core::ptr::addr_of_mut!(DAMAGE)).next() else {
        return core::ptr::null();
    };
    *region = rect.panel();
    let band = &mut (*core::ptr::addr_of_mut!(BANDS))[slot as usize];
    band.set_row(rect.y0 / display::BAND_H);
    band.clip_to(embedded_graphics::primitives::Rectangle::new(
        embedded_graphics::prelude::Point::new(rect.x0 as i32, rect.y0 as i32),
        embedded_graphics::prelude::Size::new(
            (rect.x1 - rect.x0) as u32,
            (rect.y1 - rect.y0) as u32,
        ),
    ));
    scene::draw(band, &*core::ptr::addr_of!(FRAME));
    band.orient_for_panel();
    // Rotation stays inside the source rectangle. Its logical offset and
    // full-band stride still describe the DMA source, even though the panel
    // destination above has moved to the opposite side of the screen.
    let offset = ((rect.y0 - band.y0()) as usize * display::WIDTH as usize + rect.x0 as usize) * 2;
    band.data().as_ptr().add(offset)
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_hid_connected(connected: bool) {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    core.deck_rx = false;
    let mut update = Update::new();
    core.ui.set_connected(connected, &mut update);
    core.redraw |= update.dirty;
    core.rssi = ui::bluetooth::RssiFilter::new();
    core.redraw |= core.ui.update_bluetooth(if connected {
        ui::bluetooth::Status::Unknown
    } else {
        ui::bluetooth::Status::Disconnected
    });
    core.rx.clear();
    (*core::ptr::addr_of_mut!(LINE)).clear();
}

/// Called on the app thread after the BLE worker queues a measurement.
#[no_mangle]
pub unsafe extern "C" fn owt_core_bluetooth_rssi(rssi: i8) {
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    if core.ui.connected {
        core.redraw |= core.ui.update_bluetooth(core.rssi.sample(Some(rssi)));
    }
}

/// A channel-5 body, with or without its report id.
fn is_deck(body: &[u8]) -> bool {
    matches!(body, [proto::REPORT_ID, proto::CHANNEL, ..] | [proto::CHANNEL, ..])
}

/// Channel-5 bodies: every completed line is parsed into `HOST_MSG` and
/// applied (or rejected through the UI's `err` limiter), and its consequences
/// are emitted before the next line so one report cannot overflow an update.
fn deck_report(core: &mut Core, body: &[u8]) {
    let now = core.now;
    let line = unsafe { &mut *core::ptr::addr_of_mut!(LINE) };
    let mut update = Update::new();
    line.push(body, &mut |result| {
        core.deck_rx = true;
        let parsed = result
            .and_then(|line| proto::parse(line, unsafe { &mut *core::ptr::addr_of_mut!(HOST_MSG) }));
        match parsed {
            Ok(()) => core
                .ui
                .host(unsafe { &*core::ptr::addr_of!(HOST_MSG) }, now, &mut update),
            Err(why) => core.ui.reject(why, now, &mut update),
        }
        emit(core, &mut update);
        update.clear();
    });
}

#[no_mangle]
pub unsafe extern "C" fn owt_core_hid_report(data: *const u8, len: usize) {
    if data.is_null() || !(2..=wire::REPORT_LEN).contains(&len) {
        return;
    }
    let core = &mut *core::ptr::addr_of_mut!(CORE);
    let body = core::slice::from_raw_parts(data, len);
    if is_deck(body) {
        deck_report(core, body);
        return;
    }
    if let wire::Push::Complete(n) = core.rx.push(body) {
        let mut output = [0; wire::TX_CAP];
        let (_, len) = wire::handle_request(
            core.rx.data(n),
            "0.1.0-openwalkietalkie-hfp",
            orientation::BOARD,
            wire::Status {
                battery: core.ui.battery.percentage().unwrap_or(0),
                is_charging: core.ui.battery.charge == battery::ChargeState::Charging,
                recovery_protocol: None,
                maintenance: false,
            },
            &mut output,
        );
        core.rx.clear();
        if len > 0 {
            send_json(&output[..len]);
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { owt_platform_panic() }
}
