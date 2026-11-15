//! The Voice page (boards Voice, VoiceLight): what hears a press on the deck
//! and what speaks the reply — the language the recognisers listen for, the
//! two recognisers and the switch that lets Conol hear the presses meant for
//! its chats, the voices, where a press and a reply travel, a test from the
//! Mac, and the permissions this Mac grants the deck's voice.
//!
//! Everything shown is a reply the page keeps — `config.get`'s `[voice]`,
//! `stt.status`, `tts.status`, `voice.engines`, `care.test.status`, and
//! `perm.status`'s Speech Recognition, which is read whenever the page
//! opens — or the mirror's permissions. Every control writes the service's
//! config, or asks it to download, test or ask macOS, and redraws from what
//! it reads back. What the page does is [`crate::voice_actions`]; this file
//! is what it says.
//!
//! **The radios say what is chosen, the signal path what runs.** The default
//! synthesiser is `cloud`, which is Apple's voice until a key is set, so a
//! fresh Mac shows Apple voice on and not Cloud voice ([`synthesiser`]). A
//! local model that is chosen but not on this Mac (still coming down, or
//! one that would not load) hands its work to Apple's, and the signal path
//! names Apple then ([`signal_path`]); a cloud voice is drawn as the
//! provider's own service, since the reply's text goes there.
//!
//! The words are pure functions of those replies and a language, so a test
//! reads every row in every state without a window.
use crate::present;
use crate::shell::{Screen, Shell};
use crate::shots::Scene;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Layout, PopupButton, Row, Segmented, Switch, Type as _};
use crate::voice_actions::Prompts;
use deck_gui_core::care::{TestState, TestStatus, VoiceTest};
use deck_gui_core::host_state::{BLUETOOTH_SETTINGS, SPEECH_SETTINGS};
use deck_gui_core::i18n::Language;
use deck_gui_core::voice::{
    Download, Engines, SttConfig, SttStatus, TtsConfig, TtsStatus, PROVIDERS,
};
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Permission, Tone};
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Div, Entity, SharedString, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{h_flex, v_flex};
use serde_json::{json, Value};

/// Between the page's blocks, and between the two engine cards.
const GAP: f32 = 20.;
/// What the page's content is drawn at while the service is not answering:
/// its last replies stay on screen, dimmed, under the frame's strip that
/// says why.
const DIMMED: f32 = 0.55;
/// The signal path's column of route names.
const ROUTE_NAME: f32 = 104.;
/// Under the routes: the design's 14, and one more for the card's rule,
/// which the card draws over its child's last pixel where the design has a
/// border of its own.
const ROUTES_BOTTOM: f32 = 15.;
/// How tall the Speech language menu grows before it scrolls: Apple's
/// recogniser reports some sixty locales.
const LOCALE_MENU_HEIGHT: f32 = 320.;
/// How far the Conol row's sub-line may run into the 12 px gap before its
/// switch. The board's browser sets "Audio of those presses goes to Conol's
/// service." kerned, in 257 of the column's 260 px; gpui wraps on each
/// character's own advance, which makes the line 262. With 4 px of the gap
/// it breaks where the board's does, and nothing moves.
const CONOL_SUB_SLACK: f32 = 4.;

/// The cloud voice's key: the dialog's id, and its width.
pub(crate) const ADD_KEY: &str = "add-key";
const ADD_KEY_WIDTH: f32 = 420.;

/// The `config.set` names the page writes.
pub mod keys {
    /// `apple` or `whisper`.
    pub const STT_ENGINE: &str = "voice.stt.engine";
    /// `auto`, or the language code Whisper is pinned to.
    pub const STT_LANGUAGE: &str = "voice.stt.language";
    /// The locale Apple's recogniser hears.
    pub const STT_LOCALE: &str = "voice.stt.locale";
    /// Whether Conol hears the presses meant for its chats.
    pub const STT_COMPANION: &str = "voice.stt.companion";
    /// `apple`, `kokoro`, `cloud` or `mlx`.
    pub const TTS_ENGINE: &str = "voice.tts.engine";
    pub const TTS_PROVIDER: &str = "voice.tts.provider";
    pub const TTS_API_KEY: &str = "voice.tts.api_key";
    pub const TTS_MODEL: &str = "voice.tts.model";
}

// ---- what the page says, as pure functions ---------------------------------

/// A recogniser the Recognition card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recogniser {
    /// Apple's recogniser, built into macOS.
    Apple,
    /// A Whisper model on this Mac.
    Whisper,
}

impl Recogniser {
    /// Its `voice.stt.engine` word.
    pub const fn id(self) -> &'static str {
        match self {
            Recogniser::Apple => "apple",
            Recogniser::Whisper => "whisper",
        }
    }
}

/// The system's own voice, by its maker's name (never translated): what
/// speaks when nothing else is chosen.
pub const SYSTEM_VOICE: &str = if cfg!(windows) { "Windows" } else { "Apple" };

/// One of a sentence's two forms by where the app runs: `mac`'s, or on
/// Windows `pc`'s (a page's words that are about macOS itself, not only its
/// name, which `Language::text` already says as "PC" there).
pub(crate) const fn here(mac: &'static str, pc: &'static str) -> &'static str {
    if cfg!(windows) {
        pc
    } else {
        mac
    }
}

/// The Recognition radio that is on: the engine `config.get` names. `None`
/// for a word this window does not know, which no radio claims. Windows has
/// no recogniser of its own: there, the default word is Whisper too.
pub fn recogniser(config: &SttConfig) -> Option<Recogniser> {
    match config.engine.as_str() {
        "apple" if cfg!(windows) => Some(Recogniser::Whisper),
        "apple" => Some(Recogniser::Apple),
        "whisper" => Some(Recogniser::Whisper),
        _ => None,
    }
}

/// A voice the Spoken replies card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synthesiser {
    /// Apple's voice, built into macOS.
    Apple,
    /// A Kokoro voice on this Mac.
    Kokoro,
    /// A provider's voice behind the owner's own key.
    Cloud,
    /// The MLX model on this Mac, which only the command line sets up.
    Mlx,
}

impl Synthesiser {
    /// Its `voice.tts.engine` word.
    pub const fn id(self) -> &'static str {
        match self {
            Synthesiser::Apple => "apple",
            Synthesiser::Kokoro => "kokoro",
            Synthesiser::Cloud => "cloud",
            Synthesiser::Mlx => "mlx",
        }
    }
}

/// The Spoken replies radio that is on. `cloud` is the service's default
/// and speaks with Apple's voice until a key is set, so without a key it is
/// Apple voice's radio, not Cloud voice's. `None` for a word this window
/// does not know.
pub fn synthesiser(tts: &TtsConfig) -> Option<Synthesiser> {
    match tts.engine.as_str() {
        "apple" => Some(Synthesiser::Apple),
        "cloud" if tts.has_key() => Some(Synthesiser::Cloud),
        "cloud" => Some(Synthesiser::Apple),
        "kokoro" => Some(Synthesiser::Kokoro),
        "mlx" => Some(Synthesiser::Mlx),
        _ => None,
    }
}

/// What a local model's row says at its right: whether the model is on this
/// Mac, and what can be done about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// On this Mac: "Installed".
    Installed,
    /// Not on this Mac: **Download · 1.6 GB**. `bytes` is 0 for a model the
    /// service does not catalogue, which it cannot fetch either: no button.
    Download { bytes: u64 },
    /// Coming down: the row's sub-line says how far (`None` while the size
    /// is not known) and **Cancel** stops it.
    Downloading { percent: Option<u8> },
    /// On this Mac and being read into memory: "Loading…".
    Loading,
    /// The download stopped, or the model would not load: "Failed", with the
    /// service's reason on hover, and **Retry**. `load` tells the two apart:
    /// a model that would not load is fetched again whole, since the service
    /// does not try one file twice.
    Failed { why: String, load: bool },
}

