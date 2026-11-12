//! The Voice page's rules, read without a window: which radio is on for each
//! engine (the service's default `cloud` with no key is Apple's voice), what
//! a local model's row says in each of its states, the Speech language
//! popup's value, its choices and its two writes, the signal path for each
//! pair of engines and the Conol switch, the permissions and what each one
//! offers, the test line, the key's hint and what Save Key writes, the
//! harness's scenes, and that every word the page says has a row in all four
//! languages.
use deck_client::ClientError;
use deck_gui_core::care::{TestState, TestStatus, VoiceTest};
use deck_gui_core::i18n::Language;
use deck_gui_core::voice::{
    Download, Engines, Prompt, SttConfig, SttStatus, TestReport, TtsConfig, TtsStatus,
};
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Permission, Snapshot, Tone};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::shots::Scene;
use openwalkietalkie_app::voice::{
    bluetooth_grant, dialog_provider, key_tail, locale_language, locale_name, percent,
    provider_name, recogniser, recognises, save_key_action, signal_path, speaks, speech_choice,
    speech_grant, speech_language_action, speech_locales, speech_reported, speech_state, staged,
    stt_slot, synthesiser, test_line, tts_slot, Grant, Offer, Recogniser, Route, Slot,
    SpeechChoice, Stop, Synthesiser, TestLine, NAMED_LOCALES,
};
use openwalkietalkie_app::voice_actions::{chime_complaint, refusal, Prompts, Refusal};
use serde_json::json;

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;

/// The Whisper model's size, as the service catalogues it.
const WHISPER_BYTES: u64 = 1_624_555_275;

fn tts(engine: &str, provider: &str, key_hint: &str) -> TtsConfig {
    TtsConfig {
        engine: engine.into(),
        provider: provider.into(),
        key_hint: key_hint.into(),
        ..TtsConfig::default()
    }
}

fn stt(engine: &str, language: &str, locale: &str, companion: bool) -> SttConfig {
    SttConfig {
        engine: engine.into(),
        language: language.into(),
        locale: locale.into(),
        companion,
        ..SttConfig::default()
    }
}

/// The Whisper model on this Mac, loaded.
fn whisper_here() -> SttStatus {
    SttStatus {
        engine: "whisper".into(),
        model: "large-v3-turbo".into(),
        total: WHISPER_BYTES,
        present: true,
        loaded: true,
        ..SttStatus::default()
    }
}

/// A download of the Whisper model, `bytes` of `total` in.
fn coming_down(bytes: u64, total: u64) -> Option<Download> {
    Some(Download {
        model: "large-v3-turbo".into(),
        bytes,
        total,
        running: true,
        ..Download::default()
    })
}

#[test]
fn the_reply_radio_is_the_voice_that_speaks_and_the_default_cloud_with_no_key_is_apples() {
    let key = "\u{2026}mnop (35 chars)";
    assert_eq!(
        synthesiser(&tts("apple", "openai", "")),
        Some(Synthesiser::Apple)
    );
    // The service's default, on a fresh Mac: Apple's voice speaks, so it is
    // Apple voice's radio that is on and not Cloud voice's.
    assert_eq!(
        synthesiser(&tts("cloud", "openai", "")),
        Some(Synthesiser::Apple)
    );
    let fresh = TtsConfig::parse(&json!({"voice": {"tts.engine": "cloud", "tts.api_key": ""}}));
    assert_eq!(synthesiser(&fresh), Some(Synthesiser::Apple));
    assert_eq!(
        synthesiser(&tts("cloud", "openai", key)),
        Some(Synthesiser::Cloud)
    );
    // A key kept while another voice speaks does not move the radio.
    assert_eq!(
        synthesiser(&tts("kokoro", "openai", key)),
        Some(Synthesiser::Kokoro)
    );
    assert_eq!(
        synthesiser(&tts("kokoro", "openai", "")),
        Some(Synthesiser::Kokoro)
    );
    assert_eq!(
        synthesiser(&tts("mlx", "openai", "")),
        Some(Synthesiser::Mlx)
    );
    // A word this window does not know is no radio's.
    assert_eq!(synthesiser(&tts("piper", "openai", "")), None);
    assert_eq!(synthesiser(&TtsConfig::default()), None);
    // What each choice writes.
    let words: Vec<&str> = [
        Synthesiser::Apple,
        Synthesiser::Kokoro,
        Synthesiser::Cloud,
        Synthesiser::Mlx,
    ]
    .map(Synthesiser::id)
    .to_vec();
    assert_eq!(words, ["apple", "kokoro", "cloud", "mlx"]);
}

#[test]
fn the_recognition_radio_is_the_engine_the_settings_name() {
    assert_eq!(
        recogniser(&stt("apple", "auto", "en-US", true)),
        Some(Recogniser::Apple)
    );
    assert_eq!(
        recogniser(&stt("whisper", "auto", "en-US", true)),
        Some(Recogniser::Whisper)
    );
    assert_eq!(recogniser(&stt("", "auto", "en-US", true)), None);
    assert_eq!(recogniser(&stt("deepgram", "auto", "en-US", true)), None);
    assert_eq!(Recogniser::Apple.id(), "apple");
    assert_eq!(Recogniser::Whisper.id(), "whisper");
}

