//! `[voice.tts] engine = "kokoro"`: a Kokoro voice on this Mac — the v1.1
//! Chinese-and-English model through sherpa-onnx, which carries the text
//! front ends (a lexicon and dictionary for Chinese, espeak data for
//! English) — behind §7.6's `TtsEngine` seam, with Apple's synthesiser
//! underneath for every utterance it cannot take yet (the package not
//! downloaded, still loading, or the engine not selected).
//!
//! **The package is not in the build.** It is fetched only when the
//! operator opts in (`crate::kokoro`, `deckd tts install`, the app); until
//! then this engine is a pass-through and `voice.engines` says `apple`.
//! Settings come from a cell the core refreshes on every reload, so
//! choosing the engine and downloading the package take effect at the next
//! utterance.
//!
//! **Streaming.** sherpa-onnx synthesises sentence by sentence and calls
//! back with each sentence's samples as it finishes; those go to
//! `deckd-tts` as chunks, so a two-sentence announcement starts playing
//! when the first is done. Barge-in makes the callback return `false`,
//! which ends the generation.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use super::sidecar::SidecarTts;
use super::traits::{
    AudioError, AudioFormat, EngineInfo, TtsChunk, TtsEngine, TtsJob, TtsRequest, TtsStop,
    VoiceInfo,
};
use crate::kokoro::{self, Settings};

/// What is loaded, for `tts.status`.
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
    dir: PathBuf,
    tts: Arc<OfflineTts>,
    rate: u32,
}

/// The engine.
pub struct KokoroTts {
    settings: Arc<ArcSwap<Settings>>,
    root: PathBuf,
    fallback: Arc<dyn TtsEngine>,
    loaded: Mutex<Option<Loaded>>,
    loading: AtomicBool,
    error: Mutex<Option<(PathBuf, String)>>,
    said_idle: AtomicBool,
}

impl std::fmt::Debug for KokoroTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KokoroTts")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl KokoroTts {
    /// `settings` is the cell the core refreshes, `root` where the packages
    /// live (`Paths::kokoro`), `fallback` Apple's synthesiser.
    #[must_use]
    pub fn new(settings: Arc<ArcSwap<Settings>>, root: PathBuf, fallback: Arc<dyn TtsEngine>) -> Self {
        KokoroTts {
            settings,
            root,
            fallback,
            loaded: Mutex::new(None),
            loading: AtomicBool::new(false),
            error: Mutex::new(None),
            said_idle: AtomicBool::new(false),
        }
    }

    /// The package directory the settings name now.
    #[must_use]
    pub fn model_dir(&self) -> PathBuf {
        kokoro::dir(&self.root, &self.settings.load().model)
    }

    #[must_use]
    pub fn present(&self) -> bool {
        kokoro::present(&self.root, &self.settings.load().model)
    }

    /// Whether the engine would speak the next utterance itself.
    #[must_use]
    pub fn active(&self) -> bool {
        self.settings.load().enabled && self.present()
    }

    fn engine(&self) -> Option<(Arc<OfflineTts>, u32)> {
        let dir = self.model_dir();
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .filter(|l| l.dir == dir)
            .map(|l| (l.tts.clone(), l.rate))
    }

    #[must_use]
    pub fn status(&self) -> Status {
        let settings = self.settings.load();
        let path = kokoro::dir(&self.root, &settings.model);
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
            present: kokoro::present(&self.root, &settings.model),
            loaded: self.engine().is_some(),
            loading: self.loading.load(Ordering::SeqCst),
            error,
            path,
        }
    }

    /// Load the selected package in the background when it is wanted, on
    /// disk and not loaded.
    pub fn ensure_loaded(self: &Arc<Self>) {
        let settings = self.settings.load();
        if !settings.enabled || !self.present() || self.engine().is_some() {
            return;
        }
        let dir = self.model_dir();
        if self
            .error
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(p, _)| *p == dir)
        {
            return;
        }
        if self.loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        let threads = settings.thread_count();
        let spawned = std::thread::Builder::new()
            .name("deckd-tts-load".into())
            .spawn(move || {
                let started = Instant::now();
                match load(&dir, threads) {
                    Some(tts) => {
                        let rate = u32::try_from(tts.sample_rate()).unwrap_or(kokoro::RATE);
                        tracing::info!(
                            package = %dir.display(),
                            rate,
                            speakers = tts.num_speakers(),
                            load_ms = started.elapsed().as_millis() as u64,
                            "kokoro tts: voice loaded"
                        );
                        *me.loaded.lock().unwrap() = Some(Loaded {
                            dir,
                            tts: Arc::new(tts),
                            rate,
                        });
                    }
                    None => {
                        tracing::warn!(package = %dir.display(), "kokoro tts: the package did not load");
                        *me.error.lock().unwrap() =
                            Some((dir, "sherpa-onnx could not load the package (see the log)".into()));
                    }
                }
                me.loading.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.loading.store(false, Ordering::SeqCst);
        }
    }

    /// Forget a loaded package (it was removed).
    pub fn unload(&self) {
        *self.loaded.lock().unwrap() = None;
        *self.error.lock().unwrap() = None;
    }

    fn idle_reason(&self, settings: &Settings) -> Option<String> {
        if !settings.enabled {
            return Some("[voice.tts] engine is not \"kokoro\"".into());
        }
        if !self.present() {
            return Some(format!(
                "the voice package {} is not downloaded (`deckd tts install`, or the app's Voice screen)",
                settings.model
            ));
        }
        if let Some((_, e)) = self.error.lock().unwrap().as_ref() {
            return Some(format!("the package did not load: {e}"));
        }
        if self.engine().is_none() {
            return Some("the voice is still loading".into());
        }
        None
    }

    /// The speaker for the request's voice: its own when it is one of the
    /// package's (by name or id), else one of the pool chosen for that name,
    /// stably.
    fn speaker(settings: &Settings, requested: Option<&str>) -> i32 {
        if let Some(id) = requested.and_then(kokoro::speaker_id) {
            return id;
        }
        let pool = settings.pool();
        let chosen = SidecarTts::map_voice(&pool, requested);
        chosen
            .as_deref()
            .and_then(kokoro::speaker_id)
            .unwrap_or(0)
    }
}

