//! `deckd-audio` — §7.2's **pure control loop**: a `crossbeam::select!` over
//! route events, the core's commands, `deckd-tts`'s reports and a 50 ms tick.
//!
//! It never synthesises and never resamples. That is not tidiness, it is
//! §7.5.1: `Capture::Open` is the first thing a press does, and V1 measured
//! SCO bring-up at **1031 ms median** — so an open that queued behind a
//! synthesis would add the whole of that synthesis to the front of every
//! press. Here, `Listen` is a device open and three atomic stores, and
//! anything that takes time is on `deckd-tts` or inside cpal.
//!
//! It follows the `deckd-hid` precedent (handle + channels) for its shape,
//! and owns the cpal `Stream` for its whole life the way `deckd-hid-probe`
//! owns its `HidApi` — not because the `Stream` is `!Send` (V10: it is
//! `Send + Sync` in cpal 0.18.2) but because a device's lifetime belongs to
//! one thread and the route SM is here.
//!
//! **The capture callback does not touch the recogniser.** It scales by the
//! mic gain, pushes into an `rtrb` ring and updates two atomics; this loop
//! drains the ring on its tick and feeds `SttSession`. §7.1 draws it that
//! way and V18's reading of the *output* callback's thread policy is reason
//! enough to treat the input callback the same.
//!
//! **V2 lives in the level meter.** The liveness signal is the first
//! `voice.level` with a non-zero RMS, not the first callback: callbacks
//! start at ~28 ms with `AudioUnitRender` returning `noErr` and every sample
//! an exact zero for about a second. §7.4's dead-mic check keys off the
//! former, and this loop is what makes the difference visible.
use super::lanes::{Lane, Lanes, Mixer};
use super::route::{RouteAction, RouteMachine};
use super::traits::{
    AudioError, BoxFuture, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, DeviceSide, RouteEvent,
    StreamFaults, SttEngine, SttEvent, SttSession, SttStart,
};
use super::tts::{Chime, TtsCmd, TtsHandle, TtsReport};
use super::FromAudio;
use crate::config::VoiceTransport;
use crossbeam_channel::{select, Receiver, Sender};
use deck_model::{AudioCmd, SetKey, UtterancePhase, VoiceIn, VoiceInput};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// §7.2's control-loop tick.
pub const TICK: Duration = Duration::from_millis(50);

/// The capture ring: 4 s of mono f32 at the deck's 16 kHz.
///
/// It was 2 s, which is plenty for a callback that delivers in real time
/// and a loop that drains every 50 ms. The BLE capture (`audio::ble`) does
/// not deliver in real time at its open: a press's first audio can reach
/// the Mac before the press's HID line does, so the capture hands over up to
/// [`crate::audio::ble::PREROLL`] (2 s) of the stream in one go and then
/// carries on live. A ring no bigger than the pre-roll would drop the live
/// frames that land before the next drain — from the middle of the
/// sentence, since `rtrb` refuses the newest sample when full.
pub const CAPTURE_RING: usize = 4 * 16_000;

/// Default final-decode budget (defined beside the traits so non-macOS builds see it).
pub use super::traits::FINISH_BUDGET;
/// Bound backend workers and their model state during repeated quick presses.
const MAX_FINALS: usize = 4;
/// How long a release waits for the deck's END before closing the capture:
/// the frames encoded before the release are still crossing the radio, and
/// closing on the HID line alone drops the last syllable of a fast
/// sentence. END normally lands within ~100 ms; this is the ceiling.
pub const TAIL_GRACE: Duration = Duration::from_millis(300);

struct PendingFinal {
    future: BoxFuture<'static, Result<String, AudioError>>,
    started: Instant,
    deadline: Instant,
}

/// How long the lanes must have been quiet before `mic_warm` opens the
/// input.
///
/// Opening the input takes the whole Bluetooth link into SCO and §7.2
/// closes the output with it (V5: the deck returns our own TTS to its own
/// mic at ≈ +17 dB, so an open output during capture is not an option). A
/// warm-up that landed in the middle of the question announcement would
/// therefore silence the very sentence telling the operator to press.
///
/// Three seconds because that is what the plumbing can promise:
/// `TtsReport::Ended` fires when the last sample is **queued**, the lane
/// ring is two seconds deep ([`crate::audio::lanes::RING_FRAMES`]), and
/// there is no other signal for "the mixer has finished playing it". Two
/// seconds plus a margin, and a comment rather than a pretence of
/// precision.
pub const WARM_SETTLE: Duration = Duration::from_secs(3);

/// Where the output stream lives. cpal on macOS, a fake in every test.
///
/// The [`Mixer`] is **moved in**, because the render closure owns it and the
/// closure dies with the stream (§7.2). Dropping the stream runs the mixer's
/// `Drop`, which is what puts the lane consumers back.
pub trait OutputSink: Send {
    /// # Errors
    /// Whatever the platform could not do. The route SM answers an error
    /// with `RouteEvent::OutputFailed`.
    fn open(
        &mut self,
        rate: u32,
        mixer: Mixer,
        faults: Arc<StreamFaults>,
    ) -> Result<(), AudioError>;

    /// Drop the stream, and with it the render closure and the mixer.
    fn close(&mut self);

    fn is_open(&self) -> bool;

    /// What `voice.route{output_uid}` names while this sink is the route:
    /// `None` defers to the CoreAudio pair (`[voice] device_uid` +
    /// `:output`), which is what every sink but the BLE one plays to.
    fn uid(&self) -> Option<String> {
        None
    }

    /// Drive one render quantum by hand — §14's "fake render callback at
    /// 512-frame quanta".
    ///
    /// A sink with a real device answers `false`: its callback belongs to
    /// CoreAudio's real-time thread and nobody else may call it. Only
    /// [`NullOutput`] answers `true`, which is what lets a deterministic
    /// test drive the mixer through the whole control loop without a
    /// device.
    fn render_now(&mut self, out: &mut [f32]) -> bool {
        let _ = out;
        false
    }

    /// `[voice] device_uid` changed under a hot reload (B14). The default
    /// is a no-op, because a sink with no device has nothing to re-point.
    fn set_device(&mut self, uid: Option<String>) {
        let _ = uid;
    }
}

/// A sink with no device behind it: the `--no-device` build, and the one
/// every deterministic test uses.
#[derive(Debug, Default)]
pub struct NullOutput {
    mixer: Option<Mixer>,
    rate: Option<u32>,
}

impl NullOutput {
    /// Render one quantum, the way a real callback would. `None` when
    /// nothing is open.
    pub fn render(&mut self, out: &mut [f32]) -> bool {
        match self.mixer.as_mut() {
            Some(mixer) => {
                mixer.render(out);
                true
            }
            None => false,
        }
    }

    #[must_use]
    pub fn rate(&self) -> Option<u32> {
        self.rate
    }
}

impl OutputSink for NullOutput {
    fn render_now(&mut self, out: &mut [f32]) -> bool {
        self.render(out)
    }

    fn open(
        &mut self,
        rate: u32,
        mixer: Mixer,
        _faults: Arc<StreamFaults>,
    ) -> Result<(), AudioError> {
        self.mixer = Some(mixer);
        self.rate = Some(rate);
        Ok(())
    }

    fn close(&mut self) {
        // Dropping it runs §7.2's `Drop` guard and the rings go back.
        self.mixer = None;
        self.rate = None;
    }

    fn is_open(&self) -> bool {
        self.mixer.is_some()
    }
}

/// What the capture callback writes and this loop reads. Atomics and one
/// SPSC ring — the input callback is a real-time thread too.
#[derive(Debug)]
struct CaptureShared {
    /// §7.1's meter: the **loudest buffer** since the last drain, as the
    /// f32 bits of its mean square. `0` means nothing has been measured.
    ///
    /// **One atomic, not two.** It used to be a sum of squares paired with
    /// a frame count, and two atomics cannot be made to agree by ordering
    /// alone: whichever order each side uses, there is an interleaving
    /// where one of the two swaps happens between the other side's pair,
    /// and the drain then divides one buffer's energy by a different
    /// buffer's frame count. Matching the orders narrows the window;
    /// carrying the whole measurement in a single word closes it, because
    /// there is nothing left to pair.
    ///
    /// `fetch_max` rather than a sum, because the bit patterns of
    /// non-negative finite floats compare exactly as their values do, so
    /// the peak-hold is one relaxed read-modify-write with no compare-
    /// exchange loop and no lost update. It is also what a level meter is
    /// for: the operator wants to know whether the microphone heard them,
    /// and an average across a 50 ms tick hides a syllable inside forty
    /// buffers of room tone.
    ///
    /// The squares are of the **gained** sample, on purpose: §7.1 puts the
    /// meter on the capture path *after* `set mic`, and the number an
    /// operator needs when a transcript comes back wrong is what the
    /// recogniser heard, not what the room did. That is also why `set mic
    /// 0` reads silence and must — it is not evidence about the microphone,
    /// and nothing infers any from it: liveness is `nonzero` below, which
    /// is raised before the gain is anywhere near the sample.
    level: AtomicU32,
    /// Purely "did any buffer arrive since the last drain". It is no longer
    /// arithmetic against `level`, so it needs no agreement with it.
    frames: AtomicU64,
    /// V2's liveness bit: [`crate::audio::buffer_is_live`] OR-ed over every
    /// buffer since the last drain.
    ///
    /// It is **not** derived from `energy`. The meter is a lossy fixed-point
    /// average scaled by `set mic`, so a quiet-but-live microphone — or one
    /// at `set mic 0` — can read exactly zero while every buffer is full of
    /// speech; keying the dead-mic check off it would declare a working mic
    /// dead. The rule is "a sample that is not an exact digital zero", and
    /// that is what this bit carries.
    nonzero: AtomicBool,
    /// The rate the device is running at, from the callback itself.
    rate: AtomicU32,
}

/// Everything `deckd-audio` owns.
pub struct Engine {
    lanes: Arc<Lanes>,
    route: RouteMachine,
    capture: Box<dyn CaptureSource>,
    output: Box<dyn OutputSink>,
    stt: Option<Arc<dyn SttEngine>>,
    /// The offers of `caps.transcribe` companions, shared with the core.
    companion: Option<Arc<super::companion::CompanionHub>>,
    tts: Option<TtsHandle>,
    faults: Arc<StreamFaults>,
    to_core: Sender<FromAudio>,

