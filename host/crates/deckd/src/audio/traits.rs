//! The engine seams of §7.6 and §7.1, **as the measurements require** rather
//! than as the design draws them (HOST-CONTRACT-W4 §2 V16, V17, V19).
//!
//! The design's block is four lines of pseudo-Rust that does not compile and
//! could not be implemented against Apple's frameworks if it did. Every
//! change below is one measured fact:
//!
//! * **Lifetimes (V16, `E0106`).** `BoxFuture<Result<String>>` names no
//!   lifetime. [`BoxFuture<'static, T>`](BoxFuture) is the fix, and `'static`
//!   is the honest bound: the future outlives the session it came from,
//!   because `finish` consumes the session.
//! * **`SttSession` is `+ Send`, and is a *handle* (V13, V16).** The ObjC
//!   objects are not `Send` and `SFSpeechRecognizer`'s result handlers are
//!   delivered on the **main thread only** — measured: a worker thread with
//!   no run loop got 0 buffers, a worker driving its own run loop got 0, main
//!   got 300/300. The session is created on the core and fed from the capture
//!   thread, so what crosses threads has to be a `Send` handle that posts to
//!   main ([`crate::audio::mainloop`]) and reads back through a channel.
//! * **A mid-session error channel (V16).** The design has errors only at
//!   `finish()`, and `kAFAssistantErrorDomain 1110` was observed arriving
//!   *during* a session. §7.4's dead-mic path needs to hear about it while
//!   the operator is still holding the button. [`SttEvent::Error`] is that
//!   channel, and it shares the partials' channel rather than sitting beside
//!   it — one `select!` arm, one ordering.
//! * **Per-locale on-device support (§7.6's `-32009`).** The refusal has to
//!   be answerable **before** a session starts (zh-CN has no on-device asset
//!   on this machine), so it is a question to the engine, not an error out of
//!   `start`. [`SttEngine::on_device`].
//! * **`TtsEngine` returns its format up front (V16).** `synthesize` in the
//!   design discards it, and AVSpeech's format varies per voice — measured
//!   22050/1/f32 for Samantha. The resampler cannot be built after the PCM
//!   starts arriving without either allocating on the audio path or throwing
//!   the first buffer away, so [`TtsJob::format`] is known before the first
//!   chunk is read.
//! * **A `stop()` handle (V16).** Barge-in (`ptt act:1` → `voice.stop all`)
//!   happens on another thread from the one draining the job, so the stop is
//!   a separate `Send + Sync` handle ([`TtsStop`]) and not `&mut self`.
//! * **`CaptureSource` exists at all (V17).** §14 assumes it; §7 never
//!   declares it. Without it, cpal → `rtrb` → `AVAudioPCMBuffer`, the mic
//!   gain and the RMS meter have no test that does not need a microphone and
//!   a TCC grant.
//! * **`RouteEvent` exists at all (V17).** The route SM must consume events,
//!   never call CoreAudio, so that §15's row 7 can script the rate flips.
//! * **The fault path is atomics (V19).** cpal's `Xrun` listener fires **on
//!   the real-time thread** and calls the user error callback, and on
//!   `StreamInvalidated`/`DeviceNotAvailable` cpal pauses the unit *before*
//!   calling it. So the error callback may not allocate, lock or log:
//!   [`StreamFaults`] is a single `fetch_or` and the control loop drains it.
use deck_model::VoiceInput;

/// Default final-decode budget. Only a completed decode may be dispatched.
pub const FINISH_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
use std::sync::atomic::{AtomicU32, Ordering};

/// The design's `BoxFuture` with the lifetime it was missing (V16).
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Interleaved f32 is the only sample format that crosses these seams:
/// cpal gives it, AVAudio gives it, rubato takes it, the mixer sums it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    #[must_use]
    pub const fn new(rate: u32, channels: u16) -> Self {
        AudioFormat { rate, channels }
    }

    /// The mixer's own format: §7.3's lanes are interleaved stereo f32 at
    /// 44.1 kHz, which is the deck's A2DP nominal rate.
    #[must_use]
    pub const fn lane() -> Self {
        AudioFormat {
            rate: crate::audio::LANE_RATE,
            channels: 2,
        }
    }
}