/// The Whisper row's slot, from `stt.status`.
pub fn stt_slot(status: &SttStatus) -> Slot {
    slot(
        status.present,
        status.loading,
        status.load_error.as_deref(),
        status.download.as_ref(),
        status.total,
    )
}

/// The Kokoro row's slot, from `tts.status`.
pub fn tts_slot(status: &TtsStatus) -> Slot {
    slot(
        status.present,
        status.loading,
        status.load_error.as_deref(),
        status.download.as_ref(),
        status.total,
    )
}

/// A local model's slot: a download that runs comes first, then a model
/// that is not here (failed or never fetched), then one that is here and
/// would not load, is loading, or is ready.
fn slot(
    present: bool,
    loading: bool,
    load_error: Option<&str>,
    download: Option<&Download>,
    total: u64,
) -> Slot {
    if let Some(download) = download.filter(|download| download.running) {
        return Slot::Downloading {
            percent: percent(download.bytes, download.total),
        };
    }
    if !present {
        return match download.and_then(|download| download.error.as_deref()) {
            Some(why) => Slot::Failed {
                why: why.to_string(),
                load: false,
            },
            None => Slot::Download { bytes: total },
        };
    }
    match load_error {
        Some(why) => Slot::Failed {
            why: why.to_string(),
            load: true,
        },
        None if loading => Slot::Loading,
        None => Slot::Installed,
    }
}

/// How far a download is, in whole percent, rounded down so 100 is said
/// only when it is whole. `None` while its size is not known.
pub fn percent(bytes: u64, total: u64) -> Option<u8> {
    if total == 0 {
        return None;
    }
    let done = u128::from(bytes.min(total)) * 100 / u128::from(total);
    u8::try_from(done).ok()
}

/// Whether a local model can do its work: on this Mac, and not refused by
/// the engine that loads it.
fn usable(present: bool, load_error: Option<&str>) -> bool {
    present && load_error.is_none()
}

/// What hears a press now: Whisper while it is chosen and on this Mac, else
/// Apple's recogniser, which answers until the model is whole. Before
/// `stt.status` has answered, what is chosen. `None` for an engine this
/// window does not know.
pub fn recognises(config: &SttConfig, status: Option<&SttStatus>) -> Option<Recogniser> {
    match recogniser(config)? {
        Recogniser::Whisper
            if status.is_some_and(|s| !usable(s.present, s.load_error.as_deref())) =>
        {
            Some(Recogniser::Apple)
        }
        chosen => Some(chosen),
    }
}

/// A provider's name as it calls itself, or its id when this window does not
/// know it. Never translated.
pub fn provider_name(id: &str) -> &str {
    PROVIDERS
        .iter()
        .find(|(known, _, _)| known.eq_ignore_ascii_case(id))
        .map_or(id, |(_, name, _)| name)
}

/// One stop on a route the signal path draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The deck's microphone, where every route starts.
    Microphone,
    /// Software on this Mac, by its own name: Apple, Whisper, Kokoro,
    /// Qwen3-TTS.
    OnMac(&'static str),
    /// A provider's service, by its name: the reply's text goes there.
    Cloud(String),
    /// The agent session a press is for.
    Agent,
    /// The deck's speaker, where a reply ends.
    Speaker,
    /// Conol's recogniser, on its own service.
    ConolCloud,
    /// The Conol chat a press is for.
    Chat,
    /// An engine this window does not know, by the service's word for it.
    Other(String),
}

/// A route of the signal path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A press to an agent session, and its reply.
    Agents,
    /// A press to a Conol chat, while Conol hears those.
    Conol,
}

impl Route {
    /// The route's name, in `language`.
    pub fn name(self, language: Language) -> &'static str {
        match self {
            Route::Agents => language.text("Agent sessions"),
            Route::Conol => language.text("Conol chats"),
        }
    }
}

/// What speaks a reply now: Kokoro while it is chosen and on this Mac, the
/// provider's service while the cloud voice has a key, the MLX model, and
/// Apple's voice otherwise (chosen, or standing in).
pub fn speaks(tts: &TtsConfig, status: Option<&TtsStatus>) -> Stop {
    match synthesiser(tts) {
        Some(Synthesiser::Kokoro)
            if status.is_none_or(|s| usable(s.present, s.load_error.as_deref())) =>
        {
            Stop::OnMac("Kokoro")
        }
        Some(Synthesiser::Cloud) => Stop::Cloud(provider_name(&tts.provider).to_string()),
        Some(Synthesiser::Mlx) => Stop::OnMac("Qwen3-TTS"),
        Some(_) => Stop::OnMac(SYSTEM_VOICE),
        None => Stop::Other(tts.engine.clone()),
    }
}

/// The signal path: a press to an agent and its reply, each stop what runs
/// now ([`recognises`], [`speaks`]); then, while Conol hears the presses
/// meant for its chats (`voice.stt.companion`), that route, which leaves
/// this Mac.
pub fn signal_path(
    stt: &SttConfig,
    stt_status: Option<&SttStatus>,
    tts: &TtsConfig,
    tts_status: Option<&TtsStatus>,
) -> Vec<(Route, Vec<Stop>)> {
    let hears = match recognises(stt, stt_status) {
        Some(Recogniser::Whisper) => Stop::OnMac("Whisper"),
        // Nothing stands in for Whisper on Windows until its model is whole.
        Some(Recogniser::Apple) if cfg!(windows) => Stop::Other(String::new()),
        Some(Recogniser::Apple) => Stop::OnMac("Apple"),
        None => Stop::Other(stt.engine.clone()),
    };
    let mut routes = vec![(
        Route::Agents,
        vec![
            Stop::Microphone,
            hears,
            Stop::Agent,
            speaks(tts, tts_status),
            Stop::Speaker,
        ],
    )];
    if stt.companion {
        routes.push((
            Route::Conol,
            vec![Stop::Microphone, Stop::ConolCloud, Stop::Chat],
        ));
    }
    routes
}

/// What the last voice test came to, as the test row says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestLine {
    /// The deck is asking the owner to hold PTT and speak.
    Waiting,
    /// What the deck heard.
    Heard(String),
    /// It heard nothing, or nobody answered in time.
    Nothing,
    /// No deck was linked to ask.
    NoDeck,
}

impl TestLine {
    /// The line, in `language`.
    pub fn words(&self, language: Language) -> String {
        match self {
            TestLine::Waiting => language
                .text("Waiting for you to hold PTT on the deck\u{2026}")
                .to_string(),
            TestLine::Heard(words) => {
                language.format("The last test heard \u{201c}{0}\u{201d}.", &[words])
            }
            TestLine::Nothing => language.text("The last test heard nothing.").to_string(),
            TestLine::NoDeck => language.text("Link the deck first.").to_string(),
        }
    }
}

/// The test row's result line for the last voice test; `None` before the
/// first (the service keeps one for as long as it runs), and for a state
/// that is not a voice test's.
pub fn test_line(test: Option<&VoiceTest>) -> Option<TestLine> {
    let test = test?;
    Some(match &test.state {
        TestState::Waiting | TestState::Sent => TestLine::Waiting,
        TestState::Heard => match &test.heard {
            Some(words) => TestLine::Heard(words.clone()),
            None => TestLine::Nothing,
        },
        TestState::Expired => TestLine::Nothing,
        TestState::NoDeck => TestLine::NoDeck,
        TestState::Answered | TestState::Unknown(_) => return None,
    })
}

/// What can be done about a permission from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// **Ask…**: macOS asks the owner.
    Ask,
    /// **Open Settings**: macOS will not ask twice, so only the owner can
    /// change it, in System Settings.
    OpenSettings,
}

/// A permission as its row says it: the state in a word, the word's tone,
/// and what is offered beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub word: &'static str,
    pub tone: Tone,
    pub offer: Option<Offer>,
}

