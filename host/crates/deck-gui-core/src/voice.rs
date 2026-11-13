//! The three voice replies the **Voice** screen and the Welcome sheet read
//! (§4.9, §7.8): `voice.engines`, `voice.test` and `perm.request`.
//!
//! They sit beside [`crate::admin`] for the same reason it does: all three
//! are **calls and not events**, so nothing folds them into the mirror and
//! the screen that asked keeps the answer itself. The route, the UIDs, the
//! permissions and the meter are the other half of that screen and they *do*
//! come off the stream — `voice.route`, `perm.changed` and `voice.level` —
//! so none of them is here.
//!
//! Every parser is hand-written rather than derived, exactly as `admin`'s
//! are: a daemon newer than this GUI will answer with a field or a status
//! word this build has never heard of, and a derive would fail the whole
//! reply over it.
//!
//! The one thing this module is most careful about is `limits`. All three
//! replies carry the daemon's own sentences about what it did **not** do —
//! that `played:true` means the chime was queued and not that anybody heard
//! it, that `voice.test{what:"mic"}` never opens the microphone, that
//! `perm.request` does not show a prompt yet — and the screen renders them
//! verbatim. They are kept as `Vec<String>` and never summarised here,
//! because a GUI that paraphrased them would be putting its own words where
//! the daemon put a caveat.
use deck_model::VoiceTransport;
use serde_json::Value;

/// What the screens call the machine the daemon runs on.
pub const HOST: &str = if cfg!(target_os = "macos") { "Mac" } else { "computer" };

/// `voice.test{what}` — the four checks, in the order the screen offers
/// them. Only the first makes a sound.
pub const TESTS: [&str; 4] = ["speaker", "mic", "stt", "tts"];

/// `perm.request{which}` — the two permissions §7.8 can be asked about.
pub const PERMISSIONS: [&str; 2] = ["microphone", "speech"];

/// Permissions shown for the current deck. Bluetooth carries the custom
/// BLE audio protocol; Microphone applies only if capture falls back to the
/// Mac's input. Keep that permission visible without presenting it as a
/// requirement for the deck's microphone.
pub const BLE_PERMISSIONS: [&str; 3] = ["bluetooth", "speech", "microphone"];

#[must_use]
pub fn displayed_permissions(transport: Option<VoiceTransport>) -> &'static [&'static str] {
    match transport {
        Some(VoiceTransport::Coreaudio) => &PERMISSIONS,
        _ => &BLE_PERMISSIONS,
    }
}

/// §7.8's answer for a prompt that did not surface, which is the **only**
/// answer `perm.request` gives until the signed bundle of §10.1 lands
/// (§10.4). Named here so the screen can recognise it and put the daemon's
/// own `limits` where the button was.
pub const PROMPT_NOT_SHOWN: &str = "prompt_not_shown";

/// One engine as `voice.engines` reports it: `{id, on_device, locales}`.
///
/// **`locales`, not voices.** The design's shape for the TTS half was
/// `[{id, voices}]`; what an `EngineInfo` carries is the locale set, so that
/// is what the daemon answers and that is what this reads. The per-agent
/// voice *pool* is `[voice.voices] default_pool` in `config.get` with the
/// assignments in `state.json`, and it is not in this reply at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    pub id: String,
    /// Whether the engine runs on this machine rather than on a server.
    pub on_device: bool,
    pub locales: Vec<String>,
}

impl Engine {
    /// `None` when the object has no `id`, which is not an engine.
    pub fn parse(value: &Value) -> Option<Engine> {
        Some(Engine {
            id: text(value.get("id"))?,
            on_device: value
                .get("on_device")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            locales: strings(value.get("locales")),
        })
    }

    fn list(value: Option<&Value>) -> Vec<Engine> {
        value
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(Engine::parse).collect())
            .unwrap_or_default()
    }
}

/// `voice.engines` → `{stt:[…], tts:[…], selected:{stt, tts, locale}}`
/// (§4.9).
///
/// Both halves are lists because the reply is, even though this daemon
/// reports exactly one of each: it answers from the single `EngineInfo` pair
/// the audio side pushed when its engines came up (M5), so a second entry
/// would mean a daemon that grew one and this build would show it rather
/// than drop it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engines {
    pub stt: Vec<Engine>,
    pub tts: Vec<Engine>,
    /// Which of them `[voice]` selects, and the locale it selects with.
    pub selected_stt: Option<String>,
    pub selected_tts: Option<String>,
    pub locale: Option<String>,
}

