//! [`BleAudio`]: the handle the rest of deckd holds on the voice link, and
//! the state it shares with whatever drives the link.
//!
//! The split is the one `hid` and `audio` already have. Callers live on
//! their own threads (the voice pipeline, the bench probe) and never wait
//! on the radio: [`BleAudio::send`] queues a frame or refuses it at once,
//! [`BleAudio::state`] reads a snapshot, and everything else arrives on
//! bounded crossbeam channels. The platforms — CoreBluetooth's serial queue
//! on macOS, the hid thread for the cable ([`UsbPort`]), a [`FakeDeck`] in
//! tests — write the same shared state from their side, so nothing here
//! knows which one is behind it.
//!
//! One deck, two transports: the deck carries its voice frames over its
//! audio service on Bluetooth and over report channel 7 on its USB cable,
//! and each is a *port* of this one link. What the handle shows is the
//! port in use: the cable when it is ready, else Bluetooth
//! ([`Transport::PREFERENCE`], the deck's own order), and the outbox always
//! belongs to that port. Microphone frames are taken from whichever port
//! they arrive on; the deck sends a stream on one port only.
//!
//! Three details are deliberate:
//!
//! * **Link events never go stale.** The channel is small, and when it is
//!   full the oldest state gives way to the newest, so a reader that fell
//!   behind still ends on the current state rather than a backlog.
//! * **Mic events never stall the radio.** Decoding happens where the
//!   notification arrives, and the channel holds about four seconds of
//!   frames; past that, events are dropped and counted
//!   ([`Stats::mic_overflow`]). A consumer that far behind has lost the
//!   conversation anyway, and CoreBluetooth's queue must keep moving.
//! * **The outbox belongs to one port's connection.** Frames queue only
//!   while the link is `Ready`, are checked against the write limit that
//!   port reported, and are discarded (and counted) when the port or its
//!   connection changes, so a frame never leaks onto the next connection. A
//!   live microphone stream ends with `END_ERROR` when no port is ready.
use super::attach::Reach;
use super::codec;
use super::mic::{MicDecoder, MicEvent, MicStats};
use super::sendq::SendQueue;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use deck_model::Permission;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// About four seconds of 20 ms microphone frames.
pub const MIC_EVENTS: usize = 4 * 1000 / codec::FRAME_MS as usize;
/// Link state changes kept for a reader that is behind.
pub const LINK_EVENTS: usize = 16;

/// Which of the deck's links a port is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The audio service, over the deck's BLE connection.
    Ble,
    /// Report channel 7 of the HID vendor collection, over the cable.
    Usb,
}

impl Transport {
    pub const COUNT: usize = 2;
    /// The port in use when both are ready: the cable first, which is also
    /// where the deck sends a stream when both hosts listen.
    pub const PREFERENCE: [Transport; Self::COUNT] = [Transport::Usb, Transport::Ble];

    const fn index(self) -> usize {
        match self {
            Transport::Ble => 0,
            Transport::Usb => 1,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::Ble => "ble",
            Transport::Usb => "usb",
        }
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the link is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LinkState {
    /// Nothing attached. [`BleAudio::problem`] says why, when it is known.
    #[default]
    Detached,
    /// A deck is in hand: connecting, discovering or subscribing (or, on
    /// the cable, LISTEN sent and INFO awaited).
    Attaching,
    /// Mic frames are coming and the speaker takes writes of up to
    /// `max_write` bytes (`maximumWriteValueLengthForType:` for writes
    /// without response, i.e. `ATT_MTU - 3`, over Bluetooth; a report's
    /// payload over the cable). `info` is the info characteristic, `None`
    /// when it could not be read.
    Ready {
        max_write: usize,
        info: Option<[u8; 5]>,
    },
}

/// Why [`BleAudio::send`] refused a frame. Nothing was queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("the deck's audio link is not ready")]
    Detached,
    #[error("the speaker queue is full")]
    Full,
    /// Over the link's write limit, or over `codec::MAX_FRAME`, the largest
    /// frame the deck accepts.
    #[error("a {len}-byte frame is over the {max}-byte limit")]
    TooLong { len: usize, max: usize },
}

/// The peripheral the link is attached or attaching to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub name: Option<String>,
    /// CoreBluetooth's identifier for it (stable per Mac, not a MAC
    /// address), or the deck's USB serial number.
    pub id: String,
    pub transport: Transport,
}

impl Peer {
    /// The `voice.route` UID: the transport and the identifier,
    /// `ble:<CoreBluetooth identifier>` or `usb:<serial>`.
    #[must_use]
    pub fn uid(&self) -> String {
        format!("{}:{}", self.transport, self.id)
    }
}

impl std::fmt::Display for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name:?} ({})", self.id),
            None => write!(f, "an unnamed peripheral ({})", self.id),
        }
    }
}

/// Counters since the handle was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames handed to the radio.
    pub sent: u64,
    /// Frames refused because the queue was full, or discarded with a
    /// connection.
    pub dropped: u64,
    /// Frames waiting now.
    pub queued: usize,
    pub mic: MicStats,
    /// Mic events dropped because nobody read the channel.
    pub mic_overflow: u64,
}

/// The handle. Clones share one link; the link stops (and lets go of the
/// deck) when the last clone is dropped.
#[derive(Clone)]
pub struct BleAudio {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for BleAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleAudio")
            .field("state", &self.state())
            .field("transport", &self.transport())
            .finish_non_exhaustive()
    }
}