/// A permission's word and tone: granted is healthy, denied needs the owner
/// (the one with colour behind it), not asked and not reported are neutral.
fn grant_word(state: Permission, language: Language) -> (&'static str, Tone) {
    match state {
        Permission::Granted => (language.text("Granted"), Tone::Green),
        Permission::NotDetermined => (language.text("Not asked"), Tone::Gray),
        Permission::Denied => (language.text("Denied"), Tone::Amber),
        Permission::Unknown => (language.text("Not checked"), Tone::Gray),
    }
}

/// Bluetooth, for the deck's microphone and speaker over the air. It is
/// never asked for from here: macOS asks by itself when the link starts.
pub fn bluetooth_grant(state: Permission, language: Language) -> Grant {
    let (word, tone) = grant_word(state, language);
    let offer = (state == Permission::Denied).then_some(Offer::OpenSettings);
    Grant { word, tone, offer }
}

/// Speech Recognition, which only Apple's recogniser needs. **Ask…** only
/// while macOS has not been asked (`not_determined`), Apple's is what hears
/// a press (`apple`), and the service has not said it cannot show the prompt
/// (`refused`): offering it again could not work. **Open Settings** once
/// denied. A state the service did not read is "Not checked", with nothing
/// offered: there is nothing known to act on, and asking macOS about a
/// grant it may already have made would only say so.
pub fn speech_grant(state: Permission, apple: bool, refused: bool, language: Language) -> Grant {
    let (word, tone) = grant_word(state, language);
    let offer = match state {
        Permission::Denied => Some(Offer::OpenSettings),
        Permission::NotDetermined if apple && !refused => Some(Offer::Ask),
        _ => None,
    };
    Grant { word, tone, offer }
}

/// What `perm.status` says of Speech Recognition, as the service reads it
/// from macOS. `None` from a reply that does not say, or says it in a word
/// this build does not know.
pub fn speech_reported(result: &Value) -> Option<Permission> {
    serde_json::from_value(result.get("speech")?.clone()).ok()
}

/// The Speech Recognition row's state: the mirror's while it knows (the
/// service's `perm.changed` keeps it current), else what `perm.status` said
/// when the page last asked, else not checked.
pub fn speech_state(mirror: Permission, reported: Option<Permission>) -> Permission {
    match mirror {
        Permission::Unknown => reported.unwrap_or(Permission::Unknown),
        known => known,
    }
}

/// The locales this window has names for, in the order the Speech language
/// popup lists them first: the sixteen Whisper reports, with Cantonese as
/// Apple's recogniser reports it.
pub const NAMED_LOCALES: [&str; 18] = [
    "en-US", "en-GB", "zh-CN", "zh-TW", "zh-HK", "yue-CN", "ja-JP", "ko-KR", "de-DE", "fr-FR",
    "es-ES", "it-IT", "pt-BR", "ru-RU", "nl-NL", "pl-PL", "tr-TR", "vi-VN",
];

/// A locale's name in `language`; `None` for one this window has no name
/// for, which the popup shows by its code.
pub fn locale_name(code: &str, language: Language) -> Option<&'static str> {
    Some(match code {
        "en-US" => language.text("English (United States)"),
        "en-GB" => language.text("English (United Kingdom)"),
        "zh-CN" => language.text("Chinese (China mainland)"),
        "zh-TW" => language.text("Chinese (Taiwan)"),
        "zh-HK" => language.text("Cantonese (Hong Kong)"),
        "yue-CN" => language.text("Cantonese (China mainland)"),
        "ja-JP" => language.text("Japanese (Japan)"),
        "ko-KR" => language.text("Korean (South Korea)"),
        "de-DE" => language.text("German (Germany)"),
        "fr-FR" => language.text("French (France)"),
        "es-ES" => language.text("Spanish (Spain)"),
        "it-IT" => language.text("Italian (Italy)"),
        "pt-BR" => language.text("Portuguese (Brazil)"),
        "ru-RU" => language.text("Russian (Russia)"),
        "nl-NL" => language.text("Dutch (Netherlands)"),
        "pl-PL" => language.text("Polish (Poland)"),
        "tr-TR" => language.text("Turkish (Türkiye)"),
        "vi-VN" => language.text("Vietnamese (Vietnam)"),
        _ => return None,
    })
}

/// A locale's language: `zh` of `zh-CN`, `yue` of `yue-CN`, `es` of
/// `es-419`.
pub fn locale_language(locale: &str) -> &str {
    locale.split(['-', '_']).next().unwrap_or(locale)
}

/// What the Speech language popup shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechChoice {
    /// Whisper detects the language of each press (`voice.stt.language =
    /// "auto"`); Apple's recogniser keeps its locale.
    Automatic,
    /// One locale, which both recognisers hear.
    Locale(String),
    /// A language set apart from the locale (by hand, in the config file),
    /// by its code.
    Language(String),
}

impl SpeechChoice {
    /// The popup's value, in `language`.
    pub fn word(&self, language: Language) -> String {
        match self {
            SpeechChoice::Automatic => language.text("Automatic").to_string(),
            SpeechChoice::Locale(code) => {
                locale_name(code, language).map_or_else(|| code.clone(), str::to_string)
            }
            SpeechChoice::Language(code) if code.is_empty() => "\u{2014}".to_string(),
            SpeechChoice::Language(code) => code.clone(),
        }
    }
}

/// The popup's value from `[voice.stt]`: Automatic for `auto`; the locale
/// when the language follows it (empty, or the locale's own); else the
/// language alone.
pub fn speech_choice(config: &SttConfig) -> SpeechChoice {
    let language = config.language.trim();
    let locale = config.locale.trim();
    if language.eq_ignore_ascii_case("auto") {
        return SpeechChoice::Automatic;
    }
    if !locale.is_empty()
        && (language.is_empty() || locale_language(locale).eq_ignore_ascii_case(language))
    {
        return SpeechChoice::Locale(locale.to_string());
    }
    SpeechChoice::Language(language.to_string())
}

/// The locales the popup offers after Automatic: those the recogniser in use
/// reports (`voice.engines`), the ones this window has names for first, in
/// their order, then the rest as reported; and the configured locale when it
/// is not among them, so the value shown can always be chosen again.
pub fn speech_locales(engines: Option<&Engines>, config: &SttConfig) -> Vec<String> {
    let reported: Vec<&str> = engines
        .and_then(Engines::stt)
        .map(|engine| engine.locales.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let mut locales: Vec<String> = NAMED_LOCALES
        .iter()
        .filter(|code| reported.contains(code))
        .map(|code| (*code).to_string())
        .collect();
    for code in reported {
        if !locales.iter().any(|known| known == code) {
            locales.push(code.to_string());
        }
    }
    let configured = config.locale.trim();
    if !configured.is_empty() && !locales.iter().any(|known| known == configured) {
        locales.push(configured.to_string());
    }
    locales
}

/// What choosing in the Speech language popup writes. Automatic is
/// `voice.stt.language = "auto"`; a locale is its language and itself in one
/// patch, so Whisper and Apple's recogniser change together or not at all.
pub fn speech_language_action(locale: Option<&str>) -> UiAction {
    match locale {
        None => UiAction::ConfigSet {
            key: keys::STT_LANGUAGE.into(),
            value: ConfigValue::Text("auto".into()),
        },
        Some(locale) => UiAction::ConfigPatch {
            entries: vec![
                (
                    keys::STT_LANGUAGE.into(),
                    ConfigValue::Text(locale_language(locale).into()),
                ),
                (keys::STT_LOCALE.into(), ConfigValue::Text(locale.into())),
            ],
        },
    }
}

/// The key as the Cloud voice row shows it: the hint's tail (`…mnop`), which
/// tells two keys apart and is never the key. The service publishes
/// `…mnop (35 chars)`, or `8 chars` for a short one.
pub fn key_tail(hint: &str) -> &str {
    hint.split(" (").next().unwrap_or(hint).trim()
}

/// Where the key dialog says the key is kept: the Keychain when the service
/// says it keeps it there, else the service's settings (config.toml: Linux,
/// a service on a data directory of its own, one from before the Keychain).
pub fn key_kept(tts: Option<&TtsConfig>, language: Language) -> &'static str {
    if tts.is_some_and(TtsConfig::in_keychain) {
        language.text(here(
            "The key is kept in your Keychain on this Mac.",
            "The key is kept in Windows Credential Manager on this PC.",
        ))
    } else {
        language.text(here(
            "The key is kept in the service's settings on this Mac.",
            "The key is kept in the service's settings on this PC.",
        ))
    }
}