impl Engines {
    pub fn parse(result: &Value) -> Engines {
        let selected = result.get("selected");
        Engines {
            stt: Engine::list(result.get("stt")),
            tts: Engine::list(result.get("tts")),
            selected_stt: text(selected.and_then(|s| s.get("stt"))),
            selected_tts: text(selected.and_then(|s| s.get("tts"))),
            locale: text(selected.and_then(|s| s.get("locale"))),
        }
    }

    /// The engine `[voice] stt.engine` names, or the first one reported when
    /// the daemon named none. `None` for a reply with no engines at all.
    pub fn stt(&self) -> Option<&Engine> {
        pick(&self.stt, self.selected_stt.as_deref())
    }

    pub fn tts(&self) -> Option<&Engine> {
        pick(&self.tts, self.selected_tts.as_deref())
    }
}

/// The `[voice.stt]` half of `config.get` — the Voice screen's
/// **Recognition** control and the first-start prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SttConfig {
    /// `"apple"` or `"whisper"`.
    pub engine: String,
    /// `""` is the daemon's default model.
    pub model: String,
    /// `"auto"`, or a language code (`"en"`).
    pub language: String,
    /// The recogniser's locale (`"en-GB"`), which picks the dialect where
    /// `language` names only the language.
    pub locale: String,
    /// Whether the operator has answered the first-start question about a
    /// local model. False on a Mac that has never been asked.
    pub prompted: bool,
    /// `voice.stt.companion`: a connected app's own recogniser hears the
    /// presses addressed to its agents, which sends that audio off this
    /// machine. False when the service does not say.
    pub companion: bool,
}

