//! The deck's HID transport: one vendor collection (`wire::REPORT_DESC`),
//! served over two links: BLE (HID over GATT, both boards) and, on Rev A,
//! the SF32's USB port (`usb`). Report channel 2 carries the system RPC
//! (`wire`), channel 5 the deck's JSON lines (`crate::deck::proto`), and,
//! on the cable only, channel 7 the voice frames (`crate::audio::codec`:
//! what the audio service's characteristics carry over Bluetooth, one frame
//! a report). The deck is never a keyboard (see `wire::REPORT_DESC`).
//!
//! Each link owns an ordered outbound queue (`EVENTS[link]`): a deck
//! `channel` must precede its `ptt`. A deck line is encoded by the app task
//! (`hello`/`faces` need the UI state) into a static slot (`DECK_LINES`) and
//! the queue carries the slot index, so nothing the size of a line crosses a
//! task's stack. Overflow of a queue or the slots, or of an `Update`, sends
//! the deck `up` and drops that link so its host resyncs. Inbound channel-5
//! lines are parsed by the link task and handed to the app through `DECK_RX`
//! with `send().await`: a full sync is ~110 reports and must back-pressure
//! the link rather than lose state.
//!
//! Unsolicited traffic goes to the link whose host has spoken on that
//! channel, USB first (`Link::PREFERENCE`): a host that talks over the
//! cable is answered over the cable even while the Bluetooth host is still
//! connected. Replies always return on the link the request came in on.

mod bond_flash;
mod bond_record;
mod nor_flash;
pub use bond_record::Prefs;
#[cfg(feature = "hw-a")]
mod boot_info;
#[cfg(feature = "bootloader-update")]
mod boot_update;
#[cfg(feature = "audio-debug")]
pub mod debug;
#[cfg(feature = "panel-debug")]
mod panel_debug;
#[cfg(feature = "hw-a")]
pub mod usb;
#[allow(dead_code)]
pub mod wire;

use crate::{
    battery::{BatteryState, ChargeState},
    deck::{
        msg::{ErrWhy, Event, HostMsg},
        proto,
    },
    logln,
    ui::{
        bluetooth::{RssiFilter, Status as BluetoothStatus},
        UiState,
    },
};
use core::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicBool, Ordering},
};
use embassy_futures::{
    join::join,
    select::{select, select3, select4, Either, Either3, Either4},
};
use embassy_sync::{
    blocking_mutex::{raw::CriticalSectionRawMutex, Mutex},
    channel::Channel,
    signal::Signal,
};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use sifli_hal::{bind_interrupts, ipc, peripherals, rng::Rng};
use sifli_radio::bluetooth::{BleController, BleInitConfig};
use trouble_host::prelude::*;

pub struct RadioPeripherals {
    pub lcpu: peripherals::LCPU,
    pub mailbox: peripherals::MAILBOX1_CH1,
    pub dma: peripherals::DMAC2_CH8,
    pub rng: peripherals::TRNG,
}
bind_interrupts!(struct Irqs { MAILBOX2_CH1 => ipc::InterruptHandler; });

/// The two ways a host reaches the vendor collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Ble = 0,
    Usb = 1,
}

impl Link {
    pub const COUNT: usize = 2;
    /// Where unsolicited traffic goes when more than one host has spoken:
    /// the cable first.
    pub const PREFERENCE: [Link; Self::COUNT] = [Link::Usb, Link::Ble];

    pub const fn index(self) -> usize {
        self as usize
    }

    fn other(self) -> Link {
        match self {
            Link::Ble => Link::Usb,
            Link::Usb => Link::Ble,
        }
    }
}

struct Flags {
    /// BLE: connected; USB: configured and not suspended.
    up: AtomicBool,
    /// Channel 5: a line was received on this link (the device never speaks
    /// first there; `hello` is answered in the same `UiState::host` call).
    deck_spoke: AtomicBool,
}
impl Flags {
    const NEW: Flags = Flags {
        up: AtomicBool::new(false),
        deck_spoke: AtomicBool::new(false),
    };
}
static FLAGS: [Flags; Link::COUNT] = [Flags::NEW; Link::COUNT];

fn flags(link: Link) -> &'static Flags {
    &FLAGS[link.index()]
}

/// Whether any link is up, for the UI.
pub static UI_STATUS: Signal<CriticalSectionRawMutex, bool> = Signal::new();
/// BLE has its own signal: USB remains usable when Bluetooth disconnects.
pub static UI_BLUETOOTH: Signal<CriticalSectionRawMutex, BluetoothStatus> = Signal::new();
/// Parsed channel-5 lines (or why one could not be), for the app task.
pub static DECK_RX: Channel<CriticalSectionRawMutex, Result<HostMsg, ErrWhy>, 8> = Channel::new();

fn any_up() -> bool {
    FLAGS.iter().any(|f| f.up.load(Ordering::Acquire))
}

/// Whether the USB link is up (a host has configured the device). Rev B's
/// battery task reads it: the chip's VBUS pin is not wired, so this is the
/// one sign of a supply behind the charger's CHG output.
#[cfg(feature = "hw-b")]
pub fn usb_up() -> bool {
    flags(Link::Usb).up.load(Ordering::Acquire)
}

/// The preferred link whose deck host has spoken, if any.
fn deck_link() -> Option<Link> {
    Link::PREFERENCE
        .into_iter()
        .find(|link| flags(*link).deck_spoke.load(Ordering::Acquire))
}

/// True once the deck host has spoken on some link.
pub fn deck_spoke() -> bool {
    deck_link().is_some()
}

/// A link came up.
fn link_up(link: Link) {
    if flags(link).up.swap(true, Ordering::AcqRel) {
        return;
    }
    if link == Link::Ble {
        UI_BLUETOOTH.signal(BluetoothStatus::Unknown);
    }
    UI_STATUS.signal(true);
}

