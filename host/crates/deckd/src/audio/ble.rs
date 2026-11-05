//! The BLE transport's two devices (`[voice] transport = "ble"`, the
//! default): the deck's microphone as a [`CaptureSource`] and its speaker as
//! an [`OutputSink`], both over the link [`crate::ble::BleAudio`] keeps up.
//!
//! The Rev A deck is not a Bluetooth audio device any more. Its microphone
//! and speaker travel as our own 16 kHz IMA ADPCM frames over a GATT service
//! on the BLE link its HID side already uses (`docs/deck-protocol.md`,
//! "Voice"). Everything above this file — the lanes, the mixer, the route
//! machine, the recogniser — is the engine the CoreAudio transport runs, so
//! this file only has to answer the two seams in the deck's own terms, and
//! the terms differ from CoreAudio's in three ways that shape it:
//!
//! * **The microphone starts on the press, not on our open.** The deck
//!   powers its PDM microphone and starts a stream (START, an AUDIO frame
//!   every 20 ms, END) the moment PTT goes down, and `deckd-audio` opens the
//!   capture when the press's channel-5 `ptt` line arrives over HID — which
//!   can be before or after the first audio notification. So
//!   [`BleCapture`] reads the stream all the time and keeps a pre-roll of
//!   the current one from its START (at most [`PREROLL`]); an open hands
//!   over the pre-roll and then the live frames, so the recogniser hears the
//!   press from its first syllable either way. There is no bring-up to hide,
//!   which is why `mic_warm` is moot on this transport.
//! * **The speaker is a stream we frame and pace ourselves.** There is no
//!   device clock to call us back. [`BleOutput`] renders the mixer in 20 ms
//!   blocks on a monotonic clock, [`LEAD`] ahead of real time, and frames
//!   talk spurts: START when a block has sound in it, AUDIO frames with one
//!   coder state running across the spurt, END after [`HANGOVER`] of
//!   silence — so the deck's amplifier is off between announcements and its
//!   link can go back to sleep. Silence is judged with a very low threshold
//!   ([`QUIET_PEAK`], about −72 dBFS), the hangover and one block of
//!   pre-roll, so a quiet onset or a pause between words is never cut out.
//! * **Nothing here may wait for the radio.** `BleAudio::send` queues or
//!   refuses at once; a refused frame is counted and raised as
//!   [`Fault::Xrun`], the same way a late CoreAudio callback is, and the
//!   deck hears a `seq` gap it covers with silence.
//!
//! The link is half-duplex like a walkie-talkie: while a microphone stream
//! is live the deck discards speaker frames (its speaker reaches its own
//! microphone at about +17 dB and there is no echo cancellation). The route
//! machine already closes the output for a capture of ours, so nothing here
//! has to enforce it twice.
//!
//! The logic is three small pure pieces with their tests beside them —
//! [`Feed`] (pre-roll and loss filling), [`Spurts`] (talk-spurt framing) and
//! [`Pacer`] (when the next block is due) — and two thin devices around them
//! that own a thread each.
use super::engine::OutputSink;
use super::lanes::Mixer;
use super::speaker_eq::SpeakerEq;
use super::traits::{
    AudioError, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, DeviceSide, Fault,
    RouteEvent, StreamFaults,
};
use super::FromAudio;
use crate::ble::codec::{self, END_ABORT, END_ERROR, END_RELEASE, FRAME_SAMPLES, MAX_FRAME};
use crate::ble::{BleAudio, Info, LinkState, MicEvent, SendError, Transport};
use crossbeam_channel::{select, Receiver, RecvTimeoutError, Sender};
use deck_model::{Permission, VoiceIn, VoiceInput};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The microphone and baseline BLE speaker rate. USB speaker output uses
/// the negotiated rate from `output_rate`; capture always delivers at RATE.
pub const RATE: u32 = codec::RATE_HZ;
/// One 20 ms block: what the speaker renders at a time, and what one full
/// AUDIO frame carries.
pub const BLOCK: usize = FRAME_SAMPLES;
/// [`BLOCK`] as time.
pub const BLOCK_TIME: Duration = Duration::from_millis(20);
/// How much of a stream the capture keeps for an open that comes late.
pub const PREROLL: Duration = Duration::from_secs(2);
/// [`PREROLL`] in samples.
pub const PREROLL_SAMPLES: usize = 2 * RATE as usize;
/// A live stream that has sent nothing for this long is not the press an
/// open is for: its END went missing. A healthy stream sends every 20 ms,
/// and the worst gap measured even on the slow link was 345 ms.
pub const STALE: Duration = Duration::from_secs(1);
/// The most silence one gap is filled with. A longer gap is a stall rather
/// than a lost frame, and filling it whole would only push live audio out
/// of the engine's capture ring.
pub const MAX_FILL: Duration = Duration::from_secs(1);
/// [`MAX_FILL`] in samples.
const MAX_FILL_SAMPLES: usize = RATE as usize;
/// How far ahead of real time the speaker renders: the bench probe's lead,
/// well inside the deck's 640 ms queue and "a lead of up to about 100 ms is
/// fine" (`docs/deck-protocol.md`).
pub const LEAD: Duration = crate::ble::probe::LEAD;
/// Silence that ends a talk spurt. Shorter pauses — between words, and
/// mostly between sentences — stay inside one spurt, so the deck's
/// amplifier is not cycled (115 ms to power up) in the middle of a reply.
pub const HANGOVER: Duration = Duration::from_millis(300);
/// The loudest sample a block may hold and still be silence: 8 LSB, about
/// −72 dBFS. The mixer renders exact zeros when nothing plays, so this only
/// has to be above the dust a decaying filter leaves; anything louder is
/// sent.
pub const QUIET_PEAK: u16 = 8;
/// How late the speaker thread may run before it stops catching up and
/// starts its clock again (raising [`Fault::Xrun`]): a burst of more than
/// five blocks would sit in the deck's queue as latency for the rest of the
/// spurt.
pub const MAX_BEHIND: Duration = Duration::from_millis(100);

/// A poisoned lock is still a lock: one panicking thread must not take the
/// microphone down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// The link, as the route machine sees it
// ---------------------------------------------------------------------------

/// Which of the deck's two directions a link state offers: `(microphone,
/// speaker)`.
///
/// Both need the link `Ready`. The info characteristic says which the deck
/// has; a deck whose info could not be read is taken at its word that the
/// service exists, and one that speaks another dialect (version, codec or
/// rate) offers neither. A write limit with no room for audio is no
/// speaker.
#[must_use]
pub fn sides(state: &LinkState) -> (bool, bool) {
    let LinkState::Ready { max_write, info } = state else {
        return (false, false);
    };
    let (mic, speaker) = match info.map(Info::from_bytes) {
        Some(info) if info.compatible() => (info.mic(), info.speaker()),
        Some(_) => (false, false),
        None => (true, true),
    };
    let room = codec::samples_per_frame((*max_write).min(MAX_FRAME)) > 0;
    (mic, speaker && room)
}

/// What the route machine hears when the link changes: on `Ready` the
/// speaker's rate first (so the machine never sees a present device with no
/// rate), then the two sides; otherwise both sides gone.
#[must_use]
pub fn output_rate(state: &LinkState, transport: Option<Transport>) -> u32 {
    if let (Some(Transport::Usb), LinkState::Ready { info: Some(bytes), .. }) = (transport, state) {
        let info = Info::from_bytes(*bytes);
        if info.compatible() && info.speaker() && info.features & codec::FEATURE_USB_SPEAKER_48K != 0 {
            return codec::USB_SPEAKER_RATE_HZ;
        }
    }
    RATE
}

pub fn route_events(state: &LinkState, transport: Option<Transport>) -> Vec<RouteEvent> {
    let (mic, speaker) = sides(state);
    let mut events = Vec::with_capacity(3);
    if matches!(state, LinkState::Ready { .. }) {
        events.push(RouteEvent::NominalRate { rate: output_rate(state, transport) });
    }
    events.push(RouteEvent::DevicePresent {
        side: DeviceSide::Input,
        present: mic,
    });
    events.push(RouteEvent::DevicePresent {
        side: DeviceSide::Output,
        present: speaker,
    });
    events
}

