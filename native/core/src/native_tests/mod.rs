//! End-to-end harness for the C-facing core: extern "C" board mocks, the
//! shared CORE lock, `reset()` and the HID report decoder. Tests live in the
//! child modules, one owner per file.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};

mod deck;
mod io;
mod power_battery;
mod preferences;

static CORE_LOCK: Mutex<()> = Mutex::new(());
static REPLIES: AtomicUsize = AtomicUsize::new(0);
static REPORTS: Mutex<std::vec::Vec<std::vec::Vec<u8>>> = Mutex::new(std::vec::Vec::new());
static TOUCH_REPORTS: Mutex<std::collections::VecDeque<[u8; 15]>> =
    Mutex::new(std::collections::VecDeque::new());
static KEY_EVENTS: Mutex<std::collections::VecDeque<u8>> =
    Mutex::new(std::collections::VecDeque::new());
static TOUCH_IRQ: AtomicBool = AtomicBool::new(false);
static KEYPAD_IRQ: AtomicBool = AtomicBool::new(false);
static DIRECT_IRQ: AtomicBool = AtomicBool::new(false);
static DIRECT_RAW: AtomicUsize = AtomicUsize::new(0xff);
static INPUT_READS: AtomicUsize = AtomicUsize::new(0);
static KEY_ON_ACK: AtomicBool = AtomicBool::new(false);
static KEYPAD_BUS_FAILS: AtomicBool = AtomicBool::new(false);
static BATTERY_MV: AtomicUsize = AtomicUsize::new(4200);
static CHARGER_STATUS: AtomicUsize = AtomicUsize::new(0);
static BATTERY_READS: AtomicUsize = AtomicUsize::new(0);
static WATCHDOG_FEEDS: AtomicUsize = AtomicUsize::new(0);
static UI_PREFS: Mutex<Option<[u8; bond_record::SIZE]>> = Mutex::new(None);
static UI_PREFS_WRITES: AtomicUsize = AtomicUsize::new(0);

/// One failed test must not bury the others under PoisonError panics.
fn lock<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Fresh core, empty mock queues. Hold `CORE_LOCK` for the whole test first.
fn reset() {
    BATTERY_MV.store(4200, Ordering::Relaxed);
    CHARGER_STATUS.store(0, Ordering::Relaxed);
    BATTERY_READS.store(0, Ordering::Relaxed);
    WATCHDOG_FEEDS.store(0, Ordering::Relaxed);
    *lock(&UI_PREFS) = None;
    UI_PREFS_WRITES.store(0, Ordering::Relaxed);
    unsafe {
        *core::ptr::addr_of_mut!(CORE) = Core::new();
        (*core::ptr::addr_of_mut!(CORE)).ui.reset();
        *core::ptr::addr_of_mut!(FRAME_VALID) = false;
        *core::ptr::addr_of_mut!(DAMAGE) = damage::Damage::empty();
    }
    lock(&REPORTS).clear();
    lock(&TOUCH_REPORTS).clear();
    lock(&KEY_EVENTS).clear();
    TOUCH_IRQ.store(false, Ordering::Relaxed);
    KEYPAD_IRQ.store(false, Ordering::Relaxed);
    DIRECT_IRQ.store(false, Ordering::Relaxed);
    DIRECT_RAW.store(0xff, Ordering::Relaxed);
    INPUT_READS.store(0, Ordering::Relaxed);
    KEY_ON_ACK.store(false, Ordering::Relaxed);
    KEYPAD_BUS_FAILS.store(false, Ordering::Relaxed);
    REPLIES.store(0, Ordering::Relaxed);
}

/// Feed one host line as the report bodies the C side would queue (64 bytes,
/// report id included), the way `tools/deck-sim.py` frames them.
fn host_line(line: &[u8]) {
    let mut offset = 0;
    let mut report = [0; proto::REPORT_LEN];
    loop {
        let used = proto::frame(line, offset, &mut report);
        if used == 0 {
            break;
        }
        offset += used;
        unsafe { owt_core_hid_report(report.as_ptr(), report.len()) };
    }
}

/// Drain the notified reports into (channel-2, channel-5) JSON lines.
fn drained() -> (std::vec::Vec<std::string::String>, std::vec::Vec<std::string::String>) {
    let mut rpc = std::vec::Vec::new();
    let mut deck = std::vec::Vec::new();
    for report in lock(&REPORTS).drain(..) {
        assert_eq!(report.len(), 63);
        let len = report[1] as usize;
        assert!(len <= proto::PAYLOAD_MAX);
        match report[0] {
            wire::MSG_TYPE => rpc.extend_from_slice(&report[2..2 + len]),
            proto::CHANNEL => deck.extend_from_slice(&report[2..2 + len]),
            other => panic!("report on channel {other}"),
        }
    }
    let lines = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(std::string::String::from)
            .collect::<std::vec::Vec<_>>()
    };
    (lines(&rpc), lines(&deck))
}