/// Forget a link's host and everything queued for it. Inbound deck lines are
/// kept while the other link is up: they may be its.
fn reset_hosts(link: Link) {
    flags(link).deck_spoke.store(false, Ordering::Release);
    clear_outbound(link);
    REPLIES[link.index()].clear();
    OVERFLOW[link.index()].reset();
    if !flags(link.other()).up.load(Ordering::Acquire) {
        DECK_RX.clear();
    }
}

/// A link went down: BLE disconnected, USB unconfigured or suspended.
fn link_down(link: Link) {
    let was_up = flags(link).up.swap(false, Ordering::AcqRel);
    if link == Link::Ble {
        UI_BLUETOOTH.signal(BluetoothStatus::Disconnected);
    }
    crate::audio::voice::link_lost(link);
    reset_hosts(link);
    if !was_up {
        return;
    }
    UI_STATUS.signal(any_up());
}

/// Queue one deck event, encoded now (lists come from `ui`). An event that
/// does not fit is dropped whole: a cut line must never reach the host.
pub fn deck_event(event: Event, ui: &UiState) {
    let Some(link) = deck_link() else {
        return;
    };
    let slot = DECK_LINES.lock(|lines| {
        let mut lines = lines.borrow_mut();
        let Some(slot) = (0..DECK_SLOTS).find(|slot| lines.used & (1 << slot) == 0) else {
            return Err(());
        };
        let line = &mut lines.lines[slot];
        line.len = proto::encode(event, ui, &mut line.bytes) as u16;
        if line.len == 0 {
            return Ok(None);
        }
        lines.used |= 1 << slot;
        Ok(Some(slot as u8))
    });
    match slot {
        Ok(Some(slot)) => {
            if EVENTS[link.index()].try_send(slot).is_err() {
                free_deck_slot(slot);
                OVERFLOW[link.index()].signal(());
            }
        }
        Ok(None) => {}
        Err(()) => OVERFLOW[link.index()].signal(()),
    }
}

/// An `Update` lost an event: recover as for a full queue, on every link
/// that carries a host.
pub fn overflow() {
    for link in Link::PREFERENCE {
        if flags(link).deck_spoke.load(Ordering::Acquire) {
            OVERFLOW[link.index()].signal(());
        }
    }
}

/// `DECK_LINES` slots, each freed once sent.
type EventQueue = Channel<CriticalSectionRawMutex, u8, 32>;
static EVENTS: [EventQueue; Link::COUNT] = [Channel::new(), Channel::new()];
type OverflowSignal = Signal<CriticalSectionRawMutex, ()>;
static OVERFLOW: [OverflowSignal; Link::COUNT] = [Signal::new(), Signal::new()];

struct DeckLine {
    len: u16,
    bytes: [u8; proto::TX_MAX],
}
/// One `Update` can queue at most 8 deck events; more waiting means the link
/// is stalled and the overflow rule applies.
const DECK_SLOTS: usize = 8;
struct DeckLines {
    lines: [DeckLine; DECK_SLOTS],
    used: u8,
}
const EMPTY_LINE: DeckLine = DeckLine {
    len: 0,
    bytes: [0; proto::TX_MAX],
};
static DECK_LINES: Mutex<CriticalSectionRawMutex, RefCell<DeckLines>> =
    Mutex::new(RefCell::new(DeckLines {
        lines: [EMPTY_LINE; DECK_SLOTS],
        used: 0,
    }));

fn free_deck_slot(slot: u8) {
    DECK_LINES.lock(|lines| lines.borrow_mut().used &= !(1 << slot));
}

/// Drop what one link had queued, giving its deck slots back; the other
/// link's slots stay reserved.
fn clear_outbound(link: Link) {
    while let Ok(slot) = EVENTS[link.index()].try_receive() {
        free_deck_slot(slot);
    }
}
static BATTERY: Mutex<CriticalSectionRawMutex, Cell<BatteryState>> =
    Mutex::new(Cell::new(BatteryState::UNKNOWN));
struct ReplyMessage {
    bytes: [u8; wire::TX_CAP],
    len: usize,
}
type ReplyQueue = Channel<CriticalSectionRawMutex, ReplyMessage, 4>;
static REPLIES: [ReplyQueue; Link::COUNT] = [Channel::new(), Channel::new()];

pub fn battery(state: BatteryState) {
    BATTERY.lock(|b| b.set(state));
}

/// The device's identity, as the Device Information service, the GAP name
/// and the advertisement give it (and as `usb` describes itself).
pub(crate) const MANUFACTURER: &[u8; 5] = b"Cohdl";
pub(crate) const NAME: &[u8; 16] = b"OpenWalkieTalkie";

#[gatt_server]
struct Server {
    _information: DeviceInformation,
    battery: BatteryService,
    hid: HidService,
    audio: AudioService,
}

#[gatt_service(uuid = "180a")]
struct DeviceInformation {
    #[characteristic(uuid = "2a29", read, value = *MANUFACTURER)]
    manufacturer: [u8; 5],
    #[characteristic(uuid = "2a24", read, value = *NAME)]
    model: [u8; 16],
    // USB VID source, VID 1209, PID 5754, wireless release 0101 (little-endian).
    #[characteristic(uuid = "2a50", read, value = [2, 0x09, 0x12, 0x54, 0x57, 1, 1])]
    pnp: [u8; 7],
}
#[gatt_service(uuid = "180f")]
struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify, value = 0)]
    level: u8,
}
#[gatt_service(uuid = "1812")]
struct HidService {
    #[characteristic(uuid = "2a4a", read, value = [0x11, 0x01, 0, 2])]
    information: [u8; 4],
    #[characteristic(uuid = "2a4b", read, value = wire::REPORT_DESC)]
    report_map: &'static [u8],
    #[characteristic(uuid = "2a4c", write_without_response)]
    control_point: u8,
    #[characteristic(uuid = "2a4e", read, write_without_response, value = 1)]
    protocol_mode: u8,
    #[characteristic(uuid = "2a4d", read, notify, value = [0; 63], permissions(read = encrypted, cccd = encrypted))]
    #[descriptor(uuid = "2908", read, value = [6u8, 1u8])]
    input: [u8; 63],
    // 64, not the report's 63: macOS keeps the report-id byte in its GATT
    // write (`dispatch_frame` takes either form), and a 64-byte write into a
    // 63-byte value fails with "invalid offset" before it ever reaches us.
    #[characteristic(uuid = "2a4d", read, write, write_without_response, value = [0; 64], permissions(encrypted))]
    #[descriptor(uuid = "2908", read, value = [6u8, 2u8])]
    output: [u8; 64],
}