/// What can go wrong on any of these seams.
///
/// One enum rather than one per trait: every one of them ends up in the same
/// place — `VoiceIn::Error{code}` — and §7.8's rule is that a denied grant is
/// *silence*, so the code matters and the prose does not.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    /// The UID was not among the host's devices (BLE HID only, unpaired).
    #[error("no such audio device: {0}")]
    NoDevice(String),
    /// The device is there and the OS refused us (TCC, another exclusive
    /// client). §7.8: silence, never a panic.
    #[error("audio device refused: {0}")]
    Refused(String),
    /// A locale with no on-device asset and `allow_server = false` (§7.6).
    #[error("locale {0} has no on-device recogniser")]
    NoOnDevice(String),
    /// The engine failed mid-flight (`kAFAssistantErrorDomain 1110`).
    #[error("{0}")]
    Engine(String),
    /// Nothing is installed behind this seam in this build.
    #[error("no {0} engine in this build")]
    NoEngine(&'static str),
}

impl AudioError {
    /// The `VoiceIn::Error{code}` this becomes. Stable strings: the GUI and
    /// §12's table key off them.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            AudioError::NoDevice(_) => "no_device",
            AudioError::Refused(_) => "device_refused",
            AudioError::NoOnDevice(_) => "no_on_device",
            AudioError::Engine(_) => "engine",
            AudioError::NoEngine(_) => "no_engine",
        }
    }
}

// ---------------------------------------------------------------------------
// The real-time fault path (V19)
// ---------------------------------------------------------------------------

/// One fault a stream callback can report, as a bit.
///
/// Deliberately `Copy` and tiny: it is raised from cpal's error callback,
/// which runs on the CoreAudio real-time thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Fault {
    /// `ErrorKind::Xrun` — `kAudioDeviceProcessorOverload`. cpal carries no
    /// message with it precisely because this is the RT thread.
    Xrun = 1,
    /// `ErrorKind::StreamInvalidated` — on this device that is the nominal
    /// rate flipping under us. **cpal has already paused the unit.**
    Invalidated = 2,
    /// `ErrorKind::DeviceNotAvailable`/`DeviceChanged` — the BT link went.
    /// **cpal has already paused the unit.**
    Gone = 4,
    /// `ErrorKind::PermissionDenied` — a missing TCC grant (§7.8).
    Denied = 8,
    /// Anything else cpal reports.
    Other = 16,
}

/// The faults a stream has raised since the control loop last looked.
///
/// This is the whole of what an error callback is allowed to do (V19): one
/// `fetch_or` on an `AtomicU32`. No allocation, no lock, no `tracing!`, no
/// channel — cpal's own `Xrun` path is careful for exactly this reason
/// (`try_emit_error` uses `try_lock` and drops on contention), and a
/// `crossbeam` send would take a mutex in the waker registry whenever a
/// receiver happens to be parked.
///
/// The cost of the atomic is that faults **coalesce**: ten xruns in one
/// quantum are one bit. That is the right trade — the control loop's answer
/// to "the stream faulted" does not depend on how many times it did — and the
/// counter below keeps the number for `metrics.get`.
#[derive(Debug, Default)]
pub struct StreamFaults {
    bits: AtomicU32,
    xruns: AtomicU32,
}

impl StreamFaults {
    #[must_use]
    pub const fn new() -> Self {
        StreamFaults {
            bits: AtomicU32::new(0),
            xruns: AtomicU32::new(0),
        }
    }