    /// The live recognition, while a PTT session is up.
    session: Option<Box<dyn SttSession>>,
    session_events: Option<Receiver<SttEvent>>,
    /// Recognition runs in the backend; poll completions without blocking
    /// the next capture. Report them in release order for the reducer.
    finals: std::collections::VecDeque<PendingFinal>,
    /// The capture ring's consumer, drained on the tick.
    pcm: Option<rtrb::Consumer<f32>>,
    /// What the live capture opened, so a press can inherit a warm one.
    opened: Option<CaptureOpened>,
    /// The input is open because of `mic_warm`, not because of a press.
    /// The next press takes it over; anything that wants to speak, or
    /// anything that wants the mic shut, closes it.
    warm_open: bool,
    /// A Question card is pending (`AudioCmd::Warm`), which is the fact
    /// `mic_warm = "card_pending"` is a policy about.
    card_pending: bool,
    /// When the lanes last had something to play, for [`WARM_SETTLE`].
    last_audio_at: Option<Instant>,
    shared: Arc<CaptureShared>,
    /// V2: the first buffer with a sample that is not an exact zero. `None`
    /// while the mic is warming — or dead, which looks identical for the
    /// first second and is why §7.4 cannot key off callbacks.
    first_nonzero: Option<Instant>,
    /// The same fact as an edge, so the core is told **once**
    /// ([`crate::audio::Liveness`]).
    liveness: super::Liveness,
    listen_at: Option<Instant>,

    /// One `Say` in flight on `deckd-tts` (the `deckd-hid-write` shape), and
    /// the queue behind it, held **here** so a `Stop` can drop it.
    speaking: Option<String>,
    queue: std::collections::VecDeque<(AudioCmd, Option<Instant>)>,

    device_uid: Option<String>,
    fallback_input: String,
    /// §7.1/§7.5, measured: `card_pending` keeps the input open while a
    /// question card is pending, because SCO bring-up costs ~1 s of real
    /// audio (V1) and that second is the operator's first syllables.
    mic_warm: String,
    /// §7.2, measured: A2DP resume is a fixed ~2.3 s macOS timer (V1), so
    /// the reply plays into the SCO tail rather than waiting for it —
    /// never while capture is live (V5: ≈ +17 dB of loopback).
    speak_over_sco: bool,
    /// Press → first **non-zero** sample (V2, not the first callback),
    /// above which the engine reports `voice.route{slow_sco_ms}`.
    slow_sco_ms: u32,
    locale: String,
    allow_server: bool,
    /// `[voice] transport`, fixed when the engine is built
    /// ([`Engine::set_transport`]). On BLE two of the policy keys above lose
    /// the problem they solve: there is no bring-up for `mic_warm` to hide
    /// (the deck streams from the press itself) and no SCO tail for
    /// `speak_over_sco` to play into.
    transport: VoiceTransport,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("route", &self.route.route())
            .field("listening", &self.session.is_some())
            .field("speaking", &self.speaking)
            .field("queued", &self.queue.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    #[must_use]
    pub fn new(
        lanes: Arc<Lanes>,
        capture: Box<dyn CaptureSource>,
        output: Box<dyn OutputSink>,
        to_core: Sender<FromAudio>,
    ) -> Engine {
        Engine {
            lanes,
            route: RouteMachine::new("none"),
            capture,
            output,
            stt: None,
            companion: None,
            tts: None,
            faults: Arc::new(StreamFaults::new()),
            to_core,
            session: None,
            session_events: None,
            finals: std::collections::VecDeque::new(),
            pcm: None,
            opened: None,
            warm_open: false,
            card_pending: false,
            last_audio_at: None,
            shared: Arc::new(CaptureShared {
                level: AtomicU32::new(0),
                frames: AtomicU64::new(0),
                nonzero: AtomicBool::new(false),
                rate: AtomicU32::new(0),
            }),
            first_nonzero: None,
            liveness: super::Liveness::new(),
            listen_at: None,
            speaking: None,
            queue: std::collections::VecDeque::new(),
            device_uid: None,
            fallback_input: "system_default".into(),
            mic_warm: "card_pending".into(),
            speak_over_sco: true,
            slow_sco_ms: 1500,
            locale: "en-US".into(),
            allow_server: false,
            transport: VoiceTransport::Coreaudio,
        }
    }

    /// Which transport the capture and output sinks speak (`audio::spawn`
    /// sets it once, before the first event).
    ///
    /// [`VoiceTransport::Ble`] settles the route at the BLE speaker's 16 kHz
    /// instead of A2DP's 44.1 kHz, treats `mic_warm` as `"never"` and lets
    /// `speak_over_sco` go: V5's rule — nothing plays while a capture of
    /// ours is live — is untouched, and is the deck's own rule on that link
    /// too. The default, [`VoiceTransport::Coreaudio`], is the engine every
    /// test before this transport was written against.
    /// The transport this engine runs: the route machine's settled rate and
    /// state names follow it, and the core is told (the snapshot's
    /// `voice.transport`, so a window draws the right route diagram).
    pub fn set_transport(&mut self, transport: VoiceTransport) {
        self.transport = transport;
        self.route.set_transport(transport);
        self.report(VoiceIn::Transport { transport });
    }

    #[must_use]
    pub fn transport(&self) -> VoiceTransport {
        self.transport
    }

    pub fn set_stt(&mut self, engine: Arc<dyn SttEngine>) {
        self.stt = Some(engine);
    }

    /// Let presses for a `caps.transcribe` agent be offered to its owner.
    pub fn set_companion(&mut self, hub: Arc<super::companion::CompanionHub>) {
        self.companion = Some(hub);
    }

    pub fn set_tts(&mut self, tts: TtsHandle) {
        self.tts = Some(tts);
    }

    #[must_use]
    pub fn faults(&self) -> Arc<StreamFaults> {
        self.faults.clone()
    }

    #[must_use]
    pub fn lanes(&self) -> &Arc<Lanes> {
        &self.lanes
    }

    #[must_use]
    pub fn route(&self) -> deck_model::Route {
        self.route.route()
    }

    #[must_use]
    pub fn output_mut(&mut self) -> &mut dyn OutputSink {
        self.output.as_mut()
    }

    /// V2's number, for the `--hardware` row and `metrics.get.sco`: how long
    /// after the `Listen` the first **non-zero** sample arrived.
    #[must_use]
    pub fn bring_up(&self) -> Option<Duration> {
        match (self.listen_at, self.first_nonzero) {
            (Some(start), Some(first)) => Some(first.duration_since(start)),
            _ => None,
        }
    }

    fn report(&self, voice: VoiceIn) {
        let _ = self.to_core.send(FromAudio::Voice(voice));
    }

    // -- commands from the core ------------------------------------------

    pub fn on_cmd(&mut self, cmd: AudioCmd, now: Instant) {
        match cmd {
            AudioCmd::Listen {
                input,
                locale,
                contextual,
                max_s,
                from_deck,
                companion,
                ..
            } => {
                // A press streams by itself; anything else asks the deck,
                // for the session's own budget.
                let ask = (!from_deck).then(|| max_s.clamp(1, 60) as u8);
                self.listen(input, ask, &locale, contextual, companion, now);
            }
            AudioCmd::Finish { abort } => self.finish(abort, now),
            AudioCmd::Say { ttl_s, .. } => {
                // §4.9's `ttl_s?:30` is a deadline on **starting**, not on
                // the audio's life: "drop this one if it has not started by
                // then". The queue is the only place that can honour it,
                // because by the time `deckd-tts` has the item it has
                // started.
                let deadline = ttl_s.map(|s| now + Duration::from_secs(u64::from(s)));
                self.queue.push_back((cmd, deadline));
                self.pump(now);
            }
            AudioCmd::Chime { which } => {
                if let (true, Some(which)) = (self.tts.is_some(), Chime::from_name(&which)) {
                    // Speaking beats warming: a warm input holds the link
                    // in SCO with the output closed, and a chime nobody can
                    // hear is worse than a second of bring-up on the next
                    // press.
                    self.yield_warm(now);
                    if let Some(tts) = self.tts.as_ref() {
                        self.lanes.arm_ttl(Lane::Chime, now);
                        tts.send(TtsCmd::Chime { which });
                    }
                    self.last_audio_at = Some(now);
                }
            }
            AudioCmd::Stop {
                utterance,
                lane,
                all,
            } => self.stop(all, lane.as_deref(), utterance.as_deref()),
            AudioCmd::Gain { key, value } => match key {
                SetKey::Speaker => self.lanes.set_master_step(value),
                SetKey::Mic => self.lanes.set_mic_step(value),
                _ => {}
            },
            AudioCmd::Devices {
                device_uid,
                fallback_output,
                fallback_input,
            } => {
                self.device_uid.clone_from(&device_uid);
                self.output.set_device(device_uid);
                self.fallback_input = fallback_input;
                self.route.set_fallback_output(&fallback_output);
            }
            AudioCmd::Policy {
                mic_warm,
                speak_over_sco,
                slow_sco_ms,
            } => {
                self.mic_warm = mic_warm;
                self.speak_over_sco = speak_over_sco;
                self.slow_sco_ms = slow_sco_ms;
                // A hot reload that turns `mic_warm` off must close a
                // microphone it opened, and one that turns it on must not
                // wait for the next card.
                self.apply_warm(now);
                self.pump(now);
            }
            AudioCmd::Warm { pending } => {
                self.card_pending = pending;
                self.apply_warm(now);
            }
        }
    }

    /// Open the input and start delivering into a fresh ring.
    ///
    /// Shared by the press and by `mic_warm`'s pre-open, because the whole
    /// value of the pre-open is that the press **reuses this stream**: V1
    /// measured 1031 ms median (815–1246, n=16) from start-IO to the first
    /// non-zero sample, and that second is the operator's first syllables.
    /// Two code paths that each opened their own would pay it twice.
    fn open_capture(
        &mut self,
        input: VoiceInput,
        ask_deck: Option<u8>,
        now: Instant,
    ) -> Result<CaptureOpened, AudioError> {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(CAPTURE_RING);
        let shared = self.shared.clone();
        let faults = self.faults.clone();
        // The gain is read **per callback**, not captured at open time:
        // `set mic` on the deck during a live hold has to take effect on the
        // next buffer, and a relaxed load of an `AtomicU32` is as real-time
        // safe as the copy it replaces.
        let lanes = self.lanes.clone();
        let mut producer = producer;
        let sink = CaptureSink::new(faults, move |rate, samples| {
            // Real-time: no allocation, no lock, no logging.
            shared.rate.store(rate, Ordering::Relaxed);
            // V2, before the gain is anywhere near it: the liveness bit is
            // "a sample that is not an exact digital zero", so `set mic 0`
            // cannot make a live microphone look dead.
            if super::buffer_is_live(samples) {
                shared.nonzero.store(true, Ordering::Relaxed);
            }
            let mic = lanes.mic();
            let mut energy = 0f64;
            for s in samples {
                let scaled = *s * mic;
                energy += f64::from(scaled) * f64::from(scaled);
                let _ = producer.push(scaled);
            }
            // This buffer's mean square, whole, in one word. Nothing about
            // it can be divided by another buffer's frame count.
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let mean_square = (energy / samples.len().max(1) as f64) as f32;
            // A device glitching to `inf` or `NaN` must cost a meter
            // reading, never a panic and never a latched `NaN` (whose bit
            // pattern would win every `fetch_max` from here on). This runs
            // on a real-time thread; there is nothing else it may do.
            let mean_square = if mean_square.is_finite() {
                mean_square
            } else {
                0.0
            };
            shared
                .level
                .fetch_max(mean_square.to_bits(), Ordering::Relaxed);
            shared
                .frames
                .fetch_add(samples.len() as u64, Ordering::Relaxed);
        });
        let req = CaptureOpen {
            input,
            device_uid: self.device_uid.clone(),
            fallback: self.fallback_input.clone(),
            ask_deck,
        };
        self.rearm_meter();
        let opened = self.capture.open(&req, sink)?;
        self.pcm = Some(consumer);
        self.opened = Some(opened.clone());
        let _ = now;
        Ok(opened)
    }

    /// Forget what the meter and the liveness edge saw of the previous
    /// capture — or of the warm one, before the press it is handed to.
    fn rearm_meter(&mut self) {
        self.shared.level.store(0, Ordering::Relaxed);
        self.shared.frames.store(0, Ordering::Relaxed);
        self.shared.nonzero.store(false, Ordering::Relaxed);
        self.first_nonzero = None;
        self.liveness.reset();
    }

    /// §7.5.1: the **first** thing a press does. Everything in here is an
    /// open and three atomic stores — or, when `mic_warm` has already
    /// opened it, only the atomic stores.
    fn listen(
        &mut self,
        input: VoiceInput,
        ask_deck: Option<u8>,
        locale: &str,
        contextual: Vec<String>,
        companion: Option<deck_model::AgentId>,
        now: Instant,
    ) {
        self.poll_finals(Instant::now());
        if self.finals.len() >= MAX_FINALS {
            self.report(VoiceIn::Error { code: "audio_busy".into(), message: Some("Speech recognition is still finishing previous recordings".into()) });
            return;
        }
        // 1. the microphone, before anything else.
        let opened = if self.warm_open {
            // `mic_warm` already paid V1's bring-up. Everything the warm
            // capture buffered is the room, not the operator, so the ring
            // and the meter start again from here — including the liveness
            // edge, which must fire *for this session* or §7.4 never
            // leaves `Arming`.
            self.warm_open = false;
            self.rearm_meter();
            if let Some(pcm) = self.pcm.as_mut() {
                let slots = pcm.slots();
                if slots > 0 {
                    if let Ok(chunk) = pcm.read_chunk(slots) {
                        chunk.commit_all();
                    }
                }
            }
            self.opened
                .clone()
                .expect("a warm capture recorded its open")
        } else {
            match self.open_capture(input, ask_deck, now) {
                Ok(opened) => opened,
                Err(e) => {
                    // §7.8: a denied grant is silence and a code, never a
                    // panic.
                    self.report(VoiceIn::Error {
                        code: e.code().into(),
                        message: Some(e.to_string()),
                    });
                    return;
                }
            }
        };
        self.listen_at = Some(now);

        // 2. the mixer cut, then the route.
        self.lanes.set_ptt(true);
        if let Some(tts) = self.tts.as_ref() {
            tts.cut(true, None, None);
        }
        self.drive(
            RouteEvent::OurCapture {
                open: true,
                rate: opened.rate,
            },
            now,
        );
        self.report(VoiceIn::Route {
            route: self.route.route(),
            output_uid: None,
            input_uid: opened.uid.clone(),
            stuck: self.route.stuck(),
            slow_sco_ms: None,
        });

        // 3. the recogniser, which does not gate any of the above.
        self.start_stt(locale, contextual, companion, &opened);
    }

    fn start_stt(
        &mut self,
        locale: &str,
        contextual: Vec<String>,
        companion: Option<deck_model::AgentId>,
        opened: &CaptureOpened,
    ) {
        // A transcribing companion's press: the local recogniser is tee'd,
        // never replaced — it shows the partials and is the fallback.
        if let (Some(agent), Some(hub)) = (companion, self.companion.clone()) {
            let local = self.stt.clone().and_then(|engine| {
                let req = SttStart {
                    locale: if locale.is_empty() { self.locale.clone() } else { locale.to_string() },
                    contextual,
                    allow_server: self.allow_server,
                };
                engine
                    .start(&req)
                    .map_err(|e| tracing::warn!(error = %e, "local recogniser unavailable; the companion alone transcribes"))
                    .ok()
            });
            let to_core = self.to_core.clone();
            let session = super::companion::CompanionSession::new(
                local,
                agent,
                hub,
                Box::new(move |id| {
                    let _ = to_core.send(FromAudio::Transcribe { id });
                }),
            );
            self.session_events = Some(session.events());
            self.session = Some(Box::new(session));
            return;
        }
        let Some(engine) = self.stt.clone() else {
            return;
        };
        let req = SttStart {
            locale: if locale.is_empty() {
                self.locale.clone()
            } else {
                locale.to_string()
            },
            contextual,
            allow_server: self.allow_server,
        };
        match engine.start(&req) {
            Ok(session) => {
                self.session_events = Some(session.events());
                self.session = Some(session);
            }
            Err(e) => self.report(VoiceIn::Error {
                code: e.code().into(),
                message: Some(e.to_string()),
            }),
        }
        let _ = opened;
    }

    /// §7.4's `Finalizing` / `Aborting`.
    fn finish(&mut self, abort: bool, now: Instant) {
        // The tail: everything the deck sent before its END reaches the
        // recogniser, so a release right after the last word keeps it.
        if !abort && self.session.is_some() {
            let deadline = Instant::now() + TAIL_GRACE;
            while !self.capture.ended() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
                self.drain_pcm();
            }
        }
        self.capture.close();
        self.drain_pcm();
        self.pcm = None;
        self.opened = None;
        self.warm_open = false;
        self.lanes.set_ptt(false);
        // §7.2's "resume held Question/Ready, **flush stale Chatter**".
        // Chatter queued before the press is a reply to something the
        // operator has since talked over; Question and Ready are held,
        // because they are still waiting for an answer.
        //
        // Before the reopen, deliberately: the output is still closed here,
        // so the flush empties the ring on the spot rather than depending on
        // whether the next stream's first callback beats the route SM.
        self.lanes.flush(Lane::Chatter);
        self.drive(
            RouteEvent::OurCapture {
                open: false,
                rate: 0,
            },
            now,
        );
        self.session_events = None;
        let Some(session) = self.session.take() else {
            return;
        };
        if abort {
            session.cancel();
            return;
        }
        // Measure the decode budget after capture's tail is drained. A
        // partial is only a preview: failure must not turn it into an order.
        let budget = session.finish_budget();
        let started = Instant::now();
        self.finals.push_back(PendingFinal {
            future: session.finish(), started, deadline: started + budget,
        });
        self.poll_finals(Instant::now());
    }