/// The deck's voice link (`docs/deck-protocol.md`, "Voice"): microphone
/// frames out as notifications, speaker frames in as writes, both one frame
/// a value. Encrypted like the HID reports; `audio-open` drops that for
/// bench tests against a Mac that has not paired this firmware.
#[cfg(not(feature = "audio-open"))]
#[gatt_service(uuid = "e9895507-bb72-45e5-a802-ab38fe1e4bc1")]
struct AudioService {
    #[characteristic(uuid = "6e25df5b-b48a-4203-bb04-f3d4caeb1b7a", notify, value = heapless::Vec::new(), permissions(cccd = encrypted))]
    mic: heapless::Vec<u8, { crate::audio::codec::MAX_FRAME }>,
    #[characteristic(uuid = "84de7298-ab42-46e4-9c02-f6a65d990807", write, write_without_response, value = heapless::Vec::new(), permissions(write = encrypted))]
    speaker: heapless::Vec<u8, { crate::audio::codec::MAX_FRAME }>,
    #[characteristic(uuid = "a2259f3a-8d16-4c1e-a243-78f7206e35f5", read, value = AUDIO_INFO)]
    info: [u8; 5],
}
#[cfg(feature = "audio-open")]
#[gatt_service(uuid = "e9895507-bb72-45e5-a802-ab38fe1e4bc1")]
struct AudioService {
    #[characteristic(uuid = "6e25df5b-b48a-4203-bb04-f3d4caeb1b7a", notify, value = heapless::Vec::new())]
    mic: heapless::Vec<u8, { crate::audio::codec::MAX_FRAME }>,
    #[characteristic(uuid = "84de7298-ab42-46e4-9c02-f6a65d990807", write, write_without_response, value = heapless::Vec::new())]
    speaker: heapless::Vec<u8, { crate::audio::codec::MAX_FRAME }>,
    #[characteristic(uuid = "a2259f3a-8d16-4c1e-a243-78f7206e35f5", read, value = AUDIO_INFO)]
    info: [u8; 5],
}

/// Version, codec bits, rate (kHz), frame (ms), features: bit 0 a
/// microphone, bit 1 a speaker. The dev board has neither wired up.
const AUDIO_INFO: [u8; 5] = [
    1,
    1 << (crate::audio::codec::CODEC_IMA_ADPCM - 1),
    crate::audio::codec::RATE_KHZ,
    crate::audio::codec::FRAME_MS,
    if cfg!(feature = "hw-a") { 0b11 } else { 0 },
];
/// The audio service UUID as it goes over the air (little-endian), for the
/// scan response: a host that has not paired can still find the deck.
const AUDIO_SERVICE_UUID_LE: [u8; 16] = [
    0xc1, 0x4b, 0x1e, 0xfe, 0x38, 0xab, 0x02, 0xa8, 0xe5, 0x45, 0x72, 0xbb, 0x07, 0x55, 0x89, 0xe9,
];