#[test]
fn a_local_models_row_says_whether_it_is_here_and_what_can_be_done_about_it() {
    // On this Mac, loaded or loading before the next press.
    assert_eq!(stt_slot(&whisper_here()), Slot::Installed);
    let idle = SttStatus {
        loaded: false,
        ..whisper_here()
    };
    assert_eq!(stt_slot(&idle), Slot::Installed);
    let loading = SttStatus {
        loading: true,
        ..idle.clone()
    };
    assert_eq!(stt_slot(&loading), Slot::Loading);
    // On this Mac and refused by the engine: fetched again on Retry.
    let refused = SttStatus {
        load_error: Some("bad magic".into()),
        ..idle.clone()
    };
    assert_eq!(
        stt_slot(&refused),
        Slot::Failed {
            why: "bad magic".into(),
            load: true
        }
    );
    // Not here: the download, at the catalogue's size; none for a model the
    // service cannot fetch.
    let absent = SttStatus {
        present: false,
        loaded: false,
        ..whisper_here()
    };
    assert_eq!(
        stt_slot(&absent),
        Slot::Download {
            bytes: WHISPER_BYTES
        }
    );
    let uncatalogued = SttStatus {
        total: 0,
        ..absent.clone()
    };
    assert_eq!(stt_slot(&uncatalogued), Slot::Download { bytes: 0 });
    // Coming down, however far; and with no size to say how far by.
    let downloading = SttStatus {
        download: coming_down(682_400_000, WHISPER_BYTES),
        ..absent.clone()
    };
    assert_eq!(
        stt_slot(&downloading),
        Slot::Downloading { percent: Some(42) }
    );
    let sizeless = SttStatus {
        download: coming_down(5_000, 0),
        ..absent.clone()
    };
    assert_eq!(stt_slot(&sizeless), Slot::Downloading { percent: None });
    // A download that stopped, while the model is not here.
    let stopped = SttStatus {
        download: Some(Download {
            running: false,
            error: Some("curl: (6) Could not resolve host".into()),
            ..Download::default()
        }),
        ..absent.clone()
    };
    assert_eq!(
        stt_slot(&stopped),
        Slot::Failed {
            why: "curl: (6) Could not resolve host".into(),
            load: false
        }
    );
    // The same error after the model arrived some other way is history.
    let stale = SttStatus {
        present: true,
        ..stopped
    };
    assert_eq!(stt_slot(&stale), Slot::Installed);
    // The Kokoro voice's row reads `tts.status` the same way.
    let voice = TtsStatus::parse(&json!({
        "engine": "cloud", "model": "kokoro-multi-lang-v1_1", "total": 364_816_464,
        "present": false, "download": null
    }));
    assert_eq!(tts_slot(&voice), Slot::Download { bytes: 364_816_464 });
    let voice = TtsStatus::parse(&json!({
        "engine": "kokoro", "present": true, "loaded": true, "voices": ["af_heart"]
    }));
    assert_eq!(tts_slot(&voice), Slot::Installed);
}

#[test]
fn a_download_is_said_in_whole_percent_and_100_only_when_whole() {
    assert_eq!(percent(0, 1_000), Some(0));
    assert_eq!(percent(999, 1_000), Some(99));
    assert_eq!(percent(1_000, 1_000), Some(100));
    // More than the catalogue said is whole, not more than whole.
    assert_eq!(percent(1_500, 1_000), Some(100));
    assert_eq!(percent(WHISPER_BYTES - 1, WHISPER_BYTES), Some(99));
    assert_eq!(percent(10, 0), None);
}

#[test]
fn the_speech_language_popup_reads_automatic_a_locale_or_a_language_set_by_hand() {
    i18n::install();
    let choice =
        |language: &str, locale: &str| speech_choice(&stt("whisper", language, locale, true));
    assert_eq!(choice("auto", "en-US"), SpeechChoice::Automatic);
    assert_eq!(choice("AUTO", "zh-CN"), SpeechChoice::Automatic);
    assert_eq!(choice("zh", "zh-CN"), SpeechChoice::Locale("zh-CN".into()));
    // An empty language follows the locale.
    assert_eq!(choice("", "en-GB"), SpeechChoice::Locale("en-GB".into()));
    // A language set apart from the locale is shown as it is set.
    assert_eq!(choice("ja", "en-US"), SpeechChoice::Language("ja".into()));
    assert_eq!(choice("", ""), SpeechChoice::Language(String::new()));
    // In words.
    assert_eq!(SpeechChoice::Automatic.word(EN), "Automatic");
    assert_eq!(SpeechChoice::Automatic.word(ZH), "自动");
    let chinese = SpeechChoice::Locale("zh-CN".into());
    assert_eq!(chinese.word(EN), "Chinese (China mainland)");
    assert_eq!(chinese.word(ZH), "中文（中国大陆）");
    // A locale with no name here is its code; a language alone is its code.
    assert_eq!(SpeechChoice::Locale("ar-SA".into()).word(ZH), "ar-SA");
    assert_eq!(SpeechChoice::Language("ja".into()).word(EN), "ja");
    assert_eq!(SpeechChoice::Language(String::new()).word(EN), "\u{2014}");
}

