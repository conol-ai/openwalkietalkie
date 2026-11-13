//! The fakes behind [`super::traits`], **co-designed with the real ones**
//! (HOST-CONTRACT-W4 §9) — because these are what CI runs.
//!
//! V15's verdict is that every week-4 exit test is reachable with the real
//! engines on this machine. That is not an argument for testing with them:
//! a row that needs a Bluetooth headset, a TCC grant and a second of SCO
//! bring-up is a row that runs once a week and is skipped the rest of the
//! time. The deterministic rows run here, against these, on a clock the test
//! owns; the real engines have the `--hardware` path and nothing else.
//!
//! What they are shaped by, so they can fail the way the hardware does:
//!
//! * **Silence is not absence (V2).** [`FakeCapture`] delivers buffers of
//!   exact digital zero by default, because that is what the deck does for
//!   the first ~1 s of every press: the callback starts at ~28 ms,
//!   `AudioUnitRender` returns `noErr`, and every sample is zero until
//!   1031 ms (median, n=16). A fake that delivered a sine on the first
//!   callback would let a dead-mic check that keys off "did a buffer
//!   arrive?" pass, which is precisely the bug V2 exists to prevent.
//! * **Errors arrive mid-session (V16).** [`FakeStt`] can push an error
//!   between two partials, not only at `finish()`.
//! * **The TTS format is not 44.1 k (V16).** [`FakeTts`] defaults to
//!   22050/1, AVSpeech's measured format for Samantha, so anything that
//!   forgot to resample is audibly and testably wrong rather than
//!   accidentally right.
//! * **A refusal is a first-class outcome.** Each fake can be told to refuse,
//!   because §7.8's rule — a denied grant is silence, not an error — has to
//!   be exercised without touching a TCC grant.
use super::traits::{
    AudioError, AudioFormat, BoxFuture, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource,
    EngineInfo, Fault, OnDevice, SttEngine, SttEvent, SttSession, SttStart, TtsChunk, TtsEngine,
    TtsJob, TtsRequest, TtsStop, VoiceInfo,
};
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// What a test does to a [`FakeCapture`] after handing it over.
#[derive(Clone, Debug, Default)]
pub struct FakeCaptureHandle {
    inner: Arc<CaptureShared>,
}

#[derive(Debug, Default)]
struct CaptureShared {
    opens: Mutex<Vec<CaptureOpen>>,
    closes: AtomicUsize,
    open_now: AtomicBool,
    refuse: Mutex<Option<AudioError>>,
    rate: AtomicUsize,
    sink: Mutex<Option<CaptureSink>>,
}

impl FakeCaptureHandle {
    /// Every open the engine asked for, in order — so a test can assert
    /// §15 row 3's "the fake capture was never opened".
    #[must_use]
    pub fn opens(&self) -> Vec<CaptureOpen> {
        self.inner.opens.lock().unwrap().clone()
    }

    #[must_use]
    pub fn closes(&self) -> usize {
        self.inner.closes.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.inner.open_now.load(Ordering::SeqCst)
    }

    /// Make the next open fail (TCC denial, the deck unpaired).
    pub fn refuse_with(&self, error: AudioError) {
        *self.inner.refuse.lock().unwrap() = Some(error);
    }

    /// The rate the fake device reports. 16000 by default: V8 measured
    /// `AvailableNominalSampleRates == {16000}` on the deck's input, so
    /// there is no 8 kHz CVSD path to fake on this hardware.
    pub fn set_rate(&self, rate: u32) {
        self.inner.rate.store(rate as usize, Ordering::SeqCst);
    }

    /// One callback of **digital silence** — V2's first second.
    pub fn deliver_silence(&self, frames: usize) {
        self.deliver(&vec![0.0; frames]);
    }

    /// One callback of real audio.
    pub fn deliver(&self, samples: &[f32]) {
        let rate = self.rate();
        if let Some(sink) = self.inner.sink.lock().unwrap().as_mut() {
            sink.deliver(rate, samples);
        }
    }

    /// Raise a fault the way cpal's error callback would (V19).
    pub fn fault(&self, fault: Fault) {
        if let Some(sink) = self.inner.sink.lock().unwrap().as_ref() {
            sink.fault(fault);
        }
    }

    #[must_use]
    pub fn rate(&self) -> u32 {
        let stored = self.inner.rate.load(Ordering::SeqCst);
        if stored == 0 {
            16_000
        } else {
            u32::try_from(stored).unwrap_or(16_000)
        }
    }
}