/// Channel-5 lines only; any channel-2 traffic is a failure.
fn deck_lines() -> std::vec::Vec<std::string::String> {
    let (rpc, deck) = drained();
    assert!(rpc.is_empty(), "unexpected channel-2 traffic: {rpc:?}");
    deck
}

/// One system request on channel 2, in a single 64-byte report.
fn rpc_request(json: &[u8]) {
    let mut report = [0; wire::REPORT_LEN];
    assert!(wire::frame(json, 0, &mut report) == json.len() + 1, "one report");
    unsafe { owt_core_hid_report(report.as_ptr(), report.len()) };
}

const HELLO: &[u8] = br#"{"m":"hello","v":1,"sid":7,"hm":"19:42"}"#;

/// One keypad edge through the TCA8418 mock, then a poll at `now`.
fn key(number: u8, pressed: bool, now: u32) {
    lock(&KEY_EVENTS).push_back(if pressed { number | 0x80 } else { number });
    KEYPAD_IRQ.store(true, Ordering::Relaxed);
    unsafe { owt_core_poll(now) };
}

const AGENT: &[u8] = br#"{"m":"agent","id":1,"kind":"claude","name":"web-ui","state":"working"}"#;

/// A fresh core with the link up and a deck host that has said hello and
/// announced agent 1 (the active channel), on `template`'s face. Idle-home is
/// off so the clock jumps of the power tests keep the face. Drained.
fn hosted(template: crate::deck::msg::Template) -> &'static mut Core {
    reset();
    unsafe {
        let core = &mut *core::ptr::addr_of_mut!(CORE);
        owt_core_hid_connected(true);
        host_line(HELLO);
        host_line(AGENT);
        core.ui.deck.settings.idle_home_s = 0;
        core.ui.global.active_channel = 1;
        core.ui.on_face(template);
        drained();
        core
    }
}

/// The host's heartbeat, stamped `at`: keeps it present across a clock jump.
fn heartbeat(core: &mut Core, at: u32) {
    core.now = at;
    host_line(HELLO);
    drained();
}

/// A probe point whose raw frames are pinned below.
const PROBE: (u16, u16) = (186, 294);

/// Raw panel position that this build's mounting maps to logical (x, y).
fn raw_point((x, y): (u16, u16)) -> (u16, u16) {
    orientation::point(x, y).unwrap() // Either mounting is its own inverse.
}

/// One-contact CST9220 report landing on logical (x, y).
fn touch_frame(logical: (u16, u16)) -> [u8; 15] {
    let (x, y) = raw_point(logical);
    let mut bytes = [0; 15];
    bytes[..7].copy_from_slice(&[
        6,
        (x >> 4) as u8,
        (y >> 4) as u8,
        ((x & 15) << 4 | (y & 15)) as u8,
        0x20,
        1,
        0xab,
    ]);
    bytes
}

/// Empty release frame; its coordinate slot retains the last contact.
fn release_frame(logical: (u16, u16)) -> [u8; 15] {
    let mut bytes = touch_frame(logical);
    bytes[0] = 0;
    bytes[5] = 0;
    bytes
}

#[test]
fn touch_frames_encode_the_mounted_raw_position() {
    // The half-turned panel reports the probe at raw (293, 185).
    let turned = [6, 18, 11, 0x59, 0x20, 1, 0xab, 0, 0, 0, 0, 0, 0, 0, 0];
    let upright = [6, 11, 18, 0xa6, 0x20, 1, 0xab, 0, 0, 0, 0, 0, 0, 0, 0];
    let expected = if orientation::ROTATE_180 {
        turned
    } else {
        upright
    };
    assert_eq!(touch_frame(PROBE), expected);
    let (x, y) = raw_point(PROBE);
    assert_eq!(
        cst9220::Report::decode(expected),
        cst9220::Report::Contact { x, y }
    );
    assert_eq!(
        cst9220::Report::decode(release_frame(PROBE)),
        cst9220::Report::Released
    );
}

// ---- board mocks ------------------------------------------------------------