struct Inner {
    shared: Arc<Shared>,
    backend: Backend,
}

enum Backend {
    #[cfg(target_os = "macos")]
    CoreBluetooth(super::central::Backend),
    #[cfg(target_os = "linux")]
    BlueZ(super::bluez::Backend),
    #[cfg(windows)]
    WinRt(super::winrt::Backend),
    /// Nothing drives the Bluetooth port from here: a platform without
    /// CoreBluetooth, or a [`FakeDeck`] that a test drives by hand.
    Manual,
}

impl BleAudio {
    /// Start the daemon's link: CoreBluetooth on a private serial queue,
    /// attaching to the deck whenever the radio is on and the system holds
    /// the deck connected, for as long as a clone of this handle lives.
    /// Returns at once, `Detached`. The cable's port is [`BleAudio::usb_port`].
    ///
    /// [`Reach::Connected`]: it never scans and never connects to an
    /// advertising deck, because on an unpaired deck that is a pairing
    /// request on the operator's screen (see [`super::attach`]). A paired
    /// deck is one the system keeps connected for HID.
    ///
    /// On a platform without CoreBluetooth the handle stays `Detached`.
    #[must_use]
    pub fn spawn() -> BleAudio {
        BleAudio::spawn_with(super::DECK_NAME, Reach::Connected)
    }

    /// The bench's link, for a deck that goes by `name` (a bench board, a
    /// renamed build): [`Reach::Advertising`], so a deck that is only
    /// advertising is scanned for and connected to. The audio service is
    /// still what it attaches to; the name is how a deck without it in the
    /// system's cache is found.
    #[must_use]
    pub fn spawn_named(name: &str) -> BleAudio {
        BleAudio::spawn_with(name, Reach::Advertising)
    }

    /// A link for a deck that goes by `name`, attaching as far as `reach`
    /// allows.
    #[must_use]
    pub fn spawn_with(name: &str, reach: Reach) -> BleAudio {
        let shared = Shared::new();
        #[cfg(target_os = "macos")]
        let backend =
            Backend::CoreBluetooth(super::central::Backend::spawn(shared.clone(), name, reach));
        #[cfg(target_os = "linux")]
        let backend = {
            // BlueZ never scans or pairs here: `reach` only matters to CoreBluetooth.
            let _ = reach;
            Backend::BlueZ(super::bluez::Backend::spawn(shared.clone(), name))
        };
        #[cfg(windows)]
        let backend = {
            // Like BlueZ, WinRT here never scans or pairs.
            let _ = reach;
            Backend::WinRt(super::winrt::Backend::spawn(shared.clone(), name))
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        let backend = {
            let _ = (name, reach);
            shared.set_problem(
                Transport::Ble,
                Some("CoreBluetooth exists only on macOS".into()),
            );
            Backend::Manual
        };
        BleAudio {
            inner: Arc::new(Inner { shared, backend }),
        }
    }

    /// A handle with no radio behind it, and the deck's side of it.
    #[must_use]
    pub fn with_fake() -> (BleAudio, FakeDeck) {
        let shared = Shared::new();
        let fake = FakeDeck {
            shared: shared.clone(),
        };
        let handle = BleAudio {
            inner: Arc::new(Inner {
                shared,
                backend: Backend::Manual,
            }),
        };
        (handle, fake)
    }

    /// The cable's port of this link, for the hid thread to drive.
    #[must_use]
    pub fn usb_port(&self) -> UsbPort {
        UsbPort::new(self.inner.shared.clone())
    }

    #[must_use]
    pub fn state(&self) -> LinkState {
        self.inner.shared.state()
    }

    /// The port in use (`Some` exactly while `Ready`).
    #[must_use]
    pub fn transport(&self) -> Option<Transport> {
        self.inner.shared.active()
    }

    /// Every state change, newest last (bounded; when full, the oldest goes).
    /// Clones of the receiver compete for events: give it to one reader.
    #[must_use]
    pub fn link_events(&self) -> Receiver<LinkState> {
        self.inner.shared.links.1.clone()
    }

    /// The microphone, decoded (bounded at about four seconds; overflow is
    /// dropped and counted). Clones compete for events: one reader.
    #[must_use]
    pub fn mic_events(&self) -> Receiver<MicEvent> {
        self.inner.shared.mics.1.clone()
    }

    /// Queue one frame for the deck's speaker (a write without response
    /// over Bluetooth, a channel-7 report over the cable). It goes out as
    /// soon as the port's flow control allows, in order. Refused at once
    /// when the link is not ready, the frame is over the limit, or the
    /// queue is full.
    pub fn send(&self, frame: &[u8]) -> Result<(), SendError> {
        self.inner.shared.send(frame)?;
        match self.inner.shared.active() {
            Some(Transport::Usb) => self.inner.shared.wake_usb(),
            _ => match &self.inner.backend {
                #[cfg(target_os = "macos")]
                Backend::CoreBluetooth(backend) => backend.kick(),
                #[cfg(target_os = "linux")]
                Backend::BlueZ(backend) => backend.kick(),
                #[cfg(windows)]
                Backend::WinRt(backend) => backend.kick(),
                Backend::Manual => {}
            },
        }
        Ok(())
    }

    /// Send a MIC request: `mode` is `codec::MIC_OFF`, `MIC_ON` or
    /// `MIC_LOOPBACK`, `seconds` is clamped to the contract's 1 to 60.
    pub fn mic(&self, mode: u8, seconds: u8) -> Result<(), SendError> {
        let mut frame = [0u8; 3];
        let n = codec::mic_frame(mode, seconds.clamp(1, 60), &mut frame);
        self.send(&frame[..n])
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.inner.shared.stats()
    }

    /// The peripheral being attached or attached to (on the port in use).
    #[must_use]
    pub fn peer(&self) -> Option<Peer> {
        lock(&self.inner.shared.snapshot).peer.clone()
    }

    /// Why the link is not ready, when a platform said (cleared while
    /// attaching or ready).
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        lock(&self.inner.shared.snapshot).problem.clone()
    }