#[test]
fn every_named_locale_has_its_name_in_the_four_languages() {
    i18n::install();
    for code in NAMED_LOCALES {
        let name = locale_name(code, EN).unwrap_or_else(|| panic!("{code} has no name"));
        assert!(deck_gui_core::i18n::has_entry(name), "{name} has no row");
        for language in [
            Language::SimplifiedChinese,
            Language::TraditionalChinese,
            Language::Japanese,
        ] {
            assert_ne!(
                locale_name(code, language),
                Some(name),
                "{code} in {language:?}"
            );
        }
    }
    assert_eq!(locale_name("zh-HK", EN), Some("Cantonese (Hong Kong)"));
    assert_eq!(
        locale_name("yue-CN", Language::Japanese),
        Some("広東語（中国本土）")
    );
    assert_eq!(locale_name("hi-IN-translit", EN), None);
}

#[test]
fn the_popup_offers_the_named_locales_first_then_the_rest_and_always_the_one_set() {
    let english = stt("apple", "auto", "en-US", true);
    // Apple's recogniser reports sixty-two: the eighteen with names come
    // first, in the window's order, then the others as reported.
    let apple = Engines::parse(
        &serde_json::from_str(include_str!(
            "../../deck-gui-core/tests/fixtures/daemon/voice_engines.json"
        ))
        .expect("the service's own fixture"),
    );
    let locales = speech_locales(Some(&apple), &english);
    assert_eq!(locales.len(), 62);
    assert_eq!(&locales[..18], NAMED_LOCALES.map(String::from).as_slice());
    assert_eq!(&locales[18..21], ["ar-SA", "ca-ES", "cs-CZ"]);
    assert!(locales.iter().any(|code| code == "hi-IN-translit"));
    // Whisper's sixteen all have names, in the window's order.
    let whisper = Engines::parse(&json!({
        "selected": {"stt": "whisper"},
        "stt": [
            {"id": "apple", "locales": ["fr-CA"]},
            {"id": "whisper", "locales": ["vi-VN", "en-US", "zh-CN", "ja-JP"]}
        ]
    }));
    assert_eq!(
        speech_locales(Some(&whisper), &english),
        ["en-US", "zh-CN", "ja-JP", "vi-VN"]
    );
    // A locale the recogniser does not report is still offered, last, so
    // the value shown can be chosen again; none is offered twice.
    let welsh = stt("whisper", "cy", "cy-GB", true);
    assert_eq!(
        speech_locales(Some(&whisper), &welsh),
        ["en-US", "zh-CN", "ja-JP", "vi-VN", "cy-GB"]
    );
    // No engine report (a service started with `--no-device`): the one set.
    assert_eq!(speech_locales(None, &english), ["en-US"]);
    assert!(speech_locales(None, &stt("apple", "auto", "", true)).is_empty());
}

#[test]
fn choosing_a_language_writes_its_language_and_locale_together_and_automatic_writes_auto() {
    assert_eq!(
        speech_language_action(None),
        UiAction::ConfigSet {
            key: "voice.stt.language".into(),
            value: ConfigValue::Text("auto".into()),
        }
    );
    assert_eq!(
        speech_language_action(None).request(),
        (
            "config.set",
            json!({"patch": {"voice.stt.language": "auto"}})
        )
    );
    // One patch: Whisper's language and Apple's locale land together.
    assert_eq!(
        speech_language_action(Some("zh-CN")).request(),
        (
            "config.set",
            json!({"patch": {"voice.stt.language": "zh", "voice.stt.locale": "zh-CN"}})
        )
    );
    assert_eq!(
        speech_language_action(Some("yue-CN")).request().1["patch"]["voice.stt.language"],
        "yue"
    );
    for (locale, language) in [("es-419", "es"), ("hi-IN-translit", "hi"), ("en_GB", "en")] {
        assert_eq!(locale_language(locale), language);
    }
}