#[no_mangle]
unsafe extern "C" fn owt_i2c_transfer(
    address: u8,
    tx: *const u8,
    tx_len: usize,
    rx: *mut u8,
    len: usize,
) -> i32 {
    if address == tca8418::ADDR && KEYPAD_BUS_FAILS.load(Ordering::Relaxed) {
        return 1;
    }
    if address == tca8418::ADDR
        && tx_len == 2
        && *tx == 2
        && KEY_ON_ACK.swap(false, Ordering::Relaxed)
    {
        lock(&KEY_EVENTS).extend([44 | 0x80, 44]);
    }
    if len != 0 {
        let bytes = core::slice::from_raw_parts_mut(rx, len);
        bytes.fill(0);
        if address == cst9220::ADDRESS && len == 15 {
            INPUT_READS.fetch_add(1, Ordering::Relaxed);
            bytes[0] = 0xab; // No fresh touch report.
            if let Some(report) = lock(&TOUCH_REPORTS).pop_front() {
                bytes.copy_from_slice(&report);
            }
        } else if address == tca8418::ADDR && tx_len == 1 && *tx == 4 {
            INPUT_READS.fetch_add(1, Ordering::Relaxed);
            bytes[0] = lock(&KEY_EVENTS).pop_front().unwrap_or(0);
        } else if address == tca8418::ADDR && tx_len == 1 && *tx == 3 {
            bytes[0] = lock(&KEY_EVENTS).len() as u8;
        } else if address == charger::ADDRESS && tx_len == 1 && *tx == charger::REG_STATUS {
            bytes[0] = CHARGER_STATUS.load(Ordering::Relaxed) as u8;
        } else if address == charger::ADDRESS && tx_len == 1 && *tx == charger::REG_VOLTAGE {
            bytes[0] = 0x8c; // 4.2V, as configured by charger::initialize.
        }
    }
    0
}
#[no_mangle]
extern "C" fn owt_delay_ms(_: u32) {}
#[no_mangle]
unsafe extern "C" fn owt_ui_prefs_read(bytes: *mut u8, length: usize) -> bool {
    let Some(saved) = *lock(&UI_PREFS) else { return false };
    assert_eq!(length, saved.len());
    core::slice::from_raw_parts_mut(bytes, length).copy_from_slice(&saved);
    true
}
#[no_mangle]
unsafe extern "C" fn owt_ui_prefs_write(bytes: *const u8, length: usize) -> bool {
    assert_eq!(length, bond_record::SIZE);
    *lock(&UI_PREFS) = Some(core::slice::from_raw_parts(bytes, length).try_into().unwrap());
    UI_PREFS_WRITES.fetch_add(1, Ordering::Relaxed);
    true
}
#[no_mangle]
unsafe extern "C" fn owt_hid_notify(data: *const u8, length: usize) -> i32 {
    REPLIES.fetch_add(1, Ordering::Relaxed);
    lock(&REPORTS).push(core::slice::from_raw_parts(data, length).to_vec());
    0
}
#[no_mangle]
extern "C" fn owt_touch_asserted() -> bool {
    TOUCH_IRQ.swap(false, Ordering::Relaxed)
}
#[no_mangle]
extern "C" fn owt_keypad_asserted() -> bool {
    KEYPAD_IRQ.swap(false, Ordering::Relaxed)
}
#[no_mangle]
extern "C" fn owt_inputs_asserted() -> bool {
    DIRECT_IRQ.swap(false, Ordering::Relaxed)
}
#[no_mangle]
extern "C" fn owt_inputs_read() -> u8 {
    DIRECT_RAW.load(Ordering::Relaxed) as u8
}
#[no_mangle]
extern "C" fn owt_keypad_retry() {
    KEYPAD_IRQ.store(true, Ordering::Relaxed);
}
#[no_mangle]
extern "C" fn owt_watchdog_result(ok: bool) {
    assert!(ok);
    WATCHDOG_FEEDS.fetch_add(1, Ordering::Relaxed);
}
#[no_mangle]
extern "C" fn owt_touch_trace(_: u8, _: u16, _: u16, _: bool) {}
#[no_mangle]
extern "C" fn owt_input_wake_trace(_: u8, _: u32) {}
#[no_mangle]
extern "C" fn owt_battery_trace(_: u16, _: u8, _: u8, _: u8) {}
#[no_mangle]
extern "C" fn owt_battery_mv() -> u16 {
    BATTERY_READS.fetch_add(1, Ordering::Relaxed);
    BATTERY_MV.load(Ordering::Relaxed) as u16
}
/// Rev B's CHG pin: the tests' deck is never on a charger.
#[cfg(feature = "hw-b")]
#[no_mangle]
extern "C" fn owt_charger_charging() -> bool {
    false
}