/// The provider the key dialog opens on: the configured one, when this
/// window knows it, else the first.
pub fn dialog_provider(tts: Option<&TtsConfig>) -> &'static str {
    tts.and_then(TtsConfig::provider_row)
        .map_or(PROVIDERS[0].0, |(id, _, _)| id)
}

/// What **Save Key** writes, in one patch: the provider, the key, and the
/// cloud voice as the one that speaks — the dialog belongs to Cloud voice's
/// row, so saving a key there is choosing it. A provider that changes takes
/// its own default model: the dialog has no model field, and a model one
/// provider names is refused by the other.
pub fn save_key_action(provider: &str, key: &str, current: Option<&TtsConfig>) -> UiAction {
    let mut entries = vec![
        (
            keys::TTS_PROVIDER.to_string(),
            ConfigValue::Text(provider.to_string()),
        ),
        (
            keys::TTS_API_KEY.to_string(),
            ConfigValue::Text(key.to_string()),
        ),
        (
            keys::TTS_ENGINE.to_string(),
            ConfigValue::Text(Synthesiser::Cloud.id().to_string()),
        ),
    ];
    let stale_model = current
        .is_some_and(|tts| !tts.model.is_empty() && !tts.provider.eq_ignore_ascii_case(provider));
    if stale_model {
        entries.push((keys::TTS_MODEL.to_string(), ConfigValue::Null));
    }
    UiAction::ConfigPatch { entries }
}

// ---- the harness's scenes ----------------------------------------------------

/// The boards' last voice test, as the service's own fixture has it: it
/// heard "hello".
const BOARD_TEST: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_test_status.json");

/// The Whisper model and the Kokoro voice the service catalogues by
/// default, by name and size: what `stt.status` and `tts.status` report.
const WHISPER: (&str, u64) = ("large-v3-turbo", 1_624_555_275);
const KOKORO: (&str, u64) = ("kokoro-multi-lang-v1_1", 364_855_331);

/// The sixteen locales Whisper reports.
const WHISPER_LOCALES: [&str; 16] = [
    "en-US", "en-GB", "zh-CN", "zh-TW", "ja-JP", "ko-KR", "de-DE", "fr-FR", "es-ES", "it-IT",
    "pt-BR", "ru-RU", "nl-NL", "pl-PL", "tr-TR", "vi-VN",
];

/// What a harness scene puts in the page: the replies it would have asked
/// the service for, each parsed by the parser it reads the service with.
#[derive(Clone, Debug, PartialEq)]
pub struct Staged {
    pub engines: Engines,
    pub stt_config: SttConfig,
    pub tts: TtsConfig,
    pub whisper: SttStatus,
    pub kokoro: TtsStatus,
    pub test: TestStatus,
    /// The cloud voice's key dialog is up.
    pub add_key: bool,
}

/// The page's scenes:
///
/// * `voice` (boards Voice and VoiceLight): Automatic; Whisper and Kokoro
///   chosen and on this Mac; Conol on; no key; the last test heard "hello".
/// * `voice-downloading`: one locale chosen; Whisper chosen and coming down,
///   so Apple's recogniser hears in the meantime; the cloud voice with no
///   key, so Apple's voice speaks, and the Kokoro voice not fetched; the
///   last test heard nothing.
/// * `voice-key`: Apple's recogniser, with the Whisper model on this Mac
///   anyway; an ElevenLabs key, so the cloud voice speaks; the Kokoro
///   voice's download failed; Conol off; the key dialog up.
///
/// And first run's `first-voice`, which draws this page's two engine cards
/// ([`Shell::render_voice_engines`]): a fresh Mac, as a new service answers
/// — Apple's recogniser and the cloud voice with no key (so Apple's voice),
/// neither model fetched, Conol on.
pub fn staged(scene: Scene) -> Option<Staged> {
    let config = |stt: Value, tts: Value| {
        let mut voice = serde_json::Map::new();
        for (key, value) in [stt, tts].iter().filter_map(Value::as_object).flatten() {
            voice.insert(key.clone(), value.clone());
        }
        json!({ "voice": voice })
    };
    let engines = |stt: &str| {
        Engines::parse(&json!({
            "selected": {"stt": stt, "tts": "apple", "locale": "en-US"},
            "stt": [{"id": stt, "on_device": true, "locales": WHISPER_LOCALES}],
            "tts": [{"id": "apple", "on_device": true, "locales": ["en-US", "zh-CN"]}],
        }))
    };
    let model = |engine: &str, (name, total): (&str, u64), present: bool, loaded: bool| {
        json!({
            "engine": engine, "model": name, "total": total, "present": present,
            "loaded": loaded, "loading": false, "load_error": null, "download": null,
        })
    };
    let (staged, add_key) = match scene {
        Scene::Voice => (
            (
                engines("whisper"),
                config(
                    json!({"stt.engine": "whisper", "stt.language": "auto", "stt.locale": "en-US",
                           "stt.companion": true, "stt.prompted": true}),
                    json!({"tts.engine": "kokoro", "tts.provider": "openai", "tts.api_key": "",
                           "tts.model": ""}),
                ),
                model("whisper", WHISPER, true, true),
                model("kokoro", KOKORO, true, true),
                serde_json::from_str::<Value>(BOARD_TEST).unwrap_or_default(),
            ),
            false,
        ),
        Scene::VoiceDownloading => {
            let mut whisper = model("whisper", WHISPER, false, false);
            whisper["download"] = json!({
                "model": WHISPER.0, "total": WHISPER.1, "bytes": 682_400_000,
                "running": true, "done": false, "error": null,
            });
            (
                (
                    engines("whisper"),
                    config(
                        json!({"stt.engine": "whisper", "stt.language": "zh",
                               "stt.locale": "zh-CN", "stt.companion": true}),
                        json!({"tts.engine": "cloud", "tts.provider": "openai",
                               "tts.api_key": "", "tts.model": ""}),
                    ),
                    whisper,
                    model("cloud", KOKORO, false, false),
                    json!({"question": null, "voice": {
                        "state": "expired", "at": "2026-11-11T09:57:30.000Z", "heard": null}}),
                ),
                false,
            )
        }
        Scene::VoiceKey => {
            let mut kokoro = model("cloud", KOKORO, false, false);
            kokoro["download"] = json!({
                "model": KOKORO.0, "total": KOKORO.1, "bytes": 0, "running": false,
                "done": false, "error": "curl: (6) Could not resolve host: github.com",
            });
            (
                (
                    engines("apple"),
                    config(
                        json!({"stt.engine": "apple", "stt.language": "auto",
                               "stt.locale": "en-US", "stt.companion": false}),
                        json!({"tts.engine": "cloud", "tts.provider": "elevenlabs",
                               "tts.api_key": "\u{2026}mnop (35 chars)",
                               "tts.key_store": "keychain", "tts.model": ""}),
                    ),
                    model("apple", WHISPER, true, false),
                    kokoro,
                    json!({"question": null, "voice": null}),
                ),
                true,
            )
        }
        Scene::FirstVoice => (
            (
                engines("apple"),
                config(
                    json!({"stt.engine": "apple", "stt.language": "auto", "stt.locale": "en-US",
                           "stt.companion": true, "stt.prompted": false}),
                    json!({"tts.engine": "cloud", "tts.provider": "openai", "tts.api_key": "",
                           "tts.model": ""}),
                ),
                model("apple", WHISPER, false, false),
                model("cloud", KOKORO, false, false),
                json!({"question": null, "voice": null}),
            ),
            false,
        ),
        _ => return None,
    };
    let (engines, config, whisper, kokoro, test) = staged;
    Some(Staged {
        engines,
        stt_config: SttConfig::parse(&config),
        tts: TtsConfig::parse(&config),
        whisper: SttStatus::parse(&whisper),
        kokoro: TtsStatus::parse(&kokoro),
        test: TestStatus::parse(&test),
        add_key,
    })
}

