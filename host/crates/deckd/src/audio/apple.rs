//! The Apple engines behind §7.6's seams: `AVSpeechSynthesizer` and
//! `SFSpeechRecognizer`, both driven **from the main thread** (V13).
//!
//! Nothing in here is `Send`, and nothing in here crosses a thread. What
//! crosses is the handle: every ObjC object lives in a `thread_local`
//! registry on `main`, keyed by a job id, and every method call is a
//! [`crate::audio::mainloop::post`]. That is not a style choice —
//!
//! > V13: AVSpeech buffer callbacks are delivered on the MAIN THREAD ONLY.
//! > Worker thread with no run loop → 0 buffers. Worker driving its own run
//! > loop → 0 buffers. Main thread driving its run loop → 300/300 buffers.
//!
//! — and the same applies to `SFSpeechRecognizer`'s result handlers. A
//! design that put the objects on `deckd-tts` would compile, pass a review
//! and produce silence.
//!
//! **V14 is why this is testable at all without a bundle**: on-device STT
//! and TTS need **no TCC grant**. `SFSpeechRecognizer` with
//! `requiresOnDeviceRecognition = true` produced real partials and finals at
//! `authorizationStatus == notDetermined`, from a terminal *and* from a
//! launchd-spawned process with no Info.plist; `AVSpeechSynthesizer` needs
//! nothing at all. Neither of them is what §7.8's prompting flow is about,
//! and nothing here may be read as evidence about the daemon's TCC path
//! (V9).
use super::apple_transcript::{AppleTranscript, SpeechSpan};
use super::mainloop;
use super::traits::{
    AudioError, AudioFormat, BoxFuture, EngineInfo, OnDevice, SttEngine, SttEvent, SttSession,
    SttStart, TtsChunk, TtsEngine, TtsJob, TtsRequest, TtsStop, VoiceInfo,
};
use block2::RcBlock;
use crossbeam_channel::Receiver;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioCommonFormat, AVAudioFormat, AVAudioPCMBuffer, AVSpeechBoundary,
    AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceQuality, AVSpeechSynthesizer, AVSpeechUtterance,
};
use objc2_foundation::{NSArray, NSLocale, NSString};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognizer,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long a caller on `deckd-tts` waits for AVSpeech's **first** buffer,
/// which is when the format becomes known (V16).
const FORMAT_WAIT: Duration = Duration::from_secs(3);

/// How long a call that has to run on main waits for its answer before
/// giving up. Main is a run loop, not a queue that can be blocked for ever,
/// but a TCC prompt can sit on it — so this is generous and finite.
const MAIN_WAIT: Duration = Duration::from_secs(5);

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Run `f` on main and wait for its answer.
///
/// **Called from main, it runs `f` directly.** Posting and then waiting
/// would deadlock for `MAIN_WAIT` and then answer `None`: the queue drains
/// on main, and main is the thread doing the waiting. The daemon reaches
/// these engines from `deckd-audio` and `deckd-tts`, but `voice.engines`
/// can be answered from anywhere and a self-deadlock is not an acceptable
/// way to find that out.
fn on_main<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    if mainloop::is_main_thread() {
        return Some(f());
    }
    mainloop::call(f).recv_timeout(MAIN_WAIT).ok()
}

// ---------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------

thread_local! {
    /// Live synthesisers, on main. One per utterance: `AVSpeechSynthesizer`
    /// is cheap and a shared one would make "stop that utterance" mean
    /// "stop whatever is speaking".
    static SYNTHS: RefCell<HashMap<u64, Retained<AVSpeechSynthesizer>>> =
        RefCell::new(HashMap::new());
}

/// §7.6's v1 TTS engine.
#[derive(Debug, Default)]
pub struct AppleTts;

impl AppleTts {
    #[must_use]
    pub fn new() -> Self {
        AppleTts
    }
}