/// `deckd-ble-link`: the link's state changes, as route events.
///
/// It stands in for the CoreAudio property watcher, and like it, it sends a
/// [`RouteEvent::Tick`] while idle so it notices the control loop has gone
/// and exits — dropping its clone of the link with it.
pub fn watch(
    ble: BleAudio,
    routes: Sender<RouteEvent>,
    voice: Sender<FromAudio>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("deckd-ble-link".into())
        .spawn(move || {
            let events = ble.link_events();
            // The channel carries changes; start from the state now.
            while events.try_recv().is_ok() {}
            let mut last = ble.state();
            for event in route_events(&last, ble.transport()) {
                if routes.send(event).is_err() {
                    return;
                }
            }
            // The Bluetooth grant, for `perm.status` and `perm.changed`:
            // read on every wake (at least once a second), told on a change.
            let mut grant = Permission::Unknown;
            loop {
                let now = ble.bluetooth();
                if now != grant {
                    grant = now;
                    let report = VoiceIn::Bluetooth { permission: now };
                    if voice.send(FromAudio::Voice(report)).is_err() {
                        return;
                    }
                }
                match events.recv_timeout(Duration::from_secs(1)) {
                    Ok(state) if state == last => {}
                    Ok(state) => {
                        let (mic, speaker) = sides(&state);
                        if let (LinkState::Ready { info, .. }, false, false) =
                            (&state, mic, speaker)
                        {
                            tracing::warn!(
                                info = %info.map(|b| crate::ble::hex(&b)).unwrap_or_default(),
                                "the deck's audio service offers no usable microphone or speaker"
                            );
                        }
                        tracing::debug!(?state, mic, speaker, "deck audio link changed");
                        for event in route_events(&state, ble.transport()) {
                            if routes.send(event).is_err() {
                                return;
                            }
                        }
                        last = state;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if routes.is_empty() && routes.send(RouteEvent::Tick).is_err() {
                            return; // the control loop is gone
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .ok()
}

/// The `voice.route` UID of the deck behind a link: `ble:` and the
/// CoreBluetooth identifier (stable per Mac), or `usb:` and the serial
/// number, for the port in use.
fn uid_of(ble: &BleAudio) -> Option<String> {
    ble.peer().map(|peer| peer.uid())
}

// ---------------------------------------------------------------------------
// Capture: the pre-roll and the loss filling
// ---------------------------------------------------------------------------

/// What an open capture is attached to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Bound {
    /// No capture is open.
    #[default]
    Closed,
    /// Open, and waiting for the next stream to start.
    Waiting,
    /// Open, and delivering this stream.
    On(u8),
    /// Open, and its stream has ended: a capture is one press, so it
    /// delivers nothing more until it is closed.
    Done,
}

/// The latest microphone stream, as far as a capture needs it.
#[derive(Debug)]
struct Stream {
    id: u8,
    live: bool,
    /// The `seq` the next AUDIO frame should carry (for the tail at END).
    next_seq: u8,
    /// Samples per AUDIO frame in this stream: what a lost frame was worth.
    frame_len: usize,
    /// When the START or the last frame arrived.
    heard: Instant,
    /// The stream from its START, at most [`PREROLL`], as delivered chunks.
    roll: VecDeque<Vec<f32>>,
    rolled: usize,
}

impl Stream {
    fn new(id: u8, now: Instant) -> Stream {
        Stream {
            id,
            live: true,
            next_seq: 0,
            frame_len: FRAME_SAMPLES,
            heard: now,
            roll: VecDeque::new(),
            rolled: 0,
        }
    }

    fn keep(&mut self, chunk: Vec<f32>) {
        self.rolled += chunk.len();
        self.roll.push_back(chunk);
        while self.rolled > PREROLL_SAMPLES {
            match self.roll.pop_front() {
                Some(old) => self.rolled -= old.len(),
                None => break,
            }
        }
    }

    fn forget(&mut self) {
        self.roll.clear();
        self.rolled = 0;
    }
}

/// What [`Feed::open`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opened {
    /// A stream was live: its pre-roll (`samples`, at most [`PREROLL`]) went
    /// to the sink first, and its live frames follow.
    PreRoll { stream: u8, samples: usize },
    /// Nothing live: the next stream that starts is this capture's.
    Waiting,
}

/// [`BleCapture`]'s logic, as a fold: microphone events and opens in,
/// PCM for the sink out (16 kHz mono f32, `i16 / 32768`).
///
/// The rules, each a case a test pins:
///
/// * An open while a stream is live (and not [`STALE`]) gets the stream's
///   pre-roll, then its live frames. An open with none waits for the next
///   START.
/// * A lost frame is silence of the frame's own length, so the recogniser's
///   timeline stays the deck's (capped at [`MAX_FILL`] per gap); so is the
///   tail the deck's END says went missing.
/// * A capture is one press: when its stream ENDs it delivers nothing more.
///   A stream cut short by the link (`END_ERROR`, which the host also
///   synthesises when the link drops) sends it back to waiting instead, so
///   a press the deck starts again is still heard.
/// * A close forgets what the capture heard, so a re-open during the same
///   stream does not hear it twice.
/// * A re-open onto the very stream the last close was bound to is
///   provisional: that is either the same press carrying on, or a new press
///   whose HID line beat the old stream's END over the radio. Either way,
///   when that stream ends the capture waits for the next START instead of
///   being done, so a quick re-press is never lost.
#[derive(Debug, Default)]
pub struct Feed {
    latest: Option<Stream>,
    bound: Bound,
    /// The stream the last close was bound to.
    closed_on: Option<u8>,
    /// The current binding was made at open to `closed_on`'s stream.
    provisional: bool,
    /// Silence inserted for lost frames, in samples.
    filled: u64,
}

/// Silence for `frames` lost frames of `frame_len` samples, capped.
fn fill_len(frames: u8, frame_len: usize) -> usize {
    (usize::from(frames) * frame_len).min(MAX_FILL_SAMPLES)
}

/// `n` samples of silence, in chunks no longer than a block.
fn silence(n: usize) -> Vec<Vec<f32>> {
    let mut chunks = Vec::new();
    let mut left = n;
    while left > 0 {
        let len = left.min(BLOCK);
        chunks.push(vec![0.0; len]);
        left -= len;
    }
    chunks
}

/// The codec's `i16` as the engine's f32.
fn pcm(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|s| f32::from(*s) / 32768.0).collect()
}

impl Feed {
    #[must_use]
    pub fn new() -> Feed {
        Feed::default()
    }

    /// A capture is open (waiting, delivering or done).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.bound != Bound::Closed
    }

    /// Samples a capture opening now would get as pre-roll (ignoring
    /// staleness).
    #[must_use]
    pub fn preroll(&self) -> usize {
        self.latest
            .as_ref()
            .filter(|s| s.live)
            .map_or(0, |s| s.rolled)
    }

    /// Silence inserted for lost frames so far, in samples.
    #[must_use]
    pub fn filled(&self) -> u64 {
        self.filled
    }

    /// One microphone event, at `now`.
    pub fn event(&mut self, now: Instant, event: MicEvent, deliver: &mut dyn FnMut(&[f32])) {
        match event {
            MicEvent::Start { stream } => {
                self.latest = Some(Stream::new(stream, now));
                if self.bound == Bound::Waiting {
                    self.bound = Bound::On(stream);
                    self.provisional = false;
                }
            }
            MicEvent::Audio {
                stream,
                seq,
                lost_frames,
                samples,
            } => {
                let on = self.bound == Bound::On(stream);
                let Some(s) = self.latest.as_mut().filter(|s| s.id == stream && s.live) else {
                    return;
                };
                s.heard = now;
                s.next_seq = seq.wrapping_add(1);
                if !samples.is_empty() {
                    s.frame_len = samples.len();
                }
                let fill = fill_len(lost_frames, s.frame_len);
                for chunk in silence(fill).into_iter().chain([pcm(&samples)]) {
                    if on {
                        deliver(&chunk);
                    }
                    s.keep(chunk);
                }
                self.filled += fill as u64;
            }
            MicEvent::End {
                stream,
                next_seq,
                reason,
            } => {
                let on = self.bound == Bound::On(stream);
                if let Some(s) = self.latest.as_mut().filter(|s| s.id == stream && s.live) {
                    let tail = fill_len(next_seq.wrapping_sub(s.next_seq), s.frame_len);
                    for chunk in silence(tail) {
                        if on {
                            deliver(&chunk);
                        }
                        s.keep(chunk);
                    }
                    s.live = false;
                    self.filled += tail as u64;
                }
                if on {
                    self.bound = if reason == END_ERROR || self.provisional {
                        Bound::Waiting
                    } else {
                        Bound::Done
                    };
                    self.provisional = false;
                }
            }
        }
    }

    /// A capture opens at `now`.
    pub fn open(&mut self, now: Instant, deliver: &mut dyn FnMut(&[f32])) -> Opened {
        match self.latest.as_ref() {
            Some(s) if s.live && now.saturating_duration_since(s.heard) <= STALE => {
                for chunk in &s.roll {
                    deliver(chunk);
                }
                self.bound = Bound::On(s.id);
                self.provisional = self.closed_on == Some(s.id);
                Opened::PreRoll {
                    stream: s.id,
                    samples: s.rolled,
                }
            }
            _ => {
                self.bound = Bound::Waiting;
                self.provisional = false;
                Opened::Waiting
            }
        }
    }

    /// Nothing more is coming for the bound stream: it has ENDed (or there
    /// is none). While it is live, frames the deck encoded before the
    /// release may still be on the radio.
    #[must_use]
    pub fn ended(&self) -> bool {
        !matches!(self.bound, Bound::On(_))
    }

    /// The capture closes.
    pub fn close(&mut self) {
        self.closed_on = None;
        if let Bound::On(id) = self.bound {
            if let Some(s) = self.latest.as_mut().filter(|s| s.id == id && s.live) {
                s.forget();
            }
            self.closed_on = Some(id);
        }
        self.bound = Bound::Closed;
        self.provisional = false;
    }
}

/// The feed and the sink it delivers into: one lock, so an open's pre-roll
/// and the reader's live frames can never interleave or repeat.
#[derive(Default)]
struct Tap {
    feed: Feed,
    sink: Option<CaptureSink>,
}

fn tap(shared: &Mutex<Tap>, now: Instant, event: MicEvent) {
    let mut guard = lock(shared);
    let Tap { feed, sink } = &mut *guard;
    feed.event(now, event, &mut |samples| {
        if let Some(sink) = sink.as_mut() {
            sink.deliver(RATE, samples);
        }
    });
}

struct Reader {
    stop: Sender<()>,
    join: std::thread::JoinHandle<()>,
}

/// The deck's microphone over BLE, behind §7's capture seam.
///
/// A `deckd-ble-mic` thread is the link's one mic-event reader for as long
/// as this lives (the channel is single-consumer). It delivers on its own
/// thread, as cpal does on CoreAudio's, so the engine's sink — which applies
/// `set mic` and measures the level — is the same closure either way, and
/// the gain is applied there and only there.
///
/// When the deck's microphone is not available at open time (the link is
/// not ready, or the deck has none), the open falls back exactly as the
/// CoreAudio capture does without the deck: `fallback_input =
/// "system_default"` opens the Mac's default input through `fallback`,
/// `"none"` is [`AudioError::NoDevice`]. `voice.listen{input:"mac"}` always
/// goes to `fallback`.
pub struct BleCapture {
    ble: BleAudio,
    shared: Arc<Mutex<Tap>>,
    reader: Option<Reader>,
    fallback: Box<dyn CaptureSource>,
    on_fallback: bool,
    /// The open asked the deck for its microphone (MIC on), so the close
    /// owes it a MIC off.
    asked: bool,
}

impl std::fmt::Debug for BleCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleCapture")
            .field("link", &self.ble.state())
            .field("on_fallback", &self.on_fallback)
            .finish_non_exhaustive()
    }
}

impl BleCapture {
    /// The capture on `ble`, reading its microphone from now on.
    #[must_use]
    pub fn new(ble: BleAudio, fallback: Box<dyn CaptureSource>) -> BleCapture {
        let mut capture = BleCapture::unread(ble, fallback);
        let (stop, stopped) = crossbeam_channel::bounded::<()>(1);
        let mic = capture.ble.mic_events();
        let shared = capture.shared.clone();
        capture.reader = std::thread::Builder::new()
            .name("deckd-ble-mic".into())
            .spawn(move || loop {
                select! {
                    recv(mic) -> event => match event {
                        Ok(event) => tap(&shared, Instant::now(), event),
                        Err(_) => return,
                    },
                    recv(stopped) -> _ => return,
                }
            })
            .ok()
            .map(|join| Reader { stop, join });
        capture
    }

    /// Without the reader thread: the tests' version, driven by a
    /// `pumper`.
    fn unread(ble: BleAudio, fallback: Box<dyn CaptureSource>) -> BleCapture {
        BleCapture {
            ble,
            shared: Arc::new(Mutex::new(Tap::default())),
            reader: None,
            fallback,
            on_fallback: false,
            asked: false,
        }
    }

    /// Ask the deck to stream (`seconds`), or to stop (`None`).
    fn ask(&self, seconds: Option<u8>) -> Result<(), SendError> {
        let mut frame = [0u8; 3];
        let n = match seconds {
            Some(s) => codec::mic_frame(codec::MIC_ON, s, &mut frame),
            None => codec::mic_frame(codec::MIC_OFF, 0, &mut frame),
        };
        self.ble.send(&frame[..n])
    }

    /// What the reader would do with everything waiting, here and now —
    /// for a test that holds this capture without its thread, even after it
    /// moved into an engine.
    #[cfg(test)]
    fn pumper(&self) -> impl Fn(Instant) + use<> {
        let (ble, shared) = (self.ble.clone(), self.shared.clone());
        move |now| {
            for event in ble.mic_events().try_iter() {
                tap(&shared, now, event);
            }
        }
    }