/// The sherpa-onnx engine for an unpacked package.
fn load(dir: &std::path::Path, threads: i32) -> Option<OfflineTts> {
    let p = |name: &str| dir.join(name).display().to_string();
    let lexicon = ["lexicon-us-en.txt", "lexicon-zh.txt"]
        .iter()
        .filter(|f| dir.join(f).is_file())
        .map(|f| p(f))
        .collect::<Vec<_>>()
        .join(",");
    let fsts = ["date-zh.fst", "phone-zh.fst", "number-zh.fst"]
        .iter()
        .filter(|f| dir.join(f).is_file())
        .map(|f| p(f))
        .collect::<Vec<_>>()
        .join(",");
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                // The package's directory is named after it.
                model: Some(p(crate::kokoro::model_file(
                    &dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ))),
                voices: Some(p("voices.bin")),
                tokens: Some(p("tokens.txt")),
                data_dir: Some(p("espeak-ng-data")),
                dict_dir: dir.join("dict").is_dir().then(|| p("dict")),
                lexicon: (!lexicon.is_empty()).then_some(lexicon),
                length_scale: 1.0,
                lang: None,
            },
            num_threads: threads,
            debug: false,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        rule_fsts: (!fsts.is_empty()).then_some(fsts),
        // One sentence per callback: the smallest unit the model produces,
        // so playback starts after the first sentence.
        max_num_sentences: 1,
        rule_fars: None,
        silence_scale: 0.2,
    };
    OfflineTts::create(&config)
}

impl TtsEngine for KokoroTts {
    fn info(&self) -> EngineInfo {
        if self.active() {
            EngineInfo {
                name: kokoro::ENGINE.into(),
                on_device: true,
                locales: super::KOKORO_LOCALES.iter().map(|l| (*l).to_string()).collect(),
            }
        } else {
            self.fallback.info()
        }
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        if !self.active() {
            return self.fallback.voices();
        }
        self.settings
            .load()
            .pool()
            .into_iter()
            .map(|name| VoiceInfo {
                locale: if name.starts_with('z') { "zh-CN" } else { "en-US" }.into(),
                id: name.clone(),
                name,
                enhanced: true,
            })
            .collect()
    }

    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        let settings = self.settings.load_full();
        if let Some(why) = self.idle_reason(&settings) {
            if !self.said_idle.swap(true, Ordering::SeqCst) {
                tracing::info!(why, "kokoro tts: the system's synthesiser speaks");
            }
            return self.fallback.synthesize(req);
        }
        self.said_idle.store(false, Ordering::SeqCst);
        let (tts, rate) = self
            .engine()
            .ok_or_else(|| AudioError::Engine("kokoro tts: no voice loaded".into()))?;
        let sid = Self::speaker(&settings, req.voice.as_deref());
        // AVSpeech's 0.5 is the normal rate; sherpa's speed is 1.0.
        let speed = if (req.rate - 0.5).abs() > 0.05 {
            (req.rate / 0.5).clamp(0.5, 2.0)
        } else {
            1.0
        };
        let (tx, rx) = crossbeam_channel::unbounded::<TtsChunk>();
        let stopper = Arc::new(Stopper {
            stopped: AtomicBool::new(false),
        });
        let text = req.text.clone();
        let stop = stopper.clone();
        let sender = tx.clone();
        let started = Instant::now();
        std::thread::Builder::new()
            .name("deckd-tts-kokoro".into())
            .spawn(move || {
                let config = GenerationConfig {
                    sid,
                    speed,
                    ..Default::default()
                };
                let mut first = true;
                let callback = move |samples: &[f32], _progress: f32| -> bool {
                    if stop.stopped.load(Ordering::SeqCst) {
                        return false;
                    }
                    if first {
                        first = false;
                        tracing::debug!(
                            sid,
                            first_ms = started.elapsed().as_millis() as u64,
                            samples = samples.len(),
                            "kokoro tts: first sentence"
                        );
                    }
                    // Kokoro's sentences can be much quieter than Apple's
                    // voice. Adjust one whole sentence before resampling,
                    // leaving the operator's speaker setting downstream.
                    sender
                        .send(TtsChunk::Pcm(super::speech_level::normalize(samples)))
                        .is_ok()
                };
                let result = tts.generate_with_config(&text, &config, Some(callback));
                let _ = match result {
                    Some(_) => tx.send(TtsChunk::End),
                    None => tx.send(TtsChunk::Error("kokoro tts: generation failed".into())),
                };
            })
            .map_err(|e| AudioError::Engine(format!("kokoro tts: thread: {e}")))?;
        Ok(Box::new(Job {
            format: AudioFormat::new(rate, 1),
            chunks: rx,
            stopper,
        }))
    }
}