    fn poll_finals(&mut self, now: Instant) {
        use std::task::{Context, Poll, Waker};
        let mut context = Context::from_waker(Waker::noop());
        while let Some(pending) = self.finals.front_mut() {
            let result = match pending.future.as_mut().poll(&mut context) {
                Poll::Ready(result) => result,
                Poll::Pending if now >= pending.deadline => Err(AudioError::Engine(format!(
                    "speech recognition timed out after {} ms",
                    pending.deadline.duration_since(pending.started).as_millis()
                ))),
                Poll::Pending => break,
            };
            let pending = self.finals.pop_front().unwrap();
            match result {
                Ok(text) => {
                    tracing::info!(decode_ms = pending.started.elapsed().as_millis() as u64, "stt: final decode completed");
                    self.report(VoiceIn::Final { text });
                }
                Err(error) => {
                    tracing::warn!(%error, "stt: final decode failed; nothing sent");
                    self.report(VoiceIn::FinalFailed { code: error.code().into(), message: error.to_string() });
                }
            }
        }
    }

    fn stop(&mut self, all: bool, lane: Option<&str>, utterance: Option<&str>) {
        let lane = lane.and_then(Lane::from_name);
        if let Some(tts) = self.tts.as_ref() {
            tts.cut(all, lane, utterance);
        }
        for l in Lane::ALL {
            if all || lane == Some(l) {
                // `Lanes::flush` empties the ring itself when no stream
                // holds the consumer, so this is the same command whether
                // or not the output happens to be open.
                self.lanes.flush(l);
            }
        }
        // Whatever has not started yet is dropped with it (§4.9's
        // `voice.stop{all}` means all, not "all that began").
        self.queue.retain(|(cmd, _)| match cmd {
            AudioCmd::Say {
                utterance: queued,
                lane: queued_lane,
                ..
            } => {
                !(all
                    || lane.is_some_and(|l| l.name() == queued_lane)
                    || utterance.is_some_and(|u| u == queued))
            }
            _ => true,
        });
    }

    // -- the three policy keys (§7.1/§7.2/§7.5) ---------------------------

    /// `[voice] speak_over_sco`, with V5's rule underneath it.
    ///
    /// **V5 first, and it is not negotiable.** The deck's speaker returns
    /// our own TTS to the deck's own mic at ≈ +17 dB with no AEC, so
    /// nothing may start while a capture of ours is live — not a chime, not
    /// a reply, and not because `speak_over_sco = true`. That key is about
    /// the SCO *tail*, after the capture has closed and while the link
    /// spends its fixed 2307 ms getting back to A2DP (V1); it is not a
    /// licence to talk over the microphone.
    ///
    /// With the key off, an announcement waits for A2DP rather than playing
    /// into the tail at 16 kHz. `ScoStuck` is the exception in both
    /// directions: A2DP is not coming back, §7.2's own answer is "play at
    /// 16 k", and waiting for ever is not an answer at all.
    fn may_speak(&self) -> bool {
        if self.session.is_some() || self.lanes.ptt() {
            return false;
        }
        // On BLE there is no tail to wait out: the release reopens the
        // speaker at its only rate in the same turn, so the key has nothing
        // to decide.
        if self.speak_over_sco || self.route.stuck() || self.transport == VoiceTransport::Ble {
            return true;
        }
        !matches!(
            self.route.route(),
            deck_model::Route::ScoOurs
                | deck_model::Route::ScoExternal
                | deck_model::Route::Reopening
        )
    }