    fn open_fallback(
        &mut self,
        req: &CaptureOpen,
        sink: CaptureSink,
    ) -> Result<CaptureOpened, AudioError> {
        let opened = self.fallback.open(req, sink)?;
        self.on_fallback = true;
        Ok(opened)
    }
}

impl CaptureSource for BleCapture {
    fn open(
        &mut self,
        req: &CaptureOpen,
        mut sink: CaptureSink,
    ) -> Result<CaptureOpened, AudioError> {
        self.close();
        if req.input == VoiceInput::Mac {
            return self.open_fallback(req, sink);
        }
        let (mic, _) = sides(&self.ble.state());
        if !mic {
            let why = self
                .ble
                .problem()
                .unwrap_or_else(|| "the deck's audio link is not ready".into());
            if req.fallback != "system_default" {
                return Err(AudioError::NoDevice(format!(
                    "the deck's microphone (over Bluetooth LE or its cable): {why}"
                )));
            }
            tracing::info!(%why, "no deck microphone over Bluetooth or USB: capturing from the Mac's default input");
            return self.open_fallback(req, sink);
        }
        let opened = {
            let mut guard = lock(&self.shared);
            let opened = guard
                .feed
                .open(Instant::now(), &mut |samples| sink.deliver(RATE, samples));
            guard.sink = Some(sink);
            opened
        };
        tracing::debug!(?opened, transport = ?self.ble.transport(), "deck microphone open");
        // Nobody pressed (`voice.listen`): the deck streams only when asked.
        // A press it is already streaming carries on as it was.
        if let Some(seconds) = req.ask_deck {
            match self.ask(Some(seconds)) {
                Ok(()) => self.asked = true,
                Err(e) => tracing::warn!(%e, "could not ask the deck for its microphone"),
            }
        }
        Ok(CaptureOpened {
            rate: RATE,
            channels: 1,
            uid: uid_of(&self.ble),
        })
    }

    fn close(&mut self) {
        if self.on_fallback {
            self.fallback.close();
            self.on_fallback = false;
        }
        // What we asked for, we let go of. The deck ignores this if a press
        // has since taken the stream over, so it never cuts an operator off.
        if std::mem::take(&mut self.asked) {
            if let Err(e) = self.ask(None) {
                tracing::debug!(%e, "could not tell the deck to stop its microphone");
            }
        }
        let mut guard = lock(&self.shared);
        guard.feed.close();
        guard.sink = None;
    }

    fn is_open(&self) -> bool {
        (self.on_fallback && self.fallback.is_open()) || lock(&self.shared).feed.is_open()
    }

    fn ended(&self) -> bool {
        if self.on_fallback {
            return self.fallback.ended();
        }
        lock(&self.shared).feed.ended()
    }
}

impl Drop for BleCapture {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            drop(reader.stop);
            let _ = reader.join.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Output: talk spurts
// ---------------------------------------------------------------------------

/// What the speaker has done since its [`BleOutput`] was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeakerStats {
    /// Talk spurts started (START frames the link took).
    pub spurts: u64,
    /// AUDIO frames the link took.
    pub frames: u64,
    /// START, AUDIO and END frames a full queue refused. A lost AUDIO frame
    /// is a `seq` gap the deck covers with silence; a lost END is the
    /// deck's own 300 ms timeout.
    pub dropped: u64,
    /// Spurts the link went away under.
    pub cut: u64,
}

/// What one block did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Silence, and no spurt open: nothing was sent.
    Quiet,
    /// A spurt began with this block: START, the pre-roll block, this one.
    Started,
    /// Part of an open spurt.
    Playing,
    /// The hangover ran out with this block: END went after it.
    Ended,
    /// Sound, but the link could not take a spurt (not ready, no room, or
    /// START refused). Kept as the pre-roll of the next attempt.
    Held,
    /// The link went away under an open spurt: over, with no END to send.
    Cut,
}

struct Spurt {
    stream: u8,
    seq: u8,
    /// One coder state across the whole spurt: each frame's header carries
    /// it, so the deck can decode any frame on its own.
    state: codec::State,
    /// Samples per AUDIO frame on this connection.
    per_frame: usize,
    /// Rendered samples not yet framed (only when frames are shorter than a
    /// block, on a small attribute limit).
    pending: Vec<i16>,
    /// Silence since the last block with sound.
    quiet: Duration,
}

/// The speaker's framer: one 20 ms block at its negotiated mono rate in,
/// frames to the link out (see the module docs for the rules).
pub struct Spurts {
    rate: u32,
    next_stream: u8,
    live: Option<Spurt>,
    /// The last silent block, sent ahead of a spurt's first block so a slow
    /// onset is never cut off.
    preroll: Vec<i16>,
    stats: SpeakerStats,
}

impl std::fmt::Debug for Spurts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spurts")
            .field("live", &self.live.as_ref().map(|s| s.stream))
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Every sample within [`QUIET_PEAK`] of zero.
#[must_use]
pub fn is_quiet(block: &[i16]) -> bool {
    block.iter().all(|s| s.unsigned_abs() <= QUIET_PEAK)
}

impl Spurts {
    /// A framer whose first spurt is `first_stream` (any value; consecutive
    /// spurts count up from it, mod 256).
    #[must_use]
    pub fn new(first_stream: u8) -> Spurts {
        Spurts {
            rate: RATE,
            next_stream: first_stream,
            live: None,
            preroll: Vec::with_capacity(BLOCK),
            stats: SpeakerStats::default(),
        }
    }

    #[must_use]
    pub fn stats(&self) -> SpeakerStats {
        self.stats
    }

    /// The open spurt's stream id.
    #[must_use]
    pub fn live(&self) -> Option<u8> {
        self.live.as_ref().map(|s| s.stream)
    }

    /// One block, framed onto `link`.
    pub fn block(&mut self, block: &[i16], link: &BleAudio) -> Step {
        if output_rate(&link.state(), link.transport()) != self.rate {
            self.preroll.clear();
            return self.cut();
        }
        let loud = !is_quiet(block);
        let mut started = false;
        if self.live.is_none() {
            if !loud || !self.start(link) {
                self.preroll.clear();
                self.preroll.extend_from_slice(block);
                return if loud { Step::Held } else { Step::Quiet };
            }
            started = true;
            let pre = std::mem::take(&mut self.preroll);
            let sent = self.audio(&pre, link);
            self.preroll = pre;
            self.preroll.clear();
            if sent.is_err() {
                return self.cut();
            }
        }
        if self.audio(block, link).is_err() {
            return self.cut();
        }
        let Some(spurt) = self.live.as_mut() else {
            return Step::Cut;
        };
        if loud {
            spurt.quiet = Duration::ZERO;
        } else {
            spurt.quiet += BLOCK_TIME;
        }
        if spurt.quiet >= HANGOVER {
            self.end(link, END_RELEASE);
            return Step::Ended;
        }
        if started {
            Step::Started
        } else {
            Step::Playing
        }
    }

    /// The output is closing: an open spurt ends now, `END_ABORT`.
    pub fn close(&mut self, link: &BleAudio) {
        self.end(link, END_ABORT);
        self.preroll.clear();
    }

    fn start(&mut self, link: &BleAudio) -> bool {
        let LinkState::Ready { max_write, .. } = link.state() else {
            return false;
        };
        let per_frame = codec::samples_per_frame(max_write.min(MAX_FRAME));
        if per_frame == 0 {
            return false;
        }
        let stream = self.next_stream;
        let mut buf = [0u8; MAX_FRAME];
        let n = codec::start_frame(stream, &mut buf);
        buf[3] = (self.rate / 1000) as u8;
        match link.send(&buf[..n]) {
            Ok(()) => {}
            Err(SendError::Full) => {
                self.stats.dropped += 1;
                return false;
            }
            Err(_) => return false,
        }
        self.next_stream = stream.wrapping_add(1);
        self.stats.spurts += 1;
        self.live = Some(Spurt {
            stream,
            seq: 0,
            state: codec::State::default(),
            per_frame,
            pending: Vec::with_capacity(2 * BLOCK),
            quiet: Duration::ZERO,
        });
        true
    }