impl SttConfig {
    pub fn parse(result: &Value) -> SttConfig {
        let voice = result.get("voice").unwrap_or(&Value::Null);
        SttConfig {
            engine: text(voice.get("stt.engine")).unwrap_or_default(),
            model: text(voice.get("stt.model")).unwrap_or_default(),
            language: text(voice.get("stt.language")).unwrap_or_default(),
            locale: text(voice.get("stt.locale")).unwrap_or_default(),
            prompted: voice
                .get("stt.prompted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            companion: voice
                .get("stt.companion")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    pub fn whisper_selected(&self) -> bool {
        self.engine == "whisper"
    }
}

/// A download in flight, or the one that last ran, as `stt.status` reports
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Download {
    pub model: String,
    pub bytes: u64,
    pub total: u64,
    pub running: bool,
    pub done: bool,
    pub error: Option<String>,
}

/// `stt.status` → the local recogniser's model and where it stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SttStatus {
    pub engine: String,
    /// The local model answers the next press: selected and on disk.
    pub active: bool,
    pub model: String,
    /// The model's size, `0` for one the daemon does not catalogue.
    pub total: u64,
    pub present: bool,
    pub loaded: bool,
    pub loading: bool,
    pub load_error: Option<String>,
    pub download: Option<Download>,
    pub prompted: bool,
}

impl SttStatus {
    pub fn parse(result: &Value) -> SttStatus {
        let download = result.get("download").filter(|d| d.is_object()).map(|d| Download {
            model: text(d.get("model")).unwrap_or_default(),
            bytes: d.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            total: d.get("total").and_then(Value::as_u64).unwrap_or(0),
            running: d.get("running").and_then(Value::as_bool).unwrap_or(false),
            done: d.get("done").and_then(Value::as_bool).unwrap_or(false),
            error: text(d.get("error")),
        });
        SttStatus {
            engine: text(result.get("engine")).unwrap_or_default(),
            active: result.get("active").and_then(Value::as_bool).unwrap_or(false),
            model: text(result.get("model")).unwrap_or_default(),
            total: result.get("total").and_then(Value::as_u64).unwrap_or(0),
            present: result.get("present").and_then(Value::as_bool).unwrap_or(false),
            loaded: result.get("loaded").and_then(Value::as_bool).unwrap_or(false),
            loading: result.get("loading").and_then(Value::as_bool).unwrap_or(false),
            load_error: text(result.get("load_error")),
            download,
            prompted: result.get("prompted").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// A download is running now.
    pub fn downloading(&self) -> bool {
        self.download.as_ref().is_some_and(|d| d.running)
    }

    /// The running download's progress, 0.0 to 1.0, when its size is known.
    pub fn fraction(&self) -> Option<f32> {
        let d = self.download.as_ref().filter(|d| d.running && d.total > 0)?;
        #[allow(clippy::cast_precision_loss)]
        Some((d.bytes as f32 / d.total as f32).clamp(0.0, 1.0))
    }

    /// The one line under the control: what recognises, and what the
    /// model is doing.
    pub fn line(&self) -> String {
        let size = size_text(self.total);
        if let Some(d) = self.download.as_ref().filter(|d| d.running) {
            return match self.fraction() {
                Some(f) => format!(
                    "Downloading {} \u{2014} {} of {size} ({:.0}%)",
                    d.model,
                    size_text(d.bytes),
                    f * 100.
                ),
                None => format!("Downloading {} \u{2014} {}", d.model, size_text(d.bytes)),
            };
        }
        if let Some(why) = self.download.as_ref().and_then(|d| d.error.clone()) {
            return format!("The download of {} stopped: {why}", self.model);
        }
        if self.engine != "whisper" {
            return if self.present {
                format!("Apple's recogniser is selected; {} ({size}) stays on disk.", self.model)
            } else {
                "Apple's on-device recogniser. Nothing to download.".to_string()
            };
        }
        if !self.present {
            return format!("Whisper is selected but {} ({size}) is not downloaded: Apple's recogniser answers until it is.", self.model);
        }
        if let Some(e) = &self.load_error {
            return format!("{} did not load: {e}", self.model);
        }
        if self.loading {
            return format!("Loading {} ({size})\u{2026}", self.model);
        }
        if self.loaded {
            return format!("{} ({size}) is loaded and recognises on this {HOST}.", self.model);
        }
        format!("{} ({size}) is downloaded; it loads before the next press.", self.model)
    }
}

/// `tts.status` → the local voice's package and where it stands. The same
/// shape as [`SttStatus`], with the voice's own words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TtsStatus {
    pub engine: String,
    pub active: bool,
    pub model: String,
    pub total: u64,
    pub present: bool,
    pub loaded: bool,
    pub loading: bool,
    pub load_error: Option<String>,
    pub download: Option<Download>,
    /// The pool the daemon assigns agents from, in the package's names.
    pub voices: Vec<String>,
}

impl TtsStatus {
    pub fn parse(result: &Value) -> TtsStatus {
        let base = SttStatus::parse(result);
        TtsStatus {
            engine: base.engine,
            active: base.active,
            model: base.model,
            total: base.total,
            present: base.present,
            loaded: base.loaded,
            loading: base.loading,
            load_error: base.load_error,
            download: base.download,
            voices: strings(result.get("voices")),
        }
    }

    pub fn downloading(&self) -> bool {
        self.download.as_ref().is_some_and(|d| d.running)
    }

    pub fn fraction(&self) -> Option<f32> {
        let d = self.download.as_ref().filter(|d| d.running && d.total > 0)?;
        #[allow(clippy::cast_precision_loss)]
        Some((d.bytes as f32 / d.total as f32).clamp(0.0, 1.0))
    }

    /// The line under the voice's row.
    pub fn line(&self) -> String {
        let size = size_text(self.total);
        if let Some(d) = self.download.as_ref().filter(|d| d.running) {
            return match self.fraction() {
                Some(f) => format!("Downloading {} \u{2014} {} of {size} ({:.0}%)", d.model, size_text(d.bytes), f * 100.),
                None => format!("Downloading {} \u{2014} {}", d.model, size_text(d.bytes)),
            };
        }
        if let Some(why) = self.download.as_ref().and_then(|d| d.error.clone()) {
            return format!("The download of {} stopped: {why}", self.model);
        }
        if self.engine != "kokoro" {
            return if self.present {
                format!("The Kokoro voice is not selected; {} ({size}) stays on disk.", self.model)
            } else {
                format!("A Kokoro voice ({size}) can be downloaded here; nothing is until you ask.")
            };
        }
        if !self.present {
            return format!("Kokoro is selected but {} ({size}) is not downloaded: Apple's synthesiser speaks until it is.", self.model);
        }
        if let Some(e) = &self.load_error {
            return format!("{} did not load: {e}", self.model);
        }
        if self.loading {
            return format!("Loading {} ({size})\u{2026}", self.model);
        }
        let voices = if self.voices.is_empty() {
            String::new()
        } else {
            format!(" Agents speak as {}.", self.voices.join(", "))
        };
        if self.loaded {
            return format!("{} ({size}) is loaded and speaks on this {HOST}.{voices}", self.model);
        }
        format!("{} ({size}) is downloaded; it loads before the next announcement.{voices}", self.model)
    }
}

/// `1.6 GB`, `574 MB`, `0 B`.
pub fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", b / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", b / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// The two hosted synthesisers the daemon knows (`[voice.tts] provider`),
/// in the order the screen offers them, with the model each uses when
/// `model` is empty.
pub const PROVIDERS: [(&str, &str, &str); 2] = [
    ("openai", "OpenAI", "gpt-4o-mini-tts"),
    ("elevenlabs", "ElevenLabs", "eleven_flash_v2_5"),
];

/// The `[voice.tts]` half of `config.get` — the Voice screen's **Spoken
/// replies** control: bring your own key.
///
/// The key itself is never in the reply. `config.get` publishes
/// `tts.api_key` as a **hint** — its length and last four characters,
/// `""` for none — which is all a screen needs to say "a key is set" and
/// to tell two keys apart; a key is written with `config.set` and read back
/// only as that hint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TtsConfig {
    /// `"cloud"` (the provider with a key, Apple's without), `"mlx"` (a local
    /// model), `"apple"`.
    pub engine: String,
    pub provider: String,
    /// `""` when no key is set.
    pub key_hint: String,
    /// `""` is the provider's default model.
    pub model: String,
    pub voices: Vec<String>,
}

impl TtsConfig {
    pub fn parse(result: &Value) -> TtsConfig {
        let voice = result.get("voice").unwrap_or(&Value::Null);
        TtsConfig {
            engine: text(voice.get("tts.engine")).unwrap_or_default(),
            provider: text(voice.get("tts.provider")).unwrap_or_default(),
            key_hint: text(voice.get("tts.api_key")).unwrap_or_default(),
            model: text(voice.get("tts.model")).unwrap_or_default(),
            voices: strings(voice.get("tts.voices")),
        }
    }

    pub fn has_key(&self) -> bool {
        !self.key_hint.is_empty()
    }

    /// Whether the control applies at all: with `engine = "mlx"` or
    /// `"apple"` the key is kept but not used, and the screen says so.
    pub fn cloud_selected(&self) -> bool {
        self.engine == "cloud"
    }

    /// The provider's display name and default model, when it is one the
    /// screen knows.
    pub fn provider_row(&self) -> Option<(&'static str, &'static str, &'static str)> {
        PROVIDERS
            .iter()
            .copied()
            .find(|(id, _, _)| self.provider.eq_ignore_ascii_case(id))
    }

    /// The model the daemon sends: the configured one, else the
    /// provider's default, else nothing to say.
    pub fn effective_model(&self) -> Option<String> {
        if !self.model.is_empty() {
            return Some(self.model.clone());
        }
        self.provider_row().map(|(_, _, model)| model.to_string())
    }
}

fn pick<'a>(list: &'a [Engine], selected: Option<&str>) -> Option<&'a Engine> {
    match selected {
        Some(name) => list.iter().find(|e| e.id == name).or_else(|| list.first()),
        None => list.first(),
    }
}