#[test]
fn the_signal_path_names_what_hears_and_what_speaks_now() {
    let key = "\u{2026}mnop (35 chars)";
    let here = whisper_here();
    let kokoro_here = TtsStatus {
        engine: "kokoro".into(),
        present: true,
        ..TtsStatus::default()
    };
    let agents = |stt_config: &SttConfig, tts_config: &TtsConfig| {
        let routes = signal_path(stt_config, Some(&here), tts_config, Some(&kokoro_here));
        assert_eq!(routes[0].0, Route::Agents);
        routes[0].1.clone()
    };
    let path = |hears: Stop, speaker: Stop| {
        vec![Stop::Microphone, hears, Stop::Agent, speaker, Stop::Speaker]
    };
    for (recogniser, hears) in [
        ("apple", Stop::OnMac("Apple")),
        ("whisper", Stop::OnMac("Whisper")),
    ] {
        let config = stt(recogniser, "auto", "en-US", false);
        for (voice, speaker) in [
            (tts("apple", "openai", ""), Stop::OnMac("Apple")),
            (tts("cloud", "openai", ""), Stop::OnMac("Apple")),
            (tts("cloud", "openai", key), Stop::Cloud("OpenAI".into())),
            (
                tts("cloud", "elevenlabs", key),
                Stop::Cloud("ElevenLabs".into()),
            ),
            (tts("kokoro", "openai", key), Stop::OnMac("Kokoro")),
            (tts("mlx", "openai", ""), Stop::OnMac("Qwen3-TTS")),
        ] {
            assert_eq!(
                agents(&config, &voice),
                path(hears.clone(), speaker),
                "{recogniser} and {}",
                voice.engine
            );
        }
    }
    // A model chosen and not here does no work: Apple's stands in.
    let whisper = stt("whisper", "auto", "en-US", false);
    let downloading = SttStatus {
        present: false,
        download: coming_down(1, WHISPER_BYTES),
        ..here.clone()
    };
    assert_eq!(
        recognises(&whisper, Some(&downloading)),
        Some(Recogniser::Apple)
    );
    let refused = SttStatus {
        load_error: Some("bad magic".into()),
        ..here.clone()
    };
    assert_eq!(
        recognises(&whisper, Some(&refused)),
        Some(Recogniser::Apple)
    );
    // Before stt.status answers, what is chosen.
    assert_eq!(recognises(&whisper, None), Some(Recogniser::Whisper));
    let kokoro = tts("kokoro", "openai", "");
    let not_here = TtsStatus {
        present: false,
        ..kokoro_here.clone()
    };
    assert_eq!(speaks(&kokoro, Some(&not_here)), Stop::OnMac("Apple"));
    assert_eq!(speaks(&kokoro, None), Stop::OnMac("Kokoro"));
    // A provider this window does not know is named as the service names it.
    assert_eq!(
        speaks(&tts("cloud", "acme", key), None),
        Stop::Cloud("acme".into())
    );
    assert_eq!(
        speaks(&tts("piper", "", ""), None),
        Stop::Other("piper".into())
    );
    // Conol's route is drawn only while Conol hears its chats' presses.
    let apple = tts("apple", "openai", "");
    let with_conol = signal_path(&stt("apple", "auto", "en-US", true), None, &apple, None);
    assert_eq!(with_conol.len(), 2);
    assert_eq!(
        with_conol[1],
        (
            Route::Conol,
            vec![Stop::Microphone, Stop::ConolCloud, Stop::Chat]
        )
    );
    assert_eq!(
        signal_path(&stt("apple", "auto", "en-US", false), None, &apple, None).len(),
        1
    );
    i18n::install();
    assert_eq!(Route::Agents.name(EN), "Agent sessions");
    assert_eq!(Route::Conol.name(ZH), "Conol 聊天");
}

#[test]
fn each_permission_offers_the_one_thing_that_can_honestly_be_done_about_it() {
    i18n::install();
    let grant = |word, tone, offer| Grant { word, tone, offer };
    // Bluetooth is never asked for from here: macOS asks when the link
    // starts. Denied, only System Settings can change it.
    assert_eq!(
        bluetooth_grant(Permission::Granted, EN),
        grant("Granted", Tone::Green, None)
    );
    assert_eq!(
        bluetooth_grant(Permission::NotDetermined, EN),
        grant("Not asked", Tone::Gray, None)
    );
    assert_eq!(
        bluetooth_grant(Permission::Denied, EN),
        grant("Denied", Tone::Amber, Some(Offer::OpenSettings))
    );
    assert_eq!(
        bluetooth_grant(Permission::Unknown, EN),
        grant("Not checked", Tone::Gray, None)
    );
    // Speech Recognition: Ask… only while macOS has not been asked and
    // Apple's recogniser hears.
    assert_eq!(
        speech_grant(Permission::NotDetermined, true, false, EN),
        grant("Not asked", Tone::Gray, Some(Offer::Ask))
    );
    assert_eq!(
        speech_grant(Permission::NotDetermined, false, false, EN),
        grant("Not asked", Tone::Gray, None)
    );
    // A state the service did not read is not checked, and offers nothing:
    // macOS may have answered already, and there is nothing known to act on.
    for apple in [true, false] {
        assert_eq!(
            speech_grant(Permission::Unknown, apple, false, EN),
            grant("Not checked", Tone::Gray, None)
        );
    }
    // Not after the service said it cannot show the prompt.
    assert_eq!(
        speech_grant(Permission::NotDetermined, true, true, EN).offer,
        None
    );
    // macOS will not ask twice: denied is System Settings, whoever hears.
    for apple in [true, false] {
        assert_eq!(
            speech_grant(Permission::Denied, apple, false, EN).offer,
            Some(Offer::OpenSettings)
        );
        assert_eq!(
            speech_grant(Permission::Granted, apple, false, EN).offer,
            None
        );
    }
    assert_eq!(
        speech_grant(Permission::Granted, true, false, ZH).word,
        "已授权"
    );
    assert_eq!(
        speech_grant(Permission::NotDetermined, true, false, ZH).word,
        "尚未请求"
    );
    assert_eq!(
        speech_grant(Permission::Denied, true, false, ZH).word,
        "已拒绝"
    );
}