// ---- the pane ----------------------------------------------------------------

/// The page's own state, which the shell owns: the replies nothing folds
/// into the mirror, what is in flight, and the key dialog's field.
pub struct VoicePane {
    /// `voice.engines`: the locales the recognisers report. `None` before
    /// it answers, and from a service with no audio engine.
    pub(crate) engines: Option<Engines>,
    /// `[voice.stt]` and `[voice.tts]`, as `config.get` last published them.
    pub(crate) stt_config: Option<SttConfig>,
    pub(crate) tts: Option<TtsConfig>,
    /// `config.get` is not a method of this service: nothing here can be read.
    pub(crate) config_missing: bool,
    /// `stt.status` and `tts.status`: the Whisper model and the Kokoro voice.
    pub(crate) whisper: Option<SttStatus>,
    pub(crate) kokoro: Option<TtsStatus>,
    /// A re-read of either is scheduled while it downloads or loads.
    pub(crate) whisper_polling: bool,
    pub(crate) kokoro_polling: bool,
    /// `care.test.status`'s voice half: the last voice test.
    pub(crate) test: Option<VoiceTest>,
    /// The service has no voice test (`care.test.*` answer `-32601`).
    pub(crate) tests_missing: bool,
    /// A `care.test.status` is in flight.
    pub(crate) test_asking: bool,
    /// The last `care.test.status` failed: the test is no longer followed
    /// until the page reads everything again.
    pub(crate) test_failed: bool,
    /// What `perm.request` answered, per permission.
    pub(crate) prompts: Prompts,
    /// What `perm.status` last said of Speech Recognition; `None` until it
    /// has, and from a service that does not say.
    pub(crate) speech: Option<Permission>,
    /// A `perm.status` is in flight.
    pub(crate) speech_asking: bool,
    /// The key dialog's masked field, and the provider chosen in it. The
    /// field is emptied once a key is saved, and once the dialog has gone:
    /// the window never keeps a key.
    pub(crate) key: Entity<InputState>,
    pub(crate) provider: &'static str,
    pub(crate) key_open: bool,
    /// `config.get` has answered once; `busy` while it is asked.
    pub(crate) asked: bool,
    pub(crate) busy: bool,
    /// A `config.set` of the page's is in flight.
    pub(crate) writing: bool,
}

impl VoicePane {
    /// The pane, and the subscription that saves the key on Return.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> (VoicePane, Vec<Subscription>) {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste the provider's API key\u{2026}")
                .masked(true)
        });
        let save = cx.subscribe_in(&key, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_cloud_key(window, cx);
            }
        });
        let pane = VoicePane {
            engines: None,
            stt_config: None,
            tts: None,
            config_missing: false,
            whisper: None,
            kokoro: None,
            whisper_polling: false,
            kokoro_polling: false,
            test: None,
            tests_missing: false,
            test_asking: false,
            test_failed: false,
            prompts: Prompts::default(),
            speech: None,
            speech_asking: false,
            key,
            provider: PROVIDERS[0].0,
            key_open: false,
            asked: false,
            busy: false,
            writing: false,
        };
        (pane, vec![save])
    }

    /// Put a harness scene's replies in the pane, as if the service had
    /// answered with them, and count them as asked: a staged page asks the
    /// service nothing.
    fn stage(&mut self, staged: Staged) {
        self.engines = Some(staged.engines);
        self.stt_config = Some(staged.stt_config);
        self.tts = Some(staged.tts);
        self.config_missing = false;
        self.whisper = Some(staged.whisper);
        self.kokoro = Some(staged.kokoro);
        self.test = staged.test.voice;
        self.tests_missing = false;
        self.test_asking = false;
        self.test_failed = false;
        self.prompts = Prompts::default();
        // The scene's mirror says what the permissions are.
        self.speech = None;
        self.speech_asking = false;
        self.asked = true;
        self.busy = false;
        self.writing = false;
    }

    /// What hears a press now, as far as the pane can tell.
    fn recognises(&self) -> Option<Recogniser> {
        recognises(self.stt_config.as_ref()?, self.whisper.as_ref())
    }
}

/// The Whisper model or the Kokoro voice: a local model's row offers the
/// same things for either, each asked of its own methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Whisper,
    Kokoro,
}

/// The ids on a local model's row: its download, cancel and retry buttons,
/// and the failed state's word (it carries the reason on hover).
struct ModelIds {
    download: &'static str,
    cancel: &'static str,
    retry: &'static str,
    failed: &'static str,
}

impl Model {
    const fn ids(self) -> ModelIds {
        match self {
            Model::Whisper => ModelIds {
                download: "voice-whisper-download",
                cancel: "voice-whisper-cancel",
                retry: "voice-whisper-retry",
                failed: "voice-whisper-failed",
            },
            Model::Kokoro => ModelIds {
                download: "voice-kokoro-download",
                cancel: "voice-kokoro-cancel",
                retry: "voice-kokoro-retry",
                failed: "voice-kokoro-failed",
            },
        }
    }
}