impl TtsEngine for AppleTts {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "apple".into(),
            on_device: true,
            locales: self
                .voices()
                .into_iter()
                .map(|v| v.locale)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        on_main(|| {
            // SAFETY: a read of the framework's own voice list, on main.
            let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
            voices
                .iter()
                .map(|voice| {
                    // SAFETY: each element is a live `AVSpeechSynthesisVoice`.
                    unsafe {
                        VoiceInfo {
                            id: voice.identifier().to_string(),
                            name: voice.name().to_string(),
                            locale: voice.language().to_string(),
                            // §7.7's `default_pool` wants four *enhanced*
                            // en-US voices; premium counts as enhanced or
                            // better, and Siri's are not exposed here at all.
                            enhanced: voice.quality() != AVSpeechSynthesisVoiceQuality::Default,
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        let id = next_id();
        let (chunks_tx, chunks) = crossbeam_channel::unbounded::<TtsChunk>();
        let (format_tx, format_rx) = crossbeam_channel::bounded::<AudioFormat>(1);
        let text = req.text.clone();
        let voice_id = req.voice.clone();
        let rate = req.rate;

        mainloop::post(move || {
            // SAFETY: every call below is on main, on objects created here.
            unsafe {
                let synth = AVSpeechSynthesizer::new();
                let utterance =
                    AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(&text));
                if rate > 0.0 {
                    utterance.setRate(rate);
                }
                if let Some(identifier) = voice_id.as_deref() {
                    if let Some(voice) =
                        AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(identifier))
                    {
                        utterance.setVoice(Some(&voice));
                    }
                }
                let sent_format = std::cell::Cell::new(false);
                let block = RcBlock::new(move |buffer: std::ptr::NonNull<AVAudioBuffer>| {
                    let buffer = buffer.as_ref();
                    let Some(pcm) = buffer.downcast_ref::<AVAudioPCMBuffer>() else {
                        return;
                    };
                    let format = pcm.format();
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss
                    )]
                    let ours = AudioFormat::new(
                        format.sampleRate().round() as u32,
                        format.channelCount() as u16,
                    );
                    if !sent_format.replace(true) {
                        // V16: the format is answered **before** any PCM is
                        // handed on, so the resampler exists before the
                        // audio does.
                        let _ = format_tx.try_send(ours);
                    }
                    let frames = pcm.frameLength() as usize;
                    if frames == 0 {
                        // AVSpeech's end-of-utterance marker.
                        let _ = chunks_tx.send(TtsChunk::End);
                        SYNTHS.with_borrow_mut(|live| live.remove(&id));
                        return;
                    }
                    let channels = usize::from(ours.channels).max(1);
                    let data = pcm.floatChannelData();
                    if data.is_null() {
                        return;
                    }
                    let mut samples = Vec::with_capacity(frames * channels);
                    let stride = pcm.stride();
                    for frame in 0..frames {
                        for channel in 0..channels {
                            let base = *data.add(channel);
                            samples.push(*base.as_ptr().add(frame * stride));
                        }
                    }
                    let _ = chunks_tx.send(TtsChunk::Pcm(samples));
                });
                SYNTHS.with_borrow_mut(|live| live.insert(id, synth.clone()));
                synth.writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&block));
                // The block is retained by `writeUtterance:` for the life of
                // the write; `RcBlock` releases our own reference here.
            }
        });

        // Block on `deckd-tts` — the thread whose whole job is to wait for
        // audio — until the format is known. This is the one place the seam
        // is synchronous, and V16 is why: the alternative is building a
        // resampler after the first buffer and throwing that buffer away.
        let format = format_rx
            .recv_timeout(FORMAT_WAIT)
            .map_err(|_| AudioError::Engine("AVSpeech produced no buffers".into()))?;
        Ok(Box::new(AppleTtsJob {
            format,
            chunks,
            stopper: Arc::new(AppleStopper { id }),
        }))
    }
}

struct AppleTtsJob {
    format: AudioFormat,
    chunks: Receiver<TtsChunk>,
    stopper: Arc<dyn TtsStop>,
}

