//! The seam between the audio side and the core (HOST-CONTRACT-W4 §3 B1),
//! built to the shape `deckd::hid` already has: a **bounded** channel out of
//! the core that is only ever `try_send`, an **unbounded** one back, and a
//! handle the core owns.
//!
//! The asymmetry is the same one §2.3 argues for the device link, and it
//! matters more here. V1 measured SCO bring-up at **1031 ms median**, and a
//! bring-up wedged behind a Bluetooth timeout has no upper bound at all — so
//! a core that blocked on `send` would stop answering every agent, every
//! hook and every device line for as long as a headset felt like taking.
//!
//! What a refusal *means* is per command, and the commands say so themselves
//! ([`deck_model::AudioCmd::retry_when_refused`] and
//! [`deck_model::AudioCmd::aborts_when_refused`]): unlike a device op there
//! is no re-projection here, so a dropped `AudioCmd` is gone for good and
//! "drop it" cannot be the answer for all of them.
//!
//! **There may be no engine at all.** There is one now — [`spawn`] builds
//! it — but `--no-device` installs none, a non-macOS build has none, and a
//! daemon running without one must behave exactly as it did before: the
//! command is logged and forgotten, and a PTT press still runs the whole
//! §7.4 machine against no audio. That is what [`AudioHandle::none`] is, and
//! it is why "the queue refused it" and "there is nothing to refuse it" are
//! different states rather than the same `Disconnected`.
//!
//! The engine itself is the rest of this module: [`traits`] declares the
//! seams the measurements require (V16/V17), [`fakes`] is what CI runs
//! behind them, [`lanes`] and its `Mixer` are §7.3's real-time mixer,
//! [`route`] is §7.2's state machine as a pure fold, [`engine`] is
//! `deckd-audio`, [`tts`] is `deckd-tts`, [`coreaudio`] and [`apple`] are
//! the platform, and [`mainloop`] is B10. [`ble`] is the deck's microphone
//! and speaker over its BLE audio service — `[voice] transport = "ble"`, the
//! default since the Rev A firmware stopped being a Classic audio device —
//! behind the same two seams the CoreAudio devices sit behind.
#[cfg(target_os = "macos")]
pub mod ble;
pub mod fakes;
// The engine's own modules are macOS-gated exactly like the crates they are
// built on (B11, and `tests/voice_deps.rs` pins that gating): `rtrb` and
// `rubato` are declared under `[target.'cfg(target_os = "macos")'
// .dependencies]` beside CoreAudio and Speech.framework, so a `cargo check`
// for §13's Linux port must find the **seam** here and no engine — which is
// exactly the state §15 leaves that port in until v2.
#[cfg(target_os = "macos")]
pub mod apple;
#[cfg(target_os = "macos")]
mod apple_transcript;
#[cfg(target_os = "macos")]
pub mod coreaudio;
#[cfg(target_os = "macos")]
pub mod engine;
#[cfg(target_os = "macos")]
pub mod lanes;
#[cfg(target_os = "macos")]
pub mod mainloop;
pub mod route;
pub mod cloud;
pub mod sidecar;
#[cfg(any(target_os = "macos", test))]
mod speaker_eq;
#[cfg(any(target_os = "macos", test))]
mod adpcm;
#[cfg(any(target_os = "macos", test))]
mod speech_level;
#[cfg(target_os = "macos")]
pub mod whisper;
#[cfg(target_os = "macos")]
pub mod kokoro;
pub mod traits;
#[cfg(target_os = "macos")]
pub mod tts;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use deck_model::{AudioCmd, VoiceIn};

/// The mixer's rate (§7.3): the deck's A2DP nominal rate, which every lane
/// ring is filled at and every render callback drains at.
pub const LANE_RATE: u32 = 44_100;