impl Shell {
    /// The page. While the service is not answering, everything under the
    /// header is its last answer, dimmed, with every control off.
    pub(crate) fn render_voice(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let header = ui::Header::new(
            self.tr(Screen::Voice.label()),
            self.tr("Push to talk, and replies read on the deck's speaker."),
        );
        let content = if self.voice.config_missing {
            v_flex().child(ui::hint(
                self.tr("This needs a newer background service."),
                &p,
            ))
        } else {
            v_flex()
                .gap(px(GAP))
                .child(self.speech_language_card(dim, &p, cx))
                .child(self.voice_engine_cards(dim, true, &p, cx))
                .child(self.signal_path_section(dim, &p, cx))
                // macOS's grants; Windows asks for neither.
                .when(cfg!(not(windows)), |page| {
                    page.child(self.permissions_section(dim, &p, cx))
                })
        };
        let body = v_flex()
            .gap(px(GAP))
            .child(header)
            .child(content.when(dim, |content| content.opacity(DIMMED)));
        ui::page("voice", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// The page's dialog `id` and its width: the cloud voice's key.
    pub(crate) fn voice_dialog(
        &self,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        (id == ADD_KEY).then(|| (self.render_add_key(cx), ADD_KEY_WIDTH))
    }

    /// The key dialog went: the tick empties its field.
    pub(crate) fn voice_dialog_closed(&mut self, id: &'static str, _cx: &mut Context<Self>) {
        if id == ADD_KEY {
            self.voice.key_open = false;
        }
    }

    /// The key field's placeholder, in the window's language.
    pub(crate) fn localize_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.tr("Paste the provider's API key\u{2026}");
        self.voice.key.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    /// Nothing to do: the permissions the mirror has are read as they are
    /// drawn, and nothing else the page shows arrives as an event.
    pub(crate) fn on_voice_event(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Once a second: a key typed into a dialog that went without saving it
    /// is let go of, and a voice test that waits on the deck is read again.
    pub(crate) fn tick_voice(&mut self, _beat: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.voice.key_open && !self.voice.key.read(cx).value().is_empty() {
            self.voice
                .key
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let waiting = self
            .voice
            .test
            .as_ref()
            .is_some_and(|test| test.state.pending());
        if waiting && !self.voice.tests_missing && !self.voice.test_failed {
            self.refresh_voice_test(window, cx);
        }
    }

    /// The page's scenes ([`staged`]): the replies go in the pane, the page
    /// opens, and `voice-key` puts the key dialog up. First run's
    /// `first-voice` gets the pane its engine cards read, and is first run's
    /// to open.
    pub(crate) fn stage_voice(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(staged) = staged(scene) else {
            return false;
        };
        let add_key = staged.add_key;
        self.voice.stage(staged);
        if scene == Scene::FirstVoice {
            return false;
        }
        self.go(Screen::Voice, window, cx);
        if add_key {
            self.open_add_key(window, cx);
        }
        true
    }

    /// The Recognition and Spoken replies cards, one over the other, for
    /// first run's narrower column.
    pub(crate) fn render_voice_engines(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        self.voice_engine_cards(dim, false, &p, cx)
            .into_any_element()
    }

    // ---- the language --------------------------------------------------------

    /// The one card above the engines: the language they listen for.
    fn speech_language_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let row = Row::new()
            .title(self.tr("Speech language"))
            .sub(self.tr(here(
                "Whisper detects the language. Apple's recogniser needs one chosen here.",
                "Whisper detects the language, or keeps to the one chosen here.",
            )))
            .right(self.speech_language_popup(dim, cx));
        ui::card(vec![row.into_any_element()], p)
    }

    /// Automatic, then the locales the recogniser in use reports, by name
    /// where this window has one.
    fn speech_language_popup(&self, dim: bool, cx: &Context<Self>) -> PopupButton {
        const ID: &str = "voice-speech-language";
        let language = self.language();
        let Some(config) = self.voice.stt_config.as_ref() else {
            return PopupButton::new(ID, "\u{2014}").disabled(true);
        };
        let chosen = speech_choice(config);
        let locales = speech_locales(self.voice.engines.as_ref(), config);
        let shell = cx.entity();
        PopupButton::new(ID, chosen.word(language))
            .disabled(dim)
            .menu(move |menu, _, _| {
                let automatic = {
                    let shell = shell.clone();
                    PopupMenuItem::new(language.text("Automatic"))
                        .checked(chosen == SpeechChoice::Automatic)
                        .on_click(move |_, window, cx| {
                            shell.update(cx, |this, cx| {
                                this.choose_speech_language(None, window, cx);
                            });
                        })
                };
                let mut menu = menu
                    .scrollable(true)
                    .max_h(px(LOCALE_MENU_HEIGHT))
                    .item(automatic);
                for code in &locales {
                    let shell = shell.clone();
                    let name: SharedString = locale_name(code, language)
                        .map_or_else(|| code.clone().into(), SharedString::from);
                    let checked = chosen == SpeechChoice::Locale(code.clone());
                    let code = code.clone();
                    let item =
                        PopupMenuItem::new(name)
                            .checked(checked)
                            .on_click(move |_, window, cx| {
                                shell.update(cx, |this, cx| {
                                    this.choose_speech_language(Some(&code), window, cx);
                                });
                            });
                    menu = menu.item(item);
                }
                menu
            })
    }

    // ---- the engines -----------------------------------------------------------

    /// Recognition and Spoken replies: side by side on the page, their cards
    /// stretched to one height, the last row of the shorter taking the rest;
    /// one over the other in first run.
    ///
    /// Each card's choices are one stop on the Tab order ([`ui::Group`]):
    /// Tab reaches the chosen one, Up and Down move the keyboard between
    /// them, and Return or Space chooses. An arrow alone never chooses:
    /// choosing Whisper or Kokoro without its model starts a download, and
    /// the cloud voice without a key opens a dialog.
    fn voice_engine_cards(
        &self,
        dim: bool,
        side_by_side: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> Div {
        let recognition = self.recognition_card(dim, p, cx);
        let replies = self.replies_card(dim, p, cx);
        if side_by_side {
            let column = |title: &'static str, card: ui::Group| {
                ui::section(title, card.flex_1(), p).flex_1().min_w_0()
            };
            div()
                .flex()
                .items_stretch()
                .w_full()
                .gap(px(GAP))
                .child(column(self.tr("Recognition"), recognition))
                .child(column(self.tr("Spoken replies"), replies))
        } else {
            v_flex()
                .gap(px(GAP))
                .child(ui::section(self.tr("Recognition"), recognition, p))
                .child(ui::section(self.tr("Spoken replies"), replies, p))
        }
    }

    /// A choice: the radio leads the row, and the whole row is pressed.
    /// `enabled` is false while there is nothing to choose with (the service
    /// is away, or has not said what is chosen).
    fn choice(
        &self,
        id: &'static str,
        on: bool,
        enabled: bool,
        p: &Palette,
        cx: &Context<Self>,
        press: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
    ) -> Row {
        let row = Row::new().lead(ui::bare(ui::radio_dot(on, p)));
        if enabled {
            row.on_click(
                id,
                cx.listener(move |this, _, window, cx| press(this, window, cx)),
            )
        } else {
            row
        }
    }

    /// Apple's recogniser and Whisper with its model's state, the card's
    /// choices, and after them the switch that lets Conol hear the presses
    /// meant for its chats.
    fn recognition_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> ui::Group {
        let config = self.voice.stt_config.as_ref();
        let chosen = config.and_then(recogniser);
        let enabled = config.is_some() && !dim;
        let choose = |engine: Recogniser| {
            move |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                if this.voice.stt_config.as_ref().and_then(recogniser) != Some(engine) {
                    this.select_stt(engine.id(), window, cx);
                }
            }
        };
        let apple = self
            .choice(
                "voice-stt-apple",
                chosen == Some(Recogniser::Apple),
                enabled,
                p,
                cx,
                choose(Recogniser::Apple),
            )
            .title(self.tr("Apple, on this Mac"))
            .sub(self.tr("Built in. One language at a time."));
        let whisper = self
            .choice(
                "voice-stt-whisper",
                chosen == Some(Recogniser::Whisper),
                enabled,
                p,
                cx,
                choose(Recogniser::Whisper),
            )
            .title(self.tr(here("Whisper, on this Mac", "Whisper, on this PC")));
        let whisper = self.model_row(whisper, Model::Whisper, dim, p, cx);
        let companion = config.is_some_and(|config| config.companion);
        let conol_sub = self.tr(here(
            "Audio of those presses goes to Conol's service.\nOff keeps all recognition on this Mac.",
            "Audio of those presses goes to Conol's service.\nOff keeps all recognition on this PC.",
        ));
        let conol = Row::new()
            .lead(ui::bare(div()))
            .title(self.tr("Conol's cloud, for Conol chats only"))
            .sub_el(div().mr(px(-CONOL_SUB_SLACK)).child(ui::para(conol_sub)))
            .right(
                Switch::new("voice-companion", companion)
                    .disabled(!enabled)
                    .on_toggle(cx.listener(|this, on: &bool, window, cx| {
                        this.set_companion(*on, window, cx);
                    })),
            )
            .grow();
        // Windows has no recogniser of its own: Whisper is the card's one.
        let members = if cfg!(windows) {
            vec![whisper]
        } else {
            vec![apple, whisper]
        };
        let at = chosen.map(|engine| match engine {
            Recogniser::Apple => 0,
            Recogniser::Whisper => members.len() - 1,
        });
        ui::Group::new("voice-recognition", Layout::Column)
            .card(p)
            .selected(at)
            .members(members)
            .after(conol)
    }

    /// Apple's voice, Kokoro with its voice's state and the cloud voice with
    /// its key, the card's choices, and after them the MLX model while it
    /// is the one chosen (only the command line chooses it).
    fn replies_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> ui::Group {
        let tts = self.voice.tts.as_ref();
        let chosen = tts.and_then(synthesiser);
        let enabled = tts.is_some() && !dim;
        let choose = |voice: Synthesiser| {
            move |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                if this.voice.tts.as_ref().and_then(synthesiser) != Some(voice) {
                    this.select_tts(voice.id(), window, cx);
                }
            }
        };
        let apple = self
            .choice(
                "voice-tts-apple",
                chosen == Some(Synthesiser::Apple),
                enabled,
                p,
                cx,
                choose(Synthesiser::Apple),
            )
            .title(self.tr(here("Apple voice", "Windows voice")))
            .sub(self.tr("Built in."));
        let kokoro = self
            .choice(
                "voice-tts-kokoro",
                chosen == Some(Synthesiser::Kokoro),
                enabled,
                p,
                cx,
                choose(Synthesiser::Kokoro),
            )
            .title(self.tr(here("Kokoro, on this Mac", "Kokoro, on this PC")));
        let kokoro = self.model_row(kokoro, Model::Kokoro, dim, p, cx);
        // Without a key the cloud voice cannot be chosen: its row asks for
        // one, and saving it chooses the voice.
        let key = tts
            .filter(|tts| tts.has_key())
            .map(|tts| key_tail(&tts.key_hint));
        let has_key = key.is_some();
        let cloud = self
            .choice(
                "voice-tts-cloud",
                chosen == Some(Synthesiser::Cloud),
                enabled,
                p,
                cx,
                move |this, window, cx| {
                    if !has_key {
                        this.open_add_key(window, cx);
                    } else if this.voice.tts.as_ref().and_then(synthesiser)
                        != Some(Synthesiser::Cloud)
                    {
                        this.select_tts(Synthesiser::Cloud.id(), window, cx);
                    }
                },
            )
            .title(self.tr("Cloud voice, with your own key"));
        let about = self.tr("OpenAI or ElevenLabs.\nThe reply text is sent to them.");
        let open = cx.listener(|this, _, window, cx| this.open_add_key(window, cx));
        let cloud = match key {
            // The key's tail on the providers' line, at its right end: beside
            // **Change…** it left the title too little room, and the title
            // wrapped. The line under keeps the column's whole width.
            Some(tail) => cloud.sub_el(key_sub(about, tail, p)).right(
                Btn::new("voice-change-key", self.tr("Change\u{2026}"))
                    .disabled(!enabled)
                    .on_click(open),
            ),
            None => cloud.sub(about).right(
                Btn::new("voice-add-key", self.tr("Add Key"))
                    .disabled(!enabled)
                    .on_click(open),
            ),
        };
        let mlx = (chosen == Some(Synthesiser::Mlx)).then(|| {
            Row::new()
                .lead(ui::bare(ui::radio_dot(true, p)))
                .title(self.tr("Qwen3-TTS (MLX), on this Mac"))
                .sub(self.tr("Set up from the command line."))
        });
        // The card's last row takes the spare height of a stretched card.
        let (cloud, mlx) = match mlx {
            Some(mlx) => (cloud, Some(mlx.grow())),
            None => (cloud.grow(), None),
        };
        let at = chosen.and_then(|voice| match voice {
            Synthesiser::Apple => Some(0),
            Synthesiser::Kokoro => Some(1),
            Synthesiser::Cloud => Some(2),
            Synthesiser::Mlx => None,
        });
        ui::Group::new("voice-replies", Layout::Column)
            .card(p)
            .selected(at)
            .members([apple, kokoro, cloud])
            .when_some(mlx, |card, mlx| card.after(mlx))
    }

    /// A local model's row: what it is (or how far its download has come
    /// while one runs) and its slot on the right, with what can be done
    /// about it. With no status read, what it is alone.
    fn model_row(&self, row: Row, model: Model, dim: bool, p: &Palette, cx: &Context<Self>) -> Row {
        let (slot, about) = match model {
            Model::Whisper => (
                self.voice.whisper.as_ref().map(stt_slot),
                self.tr("Many languages, mixed speech."),
            ),
            Model::Kokoro => (
                self.voice.kokoro.as_ref().map(tts_slot),
                self.tr("Chinese and English."),
            ),
        };
        let ids = model.ids();
        let Some(slot) = slot else {
            return row.sub(about);
        };
        let cancel = || {
            Btn::new(ids.cancel, self.tr("Cancel"))
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| match model {
                    Model::Whisper => this.remove_stt(window, cx),
                    Model::Kokoro => this.remove_tts(window, cx),
                }))
        };
        match slot {
            Slot::Installed => {
                row.sub(about)
                    .right(ui::status(self.tr("Installed"), Tone::Green, p))
            }
            Slot::Download { bytes } if bytes > 0 => row.sub(about).right(
                Btn::new(
                    ids.download,
                    self.fmt("Download \u{b7} {0}", &[&present::size_text(bytes)]),
                )
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| match model {
                    Model::Whisper => this.install_stt(window, cx),
                    Model::Kokoro => this.install_tts(window, cx),
                })),
            ),
            Slot::Download { .. } => row.sub(about),
            Slot::Downloading {
                percent: Some(percent),
            } => row
                .sub(self.fmt("Downloading {0}%", &[&percent.to_string()]))
                .right(cancel()),
            Slot::Downloading { percent: None } => row
                .sub(about)
                .right(ui::spinner(13., p.text2))
                .right(cancel()),
            Slot::Loading => row.sub(about).right(
                h_flex()
                    .flex_none()
                    .gap(px(7.))
                    .ts(12., 16.)
                    .whitespace_nowrap()
                    .text_color(p.text2)
                    .child(ui::spinner(12., p.text2))
                    .child(self.tr("Loading\u{2026}")),
            ),
            Slot::Failed { why, load } => row
                .sub(about)
                .right(div().id(ids.failed).tooltip(ui::tip(why)).child(ui::status(
                    self.tr("Failed"),
                    Tone::Red,
                    p,
                )))
                .right(
                    Btn::new(ids.retry, self.tr("Retry"))
                        .disabled(dim)
                        .on_click(cx.listener(move |this, _, window, cx| match model {
                            Model::Whisper => this.retry_stt(load, window, cx),
                            Model::Kokoro => this.retry_tts(load, window, cx),
                        })),
                ),
        }
    }

    // ---- the signal path and the test ----------------------------------------

    /// Where a press and a reply travel, as the choices stand now, then the
    /// test from the Mac.
    fn signal_path_section(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let language = self.language();
        let routes = match (&self.voice.stt_config, &self.voice.tts) {
            (Some(stt), Some(tts)) => signal_path(
                stt,
                self.voice.whisper.as_ref(),
                tts,
                self.voice.kokoro.as_ref(),
            ),
            _ => Vec::new(),
        };
        let mut parts = Vec::new();
        if !routes.is_empty() {
            parts.push(
                v_flex()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(ROUTES_BOTTOM))
                    .children(
                        routes
                            .iter()
                            .map(|(route, stops)| route_line(*route, stops, p, language)),
                    )
                    .into_any_element(),
            );
        }
        parts.push(self.test_row(dim, p, cx).into_any_element());
        ui::section(self.tr("Signal path"), ui::card(parts, p), p)
    }

    /// The test row: what Test Voice does and what the last test heard,
    /// then the three tests, which need the deck linked. A service without
    /// the voice test keeps only the chime.
    fn test_row(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let language = self.language();
        let usable = self.linked() && !dim;
        let why = (!usable).then(|| self.tr("Link the deck first."));
        let button = |button: Btn| match why {
            Some(why) => button.disabled(true).tooltip(why),
            None => button,
        };
        let mut words = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .ts(12., 16.)
            .text_color(p.text2);
        if self.voice.tests_missing {
            words = words.child(ui::para(self.tr("This needs a newer background service.")));
        } else {
            words = words.child(ui::para(
                self.tr("Test Voice asks on the deck: hold PTT and say hello."),
            ));
            if let Some(line) = test_line(self.voice.test.as_ref()) {
                let mark = match line {
                    TestLine::Heard(_) => {
                        ui::icon(ic::CHECK_CIRCLE, 13., p.good).into_any_element()
                    }
                    TestLine::Waiting => ui::spinner(13., p.text2).into_any_element(),
                    TestLine::Nothing | TestLine::NoDeck => {
                        ui::icon(ic::INFO, 13., p.text3).into_any_element()
                    }
                };
                // What the deck heard is the owner's own words, of any
                // length: it wraps.
                words = words.child(
                    h_flex()
                        .items_start()
                        .gap(px(6.))
                        .child(div().flex_none().pt(px(1.5)).child(mark))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(ui::para(line.words(language))),
                        ),
                );
            }
        }
        let mut row = h_flex().gap(px(8.)).px(px(16.)).py(px(8.)).child(words);
        if !self.voice.tests_missing {
            row = row
                .child(button(
                    Btn::new("voice-test", self.tr("Test Voice")).on_click(
                        cx.listener(|this, _, window, cx| this.start_voice_test(window, cx)),
                    ),
                ))
                .child(button(
                    Btn::new("voice-say", self.tr("Speak a Sample"))
                        .quiet()
                        .on_click(cx.listener(|this, _, window, cx| this.speak_sample(window, cx))),
                ));
        }
        row.child(button(
            Btn::new("voice-chime", self.tr("Play a Chime"))
                .quiet()
                .on_click(cx.listener(|this, _, window, cx| this.play_chime(window, cx))),
        ))
    }

    // ---- the permissions -------------------------------------------------------

    /// Bluetooth and Speech Recognition, each with the one thing that can
    /// honestly be done about it. No microphone of this Mac's is in the path.
    fn permissions_section(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let language = self.language();
        let permissions = self.state.mirror().voice.permissions;
        let apple = self.voice.recognises() == Some(Recogniser::Apple);
        let refused = self.voice.prompts.refused("speech");
        let bluetooth = bluetooth_grant(permissions.bluetooth, language);
        let speech = speech_grant(
            speech_state(permissions.speech, self.voice.speech),
            apple,
            refused,
            language,
        );
        let rows = vec![
            self.permission_row(
                Row::new()
                    .title(self.tr("Bluetooth"))
                    .sub(self.tr("For the deck's microphone and speaker when the cable is out.")),
                bluetooth,
                ("voice-bluetooth-settings", BLUETOOTH_SETTINGS),
                dim,
                cx,
            ),
            self.permission_row(
                Row::new()
                    .title(self.tr("Speech Recognition"))
                    .sub(self.tr("Only for Apple's recogniser.")),
                speech,
                ("voice-speech-settings", SPEECH_SETTINGS),
                dim,
                cx,
            ),
        ];
        ui::section(self.tr("macOS permissions"), ui::card(rows, p), p)
    }

    /// One permission's row: its state, then **Ask…** or **Open Settings**
    /// (`settings`: the button's id and the pane it opens) when it offers
    /// one. System Settings opens whether the service is there or not;
    /// asking goes through the service.
    fn permission_row(
        &self,
        row: Row,
        grant: Grant,
        (settings_id, settings): (&'static str, &'static str),
        dim: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let p = *palette(cx);
        let row = row.right(ui::status(grant.word, grant.tone, &p));
        match grant.offer {
            Some(Offer::Ask) => row
                .right(
                    Btn::new("voice-ask-speech", self.tr("Ask\u{2026}"))
                        .disabled(dim)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request_permission("speech", window, cx);
                        })),
                )
                .into_any_element(),
            Some(Offer::OpenSettings) => row
                .right(
                    Btn::new(settings_id, self.tr("Open Settings"))
                        .on_click(move |_, _, cx: &mut App| cx.open_url(settings)),
                )
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    // ---- the key dialog --------------------------------------------------------

    /// The cloud voice's key: the provider, the masked field, **Remove Key**
    /// when one is set, **Cancel** and **Save Key**.
    fn render_add_key(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let has_key = self.voice.tts.as_ref().is_some_and(TtsConfig::has_key);
        let chosen = PROVIDERS
            .iter()
            .position(|(id, _, _)| *id == self.voice.provider)
            .unwrap_or_default();
        let providers = Segmented::new(
            "voice-key-provider",
            PROVIDERS
                .iter()
                .map(|(_, name, _)| SharedString::from(*name))
                .collect(),
            chosen,
        )
        .on_select(cx.listener(|this, at: &usize, _, cx| {
            if let Some((id, _, _)) = PROVIDERS.get(*at) {
                this.voice.provider = id;
                cx.notify();
            }
        }));
        let remove = has_key.then(|| {
            Btn::new("voice-key-remove", self.tr("Remove Key"))
                .quiet()
                .on_click(cx.listener(|this, _, window, cx| this.remove_cloud_key(window, cx)))
        });
        let buttons = h_flex()
            .mt(px(16.))
            .gap(px(8.))
            .children(remove)
            .child(div().flex_1())
            .child(
                Btn::new("voice-key-cancel", self.tr("Cancel"))
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .child(
                Btn::new("voice-key-save", self.tr("Save Key"))
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.save_cloud_key(window, cx))),
            );
        v_flex()
            .p(px(20.))
            .gap(px(4.))
            .ts(13., 18.)
            .child(div().semibold().child(self.tr("Cloud voice key")))
            .child(
                div()
                    .text_color(p.text2)
                    .child(ui::para(key_kept(self.voice.tts.as_ref(), self.language()))),
            )
            .child(
                h_flex()
                    .mt(px(12.))
                    .justify_between()
                    .child(div().text_color(p.text2).child(self.tr("Provider")))
                    .child(providers),
            )
            .child(div().mt(px(8.)).child(ui::field(&self.voice.key, &p)))
            .child(buttons)
            .into_any_element()
    }
}

