//! What the Voice page does besides drawing it ([`crate::voice`]): reading
//! the service's voice settings, engines and last test; choosing,
//! downloading and removing the local recogniser and voice; writing the
//! speech language, the Conol switch and the cloud voice's key; the three
//! tests from the Mac; and reading, and asking macOS for, the speech
//! permission.
//!
//! The engine logic is the console's Voice screen's, kept as it asked and
//! wrote: `refresh_*` with a re-read every second while a model downloads or
//! loads; `select_*`, where choosing a local model that is not on this Mac
//! starts its download (that is the opt-in); `install_*`; `remove_*`, which
//! also stops a download; one `config.set` of the page's at a time; and
//! `perm.request` with its answers kept in [`Prompts`]. What is new with the
//! page: the speech language and the cloud voice's key are written as one
//! patch each, a model that would not load is fetched again on **Retry**, a
//! method this service does not have is said in one sentence, and the tests
//! are the service's own (`care.test.*`).
use crate::present;
use crate::shell::Shell;
use crate::voice::{self, keys, Synthesiser, ADD_KEY};
use deck_client::ClientError;
use deck_gui_core::admin::method_missing;
use deck_gui_core::care::{TestState, TestStatus, VoiceTest};
use deck_gui_core::i18n::Language;
use deck_gui_core::voice::{
    Engines, Prompt, SttConfig, SttStatus, TestReport, TtsConfig, TtsStatus, TESTS,
};
use deck_gui_core::{ConfigValue, UiAction};
use gpui::{Context, Window};
use std::time::Duration;

/// How often a model's status is read again while it downloads or loads.
const POLL: Duration = Duration::from_secs(1);

/// What `perm.request` answered, per permission. Kept so that a permission
/// whose prompt the service said it cannot show is not offered **Ask…**
/// again: a button that cannot work is worse offered twice than once.
#[derive(Clone, Debug, Default)]
pub struct Prompts {
    list: Vec<Prompt>,
}

impl Prompts {
    /// The last answer about `which`.
    pub fn get(&self, which: &str) -> Option<&Prompt> {
        self.list.iter().find(|prompt| prompt.which == which)
    }

    /// Keep `prompt`, in place of any earlier answer about its permission.
    pub fn remember(&mut self, prompt: Prompt) {
        self.list.retain(|known| known.which != prompt.which);
        self.list.push(prompt);
    }

    /// The service said it cannot show macOS's prompt for `which`.
    pub fn refused(&self, which: &str) -> bool {
        self.get(which).is_some_and(Prompt::not_shown)
    }
}

/// Why a call the page made came back without an answer, as far as the
/// page acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The service is older than the method (`-32601`): the page says so in
    /// place of what the method would have shown, and claims nothing.
    Missing,
    /// The socket went. The frame's strip says the service is away, and a
    /// page keeps what it last read, dimmed.
    Away,
    /// Anything else, in the service's (or the socket's) own words.
    Failed(String),
}

/// What `error` means to the page.
pub fn refusal(error: &ClientError) -> Refusal {
    if method_missing(error) {
        Refusal::Missing
    } else if matches!(error, ClientError::Disconnected) {
        Refusal::Away
    } else {
        Refusal::Failed(error.to_string())
    }
}

/// What a write the page made says when the service refused it: what was
/// written, and why in the service's own words when it gave them
/// (`data.why`: a Keychain that would not take the key, a value out of
/// range), else the error itself.
pub fn write_failure(what: &str, error: &ClientError, language: Language) -> String {
    language.format(
        "Could not write {0}: {1}",
        &[what, &crate::updates::failure_text(error)],
    )
}

/// What **Play a Chime** says, when it says anything: that the chime was not
/// queued. A chime that was queued says it on the deck, and `played` means
/// queued and never heard, so the page claims no more than the deck does.
pub fn chime_complaint(report: &TestReport, language: Language) -> Option<String> {
    (report.played != Some(true)).then(|| present::test_headline_localized(report, language))
}

impl Shell {
    // ---- reading -----------------------------------------------------------------