/// **V2's liveness rule, written once.**
///
/// A capture buffer is evidence that the microphone is alive if and only if
/// it carries a sample that is not an exact digital zero. Nothing else will
/// do: the deck's input callback starts at ~28 ms, `AudioUnitRender` returns
/// `noErr` the whole time, and **every sample is an exact zero until
/// 1031 ms** (median of 16 trials, 815–1246 ms, 2026-09-21). A check that
/// counted buffers would pass on a microphone that is not there at all.
///
/// It lives here, outside the macOS-gated engine, because the engine and
/// the headless §15 row-2 rig must not be able to disagree about what a
/// buffer means — the previous rig decided liveness for itself, and the
/// engine that was supposed to decide it never told the core at all.
///
/// Real-time safe: one pass over a borrowed slice, no allocation, no lock,
/// nothing that can block. The capture callback calls it per buffer.
#[inline]
#[must_use]
pub fn buffer_is_live(samples: &[f32]) -> bool {
    samples.iter().any(|s| *s != 0.0)
}

/// §7.4's `Arming → Listening` edge, which happens exactly once per capture.
///
/// The engine may see a hundred non-zero buffers; the core is told once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Liveness {
    live: bool,
}

impl Liveness {
    #[must_use]
    pub const fn new() -> Liveness {
        Liveness { live: false }
    }

    /// Fold one capture buffer in. `true` **only** on the edge.
    pub fn observe(&mut self, samples: &[f32]) -> bool {
        self.observe_nonzero(buffer_is_live(samples))
    }

    /// The same edge, for a caller that has already applied
    /// [`buffer_is_live`] — the engine's capture callback does it on the
    /// real-time thread and hands the answer over as one atomic bool.
    pub fn observe_nonzero(&mut self, nonzero: bool) -> bool {
        if self.live || !nonzero {
            return false;
        }
        self.live = true;
        true
    }

    #[must_use]
    pub const fn live(&self) -> bool {
        self.live
    }

    /// A new capture: the next non-zero buffer is an edge again.
    pub fn reset(&mut self) {
        self.live = false;
    }
}

/// Drive one future to completion on **this** thread, giving up at
/// `deadline`.
///
/// For standalone callers and tests without a reactor. The audio engine
/// polls final transcripts on its regular tick so capture remains available.
/// This helper uses a waker that unparks this thread and a bounded wait.
/// A deadline is a failure, never permission to send a partial.
///
/// Returns `None` if the deadline passed first — the future is dropped,
/// which is what cancels the underlying work.
pub fn block_on_until<T>(
    mut future: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + '_>>,
    deadline: std::time::Instant,
) -> Option<T> {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return Some(value);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        // A **cadence**, not just a deadline. The Apple engines' final
        // result arrives through a `crossbeam` channel, which has no waker
        // integration, so `FinalResult` returns `Pending` without arranging
        // to be woken. Parking for the whole remaining time would turn
        // a generous timeout into a delay on every successful decode. A future that
        // *does* wake is still served the instant it does.
        const SLICE: std::time::Duration = std::time::Duration::from_millis(5);
        std::thread::park_timeout((deadline - now).min(SLICE));
    }
}

/// [`block_on_until`] with no deadline, for a future that is known to be
/// already resolved (the fakes' `finish`, a stub engine's).
pub fn block_on<T>(
    future: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + '_>>,
) -> T {
    // Far enough out that a hang is a hang and not a flake, and still a
    // number rather than a `loop`: a test that wedges here should fail, not
    // hold the gate open for ever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    block_on_until(future, deadline).expect("the future was expected to complete")
}