    /// Called from the real-time thread. Allocation-free, lock-free, and it
    /// cannot block: two relaxed read-modify-writes.
    #[inline]
    pub fn raise(&self, fault: Fault) {
        self.bits.fetch_or(fault as u32, Ordering::Relaxed);
        if matches!(fault, Fault::Xrun) {
            self.xruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Take everything raised since the last call (control loop side).
    pub fn take(&self) -> Vec<Fault> {
        let bits = self.bits.swap(0, Ordering::Relaxed);
        [
            Fault::Gone,
            Fault::Invalidated,
            Fault::Denied,
            Fault::Xrun,
            Fault::Other,
        ]
        .into_iter()
        .filter(|f| bits & (*f as u32) != 0)
        .collect()
    }

    /// Total xruns since the daemon started (`metrics.get`).
    #[must_use]
    pub fn xruns(&self) -> u32 {
        self.xruns.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Capture (V17)
// ---------------------------------------------------------------------------

/// Where the capture buffers and the capture faults go.
///
/// Both halves are handed to the source together because both are called
/// from cpal's own threads and both have to be safe there: `deliver` is the
/// input callback and `fault` is the error callback (V19).
pub struct CaptureSink {
    frames: FrameSink,
    faults: std::sync::Arc<StreamFaults>,
}

/// What a capture source calls with each buffer: `(rate, &[f32])` (V17).
pub type FrameSink = Box<dyn FnMut(u32, &[f32]) + Send + 'static>;

impl CaptureSink {
    #[must_use]
    pub fn new(
        faults: std::sync::Arc<StreamFaults>,
        frames: impl FnMut(u32, &[f32]) + Send + 'static,
    ) -> Self {
        CaptureSink {
            frames: Box::new(frames),
            faults,
        }
    }

    /// One buffer, at the rate the device is actually running (V3/V8: it is
    /// 16000 on this deck and the code never asks for another).
    #[inline]
    pub fn deliver(&mut self, rate: u32, samples: &[f32]) {
        (self.frames)(rate, samples);
    }

    /// The error callback's only move (V19).
    #[inline]
    pub fn fault(&self, fault: Fault) {
        self.faults.raise(fault);
    }

    #[must_use]
    pub fn faults(&self) -> std::sync::Arc<StreamFaults> {
        self.faults.clone()
    }
}

impl std::fmt::Debug for CaptureSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSink").finish_non_exhaustive()
    }
}

/// What a capture open asks for. Never a sample rate: §7.1 opens at exactly
/// the device's own nominal config, because cpal would otherwise try to set
/// `kAudioDevicePropertyNominalSampleRate` and a Bluetooth HFP endpoint
/// cannot honour it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureOpen {
    /// The deck's microphone, or the Mac's for `voice.listen{input:"mac"}`.
    /// There is no fallback from one to the other: a deck input that is not
    /// there is [`AudioError::NoDevice`] (decision 11).
    pub input: VoiceInput,
    /// `[voice] device_uid` — the deck, without its `:input` suffix: the
    /// configured one, else the one the engine discovered at start.
    pub device_uid: Option<String>,
    /// Nobody pressed: ask the deck to stream its microphone for this many
    /// seconds (1 to 60). Over the BLE voice link the deck streams only on
    /// its own press or when asked; `None` for a press, which already is
    /// streaming. The CoreAudio capture ignores it.
    pub ask_deck: Option<u8>,
}

/// What an open landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureOpened {
    pub rate: u32,
    pub channels: u16,
    /// The UID actually opened, for `voice.route{input_uid}`.
    pub uid: Option<String>,
}

/// The microphone, behind a seam (V17).
///
/// Implemented by cpal on macOS and by [`crate::audio::fakes::FakeCapture`]
/// in every deterministic test. It is `Send` and lives on `deckd-audio`: the
/// `Stream` is built, kept and dropped there, which is the `deckd-hid-probe`
/// precedent (a permanent thread owning a run-loop resource) and not a
/// `!Send` requirement — V10 measured cpal 0.18.2's `Stream` as `Send +
/// Sync`.
pub trait CaptureSource: Send {
    /// Open, and start delivering into `sink`. Idempotent only in the sense
    /// that a second open closes the first.
    ///
    /// # Errors
    /// [`AudioError::NoDevice`] when the input asked for is not there (for
    /// the deck's, never answered with the Mac's), [`AudioError::Refused`]
    /// when the OS said no.
    fn open(&mut self, req: &CaptureOpen, sink: CaptureSink) -> Result<CaptureOpened, AudioError>;

    /// Close it. Must be safe to call when nothing is open — §3's
    /// back-pressure retries `Finish`, so this genuinely happens twice.
    fn close(&mut self);

    fn is_open(&self) -> bool;

    /// Whether everything the source will ever deliver for this open has
    /// arrived. The deck's stream says so with its END, which follows the
    /// last frame over the radio: until then frames encoded before the
    /// release are still in flight, and a close would drop the last
    /// syllable. A source with no such notion (CoreAudio) is always done.
    fn ended(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// STT (§7.6, V16)
// ---------------------------------------------------------------------------

/// Whether a locale can be recognised without the network (§7.6's `-32009`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDevice {
    /// `supportsOnDeviceRecognition` is true (en-US here).
    Yes,
    /// The locale is supported, but only by the server (zh-CN here).
    /// `-32009` unless `[voice.stt] allow_server = true`.
    ServerOnly,
    /// The recogniser does not know the locale at all.
    Unsupported,
}

/// What `SttEngine::start` is told. Everything here comes from the reducer's
/// `AudioCmd::Listen` (B3) — the engine derives none of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SttStart {
    pub locale: String,
    /// §7.6's `contextualStrings`: agent names and the pending card's choice
    /// labels, so `cohdl-api` and `Allow always` survive recognition.
    pub contextual: Vec<String>,
    /// `[voice.stt] allow_server`. False and `on_device(locale) != Yes` is
    /// the `-32009` case, refused here rather than half-started.
    pub allow_server: bool,
}