    /// §7.1's `[voice] mic_warm`, which until now was declared, written and
    /// read by nothing at all.
    ///
    /// `"card_pending"` is the measured default (V1) and it exists to buy
    /// back the 1031 ms median bring-up — the operator's first syllables —
    /// at the one moment they are about to press: a Question card is on the
    /// deck and the answer is a button away. `"always"` needs no signal;
    /// `"never"` ignores the one it gets.
    fn apply_warm(&mut self, now: Instant) {
        if self.lanes.ptt() || self.session.is_some() {
            return; // a live press owns the microphone
        }
        // On BLE the key is moot and treated as `"never"`: the deck powers
        // its microphone and starts the stream on the press itself, and the
        // capture keeps a pre-roll from the stream's START, so a press
        // loses nothing to bring-up. An input held open "warm" would only
        // hold the speaker closed.
        let want = self.transport != VoiceTransport::Ble
            && match self.mic_warm.as_str() {
                "always" => true,
                "card_pending" => self.card_pending,
                _ => false,
            };
        if !want {
            self.yield_warm(now);
            return;
        }
        if self.warm_open || self.capture.is_open() {
            return;
        }
        if !self.lanes_settled(now) {
            return; // the tick tries again
        }
        // Warming is for a press (CoreAudio only: BLE never warms), and a
        // press streams by itself.
        let Ok(opened) = self.open_capture(VoiceInput::Deck, None, now) else {
            // §7.8: silence, not an error. A warm-up nobody asked for is
            // not worth a `voice.state{error}` on the operator's screen;
            // the press that follows will report its own failure.
            self.pcm = None;
            return;
        };
        self.warm_open = true;
        self.drive(
            RouteEvent::OurCapture {
                open: true,
                rate: opened.rate,
            },
            now,
        );
    }

    /// Give the microphone back, if we are only holding it to keep it warm.
    ///
    /// Speaking beats warming, always: a warm input holds the link in SCO
    /// with the output closed, so an announcement made while it is open is
    /// an announcement nobody hears. A second of bring-up on the next press
    /// is a smaller loss than a sentence.
    fn yield_warm(&mut self, now: Instant) {
        if !self.warm_open {
            return;
        }
        self.warm_open = false;
        self.capture.close();
        self.pcm = None;
        self.opened = None;
        self.rearm_meter();
        self.drive(
            RouteEvent::OurCapture {
                open: false,
                rate: 0,
            },
            now,
        );
    }

    /// Whether the lanes have been quiet long enough to take the link into
    /// SCO behind the operator's back ([`WARM_SETTLE`]).
    fn lanes_settled(&self, now: Instant) -> bool {
        if self.speaking.is_some() || !self.queue.is_empty() {
            return false;
        }
        if Lane::ALL.iter().any(|l| self.lanes.active(*l)) {
            return false;
        }
        self.last_audio_at
            .is_none_or(|at| now.saturating_duration_since(at) >= WARM_SETTLE)
    }

    /// One `Say` in flight (`deckd-hid-write`'s shape).
    fn pump(&mut self, now: Instant) {
        if self.speaking.is_some() {
            return;
        }
        // Anything that missed its `ttl_s` never starts, and says so: a
        // reader waiting on `voice.utterance` must not wait for ever. The
        // sweep is over the **whole** queue, not just its head: an item
        // behind a two-minute announcement is just as stale as one in
        // front of it, and §4.9's deadline is about the clock rather than
        // about position.
        let expired: Vec<String> = self
            .queue
            .iter()
            .filter(|(_, deadline)| deadline.is_some_and(|at| now >= at))
            .map(|(cmd, _)| match cmd {
                AudioCmd::Say { utterance, .. } => utterance.clone(),
                _ => String::new(),
            })
            .collect();
        if !expired.is_empty() {
            self.queue
                .retain(|(_, deadline)| deadline.is_none_or(|at| now < at));
            for utterance in expired {
                self.report(VoiceIn::Utterance {
                    utterance,
                    phase: UtterancePhase::Ended,
                });
            }
        }
        if self.queue.is_empty() {
            // Nothing to say, so nothing to take the link back for: a warm
            // microphone stays warm.
            return;
        }
        if !self.may_speak() {
            // Held, not dropped: the 50 ms tick tries again, and `ttl_s`
            // is what decides when waiting has gone on too long.
            return;
        }
        // Speaking beats warming (see `yield_warm`).
        self.yield_warm(now);
        let Some(tts) = self.tts.as_ref() else {
            // No engine: drop it rather than queue for ever, and say so.
            if let Some((AudioCmd::Say { utterance, .. }, _)) = self.queue.pop_front() {
                let _ = self.to_core.send(FromAudio::Voice(VoiceIn::Utterance {
                    utterance,
                    phase: UtterancePhase::Ended,
                }));
            }
            return;
        };
        let Some((
            AudioCmd::Say {
                utterance,
                text,
                lane,
                voice,
                ..
            },
            _,
        )) = self.queue.pop_front()
        else {
            return;
        };
        let lane_id = Lane::from_name(&lane).unwrap_or(Lane::Chatter);
        self.speaking = Some(utterance.clone());
        self.last_audio_at = Some(now);
        tts.send(TtsCmd::Say {
            utterance,
            lane: lane_id,
            text,
            voice,
        });
    }

    // -- reports from deckd-tts ------------------------------------------

    pub fn on_tts(&mut self, report: TtsReport, now: Instant) {
        match report {
            TtsReport::Started { utterance, lane } => {
                self.lanes.arm_ttl(lane, now);
                self.last_audio_at = Some(now);
                self.report(VoiceIn::Utterance {
                    utterance,
                    phase: UtterancePhase::Started,
                });
            }
            TtsReport::Ended { utterance, .. } => {
                if self.speaking.as_deref() == Some(utterance.as_str()) {
                    self.speaking = None;
                }
                // `Ended` is "the last sample is **queued**", not "played":
                // up to `RING_FRAMES` of it is still ahead of the mixer.
                self.last_audio_at = Some(now);
                self.report(VoiceIn::Utterance {
                    utterance,
                    phase: UtterancePhase::Ended,
                });
                self.pump(now);
            }
            TtsReport::Failed { utterance, code } => {
                if self.speaking.as_deref() == Some(utterance.as_str()) {
                    self.speaking = None;
                }
                self.report(VoiceIn::Error {
                    code,
                    message: Some(format!("utterance {utterance}")),
                });
                self.pump(now);
            }
        }
    }

    // -- the recogniser ---------------------------------------------------

    pub fn on_stt(&mut self, event: SttEvent) {
        match event {
            SttEvent::Partial(text) => {
                self.report(VoiceIn::Partial { text });
            }
            // The release still owns completion; finish() returns this final.
            SttEvent::Final(_) => {}
            SttEvent::Error { code, message } => self.report(VoiceIn::Error {
                code,
                message: Some(message),
            }),
        }
    }

    // -- the route --------------------------------------------------------

    pub fn on_route(&mut self, event: RouteEvent, now: Instant) {
        self.drive(event, now);
    }

    fn drive(&mut self, event: RouteEvent, now: Instant) {
        for action in self.route.on(event, now) {
            match action {
                RouteAction::OpenOutput { rate } => {
                    self.output.close();
                    // V3: the mixer is told the rate the **stream** is open
                    // at, and bridges it to the lanes' 44 100. Nothing else
                    // on this path resamples.
                    match self.lanes.take_mixer_at(rate) {
                        Some(mixer) => {
                            let faults = self.faults.clone();
                            let opened = self.output.open(rate, mixer, faults);
                            let next = if opened.is_ok() {
                                RouteEvent::OutputOpened { rate }
                            } else {
                                RouteEvent::OutputFailed
                            };
                            // Folded straight back in: the SM decides what
                            // an open means, and nothing else may.
                            self.drive(next, now);
                        }
                        None => self.drive(RouteEvent::OutputFailed, now),
                    }
                }
                RouteAction::CloseOutput => self.output.close(),
                RouteAction::ReadNominal => {
                    // The platform layer answers with a `NominalRate`; with
                    // no platform behind it there is nothing to re-read.
                }
                RouteAction::Report { route, stuck } => {
                    // An output open folds its result back in above. That
                    // result may already have settled the route and reported
                    // it; the enclosing event's Reopening report is then
                    // stale and must not overwrite it in the core and GUI.
                    if (route, stuck) != (self.route.route(), self.route.stuck()) {
                        continue;
                    }
                    self.report(VoiceIn::Route {
                        route,
                        output_uid: self
                            .output
                            .uid()
                            .or_else(|| self.device_uid.as_ref().map(|u| format!("{u}:output"))),
                        input_uid: None,
                        stuck,
                        slow_sco_ms: None,
                    });
                }
            }
        }
    }

    // -- the tick ----------------------------------------------------------

    pub fn tick(&mut self, now: Instant) {
        self.drain_pcm();
        self.poll_finals(now);
        self.pump(now);
        self.apply_warm(now);
        // The expiry *is* the flush (`Lanes::expire` raises it), and the
        // flush empties the ring when nothing is rendering.
        let _expired = self.lanes.expire(now);
        for fault in self.faults.take() {
            self.drive(RouteEvent::Fault(fault), now);
        }
        self.drive(RouteEvent::Tick, now);
    }

    /// Move the capture ring into the recogniser, and emit §7.1's meter.
    fn drain_pcm(&mut self) {
        let frames = self.shared.frames.swap(0, Ordering::Relaxed);
        let level = f32::from_bits(self.shared.level.swap(0, Ordering::Relaxed));
        let nonzero = self.shared.nonzero.swap(false, Ordering::Relaxed);
        let rate = self.shared.rate.load(Ordering::Relaxed);
        // B1: **this** is where V2's liveness leaves the engine. Before it
        // did not: `first_nonzero` was computed, exposed on `bring_up()` for
        // the `--hardware` row, and never sent — so `VoiceIn::Live` had no
        // producer outside the tests, §7.4's `Arming` could only be
        // satisfied by a non-empty partial, and a press with a beat of
        // hesitation aborted a working microphone under the operator's
        // finger after `ARMING_LIVENESS`.
        let live = self.liveness.observe_nonzero(nonzero);
        if let (true, Some(listen_at)) = (live, self.listen_at) {
            let at = Instant::now();
            self.first_nonzero = Some(at);
            self.report(VoiceIn::Live);
            // §7.5.4's `voice.route{slow_sco_ms}`, keyed off the first
            // **non-zero** sample (V2). Above the operator's threshold this
            // press cost them syllables, and the GUI is the only place that
            // can say so — `mic_warm` is the setting that fixes it.
            let waited = at.saturating_duration_since(listen_at);
            let waited = u32::try_from(waited.as_millis()).unwrap_or(u32::MAX);
            if waited > self.slow_sco_ms {
                self.report(VoiceIn::Route {
                    route: self.route.route(),
                    output_uid: None,
                    input_uid: self.opened.as_ref().and_then(|o| o.uid.clone()),
                    stuck: self.route.stuck(),
                    slow_sco_ms: Some(waited),
                });
            }
        }
        let Some(pcm) = self.pcm.as_mut() else {
            return;
        };
        let slots = pcm.slots();
        if slots > 0 {
            let mut block = Vec::with_capacity(slots);
            for _ in 0..slots {
                match pcm.pop() {
                    Ok(s) => block.push(s),
                    Err(_) => break,
                }
            }
            if let Some(session) = self.session.as_mut() {
                session.feed(&block, rate);
            }
        }
        if frames > 0 {
            self.report(VoiceIn::Level { rms: level.sqrt() });
        }
    }