    /// Whether this process may use Bluetooth, as the link last read it
    /// (`unknown` until the radio has reported).
    #[must_use]
    pub fn bluetooth(&self) -> Permission {
        lock(&self.inner.shared.snapshot).bluetooth
    }
}

/// The deck's side of a [`BleAudio::with_fake`] handle: what the Bluetooth
/// platform would do, by hand. For tests here and in the voice pipeline.
#[derive(Clone)]
pub struct FakeDeck {
    shared: Arc<Shared>,
}

impl FakeDeck {
    /// The link attaches: `Attaching`, then `Ready`.
    pub fn attach(&self, max_write: usize, info: Option<[u8; 5]>) {
        self.shared.set_peer(
            Transport::Ble,
            Some(Peer {
                name: Some(super::DECK_NAME.into()),
                id: "FAKE".into(),
                transport: Transport::Ble,
            }),
        );
        self.shared.set_state(Transport::Ble, LinkState::Attaching);
        self.shared
            .set_state(Transport::Ble, LinkState::Ready { max_write, info });
    }

    /// The link drops: queued frames are discarded, a live mic stream ends.
    pub fn detach(&self, why: &str) {
        self.shared.set_peer(Transport::Ble, None);
        self.shared.set_problem(Transport::Ble, Some(why.into()));
        self.shared.set_state(Transport::Ble, LinkState::Detached);
    }

    /// One mic notification, as the deck sent it.
    pub fn notify(&self, value: &[u8]) {
        self.shared.notify(Transport::Ble, value);
    }

    /// The radio reported this authorization.
    pub fn bluetooth(&self, permission: Permission) {
        self.shared.set_bluetooth(permission);
    }

    /// What the radio writes now: up to `budget` queued frames, oldest
    /// first (CoreBluetooth's flow control, with `budget` writes allowed).
    #[must_use]
    pub fn written(&self, budget: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut left = budget;
        self.shared.drain_for(
            Transport::Ble,
            || {
                let ok = left > 0;
                left = left.saturating_sub(1);
                ok
            },
            |frame| out.push(frame.to_vec()),
        );
        out
    }
}

/// The cable's port of the link: what the hid thread drives when the deck
/// it opened is the USB instance.
///
/// The frames are the audio service's, one per channel-7 report
/// (`codec::report`). Attaching is one exchange: the port owes the deck a
/// LISTEN, which goes out as its next report, and the deck's INFO answer
/// makes the port `Ready` with a report's payload as the write limit. A
/// deck that never answers (firmware from before voice over USB) leaves the
/// port `Detached` with that as its problem after [`UsbPort::ANSWER_WITHIN`].
/// Input reports of channel 7 are the deck's microphone (and INFO);
/// [`UsbPort::next_report`] is what the writer sends, ahead of the deck
/// lines and outside their pace, and [`UsbPort::wake`] is ready whenever a
/// frame was queued for the cable.
#[derive(Clone)]
pub struct UsbPort {
    shared: Arc<Shared>,
    /// The cable is open (the hid thread holds the USB instance).
    attached: bool,
    /// A LISTEN owed to the deck, as the next report.
    listen: Option<bool>,
    /// When LISTEN went out, until INFO answers it.
    asked_at: Option<Instant>,
}

impl std::fmt::Debug for UsbPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsbPort")
            .field("attached", &self.attached)
            .field("state", &self.shared.port_state(Transport::Usb))
            .finish_non_exhaustive()
    }
}

impl UsbPort {
    /// How long the deck has to answer LISTEN with INFO.
    pub const ANSWER_WITHIN: Duration = Duration::from_secs(2);
    const NO_ANSWER: &'static str =
        "the deck did not answer LISTEN on the cable (firmware from before voice over USB?)";

    fn new(shared: Arc<Shared>) -> UsbPort {
        UsbPort {
            shared,
            attached: false,
            listen: None,
            asked_at: None,
        }
    }

    /// The hid thread opened the deck's USB instance (`serial` is its USB
    /// serial number): `Attaching`, with a LISTEN owed.
    pub fn attach(&mut self, serial: Option<&str>) {
        self.attached = true;
        self.listen = Some(true);
        self.asked_at = None;
        self.shared.set_peer(
            Transport::Usb,
            Some(Peer {
                name: Some(super::DECK_NAME.into()),
                id: serial.unwrap_or("unknown").into(),
                transport: Transport::Usb,
            }),
        );
        self.shared.set_problem(Transport::Usb, None);
        self.shared.set_state(Transport::Usb, LinkState::Attaching);
    }

    /// The cable went away (or was never the instance opened): `Detached`,
    /// with `why` as the port's problem when there is one to tell.
    pub fn detach(&mut self, why: Option<&str>) {
        self.attached = false;
        self.listen = None;
        self.asked_at = None;
        self.shared.set_peer(Transport::Usb, None);
        self.shared
            .set_problem(Transport::Usb, why.map(str::to_string));
        self.shared.set_state(Transport::Usb, LinkState::Detached);
    }

