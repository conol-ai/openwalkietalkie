//! The voice engine on Linux: the deck's BLE audio service (BlueZ) in, the
//! local Whisper model for recognition.
//!
//! Everything above the link is the engine macOS runs ([`engine`], [`lanes`],
//! [`ble`]); what is different is what stands where Apple's frameworks do.
//! There is no recogniser to fall back on (Whisper is the engine, and until
//! its model is on disk a press says so), no local microphone to fall back
//! to (the deck's is the only one), and no system synthesiser: replies are
//! spoken by a program the operator has installed ([`super::command`],
//! `espeak-ng` unless `[voice.tts] command` says otherwise), and [`NoTts`] is
//! what stands there when none is wired.
use super::traits::{
    AudioError, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, EngineInfo, OnDevice, SttEngine,
    SttSession, SttStart, TtsEngine, TtsJob, TtsRequest, VoiceInfo,
};
use super::{ble, engine, lanes, tts, AudioCmd, AudioConfig, AudioHandle, FromAudio, CMDS_CAPACITY};
use crossbeam_channel::{bounded, unbounded};
use std::sync::Arc;

/// The deck is the only microphone: `voice.listen{input:"mac"}` has
/// nothing to open on Linux.
pub struct NoCapture;

impl CaptureSource for NoCapture {
    fn open(&mut self, _: &CaptureOpen, _: CaptureSink) -> Result<CaptureOpened, AudioError> {
        Err(AudioError::NoDevice("the deck's microphone is the only input on Linux".into()))
    }

    fn close(&mut self) {}

    fn is_open(&self) -> bool {
        false
    }
}

/// What Whisper falls through to while its model is not on disk.
pub struct NoStt;

impl SttEngine for NoStt {
    fn info(&self) -> EngineInfo {
        EngineInfo { name: "none".into(), on_device: false, locales: Vec::new() }
    }

    fn on_device(&self, _: &str) -> OnDevice {
        OnDevice::Unsupported
    }

    fn start(&self, _: &SttStart) -> Result<Box<dyn SttSession>, AudioError> {
        Err(AudioError::NoEngine("recogniser (install the Whisper model: `deckd stt ...`)"))
    }
}

/// No synthesiser: the placeholder when none is wired.
pub struct NoTts;

impl TtsEngine for NoTts {
    fn info(&self) -> EngineInfo {
        EngineInfo { name: "none".into(), on_device: false, locales: Vec::new() }
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        Vec::new()
    }

    fn synthesize(&self, _: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        Err(AudioError::NoEngine("synthesiser"))
    }
}

/// The engine, the deck's link and Whisper, on their threads. Same handle as
/// macOS returns, so the core installs it with the same `set_audio`.
#[must_use]
pub fn spawn(cfg: &AudioConfig) -> AudioHandle {
    let (cmds_tx, cmds_rx) = bounded::<AudioCmd>(CMDS_CAPACITY);
    let (events_tx, events_rx) = unbounded::<FromAudio>();
    let (routes_tx, routes_rx) = unbounded::<super::traits::RouteEvent>();
    let (reports_tx, reports_rx) = unbounded::<tts::TtsReport>();

    let lanes = lanes::Lanes::new();
    let synth: Arc<dyn TtsEngine> = cfg.local_tts.clone().unwrap_or_else(|| Arc::new(NoTts));
    let tts_info = synth.info();
    let speaker = tts::spawn(synth, lanes.clone(), reports_tx);

    let link = cfg.link.clone().unwrap_or_else(crate::ble::BleAudio::spawn);
    let mut engine = engine::Engine::new(
        lanes,
        Box::new(ble::BleCapture::new(link.clone(), Box::new(NoCapture))),
        Box::new(ble::BleOutput::new(link.clone())),
        events_tx.clone(),
    );
    engine.set_transport(crate::config::VoiceTransport::Ble);
    engine.on_cmd(
        AudioCmd::Devices {
            device_uid: cfg.device_uid.clone(),
            fallback_output: cfg.fallback_output.clone(),
        },
        std::time::Instant::now(),
    );
    let _ = ble::watch(link, routes_tx.clone(), events_tx.clone());

    let stt: Arc<dyn SttEngine> = cfg.stt.clone().unwrap_or_else(|| Arc::new(NoStt));
    let stt_info = stt.info();
    engine.set_stt(stt);
    if let Some(hub) = cfg.companion.clone() {
        engine.set_companion(hub);
    }
    engine.set_tts(speaker);
    let _ = events_tx.send(FromAudio::Engines { stt: stt_info, tts: tts_info });

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