/// `voice.test{what}` — what the daemon **observed**, which is not the same
/// as what it exercised (§4.9).
///
/// Three of the four `what`s run nothing at all: `mic` does not open the
/// microphone (a capture is a PTT session with an owner, a cap and a TCC
/// grant, and a diagnostic may not start one behind the operator's back),
/// and `stt`/`tts` report what the engine said about itself at start-up.
/// Only `speaker` makes a sound, and even there `played` means the chime was
/// **queued on the audio thread** and not that it was heard.
///
/// That is why [`TestReport::limits`] is not decoration: it is the daemon's
/// own sentences saying so, and the screen shows them under every result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestReport {
    pub what: String,
    /// The route as the test found it, in `Route`'s own wire words.
    pub route: Option<String>,
    /// The running audio transport, which may differ from a config edited
    /// since the daemon started.
    pub transport: Option<String>,
    pub output_uid: Option<String>,
    pub input_uid: Option<String>,
    /// `speaker` only: the chime was accepted by the audio thread. `None`
    /// for every other check, which queues nothing.
    pub played: Option<bool>,
    pub chime: Option<String>,
    /// `mic` only: `[voice] mic_warm`, the policy that decides when the
    /// capture is warmed.
    pub mic_warm: Option<String>,
    /// The capture policy the running transport actually applies. The BLE
    /// protocol starts capture on demand regardless of the legacy setting.
    pub mic_warm_effective: Option<String>,
    /// `stt`/`tts` only: the engine as it reported itself.
    pub engine: Option<Engine>,
    pub listening: bool,
    pub limits: Vec<String>,
}