    /// Whether the cable is the port in use.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.shared.active() == Some(Transport::Usb)
    }

    /// Whether [`UsbPort::next_report`] has something to give: a LISTEN
    /// owed, or a frame queued while the cable is the port in use.
    #[must_use]
    pub fn is_due(&self) -> bool {
        self.attached && (self.listen.is_some() || (self.is_active() && self.shared.queued() > 0))
    }

    /// One input report. `true` when it was a channel-7 report and was
    /// taken (INFO makes the port ready; anything else is the microphone).
    pub fn on_report(&mut self, report: &[u8]) -> bool {
        let Some(frame) = codec::report_frame(report) else {
            return false;
        };
        if !self.attached {
            return true;
        }
        match codec::parse(frame) {
            codec::Frame::Info { bytes } => {
                self.asked_at = None;
                tracing::info!(
                    info = %super::hex(&bytes),
                    "deck audio link ready over USB ({})",
                    super::Info::from_bytes(bytes)
                );
                self.shared.set_state(
                    Transport::Usb,
                    LinkState::Ready {
                        max_write: codec::REPORT_FRAME,
                        info: Some(bytes),
                    },
                );
            }
            // Host to deck; a deck never sends one.
            codec::Frame::Listen { .. } => {}
            _ => self.shared.notify(Transport::Usb, frame),
        }
        true
    }

    /// The next report for the cable: the LISTEN owed, else a queued frame
    /// while the cable is the port in use.
    pub fn next_report(&mut self, now: Instant) -> Option<[u8; 64]> {
        if !self.attached {
            return None;
        }
        let mut report = [0u8; 64];
        if let Some(on) = self.listen.take() {
            let mut frame = [0u8; 2];
            let n = codec::listen_frame(on, &mut frame);
            codec::report(&frame[..n], &mut report)?;
            if on {
                self.asked_at = Some(now);
            }
            return Some(report);
        }
        let taken = Cell::new(false);
        self.shared.drain_for(
            Transport::Usb,
            || !taken.get(),
            |frame| {
                if codec::report(frame, &mut report).is_some() {
                    taken.set(true);
                }
            },
        );
        taken.get().then_some(report)
    }

    /// Periodic: a LISTEN the deck has not answered in time.
    pub fn tick(&mut self, now: Instant) {
        let Some(at) = self.asked_at else { return };
        if now.saturating_duration_since(at) < Self::ANSWER_WITHIN {
            return;
        }
        self.asked_at = None;
        tracing::warn!("{}", Self::NO_ANSWER);
        self.shared
            .set_problem(Transport::Usb, Some(Self::NO_ANSWER.into()));
        self.shared.set_state(Transport::Usb, LinkState::Detached);
    }

    /// For a graceful close: the LISTEN off that lets the deck stop
    /// streaming to a host that is going away. `None` when the deck was
    /// never listening to us.
    #[must_use]
    pub fn farewell(&mut self) -> Option<[u8; 64]> {
        if !self.attached
            || !matches!(
                self.shared.port_state(Transport::Usb),
                LinkState::Ready { .. }
            )
        {
            return None;
        }
        self.listen = Some(false);
        self.next_report(Instant::now())
    }

    /// Ready whenever a frame was queued for the cable: the hid thread
    /// waits on it beside its other wake-ups, and drains it each pass.
    #[must_use]
    pub fn wake(&self) -> &Receiver<()> {
        &self.shared.usb_wake.1
    }
}

/// The state both sides of the handle touch. Every lock is held briefly
/// and never across a call into the platform's callbacks.
pub(crate) struct Shared {
    /// What the handle shows: the port in use, or the one attaching.
    snapshot: Mutex<Snapshot>,
    ports: Mutex<[Port; Transport::COUNT]>,
    links: (Sender<LinkState>, Receiver<LinkState>),
    mics: (Sender<MicEvent>, Receiver<MicEvent>),
    decoder: Mutex<MicDecoder>,
    /// The port the live microphone stream arrives on: when that port goes
    /// away the stream died with it, whatever the other port is doing.
    stream_port: Mutex<Option<Transport>>,
    tx: Mutex<Tx>,
    mic_overflow: AtomicU64,
    /// A drain is already on its way to the platform's queue (CoreBluetooth's
    /// serial queue; the other backends drain on their own thread).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) drain_posted: AtomicBool,
    /// The hid thread's wake-up: a frame was queued for the cable.
    usb_wake: (Sender<()>, Receiver<()>),
}

#[derive(Default)]
struct Snapshot {
    state: LinkState,
    active: Option<Transport>,
    peer: Option<Peer>,
    problem: Option<String>,
    /// CoreBluetooth's authorization for this process, as last read.
    bluetooth: Permission,
}

/// One transport's own state, as its platform last set it.
#[derive(Default)]
struct Port {
    state: LinkState,
    peer: Option<Peer>,
    problem: Option<String>,
}

/// `CBManagerAuthorization` (CBManager.h): not determined 0, restricted 1,
/// denied 2, allowed always 3. Restricted (a profile, parental controls) is
/// a denial the operator cannot lift either.
#[must_use]
pub fn bluetooth_permission(raw: isize) -> Permission {
    match raw {
        0 => Permission::NotDetermined,
        1 | 2 => Permission::Denied,
        3 => Permission::Granted,
        _ => Permission::Unknown,
    }
}