/// What `deckd run` needs to start the engine.
#[derive(Clone)]
pub struct AudioConfig {
    /// `[voice] transport`: the deck's BLE audio service (the default) or
    /// the CoreAudio devices of a Classic deck. Read here and nowhere else,
    /// which is why the key needs a restart.
    pub transport: crate::config::VoiceTransport,
    /// `[voice] device_uid` — the deck's UID pair, without a side suffix.
    /// `None` discovers it (§7's rule, and the name matters: see
    /// [`crate::audio::coreaudio::discover_deck`]). CoreAudio only.
    pub device_uid: Option<String>,
    pub fallback_output: String,
    pub fallback_input: String,
    /// The deck's voice link, when the daemon made one before the hid
    /// thread (which drives its cable port); `None` spawns one here, with
    /// its Bluetooth port only. BLE transport only.
    pub link: Option<crate::ble::BleAudio>,
    /// `[voice.tts] engine` at start: `"mlx"` builds the local model's
    /// sidecar ([`sidecar`]) with Apple's as the fallback, and is read once
    /// ([`crate::config::SIDECAR_KEYS`]); anything else builds the cloud
    /// engine ([`cloud`]) over Apple's, which reads `[voice.tts]` live from
    /// `cloud` below and so honours `"cloud"` and `"apple"` as they change.
    pub tts_engine: String,
    /// What the sidecar is built from when `tts_engine` names it.
    pub sidecar: Option<sidecar::SidecarConfig>,
    /// The cell the core refreshes with `[voice.tts]` on every reload, for
    /// the cloud engine. `None` leaves Apple's alone.
    pub cloud: Option<std::sync::Arc<arc_swap::ArcSwap<cloud::Settings>>>,
    /// The recogniser to install instead of Apple's own: the core builds
    /// the local-model engine ([`whisper`]) over Apple's and keeps a handle
    /// for `stt.status`. `None` is Apple's.
    pub stt: Option<std::sync::Arc<dyn traits::SttEngine>>,
    /// The synthesiser under the cloud engine (and under the sidecar): the
    /// core builds the local voice ([`kokoro`]) over Apple's and keeps a
    /// handle for `tts.status`. `None` is Apple's.
    pub local_tts: Option<std::sync::Arc<dyn traits::TtsEngine>>,
}

impl std::fmt::Debug for AudioConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioConfig")
            .field("transport", &self.transport)
            .field("device_uid", &self.device_uid)
            .field("fallback_output", &self.fallback_output)
            .field("fallback_input", &self.fallback_input)
            .field("tts_engine", &self.tts_engine)
            .field("sidecar", &self.sidecar)
            .field("stt", &self.stt.as_ref().map(|e| e.info().name))
            .field("local_tts", &self.local_tts.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            transport: crate::config::VoiceTransport::default(),
            device_uid: None,
            fallback_output: "none".into(),
            fallback_input: "system_default".into(),
            link: None,
            tts_engine: "apple".into(),
            sidecar: None,
            cloud: None,
            stt: None,
            local_tts: None,
        }
    }
}

/// What `voice.engines` reports as the synthesiser (§4.9): computed from
/// `[voice.tts]` and Apple's own report, rather than cached from one read
/// at start-up, because the answer changes with the key and with the
/// disk — `openai`/`elevenlabs` (not on device) while a key is set,
/// `kokoro` (on device) once its package is downloaded, `mlx` (on device)
/// for the sidecar, else Apple's as it reported itself.
#[must_use]
pub fn tts_info(tts: &crate::config::Tts, kokoro_present: bool, apple: &traits::EngineInfo) -> traits::EngineInfo {
    match tts.effective_engine() {
        sidecar::NAME => traits::EngineInfo {
            name: sidecar::NAME.into(),
            on_device: true,
            locales: sidecar::LOCALES.iter().map(|l| (*l).to_string()).collect(),
        },
        crate::kokoro::ENGINE if kokoro_present => traits::EngineInfo {
            name: crate::kokoro::ENGINE.into(),
            on_device: true,
            locales: KOKORO_LOCALES.iter().map(|l| (*l).to_string()).collect(),
        },
        "apple" | crate::kokoro::ENGINE => apple.clone(),
        _ => cloud::Settings::from_tts(tts)
            .engine_info()
            .unwrap_or_else(|| apple.clone()),
    }
}

/// What the Kokoro v1.1 package speaks.
pub const KOKORO_LOCALES: [&str; 3] = ["en-US", "en-GB", "zh-CN"];

/// The sidecar's configuration from `[voice.tts]` and the daemon's paths:
/// the Python of the environment `deckd tts install` makes under the
/// caches directory (or `[voice.tts] python`), and the script beside it.
#[must_use]
pub fn sidecar_config(config: &crate::config::Config, paths: &crate::config::Paths) -> sidecar::SidecarConfig {
    let tts = &config.voice.tts;
    sidecar::SidecarConfig {
        python: tts
            .python
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map_or_else(|| paths.tts.join("venv").join("bin").join("python"), std::path::PathBuf::from),
        script: paths.tts.join("sidecar.py"),
        // A Hugging Face id has a slash; anything else in `model` belongs
        // to another engine (a Kokoro package, a provider's model name).
        model: if tts.model.contains('/') {
            tts.model.clone()
        } else {
            sidecar::DEFAULT_MODEL.to_string()
        },
        voices: tts.voices.clone(),
        lang: if tts.lang.trim().is_empty() {
            "auto".to_string()
        } else {
            tts.lang.clone()
        },
        instruct: tts.instruct.clone().filter(|i| !i.trim().is_empty()),
        interval_s: tts.interval_s,
    }
}