#[embassy_executor::task]
pub async fn task(p: RadioPeripherals, address: [u8; 6]) {
    logln!("link: controller sleep disabled (connection timing workaround)");
    logln!("link: starting SF32 Bluetooth controller");
    let controller: BleController = match BleController::new(
        p.lcpu,
        p.mailbox,
        p.dma,
        Irqs,
        // With this HAL/ROM configuration, controller sleep loses connection
        // events (0x3E/0x08), even after explicitly starting Y6 and seeing LXT
        // ready. Keeping it awake is verified to pair and exchange HID RPCs.
        &BleInitConfig::default().bd_addr(address).pm_enabled(false),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            logln!("link: controller startup failed: {:?}", e);
            return;
        }
    };
    let mut rng = Rng::new_blocking(p.rng);
    let mut resources: HostResources<DefaultPacketPool, 1, 2> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(Address::random(address))
        .set_random_generator_seed(&mut rng);
    let mut saved_bond = bond_flash::load();
    if let Some(record) = &saved_bond {
        match stack.add_bond_information(bond_information(record)) {
            Ok(()) => logln!("link: restored pairing key from flash"),
            Err(e) => logln!("link: could not restore pairing: {:?}", e),
        }
    } else {
        logln!("link: no stored pairing; pair from Bluetooth settings");
    }
    let Host {
        mut peripheral,
        mut runner,
        ..
    } = stack.build();
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "OpenWalkieTalkie",
        appearance: &appearance::human_interface_device::GENERIC_HUMAN_INTERFACE_DEVICE,
    }))
    .unwrap();
    join(
        async {
            loop {
                if let Err(e) = runner.run().await {
                    logln!("link: runner error: {:?}", e);
                }
            }
        },
        async {
            loop {
                link_down(Link::Ble);
                let mut adv_data = [0; 31];
                let len = AdStructure::encode_slice(
                    &[
                        AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                        AdStructure::ServiceUuids16(&[[0x12, 0x18], [0x0f, 0x18]]),
                        AdStructure::CompleteLocalName(NAME),
                    ],
                    &mut adv_data,
                )
                .unwrap();
                let mut scan_data = [0; 31];
                let scan_len = AdStructure::encode_slice(
                    &[AdStructure::ServiceUuids128(&[AUDIO_SERVICE_UUID_LE])],
                    &mut scan_data,
                )
                .unwrap();
                let params = AdvertisementParameters {
                    interval_min: Duration::from_millis(100),
                    interval_max: Duration::from_millis(100),
                    ..Default::default()
                };
                let advertising = match peripheral
                    .advertise(
                        &params,
                        Advertisement::ConnectableScannableUndirected {
                            adv_data: &adv_data[..len],
                            scan_data: &scan_data[..scan_len],
                        },
                    )
                    .await
                {
                    Ok(a) => a,
                    Err(e) => {
                        logln!("link: advertising failed: {:?}", e);
                        Timer::after_secs(1).await;
                        continue;
                    }
                };
                logln!("link: advertising as OpenWalkieTalkie");
                let connection = match advertising.accept().await {
                    Ok(c) => c,
                    Err(e) => {
                        logln!("link: connection failed: {:?}", e);
                        continue;
                    }
                };
                let _ = connection.set_bondable(true);
                let conn = match connection.with_attribute_server(&server) {
                    Ok(c) => c,
                    Err(e) => {
                        logln!("link: GATT setup failed: {:?}", e);
                        continue;
                    }
                };
                if let Some(record) = saved_bond.as_ref().filter(|r| {
                    bond_information(r)
                        .identity
                        .match_identity(&conn.raw().peer_identity())
                }) {
                    server.set_cccd_table(
                        conn.raw(),
                        CccdTable::new([
                            (
                                server.battery.level.cccd_handle.unwrap(),
                                u16::from(record.battery_notify).into(),
                            ),
                            (
                                server.hid.input.cccd_handle.unwrap(),
                                u16::from(record.hid_notify).into(),
                            ),
                            (server.audio.mic.cccd_handle.unwrap(), 0u16.into()),
                        ]),
                    );
                }
                logln!("link: BLE connected");
                link_up(Link::Ble);
                let mut sink = BleSink {
                    server: &server,
                    conn: &conn,
                };
                select4(
                    gatt_task(&server, &conn, &mut saved_bond),
                    transmit_task(Link::Ble, &mut sink),
                    voice_task(&stack, &server, &conn),
                    rssi_task(&stack, &conn),
                )
                .await;
                conn.raw().disconnect();
                link_down(Link::Ble);
                logln!("link: disconnected; advertising again");
            }
        },
    )
    .await;
}

/// Cancelled along with the other connection tasks on disconnect. A failed
/// sample hides the old bars but must never terminate an otherwise good link.
async fn rssi_task(
    stack: &Stack<'_, BleController, DefaultPacketPool>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
) {
    let mut filter = RssiFilter::new();
    loop {
        let sample = with_timeout(Duration::from_secs(2), conn.raw().rssi(stack))
            .await
            .ok()
            .and_then(Result::ok);
        UI_BLUETOOTH.signal(filter.sample(sample));
        Timer::after_secs(3).await;
    }
}

async fn gatt_task(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    saved: &mut Option<bond_record::Record>,
) {
    let mut inbound = Inbound::new();
    let mut speaker_drops = 0u32;
    let mut burst = Burst::new();
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                logln!("link: disconnect reason {:?}", reason);
                return;
            }
            GattConnectionEvent::PairingComplete { bond, .. } => {
                logln!("link: pairing complete (bond={})", bond.is_some());
                if let Some(bond) = bond.filter(|b| b.is_bonded && b.security_level.encrypted()) {
                    let (hid_notify, battery_notify) = notification_preferences(server, conn);
                    persist_bond(
                        saved,
                        bond_record::Record {
                            address: bond.identity.bd_addr.0,
                            ltk: bond.ltk.0.to_le_bytes(),
                            irk: bond.identity.irk.map(|key| key.0.to_le_bytes()),
                            authenticated: bond.security_level.authenticated(),
                            hid_notify,
                            battery_notify,
                        },
                    );
                }
            }
            GattConnectionEvent::PairingFailed(e) => logln!("link: pairing failed: {:?}", e),
            GattConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
                logln!("link: PHY tx {:?} rx {:?}", tx_phy, rx_phy)
            }
            GattConnectionEvent::DataLengthUpdated {
                max_tx_octets,
                max_rx_octets,
                ..
            } => logln!(
                "link: data length tx {} rx {} bytes (ATT MTU {})",
                max_tx_octets,
                max_rx_octets,
                conn.raw().att_mtu()
            ),
            GattConnectionEvent::ConnectionParamsUpdated {
                conn_interval,
                peripheral_latency,
                supervision_timeout,
            } => logln!(
                "link: connection interval {} us, latency {}, timeout {} ms",
                conn_interval.as_micros(),
                peripheral_latency,
                supervision_timeout.as_millis()
            ),
            GattConnectionEvent::Gatt { event } => {
                let mut frame = [0u8; 64];
                let mut len = 0;
                let mut cccd_written = false;
                let mut mic_cccd = false;
                if let GattEvent::Write(e) = &event {
                    cccd_written = Some(e.handle()) == server.hid.input.cccd_handle
                        || Some(e.handle()) == server.battery.level.cccd_handle;
                    mic_cccd = Some(e.handle()) == server.audio.mic.cccd_handle;
                    if e.handle() == server.hid.output.handle && e.data().len() <= frame.len() {
                        len = e.data().len();
                        frame[..len].copy_from_slice(e.data());
                        if burst.report(Instant::now()) {
                            crate::audio::voice::ACTIVITY.signal(());
                        }
                    }
                    // Speaker frames are queued before the reply goes out:
                    // the write is decided whatever the reply's fate.
                    if e.handle() == server.audio.speaker.handle
                        && !crate::audio::voice::host_frame(Link::Ble, e.data())
                    {
                        // Logging blocks: once, then every 50th.
                        if speaker_drops % 50 == 0 {
                            logln!(
                                "voice: speaker frame dropped ({} so far): queue full or oversized",
                                speaker_drops + 1
                            );
                        }
                        speaker_drops += 1;
                    }
                }
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => {
                        logln!("link: GATT error: {:?}", e);
                        continue;
                    }
                }
                if mic_cccd {
                    // A live capture notices an unsubscribe by itself and
                    // ends its stream.
                    let on = subscribed(server, conn, server.audio.mic.cccd_handle);
                    let mtu = conn.raw().att_mtu();
                    crate::audio::voice::subscribe(Link::Ble, on, mtu.saturating_sub(3));
                    logln!(
                        "voice: host {} the microphone (ATT MTU {})",
                        if on {
                            "subscribed to"
                        } else {
                            "unsubscribed from"
                        },
                        mtu
                    );
                }
                if cccd_written {
                    if let Some(mut record) = saved
                        .as_ref()
                        .filter(|r| {
                            bond_information(r)
                                .identity
                                .match_identity(&conn.raw().peer_identity())
                        })
                        .cloned()
                    {
                        (record.hid_notify, record.battery_notify) =
                            notification_preferences(server, conn);
                        persist_bond(saved, record);
                    }
                }
                if len == 0 {
                    continue;
                }
                dispatch_frame(Link::Ble, &mut inbound, &frame[..len]).await;
            }
            _ => {}
        }
    }
}