impl TtsJob for AppleTtsJob {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn chunks(&self) -> Receiver<TtsChunk> {
        self.chunks.clone()
    }

    fn stopper(&self) -> Arc<dyn TtsStop> {
        self.stopper.clone()
    }
}

/// V16's barge-in handle: `Send + Sync`, callable from `deckd-audio` while
/// `deckd-tts` is asleep inside the sink.
struct AppleStopper {
    id: u64,
}

impl TtsStop for AppleStopper {
    fn stop(&self) {
        let id = self.id;
        mainloop::post(move || {
            SYNTHS.with_borrow_mut(|live| {
                if let Some(synth) = live.remove(&id) {
                    // SAFETY: on main, on a synthesiser created there.
                    unsafe {
                        synth.stopSpeakingAtBoundary(AVSpeechBoundary::Immediate);
                    }
                }
            });
        });
    }
}

// ---------------------------------------------------------------------------
// STT
// ---------------------------------------------------------------------------

struct LiveStt {
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    /// Kept alive: a dropped recogniser ends its own tasks.
    _recognizer: Retained<SFSpeechRecognizer>,
}

thread_local! {
    static SESSIONS: RefCell<HashMap<u64, LiveStt>> = RefCell::new(HashMap::new());
}

/// §7.6's v1 STT engine.
#[derive(Debug, Default)]
pub struct AppleStt;

impl AppleStt {
    #[must_use]
    pub fn new() -> Self {
        AppleStt
    }
}

/// `SFSpeechRecognizer` for a locale, on main.
fn recognizer_for(locale: &str) -> Option<Retained<SFSpeechRecognizer>> {
    // SAFETY: on main; `initWithLocale:` takes an `NSLocale` and may return
    // nil for a locale the recogniser does not know.
    unsafe {
        let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(locale));
        SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale)
    }
}

impl SttEngine for AppleStt {
    fn info(&self) -> EngineInfo {
        let locales = on_main(|| {
            // SAFETY: a read of the framework's own locale set, on main.
            let set = unsafe { SFSpeechRecognizer::supportedLocales() };
            let mut out: Vec<String> = set
                .iter()
                // SAFETY: each element is an `NSLocale`.
                .map(|l| l.localeIdentifier().to_string())
                .collect();
            out.sort_unstable();
            out
        })
        .unwrap_or_default();
        EngineInfo {
            name: "apple".into(),
            on_device: true,
            locales,
        }
    }

    /// §7.6's `-32009`, answered before anything opens. en-US is on-device
    /// on this machine; zh-CN and en-GB are not ("No Assistant asset for
    /// language").
    fn on_device(&self, locale: &str) -> OnDevice {
        let locale = locale.to_string();
        on_main(move || match recognizer_for(&locale) {
            // SAFETY: on main, on a recogniser created there.
            Some(recognizer) => unsafe {
                if !recognizer.isAvailable() {
                    OnDevice::Unsupported
                } else if recognizer.supportsOnDeviceRecognition() {
                    OnDevice::Yes
                } else {
                    OnDevice::ServerOnly
                }
            },
            None => OnDevice::Unsupported,
        })
        .unwrap_or(OnDevice::Unsupported)
    }

