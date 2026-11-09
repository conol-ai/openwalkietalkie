//! The **Voice** screen (§8.2's Voice row, the minimal one §15 ships): the
//! route machine, the two device UIDs, the capture meter, the STT and TTS
//! engines, the **Spoken replies** control (bring your own key), the
//! permissions, and Test.
//!
//! It draws from both sides of the split, and the difference matters:
//!
//! * the **route**, the **UIDs**, the **permissions** and the **meter** are
//!   the mirror — `voice.route`, `perm.changed` and `voice.level` fold in and
//!   this screen renders whatever is there, live, with nothing to ask for;
//! * the **engines**, the **`[voice.tts]` keys** and **Test** are calls, and
//!   nothing folds their answers into the mirror, so the pane keeps them
//!   itself.
//!
//! **Spoken replies** is the one control on this screen that writes: the
//! provider, the API key and the model go to `config.set` one key at a
//! time, and the daemon applies them at the next announcement (§7.6: the
//! cloud synthesiser reads `[voice.tts]` live). The key is typed into a
//! masked field and is never read back — `config.get` publishes a hint,
//! its length and last four characters — and the paragraph over the field
//! says what a key does: the announcements' text leaves the machine. That
//! sentence is why `voice.tts.api_key` may be a managed key at all.
//!
//! The screen's one discipline is not to turn a reading into a verdict. The
//! daemon is careful to say what it did not do — `played:true` means the
//! chime was queued on the audio thread and not that it was heard;
//! `voice.test{what:"mic"}` never opens the microphone; a `voice.level` of
//! exactly zero is what a warming SCO link and a dead microphone both report
//! (V2) — and each of those caveats arrives in the reply's own `limits`.
//! They are rendered **verbatim**, under the result they belong to. A GUI
//! that summarised them would be putting its own words where the daemon put
//! a hedge, which is the one thing this screen exists not to do.
use crate::present;
use crate::shell::Shell;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Row, Severity, Tint, Type as _};
use deck_gui_core::admin::method_missing;
use deck_gui_core::voice::{
    displayed_permissions, size_text, Engine, Engines, Prompt, SttConfig, SttStatus, TestReport,
    TtsConfig, TtsStatus, PROVIDERS, TESTS,
};
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Permission, Route};
use gpui::{
    div, prelude::*, px, relative, AnyElement, App, Context, Div, Entity, SharedString, Stateful,
    Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use std::time::Duration;

/// The default local model's size, for the first-start prompt before
/// `stt.status` has answered (large-v3-turbo).
const DEFAULT_MODEL_BYTES: u64 = 1_624_555_275;
/// The default voice package's size, likewise (kokoro-multi-lang-v1_1).
const DEFAULT_VOICE_BYTES: u64 = 364_816_464;

/// The first-start prompt's answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opt {
    /// Apple's recogniser and synthesiser: nothing downloaded.
    Keep,
    /// The Whisper recogniser only.
    Recogniser,
    /// The recogniser and the Kokoro voice.
    Both,
}
/// How often the screen re-reads `stt.status` while a download or a load
/// is in flight.
const STT_POLL: Duration = Duration::from_millis(1000);

/// What `perm.request` answered, per permission. Kept so the sentences
/// stay on screen after the toast has gone — they are the reason the
/// button is not offered a second time.
#[derive(Default)]
struct Prompts {
    list: Vec<Prompt>,
}

impl Prompts {
    fn get(&self, which: &str) -> Option<&Prompt> {
        self.list.iter().find(|p| p.which == which)
    }

    fn remember(&mut self, prompt: Prompt) {
        self.list.retain(|p| p.which != prompt.which);
        self.list.push(prompt);
    }
}

/// The Voice screen's own view state: the replies nothing folds, and the
/// two fields of the Spoken replies control.
pub struct VoicePane {
    engines: Option<Engines>,
    /// Why there are no engines. A `-32013` here is a daemon with no audio
    /// engine, which is a state and not a failure (`--no-device`, a build
    /// without one, or the moment before the engines' one report lands).
    engines_error: Option<String>,
    /// `[voice.tts]` as `config.get` last published it.
    tts: Option<TtsConfig>,
    tts_error: Option<String>,
    /// The key being typed. Masked, and emptied once it is saved: the
    /// window never holds a key it has already handed over.
    key: Entity<InputState>,
    /// The model, blank for the provider's default.
    model: Entity<InputState>,
    /// `[voice.stt]` as `config.get` last published it.
    stt_config: Option<SttConfig>,
    /// `stt.status` as last answered: the local model and its download.
    stt: Option<SttStatus>,
    stt_error: Option<String>,
    /// A re-read of `stt.status` is scheduled.
    stt_polling: bool,
    /// `tts.status` as last answered: the local voice and its download.
    voice: Option<TtsStatus>,
    voice_error: Option<String>,
    voice_polling: bool,
    /// The last `voice.test`, whichever check it was.
    report: Option<TestReport>,
    report_error: Option<String>,
    prompts: Prompts,
    asked: bool,
    busy: bool,
    /// A `config.set` of this control is in flight.
    writing: bool,
}

impl VoicePane {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> (VoicePane, Vec<Subscription>) {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste the provider's API key\u{2026}")
                .masked(true)
        });
        let model = cx.new(|cx| InputState::new(window, cx).placeholder("Provider default"));
        let save = cx.subscribe_in(&key, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_tts_key(window, cx);
            }
        });
        let set_model = cx.subscribe_in(&model, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.set_tts_model(window, cx);
            }
        });
        (
            VoicePane {
                engines: None,
                engines_error: None,
                tts: None,
                tts_error: None,
                key,
                model,
                stt_config: None,
                stt: None,
                stt_error: None,
                stt_polling: false,
                voice: None,
                voice_error: None,
                voice_polling: false,
                report: None,
                report_error: None,
                prompts: Prompts::default(),
                asked: false,
                busy: false,
                writing: false,
            },
            vec![save, set_model],
        )
    }

    fn prompt(&self, which: &str) -> Option<&Prompt> {
        self.prompts.get(which)
    }

    fn remember(&mut self, prompt: Prompt) {
        self.prompts.remember(prompt);
    }
}