/// Host reports arriving in a burst (a sync, a multi-report line) ask for
/// the fast link, as audio does: on macOS's idle link (latency 22) every
/// write waits up to 345 ms for the deck to listen, and a full sync then
/// takes 20 s instead of under one. The heartbeat, one report every 5 s,
/// never counts as a burst, so an idle session stays on the idle link.
struct Burst {
    times: [Instant; Self::REPORTS],
    next: usize,
}

impl Burst {
    const REPORTS: usize = 3;
    const WINDOW: Duration = Duration::from_millis(1500);

    fn new() -> Self {
        Self {
            times: [Instant::MIN; Self::REPORTS],
            next: 0,
        }
    }

    /// Note one host report; true when it completes a burst.
    fn report(&mut self, now: Instant) -> bool {
        self.times[self.next] = now;
        self.next = (self.next + 1) % Self::REPORTS;
        // The slot just overwritten next holds the oldest of the last three.
        now.saturating_duration_since(self.times[self.next]) <= Self::WINDOW
    }
}

/// What one link's inbound side keeps between reports: the system request
/// under reassembly and the deck line under assembly.
struct Inbound {
    rx: wire::Reassembler,
    deck_rx: proto::LineAssembler,
}

impl Inbound {
    const fn new() -> Self {
        Self {
            rx: wire::Reassembler::new(),
            deck_rx: proto::LineAssembler::new(),
        }
    }
}

/// One output report from a host, with or without its leading report-id
/// byte: a deck fragment (channel 5) feeds the line assembler, a voice
/// frame (channel 7) the voice glue, anything else is a piece of a system
/// request. Replies go back on `link`.
async fn dispatch_frame(link: Link, inbound: &mut Inbound, frame: &[u8]) {
    if is_deck(frame) {
        deck_report(link, &mut inbound.deck_rx, frame).await;
        return;
    }
    if let Some(voice) = crate::audio::codec::report_frame(frame) {
        voice_report(link, voice);
        return;
    }
    match inbound.rx.push(frame) {
        wire::Push::Complete(n) => {
            let battery = BATTERY.lock(Cell::get);
            let mut message = ReplyMessage {
                bytes: [0; wire::TX_CAP],
                len: 0,
            };
            #[cfg(feature = "bootloader-update")]
            if let Some(len) = (link == Link::Usb)
                .then(|| boot_update::handle(inbound.rx.data(n), &mut message.bytes))
                .flatten()
            {
                inbound.rx.clear();
                message.len = len;
                REPLIES[link.index()].send(message).await;
                return;
            }
            #[cfg(feature = "panel-debug")]
            if let Some(len) = panel_debug::handle(inbound.rx.data(n), &mut message.bytes).await {
                inbound.rx.clear();
                message.len = len;
                REPLIES[link.index()].send(message).await;
                return;
            }
            #[cfg(feature = "audio-debug")]
            if let Some(len) = debug::handle(inbound.rx.data(n), &mut message.bytes) {
                inbound.rx.clear();
                message.len = len;
                REPLIES[link.index()].send(message).await;
                return;
            }
            let (handled, len) = wire::handle_request(
                inbound.rx.data(n),
                concat!(env!("CARGO_PKG_VERSION"), "-openwalkietalkie"),
                crate::orientation::BOARD,
                wire::Status {
                    battery: battery.percentage().unwrap_or(0),
                    is_charging: matches!(battery.charge, ChargeState::Charging),
                    recovery_protocol: {
                        #[cfg(feature = "hw-a")]
                        {
                            boot_info::recovery_protocol()
                        }
                        #[cfg(not(feature = "hw-a"))]
                        {
                            None
                        }
                    },
                    maintenance: cfg!(feature = "bootloader-update"),
                },
                &mut message.bytes,
            );
            inbound.rx.clear();
            logln!("link: {:?} host {:?}", link, handled);
            if len > 0 {
                message.len = len;
                REPLIES[link.index()].send(message).await;
            }
            if let wire::Handled::Reboot { recovery } = handled {
                crate::app::request_reboot(recovery);
            }
        }
        wire::Push::Dropped => logln!("link: oversized RPC discarded"),
        _ => {}
    }
}

/// Voice frames dropped on the cable (queue full or oversized), for the log
/// cadence: once, then every 50th.
static USB_SPEAKER_DROPS: Mutex<CriticalSectionRawMutex, Cell<u32>> = Mutex::new(Cell::new(0));

