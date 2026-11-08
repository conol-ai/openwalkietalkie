//! The `deckd-hid` thread (design §6.2): hidapi read at ≤2 ms, the
//! `DeviceView`, the outbox with its 8 ms pacer and the framer on channel 5.
//! Non-exclusive open so the bench tools (`tools/flash-usb.py`, the
//! diagnostics channel) can talk to the deck beside the daemon.
//!
//! `Link` is the whole thread minus the I/O — ops and input reports in,
//! output reports and `FromHid` messages out — so the session, barrier and
//! demux logic is unit-tested with a simulated clock; `spawn` wraps it in
//! the hidapi loop.
use crate::device::outbox::Outbox;
use crate::device::session::{Action, Input, Session};
use crate::device::view::DeviceView;
use crate::device::writer::{Writer, Written, CLOSE_BUDGET};
pub use crate::metrics::Metric;
use crossbeam_channel::{Receiver, Sender};
use deck_model::{Control, DeviceLine, DeviceOp, HostLine, Key, LinkEvent, SessionState};
use deck_proto::{Assembler, ErrWhy, REPORT_ID, REPORT_LEN};
use hidapi::{HidApi, HidDevice};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

pub const VID: u16 = 0x1209;
pub const PID: u16 = 0x5754;
pub const USAGE_PAGE: u16 = 0xff00;
/// Capacity of the core → hid channel (§2.3).
pub const OPS_CAPACITY: usize = 512;
/// Re-enumeration period while no device is open.
pub const ENUMERATE_EVERY: Duration = Duration::from_millis(800);
/// Input poll period while linked (zero-timeout reads).
pub const POLL: Duration = Duration::from_millis(2);
/// Without any input report for this long, re-enumerate to confirm presence.
pub const PRESENCE_EVERY: Duration = Duration::from_secs(2);
/// Two burst-following drops within this window are the overflow signature.
pub const BACKOFF_WINDOW: Duration = Duration::from_secs(300);
/// Reports drained per poll.
const DRAIN_MAX: usize = 64;

/// What the hid thread sends the core (unbounded, §2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromHid {
    Line(DeviceLine),
    Link(LinkEvent),
    Metric(Metric),
}

/// Debug controls (`deckd device flood`, a manual barrier, a metric now).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidDebug {
    Flood(u64),
    Sync,
    Metric,
}

/// The core's handle on the thread.
#[derive(Clone, Default)]
pub struct Maintenance {
    pub requested: Arc<AtomicBool>,
    pub ready: Arc<AtomicBool>,
}

pub struct HidHandle {
    pub maintenance: Maintenance,
    /// Bounded (512); the core uses `try_send` only.
    pub ops: Sender<DeviceOp>,
    /// Unbounded: device edges are wire-limited.
    pub lines: Receiver<FromHid>,
    /// Debug controls; never on the wire path.
    pub debug: Sender<HidDebug>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HidHandle {
    /// A handle with no thread behind it: the core's tests hold the other
    /// ends and see every op the core sends.
    pub fn detached() -> (
        Self,
        Receiver<DeviceOp>,
        Sender<FromHid>,
        Receiver<HidDebug>,
    ) {
        let (ops_tx, ops_rx) = crossbeam_channel::bounded::<DeviceOp>(OPS_CAPACITY);
        let (lines_tx, lines_rx) = crossbeam_channel::unbounded::<FromHid>();
        let (debug_tx, debug_rx) = crossbeam_channel::unbounded::<HidDebug>();
        (
            HidHandle {
                maintenance: Maintenance::default(),
                ops: ops_tx,
                lines: lines_rx,
                debug: debug_tx,
                join: None,
            },
            ops_rx,
            lines_tx,
            debug_rx,
        )
    }

    /// Dropping the `ops` sender ends the thread; join it.
    pub fn join(mut self) {
        drop(self.ops);
        drop(self.debug);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Clone, Debug)]
pub struct HidConfig {
    pub pace: Duration,
    pub hello_every: Duration,
    pub sid: u32,
    pub clock_local: bool,
    /// False: never enumerate or open a device (tests, `--no-device`).
    pub hardware: bool,
    /// The cable's port of the voice link, driven here whenever the
    /// instance opened is the USB one: channel-7 reports in and out,
    /// beside the deck lines. `None` without a voice engine.
    pub audio: Option<crate::ble::UsbPort>,
}

#[derive(Debug, thiserror::Error)]
pub enum HidError {
    #[error("cannot initialize HID")]
    Init,
    #[error("HID enumeration failed")]
    Enumerate,
    #[error("OpenWalkieTalkie is not connected")]
    NotFound,
    #[error("cannot open HID device")]
    Open,
    #[error("HID {0} failed")]
    Io(&'static str),
}

/// One-off HID probes (`deckd doctor`) run on this permanent thread, and
/// the `HidApi` they share is created once and never dropped.
///
/// hidapi on macOS keeps a process-global `IOHIDManager` scheduled on the
/// CoreFoundation run loop of whichever thread first initialised it, and a
/// thread's run loop dies with the thread. A second `hid_enumerate` from a
/// different thread then faults inside `__IOHIDManagerDeviceAdded →
/// IOHIDDeviceScheduleWithRunLoop` (`EXC_BAD_ACCESS` at 0x10 in
/// `objc_msgSend`) — measured: `cargo test -p deckd --lib`, whose tests each
/// get a fresh thread and two of which call `doctor`, died there in four
/// runs out of six. The daemon's own `deckd-hid` thread keeps its `HidApi`
/// alive on one thread for its whole life and is safe as it is; a one-off
/// probe is what needs a home.
type Probe = Box<dyn FnOnce(Option<&mut HidApi>) + Send>;

fn prober() -> &'static crossbeam_channel::Sender<Probe> {
    static PROBER: std::sync::OnceLock<crossbeam_channel::Sender<Probe>> =
        std::sync::OnceLock::new();
    PROBER.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<Probe>();
        std::thread::Builder::new()
            .name("deckd-hid-probe".into())
            .spawn(move || {
                // Never dropped, never re-created: this thread outlives
                // every caller, so the run loop hidapi captured stays alive.
                let mut api = HidApi::new().ok();
                while let Ok(probe) = rx.recv() {
                    // The list is cached in the `HidApi`, and this one is
                    // never re-created: re-enumerate, or a probe after a
                    // device was plugged in would answer from before it.
                    if let Some(api) = api.as_mut() {
                        let _ = api.refresh_devices();
                    }
                    // A panicking probe must not take the thread — and with
                    // it every later probe — down with it.
                    let api = api.as_mut();
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || probe(api)));
                }
            })
            .expect("the HID probe thread");
        tx
    })
}

/// Ask the shared `HidApi` something, from any thread; the device list is
/// refreshed first. `Err` when HID could not be initialised at all, or when
/// `f` panicked.
pub fn probe<T: Send + 'static>(
    f: impl FnOnce(&HidApi) -> T + Send + 'static,
) -> Result<T, HidError> {
    probe_mut(move |api| f(api))
}

pub fn probe_mut<T: Send + 'static>(
    f: impl FnOnce(&mut HidApi) -> T + Send + 'static,
) -> Result<T, HidError> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let job: Probe = Box::new(move |api| {
        if let Some(api) = api {
            let _ = tx.send(f(api));
        }
    });
    prober().send(job).map_err(|_| HidError::Init)?;
    rx.recv().map_err(|_| HidError::Init)
}

/// One enumerated instance of the deck's vendor collection.
#[derive(Clone, Debug)]
pub struct Instance {
    pub path: std::ffi::CString,
    pub bus: hidapi::BusType,
    /// The USB serial number (the deck's Bluetooth address), when hidapi
    /// has it.
    pub serial: Option<String>,
}

/// Every instance of the vendor collection, in hidapi's order.
fn instances(api: &mut HidApi) -> Result<Vec<Instance>, HidError> {
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    api.refresh_devices().map_err(|_| HidError::Enumerate)?;
    Ok(api
        .device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID && d.usage_page() == USAGE_PAGE)
        .map(|d| Instance {
            path: d.path().to_owned(),
            bus: d.bus_type(),
            serial: d.serial_number().map(str::to_owned),
        })
        .collect())
}

/// The instance to talk to. The Rev A deck is the same HID device over
/// Bluetooth and over its USB cable; the cable is faster (no BLE ack per
/// report) and a Bluetooth instance may be another, idle board, so USB wins
/// whenever one is present. Otherwise hidapi's first.
pub fn pick(found: &[Instance]) -> Option<&Instance> {
    found
        .iter()
        .find(|i| matches!(i.bus, hidapi::BusType::Usb))
        .or_else(|| found.first())
}

/// Open the deck's vendor collection, non-exclusively.
pub fn open_device(api: &mut HidApi) -> Result<HidDevice, HidError> {
    let found = instances(api)?;
    let chosen = pick(&found).ok_or(HidError::NotFound)?;
    api.open_path(&chosen.path).map_err(|_| HidError::Open)
}

/// True when the deck is still enumerated (macOS may deliver nothing, and no
/// error, for a device that silently went away).
pub fn present(api: &mut HidApi) -> bool {
    api.refresh_devices().is_ok()
        && api
            .device_list()
            .any(|d| d.vendor_id() == VID && d.product_id() == PID)
}

/// Drain the host-side input queue with zero-timeout reads (macOS reports
/// removal through it); `Ok(reports)` with the buffers read.
pub fn drain(device: &HidDevice, max: usize) -> Result<Vec<[u8; 64]>, HidError> {
    let mut out = Vec::new();
    for _ in 0..max {
        let mut report = [0u8; 64];
        let n = device
            .read_timeout(&mut report, 0)
            .map_err(|_| HidError::Io("read"))?;
        if n == 0 {
            break;
        }
        out.push(report);
    }
    Ok(out)
}

/// The channel byte of an input report (with or without the report id).
pub fn channel_of(report: &[u8]) -> Option<u8> {
    match report {
        [REPORT_ID, channel, ..] => Some(*channel),
        [channel, ..] => Some(*channel),
        [] => None,
    }
}