/// The Cloud voice row's sub-line once a key is set: the sheet's two lines,
/// the key's tail (`…mnop`) at the right end of the first, the providers'
/// line, and the second as wide as the column, so the tail takes no room
/// from it. A translation with no line break keeps its words beside the
/// tail.
fn key_sub(about: &str, tail: &str, p: &Palette) -> Div {
    let (providers, rest) = about.split_once('\n').unwrap_or((about, ""));
    v_flex()
        .child(
            h_flex()
                .items_start()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(ui::para(providers.to_string())),
                )
                .child(ui::mono(tail.to_string(), p.text2).flex_none()),
        )
        .when(!rest.is_empty(), |sub| {
            sub.child(ui::para(rest.to_string()))
        })
}

/// One route: its name, then its stops with an arrow between each two.
fn route_line(route: Route, stops: &[Stop], p: &Palette, language: Language) -> Div {
    let mut line = h_flex().gap(px(10.)).child(
        div()
            .flex_none()
            .w(px(ROUTE_NAME))
            .ts(12., 16.)
            .text_color(p.text3)
            .child(route.name(language)),
    );
    for (i, stop) in stops.iter().enumerate() {
        if i > 0 {
            line = line.child(ui::icon(ic::ARROW_RIGHT, 14., p.text3));
        }
        line = line.child(stop_mark(stop, p, language));
    }
    line
}