/// A channel-7 report from the cable's host. LISTEN is answered here: it
/// subscribes the cable to microphone frames and the INFO answer goes out
/// as that link's next voice frame. Everything else (MIC requests, speaker
/// frames) is the voice glue's. Over Bluetooth the audio service carries
/// the frames and has its own subscription, so a channel-7 report there is
/// a host on the wrong link, and is ignored.
fn voice_report(link: Link, frame: &[u8]) {
    use crate::audio::{codec, voice};
    if link != Link::Usb {
        return;
    }
    match codec::parse(frame) {
        codec::Frame::Listen { on } => {
            voice::subscribe(link, on, codec::REPORT_FRAME as u16);
            logln!(
                "voice: the USB host {} the microphone",
                if on {
                    "listens to"
                } else {
                    "stopped listening to"
                }
            );
            if on {
                let mut bytes = [0u8; 1 + codec::INFO_LEN];
                let mut info = AUDIO_INFO;
                if link == Link::Usb && info[4] & 0b10 != 0 {
                    info[4] |= codec::FEATURE_USB_SPEAKER_48K;
                }
                let n = codec::info_frame(&info, &mut bytes);
                if let Some(info) = voice::Frame::new(&bytes[..n]) {
                    let _ = voice::MIC_FRAMES[link.index()].try_send(info);
                }
            }
        }
        _ => {
            if !voice::host_frame(link, frame) {
                let drops = USB_SPEAKER_DROPS.lock(|d| {
                    let n = d.get();
                    d.set(n.wrapping_add(1));
                    n
                });
                if drops % 50 == 0 {
                    logln!(
                        "voice: speaker frame dropped on USB ({} so far): queue full or oversized",
                        drops + 1
                    );
                }
            }
        }
    }
}

/// A channel-5 body, with or without its report id.
fn is_deck(body: &[u8]) -> bool {
    matches!(
        body,
        [proto::REPORT_ID, proto::CHANNEL, ..] | [proto::CHANNEL, ..]
    )
}

/// Feed one channel-5 body to the assembler one byte at a time, so at most one
/// line completes per push and its delivery can await queue space.
async fn deck_report(link: Link, assembler: &mut proto::LineAssembler, body: &[u8]) {
    let body = match body {
        [proto::REPORT_ID, ..] => &body[1..],
        _ => body,
    };
    let [_, len, payload @ ..] = body else {
        return;
    };
    let payload = &payload[..(*len as usize).min(payload.len())];
    let mut msg = HostMsg::None;
    for &byte in payload {
        let mut done = None;
        assembler.push(&[proto::CHANNEL, 1, byte], &mut |result| {
            done = Some(result.and_then(|line| proto::parse(line, &mut msg)));
        });
        if let Some(result) = done {
            flags(link).deck_spoke.store(true, Ordering::Release);
            DECK_RX.send(result.map(|()| msg)).await;
        }
    }
}

fn bond_information(record: &bond_record::Record) -> BondInformation {
    BondInformation::new(
        Identity {
            bd_addr: BdAddr(record.address),
            irk: record
                .irk
                .map(|key| IdentityResolvingKey(u128::from_le_bytes(key))),
        },
        LongTermKey(u128::from_le_bytes(record.ltk)),
        if record.authenticated {
            SecurityLevel::EncryptedAuthenticated
        } else {
            SecurityLevel::Encrypted
        },
        true,
    )
}

fn notification_preferences(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
) -> (bool, bool) {
    let mut flags = (false, false);
    if let Some(table) = server.get_cccd_table(conn.raw()) {
        for (handle, value) in table.inner() {
            if Some(*handle) == server.hid.input.cccd_handle {
                flags.0 = value.should_notify();
            }
            if Some(*handle) == server.battery.level.cccd_handle {
                flags.1 = value.should_notify();
            }
        }
    }
    flags
}

/// Whether this connection has notifications on for `cccd`.
fn subscribed(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    cccd: Option<u16>,
) -> bool {
    server.get_cccd_table(conn.raw()).is_some_and(|table| {
        table
            .inner()
            .iter()
            .any(|(handle, value)| Some(*handle) == cccd && value.should_notify())
    })
}

/// The voice link's half of a connection: ask for the link settings a
/// 64 kbit/s stream needs, then hand microphone frames to the radio.
async fn voice_task(
    stack: &Stack<'_, BleController, DefaultPacketPool>,
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
) {
    use crate::audio::voice::{self, ACTIVITY, MIC_FRAMES};
    let frames = &MIC_FRAMES[Link::Ble.index()];
    frames.clear();
    // Activity from before this connection is no reason to speed it up.
    ACTIVITY.reset();
    // 251-byte link packets (the largest, at 2120 us airtime): without the
    // data length extension each frame is cut into 27-byte pieces and the
    // stream does not fit in a 15 ms connection event.
    if let Err(e) = conn.raw().update_data_length(stack, 251, 2120).await {
        logln!("link: data length request failed: {:?}", e);
    }
    if let Err(e) = conn.raw().set_phy(stack, PhyKind::Le2M).await {
        logln!("link: 2M PHY request failed: {:?}", e);
    }
    let params = conn.raw().params();
    logln!(
        "link: link interval {} us, latency {}, ATT MTU {}",
        params.conn_interval.as_micros(),
        params.peripheral_latency,
        conn.raw().att_mtu()
    );
    let mut value: heapless::Vec<u8, { crate::audio::codec::MAX_FRAME }> = heapless::Vec::new();
    // The host's own choice, to go back to once audio stops: taken from a
    // link that has latency, so a stream starting before the previous
    // relax has landed cannot make the fast parameters the baseline.
    let mut baseline: Option<ConnParams> = None;
    let mut fast = false;
    let mut relax_at = Instant::now();
    // When the fast link was last asked for. The host can refuse it or undo
    // it (a collision with its own update, a later change of mind), so while
    // audio flows a link that still has latency is asked again, at most
    // once a second.
    let mut asked_at: Option<Instant> = None;
    let mut notify_failures = 0u32;
    loop {
        let relax = async {
            if fast {
                Timer::at(relax_at).await
            } else {
                core::future::pending().await
            }
        };
        let frame = match select3(frames.receive(), ACTIVITY.wait(), relax).await {
            Either3::First(frame) => Some(frame),
            Either3::Second(()) => None,
            Either3::Third(()) => {
                fast = false;
                asked_at = None;
                let idle = baseline.as_ref().map(relaxed).unwrap_or(IDLE_LINK);
                request_params(stack, conn, &idle, "relaxed").await;
                continue;
            }
        };
        relax_at = Instant::now() + FAST_HOLD;
        let now = conn.raw().params();
        if !fast {
            fast = true;
            if now.peripheral_latency > 0 {
                baseline = Some(now);
            }
        }
        if now.peripheral_latency > 0
            && asked_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            asked_at = Some(Instant::now());
            request_params(stack, conn, &FAST_LINK, "fast").await;
        }
        let Some(frame) = frame else {
            continue;
        };
        // The largest value this link carries; the microphone task sizes
        // its frames from it.
        let mtu = conn.raw().att_mtu();
        voice::set_max_value(Link::Ble, mtu.saturating_sub(3));
        value.clear();
        if value.extend_from_slice(frame.as_bytes()).is_err() {
            continue;
        }
        match with_timeout(
            Duration::from_millis(500),
            server.audio.mic.notify(conn, &value),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // Logging blocks: once, then every 50th.
                if notify_failures % 50 == 0 {
                    logln!(
                        "voice: mic notify failed ({} so far): {:?}",
                        notify_failures + 1,
                        e
                    );
                }
                notify_failures += 1;
            }
            Err(_) => {
                // A link that cannot take one frame in half a second is
                // gone for practical purposes; the HID side agrees.
                logln!("voice: mic notify stalled; dropping the link");
                conn.raw().disconnect();
                return;
            }
        }
    }
}

