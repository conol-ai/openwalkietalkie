//! `[voice.stt] engine = "whisper"`: a recogniser on this Mac — whisper.cpp
//! through `whisper-rs`, with the Metal backend — behind §7.6's `SttEngine`
//! seam, with Apple's recogniser underneath for everything it cannot do
//! yet (the model not downloaded, still loading, or the engine not
//! selected).
//!
//! **Why it exists.** Apple's recogniser has no on-device asset for Chinese
//! on this machine, so a `zh-CN` press is refused unless speech is sent to
//! Apple's server. A Whisper model recognises Chinese and English in one
//! utterance, on the machine, and `[voice.stt] language = "auto"` lets it
//! pick per press.
//!
//! **The weights are not in the build.** They are fetched only when the
//! operator opts in (`crate::stt`, `deckd stt install`, the app's
//! first-start prompt); until then this engine is a pass-through to Apple's
//! and `voice.engines` says `apple`. Settings are read from a cell the core
//! refreshes on every reload, so choosing the engine, downloading the
//! model and changing the language all take effect at the next press.
//!
//! **Partials.** Whisper decodes a whole buffer; it does not stream. The
//! live transcript the deck shows while PTT is held is made the way every
//! local dictation tool makes it: the session's thread re-decodes the audio
//! accumulated so far about once a second and reports the text as a
//! partial; the final is one more decode of everything at release. On an
//! M-series GPU large-v3-turbo decodes a few seconds of speech in well
//! under a second, so the partials keep up with the press.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use super::traits::{
    AudioError, BoxFuture, EngineInfo, OnDevice, SttEngine, SttEvent, SttSession, SttStart,
};
use crate::stt::{self, Settings};

/// The model's input rate.
pub const RATE: u32 = 16_000;
/// Whisper refuses less than a second of audio; shorter buffers are padded
/// to this with silence.
const MIN_SAMPLES: usize = (RATE as usize * 11) / 10;
/// How long between two partial decodes, at least.
const PARTIAL_EVERY: Duration = Duration::from_millis(900);
/// How much new audio a partial decode waits for, at least.
const PARTIAL_NEW: usize = RATE as usize * 4 / 10;
/// The final's budget: a fixed part plus this much per second of audio in
/// the press. Leave room for long speech and GPU contention; this is a
/// failure ceiling, not a latency target. Successful decodes return at once.
const FINISH_FIXED: Duration = Duration::from_secs(10);
const FINISH_PER_SECOND: Duration = Duration::from_secs(1);
const FINISH_MAX: Duration = Duration::from_secs(90);

/// The encoder positions covering `samples` of 16 kHz audio, with room to
/// spare, between a floor Whisper still decodes well from and the full window.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn audio_ctx(samples: usize) -> i32 {
    let positions = samples / 320 + 128;
    i32::try_from(positions.next_multiple_of(64).clamp(512, 1500)).unwrap_or(1500)
}

fn final_budget(fed: Duration) -> Duration {
    (FINISH_FIXED + FINISH_PER_SECOND.mul_f64(fed.as_secs_f64()))
        .clamp(super::engine::FINISH_BUDGET, FINISH_MAX)
}
/// What `voice.engines` reports the model speaks; Whisper covers ~100.
const LOCALES: [&str; 16] = [
    "en-US", "en-GB", "zh-CN", "zh-TW", "ja-JP", "ko-KR", "de-DE", "fr-FR", "es-ES", "it-IT",
    "pt-BR", "ru-RU", "nl-NL", "pl-PL", "tr-TR", "vi-VN",
];

/// What is loaded, for `stt.status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub model: String,
    pub path: PathBuf,
    pub present: bool,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
}

struct Loaded {
    path: PathBuf,
    ctx: Arc<WhisperContext>,
}

/// The engine.
pub struct WhisperStt {
    settings: Arc<ArcSwap<Settings>>,
    dir: PathBuf,
    fallback: Arc<dyn SttEngine>,
    loaded: Mutex<Option<Loaded>>,
    loading: AtomicBool,
    /// The last load's failure, by the file it was for.
    error: Mutex<Option<(PathBuf, String)>>,
    /// Whether the "no model yet, Apple's speaks" line has been logged.
    said_idle: AtomicBool,
}