/// The one line under the key field: what speaks, and with what.
///
/// `tts` is the config, `engine` is what `voice.engines` reports as the
/// synthesiser in force — the two agree by construction (the daemon names
/// the engine from the same config), so the line uses the config's words
/// and the engine's badge, and says nothing it cannot read.
pub(crate) fn replies_line(tts: &TtsConfig, voice: Option<&TtsStatus>, engine: Option<&Engine>) -> String {
    let provider = tts.provider_row().map_or(tts.provider.as_str(), |(_, name, _)| name);
    if tts.engine == "kokoro" {
        return voice.map_or_else(
            || "Reading the voice's status\u{2026}".to_string(),
            TtsStatus::line,
        );
    }
    if tts.engine == "apple" {
        return format!("Apple's synthesiser speaks, on this {}.", deck_gui_core::voice::HOST);
    }
    if !tts.cloud_selected() {
        return format!(
            "[voice.tts] engine is \"{}\": the key is kept but not used until the engine is \"cloud\".",
            tts.engine
        );
    }
    if !tts.has_key() {
        return format!("No key: Apple's synthesiser speaks, on this {}.", deck_gui_core::voice::HOST);
    }
    let model = tts
        .effective_model()
        .map(|m| format!(", model {m}"))
        .unwrap_or_default();
    let badge = engine
        .map(|e| format!(" \u{b7} {}", present::engine_badge(e)))
        .unwrap_or_default();
    format!("Key {} \u{2014} spoken by {provider}{model}{badge}", tts.key_hint)
}

impl Shell {
    /// Ask for the one thing this screen cannot get off the stream.
    ///
    /// `voice.engines` only: the route, the UIDs, the permissions and the
    /// meter are events, and asking for them would be asking for something
    /// the mirror already has.
    pub(crate) fn refresh_voice(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voice.busy || (self.voice.asked && !force) {
            return;
        }
        self.voice.busy = true;
        self.ask(
            UiAction::VoiceEngines,
            window,
            cx,
            |this, result, _window, _cx| {
                this.voice.busy = false;
                match result {
                    Ok(value) => {
                        this.voice.engines = Some(Engines::parse(&value));
                        this.voice.engines_error = None;
                        // Only an answer counts as having asked, so opening
                        // the screen again after a refusal asks again.
                        this.voice.asked = true;
                    }
                    Err(e) => {
                        this.voice.engines = None;
                        this.voice.engines_error = Some(if method_missing(&e) {
                            present::missing_method_line("voice.engines")
                        } else {
                            present::engines_missing_line(&e.to_string())
                        });
                    }
                }
            },
        );
        // …and `[voice.tts]`, which is `config.get`'s to answer. The same
        // call the Service screen makes; this screen keeps its own slice.
        self.ask(UiAction::ConfigGet, window, cx, |this, result, window, cx| {
            match result {
                Ok(value) => {
                    let tts = TtsConfig::parse(&value);
                    // The model field shows what is configured and hints
                    // at what the provider uses when nothing is.
                    let placeholder = tts
                        .provider_row()
                        .map_or_else(|| "Provider default".to_string(), |(_, _, m)| m.to_string());
                    this.voice.model.update(cx, |input, cx| {
                        input.set_placeholder(placeholder, window, cx);
                        if input.value().as_ref() != tts.model {
                            input.set_value(tts.model.clone(), window, cx);
                        }
                    });
                    this.voice.tts = Some(tts);
                    this.voice.tts_error = None;
                    this.voice.stt_config = Some(SttConfig::parse(&value));
                }
                Err(e) => {
                    this.voice.tts = None;
                    this.voice.tts_error = Some(if method_missing(&e) {
                        present::missing_method_line("config.get")
                    } else {
                        format!("config.get did not answer: {e}")
                    });
                }
            }
        });
        self.refresh_stt(window, cx);
        self.refresh_tts_status(window, cx);
    }