/// The thread's logic without the I/O.
pub struct Link {
    view: DeviceView,
    outbox: Outbox,
    session: Session,
    assembler: Assembler,
    linked: bool,
    /// The handle must be dropped (6 misses): the thread re-enumerates.
    drop_handle: bool,
    /// `sid` and `boot` of the last hello reply.
    last_reply: Option<(u32, bool)>,
    burst_drops: Vec<Instant>,
    misses_seen: u32,
    stats: Metric,
    writes: u64,
    write_total_us: u64,
    /// `answer` input reports whose `card clear` has not gone out yet,
    /// keyed by (slot, card) — the start of the §4.8 answer→clear budget.
    answers: HashMap<(u8, u16), Instant>,
    /// The answer a `card clear` just handed to the writer belongs to,
    /// until that write returns (the `_wire_us` half).
    wire_pending: Option<Instant>,
    writes_in_flight: u32,
    out: Vec<FromHid>,
    /// The voice link's cable port (`HidConfig::audio`).
    audio: Option<crate::ble::UsbPort>,
    /// The write in flight is a voice report: it is outside the outbox's
    /// pace and its barrier, so `after_write` leaves those alone.
    voice_in_flight: bool,
}

/// A `hid_write` longer than this counts as slow.
pub const SLOW_WRITE: Duration = Duration::from_millis(100);
/// An `answer` whose `card clear` never went out is forgotten after this
/// (the store has failed the card long before, §4.8).
pub const ANSWER_CLEAR_MAX_AGE: Duration = Duration::from_secs(30);
/// At most this many answers are timed at once (one per slot, plus slack).
const ANSWERS_TRACKED_MAX: usize = 16;

/// The channel-5 payload of an output report, without the report id, the
/// channel and the length byte. A `Chunker` never glues two lines into one
/// report, and a `card clear` line is far under `PAYLOAD_MAX`, so a clear
/// is always exactly one payload.
fn report_payload(report: &[u8; REPORT_LEN]) -> Option<&[u8]> {
    if report[0] != REPORT_ID || report[1] != deck_proto::CHANNEL {
        return None;
    }
    let len = usize::from(report[2]);
    report.get(3..3 + len)
}

/// The exact bytes `{"m":"card","agent":…,"id":…,"clear":1}\n` the encoder
/// produces for this clear, so a report can be recognised by comparison
/// rather than by re-parsing JSON on the hot path.
fn clear_line(slot: u8, id: u16) -> Option<Vec<u8>> {
    deck_proto::encode(&HostLine::CardClear { slot, id }).ok()
}

/// Cheap pre-filter: only a `card … clear` line can close an answer.
const CLEAR_TAIL: &[u8] = b",\"clear\":1}\n";

/// Microseconds, saturating at `u32::MAX` (71 minutes: nothing here is ever
/// that old, and a metric must never wrap).
fn micros(d: Duration) -> u32 {
    d.as_micros().min(u128::from(u32::MAX)) as u32
}

impl Link {
    pub fn new(cfg: &HidConfig) -> Self {
        let mut outbox = Outbox::new(cfg.pace, cfg.sid);
        outbox.set_clock(cfg.clock_local, None);
        outbox.hold_lanes(true);
        Link {
            view: DeviceView::new(),
            outbox,
            session: Session::new(cfg.sid, cfg.hello_every),
            assembler: Assembler::new(),
            linked: false,
            drop_handle: false,
            last_reply: None,
            burst_drops: Vec::new(),
            misses_seen: 0,
            writes: 0,
            write_total_us: 0,
            answers: HashMap::new(),
            wire_pending: None,
            writes_in_flight: 0,
            stats: Metric {
                pace_ms: cfg.pace.max(Duration::from_millis(8)).as_millis() as u16,
                ..Metric::default()
            },
            out: Vec::new(),
            audio: cfg.audio.clone(),
            voice_in_flight: false,
        }
    }

    /// The cable port's wake-up, for the thread's wait: ready whenever a
    /// voice frame was queued for the cable.
    pub fn audio_wake(&self) -> Option<&Receiver<()>> {
        self.audio.as_ref().map(crate::ble::UsbPort::wake)
    }

    /// The instance opened is the USB one: the voice link's cable port
    /// attaches (its LISTEN is the next report out).
    pub fn cable_up(&mut self, serial: Option<&str>) {
        if let Some(port) = self.audio.as_mut() {
            port.attach(serial);
        }
    }

    /// The report that lets the deck stop streaming to us, for a graceful
    /// close; `None` when the cable was never listening.
    pub fn farewell(&mut self) -> Option<[u8; REPORT_LEN]> {
        self.audio.as_mut().and_then(crate::ble::UsbPort::farewell)
    }

    pub fn view(&self) -> &DeviceView {
        &self.view
    }

    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn is_linked(&self) -> bool {
        self.linked
    }

    /// Messages for the core, in order.
    pub fn drain(&mut self) -> Vec<FromHid> {
        std::mem::take(&mut self.out)
    }

    /// True once after the session asked for a reopen; the thread drops the handle.
    pub fn take_drop_handle(&mut self) -> bool {
        std::mem::take(&mut self.drop_handle)
    }

    /// A `DeviceOp` from the core: view first, outbox second (§6.2).
    pub fn apply(&mut self, op: DeviceOp, now: Instant) {
        self.view.apply(&op);
        self.outbox.enqueue(&self.view, op, now);
    }

    pub fn debug(&mut self, cmd: HidDebug, now: Instant) {
        match cmd {
            HidDebug::Flood(n) => self.outbox.flood(n),
            HidDebug::Sync => {
                if self.linked {
                    self.outbox.begin_barrier(&self.view, false);
                    self.stats.barriers += 1;
                }
            }
            HidDebug::Metric => {
                let m = self.metric(now);
                self.out.push(FromHid::Metric(m));
            }
        }
    }

    /// The device was opened.
    pub fn link_up(&mut self, now: Instant) {
        self.linked = true;
        self.drop_handle = false;
        self.assembler.clear();
        self.outbox.clear_link(now);
        self.session_step(Input::LinkUp, now);
    }

    /// Read/write error or removal: the handle is gone.
    pub fn link_down(&mut self, now: Instant, why: &str) {
        if !self.linked {
            return;
        }
        tracing::warn!(why, "device link down");
        self.dropped(now);
        self.session_step(Input::LinkDown, now);
    }

    /// Bookkeeping shared by every way the link ends.
    fn dropped(&mut self, now: Instant) {
        self.linked = false;
        self.stats.link_drops += 1;
        self.voice_in_flight = false;
        if let Some(port) = self.audio.as_mut() {
            port.detach(None);
        }
        // Nothing in flight survives the link, measurements included.
        self.answers.clear();
        self.wire_pending = None;
        self.writes_in_flight = 0;
        self.outbox.clear_link(now);
        self.assembler.clear();
        if self.outbox.burst(now) {
            self.burst_drops.push(now);
            self.burst_drops
                .retain(|t| now.duration_since(*t) <= BACKOFF_WINDOW);
            if self.burst_drops.len() >= 2 {
                self.burst_drops.clear();
                self.outbox.backoff(now);
                self.stats.pace_backoffs += 1;
                let pace_ms = self.outbox.pace_at(now).as_millis() as u16;
                tracing::warn!(
                    pace_ms,
                    "overflow signature: two drops after bursts; pace backed off"
                );
                self.out
                    .push(FromHid::Link(LinkEvent::PaceBackoff { pace_ms }));
            }
        }
    }

    fn session_step(&mut self, input: Input, now: Instant) {
        for action in self.session.step(input, now) {
            match action {
                Action::SendHello => {
                    self.outbox.control(Control::Hello);
                    self.stats.hello_sent += 1;
                }
                Action::StartSync { boot } => {
                    self.outbox.begin_barrier(&self.view, boot);
                    self.stats.barriers += 1;
                    tracing::info!(
                        boot,
                        sid = self.session.sid(),
                        "full sync: writing the barrier"
                    );
                }
                Action::Reopen => {
                    tracing::warn!("six hello misses: dropping the handle to re-enumerate");
                    self.stats.reopens += 1;
                    self.dropped(now);
                    self.drop_handle = true;
                }
                Action::Report(state) => {
                    // Lane traffic only once the device answered (Syncing/Live);
                    // before that and while degraded, hello alone goes out.
                    self.outbox
                        .hold_lanes(!matches!(state, SessionState::Syncing | SessionState::Live));
                    let event = match state {
                        SessionState::Absent => LinkEvent::Down,
                        SessionState::Linked | SessionState::AwaitHello => LinkEvent::Linked,
                        SessionState::Syncing => {
                            let (sid, boot) = self.last_reply.unwrap_or((0, false));
                            LinkEvent::Syncing { sid, boot }
                        }
                        SessionState::Live => LinkEvent::Live {
                            sid: self.session.sid(),
                        },
                        SessionState::Degraded => LinkEvent::Degraded,
                    };
                    tracing::info!(?state, "session");
                    self.out.push(FromHid::Link(event));
                }
            }
        }
        let misses = self.session.misses();
        if misses > self.misses_seen {
            self.stats.hello_misses_total += misses - self.misses_seen;
        }
        self.misses_seen = misses;
    }

    /// One input report: demux by channel (§6.2).
    pub fn on_report(&mut self, report: &[u8], now: Instant) {
        self.stats.reports_received += 1;
        match channel_of(report) {
            Some(deck_proto::CHANNEL) => {
                let mut lines = Vec::new();
                self.assembler
                    .push_report(report, &mut |line| lines.push(line));
                for line in lines {
                    match line {
                        Ok(line) => self.on_line(line, now),
                        Err(e) => {
                            self.stats.decode_errors += 1;
                            tracing::warn!(error = %e, "device line dropped");
                        }
                    }
                }
            }
            Some(2) => self.stats.ch2_reports += 1,
            Some(4) => self.stats.ch4_reports += 1,
            Some(crate::ble::codec::CHANNEL) => {
                self.stats.voice_reports += 1;
                if let Some(port) = self.audio.as_mut() {
                    port.on_report(report);
                }
            }
            _ => self.stats.other_reports += 1,
        }
    }