    /// Ask the service for everything the page draws, unless it has answered
    /// once already; `force` asks again (a reconnect, after a write). The
    /// voice settings are the page; the engines, the models and the last
    /// test fill it in. Speech Recognition is asked every time, so a grant
    /// made in System Settings meanwhile shows when the page opens again.
    pub(crate) fn refresh_voice(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.read_speech_permission(window, cx);
        if self.voice.busy || (self.voice.asked && !force) {
            return;
        }
        self.voice.busy = true;
        self.ask(
            UiAction::ConfigGet,
            window,
            cx,
            |this, result, window, cx| {
                this.voice.busy = false;
                match result {
                    Ok(value) => {
                        this.voice.stt_config = Some(SttConfig::parse(&value));
                        this.voice.tts = Some(TtsConfig::parse(&value));
                        this.voice.config_missing = false;
                        this.voice.asked = true;
                    }
                    Err(e) => match refusal(&e) {
                        Refusal::Missing => this.voice.config_missing = true,
                        Refusal::Away => {}
                        Refusal::Failed(why) => this.toast(
                            this.fmt("config.get did not answer: {0}", &[&why]),
                            window,
                            cx,
                        ),
                    },
                }
            },
        );
        // The recognisers' locales, for the Speech language popup. A service
        // with no audio engine (`--no-device`) answers `-32013`, which is a
        // state and not a failure: the popup offers Automatic and the locale
        // the settings name.
        self.ask(UiAction::VoiceEngines, window, cx, |this, result, _, _| {
            if let Ok(value) = result {
                this.voice.engines = Some(Engines::parse(&value));
            }
        });
        self.refresh_stt(window, cx);
        self.refresh_tts_status(window, cx);
        self.voice.test_failed = false;
        self.refresh_voice_test(window, cx);
    }