/// A microphone that is entirely a test's to drive.
#[derive(Debug, Default)]
pub struct FakeCapture {
    inner: Arc<CaptureShared>,
}

impl FakeCapture {
    #[must_use]
    pub fn new() -> (Self, FakeCaptureHandle) {
        let capture = FakeCapture::default();
        let handle = FakeCaptureHandle {
            inner: capture.inner.clone(),
        };
        (capture, handle)
    }
}

impl CaptureSource for FakeCapture {
    fn open(&mut self, req: &CaptureOpen, sink: CaptureSink) -> Result<CaptureOpened, AudioError> {
        self.inner.opens.lock().unwrap().push(req.clone());
        if let Some(error) = self.inner.refuse.lock().unwrap().take() {
            return Err(error);
        }
        let rate = {
            let stored = self.inner.rate.load(Ordering::SeqCst);
            if stored == 0 {
                16_000
            } else {
                u32::try_from(stored).unwrap_or(16_000)
            }
        };
        *self.inner.sink.lock().unwrap() = Some(sink);
        self.inner.open_now.store(true, Ordering::SeqCst);
        Ok(CaptureOpened {
            rate,
            channels: 1,
            uid: req.device_uid.clone().map(|u| format!("{u}:input")),
        })
    }

    fn close(&mut self) {
        // A second close is normal: §3 retries a refused `Finish`.
        if self.inner.open_now.swap(false, Ordering::SeqCst) {
            *self.inner.sink.lock().unwrap() = None;
        }
        self.inner.closes.fetch_add(1, Ordering::SeqCst);
    }

    fn is_open(&self) -> bool {
        self.inner.open_now.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// STT
// ---------------------------------------------------------------------------

/// What a test does to the session a [`FakeStt`] handed out.
#[derive(Clone, Debug)]
pub struct FakeSttHandle {
    inner: Arc<SttShared>,
}

#[derive(Debug, Default)]
struct SttShared {
    starts: Mutex<Vec<SttStart>>,
    fed: Mutex<Vec<(u32, usize)>>,
    events: Mutex<Option<Sender<SttEvent>>>,
    finals: Mutex<Vec<Result<String, AudioError>>>,
    cancelled: AtomicUsize,
    finished: AtomicUsize,
    server_only: Mutex<Vec<String>>,
    /// A zero-budget pending future exercises failure without a real wait.
    never_finishes: AtomicBool,
    finish_delay: Mutex<std::time::Duration>,
}

impl FakeSttHandle {
    /// Every `start` the engine made, with the locale and the contextual
    /// strings B3 threaded through.
    #[must_use]
    pub fn starts(&self) -> Vec<SttStart> {
        self.inner.starts.lock().unwrap().clone()
    }

    /// `(rate, samples)` of every `feed`, so a test can prove the capture
    /// reached the recogniser at the device's own rate.
    #[must_use]
    pub fn fed(&self) -> Vec<(u32, usize)> {
        self.inner.fed.lock().unwrap().clone()
    }

    #[must_use]
    pub fn cancelled(&self) -> usize {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn finished(&self) -> usize {
        self.inner.finished.load(Ordering::SeqCst)
    }

    /// Push one event into the live session. Mid-session errors included —
    /// that is V16's whole point.
    pub fn say(&self, event: SttEvent) -> bool {
        self.inner
            .events
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| tx.send(event).is_ok())
    }

    /// What the next `finish()` resolves to. Queued, so a test can script
    /// two sessions.
    pub fn finish_with(&self, result: Result<String, AudioError>) {
        self.inner.finals.lock().unwrap().push(result);
    }

    /// Make `finish()` hang. The caller's deadline is then the only thing
    /// that ends the session, without dispatching a partial.
    pub fn never_finishes(&self) {
        self.inner.never_finishes.store(true, Ordering::SeqCst);
    }

    pub fn finish_after(&self, delay: std::time::Duration, result: Result<String, AudioError>) {
        *self.inner.finish_delay.lock().unwrap() = delay;
        self.finish_with(result);
    }

    /// Locales this engine only has a *server* recogniser for (§7.6's
    /// `-32009`; zh-CN on this machine).
    pub fn server_only(&self, locale: &str) {
        self.inner
            .server_only
            .lock()
            .unwrap()
            .push(locale.to_string());
    }
}

/// A recogniser with no network, no assets and no microphone.
#[derive(Debug, Default)]
pub struct FakeStt {
    inner: Arc<SttShared>,
}

impl FakeStt {
    #[must_use]
    pub fn new() -> (Self, FakeSttHandle) {
        let engine = FakeStt::default();
        let handle = FakeSttHandle {
            inner: engine.inner.clone(),
        };
        (engine, handle)
    }
}

impl SttEngine for FakeStt {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "fake".into(),
            on_device: true,
            locales: vec!["en-US".into(), "zh-CN".into()],
        }
    }