    fn on_line(&mut self, line: DeviceLine, now: Instant) {
        self.stats.lines_received += 1;
        if let DeviceLine::Answer { slot, card, .. } = &line {
            self.start_answer_clock(*slot, *card, now);
        }
        // The core sees the line before any link event it causes (a hello's
        // fw and faces land in the store before the session changes).
        self.out.push(FromHid::Line(line.clone()));
        match &line {
            DeviceLine::Hello {
                sid, boot, faces, ..
            } => {
                self.stats.hello_replies += 1;
                self.view.set_faces(faces.clone());
                if !boot {
                    // boot:0 → the device's strip is the truth (P:95-99).
                    let faces_line = HostLine::Faces {
                        list: faces.clone(),
                    };
                    self.view
                        .apply(&DeviceOp::Put(Key::Faces, faces_line.clone()));
                    self.outbox.shadow_put(Key::Faces, faces_line);
                }
                self.last_reply = Some((*sid, *boot));
                self.session_step(
                    Input::HelloReply {
                        sid: *sid,
                        boot: *boot,
                    },
                    now,
                );
                self.outbox.reconsider(&self.view, now);
            }
            DeviceLine::Faces { list } => {
                self.view.set_faces(list.clone());
                let faces_line = HostLine::Faces { list: list.clone() };
                self.view
                    .apply(&DeviceOp::Put(Key::Faces, faces_line.clone()));
                self.outbox.shadow_put(Key::Faces, faces_line);
                self.outbox.reconsider(&self.view, now);
            }
            DeviceLine::Face { f } => {
                self.view.set_visible_face(Some(*f));
                self.outbox.reconsider(&self.view, now);
            }
            DeviceLine::Set { key, value } => {
                if key.accepts(*value) {
                    let set = HostLine::Set {
                        key: *key,
                        value: *value,
                    };
                    self.view.apply(&DeviceOp::Put(Key::Set(*key), set.clone()));
                    self.outbox.shadow_put(Key::Set(*key), set);
                }
            }
            DeviceLine::Err { why } => {
                match why {
                    ErrWhy::Size => self.stats.err_size += 1,
                    ErrWhy::Parse => self.stats.err_parse += 1,
                    ErrWhy::Range => self.stats.err_range += 1,
                    ErrWhy::Unknown => self.stats.err_unknown += 1,
                }
                // Redacted: the `m` and byte length of the last lines written.
                tracing::warn!(?why, recent = ?self.outbox.recent(3), "device refused a line");
            }
            _ => {}
        }
    }

    /// Periodic work: the hello timers, and the cable port's LISTEN
    /// deadline.
    pub fn tick(&mut self, now: Instant) {
        if !self.linked {
            return;
        }
        self.session_step(Input::Tick, now);
        if let Some(port) = self.audio.as_mut() {
            port.tick(now);
        }
    }

    /// The cable port's next report, if it has one: the LISTEN it owes, or
    /// a voice frame while the cable is the port in use. Outside the
    /// outbox's pace: a speaker frame every 7 ms is what the cable is for.
    fn next_voice_report(&mut self, now: Instant) -> Option<[u8; REPORT_LEN]> {
        if !self.linked {
            return None;
        }
        self.audio.as_mut().and_then(|port| port.next_report(now))
    }

    /// The next report to write, if the pacer allows one.
    pub fn next_report(&mut self, now: Instant) -> Option<[u8; REPORT_LEN]> {
        if !self.linked {
            return None;
        }
        self.outbox.next_report(&self.view, now)
    }

    /// The deck reported an `answer`: the §4.8 budget starts here, and ends
    /// when the `card clear` for that (slot, card) is handed to the writer.
    fn start_answer_clock(&mut self, slot: u8, card: u16, now: Instant) {
        // An answer whose clear never went out is forgotten: the store has
        // failed the card long before, and the map must stay bounded.
        self.answers
            .retain(|_, at| now.saturating_duration_since(*at) < ANSWER_CLEAR_MAX_AGE);
        while self.answers.len() >= ANSWERS_TRACKED_MAX {
            let Some(oldest) = self
                .answers
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.answers.remove(&oldest);
        }
        self.answers.insert((slot, card), now);
    }

    /// A report is on its way to the writer: count it in flight and, when it
    /// is the `card clear` of a timed answer, close the host half of the
    /// answer→clear budget and open the wire half.
    pub fn note_handed(&mut self, report: &[u8; REPORT_LEN], now: Instant) {
        self.writes_in_flight = self.writes_in_flight.saturating_add(1);
        self.stats.writes_in_flight_max =
            self.stats.writes_in_flight_max.max(self.writes_in_flight);
        if self.answers.is_empty() {
            return;
        }
        let Some(payload) = report_payload(report) else {
            return;
        };
        if !payload.ends_with(CLEAR_TAIL) {
            return;
        }
        let matched = self
            .answers
            .keys()
            .find(|(slot, card)| clear_line(*slot, *card).as_deref() == Some(payload))
            .copied();
        let Some(at) = matched.and_then(|key| self.answers.remove(&key)) else {
            return;
        };
        let us = micros(now.saturating_duration_since(at));
        self.stats.answer_to_clear_host_us = us;
        self.stats.answer_to_clear_host_max_us = self.stats.answer_to_clear_host_max_us.max(us);
        self.wire_pending = Some(at);
    }

    /// How long the last `hid_write` blocked.
    pub fn note_write(&mut self, took: Duration) {
        let us = u64::from(micros(took));
        self.writes_in_flight = self.writes_in_flight.saturating_sub(1);
        self.writes += 1;
        self.write_total_us += us;
        self.stats.write_max_us = self.stats.write_max_us.max(us as u32);
        if took >= SLOW_WRITE {
            self.stats.writes_slow += 1;
        }
    }

    /// Drain whatever the writer finished and detect a hung write; `Some`
    /// is the reason the link must go down (§3.6).
    pub fn poll_writer(
        &mut self,
        writer: &mut crate::device::writer::Writer,
        now: Instant,
    ) -> Option<&'static str> {
        for Written { took, ok } in writer.drain() {
            self.note_write(took);
            if !ok {
                return Some("write");
            }
            self.after_write(now);
        }
        if writer.lost() {
            return Some("write");
        }
        if writer.stalled(now) {
            self.stats.write_stalls += 1;
            return Some("write_stall");
        }
        None
    }

    /// Hand the writer its next report while it is idle: a voice report
    /// first (the cable's microphone and speaker keep a beat the deck lines
    /// do not), else the outbox's next due report. `Some` is the reason the
    /// link must go down.
    pub fn pump_writer(
        &mut self,
        writer: &mut crate::device::writer::Writer,
        now: Instant,
    ) -> Option<&'static str> {
        if !writer.idle() {
            return None;
        }
        let (report, voice) = match self.next_voice_report(now) {
            Some(report) => (report, true),
            None => (self.next_report(now)?, false),
        };
        if writer.write(report, now) {
            self.voice_in_flight = voice;
            self.note_handed(&report, now);
            None
        } else {
            // The thread is gone (it never refuses while idle).
            Some("write")
        }
    }

    /// After a successful write: the wire half of the answer→clear budget
    /// closes here, and a finished barrier makes the session live. A voice
    /// report was none of the outbox's: its pace and barrier stand.
    pub fn after_write(&mut self, now: Instant) {
        if std::mem::take(&mut self.voice_in_flight) {
            return;
        }
        if let Some(at) = self.wire_pending.take() {
            let us = micros(now.saturating_duration_since(at));
            self.stats.answer_to_clear_wire_us = us;
            self.stats.answer_to_clear_wire_max_us = self.stats.answer_to_clear_wire_max_us.max(us);
        }
        self.outbox.report_written();
        if self.outbox.take_barrier_done() {
            let sid = self.session.sid();
            tracing::info!(sid, "synced");
            self.out.push(FromHid::Link(LinkEvent::Synced { sid }));
            self.session_step(Input::BarrierDone, now);
        }
    }

    /// How long the thread may park before the next write is due.
    ///
    /// Never zero while linked. `Outbox::next_write_in` answers "now" for
    /// anything queued once the pace has elapsed, but a report is only
    /// actually due when the loop can hand it over: `writer_idle` is false
    /// for the whole BLE round trip (60–360 ms, §6.1) and `next_report` can
    /// decline a queued line anyway (held lanes, a spent title bucket, a
    /// gated face). Both cases answered zero before, and a zero timeout on
    /// `ops` is a busy spin — so the input poll is the floor and the
    /// writer's `done` channel (`park`) is what actually ends the wait.
    pub fn sleep_for(&self, now: Instant, writer_idle: bool) -> Duration {
        if !self.linked {
            return ENUMERATE_EVERY;
        }
        if !writer_idle {
            return POLL;
        }
        // A voice report is due now; the port's wake-up ends the wait early
        // for a frame queued later.
        if self.audio.as_ref().is_some_and(crate::ble::UsbPort::is_due) {
            return Duration::from_micros(1);
        }
        match self.outbox.next_write_in(now) {
            Some(due) if due > Duration::ZERO => POLL.min(due),
            _ => POLL,
        }
    }

    pub fn metric(&mut self, now: Instant) -> Metric {
        let depth = self.outbox.depths(now);
        let flood = self.outbox.flood_state();
        Metric {
            session: self.session.state(),
            pace_ms: self.outbox.pace_at(now).as_millis() as u16,
            backoff: self.outbox.in_backoff(now),
            outbox_depth: depth.total(),
            p0: depth.p0,
            p1: depth.p1,
            p2: depth.p2,
            p3: depth.p3,
            barrier: depth.barrier,
            reports_sent: self.outbox.reports_sent(),
            lines_sent: self.outbox.lines_sent(),
            encode_errors: self.outbox.encode_errors(),
            hello_misses: self.session.misses(),
            flood_sent: flood.sent,
            flood_remaining: flood.remaining,
            write_avg_us: self.write_total_us.checked_div(self.writes).unwrap_or(0) as u32,
            ..self.stats
        }
    }
}