impl std::fmt::Debug for WhisperStt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperStt")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl WhisperStt {
    /// `settings` is the cell the core refreshes, `dir` where the models
    /// live (`Paths::stt`), `fallback` Apple's recogniser.
    #[must_use]
    pub fn new(settings: Arc<ArcSwap<Settings>>, dir: PathBuf, fallback: Arc<dyn SttEngine>) -> Self {
        WhisperStt {
            settings,
            dir,
            fallback,
            loaded: Mutex::new(None),
            loading: AtomicBool::new(false),
            error: Mutex::new(None),
            said_idle: AtomicBool::new(false),
        }
    }

    /// The model file the settings name now.
    #[must_use]
    pub fn model_path(&self) -> PathBuf {
        stt::model_path(&self.dir, &self.settings.load().model)
    }

    /// Whether the selected model is on disk and whole.
    #[must_use]
    pub fn present(&self) -> bool {
        stt::present(&self.dir, &self.settings.load().model)
    }

    /// Whether the engine would answer the next press itself.
    #[must_use]
    pub fn active(&self) -> bool {
        self.settings.load().enabled && self.present()
    }

    fn context(&self) -> Option<Arc<WhisperContext>> {
        let path = self.model_path();
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .filter(|l| l.path == path)
            .map(|l| l.ctx.clone())
    }

    #[must_use]
    pub fn status(&self) -> Status {
        let settings = self.settings.load();
        let path = stt::model_path(&self.dir, &settings.model);
        let error = self
            .error
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|(_, e)| e.clone());
        Status {
            enabled: settings.enabled,
            model: settings.model.clone(),
            present: stt::present(&self.dir, &settings.model),
            loaded: self.context().is_some(),
            loading: self.loading.load(Ordering::SeqCst),
            error,
            path,
        }
    }

    /// Load the selected model in the background when it is wanted, on
    /// disk and not loaded. Called at every reload and every status read,
    /// so a model that has just finished downloading is loaded before the
    /// next press rather than by it.
    pub fn ensure_loaded(self: &Arc<Self>) {
        let settings = self.settings.load();
        if !settings.enabled || !self.present() || self.context().is_some() {
            return;
        }
        let path = self.model_path();
        if self
            .error
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(p, _)| *p == path)
        {
            return;
        }
        if self.loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("deckd-stt-load".into())
            .spawn(move || {
                let started = Instant::now();
                let mut params = WhisperContextParameters::default();
                params.use_gpu(true);
                let loaded = WhisperContext::new_with_params(&path, params);
                match loaded {
                    Ok(ctx) => {
                        tracing::info!(
                            model = %path.display(),
                            load_ms = started.elapsed().as_millis() as u64,
                            "whisper stt: model loaded"
                        );
                        *me.loaded.lock().unwrap() = Some(Loaded {
                            path,
                            ctx: Arc::new(ctx),
                        });
                    }
                    Err(e) => {
                        tracing::warn!(model = %path.display(), error = %e, "whisper stt: the model did not load");
                        *me.error.lock().unwrap() = Some((path, e.to_string()));
                    }
                }
                me.loading.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.loading.store(false, Ordering::SeqCst);
        }
    }

    /// Forget a loaded model (the file was removed).
    pub fn unload(&self) {
        *self.loaded.lock().unwrap() = None;
        *self.error.lock().unwrap() = None;
    }

    /// Why Apple's recogniser takes the next press, if it does.
    fn idle_reason(&self, settings: &Settings) -> Option<String> {
        if !settings.enabled {
            return Some("[voice.stt] engine is not \"whisper\"".into());
        }
        if !self.present() {
            return Some(format!(
                "the model {} is not downloaded (`deckd stt install`, or the app's Voice screen)",
                settings.model
            ));
        }
        if let Some((_, e)) = self.error.lock().unwrap().as_ref() {
            return Some(format!("the model did not load: {e}"));
        }
        if self.context().is_none() {
            return Some("the model is still loading".into());
        }
        None
    }
}