impl TestReport {
    pub fn parse(result: &Value) -> TestReport {
        TestReport {
            what: text(result.get("what")).unwrap_or_default(),
            route: text(result.get("route")),
            transport: text(result.get("transport")),
            output_uid: text(result.get("output_uid")),
            input_uid: text(result.get("input_uid")),
            played: result.get("played").and_then(Value::as_bool),
            chime: text(result.get("chime")),
            mic_warm: text(result.get("mic_warm")),
            mic_warm_effective: text(result.get("mic_warm_effective")),
            engine: result.get("engine").and_then(Engine::parse),
            listening: result
                .get("listening")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            limits: strings(result.get("limits")),
        }
    }

    /// Did this check make a sound? Only the speaker one can, and only when
    /// the queue took it.
    pub fn made_a_sound(&self) -> bool {
        self.what == "speaker" && self.played == Some(true)
    }
}

/// `perm.request{which}` → `{which, status, limits}` (§7.8).
///
/// Stubbed at the prompt until week 7's signed bundle: the status is
/// [`PROMPT_NOT_SHOWN`] and the reason is in `limits`, in the daemon's
/// words. The screen shows those words instead of leaving a button that
/// cannot work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    pub which: String,
    pub status: String,
    pub limits: Vec<String>,
}

impl Prompt {
    pub fn parse(result: &Value) -> Prompt {
        Prompt {
            which: text(result.get("which")).unwrap_or_default(),
            status: text(result.get("status")).unwrap_or_default(),
            limits: strings(result.get("limits")),
        }
    }

    /// The prompt did not surface, so nothing was asked and nothing changed.
    pub fn not_shown(&self) -> bool {
        self.status == PROMPT_NOT_SHOWN
    }
}