#[test]
fn speech_recognition_is_what_the_service_reports_and_not_checked_only_when_it_cannot_say() {
    i18n::install();
    let status = |speech: serde_json::Value| {
        json!({
            "microphone": "unknown", "speech": speech, "bluetooth": "granted",
            "automation": {"ghostty": "unknown"}, "accessibility": "unknown", "limits": []
        })
    };
    // `perm.status` in each of macOS's answers.
    for (word, want) in [
        ("not_determined", Some(Permission::NotDetermined)),
        ("granted", Some(Permission::Granted)),
        ("denied", Some(Permission::Denied)),
        ("unknown", Some(Permission::Unknown)),
        // A word this build does not know is not a state it can show.
        ("restricted", None),
    ] {
        assert_eq!(speech_reported(&status(json!(word))), want, "{word}");
    }
    assert_eq!(speech_reported(&json!({"microphone": "unknown"})), None);
    // The mirror's while it knows (the service's events keep it current),
    // else what `perm.status` said, else not checked.
    assert_eq!(
        speech_state(Permission::Unknown, Some(Permission::Granted)),
        Permission::Granted
    );
    assert_eq!(
        speech_state(Permission::Denied, Some(Permission::NotDetermined)),
        Permission::Denied
    );
    assert_eq!(speech_state(Permission::Unknown, None), Permission::Unknown);
    // What the row then says and offers, Apple's recogniser hearing.
    let row = |speech: &str| {
        let shown = speech_state(Permission::Unknown, speech_reported(&status(json!(speech))));
        speech_grant(shown, true, false, EN)
    };
    assert_eq!(row("not_determined").word, "Not asked");
    assert_eq!(row("not_determined").offer, Some(Offer::Ask));
    assert_eq!(row("granted").word, "Granted");
    assert_eq!(row("granted").offer, None);
    assert_eq!(row("denied").offer, Some(Offer::OpenSettings));
    assert_eq!(row("unknown").word, "Not checked");
    assert_eq!(row("unknown").offer, None);
    // The fresh test service's own reply: it could not read it.
    let recorded = recorded(RECORDED_PERM);
    assert_eq!(speech_reported(&recorded), Some(Permission::Unknown));
    assert_eq!(
        speech_grant(
            speech_state(Permission::Unknown, speech_reported(&recorded)),
            true,
            false,
            EN
        ),
        Grant {
            word: "Not checked",
            tone: Tone::Gray,
            offer: None
        }
    );
    // What the page asks for it.
    assert_eq!(UiAction::PermStatus.request().0, "perm.status");
}

#[test]
fn a_permission_whose_prompt_the_service_cannot_show_is_not_offered_again() {
    let mut prompts = Prompts::default();
    assert!(prompts.get("speech").is_none());
    assert!(!prompts.refused("speech"));
    let not_shown = Prompt::parse(&json!({
        "which": "speech",
        "status": "prompt_not_shown",
        "limits": ["the prompt is not shown: it needs the signed bundle"]
    }));
    prompts.remember(not_shown);
    assert!(prompts.refused("speech"));
    assert!(!prompts.refused("microphone"));
    // One answer per permission: a later one takes its place.
    prompts.remember(Prompt::parse(
        &json!({"which": "speech", "status": "granted"}),
    ));
    assert!(!prompts.refused("speech"));
    assert_eq!(
        prompts.get("speech").map(|prompt| prompt.status.as_str()),
        Some("granted")
    );
}