/// How long the link stays fast after the last audio: one PTT exchange
/// (the question, the wait, the spoken answer) should not bounce it.
const FAST_HOLD: Duration = Duration::from_secs(5);

/// A 64 kbit/s stream each way needs every connection event: 15 ms (the
/// shortest interval Apple's accessory guidelines allow) and no peripheral
/// latency; the 2 s timeout is what macOS already uses for the deck.
const FAST_LINK: RequestedConnParams = RequestedConnParams {
    min_connection_interval: Duration::from_millis(15),
    max_connection_interval: Duration::from_millis(15),
    max_latency: 0,
    min_event_length: Duration::from_ticks(0),
    max_event_length: Duration::from_millis(15),
    supervision_timeout: Duration::from_secs(2),
};

/// What macOS picks for the deck when nobody asks (measured 2026-10-04): the
/// idle link to go back to when the stream started before the host had set
/// one of its own.
const IDLE_LINK: RequestedConnParams = RequestedConnParams {
    min_connection_interval: Duration::from_millis(15),
    max_connection_interval: Duration::from_millis(15),
    max_latency: 22,
    min_event_length: Duration::from_ticks(0),
    max_event_length: Duration::from_ticks(0),
    supervision_timeout: Duration::from_secs(2),
};

/// What the host had chosen before the stream, asked for again, inside
/// Apple's accessory rules: the interval a multiple of 15 ms and either 15 ms
/// at both ends or 15 ms between them; the timeout 2 to 6 s; latency at most
/// 30, with interval max x (latency + 1) at most 2 s and, times 3, under the
/// timeout.
fn relaxed(before: &ConnParams) -> RequestedConnParams {
    const STEP_US: u64 = 15_000;
    let interval_us = (before.conn_interval.as_micros() / STEP_US).max(1) * STEP_US;
    let max_us = if interval_us > STEP_US {
        interval_us + STEP_US
    } else {
        interval_us
    };
    let timeout = before
        .supervision_timeout
        .max(Duration::from_secs(2))
        .min(Duration::from_secs(6));
    let events = (2_000_000 / max_us).min((timeout.as_micros() - 1) / (3 * max_us));
    let latency = (events.saturating_sub(1).min(30) as u16).min(before.peripheral_latency);
    RequestedConnParams {
        min_connection_interval: Duration::from_micros(interval_us),
        max_connection_interval: Duration::from_micros(max_us),
        max_latency: latency,
        min_event_length: Duration::from_ticks(0),
        max_event_length: Duration::from_ticks(0),
        supervision_timeout: timeout,
    }
}

async fn request_params(
    stack: &Stack<'_, BleController, DefaultPacketPool>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    params: &RequestedConnParams,
    what: &str,
) {
    match conn.raw().update_connection_params(stack, params).await {
        Ok(()) => logln!(
            "voice: asked for the {} link ({} us, latency {})",
            what,
            params.max_connection_interval.as_micros(),
            params.max_latency
        ),
        Err(e) => logln!("voice: {} link request failed: {:?}", what, e),
    }
}

/// The device preferences as last saved (`Prefs::default()` on a fresh or
/// factory-reset deck).
pub fn prefs() -> Prefs {
    bond_flash::load_prefs()
}

/// Save the device preferences; a no-op when nothing changed. Logged, never
/// fatal: a deck that cannot remember shows the tutorial again.
pub fn save_prefs(prefs: Prefs) {
    match bond_flash::save_prefs(&prefs) {
        Ok(()) => logln!("link: preferences saved to flash"),
        Err(e) => logln!("link: preference storage failed: {:?}", e),
    }
}

/// Factory reset: blank the journal, pairing key and preferences alike. The
/// caller reboots afterwards; until then the stack still holds the key in
/// RAM, so the host stays connected.
pub fn erase_storage() -> bool {
    match bond_flash::erase() {
        Ok(()) => {
            logln!("link: storage erased (pairing and preferences)");
            true
        }
        Err(e) => {
            logln!("link: storage erase failed: {:?}", e);
            false
        }
    }
}