/// One thing the recogniser said.
///
/// The design had `partials() -> watch::Receiver<String>` and errors only at
/// `finish()`. Both are replaced by this single ordered channel (V16):
/// a `watch` cannot carry an error, `deckd-audio` is a `crossbeam::select!`
/// and not a tokio task, and keep-latest is the outbox's job now (B16) — so
/// the engine no longer has to be the thing that drops partials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SttEvent {
    Partial(String),
    /// The recogniser called it final on its own (`isFinal`), before
    /// `finish()` was asked for.
    Final(String),
    /// Mid-session, and the reason this is not just a `finish()` result:
    /// `kAFAssistantErrorDomain 1110` arrives while the operator is still
    /// holding the button.
    Error {
        code: String,
        message: String,
    },
}

/// One live recognition. **`Send`, and a handle** (V13/V16): the ObjC
/// objects it stands for are not `Send` and their callbacks land on main, so
/// an implementation posts work there and reads results back through
/// [`SttSession::events`].
pub trait SttSession: Send {
    /// Mono f32 at `rate` — whatever the device gave us, not a fixed rate
    /// (V8: 16000 here, but 8 kHz CVSD stays defensive code).
    fn feed(&mut self, mono_f32: &[f32], rate: u32);

    /// Partials, an early final, and mid-session errors, in order.
    fn events(&self) -> crossbeam_channel::Receiver<SttEvent>;

    /// §7.4's `Finalizing`: ask for the final and give up on it after the
    /// caller's own deadline.
    ///
    /// Consumes the session, which is why the future is `'static`.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>>;

    /// How long the caller waits for the complete final before reporting a
    /// failure. A timeout must never promote partial text to a final.
    fn finish_budget(&self) -> std::time::Duration {
        FINISH_BUDGET
    }

    /// §7.4's `Aborting`: no final wanted, drop everything.
    fn cancel(self: Box<Self>);
}

/// What `voice.engines` reports about one engine (§4.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    pub name: String,
    pub on_device: bool,
    /// Locales the engine will accept at all.
    pub locales: Vec<String>,
}

/// The recogniser, behind a seam.
pub trait SttEngine: Send + Sync {
    fn info(&self) -> EngineInfo;

    /// §7.6's `-32009`, answerable **before** anything is opened.
    fn on_device(&self, locale: &str) -> OnDevice;

    /// # Errors
    /// [`AudioError::NoOnDevice`] for the `-32009` case, and whatever the
    /// engine could not do.
    fn start(&self, req: &SttStart) -> Result<Box<dyn SttSession>, AudioError>;
}

// ---------------------------------------------------------------------------
// TTS (§7.6, V16)
// ---------------------------------------------------------------------------

/// One installed voice (§7.7's per-agent pool).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    pub locale: String,
    /// `AVSpeechSynthesisVoiceQuality.enhanced`/`premium`. §7.7's
    /// `default_pool` wants four distinct enhanced en-US voices.
    pub enhanced: bool,
}

/// What to say.
#[derive(Clone, Debug, PartialEq)]
pub struct TtsRequest {
    pub text: String,
    /// `AVSpeechSynthesisVoice` identifier; `None` is the system default.
    pub voice: Option<String>,
    /// `AVSpeechUtterance.rate`, 0.0..=1.0.
    pub rate: f32,
}

/// One chunk of synthesised audio, in the job's own format.
#[derive(Clone, Debug, PartialEq)]
pub enum TtsChunk {
    /// Interleaved samples in [`TtsJob::format`].
    Pcm(Vec<f32>),
    /// AVSpeech's zero-`frameLength` buffer: the utterance is over.
    End,
    Error(String),
}

/// Barge-in, from another thread (V16).
///
/// `ptt act:1` cuts everything on `deckd-audio` while `deckd-tts` is blocked
/// filling a ring, so the stop cannot be `&mut self` on the job.
pub trait TtsStop: Send + Sync {
    /// Idempotent, and safe to call after the job has already ended.
    fn stop(&self);
}

/// One synthesis in flight.
pub trait TtsJob: Send {
    /// **Known before the first chunk is read** (V16). AVSpeech's format
    /// varies per voice — 22050/1/f32 for Samantha — and the resampler has
    /// to exist before the PCM does, or the first buffer is either dropped
    /// or resampled by something allocated on the fly.
    fn format(&self) -> AudioFormat;