    /// `stt.status`, and again every second while the Whisper model
    /// downloads or loads, so its row moves on its own.
    pub(crate) fn refresh_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::SttStatus,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let status = SttStatus::parse(&value);
                    let busy = status.downloading() || status.loading;
                    this.voice.whisper = Some(status);
                    if busy {
                        this.schedule_stt_poll(window, cx);
                    }
                }
                Err(e) => match refusal(&e) {
                    Refusal::Away => {}
                    Refusal::Missing => this.voice.whisper = None,
                    Refusal::Failed(why) => {
                        this.voice.whisper = None;
                        this.toast(
                            this.fmt("stt.status did not answer: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                },
            },
        );
    }

    fn schedule_stt_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.whisper_polling {
            return;
        }
        self.voice.whisper_polling = true;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(POLL).await;
            this.update_in(cx, |this, window, cx| {
                this.voice.whisper_polling = false;
                this.refresh_stt(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// `tts.status`, polled the same way for the Kokoro voice.
    pub(crate) fn refresh_tts_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::TtsStatus,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let status = TtsStatus::parse(&value);
                    let busy = status.downloading() || status.loading;
                    this.voice.kokoro = Some(status);
                    if busy {
                        this.schedule_tts_poll(window, cx);
                    }
                }
                Err(e) => match refusal(&e) {
                    Refusal::Away => {}
                    Refusal::Missing => this.voice.kokoro = None,
                    Refusal::Failed(why) => {
                        this.voice.kokoro = None;
                        this.toast(
                            this.fmt("tts.status did not answer: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                },
            },
        );
    }

    fn schedule_tts_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.kokoro_polling {
            return;
        }
        self.voice.kokoro_polling = true;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(POLL).await;
            this.update_in(cx, |this, window, cx| {
                this.voice.kokoro_polling = false;
                this.refresh_tts_status(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// `care.test.status`'s voice half: read when the page opens, and every
    /// second while a test waits on the deck (the tick asks).
    pub(crate) fn refresh_voice_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.test_asking {
            return;
        }
        self.voice.test_asking = true;
        self.ask(
            UiAction::TestStatus,
            window,
            cx,
            |this, result, window, cx| {
                this.voice.test_asking = false;
                match result {
                    Ok(value) => {
                        this.voice.test = TestStatus::parse(&value).voice;
                        this.voice.tests_missing = false;
                    }
                    Err(e) => match refusal(&e) {
                        Refusal::Missing => this.voice.tests_missing = true,
                        Refusal::Away => {}
                        // The test is no longer followed: said once, not once a
                        // second.
                        Refusal::Failed(why) => {
                            this.voice.test_failed = true;
                            this.toast(
                                this.fmt(
                                    "{0} failed: {1}",
                                    &[this.tr("Read the test result"), &why],
                                ),
                                window,
                                cx,
                            );
                        }
                    },
                }
            },
        );
    }

    // ---- the recogniser ------------------------------------------------------

    /// Choose the recogniser: `[voice.stt] engine`. Choosing Whisper without
    /// its model on this Mac starts the download in the same breath — that
    /// is the opt-in — and Apple's recogniser answers until the file is
    /// whole.
    pub(crate) fn select_stt(
        &mut self,
        engine: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_model = engine == voice::Recogniser::Whisper.id()
            && self
                .voice
                .whisper
                .as_ref()
                .is_some_and(|status| !status.present && !status.downloading());
        let action = UiAction::ConfigSet {
            key: keys::STT_ENGINE.into(),
            value: ConfigValue::Text(engine.into()),
        };
        self.ask(
            action,
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(_) if needs_model => this.install_stt(window, cx),
                Ok(_) => {
                    let said = if engine == voice::Recogniser::Whisper.id() {
                        this.tr("Whisper recognises from the next press.")
                    } else {
                        this.tr("Apple's recogniser from the next press.")
                    };
                    this.toast(said, window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not select the engine: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// `stt.install`: fetch the model. The reply is a status; the poll takes
    /// it from there.
    pub(crate) fn install_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = UiAction::SttInstall { model: None };
        self.ask(
            action,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let status = SttStatus::parse(&value);
                    this.toast(
                        this.fmt(
                            "Downloading {0} ({1}).",
                            &[&status.model, &present::size_text(status.total)],
                        ),
                        window,
                        cx,
                    );
                    this.voice.whisper = Some(status);
                    this.schedule_stt_poll(window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not start the download: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// `stt.remove`: the model goes, and so does a download in flight
    /// (**Cancel**).
    pub(crate) fn remove_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::SttRemove,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    this.voice.whisper = Some(SttStatus::parse(&value));
                    this.toast(
                        this.tr("Model removed. Apple's recogniser from the next press."),
                        window,
                        cx,
                    );
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not remove the model: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// **Retry** beside a Whisper model that failed. A download that stopped
    /// starts again; a model on this Mac that would not load is removed and
    /// fetched again whole, since the service does not try one file twice
    /// until it has been removed.
    pub(crate) fn retry_stt(&mut self, load: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !load {
            self.install_stt(window, cx);
            return;
        }
        self.ask(
            UiAction::SttRemove,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(_) => this.install_stt(window, cx),
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not remove the model: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// The Speech language popup: Automatic (`None`) or a locale, written
    /// as [`voice::speech_language_action`] says.
    pub(crate) fn choose_speech_language(
        &mut self,
        locale: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = voice::speech_language_action(locale);
        self.write_voice(action, keys::STT_LANGUAGE, |_, _, _| {}, window, cx);
    }

    /// The Conol switch: `voice.stt.companion`.
    pub(crate) fn set_companion(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let action = UiAction::ConfigSet {
            key: keys::STT_COMPANION.into(),
            value: ConfigValue::Bool(on),
        };
        self.write_voice(action, keys::STT_COMPANION, |_, _, _| {}, window, cx);
    }

    // ---- the voice -------------------------------------------------------------

    /// Choose the voice: `[voice.tts] engine`. Choosing Kokoro without its
    /// voice on this Mac starts the download, as Whisper's does.
    pub(crate) fn select_tts(
        &mut self,
        engine: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_package = engine == Synthesiser::Kokoro.id()
            && self
                .voice
                .kokoro
                .as_ref()
                .is_some_and(|status| !status.present && !status.downloading());
        let action = UiAction::ConfigSet {
            key: keys::TTS_ENGINE.into(),
            value: ConfigValue::Text(engine.into()),
        };
        let done = move |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            if needs_package {
                this.install_tts(window, cx);
                return;
            }
            let said = match engine {
                "kokoro" => this.tr("The Kokoro voice speaks from the next announcement."),
                "cloud" => this.tr("The provider speaks when a key is set; Apple's otherwise."),
                _ => this.tr("Apple's synthesiser from the next announcement."),
            };
            this.toast(said, window, cx);
        };
        self.write_voice(action, keys::TTS_ENGINE, done, window, cx);
    }

    /// `tts.install`: fetch the Kokoro voice.
    pub(crate) fn install_tts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = UiAction::TtsInstall { model: None };
        self.ask(
            action,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let status = TtsStatus::parse(&value);
                    this.toast(
                        this.fmt(
                            "Downloading the {0} voice ({1}).",
                            &[&status.model, &present::size_text(status.total)],
                        ),
                        window,
                        cx,
                    );
                    this.voice.kokoro = Some(status);
                    this.schedule_tts_poll(window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not start the download: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// `tts.remove`: the voice goes, and so does a download in flight
    /// (**Cancel**).
    pub(crate) fn remove_tts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::TtsRemove,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    this.voice.kokoro = Some(TtsStatus::parse(&value));
                    this.toast(
                        this.tr("Voice removed. Apple's synthesiser from the next announcement."),
                        window,
                        cx,
                    );
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not remove the voice: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// **Retry** beside a Kokoro voice that failed, as [`Shell::retry_stt`].
    pub(crate) fn retry_tts(&mut self, load: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !load {
            self.install_tts(window, cx);
            return;
        }
        self.ask(
            UiAction::TtsRemove,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(_) => this.install_tts(window, cx),
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not remove the voice: {0}", &[&why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    // ---- the cloud voice's key -----------------------------------------------------

    /// Put the key dialog up, on the configured provider, its field empty
    /// and given the keyboard.
    pub(crate) fn open_add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.voice.provider = voice::dialog_provider(self.voice.tts.as_ref());
        self.voice
            .key
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.voice.key_open = true;
        self.open_dialog(ADD_KEY, window);
        self.voice
            .key
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// **Save Key** (or Return in the field): the provider, the key and the
    /// cloud voice in one patch ([`voice::save_key_action`]). The field is
    /// emptied the moment the service has the key.
    pub(crate) fn save_cloud_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.voice.key.read(cx).value().trim().to_string();
        if typed.is_empty() {
            self.voice
                .key
                .update(cx, |input, cx| input.focus(window, cx));
            self.toast(self.tr("Paste the key first."), window, cx);
            return;
        }
        let provider = self.voice.provider;
        let action = voice::save_key_action(provider, &typed, self.voice.tts.as_ref());
        let done = move |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            this.voice
                .key
                .update(cx, |input, cx| input.set_value("", window, cx));
            this.close_dialog(window, cx);
            this.toast(
                this.fmt(
                    "Key saved. Announcements are spoken by {0} from the next one.",
                    &[voice::provider_name(provider)],
                ),
                window,
                cx,
            );
        };
        self.write_voice(action, keys::TTS_API_KEY, done, window, cx);
    }

    /// **Remove Key**: the key is unset. While the cloud voice is the one
    /// chosen, Apple's voice speaks from then on, and the toast says so.
    pub(crate) fn remove_cloud_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let speaking = self
            .voice
            .tts
            .as_ref()
            .is_some_and(TtsConfig::cloud_selected);
        let action = UiAction::ConfigSet {
            key: keys::TTS_API_KEY.into(),
            value: ConfigValue::Null,
        };
        let done = move |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            this.close_dialog(window, cx);
            if speaking {
                this.toast(
                    this.tr("Key removed. Apple's synthesiser speaks."),
                    window,
                    cx,
                );
            }
        };
        self.write_voice(action, keys::TTS_API_KEY, done, window, cx);
    }

    /// One write of the page's (`what` names it if it fails), then
    /// everything read again: the service names what speaks from its own
    /// settings, so the page follows them. One at a time: a write while
    /// another is in flight is dropped.
    fn write_voice(
        &mut self,
        action: UiAction,
        what: &'static str,
        done: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voice.writing {
            return;
        }
        self.voice.writing = true;
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.voice.writing = false;
            match result {
                Ok(_) => {
                    done(this, window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if this.missing_or(&e, window, cx).is_some() {
                        this.toast(write_failure(what, &e, this.language()), window, cx);
                    }
                }
            }
        });
    }

    // ---- the tests -----------------------------------------------------------------

    /// **Test Voice**: the deck asks the owner to hold PTT and say hello;
    /// the tick follows the test until it heard something or gave up.
    pub(crate) fn start_voice_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::TestVoice,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let state = TestState::of_reply(&value).unwrap_or(TestState::Waiting);
                    this.voice.test = Some(VoiceTest {
                        state,
                        at: Some(present::now()),
                        heard: None,
                    });
                    this.voice.test_failed = false;
                }
                Err(e) => match refusal(&e) {
                    Refusal::Missing => this.voice.tests_missing = true,
                    Refusal::Away | Refusal::Failed(_) => this.toast(
                        this.fmt(
                            "{0} failed: {1}",
                            &[this.tr("Start the voice test"), &e.to_string()],
                        ),
                        window,
                        cx,
                    ),
                },
            },
        );
    }

    /// **Speak a Sample**: a sentence spoken on the deck, in its language.
    pub(crate) fn speak_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::TestSay, window, cx, |this, result, window, cx| {
            if let Err(e) = result {
                match refusal(&e) {
                    Refusal::Missing => this.voice.tests_missing = true,
                    Refusal::Away | Refusal::Failed(_) => this.toast(
                        this.fmt(
                            "{0} failed: {1}",
                            &[this.tr("Speak a sample"), &e.to_string()],
                        ),
                        window,
                        cx,
                    ),
                }
            }
        });
    }

    /// **Play a Chime**: `voice.test{what:"speaker"}`, the one check that
    /// makes a sound, on the deck's speaker at its own gain.
    pub(crate) fn play_chime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = UiAction::VoiceTest {
            what: TESTS[0].to_string(),
        };
        self.ask(
            action,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => {
                    let report = TestReport::parse(&value);
                    if let Some(complaint) = chime_complaint(&report, this.language()) {
                        this.toast(complaint, window, cx);
                    }
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("{0} failed: {1}", &[this.tr("Test the voice path"), &why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    // ---- the permission ------------------------------------------------------------

    /// `perm.status`'s Speech Recognition, unless a read is on its way. A
    /// read that fails keeps what was known: the row says nothing it has not
    /// read.
    pub(crate) fn read_speech_permission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.speech_asking {
            return;
        }
        self.voice.speech_asking = true;
        self.ask(UiAction::PermStatus, window, cx, |this, result, _, _| {
            this.voice.speech_asking = false;
            if let Ok(value) = result {
                this.voice.speech = voice::speech_reported(&value);
            }
        });
    }

    /// `perm.request{which}`. The answer is said, and kept: once the service
    /// has said it cannot show the prompt, **Ask…** is not offered again.
    /// The permission is read again after it, since what macOS says may
    /// have changed.
    pub(crate) fn request_permission(
        &mut self,
        which: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = UiAction::PermRequest {
            which: which.to_string(),
        };
        self.ask(
            action,
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(value) => {
                    let prompt = Prompt::parse(&value);
                    this.toast(
                        present::prompt_line_localized(&prompt, this.language()),
                        window,
                        cx,
                    );
                    this.voice.prompts.remember(prompt);
                    this.read_speech_permission(window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(
                            this.fmt("Could not ask for the {0}: {1}", &[this.tr(which), &why]),
                            window,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// A press's call came back refused: a service older than the method is
    /// said in one sentence and `None` handed back; otherwise the error's
    /// words, for the press's own sentence.
    fn missing_or(
        &self,
        error: &ClientError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if method_missing(error) {
            self.toast(
                self.tr("This needs a newer background service."),
                window,
                cx,
            );
            return None;
        }
        Some(crate::updates::failure_text(error))
    }
}