#[test]
fn the_test_line_says_what_the_last_voice_test_heard() {
    i18n::install();
    let test = |state: TestState, heard: Option<&str>| VoiceTest {
        state,
        at: None,
        heard: heard.map(str::to_string),
    };
    let line = |state, heard| test_line(Some(&test(state, heard)));
    assert_eq!(test_line(None), None);
    assert_eq!(line(TestState::Waiting, None), Some(TestLine::Waiting));
    assert_eq!(line(TestState::Sent, None), Some(TestLine::Waiting));
    assert_eq!(
        line(TestState::Heard, Some("hello")),
        Some(TestLine::Heard("hello".into()))
    );
    assert_eq!(line(TestState::Heard, None), Some(TestLine::Nothing));
    assert_eq!(line(TestState::Expired, None), Some(TestLine::Nothing));
    assert_eq!(line(TestState::NoDeck, None), Some(TestLine::NoDeck));
    // A question's state, or a word this window does not know, says nothing.
    assert_eq!(line(TestState::Answered, None), None);
    assert_eq!(line(TestState::Unknown("echo".into()), None), None);
    // In words.
    assert_eq!(
        TestLine::Waiting.words(EN),
        "Waiting for you to hold PTT on the deck\u{2026}"
    );
    assert_eq!(
        TestLine::Heard("hello".into()).words(EN),
        "The last test heard \u{201c}hello\u{201d}."
    );
    assert_eq!(
        TestLine::Heard("hello".into()).words(ZH),
        "上次测试听到的是“hello”。"
    );
    assert_eq!(TestLine::Nothing.words(EN), "The last test heard nothing.");
    assert_eq!(TestLine::NoDeck.words(EN), "Link the deck first.");
    // The service's own reply, as its fixture has it.
    let status = TestStatus::parse(
        &serde_json::from_str(include_str!(
            "../../deck-gui-core/tests/fixtures/daemon/care_test_status.json"
        ))
        .expect("the service's own fixture"),
    );
    assert_eq!(
        test_line(status.voice.as_ref()),
        Some(TestLine::Heard("hello".into()))
    );
}

#[test]
fn the_cloud_row_shows_the_keys_hint_and_never_a_key() {
    assert_eq!(key_tail("\u{2026}mnop (35 chars)"), "\u{2026}mnop");
    assert_eq!(key_tail("8 chars"), "8 chars");
    assert_eq!(key_tail(""), "");
    // config.get publishes a hint and never the key; no hint is no key.
    let config = TtsConfig::parse(&json!({"voice": {
        "tts.engine": "cloud", "tts.provider": "openai",
        "tts.api_key": "\u{2026}mnop (35 chars)"
    }}));
    assert!(config.has_key());
    assert_eq!(key_tail(&config.key_hint), "\u{2026}mnop");
    assert!(!TtsConfig::parse(&json!({"voice": {"tts.api_key": ""}})).has_key());
    // With a key, the tail stands at the end of the row's first sub-line,
    // the providers', and the second keeps the column's width: every
    // language's sub-line is the providers, one break, then the rest.
    let about = i18n::voice::STRINGS
        .iter()
        .find(|row| row.0 == "OpenAI or ElevenLabs.\nThe reply text is sent to them.")
        .expect("the Cloud voice row's sub-line");
    for cell in [about.0, about.1, about.2, about.3] {
        assert_eq!(cell.matches('\n').count(), 1, "{cell:?}");
        assert!(cell.starts_with("OpenAI"), "{cell:?}");
    }
}