    fn start(&self, req: &SttStart) -> Result<Box<dyn SttSession>, AudioError> {
        // The refusal happens here, before a microphone is opened (§7.6).
        if !req.allow_server && self.on_device(&req.locale) != OnDevice::Yes {
            return Err(AudioError::NoOnDevice(req.locale.clone()));
        }
        let id = next_id();
        let (events_tx, events) = crossbeam_channel::unbounded::<SttEvent>();
        let (final_tx, final_rx) = crossbeam_channel::bounded::<Result<String, AudioError>>(1);
        let locale = req.locale.clone();
        let contextual = req.contextual.clone();
        let on_device = !req.allow_server;

        let started = on_main(move || {
            // SAFETY: every call below is on main, on objects created here.
            unsafe {
                let Some(recognizer) = recognizer_for(&locale) else {
                    return false;
                };
                let request = SFSpeechAudioBufferRecognitionRequest::new();
                request.setShouldReportPartialResults(true);
                request.setRequiresOnDeviceRecognition(on_device);
                if !contextual.is_empty() {
                    // §7.6: agent names and the pending card's labels, so
                    // "cohdl-api" survives an en-US recogniser.
                    let strings: Vec<Retained<NSString>> =
                        contextual.iter().map(|s| NSString::from_str(s)).collect();
                    let refs: Vec<&NSString> = strings.iter().map(AsRef::as_ref).collect();
                    request.setContextualStrings(&NSArray::from_slice(&refs));
                }
                let transcript = RefCell::new(AppleTranscript::default());
                let handler = RcBlock::new(
                    move |result: *mut SFSpeechRecognitionResult,
                          error: *mut objc2_foundation::NSError| {
                        if let Some(result) = result.as_ref() {
                            let best = result.bestTranscription();
                            // A pause can settle a phrase without ending the
                            // task. The next partial then starts from scratch.
                            // Metadata identifies these settled results; their
                            // audio ranges also handle repeated callbacks and
                            // cumulative finals without duplicating words.
                            let span = result.speechRecognitionMetadata().map(|metadata| {
                                let start = metadata.speechStartTimestamp();
                                let end = best
                                    .segments()
                                    .iter()
                                    .fold(start + metadata.speechDuration(), |end, word| {
                                        end.max(word.timestamp() + word.duration())
                                    });
                                SpeechSpan { start, end }
                            });
                            let text = transcript
                                .borrow_mut()
                                .update(best.formattedString().to_string(), span);
                            if result.isFinal() {
                                let _ = final_tx.try_send(Ok(text.clone()));
                                let _ = events_tx.send(SttEvent::Final(text));
                            } else {
                                let _ = events_tx.send(SttEvent::Partial(text));
                            }
                        }
                        if let Some(error) = error.as_ref() {
                            // V16: this arrives **mid-session**
                            // (`kAFAssistantErrorDomain 1110` was observed),
                            // not at the end, and §7.4's dead-mic path is
                            // waiting for it.
                            let code = error.code().to_string();
                            let message = error.localizedDescription().to_string();
                            let _ = final_tx.try_send(Err(AudioError::Engine(message.clone())));
                            let _ = events_tx.send(SttEvent::Error { code, message });
                        }
                    },
                );
                let task = recognizer.recognitionTaskWithRequest_resultHandler(&request, &handler);
                SESSIONS.with_borrow_mut(|live| {
                    live.insert(
                        id,
                        LiveStt {
                            request,
                            task,
                            _recognizer: recognizer,
                        },
                    );
                });
                true
            }
        });
        if started != Some(true) {
            return Err(AudioError::Engine(format!(
                "no recogniser for {}",
                req.locale
            )));
        }
        Ok(Box::new(AppleSttSession {
            id,
            events,
            final_rx,
        }))
    }
}

/// The **Send handle** of V16. Every method posts to main; nothing ObjC is
/// ever touched from the thread that holds this.
struct AppleSttSession {
    id: u64,
    events: Receiver<SttEvent>,
    final_rx: Receiver<Result<String, AudioError>>,
}