/// Spawn the thread: enumerate every 800 ms, poll at ≤2 ms, pace the writes.
pub fn spawn(cfg: HidConfig) -> HidHandle {
    let (ops_tx, ops_rx) = crossbeam_channel::bounded::<DeviceOp>(OPS_CAPACITY);
    let (lines_tx, lines_rx) = crossbeam_channel::unbounded::<FromHid>();
    let (debug_tx, debug_rx) = crossbeam_channel::unbounded::<HidDebug>();
    let maintenance = Maintenance::default();
    let thread_maintenance = maintenance.clone();
    let join = std::thread::Builder::new()
        .name("deckd-hid".into())
        .spawn(move || thread_main(cfg, ops_rx, lines_tx, debug_rx, thread_maintenance))
        .ok();
    HidHandle {
        maintenance,
        ops: ops_tx,
        lines: lines_rx,
        debug: debug_tx,
        join,
    }
}

fn thread_main(
    cfg: HidConfig,
    ops: Receiver<DeviceOp>,
    to_core: Sender<FromHid>,
    debug: Receiver<HidDebug>,
    maintenance: Maintenance,
) {
    let mut link = Link::new(&cfg);
    let mut api: Option<HidApi> = None;
    let mut device: Option<HidDevice> = None;
    // The `deckd-hid-write` thread and its own open of the deck; `None`
    // whenever no device is open. A stall abandons it, so the next open
    // spawns a new one (§3.6).
    let mut writer: Option<Writer> = None;
    let mut enumerated: Option<Instant> = None;
    let mut last_open_error: Option<String> = None;
    let mut presence_checked = Instant::now();
    let mut reported = Instant::now();
    loop {
        let now = Instant::now();
        loop {
            match ops.try_recv() {
                Ok(op) => link.apply(op, now),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // Let the deck stop streaming to a host that is going:
                    // the farewell rides the close budget.
                    if let (Some(bye), Some(w)) = (link.farewell(), writer.as_mut()) {
                        if w.idle() {
                            w.write(bye, now);
                        }
                    }
                    close_writer(writer.take(), CLOSE_BUDGET);
                    return;
                }
            }
        }
        while let Ok(cmd) = debug.try_recv() {
            link.debug(cmd, now);
        }
        // The cable port's wake-up is a level, not a count: one token
        // however many frames were queued, drained here, acted on below.
        if let Some(wake) = link.audio_wake() {
            while wake.try_recv().is_ok() {}
        }
        if maintenance.requested.load(Ordering::Acquire) {
            if device.take().is_some() {
                link.link_down(now, "firmware_update");
            }
            // A blocked writer must not overlap an updater. Leave ready false
            // if it cannot close; the update times out before sending anything.
            if let Some(w) = writer.take() {
                if !w.close(CLOSE_BUDGET) {
                    for msg in link.drain() { let _ = to_core.send(msg); }
                    while maintenance.requested.load(Ordering::Acquire) {
                        std::thread::sleep(POLL);
                    }
                    continue;
                }
            }
            maintenance.ready.store(true, Ordering::Release);
            for msg in link.drain() { let _ = to_core.send(msg); }
            std::thread::sleep(POLL);
            continue;
        }
        maintenance.ready.store(false, Ordering::Release);
        if device.is_none() {
            if cfg.hardware && enumerated.is_none_or(|t| now.duration_since(t) >= ENUMERATE_EVERY) {
                enumerated = Some(now);
                let opened = match api.as_mut() {
                    Some(api) => open_pair(api),
                    None => match HidApi::new() {
                        Ok(a) => open_pair(api.insert(a)),
                        Err(_) => Err(HidError::Init),
                    },
                };
                match opened {
                    Ok((read, write, instance)) => {
                        tracing::info!("device opened (non-exclusive, read + write handles)");
                        device = Some(read);
                        writer = Some(crate::device::writer::spawn_device(write));
                        presence_checked = now;
                        last_open_error = None;
                        link.link_up(now);
                        if matches!(instance.bus, hidapi::BusType::Usb) {
                            link.cable_up(instance.serial.as_deref());
                        }
                    }
                    Err(HidError::NotFound) => {}
                    Err(e) => {
                        let text = e.to_string();
                        if last_open_error.as_deref() != Some(&text) {
                            tracing::warn!("{text}");
                            last_open_error = Some(text);
                        }
                    }
                }
            }
        } else {
            let mut lost: Option<&str> = None;
            if let Some(dev) = device.as_ref() {
                match drain(dev, DRAIN_MAX) {
                    Ok(reports) => {
                        if !reports.is_empty() {
                            presence_checked = now;
                        }
                        for report in &reports {
                            link.on_report(report, now);
                        }
                    }
                    Err(_) => lost = Some("read"),
                }
            }
            // Whatever the writer finished since the last iteration, and a
            // write that never returned.
            if let Some(w) = writer.as_mut() {
                if let Some(why) = link.poll_writer(w, now) {
                    lost = lost.or(Some(why));
                }
            }
            if lost.is_none() && now.duration_since(presence_checked) >= PRESENCE_EVERY {
                presence_checked = now;
                if api.as_mut().is_some_and(|api| !present(api)) {
                    lost = Some("removed");
                }
            }
            if lost.is_none() {
                link.tick(now);
                if link.take_drop_handle() {
                    device = None;
                    close_writer(writer.take(), CLOSE_BUDGET);
                }
            }
            if lost.is_none() && device.is_some() {
                if let Some(w) = writer.as_mut() {
                    lost = link.pump_writer(w, now);
                }
            }
            if let Some(why) = lost {
                device = None;
                // A stall is a write already five seconds overdue: abandon
                // it at once rather than spend the budget on the hot path.
                let budget = if why == "write_stall" {
                    Duration::ZERO
                } else {
                    CLOSE_BUDGET
                };
                close_writer(writer.take(), budget);
                link.link_down(now, why);
            }
        }
        for msg in link.drain() {
            if to_core.send(msg).is_err() {
                close_writer(writer.take(), CLOSE_BUDGET);
                return;
            }
        }
        if now.duration_since(reported) >= crate::metrics::REPORT_EVERY {
            reported = now;
            let m = link.metric(now);
            if to_core.send(FromHid::Metric(m)).is_err() {
                close_writer(writer.take(), CLOSE_BUDGET);
                return;
            }
        }
        // Park until there is something to do. Whatever woke us — an op, a
        // finished write, the core dropping the channel — is drained at the
        // top of the next pass, so nothing is consumed here.
        let idle = writer.as_ref().is_none_or(Writer::idle);
        let wait = link.sleep_for(Instant::now(), idle);
        let _ = park(&ops, writer.as_ref(), link.audio_wake(), wait);
    }
}

/// Why the loop woke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Woke {
    /// An op is waiting, or the core dropped the channel.
    Op,
    /// The write in flight returned, or its thread ended.
    Write,
    /// A voice frame was queued for the cable.
    Voice,
    /// Nothing happened before the deadline.
    Timeout,
}

/// Wait for an op, for the in-flight write to return, for a voice frame
/// queued for the cable, or for `wait`.
///
/// The writer's `done` channel belongs in the wait: §3.6 moved the blocking
/// `hid_write` off this thread, so from the pace elapsing until the BLE
/// round trip returns the outbox has a report due that nobody may hand over.
/// Waiting on `ops` alone with the outbox's timeout spun the thread at 100 %
/// for the length of every write. Nothing is consumed here (a `Written` must
/// reach `Writer::drain`, which is what clears the in-flight mark; the
/// voice wake-up is drained at the top of the pass), so a spurious ready
/// costs one harmless pass.
fn park(
    ops: &Receiver<DeviceOp>,
    writer: Option<&Writer>,
    voice: Option<&Receiver<()>>,
    wait: Duration,
) -> Woke {
    let mut select = crossbeam_channel::Select::new();
    let op_arm = select.recv(ops);
    // Only while a write is actually in flight: an idle writer's `done` is
    // empty forever, and a closed one would be ready forever.
    let write_arm = writer
        .filter(|w| !w.idle())
        .map(|w| select.recv(&w.done));
    if let Some(voice) = voice {
        let _ = select.recv(voice);
    }
    match select.ready_timeout(wait) {
        Ok(arm) if arm == op_arm => Woke::Op,
        Ok(arm) if Some(arm) == write_arm => Woke::Write,
        Ok(_) => Woke::Voice,
        Err(crossbeam_channel::ReadyTimeoutError) => Woke::Timeout,
    }
}