    fn chunks(&self) -> crossbeam_channel::Receiver<TtsChunk>;

    /// A handle that outlives `self`'s borrow, for the barge-in.
    fn stopper(&self) -> std::sync::Arc<dyn TtsStop>;
}

/// The synthesiser, behind a seam.
pub trait TtsEngine: Send + Sync {
    fn info(&self) -> EngineInfo;

    fn voices(&self) -> Vec<VoiceInfo>;

    /// # Errors
    /// Whatever the engine could not start.
    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError>;
}

// ---------------------------------------------------------------------------
// The route's input (V17)
// ---------------------------------------------------------------------------

/// Which of the deck's two devices an event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceSide {
    Input,
    Output,
}

/// Everything the route state machine of §7.2 is allowed to know (V17).
///
/// The machine never calls CoreAudio: it folds these. That is what makes
/// §15's row 7 a scripted, deterministic test instead of a Bluetooth ritual,
/// and it is why `ScoExternal` — V7's *routine* path on this machine, since
/// the deck is the system default input — can be covered at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteEvent {
    /// The output device's `kAudioDevicePropertyNominalSampleRate` moved.
    /// From the standalone property listener while no stream is open, or
    /// from re-reading it after a [`Fault::Invalidated`].
    NominalRate {
        rate: u32,
    },
    /// The deck's UID appeared / disappeared (`DeviceIsAlive`, enumeration).
    DevicePresent {
        side: DeviceSide,
        present: bool,
    },
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere` on the **input**: some
    /// other process is holding the mic (V7 — Zoom, dictation).
    ExternalCapture {
        running: bool,
    },
    /// Our own capture opened at `rate` / closed. The machine cannot infer
    /// this: the rate flip it causes arrives ~110 ms later.
    OurCapture {
        open: bool,
        rate: u32,
    },
    /// An attempt to open the output finished.
    OutputOpened {
        rate: u32,
    },
    OutputFailed,
    /// A fault the render or error callback raised (V19).
    Fault(Fault),
    /// The control loop's 50 ms tick, so the machine can time its own waits
    /// without owning a clock.
    Tick,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V19's contract in one assertion: the error callback's whole move is a
    /// pair of atomic read-modify-writes, so it can be called from the
    /// real-time thread. The test that matters is the one in
    /// `tests/voice_mixer.rs` that counts allocations; this one pins the
    /// *coalescing*, which is the price of the atomic and is deliberate.
    #[test]
    fn faults_coalesce_and_the_xrun_count_survives_the_take() {
        let faults = StreamFaults::new();
        assert!(faults.take().is_empty());
        for _ in 0..7 {
            faults.raise(Fault::Xrun);
        }
        faults.raise(Fault::Invalidated);
        assert_eq!(faults.take(), vec![Fault::Invalidated, Fault::Xrun]);
        assert!(
            faults.take().is_empty(),
            "a take clears; the control loop must not see one fault twice"
        );
        assert_eq!(
            faults.xruns(),
            7,
            "the bit coalesces, the metrics counter does not"
        );
        // `Gone` sorts first: a stream that went away is the fault the route
        // SM has to act on, and an xrun raised in the same quantum is noise.
        faults.raise(Fault::Xrun);
        faults.raise(Fault::Gone);
        assert_eq!(faults.take()[0], Fault::Gone);
    }

    /// Every error the seams can produce has a stable code, because that is
    /// what reaches `VoiceIn::Error{code}` and the GUI.
    #[test]
    fn every_audio_error_has_a_stable_code() {
        let all = [
            AudioError::NoDevice("x".into()),
            AudioError::Refused("x".into()),
            AudioError::NoOnDevice("zh-CN".into()),
            AudioError::Engine("1110".into()),
            AudioError::NoEngine("stt"),
        ];
        let codes: Vec<&str> = all.iter().map(AudioError::code).collect();
        assert_eq!(
            codes,
            vec![
                "no_device",
                "device_refused",
                "no_on_device",
                "engine",
                "no_engine"
            ]
        );
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "codes must be distinct");
    }

    /// The lane format is the one §7.3 names, and the one every resampler in
    /// `deckd-tts` targets.
    #[test]
    fn the_lane_format_is_stereo_at_the_a2dp_rate() {
        assert_eq!(AudioFormat::lane(), AudioFormat::new(44_100, 2));
    }
}