/// Start the whole engine: `deckd-audio`, `deckd-tts`, the devices the
/// transport names, and the Apple engines behind §7.6's seams.
///
/// * `transport = "ble"`: the deck's link ([`crate::ble::BleAudio::spawn`],
///   which only ever attaches to a deck the system already holds), its
///   microphone and speaker ([`ble::BleCapture`], [`ble::BleOutput`]) and
///   `deckd-ble-link` turning link states into route events. No CoreAudio
///   discovery and no property watcher: nothing about this transport is a
///   CoreAudio device. The Mac's own input stays the `fallback_input`.
/// * `transport = "coreaudio"`: exactly what this did before the BLE
///   transport existed.
///
/// The handle it returns is the same one the core already knows how to use
/// ([`AudioHandle`]), so installing an engine is one `set_audio` and no
/// change to the core at all — which is what the seam was for.
#[cfg(target_os = "macos")]
#[must_use]
pub fn spawn(cfg: &AudioConfig) -> AudioHandle {
    use crate::config::VoiceTransport;
    use std::sync::Arc;

    let (cmds_tx, cmds_rx) = crossbeam_channel::bounded::<AudioCmd>(CMDS_CAPACITY);
    let (events_tx, events_rx) = crossbeam_channel::unbounded::<FromAudio>();
    let (routes_tx, routes_rx) = crossbeam_channel::unbounded::<traits::RouteEvent>();
    let (reports_tx, reports_rx) = crossbeam_channel::unbounded::<tts::TtsReport>();

    let lanes = lanes::Lanes::new();
    // The synthesiser: Apple's underneath always, and over it either the
    // local model's sidecar (whatever it cannot speak — not installed,
    // still loading, died — falls through) or the cloud engine, which is a
    // pass-through to Apple's until `[voice.tts]` holds a key. `voice.engines`
    // names the one in force from the config ([`tts_info`]), so the report
    // posted below is Apple's own.
    // The local voice (Kokoro over Apple's), when the core built one, is
    // the base everything else falls through to.
    let apple_tts: Arc<dyn traits::TtsEngine> = Arc::new(apple::AppleTts::new());
    let base: Arc<dyn traits::TtsEngine> = cfg.local_tts.clone().unwrap_or_else(|| apple_tts.clone());
    let tts_engine: Arc<dyn traits::TtsEngine> = match (cfg.tts_engine.as_str(), cfg.sidecar.clone(), cfg.cloud.clone()) {
        (sidecar::NAME, Some(sidecar_cfg), _) => {
            // This build's script, where the sidecar runs it from.
            if let Err(why) = sidecar::write_script(&sidecar_cfg.script) {
                tracing::warn!(why, "mlx tts: cannot write the sidecar script");
            }
            Arc::new(sidecar::SidecarTts::new(sidecar_cfg, Some(base)))
        }
        (_, _, Some(settings)) => Arc::new(cloud::CloudTts::new(settings, base)),
        (other, _, None) => {
            if other != "apple" && other != crate::kokoro::ENGINE {
                tracing::warn!(engine = other, "[voice.tts] engine: no cloud settings; Apple's speaks");
            }
            base
        }
    };
    let tts = tts::spawn(tts_engine, lanes.clone(), reports_tx);
    let mut engine = match cfg.transport {
        VoiceTransport::Ble => ble_engine(cfg, lanes, &events_tx, &routes_tx),
        VoiceTransport::Coreaudio => coreaudio_engine(cfg, lanes, &events_tx, &routes_tx),
    };
    engine.set_stt(cfg.stt.clone().unwrap_or_else(|| Arc::new(apple::AppleStt::new())));
    engine.set_tts(tts);

    // §4.9's `voice.engines`, read once and pushed (M5). `apple::engines()`
    // enumerates the recogniser's locales and the installed voices, which
    // are framework reads that belong on `main` — so it is **posted** and
    // runs on the first turn of B10's pump, rather than called here where
    // it would sit in front of `deckd run` for as long as the frameworks
    // take. Nothing waits for it: until it lands the core answers
    // `voice.engines` `-32013`, which is also the honest answer while an
    // engine is still coming up.
    let engines_tx = events_tx.clone();
    mainloop::post(move || {
        let (stt, tts) = apple::engines();
        let _ = engines_tx.send(FromAudio::Engines { stt, tts });
    });

    let join = std::thread::Builder::new()
        .name("deckd-audio".into())
        .spawn(move || engine::run(engine, &cmds_rx, &routes_rx, &reports_rx))
        .ok();
    AudioHandle {
        cmds: Some(cmds_tx),
        events: events_rx,
        _idle: events_tx,
        join,
        transport: Some(cfg.transport),
    }
}