/// A stop: its mark and its name. What runs on this Mac is a laptop, a
/// service elsewhere a globe; an engine this window does not know has no
/// mark, since it cannot say where it runs.
fn stop_mark(stop: &Stop, p: &Palette, language: Language) -> Div {
    let (mark, name): (Option<&'static str>, SharedString) = match stop {
        Stop::Microphone => (
            Some(ic::MICROPHONE),
            language.text("Deck microphone").into(),
        ),
        Stop::OnMac(name) => (Some(ic::LAPTOP), (*name).into()),
        Stop::Cloud(name) => (Some(ic::GLOBAL), name.clone().into()),
        Stop::Agent => (Some(ic::TERMINAL), language.text("The agent").into()),
        Stop::Speaker => (Some(ic::VOLUME), language.text("Deck speaker").into()),
        Stop::ConolCloud => (Some(ic::GLOBAL), language.text("Conol's cloud").into()),
        Stop::Chat => (Some(ic::CHAT), language.text("The chat").into()),
        Stop::Other(word) if word.is_empty() => (None, "\u{2014}".into()),
        Stop::Other(word) => (None, word.clone().into()),
    };
    h_flex()
        .flex_none()
        .gap(px(6.))
        .ts(13., 18.)
        .whitespace_nowrap()
        .text_color(p.text2)
        .children(mark.map(|mark| ui::icon(mark, 15., p.text2)))
        .child(name)
}