    /// The deck appeared or went away (the platform's enumeration).
    pub fn on_device(&mut self, side: DeviceSide, present: bool, now: Instant) {
        self.drive(RouteEvent::DevicePresent { side, present }, now);
    }
}

/// The `deckd-audio` thread: `deckd-hid`'s shape, one `select!`.
pub fn run(
    mut engine: Engine,
    cmds: &Receiver<AudioCmd>,
    routes: &Receiver<RouteEvent>,
    tts: &Receiver<TtsReport>,
) {
    let ticker = crossbeam_channel::tick(TICK);
    loop {
        // The recogniser's channel is per session, so it cannot be a fixed
        // `select!` arm; it is drained first instead, which also gives a
        // partial priority over a tick.
        let events: Vec<SttEvent> = engine
            .session_events
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            engine.on_stt(event);
        }
        select! {
            recv(cmds) -> cmd => match cmd {
                Ok(cmd) => engine.on_cmd(cmd, Instant::now()),
                Err(_) => break, // the core dropped the handle
            },
            recv(routes) -> event => if let Ok(event) = event {
                engine.on_route(event, Instant::now());
            },
            recv(tts) -> report => if let Ok(report) = report {
                engine.on_tts(report, Instant::now());
            },
            recv(ticker) -> _ => engine.tick(Instant::now()),
        }
    }
    engine.output.close();
    engine.capture.close();
    if let Some(tts) = engine.tts.take() {
        tts.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::fakes::{FakeCapture, FakeStt, FakeTts};
    use deck_model::PttCtx;

    struct Rig {
        engine: Engine,
        capture: crate::audio::fakes::FakeCaptureHandle,
        stt: crate::audio::fakes::FakeSttHandle,
        from_core: Receiver<FromAudio>,
        reports: Receiver<TtsReport>,
    }

    #[test]
    fn a_companion_press_is_offered_and_its_text_is_the_final() {
        let hub = crate::audio::companion::CompanionHub::new();
        let mut rig = rig();
        rig.engine.set_companion(hub.clone());
        let t0 = Instant::now();
        let AudioCmd::Listen { agent, ctx, card, max_s, from_deck, input, locale, contextual, .. } = listen() else {
            unreachable!()
        };
        let conol = deck_model::AgentId("ag_conol".into());
        rig.engine.on_cmd(
            AudioCmd::Listen { agent, ctx, card, max_s, from_deck, input, locale, contextual, companion: Some(conol.clone()) },
            t0,
        );
        for i in 0..2 {
            rig.capture.deliver(&[0.3; 8_000]);
            rig.engine.tick(t0 + Duration::from_millis(50 * (i + 1)));
        }
        let fed: usize = rig.stt.fed().iter().map(|(_, n)| n).sum();
        assert_eq!(fed, 16_000, "the local recogniser still hears the press");
        rig.stt.finish_with(Ok("apple's guess".into()));
        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, t0);
        let reported: Vec<FromAudio> = rig.from_core.try_iter().collect();
        let id = reported
            .iter()
            .find_map(|m| match m { FromAudio::Transcribe { id } => Some(*id), _ => None })
            .expect("the release offers the recording");
        assert!(!reported.iter().any(|m| matches!(m, FromAudio::Voice(VoiceIn::Final { .. }))), "the local final waits");
        let info = hub.info(id).unwrap();
        assert_eq!((info.agent, info.ms), (conol, 1000));
        hub.bind(id, deck_model::SessionId(3));
        hub.resolve(id, deck_model::SessionId(3), Ok("Conol's transcript".into())).unwrap();
        rig.engine.tick(t0 + Duration::from_millis(200));
        let finals: Vec<VoiceIn> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter(|v| matches!(v, VoiceIn::Final { .. } | VoiceIn::FinalFailed { .. }))
            .collect();
        assert_eq!(finals, vec![VoiceIn::Final { text: "Conol's transcript".into() }]);
        assert!(hub.is_empty());
    }

    fn rig() -> Rig {
        let lanes = Lanes::new();
        let (capture, capture_handle) = FakeCapture::new();
        let (to_core, from_core) = crossbeam_channel::unbounded();
        let mut engine = Engine::new(
            lanes.clone(),
            Box::new(capture),
            Box::new(NullOutput::default()),
            to_core,
        );
        let (stt, stt_handle) = FakeStt::new();
        engine.set_stt(Arc::new(stt));
        let (tts_engine, _tts_handle) = FakeTts::new();
        let (tx, reports) = crossbeam_channel::unbounded();
        engine.set_tts(crate::audio::tts::spawn(Arc::new(tts_engine), lanes, tx));
        Rig {
            engine,
            capture: capture_handle,
            stt: stt_handle,
            from_core,
            reports,
        }
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
            contextual: vec!["cohdl-api".into()],
            companion: None,
        }
    }

    /// §7.5.1 and V2 in one press.
    ///
    /// The open is the first thing that happens; the first second of
    /// callbacks is digital silence and the meter says so; and the liveness
    /// signal is the first **non-zero** RMS, ~1031 ms in on real hardware.
    #[test]
    fn a_press_opens_the_microphone_first_and_the_meter_is_the_liveness_signal() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(
            AudioCmd::Devices {
                device_uid: Some("C2-2D-81-AB-88-81".into()),
                fallback_output: "none".into(),
                fallback_input: "system_default".into(),
            },
            t0,
        );
        rig.engine.on_cmd(listen(), t0);
        assert_eq!(rig.capture.opens().len(), 1, "the mic, first");
        assert_eq!(
            rig.capture.opens()[0].device_uid.as_deref(),
            Some("C2-2D-81-AB-88-81"),
            "by UID, never the system default (§7)"
        );
        assert!(rig.engine.lanes().ptt(), "and everything is cut");
        assert_eq!(
            rig.stt.starts()[0].contextual,
            vec!["cohdl-api".to_string()],
            "B3's contextual strings reach the recogniser"
        );

        // V2's first second: real callbacks, `noErr`, every sample zero.
        for _ in 0..40 {
            rig.capture.deliver_silence(160);
        }
        rig.engine.tick(t0 + Duration::from_millis(50));
        let levels: Vec<f32> = rig
            .from_core
            .try_iter()
            .filter_map(|m| match m {
                FromAudio::Voice(VoiceIn::Level { rms }) => Some(rms),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![0.0], "a live mic reading exactly zero");
        assert!(
            rig.engine.bring_up().is_none(),
            "V2: buffers arrived and the mic is still not known to be alive"
        );
        assert_eq!(rig.stt.fed()[0].0, 16_000, "fed at the device's own rate");

        // …and then real audio.
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0 + Duration::from_millis(1100));
        let seen: Vec<f32> = rig
            .from_core
            .try_iter()
            .filter_map(|m| match m {
                FromAudio::Voice(VoiceIn::Level { rms }) => Some(rms),
                _ => None,
            })
            .collect();
        let level = *seen.last().expect("a level");
        assert!((level - 0.3).abs() < 1e-3, "rms {level}");
        assert!(
            rig.engine.bring_up().is_some(),
            "V2: *now* the mic is known to be alive"
        );
    }

    /// B1: the engine is the **producer** of `VoiceIn::Live`.
    ///
    /// Before this, `first_nonzero` was computed and never sent. The only
    /// things that ever constructed `VoiceIn::Live` were tests, so in the
    /// shipped daemon §7.4's `Arming` could be satisfied by exactly one
    /// thing: a non-empty *partial*. That means the operator had to get a
    /// recognised word out of SFSpeech within `ARMING_LIVENESS` (2500 ms)
    /// of the press — on top of a bring-up measured at 1031 ms median and
    /// 1246 ms worst. A press with a beat of hesitation aborted a working
    /// microphone under the operator's finger and threw the sentence away.
    ///
    /// The edge is keyed on a **sample that is not an exact digital zero**,
    /// not on the meter: the meter is scaled by `set mic` and quantised, so
    /// `set mic 0` reads silence on a microphone that is working perfectly.
    #[test]
    fn the_engine_tells_the_core_the_mic_is_live_on_the_first_non_zero_sample() {
        let live = |rig: &Rig| -> usize {
            rig.from_core
                .try_iter()
                .filter(|m| matches!(m, FromAudio::Voice(VoiceIn::Live)))
                .count()
        };
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);

        // V2's first second: buffers arrive, `AudioUnitRender` says `noErr`,
        // every sample is an exact zero. Nothing may be claimed from that.
        for _ in 0..40 {
            rig.capture.deliver_silence(160);
        }
        rig.engine.tick(t0 + Duration::from_millis(50));
        assert_eq!(live(&rig), 0, "a warming mic and a dead one look alike");

        // …and then real audio. Once, on the edge.
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0 + Duration::from_millis(1100));
        assert_eq!(live(&rig), 1, "V2: the first non-zero sample is the edge");
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0 + Duration::from_millis(1150));
        assert_eq!(live(&rig), 0, "and the edge happens once, not per buffer");

        // The rule is the sample, not the meter. At `set mic 0` every
        // sample that reaches the ring — and the meter — is an exact zero,
        // and the microphone is still alive.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(
            AudioCmd::Gain {
                key: SetKey::Mic,
                value: 0,
            },
            t0,
        );
        rig.engine.on_cmd(listen(), t0);
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0 + Duration::from_millis(50));
        let levels: Vec<f32> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Level { rms } => Some(rms),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![0.0], "muted to the recogniser, as asked");
        assert!(
            rig.engine.bring_up().is_some(),
            "and still a working microphone: `set mic 0` is a gain, not a \
             verdict on the hardware"
        );
    }

    /// §7.1's meter, at the levels people actually speak at.
    ///
    /// The accumulator truncates, so its fixed-point scale is its noise
    /// floor. At 2^20 that floor was **−60.2 dBFS** and everything below it
    /// contributed exactly zero: −62 dBFS read digital silence, −55 dBFS
    /// read low, and `set mic 1` on an ordinary −20 dBFS voice read −6 dB
    /// low because the product fell under the floor. A `voice.level` that
    /// is wrong about quiet audio is wrong about the only audio anyone
    /// needs a meter for.
    #[test]
    fn the_meter_measures_a_whisper_instead_of_rounding_it_to_silence() {
        fn amplitude(dbfs: f32) -> f32 {
            10f32.powf(dbfs / 20.0)
        }

        // A constant level is its own RMS, so the expected value is the
        // sample times the gain and nothing has to be reasoned about.
        let measure = |mic: u16, dbfs: f32| -> (f32, f32) {
            let mut rig = rig();
            let t0 = Instant::now();
            rig.engine.on_cmd(
                AudioCmd::Gain {
                    key: SetKey::Mic,
                    value: mic,
                },
                t0,
            );
            rig.engine.on_cmd(listen(), t0);
            rig.capture.deliver(&vec![amplitude(dbfs); 512]);
            rig.engine.tick(t0 + Duration::from_millis(50));
            let rms = rig
                .from_core
                .try_iter()
                .filter_map(FromAudio::voice)
                .filter_map(|v| match v {
                    VoiceIn::Level { rms } => Some(rms),
                    _ => None,
                })
                .last()
                .expect("a level");
            (
                rms,
                amplitude(dbfs) * crate::audio::lanes::perceptual_gain(mic),
            )
        };

        for (mic, dbfs) in [
            (10, -20.0),  // a control, above the old floor
            (10, -55.0),  // measured 4.9 % low in amplitude
            (10, -62.0),  // measured as digital silence
            (1, -20.0),   // −60 dBFS after the gain: measured −6 dB low
            (10, -100.0), // a room at night, and still a number
        ] {
            let (got, want) = measure(mic, dbfs);
            assert!(
                (got - want).abs() <= want * 0.01,
                "set mic {mic} at {dbfs} dBFS: the meter says {got:e}, the \
                 recogniser is hearing {want:e}"
            );
        }

        // …and `set mic 0` still reads silence, because that is the truth
        // about what the recogniser is being fed. It is not evidence about
        // the microphone, and B1's liveness does not come from here.
        let (got, _) = measure(0, -20.0);
        assert_eq!(got, 0.0, "a mute is a mute");
    }

    fn policy(mic_warm: &str, speak_over_sco: bool, slow_sco_ms: u32) -> AudioCmd {
        AudioCmd::Policy {
            mic_warm: mic_warm.into(),
            speak_over_sco,
            slow_sco_ms,
        }
    }

    fn say(n: u32) -> AudioCmd {
        AudioCmd::Say {
            utterance: format!("ut_{n}"),
            text: "bot is ready".into(),
            agent: None,
            lane: "ready".into(),
            voice: None,
            interrupt: false,
            ttl_s: None,
        }
    }

    /// V5 and `[voice] speak_over_sco`, which were declared, plumbed and
    /// read by nothing.
    ///
    /// V5 measured the deck's speaker→mic loopback at ≈ +17 dB with no AEC,
    /// so anything that plays while a capture of ours is live is fed
    /// straight back into the recogniser. Nothing enforced that: `pump`
    /// started whatever was queued. And `speak_over_sco = false` — the
    /// operator asking not to hear a 16 kHz reply into the SCO tail — did
    /// nothing at all.
    #[test]
    fn nothing_plays_over_our_own_capture_and_speak_over_sco_can_wait_for_a2dp() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);
        rig.engine.on_cmd(say(1), t0);
        assert!(
            rig.engine.speaking.is_none(),
            "V5: the deck returns our own TTS to its own mic at +17 dB"
        );
        assert_eq!(rig.engine.queue.len(), 1, "held, not dropped");
        rig.engine.tick(t0 + Duration::from_millis(50));
        assert!(rig.engine.speaking.is_none(), "and it stays held");
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
        rig.engine.tick(t0 + Duration::from_millis(100));
        assert_eq!(
            rig.engine.speaking.as_deref(),
            Some("ut_1"),
            "…until the microphone is shut"
        );

        // `speak_over_sco = false`: wait out the 2307 ms A2DP resume rather
        // than play the reply at the tail rate.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(policy("never", false, 1500), t0);
        rig.engine.on_device(DeviceSide::Output, true, t0);
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 44_100 }, t0);
        rig.engine.on_cmd(listen(), t0);
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 16_000 }, t0);
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
        assert_eq!(rig.engine.route(), deck_model::Route::Reopening);
        rig.engine.on_cmd(say(2), t0);
        assert!(
            rig.engine.speaking.is_none(),
            "speak_over_sco = false waits for A2DP"
        );
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 44_100 }, t0);
        rig.engine.tick(t0 + Duration::from_millis(50));
        assert_eq!(rig.engine.route(), deck_model::Route::A2dp);
        assert_eq!(
            rig.engine.speaking.as_deref(),
            Some("ut_2"),
            "…and speaks when it is back"
        );
    }

    /// A synchronous output open reports its settled route to the core,
    /// including a reopen after PTT. The enclosing event must not emit its
    /// stale Reopening report after the open result's Ble/A2dp report.
    #[test]
    fn successful_output_opens_report_the_settled_route_after_attach_and_ptt() {
        use deck_model::Route;

        fn reported_routes(from_core: &Receiver<FromAudio>) -> Vec<(Route, bool)> {
            from_core
                .try_iter()
                .filter_map(FromAudio::voice)
                .filter_map(|event| match event {
                    VoiceIn::Route { route, stuck, .. } => Some((route, stuck)),
                    _ => None,
                })
                .collect()
        }

        for (transport, rate, settled, ours) in [
            (VoiceTransport::Ble, 16_000, Route::Ble, Route::BleOurs),
            (
                VoiceTransport::Coreaudio,
                44_100,
                Route::A2dp,
                Route::ScoOurs,
            ),
        ] {
            let mut rig = rig();
            rig.engine.set_transport(transport);
            let now = Instant::now();
            rig.engine.on_route(RouteEvent::NominalRate { rate }, now);
            rig.engine.on_device(DeviceSide::Output, true, now);
            assert!(rig.engine.output_mut().is_open());
            assert_eq!(rig.engine.route(), settled);
            assert_eq!(reported_routes(&rig.from_core), vec![(settled, false)]);

            rig.engine.on_cmd(listen(), now);
            // The transition and then the capture's input UID both report
            // the active capture route.
            assert_eq!(reported_routes(&rig.from_core), vec![(ours, false); 2]);
            rig.engine.on_cmd(AudioCmd::Finish { abort: true }, now);
            assert!(rig.engine.output_mut().is_open());
            assert_eq!(rig.engine.route(), settled);
            assert_eq!(reported_routes(&rig.from_core), vec![(settled, false)]);
        }
    }

    /// `[voice] transport = "ble"` in the control loop.
    ///
    /// The route settles at the BLE speaker's 16 kHz instead of A2DP's
    /// 44.1 kHz; `mic_warm` is moot, because the deck streams from the press
    /// itself and an input held open would only hold the speaker shut; and
    /// `speak_over_sco = false` has no tail to wait out. What does not move
    /// is V5: nothing plays while a capture of ours is live.
    #[test]
    fn on_ble_the_route_settles_at_16_khz_mic_warm_is_moot_and_nothing_waits_for_a2dp() {
        let mut rig = rig();
        rig.engine.set_transport(VoiceTransport::Ble);
        assert_eq!(rig.engine.transport(), VoiceTransport::Ble);
        let t0 = Instant::now();
        // Both keys at their most demanding.
        rig.engine.on_cmd(policy("always", false, 1500), t0);
        rig.engine.on_cmd(AudioCmd::Warm { pending: true }, t0);
        rig.engine.tick(t0 + WARM_SETTLE);
        assert!(
            rig.capture.opens().is_empty(),
            "mic_warm is treated as \"never\" on BLE"
        );

        // The link comes up: 16 kHz is where it settles, at once.
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 16_000 }, t0);
        rig.engine.on_device(DeviceSide::Output, true, t0);
        assert!(rig.engine.output_mut().is_open());
        assert_eq!(rig.engine.route(), deck_model::Route::Ble);

        // A press closes the speaker, and a reply waits for the release.
        rig.engine.on_cmd(listen(), t0);
        assert!(!rig.engine.output_mut().is_open());
        assert_eq!(rig.engine.route(), deck_model::Route::BleOurs);
        rig.engine.on_cmd(say(1), t0);
        assert!(rig.engine.speaking.is_none(), "V5 holds on every transport");

        // The release reopens it in the same turn, and the reply goes
        // although `speak_over_sco` is off: there is no A2DP to wait for.
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
        assert!(rig.engine.output_mut().is_open());
        assert_eq!(rig.engine.route(), deck_model::Route::Ble);
        rig.engine.tick(t0 + Duration::from_millis(50));
        assert_eq!(rig.engine.speaking.as_deref(), Some("ut_1"));
        rig.engine.on_cmd(AudioCmd::Warm { pending: true }, t0);
        rig.engine
            .tick(t0 + WARM_SETTLE * 4 + Duration::from_millis(50));
        assert_eq!(
            rig.capture.opens().len(),
            1,
            "the press was the only open: a pending card after it opens nothing"
        );
    }

    /// §7.1's `[voice] mic_warm`, whose entire purpose is to spend V1's
    /// measured 1031 ms of bring-up **before** the press instead of during
    /// it — and which, until now, nothing read.
    #[test]
    fn mic_warm_opens_before_the_press_and_the_press_inherits_the_warm_link() {
        let mut rig = rig();
        let t0 = Instant::now();
        // An announcement is in flight, so the warm-up waits: opening the
        // input takes the link into SCO and closes the output, and a
        // warm-up that silenced the question would defeat its own purpose.
        rig.engine.last_audio_at = Some(t0);
        rig.engine.on_cmd(AudioCmd::Warm { pending: true }, t0);
        assert!(
            rig.capture.opens().is_empty(),
            "not in the middle of the announcement that says to press"
        );
        rig.engine.tick(t0 + WARM_SETTLE);
        assert_eq!(
            rig.capture.opens().len(),
            1,
            "…and then, while the operator is reading the card"
        );

        // The link comes up on its own clock, before anybody presses.
        let warm = t0 + WARM_SETTLE;
        for _ in 0..40 {
            rig.capture.deliver_silence(160);
        }
        rig.capture.deliver(&[0.2; 160]);
        rig.engine.tick(warm + Duration::from_secs(1));
        let _ = rig.from_core.try_iter().count();

        // The press takes it over: no second open, and no second wait.
        let press = warm + Duration::from_secs(2);
        rig.engine.on_cmd(listen(), press);
        assert_eq!(
            rig.capture.opens().len(),
            1,
            "V1's second was spent before the press, not during it"
        );
        rig.capture.deliver(&[0.2; 160]);
        rig.engine.tick(press + Duration::from_millis(50));
        let bring_up = rig.engine.bring_up().expect("a live microphone");
        assert!(
            bring_up < Duration::from_millis(100),
            "the press did not pay the bring-up again: {bring_up:?}"
        );
        assert!(
            rig.from_core
                .try_iter()
                .any(|m| matches!(m, FromAudio::Voice(VoiceIn::Live))),
            "and §7.4 leaves Arming for *this* session"
        );

        // The card is answered: the microphone goes away again.
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, press);
        assert_eq!(rig.capture.closes(), 1);

        // Speaking beats warming — a warm input holds the link in SCO with
        // the output closed, so an announcement made over it is one nobody
        // hears.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(AudioCmd::Warm { pending: true }, t0);
        assert!(rig.capture.is_open());
        rig.engine.on_cmd(say(9), t0);
        assert!(
            !rig.capture.is_open(),
            "the announcement gets the link back"
        );

        // …and `"never"` means never.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(policy("never", true, 1500), t0);
        rig.engine.on_cmd(AudioCmd::Warm { pending: true }, t0);
        assert!(rig.capture.opens().is_empty());
        // while `"always"` needs no card at all.
        rig.engine.on_cmd(policy("always", true, 1500), t0);
        assert_eq!(rig.capture.opens().len(), 1);
    }

    /// §7.5.4's `voice.route{slow_sco_ms}` — the third write-only key.
    ///
    /// It is an exception report, keyed off the first **non-zero** sample
    /// (V2) and not the first callback, because the callbacks start at
    /// ~28 ms on a microphone that is not there at all.
    #[test]
    fn a_slow_bring_up_is_reported_and_a_fast_one_is_not() {
        let slow = |rig: &Rig| -> Vec<u32> {
            rig.from_core
                .try_iter()
                .filter_map(FromAudio::voice)
                .filter_map(|v| match v {
                    VoiceIn::Route { slow_sco_ms, .. } => slow_sco_ms,
                    _ => None,
                })
                .collect()
        };

        let mut rig = rig();
        let t0 = Instant::now();
        // A press two seconds ago whose first real sample only lands now.
        // V1's worst of sixteen trials was 1246 ms; this is worse than the
        // 1500 ms default threshold.
        rig.engine.on_cmd(listen(), t0 - Duration::from_secs(2));
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0);
        let reported = slow(&rig);
        assert_eq!(reported.len(), 1, "once, on the edge: {reported:?}");
        assert!(
            reported[0] >= 2_000 && reported[0] < 2_500,
            "{reported:?} ms"
        );

        // A press that came up at once says nothing at all.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(listen(), t0);
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0);
        assert!(slow(&rig).is_empty(), "an exception report, not a metric");
    }

    /// §7.1's meter carries **one buffer's measurement, whole**.
    ///
    /// It used to be a sum of squares in one atomic paired with a frame
    /// count in another, written in one order and read in the other — so a
    /// callback landing between the reader's two swaps had its energy
    /// counted against a different buffer's frames. Two atomics cannot be
    /// made to agree by ordering alone: whichever order each side picks,
    /// there is an interleaving where one pair straddles the other.
    /// Matching them narrows the window; putting the whole measurement in
    /// one word closes it, because there is nothing left to pair.
    ///
    /// The race itself is a race and a test of it would be a flake, so what
    /// is pinned here is the property the representation guarantees: two
    /// buffers of different length and level inside one tick, and the
    /// reported level is one of them rather than an arithmetic blend that
    /// belongs to neither.
    #[test]
    fn the_meter_never_blends_one_buffers_energy_with_anothers_frame_count() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);
        // A syllable, then nine times as much room tone. Averaged, these
        // come out at 0.184 — a number neither buffer ever contained.
        rig.capture.deliver(&[0.5; 100]);
        rig.capture.deliver(&[0.1; 900]);
        rig.engine.tick(t0 + Duration::from_millis(50));
        let rms = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Level { rms } => Some(rms),
                _ => None,
            })
            .last()
            .expect("a level");
        assert!(
            (rms - 0.5).abs() < 1e-4,
            "the loudest buffer, whole: {rms} (0.184 is the blend)"
        );
    }

    /// V19: the capture callback may not panic, whatever the device hands
    /// it.
    ///
    /// The energy accumulator used a checked `+=`. A device glitching to
    /// `+inf` casts to `u64::MAX` (Rust float→int casts saturate) and the
    /// **next** sample overflowed it — a panic on a CoreAudio time-
    /// constraint thread, which is not a failed assertion but a hard crash
    /// of the daemon with the microphone still open.
    #[test]
    fn a_device_that_glitches_to_infinity_does_not_panic_the_real_time_thread() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);
        // Two of them: one saturates the accumulator, the second is the one
        // that used to overflow it.
        rig.capture
            .deliver(&[f32::INFINITY, f32::INFINITY, f32::NAN, 0.25]);
        rig.engine.tick(t0 + Duration::from_millis(50));
        let levels: Vec<f32> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Level { rms } => Some(rms),
                _ => None,
            })
            .collect();
        assert_eq!(levels.len(), 1, "the meter still reported");
        assert!(
            levels[0] >= 0.0,
            "and it is a number, not a NaN: {}",
            levels[0]
        );
        // …and the session is still alive, which is the whole point.
        rig.capture.deliver(&[0.3; 160]);
        rig.engine.tick(t0 + Duration::from_millis(100));
        assert!(rig.engine.bring_up().is_some());
    }

    /// §7.4's `Finalizing`: the final comes from the engine, and the last
    /// partial is the fallback when the deadline passes.
    #[test]
    fn the_release_finishes_the_session_and_never_sends_a_partial() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);
        rig.stt.say(SttEvent::Partial("run the".into()));
        rig.stt.say(SttEvent::Partial("run the tests".into()));
        for event in rig
            .engine
            .session_events
            .as_ref()
            .unwrap()
            .try_iter()
            .collect::<Vec<_>>()
        {
            rig.engine.on_stt(event);
        }
        rig.stt.finish_with(Ok("run the tests".into()));
        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, t0);
        assert_eq!(rig.capture.closes(), 1);
        assert!(!rig.engine.lanes().ptt(), "the lanes are released");
        let reported: Vec<VoiceIn> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .collect();
        assert!(reported.contains(&VoiceIn::Partial {
            text: "run the".into()
        }));
        assert!(reported.contains(&VoiceIn::Final {
            text: "run the tests".into()
        }));

        // A stalled decode never turns an incomplete preview into an order.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(listen(), t0);
        rig.stt.never_finishes();
        rig.stt.say(SttEvent::Partial("allow".into()));
        for event in rig
            .engine
            .session_events
            .as_ref()
            .unwrap()
            .try_iter()
            .collect::<Vec<_>>()
        {
            rig.engine.on_stt(event);
        }
        let _ = rig.from_core.try_iter().count();
        // The never-finishing fake has a zero budget: no wall-clock wait.
        rig.engine
            .on_cmd(AudioCmd::Finish { abort: false }, t0);
        let finals: Vec<String> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Final { text } => Some(text),
                _ => None,
            })
            .collect();
        assert!(finals.is_empty(), "a timeout must never send the partial");

        // An abort wants no final at all.
        let mut rig = super::tests::rig();
        rig.engine.on_cmd(listen(), t0);
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
        assert_eq!(rig.stt.cancelled(), 1);
        assert_eq!(rig.stt.finished(), 0);
        assert!(!rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .any(|v| matches!(v, VoiceIn::Final { .. })));
    }

    #[test]
    fn final_decode_failures_report_an_error_even_when_a_partial_exists() {
        for timeout in [false, true] {
            for partial in ["", "   ", "run the tests"] {
                let mut rig = rig();
                let t0 = Instant::now();
                rig.engine.on_cmd(listen(), t0);
                rig.engine.on_stt(SttEvent::Partial(partial.into()));
                if timeout {
                    rig.stt.never_finishes();
                } else {
                    rig.stt
                        .finish_with(Err(AudioError::Engine("decoder failed".into())));
                }
                let _ = rig.from_core.try_iter().count();
                // A never-finishing fake has a zero budget; ready errors
                // are still polled once.
                rig.engine
                    .on_cmd(AudioCmd::Finish { abort: false }, t0);
                let reported: Vec<_> = rig
                    .from_core
                    .try_iter()
                    .filter_map(FromAudio::voice)
                    .filter(|v| matches!(v, VoiceIn::Final { .. } | VoiceIn::FinalFailed { .. }))
                    .collect();
                assert_eq!(reported.len(), 1, "{reported:?}");
                let VoiceIn::FinalFailed { code, message } = &reported[0] else {
                    panic!("a failed decode must never become a successful final: {reported:?}");
                };
                assert_eq!(code, "engine");
                assert!(message.contains(if timeout { "timed out" } else { "decoder failed" }));
                assert_eq!(rig.capture.closes(), 1);
                assert!(!rig.engine.lanes().ptt());
            }
        }
    }

    #[test]
    fn a_successful_empty_decode_is_still_a_normal_final() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(listen(), t0);
        rig.stt.finish_with(Ok(String::new()));
        let _ = rig.from_core.try_iter().count();
        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, t0);
        let reported: Vec<_> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter(|v| matches!(v, VoiceIn::Final { .. } | VoiceIn::FinalFailed { .. }))
            .collect();
        assert_eq!(
            reported,
            vec![VoiceIn::Final {
                text: String::new()
            }]
        );
    }

    #[test]
    fn a_slow_final_includes_the_release_tail_instead_of_sending_the_preview() {
        let mut rig = rig();
        let now = Instant::now();
        rig.engine.on_cmd(listen(), now);
        rig.engine.on_stt(SttEvent::Partial("restart the build".into()));
        rig.capture.deliver(&[0.2; 512]);
        rig.stt.finish_after(Duration::from_millis(1700), Ok("restart the build with the cache disabled".into()));
        let _ = rig.from_core.try_iter().count();
        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, now);
        assert_eq!(rig.stt.fed().iter().map(|(_, n)| n).sum::<usize>(), 512, "audio buffered at release reaches the final decode");
        assert!(!rig.from_core.try_iter().filter_map(FromAudio::voice).any(|v| matches!(v, VoiceIn::Final { .. } | VoiceIn::FinalFailed { .. })));
        // A second press captures and finishes while the first decode waits.
        rig.engine.on_cmd(listen(), Instant::now());
        rig.capture.deliver(&[0.3; 512]);
        rig.stt.finish_after(Duration::ZERO, Ok("second complete message".into()));
        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, Instant::now());
        assert_eq!(rig.stt.starts().len(), 2);
        assert_eq!(rig.stt.fed().iter().map(|(_, n)| n).sum::<usize>(), 1024);
        assert_eq!(rig.engine.finals.len(), 2, "even a ready second result waits for the first");
        std::thread::sleep(Duration::from_millis(1710));
        rig.engine.tick(Instant::now());
        let results: Vec<_> = rig.from_core.try_iter().filter_map(FromAudio::voice)
            .filter(|v| matches!(v, VoiceIn::Final { .. } | VoiceIn::FinalFailed { .. })).collect();
        assert_eq!(results, [VoiceIn::Final { text: "restart the build with the cache disabled".into() }, VoiceIn::Final { text: "second complete message".into() }]);
    }

    #[test]
    fn pending_final_decodes_are_bounded_without_discarding_previous_recordings() {
        let mut rig = rig();
        for _ in 0..MAX_FINALS {
            rig.engine.on_cmd(listen(), Instant::now());
            rig.stt.finish_after(Duration::from_secs(60), Ok("complete".into()));
            rig.engine.on_cmd(AudioCmd::Finish { abort: false }, Instant::now());
        }
        let _ = rig.from_core.try_iter().count();
        rig.engine.on_cmd(listen(), Instant::now());
        assert_eq!(rig.stt.starts().len(), MAX_FINALS);
        assert_eq!(rig.engine.finals.len(), MAX_FINALS);
        assert!(rig.from_core.try_iter().filter_map(FromAudio::voice).any(|v| matches!(v, VoiceIn::Error { code, .. } if code == "audio_busy")));
    }

    /// B6's remainder, end to end from the thread that knows: one `Say`
    /// in flight, `started` when its first sample is queued, `ended` when
    /// its last one is, and the next one only then.
    #[test]
    fn the_engine_reports_the_utterance_phases_and_keeps_one_say_in_flight() {
        let mut rig = rig();
        let t0 = Instant::now();
        for (n, text) in [(1, "bot is ready"), (2, "bot failed")] {
            rig.engine.on_cmd(
                AudioCmd::Say {
                    utterance: format!("ut_{n}"),
                    text: text.into(),
                    agent: None,
                    lane: "ready".into(),
                    voice: None,
                    interrupt: false,
                    ttl_s: None,
                },
                t0,
            );
        }
        assert_eq!(rig.engine.speaking.as_deref(), Some("ut_1"));
        assert_eq!(rig.engine.queue.len(), 1, "one in flight, one waiting");

        let mut phases = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while phases.len() < 4 && Instant::now() < deadline {
            if let Ok(report) = rig.reports.recv_timeout(Duration::from_millis(200)) {
                rig.engine.on_tts(report, Instant::now());
            }
            for v in rig.from_core.try_iter().filter_map(FromAudio::voice) {
                if let VoiceIn::Utterance { utterance, phase } = v {
                    phases.push((utterance, phase));
                }
            }
        }
        assert_eq!(
            phases,
            vec![
                ("ut_1".to_string(), UtterancePhase::Started),
                ("ut_1".to_string(), UtterancePhase::Ended),
                ("ut_2".to_string(), UtterancePhase::Started),
                ("ut_2".to_string(), UtterancePhase::Ended),
            ],
            "and the second only starts after the first ended"
        );
    }

    /// `set mic` (§7.1/B4) reaches the capture path **during** a hold, and
    /// `voice.stop{utterance}` names an utterance rather than a lane.
    ///
    /// The second half is a shape that is easy to get wrong in a way no
    /// caller would ever notice until it mattered: a `stop` that compared
    /// the utterance against the lane name would silently cancel the wrong
    /// thing whenever somebody named a lane `"ut_7"` — and, more usefully,
    /// would fail to cancel the right thing every other time.
    #[test]
    fn the_mic_gain_is_live_and_a_stop_names_what_it_means() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_cmd(
            AudioCmd::Gain {
                key: SetKey::Mic,
                value: 10,
            },
            t0,
        );
        rig.engine.on_cmd(listen(), t0);
        let level = |rig: &Rig| -> f32 {
            rig.from_core
                .try_iter()
                .filter_map(FromAudio::voice)
                .filter_map(|v| match v {
                    VoiceIn::Level { rms } => Some(rms),
                    _ => None,
                })
                .last()
                .expect("a level")
        };
        rig.capture.deliver(&[0.5; 160]);
        rig.engine.tick(t0 + Duration::from_millis(50));
        assert!((level(&rig) - 0.5).abs() < 1e-4, "at unity");
        // Turned down mid-hold: the very next buffer is quieter, and the
        // meter says so on the next tick rather than averaging the two.
        rig.engine.on_cmd(
            AudioCmd::Gain {
                key: SetKey::Mic,
                value: 1,
            },
            t0,
        );
        rig.capture.deliver(&[0.5; 160]);
        rig.engine.tick(t0 + Duration::from_millis(100));
        let quieter = level(&rig);
        assert!(
            (quieter - 0.5 * crate::audio::lanes::perceptual_gain(1)).abs() < 1e-4,
            "the gain change reached the callback: rms {quieter}"
        );
        assert_eq!(
            rig.engine.lanes().mic(),
            crate::audio::lanes::perceptual_gain(1)
        );
        rig.engine.on_cmd(AudioCmd::Finish { abort: true }, t0);

        // `voice.stop{utterance}` drops the queued one it names, and only it.
        let say = |n: u32, lane: &str| AudioCmd::Say {
            utterance: format!("ut_{n}"),
            text: "bot said something".into(),
            agent: None,
            lane: lane.to_string(),
            voice: None,
            interrupt: false,
            ttl_s: None,
        };
        rig.engine.on_cmd(say(1, "ready"), t0);
        rig.engine.on_cmd(say(2, "chatter"), t0);
        rig.engine.on_cmd(say(3, "ready"), t0);
        assert_eq!(rig.engine.queue.len(), 2);
        rig.engine.on_cmd(
            AudioCmd::Stop {
                utterance: Some("ut_3".into()),
                lane: None,
                all: false,
            },
            t0,
        );
        let left: Vec<String> = rig
            .engine
            .queue
            .iter()
            .map(|(cmd, _)| match cmd {
                AudioCmd::Say { utterance, .. } => utterance.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(left, vec!["ut_2".to_string()], "only the named one went");

        // …and by lane, which is the other half of §4.9's `voice.stop`.
        rig.engine.on_cmd(
            AudioCmd::Stop {
                utterance: None,
                lane: Some("chatter".into()),
                all: false,
            },
            t0,
        );
        assert!(rig.engine.queue.is_empty());
    }

    /// §4.9's `ttl_s?:30` — "drop this one if it has not started by then".
    ///
    /// It is a deadline on **starting**, and the queue is the only place
    /// that can honour it: once `deckd-tts` has the item it has started.
    /// B5 carried it rather than dropping it at the API precisely so that
    /// somebody would honour it here.
    #[test]
    fn a_say_that_missed_its_ttl_never_starts_and_says_so() {
        let mut rig = rig();
        let t0 = Instant::now();
        let say = |n: u32, ttl: Option<u32>| AudioCmd::Say {
            utterance: format!("ut_{n}"),
            text: "bot said something".into(),
            agent: None,
            lane: "chatter".into(),
            voice: None,
            interrupt: false,
            ttl_s: ttl,
        };
        // One in flight with no deadline, two behind it: the first of those
        // has 30 s, the second has 1 s.
        rig.engine.on_cmd(say(1, None), t0);
        rig.engine.on_cmd(say(2, Some(30)), t0);
        rig.engine.on_cmd(say(3, Some(1)), t0);
        assert_eq!(rig.engine.speaking.as_deref(), Some("ut_1"));
        assert_eq!(rig.engine.queue.len(), 2);

        // Two seconds later `ut_3` is stale and `ut_2` is not. The tick is
        // what notices, so a queue behind a long utterance still expires.
        rig.engine.speaking = None;
        rig.engine.tick(t0 + Duration::from_secs(2));
        assert_eq!(
            rig.engine.speaking.as_deref(),
            Some("ut_2"),
            "the one that is still inside its deadline starts"
        );
        assert!(rig.engine.queue.is_empty());
        let ended: Vec<String> = rig
            .from_core
            .try_iter()
            .filter_map(FromAudio::voice)
            .filter_map(|v| match v {
                VoiceIn::Utterance {
                    utterance,
                    phase: UtterancePhase::Ended,
                } => Some(utterance),
                _ => None,
            })
            .collect();
        assert_eq!(
            ended,
            vec!["ut_3".to_string()],
            "a dropped utterance still ends: a reader waiting on \
             voice.utterance must not wait for ever"
        );
    }

    /// §7.2's route, driven from this side: the press closes the output and
    /// the release opens it again at the rate the device has *now* (V3).
    #[test]
    fn a_press_closes_the_output_and_the_release_opens_it_at_the_tail_rate() {
        let mut rig = rig();
        let t0 = Instant::now();
        rig.engine.on_device(DeviceSide::Output, true, t0);
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 44_100 }, t0);
        assert!(rig.engine.output_mut().is_open());
        assert_eq!(rig.engine.route(), deck_model::Route::A2dp);

        rig.engine.on_cmd(listen(), t0);
        assert!(
            !rig.engine.output_mut().is_open(),
            "§7.2/V5: the output is closed for our own capture"
        );
        rig.engine
            .on_route(RouteEvent::NominalRate { rate: 16_000 }, t0);
        assert_eq!(rig.engine.route(), deck_model::Route::ScoOurs);

        rig.engine.on_cmd(AudioCmd::Finish { abort: false }, t0);
        assert_eq!(rig.engine.route(), deck_model::Route::Reopening);
        assert!(
            rig.engine.output_mut().is_open(),
            "V3: open at once, at 16000, not in 2.5 s at 44100"
        );
        // And the lanes came back with it, which is §15's row 5 seen from
        // the loop rather than from the mixer.
        assert!(
            rig.engine.lanes().take_mixer().is_none(),
            "the stream has them"
        );
    }
}