/// The engine around the deck's BLE audio service.
#[cfg(target_os = "macos")]
fn ble_engine(
    cfg: &AudioConfig,
    lanes: std::sync::Arc<lanes::Lanes>,
    events_tx: &Sender<FromAudio>,
    routes_tx: &Sender<traits::RouteEvent>,
) -> engine::Engine {
    let link = cfg
        .link
        .clone()
        .unwrap_or_else(crate::ble::BleAudio::spawn);
    let mut engine = engine::Engine::new(
        lanes,
        Box::new(ble::BleCapture::new(
            link.clone(),
            Box::new(coreaudio::CpalCapture::new()),
        )),
        Box::new(ble::BleOutput::new(link.clone())),
        events_tx.clone(),
    );
    engine.set_transport(crate::config::VoiceTransport::Ble);
    engine.on_cmd(
        AudioCmd::Devices {
            device_uid: cfg.device_uid.clone(),
            fallback_output: cfg.fallback_output.clone(),
            fallback_input: cfg.fallback_input.clone(),
        },
        std::time::Instant::now(),
    );
    // The link's states become the route's events. The thread holds a
    // clone of the link; when the control loop is gone it notices on its
    // next idle tick, exits and lets go of the deck with it.
    let _ = ble::watch(link, routes_tx.clone(), events_tx.clone());
    engine
}