#[test]
fn save_key_writes_the_provider_the_key_and_the_cloud_voice_in_one_patch() {
    let openai = TtsConfig {
        model: "gpt-4o-mini-tts".into(),
        ..tts("kokoro", "openai", "")
    };
    // Saving a key on Cloud voice's row is choosing it.
    assert_eq!(
        save_key_action("openai", "sk-test", Some(&openai)).request(),
        (
            "config.set",
            json!({"patch": {
                "voice.tts.provider": "openai",
                "voice.tts.api_key": "sk-test",
                "voice.tts.engine": "cloud"
            }})
        )
    );
    // Another provider takes its own default model: the other's would be
    // refused.
    assert_eq!(
        save_key_action("elevenlabs", "el-test", Some(&openai)),
        UiAction::ConfigPatch {
            entries: vec![
                (
                    "voice.tts.provider".into(),
                    ConfigValue::Text("elevenlabs".into())
                ),
                (
                    "voice.tts.api_key".into(),
                    ConfigValue::Text("el-test".into())
                ),
                ("voice.tts.engine".into(), ConfigValue::Text("cloud".into())),
                ("voice.tts.model".into(), ConfigValue::Null),
            ]
        }
    );
    // No model set, or no settings read: nothing more to write.
    let entries = |action: UiAction| match action {
        UiAction::ConfigPatch { entries } => entries.len(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        entries(save_key_action(
            "elevenlabs",
            "k",
            Some(&tts("cloud", "openai", ""))
        )),
        3
    );
    assert_eq!(entries(save_key_action("elevenlabs", "k", None)), 3);
    // The dialog opens on the provider set, when this window knows it.
    assert_eq!(
        dialog_provider(Some(&tts("cloud", "ElevenLabs", ""))),
        "elevenlabs"
    );
    assert_eq!(dialog_provider(Some(&tts("cloud", "acme", ""))), "openai");
    assert_eq!(dialog_provider(None), "openai");
    assert_eq!(provider_name("openai"), "OpenAI");
    assert_eq!(provider_name("ELEVENLABS"), "ElevenLabs");
    assert_eq!(provider_name("acme"), "acme");
}

#[test]
fn a_service_without_a_method_is_told_apart_from_a_call_that_failed() {
    let rpc = |code: i32, message: &str| ClientError::Rpc {
        code,
        message: message.into(),
        data: None,
    };
    assert_eq!(refusal(&rpc(-32601, "method not found")), Refusal::Missing);
    assert_eq!(refusal(&ClientError::Disconnected), Refusal::Away);
    match refusal(&rpc(-32013, "degraded: voice_unavailable")) {
        Refusal::Failed(why) => assert!(why.contains("voice_unavailable"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_chime_claims_a_queue_and_never_a_sound_anybody_heard() {
    i18n::install();
    let report = |played: serde_json::Value| {
        TestReport::parse(&json!({"what": "speaker", "played": played, "chime": "ready"}))
    };
    // Queued: the deck plays it, and the page says nothing more.
    assert_eq!(chime_complaint(&report(json!(true)), EN), None);
    assert_eq!(
        chime_complaint(&report(json!(false)), EN).as_deref(),
        Some("The audio thread refused the chime")
    );
    assert!(chime_complaint(&report(json!(null)), EN).is_some());
    assert_eq!(
        UiAction::VoiceTest {
            what: "speaker".into()
        }
        .request()
        .1["what"],
        "speaker"
    );
}

#[test]
fn the_harness_scenes_are_the_services_replies_parsed_as_the_page_reads_them() {
    // The boards: Automatic; Whisper and Kokoro chosen and here; Conol on;
    // no key; the last test heard "hello".
    let board = staged(Scene::Voice).expect("the voice board");
    assert_eq!(speech_choice(&board.stt_config), SpeechChoice::Automatic);
    assert_eq!(recogniser(&board.stt_config), Some(Recogniser::Whisper));
    assert_eq!(synthesiser(&board.tts), Some(Synthesiser::Kokoro));
    assert_eq!(stt_slot(&board.whisper), Slot::Installed);
    assert_eq!(tts_slot(&board.kokoro), Slot::Installed);
    assert!(!board.tts.has_key() && board.stt_config.companion && !board.add_key);
    assert_eq!(
        test_line(board.test.voice.as_ref()),
        Some(TestLine::Heard("hello".into()))
    );
    let routes = signal_path(
        &board.stt_config,
        Some(&board.whisper),
        &board.tts,
        Some(&board.kokoro),
    );
    assert_eq!(
        routes[0].1,
        [
            Stop::Microphone,
            Stop::OnMac("Whisper"),
            Stop::Agent,
            Stop::OnMac("Kokoro"),
            Stop::Speaker
        ]
    );
    assert_eq!(routes.len(), 2);
    // Downloading: Apple's recogniser and voice stand in meanwhile.
    let downloading = staged(Scene::VoiceDownloading).expect("the download scene");
    assert_eq!(
        stt_slot(&downloading.whisper),
        Slot::Downloading { percent: Some(42) }
    );
    assert_eq!(synthesiser(&downloading.tts), Some(Synthesiser::Apple));
    assert_eq!(
        tts_slot(&downloading.kokoro),
        Slot::Download { bytes: 364_816_464 }
    );
    assert_eq!(
        speech_choice(&downloading.stt_config),
        SpeechChoice::Locale("zh-CN".into())
    );
    assert_eq!(
        test_line(downloading.test.voice.as_ref()),
        Some(TestLine::Nothing)
    );
    let routes = signal_path(
        &downloading.stt_config,
        Some(&downloading.whisper),
        &downloading.tts,
        Some(&downloading.kokoro),
    );
    assert_eq!(routes[0].1[1], Stop::OnMac("Apple"));
    assert_eq!(routes[0].1[3], Stop::OnMac("Apple"));
    // The key: ElevenLabs speaks, Apple's recogniser hears, Conol is off, and
    // the dialog is up.
    let key = staged(Scene::VoiceKey).expect("the key scene");
    assert!(key.add_key);
    assert_eq!(synthesiser(&key.tts), Some(Synthesiser::Cloud));
    assert_eq!(key_tail(&key.tts.key_hint), "\u{2026}mnop");
    assert_eq!(dialog_provider(Some(&key.tts)), "elevenlabs");
    assert_eq!(recogniser(&key.stt_config), Some(Recogniser::Apple));
    assert_eq!(stt_slot(&key.whisper), Slot::Installed);
    assert!(matches!(
        tts_slot(&key.kokoro),
        Slot::Failed { load: false, .. }
    ));
    assert_eq!(key.test, TestStatus::default());
    let routes = signal_path(
        &key.stt_config,
        Some(&key.whisper),
        &key.tts,
        Some(&key.kokoro),
    );
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].1[3], Stop::Cloud("ElevenLabs".into()));
    // First run's voice step draws the engine cards over a fresh Mac's
    // replies: Apple's recogniser and voice, nothing fetched, Conol on.
    let fresh = staged(Scene::FirstVoice).expect("first run's voice step");
    assert_eq!(recogniser(&fresh.stt_config), Some(Recogniser::Apple));
    assert_eq!(synthesiser(&fresh.tts), Some(Synthesiser::Apple));
    assert_eq!(
        stt_slot(&fresh.whisper),
        Slot::Download {
            bytes: WHISPER_BYTES
        }
    );
    assert_eq!(
        tts_slot(&fresh.kokoro),
        Slot::Download { bytes: 364_816_464 }
    );
    assert!(fresh.stt_config.companion && !fresh.add_key);
    // Every other scene is another page's, live ones included.
    for scene in [
        Scene::Main,
        Scene::Integrations,
        Scene::LiveVoice,
        Scene::FirstIntegrations,
    ] {
        assert!(staged(scene).is_none(), "{scene:?}");
    }
}

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::voice::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 67 rows, copied as written.
    assert_eq!(i18n::voice::STRINGS.len(), 67);
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = concat!(
        include_str!("../src/voice.rs"),
        include_str!("../src/voice_actions.rs")
    );
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate.
    assert_eq!(i18n::literals(source).len(), 120);
}

/// A fresh test service's own replies (`deck-gui-core`'s recorded fixtures,
/// `fixtures/daemon/recorded/`): the service's voice defaults, neither local
/// model downloaded, `--no-device` (so no audio engine), and no voice test.
const RECORDED_CONFIG: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/config.get.json");
const RECORDED_STT: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/stt.status.json");
const RECORDED_TTS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/tts.status.json");
const RECORDED_ENGINES: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/voice.engines.json");
const RECORDED_TEST_AFTER: &str = include_str!(
    "../../deck-gui-core/tests/fixtures/daemon/recorded/care.test.status.after_question.json"
);
const RECORDED_HELLO: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/session.hello.json");
const RECORDED_PERM: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/perm.status.json");

fn recorded(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("a recorded reply is JSON")
}

#[test]
fn a_fresh_test_services_own_replies_draw_apple_on_both_cards_and_offer_both_downloads() {
    i18n::install();
    let config = recorded(RECORDED_CONFIG);
    let stt = SttConfig::parse(&config);
    let tts = TtsConfig::parse(&config);
    let stt_status = SttStatus::parse(&recorded(RECORDED_STT));
    let tts_status = TtsStatus::parse(&recorded(RECORDED_TTS));
    // Apple hears and speaks: the service's default `cloud` has no key.
    assert_eq!(recogniser(&stt), Some(Recogniser::Apple));
    assert_eq!(synthesiser(&tts), Some(Synthesiser::Apple));
    assert_eq!(
        stt_slot(&stt_status),
        Slot::Download {
            bytes: WHISPER_BYTES
        }
    );
    assert_eq!(tts_slot(&tts_status), Slot::Download { bytes: 364_816_464 });
    // Conol hears the presses meant for its chats: the service's default.
    assert!(stt.companion);
    assert_eq!(
        signal_path(&stt, Some(&stt_status), &tts, Some(&tts_status)),
        vec![
            (
                Route::Agents,
                vec![
                    Stop::Microphone,
                    Stop::OnMac("Apple"),
                    Stop::Agent,
                    Stop::OnMac("Apple"),
                    Stop::Speaker
                ]
            ),
            (
                Route::Conol,
                vec![Stop::Microphone, Stop::ConolCloud, Stop::Chat]
            ),
        ]
    );
    // `voice.engines` is refused with no audio engine (`-32013`), so the
    // page has no engines: the popup offers Automatic and the locale the
    // settings name.
    let engines = recorded(RECORDED_ENGINES);
    assert_eq!(engines["error"]["code"], -32_013);
    assert_eq!(speech_choice(&stt), SpeechChoice::Automatic);
    assert_eq!(speech_choice(&stt).word(EN), "Automatic");
    assert_eq!(speech_locales(None, &stt), ["en-US"]);
    assert_eq!(locale_name("en-US", EN), Some("English (United States)"));
    // A test question is not a voice test: no line.
    let tests = TestStatus::parse(&recorded(RECORDED_TEST_AFTER));
    assert_eq!(test_line(tests.voice.as_ref()), None);
    // The permissions, from the mirror the hello made: none read yet.
    let snapshot: Snapshot = serde_json::from_value(recorded(RECORDED_HELLO)["snapshot"].clone())
        .expect("the hello's snapshot");
    let permissions = snapshot.voice.permissions;
    assert_eq!(
        bluetooth_grant(permissions.bluetooth, EN),
        Grant {
            word: "Not checked",
            tone: Tone::Gray,
            offer: None
        }
    );
    // Speech Recognition, which that service could not read either (its
    // `perm.status` says `unknown`): not checked, and no Ask… that could not
    // prompt.
    let apple = recogniser(&stt) == Some(Recogniser::Apple);
    let reported = speech_reported(&recorded(RECORDED_PERM));
    assert_eq!(
        speech_grant(
            speech_state(permissions.speech, reported),
            apple,
            false,
            EN
        ),
        Grant {
            word: "Not checked",
            tone: Tone::Gray,
            offer: None
        }
    );
}