fn persist_bond(saved: &mut Option<bond_record::Record>, record: bond_record::Record) {
    match bond_flash::save(&record) {
        Ok(()) => logln!("link: pairing key and subscriptions saved to flash"),
        Err(e) => logln!(
            "link: pairing storage failed: {:?}; reset will require re-pairing",
            e
        ),
    }
    *saved = Some(record);
}

/// Why a link stopped carrying reports; the payload is read by the derived
/// `Debug` in the failure log line.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum LinkError {
    Ble(trouble_host::Error),
    Usb(embassy_usb::driver::EndpointError),
    Timeout,
}

/// One link's way of getting a 64-byte report (report id first) to its host.
pub(crate) trait ReportSink {
    async fn send(&mut self, report: &[u8; wire::REPORT_LEN]) -> Result<(), LinkError>;
    /// Every 5 s of silence (BLE notifies the battery level).
    async fn idle(&mut self, battery_percent: u8) -> Result<(), LinkError> {
        let _ = battery_percent;
        Ok(())
    }
    /// After an overflow: make the host resync.
    fn drop_link(&mut self);
}

struct BleSink<'a, 'b, 'c> {
    server: &'a Server<'b>,
    conn: &'a GattConnection<'b, 'c, DefaultPacketPool>,
}

impl ReportSink for BleSink<'_, '_, '_> {
    /// The BLE report characteristic carries the 63-byte body; the report id
    /// lives in its Report Reference descriptor.
    async fn send(&mut self, report: &[u8; wire::REPORT_LEN]) -> Result<(), LinkError> {
        let mut body = [0; 63];
        body.copy_from_slice(&report[1..]);
        match with_timeout(
            Duration::from_millis(500),
            self.server.hid.input.notify(self.conn, &body),
        )
        .await
        {
            Ok(result) => result.map_err(LinkError::Ble)?,
            Err(_) => {
                self.conn.raw().disconnect();
                return Err(LinkError::Timeout);
            }
        }
        Timer::after_millis(4).await;
        Ok(())
    }

    async fn idle(&mut self, battery_percent: u8) -> Result<(), LinkError> {
        self.server
            .battery
            .level
            .notify(self.conn, &battery_percent)
            .await
            .map_err(LinkError::Ble)
    }

    fn drop_link(&mut self) {
        // The connection loop disconnects once this task returns.
    }
}

async fn transmit_task<S: ReportSink>(link: Link, sink: &mut S) {
    let events = &EVENTS[link.index()];
    let replies = &REPLIES[link.index()];
    let overflow = &OVERFLOW[link.index()];
    let voice = &crate::audio::voice::MIC_FRAMES[link.index()];
    loop {
        // Over Bluetooth the voice frames leave as notifications of the
        // audio service (`voice_task`); over the cable they are channel-7
        // reports of this queue.
        let voice_frame = async {
            if link == Link::Usb {
                voice.receive().await
            } else {
                core::future::pending().await
            }
        };
        let result = match select4(
            replies.receive(),
            events.receive(),
            voice_frame,
            select(overflow.wait(), Timer::after_secs(5)),
        )
        .await
        {
            Either4::First(reply) => send_line(sink, &reply.bytes[..reply.len], wire::frame).await,
            Either4::Second(slot) => {
                let result = send_deck_slot(sink, slot).await;
                free_deck_slot(slot);
                result
            }
            Either4::Third(frame) => send_voice(sink, &frame).await,
            Either4::Fourth(Either::First(())) => {
                // Drop the burst, tell the deck host every press is up, then
                // drop the link so it resyncs: a lost release must never
                // stay held.
                clear_outbound(link);
                logln!("link: {:?} output overflow; releasing host controls", link);
                if flags(link).deck_spoke.load(Ordering::Acquire) {
                    let _ = send_line(sink, br#"{"m":"up"}"#, proto::frame).await;
                }
                sink.drop_link();
                return;
            }
            Either4::Fourth(Either::Second(())) => {
                let level = BATTERY.lock(Cell::get).percentage().unwrap_or(0);
                sink.idle(level).await
            }
        };
        if let Err(e) = result {
            logln!("link: {:?} send failed: {:?}", link, e);
            return;
        }
    }
}

/// One voice frame as a channel-7 report. A frame too long for a report
/// cannot be meant for this link (the microphone task sizes its frames for
/// it), so it is dropped rather than cut.
async fn send_voice<S: ReportSink>(
    sink: &mut S,
    frame: &crate::audio::voice::Frame,
) -> Result<(), LinkError> {
    let mut report = [0; wire::REPORT_LEN];
    if crate::audio::codec::report(frame.as_bytes(), &mut report).is_none() {
        return Ok(());
    }
    sink.send(&report).await
}

/// One line, framed into reports by `frame` (which appends the LF and returns
/// 0 when done), sent with the link's own pacing on either channel.
async fn send_line<S: ReportSink>(
    sink: &mut S,
    json: &[u8],
    frame: fn(&[u8], usize, &mut [u8; wire::REPORT_LEN]) -> usize,
) -> Result<(), LinkError> {
    let mut offset = 0;
    loop {
        let mut report = [0; wire::REPORT_LEN];
        let used = frame(json, offset, &mut report);
        if used == 0 {
            return Ok(());
        }
        offset += used;
        sink.send(&report).await?;
    }
}

/// A queued deck line, framed straight out of its slot.
async fn send_deck_slot<S: ReportSink>(sink: &mut S, slot: u8) -> Result<(), LinkError> {
    let mut offset = 0;
    loop {
        let mut report = [0; wire::REPORT_LEN];
        let used = DECK_LINES.lock(|lines| {
            let lines = lines.borrow();
            let line = &lines.lines[slot as usize % DECK_SLOTS];
            proto::frame(&line.bytes[..line.len as usize], offset, &mut report)
        });
        if used == 0 {
            return Ok(());
        }
        offset += used;
        sink.send(&report).await?;
    }
}