impl SttEngine for WhisperStt {
    fn info(&self) -> EngineInfo {
        if self.active() {
            EngineInfo {
                name: stt::ENGINE.into(),
                on_device: true,
                locales: LOCALES.iter().map(|l| (*l).to_string()).collect(),
            }
        } else {
            self.fallback.info()
        }
    }

    /// Every locale is on the machine once the model is: the §7.6 refusal
    /// is Apple's, and it applies only while Apple's recogniser is the one
    /// that would answer.
    fn on_device(&self, locale: &str) -> OnDevice {
        if self.active() {
            OnDevice::Yes
        } else {
            self.fallback.on_device(locale)
        }
    }

    fn start(&self, req: &SttStart) -> Result<Box<dyn SttSession>, AudioError> {
        let settings = self.settings.load_full();
        if let Some(why) = self.idle_reason(&settings) {
            if !self.said_idle.swap(true, Ordering::SeqCst) {
                tracing::info!(why, "whisper stt: Apple's recogniser takes the presses");
            }
            return self.fallback.start(req);
        }
        self.said_idle.store(false, Ordering::SeqCst);
        let ctx = self
            .context()
            .ok_or_else(|| AudioError::Engine("whisper stt: no model loaded".into()))?;
        let state = ctx
            .create_state()
            .map_err(|e| AudioError::Engine(format!("whisper stt: {e}")))?;
        let language = settings.language_for(&req.locale);
        let prompt = (!req.contextual.is_empty()).then(|| req.contextual.join(", "));
        Ok(Box::new(Session::spawn(
            state,
            Decode {
                language,
                prompt,
                threads: settings.thread_count(),
            },
        )))
    }
}

/// What every decode of a session is run with.
#[derive(Clone, Debug)]
struct Decode {
    language: Option<String>,
    prompt: Option<String>,
    threads: i32,
}

enum Msg {
    Audio(Vec<f32>, u32),
    Finish,
    Cancel,
}

/// One press: a thread that owns the model state and the audio so far.
struct Session {
    to_worker: Sender<Msg>,
    events: Receiver<SttEvent>,
    final_rx: Receiver<Result<String, AudioError>>,
    /// Set at `finish`: a partial decode in flight stops at its next step,
    /// so the final does not queue behind it.
    abort_partial: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    /// Milliseconds of audio fed, for the final's budget.
    fed_ms: Arc<AtomicU64>,
}