/// The engine around a Classic deck's CoreAudio devices: the pre-BLE
/// engine, unchanged.
#[cfg(target_os = "macos")]
fn coreaudio_engine(
    cfg: &AudioConfig,
    lanes: std::sync::Arc<lanes::Lanes>,
    events_tx: &Sender<FromAudio>,
    routes_tx: &Sender<traits::RouteEvent>,
) -> engine::Engine {
    let uid = cfg.device_uid.clone().or_else(coreaudio::discover_deck);
    let mut engine = engine::Engine::new(
        lanes,
        Box::new(coreaudio::CpalCapture::new()),
        Box::new(coreaudio::CpalOutput::new(uid.clone())),
        events_tx.clone(),
    );
    engine.set_transport(crate::config::VoiceTransport::Coreaudio);
    engine.on_cmd(
        AudioCmd::Devices {
            device_uid: uid.clone(),
            fallback_output: cfg.fallback_output.clone(),
            fallback_input: cfg.fallback_input.clone(),
        },
        std::time::Instant::now(),
    );

    // The property watcher is its own thread and not part of the control
    // loop, for one reason: it has to **re-register**. A listener can only
    // be attached to an `AudioObjectID` that exists, the deck's ids move on
    // every connection (V8: 101/95 then, 99/93 at the probes), and the
    // whole point of watching is to notice the connection that creates
    // them. So it enumerates on a slow timer while nothing is attached.
    if let Some(uid) = uid {
        let routes = routes_tx.clone();
        let _ = std::thread::Builder::new()
            .name("deckd-audio-watch".into())
            .spawn(move || {
                let mut watcher = coreaudio::DeviceWatcher::new();
                loop {
                    if watcher.watching() < 2 {
                        let mut fresh = coreaudio::DeviceWatcher::new();
                        let input = fresh.watch(&uid, traits::DeviceSide::Input, routes.clone());
                        let output = fresh.watch(&uid, traits::DeviceSide::Output, routes.clone());
                        if input || output {
                            // Replacing it removes the old listeners first.
                            watcher = fresh;
                        }
                    }
                    if routes.is_empty() && routes.send(traits::RouteEvent::Tick).is_err() {
                        return; // the control loop is gone
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            });
    }
    engine
}

/// Capacity of the core → audio channel.
///
/// Small on purpose: the queue holds work the audio thread has not started,
/// and every item in it is already late. A hundred queued announcements are
/// not better than a counter that says a hundred were dropped.
pub const CMDS_CAPACITY: usize = 64;

/// How many `Stop`/`Finish` commands wait for the next tick before even
/// those are dropped. They are retried because losing one strands a live
/// capture — the mic stays open and the 60 s cap is the only thing that
/// closes it — but the queue is not allowed to grow without bound either.
pub const RETRY_CAPACITY: usize = 32;

/// What the audio side sends the core, the way [`crate::hid::FromHid`] is
/// what the hid thread sends it.
#[derive(Clone, Debug, PartialEq)]
pub enum FromAudio {
    /// One report for the reducer (§7): a partial, a final, a route move, a
    /// level, the TCC state, an error.
    Voice(VoiceIn),
    /// What this build's STT and TTS engines are (§4.9's `voice.engines`),
    /// reported **once**, when they come up.
    ///
    /// It is pushed rather than asked for because reading it is a
    /// main-thread hop: `SFSpeechRecognizer::supportedLocales` and
    /// `AVSpeechSynthesisVoice::speechVoices` are framework reads that
    /// belong on `main` (V13), and the core answering `voice.engines` may
    /// never park on the run loop — it is the thread every agent, every
    /// hook and every device line goes through. So [`spawn`] posts the read
    /// to main, the answer arrives here like any other report, and the core
    /// caches it. Same shape as week 5's `FromDesktop::Automation`, for the
    /// same reason.
    Engines {
        stt: traits::EngineInfo,
        tts: traits::EngineInfo,
    },
}

impl FromAudio {
    /// The `VoiceIn` of a report, for a caller that only wants those.
    #[must_use]
    pub fn voice(self) -> Option<VoiceIn> {
        match self {
            FromAudio::Voice(v) => Some(v),
            FromAudio::Engines { .. } => None,
        }
    }
}

/// The core's handle on the audio side.
pub struct AudioHandle {
    /// Bounded; `try_send` only. `None` when this build has no engine.
    cmds: Option<Sender<AudioCmd>>,
    /// Unbounded: voice edges are rate-limited by speech, not by us.
    pub events: Receiver<FromAudio>,
    /// A sender kept alive for the handle's whole life so `events` can
    /// never become `Disconnected`.
    ///
    /// Without it, a `select!` arm over a receiver whose last sender was
    /// dropped is **always ready** and the core loop spins at 100% CPU. The
    /// hid loop can afford to `break` on that, because a daemon with no hid
    /// thread has nothing to do; a daemon with no audio thread has
    /// everything else to do.
    _idle: Sender<FromAudio>,
    join: Option<std::thread::JoinHandle<()>>,
    /// The transport [`spawn`] built the engine around; `None` for a
    /// handle with no engine of its own (`none`, `detached`).
    transport: Option<crate::config::VoiceTransport>,
}

impl std::fmt::Debug for AudioHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioHandle")
            .field("engine", &self.engine())
            .field("queued", &self.queued())
            .finish()
    }
}

impl AudioHandle {
    /// No engine: every command is logged and forgotten, which is what this
    /// build does and what `--no-device` will always do.
    #[must_use]
    pub fn none() -> Self {
        let (idle, events) = crossbeam_channel::unbounded::<FromAudio>();
        AudioHandle {
            cmds: None,
            events,
            _idle: idle,
            join: None,
            transport: None,
        }
    }