    /// `tts.status`, polled like `stt.status` while a download or a load
    /// is in flight.
    pub(crate) fn refresh_tts_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::TtsStatus, window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                let status = TtsStatus::parse(&value);
                let busy = status.downloading() || status.loading;
                this.voice.voice = Some(status);
                this.voice.voice_error = None;
                if busy {
                    this.schedule_tts_poll(window, cx);
                }
            }
            Err(e) => {
                this.voice.voice = None;
                this.voice.voice_error = Some(if method_missing(&e) {
                    present::missing_method_line("tts.status")
                } else {
                    format!("tts.status did not answer: {e}")
                });
            }
        });
    }

    fn schedule_tts_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.voice_polling {
            return;
        }
        self.voice.voice_polling = true;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(STT_POLL).await;
            this.update_in(cx, |this, window, cx| {
                this.voice.voice_polling = false;
                this.refresh_tts_status(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The synthesiser chip: `[voice.tts] engine`. Choosing the Kokoro
    /// voice without its package starts the download — the opt-in.
    pub(crate) fn select_tts(&mut self, engine: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let needs_package = engine == "kokoro"
            && self.voice.voice.as_ref().is_some_and(|s| !s.present && !s.downloading());
        self.write_tts(
            "voice.tts.engine",
            ConfigValue::Text(engine.into()),
            move |this, window, cx| {
                if needs_package {
                    this.install_tts(window, cx);
                } else {
                    this.toast(
                        match engine {
                            "kokoro" => "The Kokoro voice speaks from the next announcement.",
                            "cloud" => "The provider speaks when a key is set; Apple's otherwise.",
                            _ => "Apple's synthesiser from the next announcement.",
                        },
                        window,
                        cx,
                    );
                }
            },
            window,
            cx,
        );
    }

    /// `tts.install`: the voice package download.
    pub(crate) fn install_tts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::TtsInstall { model: None }, window, cx, |this, result, window, cx| {
            match result {
                Ok(value) => {
                    let status = TtsStatus::parse(&value);
                    this.toast(
                        format!("Downloading the {} voice ({}).", status.model, size_text(status.total)),
                        window,
                        cx,
                    );
                    this.voice.voice = Some(status);
                    this.schedule_tts_poll(window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) if method_missing(&e) => {
                    this.toast(present::missing_method_line("tts.install"), window, cx);
                }
                Err(e) => this.toast(format!("Could not start the download: {e}"), window, cx),
            }
        });
    }

    /// `tts.remove`: the package goes, and so does a download in flight.
    pub(crate) fn remove_tts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::TtsRemove, window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                this.voice.voice = Some(TtsStatus::parse(&value));
                this.toast("Voice removed. Apple's synthesiser from the next announcement.", window, cx);
                this.refresh_voice(true, window, cx);
            }
            Err(e) if method_missing(&e) => {
                this.toast(present::missing_method_line("tts.remove"), window, cx);
            }
            Err(e) => this.toast(format!("Could not remove the voice: {e}"), window, cx),
        });
    }

    /// `stt.status`, and again every second while the daemon is
    /// downloading or loading the model, so the row moves on its own.
    pub(crate) fn refresh_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::SttStatus, window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                let status = SttStatus::parse(&value);
                let busy = status.downloading() || status.loading;
                this.voice.stt = Some(status);
                this.voice.stt_error = None;
                if busy {
                    this.schedule_stt_poll(window, cx);
                }
            }
            Err(e) => {
                this.voice.stt = None;
                this.voice.stt_error = Some(if method_missing(&e) {
                    present::missing_method_line("stt.status")
                } else {
                    format!("stt.status did not answer: {e}")
                });
            }
        });
    }

    fn schedule_stt_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.stt_polling {
            return;
        }
        self.voice.stt_polling = true;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(STT_POLL).await;
            this.update_in(cx, |this, window, cx| {
                this.voice.stt_polling = false;
                this.refresh_stt(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The engine chip: `[voice.stt] engine`. Choosing Whisper without its
    /// model on disk starts the download in the same breath — that is the
    /// opt-in, and the daemon keeps answering with Apple's until the file
    /// is whole.
    pub(crate) fn select_stt(&mut self, engine: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let needs_model = engine == "whisper"
            && self.voice.stt.as_ref().is_some_and(|s| !s.present && !s.downloading());
        self.ask(
            UiAction::ConfigSet {
                key: "voice.stt.engine".into(),
                value: ConfigValue::Text(engine.into()),
            },
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(_) => {
                    if needs_model {
                        this.install_stt(window, cx);
                    } else {
                        this.toast(
                            if engine == "whisper" {
                                "Whisper recognises from the next press."
                            } else {
                                "Apple's recogniser from the next press."
                            },
                            window,
                            cx,
                        );
                        this.refresh_voice(true, window, cx);
                    }
                }
                Err(e) if method_missing(&e) => {
                    this.toast(present::missing_method_line("config.set"), window, cx);
                }
                Err(e) => this.toast(format!("Could not select the engine: {e}"), window, cx),
            },
        );
    }

    /// `stt.install`: the opt-in download. The reply is a status; the poll
    /// takes it from there.
    pub(crate) fn install_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::SttInstall { model: None }, window, cx, |this, result, window, cx| {
            match result {
                Ok(value) => {
                    let status = SttStatus::parse(&value);
                    this.toast(
                        format!("Downloading {} ({}).", status.model, size_text(status.total)),
                        window,
                        cx,
                    );
                    this.voice.stt = Some(status);
                    this.schedule_stt_poll(window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) if method_missing(&e) => {
                    this.toast(present::missing_method_line("stt.install"), window, cx);
                }
                Err(e) => this.toast(format!("Could not start the download: {e}"), window, cx),
            }
        });
    }

    /// `stt.remove`: the weights go, and so does a download in flight.
    pub(crate) fn remove_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::SttRemove, window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                this.voice.stt = Some(SttStatus::parse(&value));
                this.toast("Model removed. Apple's recogniser from the next press.", window, cx);
                this.refresh_voice(true, window, cx);
            }
            Err(e) if method_missing(&e) => {
                this.toast(present::missing_method_line("stt.remove"), window, cx);
            }
            Err(e) => this.toast(format!("Could not remove the model: {e}"), window, cx),
        });
    }

    /// The first-start prompt's answer (§7.6): recorded in `[voice.stt]
    /// prompted` either way, so it is asked once per Mac; an opt-in selects
    /// the local engines, starts their downloads and opens the Voice
    /// screen where the progress is.
    pub(crate) fn answer_recognition(&mut self, opt: Opt, window: &mut Window, cx: &mut Context<Self>) {
        self.recognition_prompt = false;
        self.ask(
            UiAction::ConfigSet {
                key: "voice.stt.prompted".into(),
                value: ConfigValue::Bool(true),
            },
            window,
            cx,
            move |this, result, window, cx| {
                if let Err(e) = result {
                    this.toast(format!("Could not record the answer: {e}"), window, cx);
                    return;
                }
                if opt != Opt::Keep {
                    this.go(crate::shell::Screen::Voice, window, cx);
                    this.select_stt("whisper", window, cx);
                }
                if opt == Opt::Both {
                    this.select_tts("kokoro", window, cx);
                }
            },
        );
        cx.notify();
    }

    /// The first-start question, as a dialog (§7.6).
    pub(crate) fn render_recognition_dialog(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let bytes = self
            .voice
            .stt
            .as_ref()
            .map(|s| s.total)
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_MODEL_BYTES);
        let voice_bytes = self
            .voice
            .voice
            .as_ref()
            .map(|s| s.total)
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_VOICE_BYTES);
        let model = self
            .voice
            .stt
            .as_ref()
            .map_or("large-v3-turbo".to_string(), |s| s.model.clone());
        v_flex()
            .p(px(22.))
            .gap(px(6.))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(36.))
                            .rounded(px(10.))
                            .bg(p.accent_wash())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon(ic::MICROPHONE, 18., p.accent)),
                    )
                    .child(
                        div()
                            .ts(15., 20.)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("Recognise and speak on this {} with local models?", deck_gui_core::voice::HOST)),
                    ),
            )
            .child(
                div()
                    .mt(px(8.))
                    .ts(13., 19.)
                    .text_color(p.muted)
                    .child(ui::para(format!(
                        "What you say into the deck is recognised by Apple's on-device recogniser, \
                         which handles English and needs no download, and the replies are spoken by \
                         Apple's synthesiser. A Whisper model ({model}, {}) recognises Chinese and \
                         English in one utterance, and a Kokoro voice ({}) speaks both, each entirely \
                         on this {}. Nothing leaves the machine either way. You can change this any \
                         time on the Voice screen.",
                        size_text(bytes),
                        size_text(voice_bytes),
                        deck_gui_core::voice::HOST
                    ))),
            )
            .child(
                h_flex()
                    .mt(px(16.))
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        Btn::new("recognition-keep", "Keep Apple's")
                            .quiet()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answer_recognition(Opt::Keep, window, cx);
                            })),
                    )
                    .child(
                        Btn::new(
                            "recognition-download",
                            format!("Recogniser only ({})", size_text(bytes)),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_recognition(Opt::Recogniser, window, cx);
                        })),
                    )
                    .child(
                        Btn::new(
                            "recognition-download-both",
                            format!("Recogniser and voice ({})", size_text(bytes + voice_bytes)),
                        )
                        .solid()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_recognition(Opt::Both, window, cx);
                        })),
                    ),
            )
    }

    /// One `[voice.tts]` key through `config.set`, then the screen re-reads
    /// both replies: the daemon names the synthesiser in force from the
    /// config, so the badge moves with the key.
    fn write_tts(
        &mut self,
        key: &'static str,
        value: ConfigValue,
        done: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voice.writing {
            return;
        }
        self.voice.writing = true;
        self.ask(
            UiAction::ConfigSet {
                key: key.into(),
                value,
            },
            window,
            cx,
            move |this, result, window, cx| {
                this.voice.writing = false;
                match result {
                    Ok(_) => {
                        done(this, window, cx);
                        this.refresh_voice(true, window, cx);
                    }
                    Err(e) if method_missing(&e) => {
                        this.toast(present::missing_method_line("config.set"), window, cx);
                    }
                    Err(e) => this.toast(format!("Could not write {key}: {e}"), window, cx),
                }
            },
        );
    }

    /// The provider chip: `[voice.tts] provider`.
    pub(crate) fn set_tts_provider(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let name = PROVIDERS
            .iter()
            .find(|(pid, _, _)| *pid == id)
            .map_or(id, |(_, name, _)| name);
        self.write_tts(
            "voice.tts.provider",
            ConfigValue::Text(id.into()),
            move |this, window, cx| this.toast(format!("Provider set to {name}."), window, cx),
            window,
            cx,
        );
    }

    /// **Save key**: `[voice.tts] api_key`, from the masked field, which is
    /// emptied the moment the daemon has it.
    pub(crate) fn save_tts_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.voice.key.read(cx).value().trim().to_string();
        if typed.is_empty() {
            self.voice.key.update(cx, |input, cx| input.focus(window, cx));
            self.toast("Paste the key first.", window, cx);
            return;
        }
        let provider = self
            .voice
            .tts
            .as_ref()
            .and_then(TtsConfig::provider_row)
            .map_or("the provider", |(_, name, _)| name);
        self.write_tts(
            "voice.tts.api_key",
            ConfigValue::Text(typed),
            move |this, window, cx| {
                this.voice
                    .key
                    .update(cx, |input, cx| input.set_value("", window, cx));
                this.toast(
                    format!("Key saved. Announcements are spoken by {provider} from the next one."),
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// **Remove key**: the key is unset, Apple's synthesiser speaks.
    pub(crate) fn clear_tts_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.write_tts(
            "voice.tts.api_key",
            ConfigValue::Null,
            |this, window, cx| {
                this.toast("Key removed. Apple's synthesiser speaks.", window, cx);
            },
            window,
            cx,
        );
    }

    /// **Set model**: `[voice.tts] model`, blank for the provider's default.
    pub(crate) fn set_tts_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.voice.model.read(cx).value().trim().to_string();
        let (value, said) = if typed.is_empty() {
            (ConfigValue::Null, "Model reset to the provider's default.".to_string())
        } else {
            (ConfigValue::Text(typed.clone()), format!("Model set to {typed}."))
        };
        self.write_tts(
            "voice.tts.model",
            value,
            move |this, window, cx| this.toast(said.clone(), window, cx),
            window,
            cx,
        );
    }

    /// `voice.test{what}`. Only `speaker` makes a sound, and what it makes
    /// is §7.3's 110 ms `ready` chime at the operator's own `set speaker`
    /// gain — this button changes neither the system default device nor
    /// anybody's volume.
    pub(crate) fn run_test(
        &mut self,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ask(
            UiAction::VoiceTest {
                what: what.to_string(),
            },
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(value) => {
                    this.voice.report = Some(TestReport::parse(&value));
                    this.voice.report_error = None;
                }
                Err(e) => {
                    this.voice.report = None;
                    let line = if method_missing(&e) {
                        present::missing_method_line("voice.test")
                    } else {
                        format!("{what}: {e}")
                    };
                    this.voice.report_error = Some(line.clone());
                    this.toast(line, window, cx);
                }
            },
        );
    }

    /// `perm.request{which}` (§7.8).
    ///
    /// The reply is `{status:"prompt_not_shown"}` on every build until the
    /// signed bundle of §10.1 lands (§10.4), and the pane keeps it: once a
    /// permission has answered that, the button is replaced by the daemon's
    /// own sentences about why. Offering a button that cannot work twice is
    /// worse than not offering it once.
    pub(crate) fn request_permission(
        &mut self,
        which: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ask(
            UiAction::PermRequest {
                which: which.to_string(),
            },
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(value) => {
                    let prompt = Prompt::parse(&value);
                    this.toast(present::prompt_line(&prompt), window, cx);
                    this.voice.remember(prompt);
                }
                Err(e) => {
                    let line = if method_missing(&e) {
                        present::missing_method_line("perm.request")
                    } else {
                        format!("Could not ask for the {which}: {e}")
                    };
                    this.toast(line, window, cx);
                }
            },
        );
    }

    /// §7.3's `ready` chime, which is the only `voice.test` that makes a
    /// sound. The Welcome screen's Play button is this one.
    pub(crate) fn speaker_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_test(TESTS[0], window, cx);
    }

    /// The last `voice.test` answer, whichever screen asked for it.
    pub(crate) fn voice_report(&self) -> Option<&TestReport> {
        self.voice.report.as_ref()
    }

    // ---- the screen ----------------------------------------------------

    pub(crate) fn render_voice(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let reread = Btn::new("voice-refresh", "Re-read")
            .quiet()
            .icon(ic::REFRESH)
            .disabled(dim || self.voice.busy)
            .tooltip("Ask for the engine report again")
            .on_click(cx.listener(|this, _, window, cx| this.refresh_voice(true, window, cx)))
            .into_any_element();
        ui::page(
            "voice",
            &self.page_scroll,
            self.content_width,
            ui::PAGE_MAX,
            v_flex()
                .child(ui::page_header(
                    "Voice",
                    Some(
                        div()
                            .child(
                                "Everything here belongs to the service's audio side. The window \
                                 only watches it, and nothing on this screen changes your Mac's \
                                 audio devices.",
                            )
                            .into_any_element(),
                    ),
                    vec![reread],
                    &p,
                ))
                .child(self.render_route(cx))
                .child(self.render_meter(cx))
                .child(self.render_engines(cx))
                .child(self.render_recognition(dim, cx))
                .child(self.render_replies(dim, cx))
                .child(self.render_permissions(dim, cx))
                .child(self.render_test(dim, cx)),
        )
    }

    /// §7.2's route machine, drawn as the steps it can be in with the one it
    /// is in lit — and the two device UIDs under it.
    fn render_route(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let voice = &self.state.mirror().voice;
        let here = voice.route;
        let steps: Vec<Div> = present::route_steps(voice.transport)
            .iter()
            .map(|route| route_step(*route, *route == here, &p))
            .collect();
        let uid = |uid: Option<&str>| match uid {
            Some(uid) => div().mono().ts(12., 18.).child(uid.to_string()),
            None => div().text_color(p.faint).child("none selected"),
        };
        ui::section(
            "Route",
            ui::panel(&p)
                .gap(px(14.))
                .child(h_flex().flex_wrap().gap(px(6.)).children(steps))
                .child(
                    div()
                        .ts(13., 19.)
                        .text_color(p.text)
                        .child(present::route_help(here, voice.transport)),
                )
                .child(
                    v_flex()
                        .gap(px(6.))
                        .pt(px(12.))
                        .border_t_1()
                        .border_color(p.divider())
                        .child(ui::kv(
                            "Transport",
                            present::transport_line(voice.transport),
                            &p,
                        ))
                        .child(ui::kv("Output", uid(voice.output_uid.as_deref()), &p))
                        .child(ui::kv("Input", uid(voice.input_uid.as_deref()), &p)),
                ),
            &p,
        )
    }

    /// The capture meter (§7.1). It moves only while something is capturing,
    /// and it never says anything about the microphone: V2 measured that a
    /// dead mic and a warming SCO link both report exact digital zero for
    /// about a second, so a meter that read "no signal" would be guessing at
    /// hardware from a number that cannot tell.
    fn render_meter(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let level = self.state.level();
        let now = present::now();
        let live = level.filter(|l| present::fresh(l.at, now));
        let fill = live.map_or(0.0, |l| present::meter_level(l.rms));
        let color = if fill >= 0.95 {
            p.danger.opacity(0.9)
        } else if fill >= 0.8 {
            p.warning.opacity(0.9)
        } else {
            p.accent.opacity(0.8)
        };
        ui::section(
            "Microphone",
            ui::panel(&p)
                .gap(px(10.))
                .child(
                    h_flex()
                        .gap(px(12.))
                        .child(ui::icon(
                            ic::MICROPHONE,
                            16.,
                            if live.is_some() { p.text } else { p.faint },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded_full()
                                .bg(p.wash(0.08))
                                .child(div().h_full().w(relative(fill)).rounded_full().bg(color)),
                        ),
                )
                .child(ui::hint(present::meter_line(level, now), &p)),
            &p,
        )
    }

    /// The STT and TTS engines (§4.9), from the one report the audio side
    /// pushed when its engines came up.
    fn render_engines(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let engines = self.voice.engines.clone().unwrap_or_default();
        let locale = engines.locale.clone();
        let rows: Vec<AnyElement> = [
            ("Speech to text", ic::MICROPHONE, engines.stt()),
            ("Text to speech", ic::VOLUME, engines.tts()),
        ]
        .into_iter()
        .map(|(what, glyph, engine)| {
            let row = Row::new().tile(ui::tile(glyph, p.muted, &p)).title(what);
            match engine {
                Some(engine) => row
                    .meta(present::locales_line(&engine.locales, locale.as_deref()))
                    .trailing(ui::badge(present::engine_badge(engine), Tint::Neutral, &p)),
                None => row
                    .meta("Not reported.")
                    .trailing(ui::badge("none", Tint::Neutral, &p)),
            }
            .into_any_element()
        })
        .collect();
        ui::section(
            "Engines",
            v_flex()
                .gap(px(10.))
                .children(self.voice.engines_error.as_ref().map(|line| {
                    ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(line.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    )
                }))
                .child(ui::card(rows, &p))
                // §4.9: the TTS half reports locales. The voice pool is a
                // different object in a different call, and saying so is
                // cheaper than an operator hunting for a voice list that
                // was never here.
                .child(div().px(px(4.)).child(ui::hint(
                    "voice.engines reports the locales each engine accepts. The per-agent voice \
                     pool is the synthesiser's own ([voice.tts] voices, or [voice.voices] \
                     default_pool when Apple speaks) and the assignments the service persists \
                     \u{2014} not this list.",
                    &p,
                ))),
            &p,
        )
    }

    /// **Recognition** (§7.6): Apple's recogniser or a Whisper model on
    /// this Mac, whose weights are downloaded only from here (or the
    /// first-start prompt, or `deckd stt install`).
    fn render_recognition(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let known = self.voice.stt.is_some();
        let status = self.voice.stt.clone().unwrap_or_default();
        let engine = self
            .voice
            .stt_config
            .as_ref()
            .map_or(status.engine.clone(), |c| c.engine.clone());
        let dim = dim || !known;
        let downloading = status.downloading();
        let chips: Vec<AnyElement> = [("apple", "Apple"), ("whisper", "Whisper (local model)")]
            .into_iter()
            .map(|(id, label)| {
                ui::chip(
                    SharedString::from(format!("stt-engine-{id}")),
                    label,
                    engine == id,
                    &p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_stt(id, window, cx);
                }))
                .into_any_element()
            })
            .collect();
        let size = size_text(status.total);
        let state = if downloading {
            ("downloading", Tint::Neutral)
        } else if status.loaded {
            ("loaded", Tint::Success)
        } else if status.loading {
            ("loading", Tint::Neutral)
        } else if status.present {
            ("downloaded", Tint::Success)
        } else {
            ("not downloaded", Tint::Neutral)
        };
        let label = |text: &'static str| {
            div()
                .w(px(112.))
                .flex_none()
                .ts(12., 18.)
                .text_color(p.faint)
                .child(text)
        };
        let mut model_row = h_flex()
            .items_center()
            .gap(px(8.))
            .child(label("Model"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ts(12.5, 18.)
                    .mono()
                    .child(if status.model.is_empty() {
                        "\u{2014}".to_string()
                    } else {
                        format!("{} \u{b7} {size}", status.model)
                    }),
            )
            .child(ui::badge(state.0, state.1, &p));
        if !status.present && !downloading {
            model_row = model_row.child(
                Btn::new("stt-download", format!("Download ({size})"))
                    .solid()
                    .disabled(dim)
                    .tooltip("Fetches the model's weights into the caches directory; nothing else is downloaded")
                    .on_click(cx.listener(|this, _, window, cx| this.install_stt(window, cx))),
            );
        }
        if status.present || downloading {
            model_row = model_row.child(
                Btn::new(
                    "stt-remove",
                    if downloading { "Cancel download" } else { "Remove model" },
                )
                .danger_quiet()
                .disabled(dim)
                .tooltip("Deletes the weights; Apple's recogniser answers from then on")
                .on_click(cx.listener(|this, _, window, cx| this.remove_stt(window, cx))),
            );
        }
        ui::section(
            "Recognition",
            ui::panel(&p)
                .gap(px(12.))
                .child(ui::para(
                    "Apple's recogniser needs nothing downloaded and recognises English on this \
                     Mac. Whisper is a local model too: it recognises Chinese and English in one \
                     utterance, after a one-time download. Either way, nothing you say leaves the \
                     machine.",
                ))
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Engine"))
                        .children(chips),
                )
                .child(model_row)
                .children(status.fraction().map(|fraction| {
                    div().pl(px(120.)).child(ui::progress(fraction, &p))
                }))
                .child(ui::hint(
                    if known {
                        status.line()
                    } else {
                        "Reading the recogniser's status\u{2026}".to_string()
                    },
                    &p,
                ))
                .children(self.voice.stt_error.as_ref().map(|line| {
                    ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(line.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    )
                })),
            &p,
        )
    }

    /// **Spoken replies** — bring your own key (§7.6). The paragraph over
    /// the field is the control's licence to exist: it says that a key
    /// sends the announcements' text to the provider.
    fn render_replies(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let tts = self.voice.tts.clone().unwrap_or_default();
        let known = self.voice.tts.is_some();
        let dim = dim || !known || self.voice.writing;
        let engine = self.voice.engines.as_ref().and_then(Engines::tts);
        let label = |text: &'static str| {
            div()
                .w(px(112.))
                .flex_none()
                .ts(12., 18.)
                .text_color(p.faint)
                .child(text)
        };
        let engines: Vec<AnyElement> = [
            ("apple", "Apple"),
            ("kokoro", "Kokoro voice (local)"),
            ("cloud", "Cloud (your key)"),
        ]
        .into_iter()
        .map(|(id, name)| {
            ui::chip(
                SharedString::from(format!("tts-engine-{id}")),
                name,
                tts.engine == id,
                &p,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_tts(id, window, cx);
            }))
            .into_any_element()
        })
        .collect();
        // The Kokoro voice's package row, like the recogniser's.
        let voice = self.voice.voice.clone().unwrap_or_default();
        let voice_known = self.voice.voice.is_some();
        let downloading = voice.downloading();
        let voice_size = size_text(voice.total);
        let state = if downloading {
            ("downloading", Tint::Neutral)
        } else if voice.loaded {
            ("loaded", Tint::Success)
        } else if voice.loading {
            ("loading", Tint::Neutral)
        } else if voice.present {
            ("downloaded", Tint::Success)
        } else {
            ("not downloaded", Tint::Neutral)
        };
        let mut voice_row = h_flex()
            .items_center()
            .gap(px(8.))
            .child(label("Voice"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ts(12.5, 18.)
                    .mono()
                    .child(if voice.model.is_empty() {
                        "\u{2014}".to_string()
                    } else {
                        format!("{} \u{b7} {voice_size}", voice.model)
                    }),
            )
            .child(ui::badge(state.0, state.1, &p));
        if !voice.present && !downloading {
            voice_row = voice_row.child(
                Btn::new("tts-download", format!("Download ({voice_size})"))
                    .solid()
                    .disabled(dim || !voice_known)
                    .tooltip("Fetches the voice package into the caches directory; nothing else is downloaded")
                    .on_click(cx.listener(|this, _, window, cx| this.install_tts(window, cx))),
            );
        }
        if voice.present || downloading {
            voice_row = voice_row.child(
                Btn::new(
                    "tts-remove",
                    if downloading { "Cancel download" } else { "Remove voice" },
                )
                .danger_quiet()
                .disabled(dim || !voice_known)
                .tooltip("Deletes the package; Apple's synthesiser speaks from then on")
                .on_click(cx.listener(|this, _, window, cx| this.remove_tts(window, cx))),
            );
        }
        let chips: Vec<AnyElement> = PROVIDERS
            .into_iter()
            .map(|(id, name, _)| {
                ui::chip(
                    SharedString::from(format!("tts-provider-{id}")),
                    name,
                    tts.provider.eq_ignore_ascii_case(id),
                    &p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_tts_provider(id, window, cx);
                }))
                .into_any_element()
            })
            .collect();
        let cloud = tts.engine == "cloud";
        let kokoro = tts.engine == "kokoro";
        ui::section(
            "Spoken replies",
            ui::panel(&p)
                .gap(px(12.))
                .child(ui::para(
                    "Apple's synthesiser speaks on this Mac and needs no download. A Kokoro voice \
                     speaks Chinese and English, also on this Mac, after a one-time download. With \
                     your own key, each announcement is spoken by a provider's model instead, and its \
                     text is sent to the provider to be synthesised. Changes take effect at the next \
                     announcement.",
                ))
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Engine"))
                        .children(engines),
                )
                .children(kokoro.then_some(voice_row))
                .children(kokoro.then(|| voice.fraction()).flatten().map(|fraction| {
                    div().pl(px(120.)).child(ui::progress(fraction, &p))
                }))
                .children(cloud.then(|| {
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Provider"))
                        .children(chips)
                }))
                .children(cloud.then(|| {
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .child(label("API key"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(ui::field(&self.voice.key, &p).disabled(dim).mono()),
                        )
                        .child(
                            Btn::new("tts-key-save", "Save key")
                                .solid()
                                .disabled(dim)
                                .tooltip("Writes [voice.tts] api_key; the field is emptied once it is saved")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_tts_key(window, cx);
                                })),
                        )
                        .child(
                            Btn::new("tts-key-clear", "Remove key")
                                .danger_quiet()
                                .disabled(dim || !tts.has_key())
                                .tooltip("Unsets the key: Apple's synthesiser speaks again")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_tts_key(window, cx);
                                })),
                        )
                }))
                .children(cloud.then(|| {
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Model"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(ui::field(&self.voice.model, &p).disabled(dim).mono()),
                        )
                        .child(
                            Btn::new("tts-model-set", "Set model")
                                .disabled(dim)
                                .tooltip("Writes [voice.tts] model; blank is the provider's default")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_tts_model(window, cx);
                                })),
                        )
                }))
                .child(ui::hint(
                    if known {
                        replies_line(&tts, self.voice.voice.as_ref(), engine)
                    } else {
                        "Reading [voice.tts]\u{2026}".to_string()
                    },
                    &p,
                ))
                .children(self.voice.voice_error.as_ref().map(|line| {
                    ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(line.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    )
                }))
                .children(self.voice.tts_error.as_ref().map(|line| {
                    ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(line.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    )
                })),
            &p,
        )
    }

    /// §7.8's permissions, with what can honestly be done about each.
    fn render_permissions(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        ui::section(
            "Permissions",
            v_flex()
                .gap(px(10.))
                .child(ui::card(self.voice_permission_rows(dim, cx), &p))
                .child(
                    div()
                        .px(px(4.))
                        .child(ui::hint(present::PERMISSION_CAVEAT, &p)),
                ),
            &p,
        )
    }

    /// Welcome and Voice share the permission list, including Bluetooth
    /// before the audio engine's first transport report arrives.
    pub(crate) fn voice_permission_rows(&self, dim: bool, cx: &Context<Self>) -> Vec<AnyElement> {
        let voice = &self.state.mirror().voice;
        let permissions = voice.permissions;
        displayed_permissions(voice.transport)
            .iter()
            .copied()
            .map(|which| {
                let state = match which {
                    "microphone" => permissions.microphone,
                    "bluetooth" => permissions.bluetooth,
                    _ => permissions.speech,
                };
                self.permission_row(which, state, dim, cx)
                    .into_any_element()
            })
            .collect()
    }

    /// One permission's row. The Welcome screen's second step draws the same
    /// rows, so the two can never disagree about when Grant is offered.
    pub(crate) fn permission_row(
        &self,
        which: &'static str,
        state: Permission,
        dim: bool,
        cx: &Context<Self>,
    ) -> Row {
        let p = *palette(cx);
        let settings = match which {
            "microphone" => deck_gui_core::host_state::MICROPHONE_SETTINGS,
            "bluetooth" => deck_gui_core::host_state::BLUETOOTH_SETTINGS,
            _ => deck_gui_core::host_state::SPEECH_SETTINGS,
        };
        let glyph = match which {
            "microphone" => ic::MICROPHONE,
            "bluetooth" => ic::BLUETOOTH,
            _ => ic::CHAT,
        };
        let transport = self.state.mirror().voice.transport;
        let asked = self.voice.prompt(which);
        // The Grant button is offered only where macOS could still be asked
        // *and* this build has not already been told it cannot ask. A denied
        // permission is never one of those: macOS will not prompt twice.
        // Bluetooth never is: macOS asks by itself when the link starts, and
        // there is no `perm.request` for it.
        let offer_grant = which != "bluetooth"
            && matches!(state, Permission::NotDetermined | Permission::Unknown)
            && asked.is_none_or(|p| !p.not_shown());
        let mut row = Row::new()
            .tile(ui::tile(glyph, p.muted, &p))
            .title(present::permission_title(which, transport))
            .meta(present::permission_help(state))
            .trailing(ui::badge(
                present::permission_word(state),
                Tint::of(present::permission_tone(state)),
                &p,
            ));
        if let Some(note) = present::permission_note(which, transport) {
            row = row.detail(ui::hint(note, &p));
        }
        if offer_grant {
            row = row.trailing(
                Btn::new(
                    SharedString::from(format!("grant-{which}")),
                    "Grant\u{2026}",
                )
                .small()
                .outlined()
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request_permission(which, window, cx);
                })),
            );
        }
        if state == Permission::Denied {
            row = row.trailing(
                Btn::new(
                    SharedString::from(format!("settings-{which}")),
                    "Open Settings",
                )
                .small()
                .outlined()
                .icon(ic::EXTERNAL)
                .on_click(move |_, _, cx: &mut App| cx.open_url(settings)),
            );
        }
        // What the daemon said when this button was last pressed, in its
        // words. This is what stands in for the button once the prompt has
        // answered `prompt_not_shown`.
        if let Some(prompt) = asked {
            row = row.detail(
                v_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .ts(12., 17.)
                            .text_color(p.warning_muted)
                            .child(present::prompt_line(prompt)),
                    )
                    .children(
                        prompt
                            .limits
                            .iter()
                            .map(|limit| ui::hint(limit.clone(), &p)),
                    ),
            );
        }
        row
    }

    /// §8.2's **Test**, and the honest account of what each button does.
    fn render_test(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let buttons: Vec<Btn> = TESTS
            .into_iter()
            .map(|what| {
                let button = Btn::new(
                    SharedString::from(format!("voice-test-{what}")),
                    test_label(what),
                )
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_test(what, window, cx);
                }));
                if what == "speaker" {
                    button.solid().icon(ic::PLAY)
                } else {
                    button
                }
            })
            .collect();
        ui::section(
            "Test",
            ui::panel(&p)
                .gap(px(12.))
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .children(buttons),
                )
                .child(ui::hint(present::TEST_NOTE, &p))
                .children(
                    self.voice
                        .report
                        .as_ref()
                        .map(|report| test_result(report, true, &p)),
                )
                .children(self.voice.report_error.as_ref().map(|line| {
                    ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(line.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    )
                })),
            &p,
        )
    }
}