impl SttSession for AppleSttSession {
    fn feed(&mut self, mono_f32: &[f32], rate: u32) {
        if mono_f32.is_empty() || rate == 0 {
            return;
        }
        let id = self.id;
        // Copied once, here, on the control loop's tick — never in the
        // capture callback, which only pushes into the `rtrb` ring.
        let samples = mono_f32.to_vec();
        mainloop::post(move || {
            // SAFETY: on main; the buffer is created, filled and appended
            // without escaping this block.
            unsafe {
                let Some(format) =
                    AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
                        AVAudioFormat::alloc(),
                        AVAudioCommonFormat::PCMFormatFloat32,
                        f64::from(rate),
                        1,
                        false,
                    )
                else {
                    return;
                };
                #[allow(clippy::cast_possible_truncation)]
                let frames = samples.len() as u32;
                let Some(buffer) = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                    AVAudioPCMBuffer::alloc(),
                    &format,
                    frames,
                ) else {
                    return;
                };
                buffer.setFrameLength(frames);
                let data = buffer.floatChannelData();
                if data.is_null() {
                    return;
                }
                let channel = *data;
                std::ptr::copy_nonoverlapping(samples.as_ptr(), channel.as_ptr(), samples.len());
                SESSIONS.with_borrow(|live| {
                    if let Some(session) = live.get(&id) {
                        session.request.appendAudioPCMBuffer(&buffer);
                    }
                });
            }
        });
    }

    fn events(&self) -> Receiver<SttEvent> {
        self.events.clone()
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>> {
        let id = self.id;
        mainloop::post(move || {
            SESSIONS.with_borrow_mut(|live| {
                if let Some(session) = live.remove(&id) {
                    // SAFETY: on main, on objects created there.
                    unsafe {
                        session.request.endAudio();
                        session.task.finish();
                    }
                    // The task is kept alive by the framework until it
                    // delivers its final result; dropping our reference does
                    // not cancel it.
                }
            });
        });
        let final_rx = self.final_rx.clone();
        Box::pin(FinalResult(final_rx))
    }

    fn cancel(self: Box<Self>) {
        let id = self.id;
        mainloop::post(move || {
            SESSIONS.with_borrow_mut(|live| {
                if let Some(session) = live.remove(&id) {
                    // SAFETY: on main, on objects created there.
                    unsafe { session.task.cancel() }
                }
            });
        });
    }
}

/// The final transcript, as a future.
///
/// It has no waker: the answer comes from a `crossbeam` channel that the
/// main thread fills, and `crossbeam` has no waker integration.
/// [`crate::audio::block_on_until`] polls on a fixed cadence for exactly
/// this case, and documents it; a future *with* a waker is still served
/// immediately.
struct FinalResult(Receiver<Result<String, AudioError>>);

impl std::future::Future for FinalResult {
    type Output = Result<String, AudioError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use crossbeam_channel::TryRecvError;
        match self.0.try_recv() {
            Ok(result) => std::task::Poll::Ready(result),
            Err(TryRecvError::Empty) => std::task::Poll::Pending,
            Err(TryRecvError::Disconnected) => std::task::Poll::Ready(Err(AudioError::Engine(
                "the recogniser ended without a final result".into(),
            ))),
        }
    }
}

/// Everything the engines know about themselves, for `voice.engines`
/// (§4.9), gathered without starting anything.
#[must_use]
pub fn engines() -> (EngineInfo, EngineInfo) {
    (AppleStt::new().info(), AppleTts::new().info())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These engines can only be *driven* from the main thread (V13), which
    /// libtest never gives a `#[test]`. What can be checked here is that
    /// constructing them touches nothing and posts nothing — so a daemon
    /// that installs them before its run loop starts does not deadlock, and
    /// `deckd --no-device` does not talk to AVFoundation at all.
    #[test]
    fn constructing_an_engine_touches_no_framework() {
        let tts = AppleTts::new();
        let stt = AppleStt::new();
        // No run loop is turning on this thread, so nothing posted can have
        // run. If construction had called into AVFoundation, this test would
        // hang rather than fail — which is itself the assertion.
        assert_eq!(std::mem::size_of_val(&tts), 0);
        assert_eq!(std::mem::size_of_val(&stt), 0);
    }

    /// The Send bound of V16, checked by the compiler rather than by
    /// reading: a session is created on the core and fed from the control
    /// loop, so a `!Send` one would not compile at the seam — but it would
    /// compile *here*, which is why this assertion is written down.
    #[test]
    fn the_session_and_the_engines_are_send() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send::<Box<dyn SttSession>>();
        assert_send_sync::<AppleStt>();
        assert_send_sync::<AppleTts>();
        assert_send_sync::<AppleStopper>();
    }
}