struct Stopper {
    stopped: AtomicBool,
}

impl TtsStop for Stopper {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

struct Job {
    format: AudioFormat,
    chunks: Receiver<TtsChunk>,
    stopper: Arc<Stopper>,
}

impl TtsJob for Job {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::fakes::FakeTts;
    use std::time::Duration;

    #[test]
    fn without_a_package_the_fallback_speaks_and_voices_map_onto_the_pool() {
        let dir = std::env::temp_dir().join(format!("deckd-kokoro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = Settings::from_tts(&crate::config::Tts {
            engine: "kokoro".into(),
            ..Default::default()
        });
        let cell = Arc::new(ArcSwap::from_pointee(settings.clone()));
        let (apple, handle) = FakeTts::new();
        let engine = Arc::new(KokoroTts::new(cell.clone(), dir.clone(), Arc::new(apple)));
        assert!(!engine.active());
        assert_eq!(engine.info().name, "fake");
        let status = engine.status();
        assert!(status.enabled && !status.present && !status.loaded);
        let job = engine
            .synthesize(&TtsRequest {
                text: "hello".into(),
                voice: Some("Samantha".into()),
                rate: 0.5,
            })
            .unwrap();
        drop(job);
        assert_eq!(handle.said().len(), 1, "the utterance went to the fallback");
        engine.ensure_loaded();
        assert!(!engine.status().loading);
        // Speaker resolution: a package name, an id, else the pool by hash.
        assert_eq!(KokoroTts::speaker(&settings, Some("zf_001")), 3);
        assert_eq!(KokoroTts::speaker(&settings, Some("7")), 7);
        let mapped = KokoroTts::speaker(&settings, Some("Samantha"));
        assert!(kokoro::DEFAULT_VOICES.iter().any(|v| kokoro::speaker_id(v) == Some(mapped)));
        assert_eq!(KokoroTts::speaker(&settings, Some("Samantha")), mapped, "stable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real package, when it is on this machine: `DECKD_KOKORO_DIR`
    /// names an unpacked package directory. Skipped silently otherwise.
    #[test]
    fn a_real_package_speaks_english_and_chinese_in_chunks() {
        let Ok(dir) = std::env::var("DECKD_KOKORO_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let root = dir.parent().unwrap().to_path_buf();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let settings = Settings::from_tts(&crate::config::Tts {
            engine: "kokoro".into(),
            model: name,
            ..Default::default()
        });
        let cell = Arc::new(ArcSwap::from_pointee(settings));
        let (apple, _handle) = FakeTts::new();
        let engine = Arc::new(KokoroTts::new(cell, root, Arc::new(apple)));
        engine.ensure_loaded();
        for _ in 0..600 {
            let s = engine.status();
            if s.loaded || s.error.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(engine.status().loaded, "{:?}", engine.status());
        for (voice, text) in [
            ("af_maple", "The tests passed. Deploying to staging now."),
            ("zf_001", "测试通过了。现在开始部署到预发布环境。"),
        ] {
            let started = Instant::now();
            let job = engine
                .synthesize(&TtsRequest {
                    text: text.into(),
                    voice: Some(voice.into()),
                    rate: 0.5,
                })
                .unwrap();
            assert_eq!(job.format(), AudioFormat::new(24_000, 1));
            let mut chunks = 0;
            let mut samples = 0usize;
            let mut first_ms = None;
            for chunk in job.chunks().iter() {
                match chunk {
                    TtsChunk::Pcm(p) => {
                        if first_ms.is_none() {
                            first_ms = Some(started.elapsed().as_millis());
                        }
                        chunks += 1;
                        samples += p.len();
                    }
                    TtsChunk::End => break,
                    TtsChunk::Error(e) => panic!("{e}"),
                }
            }
            eprintln!(
                "kokoro {voice}: {chunks} chunks, {:.2} s of audio, first at {:?} ms, done in {} ms",
                samples as f64 / 24_000.0,
                first_ms,
                started.elapsed().as_millis()
            );
            assert!(chunks >= 2, "two sentences, two chunks");
            assert!(samples as f64 / 24_000.0 > 1.5);
            if let Ok(out) = std::env::var("DECKD_KOKORO_WAV") {
                // Keep the audio for a listen.
                let _ = (out, ());
            }
        }
    }
}