    /// Frame `samples` onto the open spurt. A full queue costs the frame
    /// (counted, and the `seq` moves on so the deck sees the gap); any other
    /// refusal means the link is gone.
    fn audio(&mut self, samples: &[i16], link: &BleAudio) -> Result<(), SendError> {
        let Some(spurt) = self.live.as_mut() else {
            return Ok(());
        };
        spurt.pending.extend_from_slice(samples);
        let mut buf = [0u8; MAX_FRAME];
        let mut at = 0;
        let mut result = Ok(());
        while spurt.pending.len() - at >= spurt.per_frame {
            let chunk = &spurt.pending[at..at + spurt.per_frame];
            let encode = if self.rate == RATE { super::adpcm::audio_frame } else { codec::audio_frame };
            let n = encode(spurt.stream, spurt.seq, &mut spurt.state, chunk, &mut buf);
            spurt.seq = spurt.seq.wrapping_add(1);
            at += spurt.per_frame;
            match link.send(&buf[..n]) {
                Ok(()) => self.stats.frames += 1,
                Err(SendError::Full) => self.stats.dropped += 1,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        spurt.pending.drain(..at);
        result
    }

    /// END the open spurt, flushing a partial frame first on a release.
    fn end(&mut self, link: &BleAudio, reason: u8) {
        let Some(mut spurt) = self.live.take() else {
            return;
        };
        let mut buf = [0u8; MAX_FRAME];
        if reason == END_RELEASE && !spurt.pending.is_empty() {
            let encode = if self.rate == RATE { super::adpcm::audio_frame } else { codec::audio_frame };
            let n = encode(
                spurt.stream,
                spurt.seq,
                &mut spurt.state,
                &spurt.pending,
                &mut buf,
            );
            spurt.seq = spurt.seq.wrapping_add(1);
            match link.send(&buf[..n]) {
                Ok(()) => self.stats.frames += 1,
                Err(_) => self.stats.dropped += 1,
            }
        }
        let n = codec::end_frame(spurt.stream, spurt.seq, reason, &mut buf);
        if let Err(SendError::Full) = link.send(&buf[..n]) {
            self.stats.dropped += 1;
        }
    }

    fn cut(&mut self) -> Step {
        if self.live.take().is_some() {
            self.stats.cut += 1;
        }
        Step::Cut
    }
}

/// When the speaker renders: block `k` is due at its end on the pacing
/// clock minus [`LEAD`] (`ble::probe::due`'s rule), so at an open the first
/// [`LEAD`] goes at once and every block after it on the 20 ms beat.
#[derive(Clone, Copy, Debug)]
pub struct Pacer {
    origin: Instant,
    blocks: u64,
}

impl Pacer {
    #[must_use]
    pub fn new(now: Instant) -> Pacer {
        Pacer {
            origin: now,
            blocks: 0,
        }
    }

    /// When the next block is due.
    #[must_use]
    pub fn next_due(&self) -> Instant {
        self.origin + crate::ble::probe::due(self.blocks, BLOCK, LEAD)
    }

    /// The next block was rendered.
    pub fn advance(&mut self) {
        self.blocks += 1;
    }

    /// Blocks rendered since the clock (re)started.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// More than [`MAX_BEHIND`] late: start the clock again from `now`
    /// instead of rendering the backlog in one burst. `true` when it did.
    pub fn catch_up(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.next_due()) > MAX_BEHIND {
            *self = Pacer::new(now);
            true
        } else {
            false
        }
    }
}

/// `deckd-ble-out`: render, frame and pace until told to stop, then end an
/// open spurt and give the mixer (and with it the lanes) back.
fn speak(
    ble: &BleAudio,
    mut mixer: Mixer,
    faults: &StreamFaults,
    stop: &Receiver<()>,
    mut spurts: Spurts,
) -> Spurts {
    let block = spurts.rate as usize / 50;
    let mut stereo = vec![0.0f32; 2 * block];
    let mut mono = vec![0i16; block];
    // Both USB and BLE use this output. Filter the final mono signal before
    // quantization, quiet detection, and ADPCM so speech and chimes match.
    let mut eq = SpeakerEq::at_rate(spurts.rate);
    let mut pacer = Pacer::new(Instant::now());
    loop {
        if pacer.catch_up(Instant::now()) {
            faults.raise(Fault::Xrun);
        }
        while pacer.next_due() <= Instant::now() {
            mixer.render(&mut stereo);
            eq.render(&stereo, &mut mono);
            let dropped = spurts.stats().dropped;
            match spurts.block(&mono, ble) {
                Step::Started => tracing::debug!(stream = ?spurts.live(), "speaker spurt started"),
                Step::Ended => tracing::debug!(stats = ?spurts.stats(), "speaker spurt ended"),
                Step::Cut => tracing::debug!("speaker spurt cut: the link went away"),
                Step::Quiet | Step::Playing | Step::Held => {}
            }
            if spurts.stats().dropped > dropped {
                faults.raise(Fault::Xrun);
            }
            pacer.advance();
        }
        match stop.recv_deadline(pacer.next_due()) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    spurts.close(ble);
    drop(mixer);
    spurts
}

struct Running {
    stop: Sender<()>,
    join: std::thread::JoinHandle<Spurts>,
}

/// The deck's speaker over BLE, behind §7.2's output seam.
///
/// `open` starts `deckd-ble-out`, which owns the [`Mixer`] the way a cpal
/// render closure does; `close` stops it, ends an open spurt with
/// `END_ABORT` and joins it, so the lane consumers are back before the
/// route machine asks for the next mixer. The framer outlives each open, so
/// stream ids keep counting and the counters keep adding up.
pub struct BleOutput {
    ble: BleAudio,
    running: Option<Running>,
    parked: Option<Spurts>,
}

impl std::fmt::Debug for BleOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleOutput")
            .field("open", &self.running.is_some())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl BleOutput {
    #[must_use]
    pub fn new(ble: BleAudio) -> BleOutput {
        BleOutput {
            ble,
            running: None,
            // Any id, as long as consecutive runs of the daemon differ.
            parked: Some(Spurts::new((crate::core::new_sid() & 0xFF) as u8)),
        }
    }

    /// The counters as of the last close (the running thread owns the live
    /// ones).
    #[must_use]
    pub fn stats(&self) -> SpeakerStats {
        self.parked.as_ref().map(Spurts::stats).unwrap_or_default()
    }
}

impl OutputSink for BleOutput {
    fn open(
        &mut self,
        rate: u32,
        mixer: Mixer,
        faults: Arc<StreamFaults>,
    ) -> Result<(), AudioError> {
        self.close();
        let expected = output_rate(&self.ble.state(), self.ble.transport());
        if rate != expected {
            return Err(AudioError::Refused(format!(
                "the active deck speaker link runs at {expected} Hz, not {rate}"
            )));
        }
        let mut spurts = self
            .parked
            .take()
            .unwrap_or_else(|| Spurts::new((crate::core::new_sid() & 0xFF) as u8));
        spurts.rate = rate;
        tracing::info!(rate_hz = rate, transport = ?self.ble.transport(), "opening deck speaker");
        let (stop, stopped) = crossbeam_channel::bounded::<()>(1);
        let ble = self.ble.clone();
        let join = std::thread::Builder::new()
            .name("deckd-ble-out".into())
            .spawn(move || speak(&ble, mixer, &faults, &stopped, spurts))
            .map_err(|e| AudioError::Engine(format!("cannot start the BLE speaker: {e}")))?;
        self.running = Some(Running { stop, join });
        Ok(())
    }

    fn close(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        drop(running.stop);
        match running.join.join() {
            Ok(spurts) => self.parked = Some(spurts),
            Err(_) => tracing::error!("the BLE speaker thread panicked"),
        }
    }

    fn is_open(&self) -> bool {
        self.running.is_some()
    }

    fn uid(&self) -> Option<String> {
        uid_of(&self.ble)
    }
}

impl Drop for BleOutput {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engine::{Engine, NullOutput};
    use crate::audio::fakes::{FakeCapture, FakeStt, FakeTts};
    use crate::audio::lanes::{perceptual_gain, Lane, Lanes};
    use crate::audio::tts::{chime_pcm, Chime};

    #[test]
    fn high_rate_is_negotiated_only_for_a_capable_usb_speaker() {
        let capable = LinkState::Ready { max_write: 61, info: Some([1, 1, 16, 20, 7]) };
        assert_eq!(output_rate(&capable, Some(Transport::Usb)), 48_000);
        assert_eq!(output_rate(&capable, Some(Transport::Ble)), 16_000);
        assert_eq!(output_rate(&capable, None), 16_000);
        for info in [None, Some([1, 1, 16, 20, 3]), Some([2, 1, 16, 20, 7]), Some([1, 1, 16, 20, 5])] {
            assert_eq!(output_rate(&LinkState::Ready { max_write: 61, info }, Some(Transport::Usb)), 16_000);
        }
        assert_eq!(route_events(&capable, Some(Transport::Usb))[0], RouteEvent::NominalRate { rate: 48_000 });
    }

    #[test]
    fn usb_output_renders_and_frames_at_48k_and_falls_back_after_detach() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(244, Some(INFO));
        let mut usb = ble.usb_port();
        usb.attach(Some("test"));
        usb.next_report(Instant::now()); // LISTEN
        let mut report = [0; 64];
        codec::report(&[codec::KIND_INFO, 1, 1, 16, 20, 7], &mut report).unwrap();
        assert!(usb.on_report(&report));
        let lanes = Lanes::new();
        let mut producer = lanes.take_producer(Lane::Chime).unwrap();
        for sample in chime_pcm(Chime::Ready, LANE_RATE) { producer.push(sample).unwrap(); }
        let faults = Arc::new(StreamFaults::new());
        let mut output = BleOutput::new(ble.clone());
        output.open(48_000, lanes.take_mixer_at(48_000).unwrap(), faults).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut frames = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "USB spurt did not end");
            if let Some(report) = usb.next_report(Instant::now()) {
                let frame = codec::report_frame(&report).unwrap().to_vec();
                let ended = frame[0] == codec::KIND_END;
                frames.push(frame);
                if ended { break; }
            } else { std::thread::sleep(Duration::from_millis(1)); }
        }
        output.close();
        assert_eq!(&frames[0][..5], &[codec::KIND_START, frames[0][1], 1, 48, 20]);
        let mut samples = 0;
        for (i, frame) in frames[1..frames.len()-1].iter().enumerate() {
            let codec::Frame::Audio { seq, mut state, data, .. } = codec::parse(frame) else { panic!("not audio") };
            assert_eq!(seq, i as u8);
            assert!(frame.len() <= 61);
            let mut pcm = [0;320];
            samples += codec::decode(&mut state, data, &mut pcm);
        }
        assert!((16_000..24_000).contains(&samples), "about 0.42 seconds at 48 kHz, got {samples}");
        assert_eq!(output.stats().dropped, 0);
        usb.detach(None);
        assert_eq!(output_rate(&ble.state(), ble.transport()), 16_000);
        let mixer = lanes.take_mixer_at(48_000).unwrap();
        assert!(output.open(48_000, mixer, Arc::new(StreamFaults::new())).is_err());
    }
    use crate::audio::{FromAudio, LANE_RATE};
    use crate::ble::codec::{
        audio_frame, parse, start_frame, Frame, State, END_LIMIT, KIND_AUDIO, KIND_END, KIND_START,
    };
    use crate::ble::{FakeDeck, MicDecoder};
    use deck_model::{AudioCmd, PttCtx, SetKey, VoiceIn};

    const INFO: [u8; 5] = [1, 1, 16, 20, 3];
    const MIC_ONLY: [u8; 5] = [1, 1, 16, 20, 1];
    const SPEAKER_ONLY: [u8; 5] = [1, 1, 16, 20, 2];

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn audio(stream: u8, seq: u8, lost_frames: u8, samples: Vec<i16>) -> MicEvent {
        MicEvent::Audio {
            stream,
            seq,
            lost_frames,
            samples,
        }
    }

    /// The f32 the capture delivers for one `i16`.
    fn f(sample: i16) -> f32 {
        f32::from(sample) / 32768.0
    }

    fn never(_: &[f32]) {
        panic!("nothing is open, so nothing may be delivered");
    }

    // -- the link -----------------------------------------------------------

    /// The route machine settles on what the link says: the rate first,
    /// then the sides the deck's info offers, and nothing when the link is
    /// not ready.
    #[test]
    fn link_states_become_the_route_events_the_machine_settles_on() {
        let present = |side, present| RouteEvent::DevicePresent { side, present };
        let ready = |max_write, info| LinkState::Ready { max_write, info };
        assert_eq!(
            route_events(&LinkState::Detached, Some(Transport::Ble)),
            [
                present(DeviceSide::Input, false),
                present(DeviceSide::Output, false)
            ]
        );
        assert_eq!(
            route_events(&LinkState::Attaching, Some(Transport::Ble)),
            route_events(&LinkState::Detached, Some(Transport::Ble)),
            "attaching is not ready: the outbox is not there yet"
        );
        assert_eq!(
            route_events(&ready(244, Some(INFO)), Some(Transport::Ble)),
            [
                RouteEvent::NominalRate { rate: 16_000 },
                present(DeviceSide::Input, true),
                present(DeviceSide::Output, true)
            ]
        );
        assert_eq!(sides(&ready(244, Some(MIC_ONLY))), (true, false));
        assert_eq!(sides(&ready(244, Some(SPEAKER_ONLY))), (false, true));
        assert_eq!(
            sides(&ready(244, None)),
            (true, true),
            "an unreadable info is no reason to refuse a working service"
        );
        assert_eq!(
            sides(&ready(244, Some([2, 1, 16, 20, 3]))),
            (false, false),
            "a deck speaking another dialect offers nothing this host can use"
        );
        assert_eq!(sides(&ready(244, Some([1, 1, 8, 20, 3]))), (false, false));
        assert_eq!(
            sides(&ready(6, Some(INFO))),
            (true, false),
            "a write limit with no room for a sample is no speaker"
        );
        assert_eq!(sides(&ready(20, Some(INFO))), (true, true));
    }

    /// `deckd-ble-link` seeds the route with the state now, follows every
    /// change, and lets go when the control loop does.
    #[test]
    fn the_link_watcher_follows_the_link_and_exits_with_the_control_loop() {
        let (ble, deck) = BleAudio::with_fake();
        let (routes, heard) = crossbeam_channel::unbounded();
        let (voice, told) = crossbeam_channel::unbounded();
        let join = watch(ble, routes, voice).expect("a thread");
        let next = || {
            heard
                .recv_timeout(Duration::from_secs(5))
                .expect("an event")
        };
        assert_eq!(
            [next(), next()],
            [
                RouteEvent::DevicePresent {
                    side: DeviceSide::Input,
                    present: false
                },
                RouteEvent::DevicePresent {
                    side: DeviceSide::Output,
                    present: false
                }
            ],
            "seeded with the state it starts in"
        );
        deck.attach(244, Some(INFO));
        let mut seen = Vec::new();
        while !seen.contains(&RouteEvent::DevicePresent {
            side: DeviceSide::Output,
            present: true,
        }) {
            seen.push(next());
        }
        assert!(seen.contains(&RouteEvent::NominalRate { rate: 16_000 }));
        // The radio reports the grant: the core is told once, within the
        // watcher's one-second wake, and not again while it holds.
        deck.bluetooth(Permission::Granted);
        assert_eq!(
            told.recv_timeout(Duration::from_secs(5))
                .expect("the grant"),
            FromAudio::Voice(VoiceIn::Bluetooth {
                permission: Permission::Granted
            })
        );
        assert!(told.recv_timeout(Duration::from_millis(1500)).is_err());
        deck.detach("the deck went away");
        let mut seen = Vec::new();
        while !seen.contains(&RouteEvent::DevicePresent {
            side: DeviceSide::Output,
            present: false,
        }) {
            seen.push(next());
        }
        drop(heard);
        join.join().expect("it exits once nobody listens");
    }

    // -- the capture's feed -------------------------------------------------

    /// The point of the pre-roll: the press's audio can beat the press's HID
    /// line to the Mac, and the capture opened on the line still hears the
    /// press from its first frame — then the live frames, then nothing once
    /// the stream has ended.
    #[test]
    fn a_capture_opened_after_the_press_hears_it_from_its_first_frame() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        feed.event(t0, MicEvent::Start { stream: 7 }, &mut never);
        for seq in 0..10u8 {
            let level = (i16::from(seq) + 1) * 100;
            feed.event(
                t0 + ms(20) * u32::from(seq),
                audio(7, seq, 0, vec![level; BLOCK]),
                &mut never,
            );
        }
        assert_eq!(feed.preroll(), 10 * BLOCK);

        let mut got: Vec<f32> = Vec::new();
        let mut buffers = 0;
        let opened = feed.open(t0 + ms(200), &mut |s| {
            got.extend_from_slice(s);
            buffers += 1;
        });
        assert_eq!(
            opened,
            Opened::PreRoll {
                stream: 7,
                samples: 10 * BLOCK
            }
        );
        assert_eq!(buffers, 10, "a buffer a frame, like a device callback");
        assert_eq!(got.len(), 10 * BLOCK);
        assert_eq!(got[0], f(100), "from the press's first frame");
        assert_eq!(got[9 * BLOCK], f(1000));

        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(
            t0 + ms(220),
            audio(7, 10, 0, vec![1100; BLOCK]),
            &mut deliver,
        );
        assert_eq!(got.len(), 11 * BLOCK, "and live after it");
        assert_eq!(got[10 * BLOCK], f(1100));

        // The release. A new press before the capture is closed is the next
        // capture's, not this one's.
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(
            t0 + ms(240),
            MicEvent::End {
                stream: 7,
                next_seq: 11,
                reason: END_RELEASE,
            },
            &mut deliver,
        );
        feed.event(t0 + ms(300), MicEvent::Start { stream: 8 }, &mut deliver);
        feed.event(t0 + ms(320), audio(8, 0, 0, vec![5; BLOCK]), &mut deliver);
        assert_eq!(got.len(), 11 * BLOCK, "a capture is one press");
        assert!(feed.is_open(), "done, but open until the engine closes it");
        feed.close();
        assert!(!feed.is_open());
    }

    /// The HID line can also come first: the capture waits for the stream
    /// — and never replays one that already ended.
    #[test]
    fn a_capture_opened_before_the_press_waits_for_its_stream() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        // An earlier press, over before this capture opens.
        feed.event(t0, MicEvent::Start { stream: 3 }, &mut never);
        feed.event(t0, audio(3, 0, 0, vec![9; BLOCK]), &mut never);
        feed.event(
            t0,
            MicEvent::End {
                stream: 3,
                next_seq: 1,
                reason: END_RELEASE,
            },
            &mut never,
        );
        assert_eq!(feed.preroll(), 0, "an ended stream is nobody's pre-roll");

        let mut got: Vec<f32> = Vec::new();
        assert_eq!(feed.open(t0 + ms(5), &mut never), Opened::Waiting);
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(t0 + ms(30), MicEvent::Start { stream: 4 }, &mut deliver);
        feed.event(t0 + ms(50), audio(4, 0, 0, vec![40; BLOCK]), &mut deliver);
        feed.event(t0 + ms(70), audio(4, 1, 0, vec![41; BLOCK]), &mut deliver);
        assert_eq!(got.len(), 2 * BLOCK);
        assert_eq!((got[0], got[BLOCK]), (f(40), f(41)));
    }

    /// A lost frame costs its own 20 ms of silence and nothing else, so the
    /// recogniser's timeline stays the deck's; the same for the tail the
    /// END reports; and a stall is capped rather than replayed as seconds
    /// of zeros into the engine's ring.
    #[test]
    fn lost_frames_are_silence_of_their_own_length_and_a_stall_is_capped() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        let mut got: Vec<f32> = Vec::new();
        feed.open(t0, &mut never);
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(t0, MicEvent::Start { stream: 1 }, &mut deliver);
        feed.event(t0, audio(1, 0, 0, vec![100; BLOCK]), &mut deliver);
        feed.event(t0, audio(1, 3, 2, vec![200; BLOCK]), &mut deliver);
        feed.event(
            t0,
            MicEvent::End {
                stream: 1,
                next_seq: 6,
                reason: END_LIMIT,
            },
            &mut deliver,
        );
        let mut want = vec![f(100); BLOCK];
        want.extend(vec![0.0; 2 * BLOCK]);
        want.extend(vec![f(200); BLOCK]);
        want.extend(vec![0.0; 2 * BLOCK]);
        assert_eq!(
            got, want,
            "frame 0, frames 1-2 lost, frame 3, 4-5 lost at the tail"
        );
        assert_eq!(feed.filled(), 4 * BLOCK as u64);

        // A small attribute limit: a lost frame is worth what this stream's
        // frames carry, not a full block.
        let mut feed = Feed::new();
        let mut got: Vec<f32> = Vec::new();
        feed.open(t0, &mut never);
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(t0, MicEvent::Start { stream: 2 }, &mut deliver);
        feed.event(t0, audio(2, 0, 0, vec![1; 28]), &mut deliver);
        feed.event(t0, audio(2, 4, 3, vec![1; 28]), &mut deliver);
        assert_eq!(got.len(), 28 + 3 * 28 + 28);

        // 255 frames is 5.1 s: a stall, capped at a second.
        let mut feed = Feed::new();
        let mut got: Vec<f32> = Vec::new();
        feed.open(t0, &mut never);
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(t0, MicEvent::Start { stream: 3 }, &mut deliver);
        feed.event(t0, audio(3, 255, 255, vec![7; BLOCK]), &mut deliver);
        assert_eq!(got.len(), RATE as usize + BLOCK);
        assert_eq!(*got.last().unwrap(), f(7));
    }

    /// A long press before the open keeps its last two seconds, not its
    /// first: the pre-roll runs into the live frames without a seam.
    #[test]
    fn the_preroll_is_the_last_two_seconds_of_a_long_press() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        feed.event(t0, MicEvent::Start { stream: 9 }, &mut never);
        for seq in 0..150u32 {
            feed.event(
                t0 + ms(20) * seq,
                audio(9, seq as u8, 0, vec![seq as i16; BLOCK]),
                &mut never,
            );
        }
        assert_eq!(feed.preroll(), PREROLL_SAMPLES);
        let mut got: Vec<f32> = Vec::new();
        let opened = feed.open(t0 + ms(3000), &mut |s| got.extend_from_slice(s));
        assert_eq!(
            opened,
            Opened::PreRoll {
                stream: 9,
                samples: PREROLL_SAMPLES
            }
        );
        assert_eq!(got.len(), 100 * BLOCK);
        assert_eq!(got[0], f(50), "frame 50 of 150: the last 2 s");
        assert_eq!(*got.last().unwrap(), f(149));
    }

    /// Two ways a stream can be the wrong one: it went quiet without an END
    /// (stale), or the link cut it short — after which the deck's next
    /// stream is still this capture's.
    #[test]
    fn a_stale_stream_is_not_replayed_and_a_cut_stream_waits_for_the_next() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        feed.event(t0, MicEvent::Start { stream: 1 }, &mut never);
        feed.event(t0, audio(1, 0, 0, vec![11; BLOCK]), &mut never);
        assert_eq!(
            feed.open(t0 + STALE + ms(1), &mut never),
            Opened::Waiting,
            "a second and more without a frame: its END went missing"
        );

        let mut got: Vec<f32> = Vec::new();
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        // What the decoder emits when the next press starts.
        feed.event(
            t0 + ms(1100),
            MicEvent::End {
                stream: 1,
                next_seq: 1,
                reason: END_ERROR,
            },
            &mut deliver,
        );
        feed.event(t0 + ms(1100), MicEvent::Start { stream: 2 }, &mut deliver);
        feed.event(t0 + ms(1120), audio(2, 0, 0, vec![22; BLOCK]), &mut deliver);
        assert_eq!(got, vec![f(22); BLOCK]);

        // The link drops mid-press (the host's own END_ERROR): back to
        // waiting, and the next stream is heard.
        let mut deliver = |s: &[f32]| got.extend_from_slice(s);
        feed.event(
            t0 + ms(1140),
            MicEvent::End {
                stream: 2,
                next_seq: 1,
                reason: END_ERROR,
            },
            &mut deliver,
        );
        feed.event(t0 + ms(1500), MicEvent::Start { stream: 3 }, &mut deliver);
        feed.event(t0 + ms(1520), audio(3, 0, 0, vec![33; BLOCK]), &mut deliver);
        assert_eq!(got.len(), 2 * BLOCK);
        assert_eq!(got[BLOCK], f(33));
    }

    /// A close forgets what the capture heard: a re-open during the same
    /// stream starts from the close, not from the press again.
    #[test]
    fn a_reopen_during_the_same_stream_does_not_hear_it_twice() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        feed.event(t0, MicEvent::Start { stream: 5 }, &mut never);
        for seq in 0..5u8 {
            feed.event(t0, audio(5, seq, 0, vec![1; BLOCK]), &mut never);
        }
        let mut first = 0;
        feed.open(t0, &mut |s| first += s.len());
        assert_eq!(first, 5 * BLOCK);
        feed.close();
        for seq in 5..8u8 {
            feed.event(t0, audio(5, seq, 0, vec![2; BLOCK]), &mut never);
        }
        let mut again: Vec<f32> = Vec::new();
        let opened = feed.open(t0, &mut |s| again.extend_from_slice(s));
        assert_eq!(
            opened,
            Opened::PreRoll {
                stream: 5,
                samples: 3 * BLOCK
            }
        );
        assert_eq!(again, vec![f(2); 3 * BLOCK]);
    }

    #[test]
    fn a_quick_repress_is_heard_even_when_its_line_beats_the_old_end() {
        let t0 = Instant::now();
        let mut feed = Feed::new();
        // Press 1: the HID line is the faster path, so its capture opens
        // first and binds stream 7 at its START.
        assert_eq!(feed.open(t0, &mut never), Opened::Waiting);
        feed.event(t0, MicEvent::Start { stream: 7 }, &mut never);
        feed.event(t0, audio(7, 0, 0, vec![1; BLOCK]), &mut |_| {});
        // The release closes it, and the re-press's line opens the next
        // capture before END(7) has come over the radio.
        feed.close();
        let mut heard: Vec<f32> = Vec::new();
        feed.open(t0 + ms(30), &mut |s| heard.extend_from_slice(s));
        feed.event(
            t0 + ms(40),
            MicEvent::End {
                stream: 7,
                next_seq: 1,
                reason: END_RELEASE,
            },
            &mut |s| heard.extend_from_slice(s),
        );
        // Whatever of press 1 leaked in, press 2's own stream is heard.
        heard.clear();
        feed.event(t0 + ms(50), MicEvent::Start { stream: 8 }, &mut |s| {
            heard.extend_from_slice(s);
        });
        feed.event(t0 + ms(60), audio(8, 0, 0, vec![3; BLOCK]), &mut |s| {
            heard.extend_from_slice(s);
        });
        assert_eq!(heard, vec![f(3); BLOCK]);
        // And when press 2's stream ends, that capture is done.
        feed.event(
            t0 + ms(70),
            MicEvent::End {
                stream: 8,
                next_seq: 1,
                reason: END_RELEASE,
            },
            &mut never,
        );
        feed.event(t0 + ms(80), MicEvent::Start { stream: 9 }, &mut never);
        feed.event(t0 + ms(90), audio(9, 0, 0, vec![4; BLOCK]), &mut never);
    }

    // -- the capture --------------------------------------------------------

    fn start(stream: u8) -> Vec<u8> {
        let mut out = [0u8; 8];
        let n = start_frame(stream, &mut out);
        out[..n].to_vec()
    }

    fn frame(stream: u8, seq: u8, state: &mut State, pcm: &[i16]) -> Vec<u8> {
        let mut out = [0u8; MAX_FRAME];
        let n = audio_frame(stream, seq, state, pcm, &mut out);
        out[..n].to_vec()
    }

    /// A press as the deck sends it: START and `frames` AUDIO frames of a
    /// 300 Hz tone at about −12 dBFS, plus what a decoder makes of them.
    fn press(stream: u8, frames: u8) -> (Vec<Vec<u8>>, Vec<i16>) {
        let mut state = State::default();
        let mut values = vec![start(stream)];
        let mut pcm_all = Vec::new();
        for seq in 0..frames {
            let pcm: Vec<i16> = (0..BLOCK)
                .map(|i| {
                    let t = (usize::from(seq) * BLOCK + i) as f64 / f64::from(RATE);
                    (8000.0 * (std::f64::consts::TAU * 300.0 * t).sin()) as i16
                })
                .collect();
            values.push(frame(stream, seq, &mut state, &pcm));
            pcm_all.extend(pcm);
        }
        let mut decoded = Vec::new();
        let mut decoder = MicDecoder::new();
        for value in &values {
            decoder.push(value, |e| {
                if let MicEvent::Audio { samples, .. } = e {
                    decoded.extend(samples);
                }
            });
        }
        let _ = pcm_all;
        (values, decoded)
    }

    type Heard = Arc<Mutex<Vec<(u32, Vec<f32>)>>>;

    fn sink(heard: &Heard) -> CaptureSink {
        let heard = heard.clone();
        CaptureSink::new(Arc::new(StreamFaults::new()), move |rate, samples| {
            heard.lock().unwrap().push((rate, samples.to_vec()));
        })
    }

    fn req(input: VoiceInput, fallback: &str) -> CaptureOpen {
        CaptureOpen {
            input,
            device_uid: None,
            fallback: fallback.into(),
            ask_deck: None,
        }
    }

    /// `voice.listen` (nobody pressed) asks the deck for its microphone for
    /// the session's budget and lets it go at the close; a press, which the
    /// deck is already streaming, asks for nothing.
    #[test]
    fn an_api_listen_asks_the_deck_for_its_microphone_and_a_press_does_not() {
        let (ble, deck) = BleAudio::with_fake();
        let (mac, _) = FakeCapture::new();
        let mut capture = BleCapture::unread(ble.clone(), Box::new(mac));
        let heard: Heard = Arc::default();
        deck.attach(244, Some(INFO));

        let api = CaptureOpen {
            ask_deck: Some(30),
            ..req(VoiceInput::Deck, "none")
        };
        capture.open(&api, sink(&heard)).unwrap();
        assert_eq!(deck.written(8), [vec![codec::KIND_MIC, codec::MIC_ON, 30]]);
        capture.close();
        assert_eq!(deck.written(8), [vec![codec::KIND_MIC, codec::MIC_OFF, 0]]);
        // Closing twice (the engine's Finish retry) owes nothing more.
        capture.close();
        assert!(deck.written(8).is_empty());

        // A press: nothing asked, nothing to let go of.
        capture
            .open(&req(VoiceInput::Deck, "none"), sink(&heard))
            .unwrap();
        capture.close();
        assert!(deck.written(8).is_empty());

        // The Mac's input never involves the deck.
        let mac_input = CaptureOpen {
            ask_deck: Some(30),
            ..req(VoiceInput::Mac, "system_default")
        };
        capture.open(&mac_input, sink(&heard)).unwrap();
        capture.close();
        assert!(deck.written(8).is_empty());
    }

    /// The deck's microphone at 16 kHz mono f32 (`i16 / 32768`, exactly
    /// what the decoder made), its pre-roll first; and without a ready link
    /// the open falls back exactly as the CoreAudio capture does without
    /// the deck.
    #[test]
    fn the_capture_delivers_the_deck_at_16_khz_and_falls_back_like_coreaudio() {
        let (ble, deck) = BleAudio::with_fake();
        let (mac, mac_handle) = FakeCapture::new();
        let mut capture = BleCapture::unread(ble.clone(), Box::new(mac));
        let pump = capture.pumper();
        let heard: Heard = Arc::default();

        // No link, `fallback_input = "none"`: silence and a code.
        let err = capture
            .open(&req(VoiceInput::Deck, "none"), sink(&heard))
            .unwrap_err();
        assert_eq!(err.code(), "no_device");
        assert!(mac_handle.opens().is_empty() && !capture.is_open());
        // `"system_default"`: the Mac's own input.
        capture
            .open(&req(VoiceInput::Deck, "system_default"), sink(&heard))
            .unwrap();
        assert_eq!(mac_handle.opens().len(), 1);
        assert!(capture.is_open());
        capture.close();
        assert!(!mac_handle.is_open() && !capture.is_open());

        // A deck with no microphone is no microphone.
        deck.attach(244, Some(SPEAKER_ONLY));
        assert!(capture
            .open(&req(VoiceInput::Deck, "none"), sink(&heard))
            .is_err());

        // The deck, with a press already under way when the line arrives.
        deck.detach("reconnecting");
        deck.attach(244, Some(INFO));
        let (values, decoded) = press(5, 4);
        for value in &values[..4] {
            deck.notify(value);
        }
        pump(Instant::now());
        let opened = capture
            .open(&req(VoiceInput::Deck, "none"), sink(&heard))
            .unwrap();
        assert_eq!(
            opened,
            CaptureOpened {
                rate: 16_000,
                channels: 1,
                uid: Some("ble:FAKE".into())
            }
        );
        assert_eq!(mac_handle.opens().len(), 1, "not the Mac this time");
        deck.notify(&values[4]);
        pump(Instant::now());
        let got = heard.lock().unwrap().clone();
        assert_eq!(got.len(), 4, "three frames of pre-roll and one live");
        assert!(got
            .iter()
            .all(|(rate, s)| *rate == 16_000 && s.len() == BLOCK));
        let samples: Vec<f32> = got.into_iter().flat_map(|(_, s)| s).collect();
        let want: Vec<f32> = decoded.iter().map(|s| f(*s)).collect();
        assert_eq!(
            samples, want,
            "the decoder's samples, scaled and nothing else"
        );

        // `voice.listen{input:"mac"}` is the Mac's input, deck or no deck.
        capture
            .open(&req(VoiceInput::Mac, "none"), sink(&heard))
            .unwrap();
        assert_eq!(mac_handle.opens().len(), 2);
        capture.close();
        capture.close();
    }

    /// The real reader thread: notifications reach an open capture without
    /// anybody pumping.
    #[test]
    fn the_reader_thread_delivers_live_frames() {
        let (ble, deck) = BleAudio::with_fake();
        let (mac, _) = FakeCapture::new();
        let mut capture = BleCapture::new(ble, Box::new(mac));
        deck.attach(244, Some(INFO));
        let heard: Heard = Arc::default();
        capture
            .open(&req(VoiceInput::Deck, "none"), sink(&heard))
            .unwrap();
        let (values, decoded) = press(1, 3);
        for value in &values {
            deck.notify(value);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while heard.lock().unwrap().len() < 3 && Instant::now() < deadline {
            std::thread::sleep(ms(2));
        }
        let samples: Vec<f32> = heard
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, s)| s.clone())
            .collect();
        assert_eq!(samples.len(), decoded.len());
        drop(capture); // joins the reader
    }

    fn listen() -> AudioCmd {
        AudioCmd::Listen {
            agent: None,
            ctx: PttCtx::Talk,
            card: None,
            max_s: 60,
            from_deck: true,
            input: VoiceInput::Deck,
            locale: "en-US".into(),
            contextual: Vec::new(),
            companion: None,
        }
    }

    /// Through the engine: a press whose audio beat its HID line is heard
    /// from its first frame, the liveness edge fires on it, and `set mic`
    /// is applied exactly once — by the engine's sink, as for CoreAudio.
    #[test]
    fn the_engine_hears_the_press_from_its_first_frame_with_the_gain_applied_once() {
        let run = |mic: u16| -> (f32, Vec<(u32, usize)>, bool) {
            let (ble, deck) = BleAudio::with_fake();
            let (mac, _) = FakeCapture::new();
            let capture = BleCapture::unread(ble.clone(), Box::new(mac));
            let pump = capture.pumper();
            let (to_core, from_core) = crossbeam_channel::unbounded();
            let mut engine = Engine::new(
                Lanes::new(),
                Box::new(capture),
                Box::new(NullOutput::default()),
                to_core,
            );
            engine.set_transport(crate::config::VoiceTransport::Ble);
            let (stt, stt_handle) = FakeStt::new();
            engine.set_stt(Arc::new(stt));
            let t0 = Instant::now();
            engine.on_cmd(
                AudioCmd::Gain {
                    key: SetKey::Mic,
                    value: mic,
                },
                t0,
            );
            deck.attach(244, Some(INFO));
            let (values, _) = press(2, 10);
            for value in &values {
                deck.notify(value);
            }
            pump(Instant::now());
            // The `ptt` line lands after the audio.
            engine.on_cmd(listen(), t0);
            engine.tick(t0 + ms(50));
            let reports: Vec<VoiceIn> = from_core.try_iter().filter_map(FromAudio::voice).collect();
            let level = reports
                .iter()
                .find_map(|v| match v {
                    VoiceIn::Level { rms } => Some(*rms),
                    _ => None,
                })
                .expect("a level");
            let live = reports.contains(&VoiceIn::Live);
            (level, stt_handle.fed(), live)
        };
        let (unity, fed, live) = run(10);
        assert_eq!(
            fed,
            vec![(16_000, 10 * BLOCK)],
            "all ten frames, at the deck's rate"
        );
        assert!(live, "the press's own frames are the liveness edge");
        assert!(unity > 0.1, "a −12 dBFS tone: {unity}");
        let (quiet, _, _) = run(1);
        let ratio = quiet / unity;
        assert!(
            (ratio - perceptual_gain(1)).abs() < perceptual_gain(1) * 1e-3,
            "set mic 1 scales by {} once, not twice: {ratio}",
            perceptual_gain(1)
        );
    }

    // -- the speaker's framing ----------------------------------------------

    /// One 20 ms block of a 440 Hz sine at `amplitude`, block `k` of a run.
    fn tone(k: usize, amplitude: f64) -> Vec<i16> {
        (0..BLOCK)
            .map(|i| {
                let t = (k * BLOCK + i) as f64 / f64::from(RATE);
                (amplitude * (std::f64::consts::TAU * 440.0 * t).sin()).round() as i16
            })
            .collect()
    }

    fn quiet() -> Vec<i16> {
        vec![0; BLOCK]
    }

    fn drain(deck: &FakeDeck) -> Vec<Vec<u8>> {
        deck.written(10_000)
    }

    /// `(kind, stream, seq or next_seq)` per frame.
    fn shape(frames: &[Vec<u8>]) -> Vec<(u8, u8, u8)> {
        frames.iter().map(|f| (f[0], f[1], f[2])).collect()
    }

    fn decode(frames: &[Vec<u8>]) -> Vec<MicEvent> {
        let mut decoder = MicDecoder::new();
        let mut events = Vec::new();
        for value in frames {
            decoder.push(value, |e| events.push(e));
        }
        events
    }

    /// A burst of sound is one spurt: START, the silent block before it,
    /// the sound, 300 ms of quiet tail, END — every AUDIO frame carrying
    /// the one coder state that runs across the spurt, so any of them
    /// decodes on its own.
    #[test]
    fn a_burst_of_sound_is_one_spurt_with_one_coder_state_and_a_hangover() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(244, Some(INFO));
        let mut spurts = Spurts::new(200);
        let mut fed: Vec<i16> = Vec::new();
        assert_eq!(spurts.block(&quiet(), &ble), Step::Quiet);
        assert_eq!(spurts.block(&quiet(), &ble), Step::Quiet);
        assert!(drain(&deck).is_empty(), "silence sends nothing");
        fed.extend(quiet()); // the pre-roll
        for k in 0..5 {
            let block = tone(k, 8000.0);
            let step = spurts.block(&block, &ble);
            assert_eq!(step, if k == 0 { Step::Started } else { Step::Playing });
            fed.extend(block);
        }
        for n in 1..=15 {
            let step = spurts.block(&quiet(), &ble);
            fed.extend(quiet());
            let want = if n < 15 { Step::Playing } else { Step::Ended };
            assert_eq!(step, want, "quiet block {n}: 300 ms is 15 blocks");
        }
        let frames = drain(&deck);
        let mut want = vec![(KIND_START, 200, 1)];
        want.extend((0..21u8).map(|seq| (KIND_AUDIO, 200, seq)));
        want.push((KIND_END, 200, 21));
        assert_eq!(shape(&frames), want);
        assert_eq!(frames.last().unwrap()[3], END_RELEASE);
        assert!(frames[1..22].iter().all(|f| f.len() == MAX_FRAME));

        let mut reference = State::default();
        for (i, value) in frames[1..22].iter().enumerate() {
            let Frame::Audio { state, data, .. } = parse(value) else {
                panic!("frame {i} is AUDIO");
            };
            assert_eq!(state, reference, "frame {i} carries the running state");
            let mut decoded = [0i16; BLOCK];
            assert_eq!(codec::decode(&mut reference, data, &mut decoded), BLOCK);
        }
        let events = decode(&frames);
        assert_eq!(events.len(), 23);
        assert!(matches!(
            events.last(),
            Some(MicEvent::End {
                stream: 200,
                next_seq: 21,
                reason: END_RELEASE
            })
        ));
        let decoded: Vec<i16> = events
            .iter()
            .filter_map(|e| match e {
                MicEvent::Audio { samples, .. } => Some(samples.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(decoded.len(), fed.len());
        let tone_db = crate::ble::signal::tone_ratio_db(&decoded[BLOCK..6 * BLOCK], 440.0);
        assert!(tone_db > 20.0, "the tone survives the codec: {tone_db} dB");
        assert_eq!(
            spurts.stats(),
            SpeakerStats {
                spurts: 1,
                frames: 21,
                dropped: 0,
                cut: 0
            }
        );
        assert_eq!(spurts.live(), None);

        // The next spurt is the next stream.
        spurts.block(&tone(0, 8000.0), &ble);
        assert_eq!(drain(&deck)[0][..2], [KIND_START, 201]);
    }

    /// Silence detection must not chop speech: a pause shorter than the
    /// hangover stays inside the spurt, the threshold is a few LSB, and the
    /// block before the first loud one goes out ahead of it.
    #[test]
    fn pauses_stay_inside_the_spurt_and_a_quiet_onset_is_kept() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(244, Some(INFO));
        assert!(is_quiet(&[0, 8, -8, 3]));
        assert!(!is_quiet(&[0, 9, 0]));
        assert!(!is_quiet(&[-9]));

        let mut spurts = Spurts::new(0);
        // An onset too quiet to start a spurt, then the word.
        let onset: Vec<i16> = (0..BLOCK).map(|i| (i % 7) as i16).collect();
        assert_eq!(spurts.block(&onset, &ble), Step::Quiet);
        assert_eq!(spurts.block(&tone(0, 3000.0), &ble), Step::Started);
        // A 280 ms pause between words: still one spurt.
        for _ in 0..14 {
            assert_eq!(spurts.block(&quiet(), &ble), Step::Playing);
        }
        assert_eq!(spurts.block(&tone(1, 3000.0), &ble), Step::Playing);
        let frames = drain(&deck);
        assert_eq!(
            frames.iter().filter(|f| f[0] == KIND_START).count(),
            1,
            "one spurt across the pause"
        );
        assert!(frames.iter().all(|f| f[0] != KIND_END));
        // The onset went out first, as AUDIO seq 0.
        let events = decode(&frames);
        let MicEvent::Audio { seq, samples, .. } = &events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(*seq, 0);
        assert!(
            samples.iter().any(|s| *s != 0),
            "the quiet onset is in the stream, not cut off"
        );
        // A sound one LSB over the threshold is a sound.
        let mut spurts = Spurts::new(9);
        assert_eq!(spurts.block(&vec![9; BLOCK], &ble), Step::Started);
    }

    /// Never block on the radio: a full queue costs frames (counted, and
    /// the `seq` moves on so the deck sees the gap) but not the spurt; a
    /// link that goes away cuts it; a link that is not there holds sound
    /// back rather than queueing it for a connection that does not exist.
    #[test]
    fn a_full_queue_costs_frames_and_a_lost_link_cuts_the_spurt() {
        let (ble, deck) = BleAudio::with_fake();
        let mut spurts = Spurts::new(40);
        assert_eq!(spurts.block(&tone(0, 8000.0), &ble), Step::Held);
        assert_eq!(spurts.stats(), SpeakerStats::default());

        deck.attach(244, Some(INFO));
        // Nothing drains: START and 63 AUDIO frames fill the 64-frame queue.
        for k in 0..70 {
            let step = spurts.block(&tone(k, 8000.0), &ble);
            assert!(matches!(step, Step::Started | Step::Playing), "{step:?}");
        }
        // 1 + 1 (the held block, as pre-roll) + 70 frames into 64 places.
        assert_eq!(spurts.stats().dropped, 8);
        assert_eq!(spurts.stats().frames, 63);
        assert_eq!(drain(&deck).len(), 64);
        spurts.block(&tone(70, 8000.0), &ble);
        let next = drain(&deck);
        assert_eq!(
            shape(&next),
            [(KIND_AUDIO, 40, 71)],
            "the gap is the deck's to see"
        );

        deck.detach("the deck went away");
        assert_eq!(spurts.block(&tone(71, 8000.0), &ble), Step::Cut);
        assert_eq!(spurts.stats().cut, 1);
        assert_eq!(spurts.live(), None);
        deck.attach(244, Some(INFO));
        assert_eq!(spurts.block(&tone(72, 8000.0), &ble), Step::Started);
        assert_eq!(drain(&deck)[0][..2], [KIND_START, 41], "a new stream");
    }

    /// On a small attribute limit the frames are shorter but the stream is
    /// the same: a release flushes the partial frame before END, and a
    /// close mid-spurt is END `abort`.
    #[test]
    fn a_feed_has_ended_only_once_its_bound_stream_did() {
        // Waiting, Done and Closed all mean nothing more is coming; only a
        // bound, live stream keeps the release waiting for its END.
        let mut feed = Feed::default();
        let t0 = Instant::now();
        let mut sink = |_: &[f32]| {};
        assert!(feed.ended(), "nothing open");
        assert_eq!(feed.open(t0, &mut sink), Opened::Waiting);
        assert!(feed.ended(), "waiting for a START is not a live stream");
        feed.event(t0, MicEvent::Start { stream: 1 }, &mut sink);
        assert!(!feed.ended(), "frames may still be on the radio");
        feed.event(
            t0,
            MicEvent::Audio {
                stream: 1,
                seq: 0,
                lost_frames: 0,
                samples: vec![0; 160],
            },
            &mut sink,
        );
        assert!(!feed.ended());
        feed.event(
            t0,
            MicEvent::End {
                stream: 1,
                next_seq: 1,
                reason: 0,
            },
            &mut sink,
        );
        assert!(feed.ended(), "END: the tail is in");
        feed.close();
        assert!(feed.ended());
    }

    #[test]
    fn small_frames_flush_their_tail_and_a_close_aborts_the_spurt() {
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(20, Some(INFO));
        let mut spurts = Spurts::new(3);
        // Twelve frames a block: the radio takes them as they come.
        spurts.block(&tone(0, 8000.0), &ble);
        let mut frames = drain(&deck);
        for _ in 0..15 {
            spurts.block(&quiet(), &ble);
            frames.extend(drain(&deck));
        }
        assert_eq!(spurts.stats().dropped, 0);
        assert!(frames.iter().all(|f| f.len() <= 20));
        let events = decode(&frames);
        let samples: usize = events
            .iter()
            .map(|e| match e {
                MicEvent::Audio { samples, .. } => samples.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(
            samples,
            16 * BLOCK,
            "every sample, the partial tail included"
        );
        assert!(matches!(
            events.last(),
            Some(MicEvent::End {
                reason: END_RELEASE,
                ..
            })
        ));

        deck.detach("again");
        deck.attach(244, Some(INFO));
        spurts.block(&tone(1, 8000.0), &ble);
        spurts.close(&ble);
        let frames = drain(&deck);
        let end = frames.last().unwrap();
        assert_eq!(end[..2], [KIND_END, 4]);
        assert_eq!(end[3], END_ABORT);
        spurts.close(&ble);
        assert!(drain(&deck).is_empty(), "nothing open, nothing to end");
    }

    /// The pacing clock: the first 60 ms go at once and every block after it
    /// on the 20 ms beat, so the rendered audio always runs 40–60 ms ahead
    /// (the lead, less at most one block); a stall restarts the clock
    /// instead of bursting the backlog at the deck.
    #[test]
    fn the_speaker_runs_sixty_ms_ahead_and_restarts_its_clock_after_a_stall() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0);
        assert_eq!(pacer.next_due(), t0);
        for step in 0..=1000u64 {
            let now = t0 + ms(step);
            while pacer.next_due() <= now {
                pacer.advance();
            }
            let ahead = BLOCK_TIME * pacer.blocks() as u32 - ms(step);
            assert!(
                ahead > LEAD - BLOCK_TIME && ahead <= LEAD,
                "at {step} ms: {ahead:?} ahead"
            );
        }
        assert!(!pacer.catch_up(t0 + ms(1000)));
        // The thread slept through half a second.
        let late = t0 + ms(1500);
        assert!(pacer.catch_up(late));
        assert_eq!(pacer.blocks(), 0);
        let mut burst = 0;
        while pacer.next_due() <= late {
            pacer.advance();
            burst += 1;
        }
        assert_eq!(burst, 3, "the lead again, not 25 blocks of backlog");
        // Late, but not by more than MAX_BEHIND: catch up by rendering.
        let mut pacer = Pacer::new(t0);
        assert!(!pacer.catch_up(t0 + MAX_BEHIND));
        assert_eq!(
            crate::ble::probe::due(5, BLOCK, LEAD),
            ms(60),
            "the bench probe's rule"
        );
    }

    // -- the speaker --------------------------------------------------------

    /// Poll the fake deck until an END arrives (or 5 s pass).
    fn until_end(deck: &FakeDeck) -> Vec<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut frames = Vec::new();
        while Instant::now() < deadline {
            frames.extend(drain(deck));
            if frames.iter().any(|f| f[0] == KIND_END) {
                break;
            }
            std::thread::sleep(ms(5));
        }
        frames
    }

    /// Measure what the device decodes against an uncorrected mixer at the
    /// same rate. This catches a bypass or EQ applied after PCM encoding,
    /// independently of the filter's own response tests.
    #[test]
    fn the_speaker_eq_reaches_the_encoded_device_stream() {
        let tones = [100.0, 1100.0, 3000.0, 6300.0];
        let fill = || {
            let lanes = Lanes::new();
            let mut producer = lanes.take_producer(Lane::Chime).unwrap();
            for hz in tones {
                for i in 0..LANE_RATE / 4 {
                    let sample = (0.25
                        * (std::f64::consts::TAU * hz * f64::from(i) / f64::from(LANE_RATE)).sin())
                        as f32;
                    producer.push(sample).unwrap();
                    producer.push(sample).unwrap();
                }
            }
            lanes
        };
        let reference = fill();
        let mut raw = vec![0.0; RATE as usize * 2];
        reference.take_mixer_at(RATE).unwrap().render(&mut raw);
        let raw: Vec<i16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[l, r]| (0.5 * (l + r) * 32768.0).round() as i16)
            .collect();

        let lanes = fill();
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(244, Some(INFO));
        let mut out = BleOutput::new(ble);
        out.open(
            RATE,
            lanes.take_mixer_at(RATE).unwrap(),
            Arc::new(StreamFaults::new()),
        )
        .unwrap();
        let frames = until_end(&deck);
        out.close();
        let decoded: Vec<i16> = decode(&frames)
            .into_iter()
            .filter_map(|e| match e {
                MicEvent::Audio {
                    samples,
                    lost_frames,
                    ..
                } => {
                    assert_eq!(lost_frames, 0);
                    Some(samples)
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert!(decoded.len() >= RATE as usize);
        for (i, (hz, expected)) in tones.into_iter().zip([-19.1, -8.0, -3.6, -9.0]).enumerate() {
            // The middle 100 ms avoids the mixer fade and tone transitions.
            let start = i * RATE as usize / 4 + RATE as usize / 10;
            let end = start + RATE as usize / 10;
            let correction = crate::ble::signal::tone_dbfs(&decoded[start..end], hz)
                - crate::ble::signal::tone_dbfs(&raw[start..end], hz);
            assert!(
                (correction - expected).abs() < 1.0,
                "{hz} Hz: decoded correction {correction} dB, expected {expected} dB"
            );
        }
        assert_eq!(out.stats().dropped, 0);
    }

    /// The device end to end, on its own thread: a chime in the lanes comes
    /// out as one spurt the deck can play, the mixer's 44.1 kHz turned into
    /// the deck's 16 kHz, and a close hands the lanes back.
    #[test]
    fn the_speaker_plays_a_chime_from_the_lanes_as_one_spurt() {
        let lanes = Lanes::new();
        let mut producer = lanes.take_producer(Lane::Chime).unwrap();
        for sample in chime_pcm(Chime::Ready, LANE_RATE) {
            producer.push(sample).unwrap();
        }
        let (ble, deck) = BleAudio::with_fake();
        deck.attach(244, Some(INFO));
        let mut out = BleOutput::new(ble);
        let faults = Arc::new(StreamFaults::new());
        let wrong = out.open(44_100, lanes.take_mixer_at(44_100).unwrap(), faults.clone());
        assert!(matches!(wrong, Err(AudioError::Refused(_))));
        assert!(!out.is_open());
        out.open(16_000, lanes.take_mixer_at(16_000).unwrap(), faults.clone())
            .expect("the lanes were handed back by the refusal");
        assert!(out.is_open());
        assert_eq!(out.uid().as_deref(), Some("ble:FAKE"));
        let frames = until_end(&deck);
        out.close();
        assert!(!out.is_open());
        assert!(lanes.take_mixer().is_some(), "the lanes are back");

        let events = decode(&frames);
        assert!(matches!(events.first(), Some(MicEvent::Start { .. })));
        assert!(matches!(
            events.last(),
            Some(MicEvent::End {
                reason: END_RELEASE,
                ..
            })
        ));
        let samples: Vec<i16> = events
            .iter()
            .filter_map(|e| match e {
                MicEvent::Audio {
                    samples,
                    lost_frames,
                    ..
                } => {
                    assert_eq!(*lost_frames, 0);
                    Some(samples.clone())
                }
                _ => None,
            })
            .flatten()
            .collect();
        // 110 ms of a 220 Hz knock and 300 ms of tail, give or take a block.
        assert!(
            (17 * BLOCK..=24 * BLOCK).contains(&samples.len()),
            "{} samples",
            samples.len()
        );
        let knock = crate::ble::signal::tone_ratio_db(&samples, 220.0);
        assert!(knock > 10.0, "the chime is a 220 Hz knock: {knock} dB");
        let stats = out.stats();
        assert_eq!((stats.spurts, stats.dropped, stats.cut), (1, 0, 0));
        // A shared CI runner can deschedule the speaker beyond MAX_BEHIND.
        // The pacer reports that as an xrun without dropping queued audio;
        // the frame, sample and spurt assertions above still require delivery.
        // Its timing and stall recovery are checked with explicit instants in
        // the Pacer test, independently of this threaded integration test.
        let faults = faults.take();
        assert!(
            faults.iter().all(|fault| *fault == Fault::Xrun),
            "unexpected stream faults: {faults:?}"
        );
    }

    /// `voice.test`'s path with `transport = "ble"`: the core queues
    /// `AudioCmd::Chime`, the engine hands it to `deckd-tts`, and it reaches
    /// the deck's speaker characteristic as one spurt; `voice.route` names
    /// the BLE peer as the output; and a press closes the speaker.
    #[test]
    fn a_chime_through_the_engine_reaches_the_deck_over_ble() {
        let lanes = Lanes::new();
        let (ble, deck) = BleAudio::with_fake();
        let (mac, _) = FakeCapture::new();
        let (to_core, from_core) = crossbeam_channel::unbounded();
        let mut engine = Engine::new(
            lanes.clone(),
            Box::new(BleCapture::unread(ble.clone(), Box::new(mac))),
            Box::new(BleOutput::new(ble.clone())),
            to_core,
        );
        engine.set_transport(crate::config::VoiceTransport::Ble);
        // The core hears which transport this engine runs, first thing.
        assert_eq!(
            from_core.try_recv().ok(),
            Some(FromAudio::Voice(VoiceIn::Transport {
                transport: crate::config::VoiceTransport::Ble
            }))
        );
        let (tts, _) = FakeTts::new();
        let (reports, _reports) = crossbeam_channel::unbounded();
        engine.set_tts(crate::audio::tts::spawn(Arc::new(tts), lanes, reports));
        let t0 = Instant::now();
        deck.attach(244, Some(INFO));
        for event in route_events(&ble.state(), ble.transport()) {
            engine.on_route(event, t0);
        }
        assert!(engine.output_mut().is_open());
        assert_eq!(engine.route(), deck_model::Route::Ble);
        let outputs: Vec<Option<String>> = from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Route { output_uid, .. } => Some(output_uid),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.last(), Some(&Some("ble:FAKE".to_string())));

        engine.on_cmd(
            AudioCmd::Chime {
                which: "ready".into(),
            },
            t0,
        );
        let frames = until_end(&deck);
        let events = decode(&frames);
        assert!(matches!(events.first(), Some(MicEvent::Start { .. })));
        assert!(
            events
                .iter()
                .filter(|e| matches!(e, MicEvent::Audio { .. }))
                .count()
                > 10
        );
        assert!(matches!(events.last(), Some(MicEvent::End { .. })));

        // A press: the speaker closes for the capture.
        engine.on_cmd(listen(), t0);
        assert!(!engine.output_mut().is_open());
    }
}