/// A `voice.test` answer: the headline, the fields the reply carried, and
/// the daemon's own account of what it did not check, verbatim. The Welcome
/// screen shows the same block.
pub(crate) fn test_result(report: &TestReport, with_heading: bool, p: &Palette) -> Div {
    v_flex()
        .gap(px(6.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(p.wash(0.04))
        .child(
            h_flex()
                .gap(px(8.))
                .child(ui::icon(ic::CHECK_CIRCLE, 15., p.success_muted))
                .child(
                    div()
                        .ts(13., 17.)
                        .medium()
                        .child(present::test_headline(report)),
                ),
        )
        .children(
            present::test_lines(report)
                .into_iter()
                .map(|line| div().mono().ts(12., 18.).text_color(p.muted).child(line)),
        )
        .when(with_heading && !report.limits.is_empty(), |this| {
            this.child(
                div()
                    .mt(px(6.))
                    .ts(12., 16.)
                    .medium()
                    .text_color(p.muted)
                    .child("What this did not check"),
            )
        })
        // Verbatim. These are the daemon's own sentences about what it did
        // not do, and the window does not get to paraphrase them.
        .children(report.limits.iter().map(|limit| ui::hint(limit.clone(), p)))
}

/// One step of the route diagram: the one the machine is in is lit in its
/// tone, the others are quiet.
fn route_step(route: Route, here: bool, p: &Palette) -> Div {
    let tone = present::route_tone(route);
    h_flex()
        .gap(px(6.))
        .h(px(28.))
        .px(px(10.))
        .rounded(px(8.))
        .ts(12., 16.)
        .map(|this| {
            if here {
                let gray = tone == deck_model::Tone::Gray;
                this.bg(if gray {
                    p.wash(0.12)
                } else {
                    p.tone(tone).alpha(0.14)
                })
                .text_color(if gray { p.text } else { p.tone_text(tone) })
                .medium()
                .child(ui::dot(if gray { p.muted } else { p.tone(tone) }, 6.))
            } else {
                this.bg(p.wash(0.05)).text_color(p.faint)
            }
        })
        .child(present::route_word(route))
}

/// The four Test buttons' labels. Only the first makes a sound, and the
/// labels say which one that is before it is pressed.
fn test_label(what: &str) -> &'static str {
    match what {
        "speaker" => "Play the ready chime",
        "mic" => "Report the input",
        "stt" => "Report the recogniser",
        _ => "Report the synthesiser",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_of_the_four_test_buttons_says_it_will_make_a_sound() {
        let labels: Vec<&str> = TESTS.into_iter().map(test_label).collect();
        assert_eq!(labels[0], "Play the ready chime");
        for label in &labels[1..] {
            assert!(
                label.starts_with("Report"),
                "{label} must not read as an exercise: none of these runs anything"
            );
        }
        // And the standing note over the buttons says the same thing.
        assert!(present::TEST_NOTE.contains("Only Speaker makes a sound"));
        assert!(present::TEST_NOTE.contains("does not open the microphone"));
    }

    #[test]
    fn a_permission_that_answered_prompt_not_shown_is_not_offered_a_button_again() {
        // The pane's own bookkeeping: one remembered prompt per permission,
        // replaced and never duplicated.
        let mut prompts = Prompts::default();
        assert!(prompts.get("microphone").is_none());
        prompts.remember(Prompt {
            which: "microphone".into(),
            status: "prompt_not_shown".into(),
            limits: vec!["the prompt is not shown".into()],
        });
        prompts.remember(Prompt {
            which: "microphone".into(),
            status: "prompt_not_shown".into(),
            limits: vec!["the prompt is not shown".into()],
        });
        assert_eq!(prompts.list.len(), 1, "one answer per permission");
        assert!(prompts.get("microphone").is_some_and(Prompt::not_shown));
        assert!(prompts.get("speech").is_none());
    }

    #[test]
    fn the_replies_line_says_what_speaks_and_never_a_key() {
        let engine = Engine {
            id: "openai".into(),
            on_device: false,
            locales: vec!["en-US".into()],
        };
        let mut tts = TtsConfig {
            engine: "cloud".into(),
            provider: "openai".into(),
            key_hint: "\u{2026}mnop (35 chars)".into(),
            model: String::new(),
            voices: Vec::new(),
        };
        let line = replies_line(&tts, None, Some(&engine));
        assert!(line.contains("spoken by OpenAI"), "{line}");
        assert!(line.contains("gpt-4o-mini-tts"), "{line}");
        assert!(line.contains("openai \u{b7} server"), "{line}");
        assert!(line.contains("\u{2026}mnop"), "the hint, which is not the key: {line}");
        tts.key_hint.clear();
        assert_eq!(
            replies_line(&tts, None, Some(&engine)),
            "No key: Apple's synthesiser speaks, on this Mac."
        );
        tts.engine = "mlx".into();
        assert!(replies_line(&tts, None, None).contains("not used until the engine is \"cloud\""));
        tts.engine = "kokoro".into();
        let voice = TtsStatus {
            engine: "kokoro".into(),
            model: "kokoro-multi-lang-v1_1".into(),
            total: 364_816_464,
            present: true,
            loaded: true,
            ..Default::default()
        };
        assert!(replies_line(&tts, Some(&voice), None).contains("speaks on this Mac"));
        tts.engine = "apple".into();
        assert_eq!(replies_line(&tts, None, None), "Apple's synthesiser speaks, on this Mac.");
    }
}