impl Session {
    fn spawn(mut state: WhisperState, decode: Decode) -> Session {
        let (to_worker, from_engine) = crossbeam_channel::unbounded::<Msg>();
        let (events_tx, events) = crossbeam_channel::unbounded::<SttEvent>();
        let (final_tx, final_rx) = crossbeam_channel::bounded::<Result<String, AudioError>>(1);
        let events_if_no_thread = events_tx.clone();
        let abort_partial = Arc::new(AtomicBool::new(false));
        let abort_in_worker = abort_partial.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_in_worker = cancel.clone();
        let fed_ms = Arc::new(AtomicU64::new(0));
        let spawned = std::thread::Builder::new()
            .name("deckd-stt-whisper".into())
            .spawn(move || {
                let mut audio: Vec<f32> = Vec::new();
                let mut decoded_len = 0usize;
                let mut last_decode = Instant::now();
                let mut last_text = String::new();
                loop {
                    let msg = match from_engine.recv_timeout(Duration::from_millis(250)) {
                        Ok(msg) => Some(msg),
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                    };
                    match msg {
                        Some(Msg::Audio(samples, rate)) => {
                            audio.extend(to_model_rate(&samples, rate));
                        }
                        Some(Msg::Finish) => {
                            let result = if audio.is_empty() {
                                Ok(String::new())
                            } else {
                                run(&mut state, &audio, &decode, Some(&cancel_in_worker))
                            };
                            match result {
                                Ok(text) => {
                                    let _ = events_tx.send(SttEvent::Final(text.clone()));
                                    let _ = final_tx.send(Ok(text));
                                }
                                Err(why) => {
                                    let _ = events_tx.send(SttEvent::Error {
                                        code: "engine".into(),
                                        message: why.clone(),
                                    });
                                    let _ = final_tx.send(Err(AudioError::Engine(why)));
                                }
                            }
                            return;
                        }
                        Some(Msg::Cancel) => return,
                        None => {}
                    }
                    // A partial, when enough is new and enough time passed —
                    // and not once the release is in: the final is what
                    // matters then, and a partial would only delay it.
                    if audio.len() >= decoded_len + PARTIAL_NEW
                        && last_decode.elapsed() >= PARTIAL_EVERY
                        && !abort_in_worker.load(Ordering::SeqCst)
                    {
                        decoded_len = audio.len();
                        match run(&mut state, &audio, &decode, Some(&abort_in_worker)) {
                            Ok(text) => {
                                if !abort_in_worker.load(Ordering::SeqCst)
                                    && !text.is_empty()
                                    && text != last_text
                                {
                                    last_text.clone_from(&text);
                                    let _ = events_tx.send(SttEvent::Partial(text));
                                }
                            }
                            // A release intentionally interrupts a partial.
                            Err(_) if abort_in_worker.load(Ordering::SeqCst) => {}
                            Err(why) => tracing::warn!(why, "whisper stt: partial decode failed"),
                        }
                        last_decode = Instant::now();
                    }
                }
            });
        if let Err(e) = spawned {
            let _ = events_if_no_thread.send(SttEvent::Error {
                code: "engine".into(),
                message: format!("whisper stt: thread: {e}"),
            });
        }
        Session {
            to_worker,
            events,
            final_rx,
            abort_partial,
            cancel,
            fed_ms,
        }
    }
}

impl SttSession for Session {
    fn feed(&mut self, mono_f32: &[f32], rate: u32) {
        if mono_f32.is_empty() || rate == 0 {
            return;
        }
        self.fed_ms
            .fetch_add((mono_f32.len() as u64 * 1000) / u64::from(rate), Ordering::Relaxed);
        let _ = self.to_worker.send(Msg::Audio(mono_f32.to_vec(), rate));
    }