    /// A handle with no thread behind it: the caller holds the other ends
    /// and sees every command the core sends, and can feed it voice events.
    /// The bounded capacity is the real one, so a test can fill it.
    #[must_use]
    pub fn detached() -> (Self, Receiver<AudioCmd>, Sender<FromAudio>) {
        let (cmds_tx, cmds_rx) = crossbeam_channel::bounded::<AudioCmd>(CMDS_CAPACITY);
        let (events_tx, events_rx) = crossbeam_channel::unbounded::<FromAudio>();
        (
            AudioHandle {
                cmds: Some(cmds_tx),
                events: events_rx,
                _idle: events_tx.clone(),
                join: None,
                transport: None,
            },
            cmds_rx,
            events_tx,
        )
    }

    /// Whether anything is listening on the command side.
    #[must_use]
    pub fn engine(&self) -> bool {
        self.cmds.is_some()
    }

    /// The transport the running engine was built around, which is what
    /// `[voice] transport` said at start-up rather than what it says now
    /// (the key needs a restart). `None` without an engine of its own.
    #[must_use]
    pub fn transport(&self) -> Option<crate::config::VoiceTransport> {
        self.transport
    }

    /// Commands the audio side has not taken yet.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.cmds.as_ref().map_or(0, crossbeam_channel::Sender::len)
    }

    /// `try_send`, never `send`: `None` when the audio side took it, and
    /// **the command back** when it did not, so the caller can apply its
    /// own per-command policy. A disconnected engine is a full one as far
    /// as that policy is concerned.
    ///
    /// It hands the value back rather than returning a `Result`, because
    /// there is nothing to propagate: a refused `AudioCmd` is a decision to
    /// make here and now, not an error to bubble.
    #[must_use]
    pub fn send_or_return(&self, cmd: AudioCmd) -> Option<AudioCmd> {
        match self.cmds.as_ref() {
            None => Some(cmd),
            Some(tx) => match tx.try_send(cmd) {
                Ok(()) => None,
                Err(TrySendError::Full(cmd) | TrySendError::Disconnected(cmd)) => Some(cmd),
            },
        }
    }

    /// Dropping the command sender ends the thread; join it.
    pub fn join(mut self) {
        drop(self.cmds.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Default for AudioHandle {
    fn default() -> Self {
        AudioHandle::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two states a handle can be in are genuinely different, and the
    /// difference is what keeps a daemon with no engine behaving exactly as
    /// it did before week 4.
    #[test]
    fn a_handle_with_no_engine_refuses_without_pretending_to_be_full() {
        let idle = AudioHandle::none();
        assert!(!idle.engine());
        assert_eq!(idle.queued(), 0);
        let chime = AudioCmd::Chime {
            which: "question".into(),
        };
        assert_eq!(idle.send_or_return(chime.clone()), Some(chime.clone()));

        let (audio, cmds, events) = AudioHandle::detached();
        assert!(audio.engine());
        for _ in 0..CMDS_CAPACITY {
            assert!(audio.send_or_return(chime.clone()).is_none());
        }
        assert_eq!(audio.queued(), CMDS_CAPACITY);
        assert_eq!(
            audio.send_or_return(chime.clone()),
            Some(chime.clone()),
            "bounded, and the refused command comes back"
        );
        assert_eq!(cmds.recv(), Ok(chime.clone()));
        assert!(
            audio.send_or_return(chime).is_none(),
            "one drained, one accepted"
        );
        // The event side is unbounded and never disconnects while the
        // handle lives, even with no thread on the other end.
        events
            .send(FromAudio::Voice(VoiceIn::Level { rms: 0.5 }))
            .unwrap();
        assert_eq!(
            audio.events.recv(),
            Ok(FromAudio::Voice(VoiceIn::Level { rms: 0.5 }))
        );
        // Dropping every outside sender must leave the receiver **empty**
        // and not **disconnected**: a disconnected `select!` arm is always
        // ready, and the core loop would spin at 100% CPU for the rest of
        // the daemon's life. The handle's own idle sender is what prevents
        // it, and this is the assertion that says so.
        drop(events);
        assert_eq!(
            audio.events.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        );
        assert_eq!(
            AudioHandle::none().events.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        );
    }
}