/// A `Vec<String>` from a JSON array, dropping anything that is not a
/// string. Absent and null are an empty list and never an error.
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A non-empty string field. `null`, absent and `""` are all `None`.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engines_reply() -> Value {
        json!({
            "stt": [{"id": "apple", "on_device": true, "locales": ["en-US", "en-GB"]}],
            "tts": [{"id": "apple", "on_device": true, "locales": ["en-US"]}],
            "selected": {"stt": "apple", "tts": "apple", "locale": "en-US"},
            "a_field_from_a_newer_daemon": 7
        })
    }

    #[test]
    fn stt_status_says_what_recognises_and_how_far_a_download_is() {
        let running = SttStatus::parse(&json!({
            "engine": "whisper", "active": false, "model": "large-v3-turbo",
            "total": 1624555275u64, "present": false, "loaded": false, "loading": false,
            "download": {"model": "large-v3-turbo", "bytes": 812277637u64, "total": 1624555275u64, "running": true, "done": false, "error": null},
            "prompted": true
        }));
        assert!(running.downloading());
        assert!((running.fraction().unwrap() - 0.5).abs() < 0.01);
        assert_eq!(running.line(), "Downloading large-v3-turbo \u{2014} 812 MB of 1.6 GB (50%)");
        let waiting = SttStatus::parse(&json!({
            "engine": "whisper", "active": false, "model": "large-v3-turbo", "total": 1624555275u64,
            "present": false, "loaded": false, "loading": false, "download": null
        }));
        assert!(!waiting.downloading());
        assert!(waiting.line().contains("not downloaded"), "{}", waiting.line());
        let loaded = SttStatus::parse(&json!({
            "engine": "whisper", "active": true, "model": "large-v3-turbo", "total": 1624555275u64,
            "present": true, "loaded": true, "loading": false, "download": null
        }));
        // "on this Mac." on macOS, "on this computer." elsewhere (`HOST`).
        assert_eq!(HOST, if cfg!(target_os = "macos") { "Mac" } else { "computer" });
        assert_eq!(
            loaded.line(),
            format!("large-v3-turbo (1.6 GB) is loaded and recognises on this {HOST}.")
        );
        let apple = SttStatus::parse(&json!({"engine": "apple", "model": "large-v3-turbo", "total": 1624555275u64, "present": false}));
        assert_eq!(apple.line(), "Apple's on-device recogniser. Nothing to download.");
        let failed = SttStatus::parse(&json!({
            "engine": "whisper", "model": "small", "total": 487601967u64, "present": false,
            "download": {"model": "small", "bytes": 10, "total": 487601967u64, "running": false, "done": false, "error": "curl exit 22: 404"}
        }));
        assert!(failed.line().contains("stopped: curl exit 22"), "{}", failed.line());
        assert_eq!(size_text(1_624_555_275), "1.6 GB");
        assert_eq!(size_text(574_041_195), "574 MB");
        assert_eq!(size_text(0), "0 B");
        let cfg = SttConfig::parse(&json!({"voice": {"stt.engine": "whisper", "stt.model": "", "stt.language": "auto", "stt.prompted": false}}));
        assert!(cfg.whisper_selected() && !cfg.prompted);
        assert_eq!(SttStatus::parse(&Value::Null), SttStatus::default());
    }

    #[test]
    fn the_stt_half_of_config_get_carries_the_locale_and_the_companion_switch() {
        let cfg = SttConfig::parse(&json!({"voice": {
            "stt.engine": "apple", "stt.model": "", "stt.language": "zh",
            "stt.locale": "zh-HK", "stt.prompted": true, "stt.companion": true
        }}));
        assert_eq!(cfg.language, "zh");
        assert_eq!(cfg.locale, "zh-HK", "the dialect the language alone does not name");
        assert!(cfg.companion && cfg.prompted);
        // A service that publishes neither key leaves both unsaid: no
        // locale, and the switch that sends audio off the Mac is not on.
        let older = SttConfig::parse(&json!({"voice": {
            "stt.engine": "whisper", "stt.language": "auto"
        }}));
        assert_eq!(older.locale, "");
        assert!(!older.companion);
        assert_eq!(SttConfig::parse(&Value::Null), SttConfig::default());
    }

    #[test]
    fn tts_status_speaks_the_voices_words() {
        let loaded = TtsStatus::parse(&json!({
            "engine": "kokoro", "active": true, "model": "kokoro-multi-lang-v1_1", "total": 364816464u64,
            "present": true, "loaded": true, "loading": false, "download": null,
            "voices": ["af_maple", "zm_010"]
        }));
        assert_eq!(
            loaded.line(),
            format!(
                "kokoro-multi-lang-v1_1 (365 MB) is loaded and speaks on this {HOST}. Agents speak as af_maple, zm_010."
            )
        );
        let idle = TtsStatus::parse(&json!({"engine": "cloud", "model": "kokoro-multi-lang-v1_1", "total": 364816464u64, "present": false}));
        assert!(idle.line().contains("nothing is until you ask"), "{}", idle.line());
        let running = TtsStatus::parse(&json!({
            "engine": "kokoro", "model": "kokoro-multi-lang-v1_1", "total": 364816464u64, "present": false,
            "download": {"model": "kokoro-multi-lang-v1_1", "bytes": 182408232u64, "total": 364816464u64, "running": true, "done": false, "error": null}
        }));
        assert!(running.downloading());
        assert!(running.line().ends_with("(50%)"), "{}", running.line());
        assert_eq!(TtsStatus::parse(&Value::Null), TtsStatus::default());
    }

    #[test]
    fn the_tts_half_of_config_get_carries_a_key_hint_and_never_a_key() {
        let config = json!({
            "api": {"port": 47201},
            "voice": {
                "tts.engine": "cloud",
                "tts.provider": "elevenlabs",
                "tts.api_key": "\u{2026}mnop (35 chars)",
                "tts.model": "",
                "tts.voices": ["Sarah", "George"],
            }
        });
        let tts = TtsConfig::parse(&config);
        assert!(tts.has_key());
        assert!(tts.cloud_selected());
        assert_eq!(tts.provider_row().map(|r| r.1), Some("ElevenLabs"));
        assert_eq!(tts.effective_model().as_deref(), Some("eleven_flash_v2_5"));
        assert_eq!(tts.voices, vec!["Sarah", "George"]);
        // No key is the empty hint, which is also the file's own spelling.
        let none = TtsConfig::parse(&json!({"voice": {"tts.engine": "cloud", "tts.provider": "openai", "tts.api_key": ""}}));
        assert!(!none.has_key());
        assert_eq!(none.effective_model().as_deref(), Some("gpt-4o-mini-tts"));
        // A provider this screen does not know has no row and no default.
        let odd = TtsConfig::parse(&json!({"voice": {"tts.provider": "acme", "tts.model": ""}}));
        assert!(odd.provider_row().is_none());
        assert!(odd.effective_model().is_none());
        // A configured model wins over the default.
        let own = TtsConfig::parse(&json!({"voice": {"tts.provider": "openai", "tts.model": "tts-1-hd"}}));
        assert_eq!(own.effective_model().as_deref(), Some("tts-1-hd"));
        assert_eq!(TtsConfig::parse(&Value::Null), TtsConfig::default());
    }

    #[test]
    fn the_engines_reply_carries_locales_and_never_a_voice_list() {
        let engines = Engines::parse(&engines_reply());
        assert_eq!(engines.stt().map(|e| e.id.as_str()), Some("apple"));
        assert_eq!(engines.locale.as_deref(), Some("en-US"));
        assert!(engines.stt().is_some_and(|e| e.on_device));
        assert_eq!(engines.stt().map(|e| e.locales.len()), Some(2));
        // §4.9: the TTS half reports the locales its voices cover, not the
        // voices. The pool is `[voice.voices] default_pool` plus
        // `state.json`, and a screen that read `voices` here would render an
        // empty list for ever.
        assert_eq!(
            engines.tts().map(|e| e.locales.as_slice()),
            Some(&["en-US".to_string()][..])
        );
        assert!(engines
            .tts()
            .is_some_and(|e| e.locales.iter().all(|l| !l.is_empty())));
        // A daemon with no engines at all answers `-32013`, but an empty
        // reply must still be a reply and not a panic.
        assert_eq!(Engines::parse(&Value::Null), Engines::default());
        assert!(Engines::parse(&json!({"stt": [], "tts": []}))
            .stt()
            .is_none());
        // An entry with no id is not an engine.
        assert!(Engines::parse(&json!({"stt": [{"on_device": true}]}))
            .stt()
            .is_none());
    }

    #[test]
    fn a_selected_engine_the_list_does_not_carry_falls_back_to_the_one_it_does() {
        // A `[voice] stt.engine` that names something the report does not
        // have is a config the daemon has not restarted for. Showing the
        // engine that is actually running beats showing nothing.
        let engines = Engines::parse(&json!({
            "stt": [{"id": "apple"}],
            "selected": {"stt": "whisper"}
        }));
        assert_eq!(engines.stt().map(|e| e.id.as_str()), Some("apple"));
        assert_eq!(engines.selected_stt.as_deref(), Some("whisper"));
    }

    #[test]
    fn the_speaker_test_says_the_chime_was_queued_and_keeps_the_daemons_caveat_about_it() {
        let queued = "`played` means the chime was queued on the audio thread, not that anybody \
                      heard it";
        let report = TestReport::parse(&json!({
            "what": "speaker",
            "route": "a2dp",
            "output_uid": "AirPods-out",
            "input_uid": "AirPods-in",
            "listening": false,
            "played": true,
            "chime": "ready",
            "limits": [queued, "the system default device and the system volume are never touched"]
        }));
        assert_eq!(report.what, "speaker");
        assert_eq!(report.route.as_deref(), Some("a2dp"));
        assert_eq!(report.chime.as_deref(), Some("ready"));
        assert!(report.made_a_sound());
        assert_eq!(report.limits.len(), 2);
        assert_eq!(report.limits[0], queued, "the caveat is the daemon's words");
        // A refused queue is the honest false, and it is not a sound.
        let refused = TestReport::parse(&json!({"what": "speaker", "played": false}));
        assert!(!refused.made_a_sound());
    }

    #[test]
    fn the_mic_test_reports_the_input_and_never_claims_to_have_opened_it() {
        let report = TestReport::parse(&json!({
            "what": "mic",
            "route": "none",
            "input_uid": "deck-in",
            "listening": false,
            "mic_warm": "card_pending",
            "mic_warm_effective": "never",
            "transport": "ble",
            "permissions": {"microphone": "unknown", "speech": "unknown"},
            "limits": ["the microphone is not opened: a capture is a PTT session with an owner"]
        }));
        assert_eq!(report.mic_warm.as_deref(), Some("card_pending"));
        assert_eq!(report.mic_warm_effective.as_deref(), Some("never"));
        assert_eq!(report.transport.as_deref(), Some("ble"));
        assert_eq!(report.input_uid.as_deref(), Some("deck-in"));
        // Nothing was queued and nothing was heard.
        assert_eq!(report.played, None);
        assert!(!report.made_a_sound());
        assert!(report.limits[0].contains("not opened"));
    }

    #[test]
    fn the_engine_tests_report_what_the_engine_said_about_itself_at_start_up() {
        let report = TestReport::parse(&json!({
            "what": "stt",
            "engine": {"id": "apple", "on_device": true, "locales": ["en-US"]},
            "configured": {"engine": "apple", "locale": "en-US", "allow_server": false},
            "limits": ["this is what the engine reported when it came up, not a run"]
        }));
        assert_eq!(report.engine.as_ref().map(|e| e.id.as_str()), Some("apple"));
        assert!(report.engine.as_ref().is_some_and(|e| e.on_device));
        assert!(!report.made_a_sound(), "nothing here makes a sound");
        assert!(report.limits[0].contains("not a run"));
    }

    #[test]
    fn a_permission_that_cannot_be_prompted_says_so_in_the_daemons_own_words() {
        let reply = json!({
            "which": "microphone",
            "status": "prompt_not_shown",
            "limits": [
                "the prompt is not shown: it needs the signed bundle of §10.1",
                "`prompt_not_shown` is §7.8's own answer for a prompt that did not surface"
            ]
        });
        let prompt = Prompt::parse(&reply);
        assert_eq!(prompt.which, "microphone");
        assert!(prompt.not_shown());
        assert_eq!(prompt.limits.len(), 2);
        assert!(prompt.limits[0].contains("signed bundle"));
        // A daemon that one day does show one is not this case.
        let granted = Prompt::parse(&json!({"which": "speech", "status": "granted"}));
        assert!(!granted.not_shown());
        assert_eq!(Prompt::parse(&Value::Null), Prompt::default());
    }

    #[test]
    fn the_four_checks_and_the_two_permissions_are_the_wires_own_words() {
        assert_eq!(TESTS, ["speaker", "mic", "stt", "tts"]);
        assert_eq!(TESTS[0], "speaker", "the default, and the only one heard");
        assert_eq!(PERMISSIONS, ["microphone", "speech"]);
        assert_eq!(PROMPT_NOT_SHOWN, "prompt_not_shown");
    }

    #[test]
    fn the_default_deck_permissions_include_bluetooth_and_the_mac_fallback() {
        assert_eq!(displayed_permissions(None), &BLE_PERMISSIONS);
        assert_eq!(
            displayed_permissions(Some(VoiceTransport::Ble)),
            &BLE_PERMISSIONS
        );
        assert_eq!(
            displayed_permissions(Some(VoiceTransport::Coreaudio)),
            &PERMISSIONS
        );
    }
}