    fn events(&self) -> Receiver<SttEvent> {
        self.events.clone()
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>> {
        // Stop a partial in flight first, so the final starts at once.
        self.abort_partial.store(true, Ordering::SeqCst);
        let _ = self.to_worker.send(Msg::Finish);
        Box::pin(FinalResult { receiver: self.final_rx.clone(), cancel: self.cancel.clone() })
    }

    fn finish_budget(&self) -> Duration {
        let fed = Duration::from_millis(self.fed_ms.load(Ordering::Relaxed));
        final_budget(fed)
    }

    fn cancel(self: Box<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        self.abort_partial.store(true, Ordering::SeqCst);
        let _ = self.to_worker.send(Msg::Cancel);
    }
}

/// The final transcript, as a future — the same shape as Apple's: no
/// waker, because the answer comes over a `crossbeam` channel from the
/// session's thread, and `deckd-audio` polls it on each engine tick
/// against the session's final-decode deadline.
struct FinalResult {
    receiver: Receiver<Result<String, AudioError>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for FinalResult {
    fn drop(&mut self) { self.cancel.store(true, Ordering::SeqCst); }
}

impl std::future::Future for FinalResult {
    type Output = Result<String, AudioError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use crossbeam_channel::TryRecvError;
        match self.receiver.try_recv() {
            Ok(result) => std::task::Poll::Ready(result),
            Err(TryRecvError::Empty) => std::task::Poll::Pending,
            Err(TryRecvError::Disconnected) => std::task::Poll::Ready(Err(AudioError::Engine(
                "whisper stt: the session ended without a final".into(),
            ))),
        }
    }
}

/// Mono at `rate` → mono at the model's rate, by linear interpolation. The
/// deck's link is already 16 kHz; the Mac's own microphone, which
/// `voice.listen{input:"mac"}` opens, is not.
#[must_use]
pub fn to_model_rate(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate == RATE || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = f64::from(rate) / f64::from(RATE);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let base = pos.floor() as usize;
            #[allow(clippy::cast_possible_truncation)]
            let frac = (pos - pos.floor()) as f32;
            let a = samples[base.min(samples.len() - 1)];
            let b = samples[(base + 1).min(samples.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// `user_data` must point to an `AtomicBool` kept alive through the
/// synchronous `WhisperState::full` call that installs this callback.
unsafe extern "C" fn abort_requested(user_data: *mut std::ffi::c_void) -> bool {
    // SAFETY: `run` borrows the owning Arc until `full` returns. No mutable
    // reference is created, and concurrent cancellation is an atomic store.
    unsafe { &*user_data.cast::<AtomicBool>() }.load(Ordering::SeqCst)
}

/// The context that makes the multilingual models write Mandarin in
/// simplified characters (they default to traditional).
#[cfg(target_os = "linux")]
const SIMPLIFIED_PROMPT: &str = "以下是普通话的句子，使用简体中文。";

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn has_han(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Whisper can answer a near-silent press with its own prompt.
#[cfg(target_os = "linux")]
fn echoes_prompt(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && SIMPLIFIED_PROMPT.contains(text)
}

/// One decode, and on Linux a second in simplified characters when the
/// speech turned out to be Chinese. The prompt is only ever given to a
/// decode of Chinese (configured, or heard in the first pass), so English
/// is never pulled toward it, and an answer that is just the prompt is
/// dropped.
fn run(
    state: &mut WhisperState,
    audio: &[f32],
    decode: &Decode,
    abort: Option<&Arc<AtomicBool>>,
) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let zh = decode.language.as_deref().is_some_and(|l| l == "zh" || l.starts_with("zh-"));
        let auto = decode.language.as_deref().is_none_or(|l| l == "auto");
        if decode.prompt.is_none() && zh {
            let simplified = Decode { prompt: Some(SIMPLIFIED_PROMPT.into()), ..decode.clone() };
            let text = decode_once(state, audio, &simplified, abort)?;
            return Ok(if echoes_prompt(&text) { String::new() } else { text });
        }
        let text = decode_once(state, audio, decode, abort)?;
        if decode.prompt.is_none() && auto && has_han(&text) {
            let simplified = Decode { prompt: Some(SIMPLIFIED_PROMPT.into()), language: Some("zh".into()), ..decode.clone() };
            let again = decode_once(state, audio, &simplified, abort)?;
            if !again.is_empty() && !echoes_prompt(&again) {
                return Ok(again);
            }
        }
        Ok(text)
    }
    #[cfg(not(target_os = "linux"))]
    decode_once(state, audio, decode, abort)
}

/// One decode of `audio` (16 kHz mono): the segments' text, joined.
/// `abort`, when given, is checked between decoder steps and ends the
/// decode early once set (a partial that the release has overtaken).
fn decode_once(
    state: &mut WhisperState,
    audio: &[f32],
    decode: &Decode,
    abort: Option<&Arc<AtomicBool>>,
) -> Result<String, String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    if let Some(flag) = abort {
        // whisper-rs 0.16.0's safe wrapper boxes a trait object, then casts
        // that box to the concrete closure in its trampoline. That reads
        // the wrong allocation and aborts even when our flag is false.
        // SAFETY: `flag` owns a stable AtomicBool and is borrowed for this
        // whole synchronous decode. The callback only reads that atomic;
        // it neither takes ownership nor touches the Whisper state.
        unsafe {
            params.set_abort_callback(Some(abort_requested));
            params.set_abort_callback_user_data(Arc::as_ptr(flag).cast_mut().cast());
        }
    }
    params.set_n_threads(decode.threads);
    params.set_translate(false);
    params.set_language(decode.language.as_deref());
    params.set_no_context(true);
    params.set_single_segment(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_token_timestamps(false);
    if let Some(prompt) = decode.prompt.as_deref() {
        params.set_initial_prompt(prompt);
    }
    let padded: Vec<f32>;
    let audio = if audio.len() < MIN_SAMPLES {
        let mut v = audio.to_vec();
        v.resize(MIN_SAMPLES, 0.0);
        padded = v;
        &padded
    } else {
        audio
    };
    // Linux only, where Whisper runs on the CPU and a press must finish within
    // its budget (the Mac has Metal and keeps whisper.cpp's defaults).
    // One pass: the default retries at higher temperatures re-decode the whole
    // window several times. And the encoder costs the same for 2 s as for 30 s
    // unless it is told how much of its 30 s window is audio.
    #[cfg(target_os = "linux")]
    {
        params.set_temperature_inc(0.0);
        params.set_audio_ctx(audio_ctx(audio.len()));
    }
    state.full(params, audio).map_err(|e| e.to_string())?;
    let mut text = String::new();
    for segment in state.as_iter() {
        let piece = segment.to_str_lossy().map_err(|e| e.to_string())?;
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if !text.is_empty() && !joins_without_space(text.chars().last(), piece.chars().next()) {
            text.push(' ');
        }
        text.push_str(piece);
    }
    Ok(text.trim().to_string())
}

/// Chinese and Japanese segments join with no space; everything else with
/// one.
fn joins_without_space(last: Option<char>, next: Option<char>) -> bool {
    fn cjk(c: char) -> bool {
        matches!(c as u32, 0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    }
    matches!((last, next), (Some(a), Some(b)) if cjk(a) && cjk(b))
}

/// What `voice.engines` should name as the recogniser: this engine when it
/// is selected and its model is on disk, else Apple's own report.
#[must_use]
pub fn engine_info(settings: &Settings, present: bool, apple: &EngineInfo) -> EngineInfo {
    if settings.enabled && present {
        EngineInfo {
            name: stt::ENGINE.into(),
            on_device: true,
            locales: LOCALES.iter().map(|l| (*l).to_string()).collect(),
        }
    } else {
        apple.clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_chinese_text_asks_for_the_simplified_second_pass() {
        assert!(has_han("你好 hello"));
        assert!(!has_han("hello world"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_answer_that_is_only_the_prompt_is_recognised_as_an_echo() {
        assert!(echoes_prompt("以下是普通话的句子，使用简体中文。"));
        assert!(echoes_prompt("使用简体中文"));
        assert!(!echoes_prompt("打开浏览器"));
        assert!(!echoes_prompt(""));
    }

    #[test]
    fn the_encoder_window_follows_the_audio_between_a_floor_and_the_full_30_seconds() {
        assert_eq!(super::audio_ctx(17_600), 512);
        assert_eq!(super::audio_ctx(16_000 * 12), 768);
        assert_eq!(super::audio_ctx(16_000 * 40), 1500);
    }

    use super::*;
    use crate::audio::fakes::FakeStt;

    #[test]
    fn final_decode_budget_covers_long_presses_and_cancellation_stops_the_worker() {
        assert_eq!(final_budget(Duration::ZERO), Duration::from_secs(30));
        assert_eq!(final_budget(Duration::from_secs(60)), Duration::from_secs(70));
        assert_eq!(final_budget(Duration::from_secs(200)), FINISH_MAX);
        assert!(FINISH_MAX < deck_model::FINALIZE_BUDGET);
        let (_send, receiver) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let future = FinalResult { receiver, cancel: cancel.clone() };
        drop(future);
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn resampling_keeps_the_duration_and_the_values() {
        let at_48k: Vec<f32> = (0..4800).map(|i| (i % 48) as f32 / 48.0).collect();
        let out = to_model_rate(&at_48k, 48_000);
        assert_eq!(out.len(), 1600, "100 ms stays 100 ms");
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(to_model_rate(&[0.5, 0.25], 16_000), vec![0.5, 0.25]);
        assert!(to_model_rate(&[], 48_000).is_empty());
    }

    #[test]
    fn cjk_segments_join_without_a_space_and_latin_ones_with() {
        assert!(joins_without_space(Some('你'), Some('好')));
        assert!(!joins_without_space(Some('d'), Some('h')));
        assert!(!joins_without_space(Some('好'), Some('h')));
        assert!(!joins_without_space(None, Some('h')));
    }

    #[test]
    fn without_a_model_the_fallback_answers_and_the_badge_is_apples() {
        let dir = std::env::temp_dir().join(format!("deckd-whisper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = Settings::from_stt(&crate::config::Stt {
            engine: "whisper".into(),
            ..Default::default()
        });
        let cell = Arc::new(ArcSwap::from_pointee(settings.clone()));
        let (apple, handle) = FakeStt::new();
        let engine = Arc::new(WhisperStt::new(cell.clone(), dir.clone(), Arc::new(apple)));
        assert!(!engine.active());
        assert_eq!(engine.info().name, "fake");
        assert_eq!(engine.on_device("zh-CN"), OnDevice::Yes, "the fake says yes");
        let status = engine.status();
        assert!(status.enabled && !status.present && !status.loaded);
        let session = engine
            .start(&SttStart {
                locale: "en-US".into(),
                contextual: vec![],
                allow_server: false,
            })
            .unwrap();
        drop(session);
        assert_eq!(handle.starts().len(), 1, "the press went to the fallback");
        engine.ensure_loaded();
        assert!(!engine.status().loading, "nothing to load without a file");
        // Not selected: the badge is Apple's even with a (fake) file there.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(stt::model_path(&dir, "custom"), b"not a model").unwrap();
        let mut off = settings;
        off.enabled = false;
        off.model = "custom".into();
        cell.store(Arc::new(off));
        assert!(engine.present() && !engine.active());
        let apple_info = EngineInfo {
            name: "apple".into(),
            on_device: true,
            locales: vec!["en-US".into()],
        };
        assert_eq!(engine_info(&cell.load(), true, &apple_info).name, "apple");
        let mut on = (**cell.load()).clone();
        on.enabled = true;
        assert_eq!(engine_info(&on, true, &apple_info).name, "whisper");
        assert_eq!(engine_info(&on, false, &apple_info).name, "apple");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Exercise the actual C callback boundary, including reuse of the
    /// state for a final after cancellation. A Rust-only callback test
    /// would not catch the binding's incorrect user-data cast.
    #[test]
    #[ignore = "requires DECKD_WHISPER_MODEL and access to Metal"]
    fn a_real_model_only_aborts_when_cancellation_is_requested() {
        let path = std::env::var("DECKD_WHISPER_MODEL").expect("a local ggml model path");
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .expect("the local model loads");
        let mut state = ctx.create_state().unwrap();
        let audio: Vec<f32> = (0..32_000)
            .map(|i| ((i as f32) * 0.01).sin() * 0.001)
            .collect();
        let decode = Decode {
            language: Some("en".into()),
            prompt: None,
            threads: 4,
        };
        let abort = Arc::new(AtomicBool::new(false));

        assert!(run(&mut state, &audio, &decode, None).is_ok());
        let partial = run(&mut state, &audio, &decode, Some(&abort));
        assert!(
            partial.is_ok(),
            "an uncancelled partial must decode: {partial:?}"
        );
        abort.store(true, Ordering::SeqCst);
        assert!(run(&mut state, &audio, &decode, Some(&abort)).is_err());
        // Release cancels a partial, then decodes the final on this same
        // state without a callback. The old flag must not cancel it.
        let final_ = run(&mut state, &audio, &decode, None);
        assert!(
            final_.is_ok(),
            "the final survives a cancelled partial: {final_:?}"
        );
        abort.store(false, Ordering::SeqCst);
        assert!(run(&mut state, &audio, &decode, Some(&abort)).is_ok());
    }

    /// The real model, when it is on this machine: `DECKD_WHISPER_MODEL`
    /// names a ggml file. Skipped silently otherwise, so the suite runs on
    /// a Mac that never opted in.
    #[test]
    fn a_real_model_transcribes_a_synthetic_tone_without_panicking() {
        let Ok(path) = std::env::var("DECKD_WHISPER_MODEL") else {
            return;
        };
        let path = PathBuf::from(path);
        let dir = path.parent().unwrap().to_path_buf();
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("ggml-")
            .trim_end_matches(".bin")
            .to_string();
        let settings = Settings::from_stt(&crate::config::Stt {
            engine: "whisper".into(),
            model: name,
            ..Default::default()
        });
        let cell = Arc::new(ArcSwap::from_pointee(settings));
        let (apple, _handle) = FakeStt::new();
        let engine = Arc::new(WhisperStt::new(cell, dir, Arc::new(apple)));
        engine.ensure_loaded();
        for _ in 0..600 {
            if engine.status().loaded || engine.status().error.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(engine.status().loaded, "{:?}", engine.status());
        let mut session = engine
            .start(&SttStart {
                locale: "en-US".into(),
                contextual: vec!["cohdl-api".into()],
                allow_server: false,
            })
            .unwrap();
        // `DECKD_WHISPER_WAV` (16-bit mono PCM at any rate) is fed in 100 ms
        // slices like a press, and `DECKD_WHISPER_EXPECT` must be in the
        // final; without them, two seconds of near-silence must come back
        // (as nothing) and the final must resolve.
        let (audio, rate) = match std::env::var("DECKD_WHISPER_WAV") {
            Ok(wav) => read_wav(&PathBuf::from(wav)),
            Err(_) => (
                (0..32_000).map(|i| ((i as f32) * 0.01).sin() * 0.001).collect(),
                16_000,
            ),
        };
        // `DECKD_WHISPER_REALTIME=1` feeds at the speed of speech, as a
        // press does; else as fast as the thread takes it.
        let slice = rate as usize / 10;
        let pace = if std::env::var_os("DECKD_WHISPER_REALTIME").is_some() { 100 } else { 20 };
        for chunk in audio.chunks(slice) {
            session.feed(chunk, rate);
            std::thread::sleep(Duration::from_millis(pace));
        }
        let events = session.events();
        let released = Instant::now();
        let text = crate::audio::block_on_until(session.finish(), Instant::now() + Duration::from_secs(60))
            .expect("the final arrives well within a minute")
            .unwrap();
        eprintln!("whisper final took {} ms after release", released.elapsed().as_millis());
        let partials: Vec<String> = events
            .try_iter()
            .filter_map(|e| match e {
                SttEvent::Partial(t) => Some(t),
                _ => None,
            })
            .collect();
        eprintln!("whisper final: {text:?} (partials: {partials:?})");
        if let Ok(expect) = std::env::var("DECKD_WHISPER_EXPECT") {
            assert!(
                text.to_lowercase().contains(&expect.to_lowercase()),
                "expected {expect:?} in {text:?}"
            );
        } else {
            assert!(text.len() < 200, "{text:?}");
        }
    }

    /// A RIFF WAV of 16-bit PCM, mono: the samples and the rate. Enough
    /// parser for what `say -o x.wav --data-format=LEI16@16000` writes.
    fn read_wav(path: &PathBuf) -> (Vec<f32>, u32) {
        let bytes = std::fs::read(path).expect("the wav is readable");
        // RIFF chunks from offset 12 (`say` puts a JUNK chunk before `fmt `).
        let (mut rate, mut channels, mut data) = (16_000u32, 1usize, &bytes[0..0]);
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
            if id == b"fmt " {
                channels = usize::from(u16::from_le_bytes([body[2], body[3]]));
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            } else if id == b"data" {
                data = body;
            }
            at += 8 + size + (size & 1);
        }
        assert!(!data.is_empty() && channels > 0, "no PCM in {}", path.display());
        let samples: Vec<f32> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| f32::from(i16::from_le_bytes(*p)) / 32768.0)
            .collect::<Vec<f32>>()
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();
        (samples, rate)
    }
}