#[derive(Default)]
struct Tx {
    queue: SendQueue,
    /// The largest frame the link takes; `Some` exactly while `Ready`.
    limit: Option<usize>,
}

/// A poisoned lock is still a lock: one panicking thread must not take the
/// link down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn new() -> Arc<Shared> {
        Arc::new(Shared {
            snapshot: Mutex::default(),
            ports: Mutex::default(),
            links: bounded(LINK_EVENTS),
            mics: bounded(MIC_EVENTS),
            decoder: Mutex::default(),
            stream_port: Mutex::default(),
            tx: Mutex::default(),
            mic_overflow: AtomicU64::new(0),
            drain_posted: AtomicBool::new(false),
            usb_wake: bounded(1),
        })
    }

    pub(crate) fn state(&self) -> LinkState {
        lock(&self.snapshot).state.clone()
    }

    pub(crate) fn active(&self) -> Option<Transport> {
        lock(&self.snapshot).active
    }

    fn port_state(&self, transport: Transport) -> LinkState {
        lock(&self.ports)[transport.index()].state.clone()
    }

    pub(crate) fn set_peer(&self, transport: Transport, peer: Option<Peer>) {
        lock(&self.ports)[transport.index()].peer = peer;
        self.refresh();
    }

    pub(crate) fn set_problem(&self, transport: Transport, problem: Option<String>) {
        lock(&self.ports)[transport.index()].problem = problem;
        self.refresh();
    }

    pub(crate) fn set_bluetooth(&self, permission: Permission) {
        lock(&self.snapshot).bluetooth = permission;
    }

    /// A port's new state. A port that is attaching or ready has no
    /// problem to report; a port that stops being ready takes the stream
    /// that was arriving on it with it. What follows is [`Shared::refresh`]'s.
    pub(crate) fn set_state(&self, transport: Transport, state: LinkState) {
        let ready = matches!(state, LinkState::Ready { .. });
        {
            let mut ports = lock(&self.ports);
            let port = &mut ports[transport.index()];
            if state != LinkState::Detached {
                port.problem = None;
            }
            port.state = state;
        }
        if !ready && *lock(&self.stream_port) == Some(transport) {
            self.abandon_stream();
        }
        self.refresh();
    }

    /// The live stream's END is never coming: say so.
    fn abandon_stream(&self) {
        let end = lock(&self.decoder).abandon();
        *lock(&self.stream_port) = None;
        if let Some(end) = end {
            self.deliver(end);
        }
    }

    /// The ports into the snapshot. Only a change of the shown state or of
    /// the port in use does anything: it resets the outbox for the new
    /// port (or connection), ends a live mic stream unless some port is
    /// ready, and is published.
    fn refresh(&self) {
        let (state, active, peer, problem) = {
            let ports = lock(&self.ports);
            let port = |t: Transport| &ports[t.index()];
            let active = Transport::PREFERENCE
                .into_iter()
                .find(|t| matches!(port(*t).state, LinkState::Ready { .. }));
            let attaching = Transport::PREFERENCE
                .into_iter()
                .find(|t| port(*t).state == LinkState::Attaching);
            let shown = active.or(attaching);
            let state = shown.map(|t| port(t).state.clone()).unwrap_or_default();
            let peer = shown.and_then(|t| port(t).peer.clone());
            let problem = Transport::PREFERENCE
                .into_iter()
                .find_map(|t| port(t).problem.clone());
            (state, active, peer, problem)
        };
        let changed = {
            let mut snapshot = lock(&self.snapshot);
            snapshot.peer = peer;
            snapshot.problem = if state == LinkState::Detached {
                problem
            } else {
                None
            };
            let changed = snapshot.state != state || snapshot.active != active;
            snapshot.state = state.clone();
            snapshot.active = active;
            changed
        };
        if !changed {
            return;
        }
        {
            let mut tx = lock(&self.tx);
            tx.queue.clear();
            tx.limit = match state {
                LinkState::Ready { max_write, .. } => Some(max_write.min(codec::MAX_FRAME)),
                _ => None,
            };
        }
        if !matches!(state, LinkState::Ready { .. }) {
            self.abandon_stream();
        }
        self.publish(state);
    }

    /// One mic frame, from the port it arrived on, decoded here and now.
    pub(crate) fn notify(&self, transport: Transport, value: &[u8]) {
        let mut decoder = lock(&self.decoder);
        decoder.push(value, |event| self.deliver(event));
        *lock(&self.stream_port) = decoder.live().map(|_| transport);
    }

    /// Write queued frames while `can_send` allows (see
    /// [`SendQueue::drain`]), for the port in use only: another port's
    /// platform gets nothing, however ready it is.
    pub(crate) fn drain_for(
        &self,
        transport: Transport,
        can_send: impl FnMut() -> bool,
        write: impl FnMut(&[u8]),
    ) -> usize {
        if self.active() != Some(transport) {
            return 0;
        }
        lock(&self.tx).queue.drain(can_send, write)
    }

    fn wake_usb(&self) {
        let _ = self.usb_wake.0.try_send(());
    }

    fn queued(&self) -> usize {
        lock(&self.tx).queue.len()
    }

    fn send(&self, frame: &[u8]) -> Result<(), SendError> {
        let mut tx = lock(&self.tx);
        let max = tx.limit.ok_or(SendError::Detached)?;
        if frame.len() > max {
            return Err(SendError::TooLong {
                len: frame.len(),
                max,
            });
        }
        if tx.queue.push(frame) {
            Ok(())
        } else {
            Err(SendError::Full)
        }
    }

    fn stats(&self) -> Stats {
        let (sent, dropped, queued) = {
            let tx = lock(&self.tx);
            (tx.queue.sent(), tx.queue.dropped(), tx.queue.len())
        };
        Stats {
            sent,
            dropped,
            queued,
            mic: lock(&self.decoder).stats(),
            mic_overflow: self.mic_overflow.load(Ordering::Relaxed),
        }
    }

    fn deliver(&self, event: MicEvent) {
        if self.mics.0.try_send(event).is_err() {
            self.mic_overflow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Newest last; a full channel gives up its oldest state.
    fn publish(&self, mut state: LinkState) {
        loop {
            match self.links.0.try_send(state) {
                Ok(()) => return,
                Err(TrySendError::Full(back)) => {
                    let _ = self.links.1.try_recv();
                    state = back;
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::codec::{
        audio_frame, start_frame, State, END_ERROR, FRAME_SAMPLES, KIND_INFO, KIND_LISTEN,
        MAX_FRAME, MIC_LOOPBACK, REPORT_FRAME,
    };

    const INFO: [u8; 5] = [1, 1, 16, 20, 3];

    fn audio(stream: u8, seq: u8) -> Vec<u8> {
        let mut out = [0u8; MAX_FRAME];
        let n = audio_frame(
            stream,
            seq,
            &mut State::default(),
            &[100; FRAME_SAMPLES],
            &mut out,
        );
        out[..n].to_vec()
    }

    /// A channel-7 input report as hidapi hands it over.
    fn usb_report(frame: &[u8]) -> [u8; 64] {
        let mut report = [0u8; 64];
        codec::report(frame, &mut report).unwrap();
        report
    }

    #[test]
    fn the_handle_and_the_fake_go_anywhere() {
        // The voice pipeline holds the handle on its own threads and hands
        // clones around; the tests do the same with the deck's side.
        fn shareable<T: Send + Sync + Clone + 'static>() {}
        shareable::<BleAudio>();
        shareable::<FakeDeck>();
        shareable::<UsbPort>();
        shareable::<LinkState>();
        shareable::<MicEvent>();
    }

    #[test]
    fn frames_queue_only_while_ready_and_within_the_limit() {
        let (ble, deck) = BleAudio::with_fake();
        assert_eq!(ble.state(), LinkState::Detached);
        assert_eq!(ble.transport(), None);
        assert_eq!(ble.send(&[1, 2, 3]), Err(SendError::Detached));

        deck.attach(182, Some(INFO));
        assert_eq!(
            ble.state(),
            LinkState::Ready {
                max_write: 182,
                info: Some(INFO)
            }
        );
        assert_eq!(ble.transport(), Some(Transport::Ble));
        assert_eq!(
            ble.peer().map(|p| p.to_string()).as_deref(),
            Some("\"OpenWalkieTalkie\" (FAKE)")
        );
        assert_eq!(ble.peer().map(|p| p.uid()).as_deref(), Some("ble:FAKE"));
        assert_eq!(ble.send(&[1, 2, 3]), Ok(()));
        // The deck takes 166 bytes at most, whatever the link allows.
        assert_eq!(
            ble.send(&[0; MAX_FRAME + 1]),
            Err(SendError::TooLong {
                len: MAX_FRAME + 1,
                max: MAX_FRAME
            })
        );
        assert_eq!(deck.written(8), [vec![1, 2, 3]]);

        // A small MTU: the link's own limit.
        deck.detach("gone");
        deck.attach(20, None);
        assert_eq!(
            ble.send(&[0; 21]),
            Err(SendError::TooLong { len: 21, max: 20 })
        );
        assert_eq!(ble.send(&[0; 20]), Ok(()));
    }

    #[test]
    fn a_full_queue_refuses_and_a_drop_discards_and_both_are_counted() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(182, Some(INFO));
        for i in 0..SendQueue::CAPACITY {
            assert_eq!(ble.send(&[i as u8]), Ok(()));
        }
        assert_eq!(ble.send(&[0xFF]), Err(SendError::Full));
        assert_eq!(deck.written(2), [vec![0], vec![1]]);
        assert_eq!(ble.send(&[0xFE]), Ok(()), "room again");
        let stats = ble.stats();
        assert_eq!(
            (stats.sent, stats.dropped, stats.queued),
            (2, 1, SendQueue::CAPACITY - 1)
        );
        deck.detach("the deck went away");
        assert_eq!(ble.problem().as_deref(), Some("the deck went away"));
        assert_eq!(ble.peer(), None);
        let stats = ble.stats();
        assert_eq!(
            (stats.dropped, stats.queued),
            (SendQueue::CAPACITY as u64, 0)
        );
        assert!(deck.written(100).is_empty());
        // A new connection starts empty.
        deck.attach(182, None);
        assert_eq!(ble.problem(), None);
        assert!(deck.written(100).is_empty());
    }

    #[test]
    fn mic_requests_are_three_byte_frames_with_the_seconds_clamped() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(182, Some(INFO));
        ble.mic(MIC_LOOPBACK, 5).unwrap();
        ble.mic(codec::MIC_ON, 0).unwrap();
        ble.mic(codec::MIC_OFF, 200).unwrap();
        assert_eq!(
            deck.written(8),
            [vec![0x10, 2, 5], vec![0x10, 1, 1], vec![0x10, 0, 60]]
        );
    }

    #[test]
    fn link_events_end_on_the_current_state_however_far_behind_the_reader_is() {
        let (ble, deck) = BleAudio::with_fake();
        let events = ble.link_events();
        for i in 0..20 {
            deck.attach(100 + i, None);
            deck.detach("again");
        }
        deck.attach(182, Some(INFO));
        let seen: Vec<LinkState> = events.try_iter().collect();
        assert_eq!(seen.len(), LINK_EVENTS);
        assert_eq!(seen.last(), Some(&ble.state()));
        // Repeating a state is not an event.
        deck.detach("gone");
        assert_eq!(events.try_recv(), Ok(LinkState::Detached));
        deck.detach("still gone");
        assert!(events.try_recv().is_err());
        assert_eq!(ble.problem().as_deref(), Some("still gone"));
    }

    #[test]
    fn the_mic_is_decoded_as_it_arrives_and_ends_when_the_link_drops() {
        let (ble, deck) = BleAudio::with_fake();
        let mic = ble.mic_events();
        deck.attach(182, Some(INFO));
        let mut start = [0u8; 5];
        start_frame(3, &mut start);
        deck.notify(&start);
        deck.notify(&audio(3, 0));
        deck.notify(&audio(3, 2));
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 3 }));
        let Ok(MicEvent::Audio { samples, .. }) = mic.try_recv() else {
            panic!("audio");
        };
        assert_eq!(samples.len(), FRAME_SAMPLES);
        assert!(matches!(
            mic.try_recv(),
            Ok(MicEvent::Audio {
                seq: 2,
                lost_frames: 1,
                ..
            })
        ));
        deck.detach("link lost");
        assert_eq!(
            mic.try_recv(),
            Ok(MicEvent::End {
                stream: 3,
                next_seq: 3,
                reason: END_ERROR
            })
        );
        let stats = ble.stats();
        assert_eq!((stats.mic.frames, stats.mic.lost), (2, 1));
    }

    #[test]
    fn a_reader_that_stops_reading_costs_frames_not_the_radio() {
        let (ble, deck) = BleAudio::with_fake();
        let _mic = ble.mic_events();
        deck.attach(182, None);
        let mut start = [0u8; 5];
        start_frame(1, &mut start);
        deck.notify(&start);
        for seq in 0..250u32 {
            deck.notify(&audio(1, seq as u8));
        }
        // START plus 250 frames into a channel of MIC_EVENTS.
        assert_eq!(ble.stats().mic_overflow, 251 - MIC_EVENTS as u64);
        assert_eq!(ble.stats().mic.frames, 250);
    }

    #[test]
    fn the_cable_attaches_with_one_listen_and_the_decks_info_makes_it_ready() {
        let (ble, _deck) = BleAudio::with_fake();
        let mut usb = ble.usb_port();
        let now = Instant::now();
        assert_eq!(usb.next_report(now), None, "nothing before the cable is open");
        // A channel-7 report is the port's whatever its state; before the
        // cable is open it changes nothing.
        assert!(usb.on_report(&usb_report(&[KIND_INFO, 1, 1, 16, 20, 3])));
        assert_eq!(ble.state(), LinkState::Detached);

        usb.attach(Some("AABBCC"));
        assert_eq!(ble.state(), LinkState::Attaching);
        assert_eq!(ble.peer().map(|p| p.uid()).as_deref(), Some("usb:AABBCC"));
        assert_eq!(ble.send(&[1]), Err(SendError::Detached), "not ready yet");
        // The LISTEN, once, as the first report.
        let listen = usb.next_report(now).expect("LISTEN owed");
        assert_eq!(&listen[..5], &[6, codec::CHANNEL, 2, KIND_LISTEN, 1]);
        assert_eq!(usb.next_report(now), None, "and only once");
        // The deck's answer.
        assert!(usb.on_report(&usb_report(&[KIND_INFO, 1, 1, 16, 20, 3])));
        assert_eq!(
            ble.state(),
            LinkState::Ready {
                max_write: REPORT_FRAME,
                info: Some(INFO)
            }
        );
        assert_eq!(ble.transport(), Some(Transport::Usb));
        assert!(usb.is_active());
        // Frames go out as reports, one each, and the wake-up fires.
        assert!(usb.wake().try_recv().is_err());
        ble.mic(codec::MIC_ON, 5).unwrap();
        assert_eq!(usb.wake().try_recv(), Ok(()));
        let report = usb.next_report(now).expect("the MIC frame");
        assert_eq!(&report[..6], &[6, codec::CHANNEL, 3, 0x10, 1, 5]);
        assert_eq!(usb.next_report(now), None);
        // A frame longer than a report is refused up front.
        assert_eq!(
            ble.send(&[0; REPORT_FRAME + 1]),
            Err(SendError::TooLong {
                len: REPORT_FRAME + 1,
                max: REPORT_FRAME
            })
        );
        // The deck's microphone arrives on the same port.
        let mic = ble.mic_events();
        let mut start = [0u8; 5];
        start_frame(7, &mut start);
        assert!(usb.on_report(&usb_report(&start)));
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 7 }));
        // A deck line is not the port's.
        assert!(!usb.on_report(&[6, 5, 2, b'{', b'}']));
        // Going away: the farewell lets the deck stop, then the port is detached.
        let bye = usb.farewell().expect("LISTEN off");
        assert_eq!(&bye[..5], &[6, codec::CHANNEL, 2, KIND_LISTEN, 0]);
        usb.detach(None);
        assert_eq!(ble.state(), LinkState::Detached);
        assert_eq!(ble.problem(), None);
        assert_eq!(
            mic.try_recv(),
            Ok(MicEvent::End {
                stream: 7,
                next_seq: 0,
                reason: END_ERROR
            })
        );
    }

    #[test]
    fn a_deck_that_never_answers_listen_is_reported_after_the_deadline() {
        let (ble, _deck) = BleAudio::with_fake();
        let mut usb = ble.usb_port();
        let t0 = Instant::now();
        usb.attach(None);
        usb.next_report(t0).expect("LISTEN");
        usb.tick(t0 + Duration::from_millis(1500));
        assert_eq!(ble.state(), LinkState::Attaching, "still waiting");
        usb.tick(t0 + UsbPort::ANSWER_WITHIN);
        assert_eq!(ble.state(), LinkState::Detached);
        assert!(
            ble.problem().unwrap_or_default().contains("did not answer LISTEN"),
            "{:?}",
            ble.problem()
        );
        assert_eq!(usb.farewell(), None, "it never listened to us");
        // A late INFO still counts: the deck was only slow.
        assert!(usb.on_report(&usb_report(&[KIND_INFO, 1, 1, 16, 20, 3])));
        assert!(matches!(ble.state(), LinkState::Ready { .. }));
        assert_eq!(ble.problem(), None);
    }

    #[test]
    fn the_cable_is_the_port_in_use_when_both_are_ready_and_bluetooth_stands_in_when_it_goes() {
        let (ble, deck) = BleAudio::with_fake();
        let events = ble.link_events();
        let mut usb = ble.usb_port();
        let now = Instant::now();
        deck.attach(182, Some(INFO));
        assert_eq!(ble.transport(), Some(Transport::Ble));
        ble.send(&[9, 9]).unwrap();
        // The cable comes up: it takes over, and what was queued for
        // Bluetooth is discarded with that port's limit.
        usb.attach(Some("S"));
        assert_eq!(ble.transport(), Some(Transport::Ble), "attaching is not in use");
        usb.next_report(now).expect("LISTEN");
        assert!(usb.on_report(&usb_report(&[KIND_INFO, 1, 1, 16, 20, 3])));
        assert_eq!(ble.transport(), Some(Transport::Usb));
        assert_eq!(ble.peer().map(|p| p.uid()).as_deref(), Some("usb:S"));
        assert!(deck.written(8).is_empty(), "Bluetooth gets nothing now");
        assert_eq!(ble.stats().dropped, 1);
        ble.send(&[8]).unwrap();
        assert!(deck.written(8).is_empty(), "still nothing for Bluetooth");
        assert_eq!(usb.next_report(now).map(|r| r[3]), Some(8));
        // A live stream on the cable is not ended by Bluetooth dropping...
        let mic = ble.mic_events();
        let mut start = [0u8; 5];
        start_frame(2, &mut start);
        assert!(usb.on_report(&usb_report(&start)));
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 2 }));
        deck.detach("radio off");
        assert_eq!(ble.transport(), Some(Transport::Usb));
        assert!(mic.try_recv().is_err(), "the stream is on the cable");
        assert_eq!(ble.problem(), None, "a ready link has no problem");
        // ...nor is a stream on Bluetooth ended by the cable coming up
        // (the deck keeps a stream on the port it started it on).
        deck.attach(182, Some(INFO));
        usb.detach(None);
        assert_eq!(mic.try_recv(), Ok(MicEvent::End { stream: 2, next_seq: 0, reason: END_ERROR }));
        let mut start = [0u8; 5];
        start_frame(3, &mut start);
        deck.notify(&start);
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 3 }));
        usb.attach(Some("S"));
        usb.next_report(now).expect("LISTEN");
        assert!(usb.on_report(&usb_report(&[KIND_INFO, 1, 1, 16, 20, 3])));
        assert_eq!(ble.transport(), Some(Transport::Usb));
        assert!(mic.try_recv().is_err(), "stream 3 lives on");
        deck.notify(&audio(3, 0));
        assert!(matches!(mic.try_recv(), Ok(MicEvent::Audio { stream: 3, .. })));
        let mut end = [0u8; 4];
        codec::end_frame(3, 1, codec::END_RELEASE, &mut end);
        deck.notify(&end);
        assert!(matches!(mic.try_recv(), Ok(MicEvent::End { stream: 3, .. })));
        // Back to a stream on the cable for the rest.
        start_frame(2, &mut start);
        assert!(usb.on_report(&usb_report(&start)));
        assert_eq!(mic.try_recv(), Ok(MicEvent::Start { stream: 2 }));
        deck.detach("radio off");
        // ...and when the cable goes, Bluetooth (back again) stands in.
        deck.attach(182, Some(INFO));
        usb.detach(None);
        assert_eq!(ble.transport(), Some(Transport::Ble));
        assert_eq!(ble.peer().map(|p| p.uid()).as_deref(), Some("ble:FAKE"));
        assert_eq!(
            mic.try_recv(),
            Ok(MicEvent::End {
                stream: 2,
                next_seq: 0,
                reason: END_ERROR
            }),
            "the deck's stream died with its port"
        );
        ble.send(&[7]).unwrap();
        assert_eq!(deck.written(8), [vec![7]]);
        assert_eq!(usb.next_report(now), None);
        // The events told the story in order, ending on the state now.
        let seen: Vec<LinkState> = events.try_iter().collect();
        assert_eq!(seen.last(), Some(&ble.state()));
        assert!(seen.len() >= 4, "{seen:?}");
    }
}