    fn on_device(&self, locale: &str) -> OnDevice {
        if self
            .inner
            .server_only
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == locale)
        {
            OnDevice::ServerOnly
        } else if self.info().locales.iter().any(|l| l == locale) {
            OnDevice::Yes
        } else {
            OnDevice::Unsupported
        }
    }

    fn start(&self, req: &SttStart) -> Result<Box<dyn SttSession>, AudioError> {
        self.inner.starts.lock().unwrap().push(req.clone());
        if !req.allow_server && self.on_device(&req.locale) != OnDevice::Yes {
            return Err(AudioError::NoOnDevice(req.locale.clone()));
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        *self.inner.events.lock().unwrap() = Some(tx);
        Ok(Box::new(FakeSttSession {
            inner: self.inner.clone(),
            events: rx,
        }))
    }
}

struct FakeSttSession {
    inner: Arc<SttShared>,
    events: Receiver<SttEvent>,
}

impl SttSession for FakeSttSession {
    fn feed(&mut self, mono_f32: &[f32], rate: u32) {
        self.inner.fed.lock().unwrap().push((rate, mono_f32.len()));
    }

    fn events(&self) -> Receiver<SttEvent> {
        self.events.clone()
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>> {
        self.inner.finished.fetch_add(1, Ordering::SeqCst);
        if self.inner.never_finishes.load(Ordering::SeqCst) {
            return Box::pin(std::future::pending());
        }
        let mut queued = self.inner.finals.lock().unwrap();
        let result = if queued.is_empty() {
            // The ordinary empty final of §7.4: nothing was said. Not an
            // error, and the reducer answers it with
            // `voice.state{ended, empty:true}`.
            Ok(String::new())
        } else {
            queued.remove(0)
        };
        drop(queued);
        let ready = std::time::Instant::now() + *self.inner.finish_delay.lock().unwrap();
        let mut result = Some(result);
        Box::pin(std::future::poll_fn(move |_| {
            if std::time::Instant::now() < ready { std::task::Poll::Pending }
            else { std::task::Poll::Ready(result.take().unwrap()) }
        }))
    }

    fn finish_budget(&self) -> std::time::Duration {
        if self.inner.never_finishes.load(Ordering::SeqCst) { std::time::Duration::ZERO }
        else { super::traits::FINISH_BUDGET }
    }

    fn cancel(self: Box<Self>) {
        self.inner.cancelled.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------

/// What a test does to a [`FakeTts`].
#[derive(Clone, Debug)]
pub struct FakeTtsHandle {
    inner: Arc<TtsShared>,
}

#[derive(Debug)]
struct TtsShared {
    said: Mutex<Vec<TtsRequest>>,
    format: Mutex<AudioFormat>,
    /// Frames of PCM per character of text, so a test can make an utterance
    /// exactly as long as it needs.
    frames_per_char: AtomicUsize,
    refuse: Mutex<Option<AudioError>>,
    stops: Arc<AtomicUsize>,
}

impl Default for TtsShared {
    fn default() -> Self {
        TtsShared {
            said: Mutex::new(Vec::new()),
            // AVSpeech's measured format for Samantha (V16). Not 44.1 k, on
            // purpose: the resampler must be exercised by the default.
            format: Mutex::new(AudioFormat::new(22_050, 1)),
            frames_per_char: AtomicUsize::new(256),
            refuse: Mutex::new(None),
            stops: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl FakeTtsHandle {
    #[must_use]
    pub fn said(&self) -> Vec<TtsRequest> {
        self.inner.said.lock().unwrap().clone()
    }

    #[must_use]
    pub fn stops(&self) -> usize {
        self.inner.stops.load(Ordering::SeqCst)
    }

    /// Pretend to be a voice with another native rate (they vary per voice).
    pub fn set_format(&self, format: AudioFormat) {
        *self.inner.format.lock().unwrap() = format;
    }

    pub fn set_frames_per_char(&self, frames: usize) {
        self.inner.frames_per_char.store(frames, Ordering::SeqCst);
    }

    pub fn refuse_with(&self, error: AudioError) {
        *self.inner.refuse.lock().unwrap() = Some(error);
    }
}

/// A synthesiser that produces a deterministic ramp instead of speech.
///
/// A ramp rather than silence so a resampling bug shows up as a wrong
/// *shape*, and rather than a sine so the expected sample values can be
/// written down in a test without a trig table.
#[derive(Debug, Default)]
pub struct FakeTts {
    inner: Arc<TtsShared>,
}

impl FakeTts {
    #[must_use]
    pub fn new() -> (Self, FakeTtsHandle) {
        let engine = FakeTts::default();
        let handle = FakeTtsHandle {
            inner: engine.inner.clone(),
        };
        (engine, handle)
    }
}

impl TtsEngine for FakeTts {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "fake".into(),
            on_device: true,
            locales: vec!["en-US".into()],
        }
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        (1..=4)
            .map(|n| VoiceInfo {
                id: format!("fake.voice{n}"),
                name: format!("Voice {n}"),
                locale: "en-US".into(),
                enhanced: true,
            })
            .collect()
    }

    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        self.inner.said.lock().unwrap().push(req.clone());
        if let Some(error) = self.inner.refuse.lock().unwrap().take() {
            return Err(error);
        }
        let format = *self.inner.format.lock().unwrap();
        let frames = req.text.chars().count() * self.inner.frames_per_char.load(Ordering::SeqCst);
        let (tx, rx) = crossbeam_channel::unbounded();
        // Chunked the way AVSpeech delivers: several buffers, then a
        // zero-length one. 512 frames a chunk keeps the arithmetic obvious.
        let channels = usize::from(format.channels);
        let mut sent = 0usize;
        while sent < frames {
            let n = 512.min(frames - sent);
            let mut pcm = Vec::with_capacity(n * channels);
            for i in 0..n {
                #[allow(clippy::cast_precision_loss)]
                let v = ((sent + i) % 1000) as f32 / 1000.0;
                for _ in 0..channels {
                    pcm.push(v);
                }
            }
            let _ = tx.send(TtsChunk::Pcm(pcm));
            sent += n;
        }
        let _ = tx.send(TtsChunk::End);
        Ok(Box::new(FakeTtsJob {
            format,
            chunks: rx,
            stops: self.inner.stops.clone(),
        }))
    }
}

struct FakeTtsJob {
    format: AudioFormat,
    chunks: Receiver<TtsChunk>,
    stops: Arc<AtomicUsize>,
}

struct FakeStopper(Arc<AtomicUsize>);

impl TtsStop for FakeStopper {
    fn stop(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl TtsJob for FakeTtsJob {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn chunks(&self) -> Receiver<TtsChunk> {
        self.chunks.clone()
    }

    fn stopper(&self) -> Arc<dyn TtsStop> {
        Arc::new(FakeStopper(self.stops.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::VoiceInput;

    fn open() -> CaptureOpen {
        CaptureOpen {
            input: VoiceInput::Deck,
            device_uid: Some("C2-2D-81-AB-88-81".into()),
            ask_deck: None,
        }
    }

    /// The fake's default is V2's first second: real callbacks, real
    /// `noErr`, every sample an exact zero. A dead-mic check that keys off
    /// "a buffer arrived" passes against a dead mic, and this is the fake
    /// that makes that testable without one.
    #[test]
    fn the_fake_microphone_delivers_digital_silence_by_default() {
        let (mut capture, handle) = FakeCapture::new();
        let got = Arc::new(Mutex::new(Vec::<(u32, Vec<f32>)>::new()));
        let seen = got.clone();
        let faults = Arc::new(super::super::traits::StreamFaults::new());
        let sink = CaptureSink::new(faults.clone(), move |rate, samples| {
            seen.lock().unwrap().push((rate, samples.to_vec()));
        });
        let opened = capture.open(&open(), sink).unwrap();
        assert_eq!(opened.rate, 16_000, "V8: the deck's input offers {{16000}}");
        assert_eq!(opened.uid.as_deref(), Some("C2-2D-81-AB-88-81:input"));

        handle.deliver_silence(160);
        handle.deliver_silence(160);
        let buffers = got.lock().unwrap();
        assert_eq!(buffers.len(), 2, "two callbacks arrived");
        assert!(
            buffers.iter().all(|(_, s)| s.iter().all(|v| *v == 0.0)),
            "and every sample of them is a hard zero"
        );
        drop(buffers);

        handle.deliver(&[0.5; 160]);
        assert!(got.lock().unwrap()[2].1.iter().all(|v| *v == 0.5));

        // The error callback's path, raised from where cpal would raise it.
        handle.fault(Fault::Xrun);
        assert_eq!(faults.take(), vec![Fault::Xrun]);

        capture.close();
        assert_eq!(handle.closes(), 1);
        assert!(!handle.is_open());
        capture.close();
        assert_eq!(handle.closes(), 2, "a second close is not an error");
        assert_eq!(handle.opens().len(), 1);
    }

    /// §7.6's `-32009` is answered **before** anything opens, and the engine
    /// refuses the start rather than failing halfway through it.
    #[test]
    fn a_server_only_locale_is_refused_at_start_unless_the_caller_allows_it() {
        let (engine, handle) = FakeStt::new();
        handle.server_only("zh-CN");
        assert_eq!(engine.on_device("en-US"), OnDevice::Yes);
        assert_eq!(engine.on_device("zh-CN"), OnDevice::ServerOnly);
        assert_eq!(engine.on_device("xx-XX"), OnDevice::Unsupported);

        let mut req = SttStart {
            locale: "zh-CN".into(),
            contextual: vec!["cohdl-api".into()],
            allow_server: false,
        };
        assert_eq!(
            engine.start(&req).err(),
            Some(AudioError::NoOnDevice("zh-CN".into()))
        );
        req.allow_server = true;
        assert!(engine.start(&req).is_ok(), "allow_server = true opens it");
        assert_eq!(handle.starts().len(), 2, "both attempts are recorded");
        assert_eq!(handle.starts()[0].contextual, vec!["cohdl-api".to_string()]);
    }

    /// V16's mid-session error: it arrives between two partials, not at
    /// `finish()`, and the ordering with the partials is preserved.
    #[test]
    fn an_stt_error_arrives_between_two_partials() {
        let (engine, handle) = FakeStt::new();
        let mut session = engine
            .start(&SttStart {
                locale: "en-US".into(),
                contextual: Vec::new(),
                allow_server: false,
            })
            .unwrap();
        let events = session.events();
        session.feed(&[0.0; 160], 16_000);
        assert_eq!(handle.fed(), vec![(16_000, 160)]);

        handle.say(SttEvent::Partial("allow".into()));
        handle.say(SttEvent::Error {
            code: "1110".into(),
            message: "kAFAssistantErrorDomain".into(),
        });
        handle.say(SttEvent::Partial("allow always".into()));
        let seen: Vec<SttEvent> = events.try_iter().collect();
        assert_eq!(
            seen,
            vec![
                SttEvent::Partial("allow".into()),
                SttEvent::Error {
                    code: "1110".into(),
                    message: "kAFAssistantErrorDomain".into()
                },
                SttEvent::Partial("allow always".into()),
            ]
        );
        handle.finish_with(Ok("allow always".into()));
        assert_eq!(handle.finished(), 0);
        let fut = session.finish();
        assert_eq!(handle.finished(), 1);
        assert_eq!(super::super::block_on(fut), Ok("allow always".to_string()));
    }

    /// The fake synthesiser is **not** at the lane rate, because AVSpeech is
    /// not (V16): anything that forgets to resample must come out wrong.
    #[test]
    fn the_fake_voice_is_22050_mono_like_samantha() {
        let (engine, handle) = FakeTts::new();
        handle.set_frames_per_char(600);
        let job = engine
            .synthesize(&TtsRequest {
                text: "ab".into(),
                voice: None,
                rate: 0.5,
            })
            .unwrap();
        assert_eq!(job.format(), AudioFormat::new(22_050, 1));
        assert_ne!(
            job.format(),
            AudioFormat::lane(),
            "a fake at the lane rate would hide every resampling bug"
        );
        let chunks: Vec<TtsChunk> = job.chunks().try_iter().collect();
        let frames: usize = chunks
            .iter()
            .map(|c| match c {
                TtsChunk::Pcm(p) => p.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(frames, 1200, "two chars × 600 frames");
        assert_eq!(
            chunks.last(),
            Some(&TtsChunk::End),
            "AVSpeech's zero buffer"
        );
        assert_eq!(handle.said()[0].rate, 0.5);

        let stopper = job.stopper();
        stopper.stop();
        stopper.stop();
        assert_eq!(handle.stops(), 2, "the stop handle is idempotent to call");
    }
}