/// The read handle for the hid loop and a second, write-only one for the
/// writer thread: `HidDevice` is `Send` but not `Sync` in hidapi 2.6.7, so
/// the two threads cannot share one (§3.6). Both are non-exclusive.
fn open_pair(api: &mut HidApi) -> Result<(HidDevice, HidDevice, Instance), HidError> {
    let found = instances(api)?;
    let chosen = pick(&found).ok_or(HidError::NotFound)?.clone();
    // Both handles on the one instance: a second enumeration could list the
    // other transport first and split the session across two decks.
    let read = api.open_path(&chosen.path).map_err(|_| HidError::Open)?;
    let write = api.open_path(&chosen.path).map_err(|_| HidError::Open)?;
    tracing::info!(
        "device: {:?} instance chosen out of {} ({})",
        chosen.bus,
        found.len(),
        found
            .iter()
            .map(|i| format!("{:?}", i.bus).to_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok((read, write, chosen))
}

/// Close a writer within `budget`, abandoning its thread if it outlasts it.
///
/// Every close is bounded, shutdown included: the thread may be inside a
/// `hid_write` that never returns, and waiting it out would mean `deckd-hid`
/// never ends and `deckd run` never answers SIGTERM. A writer already known
/// to be stalled gets `Duration::ZERO` (it is abandoned at once, on the hid
/// loop's hot path); every other close gets `CLOSE_BUDGET`, which a healthy
/// write finishes inside.
fn close_writer(writer: Option<Writer>, budget: Duration) {
    if let Some(w) = writer {
        if !w.close(budget) {
            tracing::warn!("hid write did not return: abandoning the writer thread");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn usb_instance_is_preferred_over_bluetooth_and_first_otherwise() {
        use super::{pick, Instance};
        use hidapi::BusType;
        use std::ffi::CString;
        let inst = |path: &str, bus| Instance {
            path: CString::new(path).unwrap(),
            bus,
            serial: None,
        };
        let both = [inst("bt", BusType::Bluetooth), inst("usb", BusType::Usb)];
        assert_eq!(pick(&both).unwrap().path.to_str().unwrap(), "usb");
        let bt_only = [
            inst("bt1", BusType::Bluetooth),
            inst("bt2", BusType::Bluetooth),
        ];
        assert_eq!(pick(&bt_only).unwrap().path.to_str().unwrap(), "bt1");
        let unknown = [inst("x", BusType::Unknown)];
        assert_eq!(pick(&unknown).unwrap().path.to_str().unwrap(), "x");
        assert!(pick(&[]).is_none());
    }

    use super::*;
    use deck_model::{
        AgentState, CardKind, FaceEntry, FaceId, Kind, SetKey, Slot, SyncReason, Template,
    };

    pub(crate) fn cfg() -> HidConfig {
        HidConfig {
            pace: Duration::from_millis(8),
            hello_every: Duration::from_secs(5),
            sid: 314_159,
            clock_local: false,
            hardware: false,
            audio: None,
        }
    }

    /// A device line as the input reports hidapi would hand us.
    fn reports(json: &str) -> Vec<[u8; 64]> {
        let mut c = deck_proto::Chunker::new();
        c.push_line(json.as_bytes());
        std::iter::from_fn(move || c.next_report()).collect()
    }

    pub(crate) fn feed(link: &mut Link, json: &str, now: Instant) {
        for r in reports(json) {
            link.on_report(&r, now);
        }
    }

    /// Write everything due from `t`, stepping the clock 8 ms per report.
    pub(crate) fn written(link: &mut Link, t: &mut Instant) -> Vec<String> {
        let mut lines = Vec::new();
        let mut partial = Vec::new();
        let mut idle = 0;
        while idle < 2 {
            link.tick(*t);
            match link.next_report(*t) {
                Some(r) => {
                    idle = 0;
                    link.after_write(*t);
                    let n = r[2] as usize;
                    partial.extend_from_slice(&r[3..3 + n]);
                    if partial.last() == Some(&b'\n') {
                        partial.pop();
                        lines.push(String::from_utf8(std::mem::take(&mut partial)).unwrap());
                    }
                }
                None => idle += 1,
            }
            *t += Duration::from_millis(8);
        }
        lines
    }

    fn links(link: &mut Link) -> Vec<LinkEvent> {
        link.drain()
            .into_iter()
            .filter_map(|m| match m {
                FromHid::Link(l) => Some(l),
                _ => None,
            })
            .collect()
    }

    pub(crate) const HELLO_BOOT: &str = r#"{"m":"hello","v":1,"fw":"0.1.0","sid":0,"boot":1,"rx":1024,"faces":[{"f":1,"t":"agents","n":"Agents"},{"f":2,"t":"omarchy","n":"Omarchy"}]}"#;
    pub(crate) const HELLO_MINE: &str = r#"{"m":"hello","v":1,"fw":"0.1.0","sid":314159,"boot":0,"rx":1024,"faces":[{"f":1,"t":"agents","n":"Agents"}]}"#;

    /// A host-side write failure (no BLE disconnect): the device keeps
    /// answering our sid, so no barrier repairs the lanes; the cut line
    /// itself must be written again (§12 "resync on return").
    #[test]
    fn a_line_cut_by_a_host_side_drop_is_resent_when_the_device_keeps_our_sid() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        assert_eq!(link.session().state(), SessionState::Live);
        link.drain();
        let reply = HostLine::Reply {
            slot: 1,
            text: "a".repeat(100),
            rev: 1,
        };
        link.apply(DeviceOp::Put(Key::Reply(Slot(1)), reply.clone()), t);
        // The first of three reports is written; the second fails.
        assert!(link.next_report(t).is_some());
        link.after_write(t);
        link.link_down(t, "write");
        assert!(!link.is_linked());
        t += Duration::from_millis(800);
        link.link_up(t);
        assert_eq!(
            written(&mut link, &mut t),
            ["{\"m\":\"hello\",\"v\":1,\"sid\":314159}"]
        );
        feed(&mut link, HELLO_MINE, t);
        assert_eq!(link.session().state(), SessionState::Live);
        let lines = written(&mut link, &mut t);
        assert_eq!(
            lines,
            [format!(
                "{{\"m\":\"reply\",\"agent\":1,\"text\":\"{}\",\"rev\":1}}",
                "a".repeat(100)
            )]
        );
        let m = link.metric(t);
        assert_eq!(m.barriers, 0, "same sid: no barrier");
        assert_eq!(m.link_drops, 1);
        assert_eq!(link.outbox().depth(), 0);
    }

    /// `boot:1`: the barrier restores the persisted strip, so `macro`/`ws`
    /// are gated on that strip, not on the power-on defaults the hello
    /// reported (the device never echoes a host `faces`, P:102-104).
    #[test]
    fn a_boot_barrier_gates_macro_and_ws_on_the_strip_it_restores() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        let strip = vec![
            FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "Agents".into(),
            },
            FaceEntry {
                f: 7,
                t: Template::Macropad,
                n: "Build".into(),
            },
        ];
        link.apply(
            DeviceOp::Put(Key::Faces, HostLine::Faces { list: strip }),
            t,
        );
        link.apply(
            DeviceOp::Put(
                Key::Macro(FaceId(7)),
                HostLine::Macro {
                    face: 7,
                    src: None,
                    keys: vec![],
                },
            ),
            t,
        );
        link.apply(
            DeviceOp::Put(
                Key::Ws,
                HostLine::Ws {
                    active: 1,
                    count: 1,
                    sel: 0,
                    windows: vec![],
                },
            ),
            t,
        );
        link.link_up(t);
        written(&mut link, &mut t);
        feed(&mut link, HELLO_BOOT, t);
        let lines = written(&mut link, &mut t);
        let at = |m: &str| lines.iter().position(|l| l.starts_with(m));
        let macro_at = at("{\"m\":\"macro\",\"f\":7").unwrap_or_else(|| panic!("{lines:?}"));
        let faces_at = at("{\"m\":\"faces\"").unwrap();
        assert!(macro_at < faces_at, "§6.5 order: macro before faces");
        assert!(
            at("{\"m\":\"ws\"").is_none(),
            "no Omarchy face in the restored strip: {lines:?}"
        );
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
        assert_eq!(
            link.view().faces().len(),
            2,
            "the device's own report is kept"
        );
        // The lanes follow the restored strip too, before the next hello.
        link.drain();
        link.apply(
            DeviceOp::Put(
                Key::Macro(FaceId(7)),
                HostLine::Macro {
                    face: 7,
                    src: Some("x".into()),
                    keys: vec![],
                },
            ),
            t,
        );
        assert_eq!(written(&mut link, &mut t).len(), 1);
        link.apply(
            DeviceOp::Put(
                Key::Macro(FaceId(2)),
                HostLine::Macro {
                    face: 2,
                    src: None,
                    keys: vec![],
                },
            ),
            t,
        );
        assert!(written(&mut link, &mut t).is_empty(), "no Macropad at f2");
    }

    #[test]
    fn a_link_up_hellos_a_zero_sid_writes_the_barrier_and_synced_goes_live() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.apply(
            DeviceOp::Put(
                Key::AgentState(Slot(1)),
                HostLine::Agent {
                    slot: 1,
                    kind: Some(Kind::Codex),
                    name: Some("a".into()),
                    project: None,
                    state: Some(AgentState::Working),
                    title: None,
                    since_s: Some(2),
                    task: None,
                    subs: None,
                },
            ),
            t,
        );
        link.apply(
            DeviceOp::Put(
                Key::Ws,
                HostLine::Ws {
                    active: 1,
                    count: 1,
                    sel: 0,
                    windows: vec![],
                },
            ),
            t,
        );
        assert!(link.next_report(t).is_none(), "nothing goes out unlinked");
        link.link_up(t);
        assert!(link.is_linked());
        assert_eq!(links(&mut link), vec![LinkEvent::Linked]);
        assert_eq!(
            written(&mut link, &mut t),
            ["{\"m\":\"hello\",\"v\":1,\"sid\":314159}"],
            "lanes wait for the device's answer"
        );
        assert!(link.outbox().lanes_held());
        feed(&mut link, HELLO_BOOT, t);
        let msgs = link.drain();
        assert!(matches!(
            &msgs[0],
            FromHid::Line(DeviceLine::Hello { boot: true, .. })
        ));
        assert_eq!(
            msgs[1],
            FromHid::Link(LinkEvent::Syncing { sid: 0, boot: true })
        );
        assert_eq!(link.view().faces().len(), 2);
        let lines = written(&mut link, &mut t);
        assert_eq!(
            lines[0],
            "{\"m\":\"agent\",\"id\":1,\"kind\":\"codex\",\"name\":\"a\",\"state\":\"working\",\"since\":2}"
        );
        assert!(
            lines[1].starts_with("{\"m\":\"ws\""),
            "face unknown, Omarchy in the strip"
        );
        assert!(
            lines
                .iter()
                .filter(|l| l.starts_with("{\"m\":\"set\""))
                .count()
                == 9,
            "boot: every set key"
        );
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
        assert_eq!(
            links(&mut link),
            vec![
                LinkEvent::Synced { sid: 314_159 },
                LinkEvent::Live { sid: 314_159 }
            ]
        );
        assert_eq!(link.session().state(), SessionState::Live);
        let m = link.metric(t);
        assert_eq!(m.hello_sent, 1);
        assert_eq!(m.hello_replies, 1);
        assert_eq!(m.barriers, 1);
        assert_eq!(m.lines_sent as usize, lines.len() + 1);
        assert_eq!(m.reports_received, 3, "the hello took three reports");
        // Heartbeat at 5 s; a reply with our sid changes nothing.
        t += Duration::from_secs(5);
        assert_eq!(
            written(&mut link, &mut t),
            ["{\"m\":\"hello\",\"v\":1,\"sid\":314159}"]
        );
        feed(&mut link, HELLO_MINE, t);
        assert!(links(&mut link).is_empty());
        assert_eq!(link.view().faces().len(), 1, "boot:0 adopts the strip");
        assert!(link.view().get(&Key::Faces).is_some());
    }

    #[test]
    fn arcade_sound_waits_for_a_compatible_device_in_boot_and_live_lanes() {
        for modern in [false, true] {
            let mut link = Link::new(&cfg());
            let mut t = Instant::now();
            let sound = |value| DeviceOp::Put(
                Key::Set(deck_proto::SetKey::Arcadesound),
                HostLine::Set { key: deck_proto::SetKey::Arcadesound, value },
            );
            link.apply(sound(2), t);
            link.link_up(t);
            written(&mut link, &mut t);
            let mut hello: serde_json::Value = serde_json::from_str(HELLO_BOOT).unwrap();
            if modern {
                hello["fw"] = "0.6.0".into();
                hello["faces"].as_array_mut().unwrap().push(serde_json::json!({"f":6,"t":"arcade","n":"Arcade"}));
            }
            feed(&mut link, &hello.to_string(), t);
            let lines = written(&mut link, &mut t);
            assert_eq!(lines.iter().any(|l| l.contains("arcadesound")), modern, "boot barrier");
            link.apply(sound(1), t);
            let lines = written(&mut link, &mut t);
            assert_eq!(lines.iter().any(|l| l.contains("arcadesound")), modern, "live settings lane");
        }
    }

    #[test]
    fn device_edits_never_echo_and_the_face_gates_ws() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        feed(&mut link, HELLO_BOOT, t);
        written(&mut link, &mut t);
        link.drain();
        // The device changes brightness: the view and shadow follow, nothing is written.
        feed(&mut link, r#"{"m":"set","k":"brightness","v":2}"#, t);
        assert!(written(&mut link, &mut t).is_empty());
        assert_eq!(
            link.view().get(&Key::Set(SetKey::Brightness)),
            Some(&HostLine::Set {
                key: SetKey::Brightness,
                value: 2
            })
        );
        // The core persists it and projects the same value back: suppressed.
        link.apply(
            DeviceOp::Put(
                Key::Set(SetKey::Brightness),
                HostLine::Set {
                    key: SetKey::Brightness,
                    value: 2,
                },
            ),
            t,
        );
        assert!(written(&mut link, &mut t).is_empty());
        // An out-of-range value from the device is ignored.
        feed(&mut link, r#"{"m":"set","k":"brightness","v":9}"#, t);
        assert_eq!(
            link.view().get(&Key::Set(SetKey::Brightness)),
            Some(&HostLine::Set {
                key: SetKey::Brightness,
                value: 2
            })
        );
        // ws while the Agents face is visible: held; Omarchy visible: sent.
        link.apply(
            DeviceOp::Put(
                Key::Ws,
                HostLine::Ws {
                    active: 2,
                    count: 2,
                    sel: 0,
                    windows: vec![],
                },
            ),
            t,
        );
        feed(&mut link, r#"{"m":"face","f":1}"#, t);
        assert!(written(&mut link, &mut t).is_empty());
        feed(&mut link, r#"{"m":"face","f":2}"#, t);
        let lines = written(&mut link, &mut t);
        assert_eq!(
            lines,
            ["{\"m\":\"ws\",\"active\":2,\"count\":2,\"sel\":0,\"windows\":[]}"]
        );
        // A faces edit is adopted and persisted by the core, not echoed.
        feed(
            &mut link,
            r#"{"m":"faces","list":[{"f":1,"t":"agents","n":"A"}]}"#,
            t,
        );
        assert!(written(&mut link, &mut t).is_empty());
        assert_eq!(link.view().faces()[0].n, "A");
        let lines: Vec<DeviceLine> = link
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                FromHid::Line(l) => Some(l),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 5, "every device line reaches the core");
        assert!(matches!(lines[4], DeviceLine::Faces { .. }));
        // A refused line is counted and logged redacted.
        feed(&mut link, r#"{"m":"err","why":"range"}"#, t);
        feed(&mut link, "not json at all", t);
        let m = link.metric(t);
        assert_eq!(m.err_range, 1);
        assert_eq!(m.decode_errors, 1);
        assert_eq!(m.lines_received, 7, "the undecodable one is not a line");
    }

    #[test]
    fn misses_degrade_reopen_and_a_foreign_sid_resyncs() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        assert_eq!(link.session().state(), SessionState::Live);
        link.drain();
        for _ in 0..3 {
            t += Duration::from_secs(5);
            written(&mut link, &mut t);
            t += Duration::from_secs(2);
            link.tick(t);
        }
        assert_eq!(link.session().state(), SessionState::Degraded);
        assert!(links(&mut link).contains(&LinkEvent::Degraded));
        assert!(link.is_linked(), "handle kept while degraded");
        // A reply from a rebooted device (foreign sid) → a barrier, then live.
        feed(
            &mut link,
            HELLO_BOOT.replace("\"sid\":0", "\"sid\":77").as_str(),
            t,
        );
        assert_eq!(
            links(&mut link),
            vec![LinkEvent::Syncing {
                sid: 77,
                boot: true
            }]
        );
        let lines = written(&mut link, &mut t);
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
        assert_eq!(link.session().state(), SessionState::Live);
        link.drain();
        // Six misses: the handle is dropped and the core hears Down.
        for _ in 0..6 {
            t += Duration::from_secs(5);
            written(&mut link, &mut t);
            t += Duration::from_secs(2);
            link.tick(t);
        }
        assert!(link.take_drop_handle());
        assert!(!link.is_linked());
        assert_eq!(links(&mut link).last(), Some(&LinkEvent::Down));
        let m = link.metric(t);
        assert_eq!(m.reopens, 1);
        assert_eq!(m.link_drops, 1);
        assert_eq!(m.hello_misses_total, 9);
        assert!(link.next_report(t).is_none());
        link.link_down(t, "twice");
        assert_eq!(
            link.metric(t).link_drops,
            1,
            "a second link_down is a no-op"
        );
    }

    #[test]
    fn two_drops_after_bursts_back_the_pace_off() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        for i in 0..2 {
            link.link_up(t);
            feed(&mut link, HELLO_BOOT, t);
            written(&mut link, &mut t);
            link.debug(HidDebug::Flood(45), t);
            written(&mut link, &mut t);
            link.link_down(t, "test");
            let events = links(&mut link);
            if i == 0 {
                assert!(!events.contains(&LinkEvent::PaceBackoff { pace_ms: 10 }));
            } else {
                assert!(events.contains(&LinkEvent::PaceBackoff { pace_ms: 10 }));
            }
            t += Duration::from_secs(10);
        }
        link.note_write(Duration::from_millis(3));
        link.note_write(Duration::from_millis(401));
        let m = link.metric(t);
        assert_eq!(m.pace_backoffs, 1);
        assert_eq!(m.pace_ms, 10);
        assert!(m.backoff);
        assert_eq!(m.flood_sent, 90);
        assert_eq!(
            (m.write_avg_us, m.write_max_us, m.writes_slow),
            (202_000, 401_000, 1)
        );
        assert_eq!(
            link.sleep_for(t, true),
            ENUMERATE_EVERY,
            "unlinked: wait for the enumeration"
        );
    }

    #[test]
    fn a_manual_sync_and_a_metric_on_demand() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.debug(HidDebug::Sync, t);
        assert!(!link.outbox().in_barrier(), "not linked: ignored");
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        link.drain();
        link.apply(
            DeviceOp::Sync {
                boot: false,
                reason: SyncReason::Manual,
            },
            t,
        );
        assert_eq!(written(&mut link, &mut t), ["{\"m\":\"synced\"}"]);
        assert_eq!(links(&mut link), vec![LinkEvent::Synced { sid: 314_159 }]);
        link.debug(HidDebug::Metric, t);
        assert!(matches!(link.drain()[..], [FromHid::Metric(_)]));
        assert!(link.sleep_for(t, true) <= POLL);
        assert_eq!(channel_of(&[6, 5, 0]), Some(5));
        assert_eq!(channel_of(&[3, 0]), Some(3));
        assert_eq!(channel_of(&[]), None);
        let _ = FaceEntry {
            f: 1,
            t: Template::Agents,
            n: "x".into(),
        };
    }

    /// §3.6: the hid loop hands one report at a time to `deckd-hid-write`
    /// and never waits for it; a write that never returns is a stall after
    /// five seconds → `link_down("write_stall")`, the thread abandoned.
    #[test]
    fn a_hung_write_stalls_the_link_after_five_seconds_and_is_abandoned() {
        use crate::device::writer::WRITE_STALL;
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        let (mut writer, go, wrote) = crate::device::writer::tests::gated();
        // The hello goes to the writer, which blocks in its sink.
        assert_eq!(link.pump_writer(&mut writer, t), None);
        assert!(!writer.idle(), "one write in flight");
        let report = wrote
            .recv_timeout(Duration::from_secs(5))
            .expect("the writer thread was handed the hello");
        let n = report[2] as usize;
        assert_eq!(
            std::str::from_utf8(&report[3..3 + n]).unwrap(),
            "{\"m\":\"hello\",\"v\":1,\"sid\":314159}\n"
        );
        // Nothing else is handed over, and nothing is reported, meanwhile.
        for _ in 0..4 {
            t += Duration::from_millis(100);
            link.tick(t);
            assert_eq!(link.poll_writer(&mut writer, t), None);
            assert_eq!(link.pump_writer(&mut writer, t), None);
        }
        assert_eq!(link.metric(t).writes_in_flight_max, 1);
        // Five seconds after the hand-off it is a stall.
        t += WRITE_STALL;
        assert_eq!(link.poll_writer(&mut writer, t), Some("write_stall"));
        link.link_down(t, "write_stall");
        assert!(!link.is_linked());
        let m = link.metric(t);
        assert_eq!((m.write_stalls, m.link_drops), (1, 1));
        assert_eq!(m.writes_in_flight_max, 1, "never two at once");
        assert!(
            !writer.close(Duration::ZERO),
            "a stalled writer is abandoned, never joined"
        );
        // Release the abandoned thread so the test leaves nothing parked.
        let _ = go.send(Ok(REPORT_LEN));
    }

    /// A write the thread reports as failed (`Written{ok:false}`) drops the
    /// link, and the report it carried is still queued for the next open.
    ///
    /// The thread's answer is waited for in *real* time and the simulated
    /// clock is left where the hand-off put it: a loop that advanced `t` by
    /// a millisecond per pass crossed the five-second stall budget in a few
    /// hundred microseconds of real time, so the old version of this test
    /// asserted `"write"` and was handed `"write_stall"` — the wrong path,
    /// and red on every run.
    #[test]
    fn a_failed_write_reported_by_the_thread_drops_the_link() {
        let mut link = Link::new(&cfg());
        let t = Instant::now();
        link.link_up(t);
        let (mut writer, go, wrote) = crate::device::writer::tests::gated();
        assert_eq!(link.pump_writer(&mut writer, t), None);
        let handed = wrote
            .recv_timeout(Duration::from_secs(5))
            .expect("the writer thread was handed the hello");
        let n = handed[2] as usize;
        assert_eq!(
            std::str::from_utf8(&handed[3..3 + n]).unwrap(),
            "{\"m\":\"hello\",\"v\":1,\"sid\":314159}\n"
        );
        let queued_before = link.outbox().depth();
        go.send(Err(crate::device::writer::WriteFailed)).unwrap();
        // The thread's `Written` has to cross a thread boundary; wait for it
        // on the channel rather than by spinning the simulated clock.
        let waited = Instant::now();
        while writer.done.is_empty() {
            assert!(
                waited.elapsed() < Duration::from_secs(5),
                "the thread never reported the failed write"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!writer.stalled(t), "long before the five-second stall");
        assert_eq!(link.poll_writer(&mut writer, t), Some("write"));
        // The discriminator against `a_writer_that_ends_without_answering…`:
        // that path leaves the write in flight and answers `"write"` too.
        // Here the thread answered, so `drain` cleared the in-flight mark.
        assert!(writer.idle(), "the failure came from a drained `Written`");
        assert!(!writer.lost(), "not the thread-gone path");
        link.link_down(t, "write");
        let m = link.metric(t);
        assert_eq!((m.write_stalls, m.link_drops), (0, 1));
        // The line the failed report carried is back in the outbox.
        assert_eq!(link.outbox().depth(), queued_before + 1);
        assert!(writer.close(CLOSE_BUDGET), "the thread ended on the error");
    }

    /// The voice link's cable port rides this thread's writer: its LISTEN
    /// goes out before the hello, the deck's INFO on channel 7 makes the
    /// port ready, a queued voice frame is handed over ahead of the deck
    /// lines and inside their 8 ms pace (a finished voice write restarts no
    /// pace), the deck's microphone frames reach the link from the same
    /// channel, and the link going down takes the port with it.
    #[test]
    fn the_cable_port_writes_first_and_outside_the_pace() {
        use crate::ble::codec::{self, END_ERROR};
        use crate::ble::{LinkState, MicEvent};
        use crate::device::writer::{Writer, WriterCmd, Written};
        let (ble, _deck) = crate::ble::BleAudio::with_fake();
        let mut link = Link::new(&HidConfig {
            audio: Some(ble.usb_port()),
            ..cfg()
        });
        let t = Instant::now();
        link.link_up(t);
        link.cable_up(Some("S"));
        let ((cmd_tx, cmd_rx), (done_tx, done_rx)) = Writer::channels();
        let mut writer = Writer::new(cmd_tx, done_rx, None);
        let handed = |link: &mut Link, writer: &mut Writer| -> [u8; REPORT_LEN] {
            assert_eq!(link.pump_writer(writer, t), None);
            let Ok(WriterCmd::Write(report)) = cmd_rx.try_recv() else {
                panic!("nothing handed to the writer");
            };
            done_tx
                .send(Written {
                    took: Duration::from_micros(700),
                    ok: true,
                })
                .unwrap();
            assert_eq!(link.poll_writer(writer, t), None);
            assert!(writer.idle());
            report
        };
        // LISTEN before the hello...
        let report = handed(&mut link, &mut writer);
        assert_eq!(&report[..5], &[6, codec::CHANNEL, 2, codec::KIND_LISTEN, 1]);
        assert_eq!(ble.state(), LinkState::Attaching);
        // ...and the hello at the same instant: no pace was restarted.
        let report = handed(&mut link, &mut writer);
        assert_eq!(report[1], deck_proto::CHANNEL);
        assert_eq!(link.next_report(t), None, "the hello is the one line due");
        // The deck's INFO: the port is ready, and counted.
        let mut info = [0u8; REPORT_LEN];
        codec::report(&[codec::KIND_INFO, 1, 1, 16, 20, 3], &mut info).unwrap();
        link.on_report(&info, t);
        assert_eq!(
            ble.state(),
            LinkState::Ready {
                max_write: codec::REPORT_FRAME,
                info: Some([1, 1, 16, 20, 3])
            }
        );
        assert_eq!(link.metric(t).voice_reports, 1);
        // A frame for the deck: due now, inside the lines' pace.
        ble.mic(codec::MIC_ON, 5).unwrap();
        assert!(link.sleep_for(t, true) < POLL, "a queued voice frame ends the wait");
        let report = handed(&mut link, &mut writer);
        assert_eq!(&report[..6], &[6, codec::CHANNEL, 3, codec::KIND_MIC, 1, 5]);
        assert_eq!(link.sleep_for(t, true), POLL);
        // The deck's microphone, on the same channel.
        let mic = ble.mic_events();
        let mut start = [0u8; REPORT_LEN];
        codec::report(&[codec::KIND_START, 4, 1, 16, 20], &mut start).unwrap();
        link.on_report(&start, t);
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 4 }));
        // The link goes: so does the port, and the stream with it.
        link.link_down(t, "removed");
        assert_eq!(ble.state(), LinkState::Detached);
        assert_eq!(
            mic.try_recv(),
            Ok(MicEvent::End {
                stream: 4,
                next_seq: 0,
                reason: END_ERROR
            })
        );
        assert_eq!(link.farewell(), None, "nothing to say to a deck that is gone");
        writer.close(CLOSE_BUDGET);
    }

    /// A writer thread that ends without answering (it panicked, or was
    /// never spawned) drops the link at once: its report can never complete,
    /// and parking on that `done` would be a wait nothing ever ends.
    #[test]
    fn a_writer_that_ends_without_answering_drops_the_link_at_once() {
        let mut link = Link::new(&cfg());
        let t = Instant::now();
        link.link_up(t);
        let ((cmd_tx, cmd_rx), (done_tx, done_rx)) = crate::device::writer::Writer::channels();
        let mut writer = crate::device::writer::Writer::new(cmd_tx, done_rx, None);
        assert_eq!(link.pump_writer(&mut writer, t), None);
        assert!(!writer.idle(), "the hello is in flight");
        assert!(cmd_rx.try_recv().is_ok());
        drop(done_tx);
        assert!(writer.drain().is_empty());
        assert!(writer.lost(), "the thread is gone with a write outstanding");
        assert!(!writer.stalled(t), "long before the five-second stall");
        assert_eq!(link.poll_writer(&mut writer, t), Some("write"));
        link.link_down(t, "write");
        let m = link.metric(t);
        assert_eq!((m.write_stalls, m.link_drops), (0, 1));
        writer.close(CLOSE_BUDGET);
    }

    /// §3.6, the writer split's wait: while a report is in flight the outbox
    /// answers "due now" for the next one (the 8 ms pace has elapsed) and the
    /// loop may hand over nothing, so a wait driven by the outbox alone is
    /// zero for the whole 60–360 ms BLE round trip — a 100 % busy spin. The
    /// input poll is the floor; `park` ends the wait on the write returning.
    #[test]
    fn a_write_in_flight_is_waited_on_and_never_spun_on() {
        // The sender is held for the whole test: a dropped one would make
        // the ops arm ready at once and hide what is being measured.
        let (ops_tx, ops_rx) = crossbeam_channel::bounded::<DeviceOp>(4);
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        assert_eq!(link.session().state(), SessionState::Live);
        link.drain();
        // Queue depth 2: the sync barrier, a roster or a title flood look
        // like this, and so does the branch's own answer→clear test.
        link.apply(
            DeviceOp::Put(
                Key::AgentState(Slot(1)),
                HostLine::Agent {
                    slot: 1,
                    kind: Some(Kind::Claude),
                    name: Some("a".into()),
                    project: None,
                    state: Some(AgentState::Question),
                    title: None,
                    since_s: None,
                    task: None,
                    subs: None,
                },
            ),
            t,
        );
        link.apply(DeviceOp::Put(Key::Card(Slot(1), 7), card(7)), t);
        assert!(link.outbox().depth() >= 2);
        let (mut writer, go, wrote) = crate::device::writer::tests::gated();
        assert_eq!(link.pump_writer(&mut writer, t), None);
        assert!(!writer.idle(), "the agent line is in flight");
        assert!(wrote.recv_timeout(Duration::from_secs(5)).is_ok());
        // The pace elapses long before the BLE write returns.
        t += Duration::from_millis(8);
        assert_eq!(
            link.outbox().next_write_in(t),
            Some(Duration::ZERO),
            "the outbox says the next report is due"
        );
        assert_eq!(link.pump_writer(&mut writer, t), None, "the writer is busy");
        assert!(!writer.idle());
        assert_eq!(
            link.sleep_for(t, writer.idle()),
            POLL,
            "a busy writer must never be waited on with a zero timeout"
        );
        // The write returns: the wait ends on `done`, not on the deadline.
        go.send(Ok(REPORT_LEN)).unwrap();
        assert_eq!(
            park(&ops_rx, Some(&writer), None, Duration::from_millis(50)),
            Woke::Write
        );
        t += Duration::from_millis(40);
        assert_eq!(link.poll_writer(&mut writer, t), None);
        assert!(writer.idle());
        // And the loop's own order keeps it out of a spin: the due report is
        // handed over first, so the wait that follows is the poll again.
        assert_eq!(link.pump_writer(&mut writer, t), None);
        assert!(!writer.idle(), "the next report went out at once");
        assert_eq!(link.sleep_for(t, writer.idle()), POLL);
        go.send(Ok(REPORT_LEN)).unwrap();
        assert_eq!(
            park(&ops_rx, Some(&writer), None, Duration::from_millis(50)),
            Woke::Write
        );
        assert!(link.poll_writer(&mut writer, t).is_none());
        drop(ops_tx);
        writer.close(CLOSE_BUDGET);
    }

    /// The other zero: a line the outbox has queued but will not produce yet
    /// (lanes held until the session is live; a spent title bucket, a gated
    /// face) also answers "due now" forever.
    #[test]
    fn a_queued_line_that_cannot_go_out_yet_never_makes_the_wait_zero() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        let hello = link.next_report(t).expect("the hello is a control");
        link.note_handed(&hello, t);
        link.after_write(t);
        t += Duration::from_millis(8);
        link.apply(DeviceOp::Put(Key::Card(Slot(1), 7), card(7)), t);
        assert_eq!(link.session().state(), SessionState::AwaitHello);
        assert!(link.outbox().lanes_held());
        assert_eq!(
            link.outbox().next_write_in(t),
            Some(Duration::ZERO),
            "queued and the pace has elapsed"
        );
        assert!(link.next_report(t).is_none(), "but the lanes are held");
        assert_eq!(
            link.sleep_for(t, true),
            POLL,
            "the input poll is the floor, not zero"
        );
    }

    /// `park` waits on the ops channel and, while a write is in flight, on
    /// the writer's `done`; it consumes neither.
    #[test]
    fn the_park_wakes_on_an_op_on_a_finished_write_and_otherwise_times_out() {
        use crate::device::writer::{Writer as W, WriterCmd};
        let (ops_tx, ops_rx) = crossbeam_channel::bounded::<DeviceOp>(4);
        let ((cmd_tx, cmd_rx), (done_tx, done_rx)) = W::channels();
        let mut writer = W::new(cmd_tx, done_rx, None);
        let wait = Duration::from_millis(20);
        let started = Instant::now();
        assert_eq!(park(&ops_rx, Some(&writer), None, wait), Woke::Timeout);
        assert!(started.elapsed() >= wait, "nothing pending: the deadline");
        // An op is ready and is left in the channel for the next pass.
        ops_tx
            .send(DeviceOp::Control(Control::CardClear(Slot(1), 7)))
            .unwrap();
        assert_eq!(park(&ops_rx, Some(&writer), None, wait), Woke::Op);
        assert!(ops_rx.try_recv().is_ok(), "park consumes nothing");
        // A write in flight that has not returned: still the deadline.
        assert!(writer.write([1; REPORT_LEN], Instant::now()));
        assert_eq!(cmd_rx.try_recv(), Ok(WriterCmd::Write([1; REPORT_LEN])));
        let started = Instant::now();
        assert_eq!(park(&ops_rx, Some(&writer), None, wait), Woke::Timeout);
        assert!(started.elapsed() >= wait);
        // Once it answers, the wait ends and `drain` still sees the report.
        done_tx
            .send(Written {
                took: Duration::from_millis(80),
                ok: true,
            })
            .unwrap();
        assert_eq!(park(&ops_rx, Some(&writer), None, wait), Woke::Write);
        assert_eq!(writer.drain().len(), 1);
        assert!(writer.idle());
        // No device open: only the ops arm is in the wait.
        assert_eq!(park(&ops_rx, None, None, Duration::from_millis(1)), Woke::Timeout);
        writer.close(CLOSE_BUDGET);
    }

    /// A question card on slot 1 (the device must have seen it, or the
    /// outbox drops the clear as a no-op).
    fn card(id: u16) -> HostLine {
        HostLine::Card {
            slot: 1,
            id,
            kind: CardKind::Q,
            text: "ok?".into(),
            choices: vec!["Yes".into(), "No".into()],
            rec: Some(0),
        }
    }

    /// The two halves of the §3.6 answer→clear budget: the deck's `answer`
    /// input report → the `card clear` report handed to the writer
    /// (`_host_us`), and → that write returning (`_wire_us`).
    #[test]
    fn an_answer_is_timed_to_the_card_clear_and_to_the_write_returning() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        assert_eq!(link.session().state(), SessionState::Live);
        link.apply(
            DeviceOp::Put(
                Key::AgentState(Slot(1)),
                HostLine::Agent {
                    slot: 1,
                    kind: Some(Kind::Claude),
                    name: Some("a".into()),
                    project: None,
                    state: Some(AgentState::Question),
                    title: None,
                    since_s: None,
                    task: None,
                    subs: None,
                },
            ),
            t,
        );
        link.apply(DeviceOp::Put(Key::Card(Slot(1), 7), card(7)), t);
        assert_eq!(written(&mut link, &mut t).len(), 2, "agent then card");
        link.drain();
        // The deck answers card 7 on slot 1.
        feed(
            &mut link,
            r#"{"m":"answer","agent":1,"card":7,"choice":0}"#,
            t,
        );
        assert!(matches!(
            link.drain().first(),
            Some(FromHid::Line(DeviceLine::Answer {
                slot: 1,
                card: 7,
                choice: 0
            }))
        ));
        // 12 ms later the core's `card clear` reaches the outbox, and the
        // hid loop hands it to the writer 3 ms after that.
        t += Duration::from_millis(12);
        link.apply(DeviceOp::Control(Control::CardClear(Slot(1), 7)), t);
        t += Duration::from_millis(3);
        let report = link.next_report(t).expect("the clear is due");
        link.note_handed(&report, t);
        assert_eq!(link.metric(t).answer_to_clear_host_us, 15_000);
        assert_eq!(link.metric(t).answer_to_clear_wire_us, 0, "still writing");
        // The BLE round trip takes 40 ms.
        t += Duration::from_millis(40);
        link.note_write(Duration::from_millis(40));
        link.after_write(t);
        let m = link.metric(t);
        assert_eq!(m.answer_to_clear_host_us, 15_000);
        assert_eq!(m.answer_to_clear_host_max_us, 15_000);
        assert_eq!(m.answer_to_clear_wire_us, 55_000);
        assert_eq!(m.answer_to_clear_wire_max_us, 55_000);
        assert_eq!(m.writes_in_flight_max, 1);
        assert!(m
            .summary()
            .contains("answer→clear host=15000us (max 15000)"));
        // A second, faster answer keeps the maxima and updates "last".
        link.apply(DeviceOp::Put(Key::Card(Slot(1), 8), card(8)), t);
        assert_eq!(written(&mut link, &mut t).len(), 1);
        feed(
            &mut link,
            r#"{"m":"answer","agent":1,"card":8,"choice":1}"#,
            t,
        );
        link.apply(DeviceOp::Control(Control::CardClear(Slot(1), 8)), t);
        t += Duration::from_millis(5);
        let report = link.next_report(t).expect("the second clear is due");
        link.note_handed(&report, t);
        link.note_write(Duration::from_millis(1));
        link.after_write(t);
        let m = link.metric(t);
        assert_eq!(
            (m.answer_to_clear_host_us, m.answer_to_clear_wire_us),
            (5_000, 5_000)
        );
        assert_eq!(m.answer_to_clear_host_max_us, 15_000);
        assert_eq!(m.answer_to_clear_wire_max_us, 55_000);
        assert_eq!(m.writes_in_flight_max, 1, "one write at a time");
        // A clear for a card nobody answered times nothing.
        link.apply(DeviceOp::Put(Key::Card(Slot(1), 9), card(9)), t);
        assert_eq!(written(&mut link, &mut t).len(), 1);
        link.apply(DeviceOp::Control(Control::CardClear(Slot(1), 9)), t);
        t += Duration::from_millis(9);
        let report = link.next_report(t).expect("the third clear is due");
        link.note_handed(&report, t);
        link.after_write(t);
        let m = link.metric(t);
        assert_eq!(
            (m.answer_to_clear_host_us, m.answer_to_clear_wire_us),
            (5_000, 5_000)
        );
        // The link going down forgets every pending measurement.
        feed(
            &mut link,
            r#"{"m":"answer","agent":1,"card":10,"choice":0}"#,
            t,
        );
        assert_eq!(link.answers.len(), 1);
        link.link_down(t, "test");
        assert!(link.answers.is_empty() && link.wire_pending.is_none());
        assert_eq!(link.metric(t).writes_in_flight_max, 1);
    }

    /// The answer map cannot grow without bound: one entry per slot plus
    /// slack, and anything older than 30 s is forgotten.
    #[test]
    fn the_answer_clock_map_stays_bounded() {
        let mut link = Link::new(&cfg());
        let mut t = Instant::now();
        for card in 0..u16::from(ANSWERS_TRACKED_MAX as u8) * 4 {
            link.start_answer_clock(1, card, t);
            t += Duration::from_millis(1);
        }
        assert_eq!(link.answers.len(), ANSWERS_TRACKED_MAX);
        t += ANSWER_CLEAR_MAX_AGE;
        link.start_answer_clock(2, 1, t);
        assert_eq!(link.answers.len(), 1, "the stale ones are dropped");
    }

    #[test]
    fn the_thread_drains_ops_and_exits_with_the_sender() {
        let handle = spawn(cfg());
        handle
            .ops
            .send(DeviceOp::Sync {
                boot: false,
                reason: SyncReason::Start,
            })
            .unwrap();
        handle.debug.send(HidDebug::Metric).unwrap();
        let msg = handle
            .lines
            .recv_timeout(Duration::from_secs(5))
            .expect("a metric on demand");
        assert!(matches!(msg, FromHid::Metric(m) if m.session == SessionState::Absent));
        handle.join();
    }
}
