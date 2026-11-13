//! §15 week 4, rows 3 and 9 (HOST-CONTRACT-W4 §3 B5): the four voice
//! methods that change something answer for real, and the §4.9 trust guard
//! still refuses a `voice.listen{ctx:"answer"}` on a permission card
//! **after** they moved onto the reducer path.
//!
//! That second half is the one that could have gone wrong silently. Before
//! B5, `voice.listen` was core-answered and `Core::voice_call` reached for
//! `Guard::check_listen` by hand. Making it a reducer method routes it
//! through `Guard::check` → `Guard::guarded` instead — **a different entry
//! point**, which inherits nothing. Had nobody added the arm, the method
//! would simply have stopped being guarded: no error, no warning, and an
//! untrusted token able to dictate an allow onto a permission card.
//!
//! So these rows assert the refusal through `Core::api_call` — the path a
//! door really takes — and assert that **no capture was opened**, which is
//! what §4.9 promises ("before capture starts") and what a check that ran
//! too late would not give.

mod core_support;

use core_support::{
    agent_of, build, claude, hold_register, permission_card, poll_register, uid, Parts,
};
use deck_model::{
    AgentId, ApiError, AudioCmd, CardId, CardSet, Deliver, EventKind, Principal, Request,
    SessionId, VoiceInput, VoiceListen, VoiceSay,
};
use deckd::audio::AudioHandle;
use deckd::core::Core;
use std::time::Instant;

/// A detached core with a real (but unattended) audio handle, so a test can
/// see every `AudioCmd` the core sent — and prove that one was **not**
/// sent.
struct Harness {
    core: Core,
    parts: Parts,
    cmds: crossbeam_channel::Receiver<AudioCmd>,
    /// Kept alive so the core's event receiver stays open.
    _events: crossbeam_channel::Sender<deckd::audio::FromAudio>,
}

impl Harness {
    fn new(name: &str) -> Harness {
        let (mut core, parts) = build(name, 512);
        let (audio, cmds, events) = AudioHandle::detached();
        core.set_audio(audio);
        Harness {
            core,
            parts,
            cmds,
            _events: events,
        }
    }

    /// Every `AudioCmd` sent since the last call.
    fn audio_cmds(&self) -> Vec<AudioCmd> {
        self.cmds.try_iter().collect()
    }

    /// An agent owned by the Claude adapter, because only an adapter may
    /// raise a card with `origin.kind:"permission"` (§4.6) and that is the
    /// only kind the §4.9 guard is about.
    fn register_agent(&mut self, name: &str, now: Instant) -> AgentId {
        let result = self.core.api_call(
            claude(),
            SessionId(1),
            true,
            Request::AgentRegister(hold_register(name)),
            now,
        );
        agent_of(&result)
    }

    /// An agent whose orders go to its **owner** (`caps.order:"poll"`), so
    /// a transcript aimed at it is never typed into a terminal.
    fn register_owner_agent(&mut self, name: &str, now: Instant) -> AgentId {
        let result = self.core.api_call(
            uid(),
            SessionId(7),
            true,
            Request::AgentRegister(poll_register(name)),
            now,
        );
        agent_of(&result)
    }

    fn push_permission_card(&mut self, agent: &AgentId, now: Instant) {
        // The §4.13 permission card, delivered by `poll` rather than
        // `hold`: `agent.card.set` refuses `hold` (that is `set_card_held`'s
        // alone), and the guard keys on `origin.kind`, not on delivery.
        let cmd = CardSet {
            deliver: Deliver::Poll,
            ..permission_card(agent)
        };
        self.core
            .api_call(claude(), SessionId(1), true, Request::CardSet(cmd), now)
            .expect("the permission card");
    }

    fn cleanup(self) {
        let dir = self.parts.dir.clone();
        drop(self);
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn listen(agent: &AgentId, ctx: &str, card: Option<CardId>) -> Request {
    Request::VoiceListen(VoiceListen {
        agent: agent.clone(),
        ctx: ctx.into(),
        card,
        max_s: 60,
        input: VoiceInput::Deck,
    })
}

fn say(text: &str, lane: &str) -> Request {
    Request::VoiceSay(VoiceSay {
        text: text.into(),
        agent: None,
        lane: lane.into(),
        voice: None,
        interrupt: false,
        ttl_s: None,
    })
}

/// The phases `voice.utterance` reported, oldest first.
fn phases(core: &mut Core) -> Vec<String> {
    core.publish();
    core.bus
        .replay(0, 500, None)
        .0
        .iter()
        .filter(|e| e.kind == EventKind::VoiceUtterance)
        .filter_map(|e| e.data["phase"].as_str().map(String::from))
        .collect()
}

#[test]
fn listen_for_an_answer_on_a_permission_card_is_refused_before_the_mic_opens() {
    let mut h = Harness::new("voice-api-guard");
    let t0 = Instant::now();
    let agent = h.register_agent("cohdl-api", t0);
    h.push_permission_card(&agent, t0);
    let _ = h.audio_cmds();
    let token = Principal::Token("mcp".into());

    // An untrusted external principal: refused, and the GUI is asked to
    // confirm (§4.2).
    let err = h
        .core
        .api_call(
            token.clone(),
            SessionId(9),
            false,
            listen(&agent, "answer", None),
            t0,
        )
        .expect_err("a permission card needs trust");
    assert_eq!(err.code, ApiError::FORBIDDEN);
    assert_eq!(err.data["need"], "trusted");
    assert_eq!(err.data["how"], "gui_confirm");
    h.core.publish();
    assert!(
        h.core
            .bus
            .replay(0, 500, None)
            .0
            .iter()
            .any(|e| e.kind == EventKind::OpConfirmRequested),
        "op.confirm_requested fires so the GUI can show its sheet"
    );
    // …and the microphone was never opened. This is the half a guard that
    // ran too late would not give.
    assert!(
        !h.audio_cmds()
            .iter()
            .any(|c| matches!(c, AudioCmd::Listen { .. })),
        "no capture was opened"
    );
    assert!(h.core.store.snapshot().voice.ptt.is_none());

    // The same call from a trusted caller goes through, with **its**
    // principal and session on the session (B2).
    h.core
        .api_call(
            token.clone(),
            SessionId(9),
            true,
            listen(&agent, "answer", None),
            t0,
        )
        .expect("a trusted caller may listen");
    let ptt = h
        .core
        .store
        .snapshot()
        .voice
        .ptt
        .clone()
        .expect("listening");
    assert_eq!(ptt.principal, token);
    assert_eq!(ptt.session, Some(SessionId(9)));
    assert!(h
        .audio_cmds()
        .iter()
        .any(|c| matches!(c, AudioCmd::Listen { .. })));

    // `ctx:"talk"` is not an *answer* — but this agent's orders go by
    // `inject`, and `Store::dispatch_order` hands the transcript straight
    // to `op_order` without passing back through `Request::OpOrder`. So the
    // capture is where guard 1's trust half has to refuse, or an untrusted
    // `voice` caller can aim the operator's dictation at a terminal of its
    // choosing (§5.5 guard 1: "a transcript from a `voice.listen` the
    // **trusted GUI** started").
    let err = h
        .core
        .api_call(
            token.clone(),
            SessionId(9),
            false,
            listen(&agent, "talk", None),
            t0,
        )
        .expect_err("an untrusted talk capture aimed at an inject agent");
    assert_eq!(err.code, ApiError::FORBIDDEN);
    assert_eq!(err.data["need"], "trusted");
    assert_eq!(err.data["how"], "gui_confirm");

    // …and the capture the trusted caller opened a moment ago is still the
    // one that is open: the refusal took nothing.
    let still = h
        .core
        .store
        .snapshot()
        .voice
        .ptt
        .clone()
        .expect("listening");
    assert_eq!(still.principal, token);

    // An agent whose orders go to its owner is a different matter: nothing
    // is typed anywhere, so `talk` stays unguarded.
    let owned = h.register_owner_agent("owner-routed", t0);
    h.core
        .api_call(token, SessionId(9), false, listen(&owned, "talk", None), t0)
        .expect("an owner-routed order is not an injection");
    assert!(h.core.store.snapshot().voice.ptt.is_some());
    h.cleanup();
}

/// The re-wire, stated as the property it has to keep: a `voice.listen` is
/// refused by the **reducer** guard now, not by the entry point it used to
/// go through, and both doors give the same answer because there is one
/// check behind them and not two copies of the rule.
#[test]
fn the_reducer_guard_is_what_refuses_a_voice_listen_now() {
    use deckd::core::guard::{Guard, Refusal};
    let mut h = Harness::new("voice-api-rewire");
    let t0 = Instant::now();
    let agent = h.register_agent("cohdl-api", t0);
    h.push_permission_card(&agent, t0);
    let snapshot = h.core.store.snapshot().clone();
    let guard = Guard::new();
    let token = Principal::Token("mcp".into());
    let req = listen(&agent, "answer", None);

    let refused = guard
        .guarded(&token, false, &req, &snapshot)
        .expect_err("guarded refuses a voice.listen on a permission card");
    assert!(matches!(refused, Refusal::Untrusted { card: Some(_), .. }));
    assert!(guard
        .check_listen(&token, false, &agent, "answer", None, &snapshot)
        .is_err());
    // Trusted passes through both.
    assert!(guard.guarded(&token, true, &req, &snapshot).is_ok());
    // A non-answer context on an **owner-routed** agent is not guarded:
    // its transcript becomes an `op.order` its owner receives and nothing
    // reaches a terminal.
    let owned = h.register_owner_agent("owner-routed", t0);
    let snapshot = h.core.store.snapshot().clone();
    assert!(guard
        .guarded(&token, false, &listen(&owned, "talk", None), &snapshot)
        .is_ok());
    // On an `inject` agent it **is** guarded, for every `ctx`: the
    // transcript is dispatched by `Store::dispatch_order` straight into
    // `op_order`, so `Request::OpOrder`'s arm never sees it and this is the
    // only door left. `ctx:"answer"` on an agent with no card falls back to
    // `dispatch_order` too, so neither context is safe to let through.
    for ctx in ["talk", "answer"] {
        assert!(
            guard
                .guarded(&token, false, &listen(&agent, ctx, None), &snapshot)
                .is_err(),
            "ctx:{ctx:?} on an inject agent"
        );
    }
    // A card that is not the agent's current one is not this card — but the
    // agent is still an `inject` agent, so the refusal stands on that.
    assert!(guard
        .guarded(
            &token,
            false,
            &listen(&agent, "answer", Some(CardId(999))),
            &snapshot
        )
        .is_err());
    assert!(guard
        .guarded(
            &token,
            false,
            &listen(&owned, "answer", Some(CardId(999))),
            &snapshot
        )
        .is_ok());
    // The device and the adapters are never guarded (§4.2).
    assert!(guard
        .guarded(&Principal::Device, false, &req, &snapshot)
        .is_ok());
    h.cleanup();
}

#[test]
fn say_stop_cancel_and_listen_answer_for_real() {
    let mut h = Harness::new("voice-api-methods");
    let t0 = Instant::now();
    let agent = h.register_agent("bot", t0);
    let _ = h.audio_cmds();
    let me = uid();

    // `voice.say` allocates an utterance and queues it.
    let said = h
        .core
        .api_call(
            me.clone(),
            SessionId(2),
            true,
            Request::VoiceSay(VoiceSay {
                text: "  tests   pass  ".into(),
                agent: Some(agent.clone()),
                lane: "ready".into(),
                voice: None,
                interrupt: false,
                ttl_s: Some(30),
            }),
            t0,
        )
        .expect("voice.say");
    let utterance = said["utterance"]
        .as_str()
        .expect("an utterance id")
        .to_string();
    assert!(utterance.starts_with("ut_"));
    let queued = h
        .audio_cmds()
        .into_iter()
        .find_map(|c| match c {
            AudioCmd::Say {
                utterance: u,
                text,
                lane,
                ttl_s,
                ..
            } => Some((u, text, lane, ttl_s)),
            _ => None,
        })
        .expect("a Say reached the engine");
    assert_eq!(queued.0, utterance);
    assert_eq!(queued.1, "tests pass", "the sanitiser ran");
    assert_eq!(queued.2, "ready");
    assert_eq!(queued.3, Some(30), "§4.9's ttl_s is carried, not dropped");
    assert_eq!(
        h.core
            .store
            .snapshot()
            .voice
            .speaking
            .as_ref()
            .map(|s| s.utterance.clone()),
        Some(utterance)
    );
    assert_eq!(phases(&mut h.core), vec!["queued"]);

    // A bad lane and empty text are `-32009`, not a queued nothing.
    for (field, req) in [("lane", say("hi", "nope")), ("text", say("   ", "chatter"))] {
        let err = h
            .core
            .api_call(me.clone(), SessionId(2), true, req, t0)
            .expect_err(field);
        assert_eq!(err.code, ApiError::INVALID, "{field}");
        assert_eq!(err.data["field"], field);
    }

    // `voice.stop{all}` cuts it, and says so.
    h.core
        .api_call(
            me.clone(),
            SessionId(2),
            true,
            Request::VoiceStop {
                utterance: None,
                lane: None,
                all: true,
            },
            t0,
        )
        .expect("voice.stop");
    assert!(h.core.store.snapshot().voice.speaking.is_none());
    assert!(h.audio_cmds().iter().any(|c| matches!(
        c,
        AudioCmd::Stop {
            all: true,
            utterance: None,
            lane: None
        }
    )));
    assert_eq!(phases(&mut h.core), vec!["queued", "cut"]);
    // A stop that names nothing at all is a mistake, not "stop everything".
    let err = h
        .core
        .api_call(
            me.clone(),
            SessionId(2),
            true,
            Request::VoiceStop {
                utterance: None,
                lane: None,
                all: false,
            },
            t0,
        )
        .expect_err("nothing named");
    assert_eq!(err.code, ApiError::INVALID);

    // `voice.cancel` aborts a live session — B5's "could not reach
    // `abort_ptt`", which left a client holding the microphone with no way
    // to let go.
    h.core
        .api_call(
            me.clone(),
            SessionId(2),
            true,
            listen(&agent, "talk", None),
            t0,
        )
        .expect("voice.listen");
    assert!(h.core.store.snapshot().voice.ptt.is_some());
    let _ = h.audio_cmds();
    h.core
        .api_call(me.clone(), SessionId(2), true, Request::VoiceCancel {}, t0)
        .expect("voice.cancel");
    assert!(h.core.store.snapshot().voice.ptt.is_none());
    assert!(h
        .audio_cmds()
        .iter()
        .any(|c| matches!(c, AudioCmd::Finish { abort: true })));

    // §4.9 admits exactly `talk` and `answer`: a caller may not synthesise
    // the device's own contexts.
    for bad in ["name", "codex", "shout"] {
        let err = h
            .core
            .api_call(
                me.clone(),
                SessionId(2),
                true,
                listen(&agent, bad, None),
                t0,
            )
            .expect_err(bad);
        assert_eq!(err.code, ApiError::INVALID, "{bad}");
        assert_eq!(err.data["field"], "ctx", "{bad}");
    }
    // An unknown agent is `-32003`, and nothing was opened for it.
    let _ = h.audio_cmds();
    let err = h
        .core
        .api_call(
            me,
            SessionId(2),
            true,
            listen(&AgentId("ag_nope".into()), "talk", None),
            t0,
        )
        .expect_err("unknown agent");
    assert_eq!(err.code, ApiError::NOT_FOUND);
    assert!(!h
        .audio_cmds()
        .iter()
        .any(|c| matches!(c, AudioCmd::Listen { .. })));
    h.cleanup();
}

/// `voice.say`'s 4/s bucket (§11) survived the move to the reducer path:
/// it is taken in `RateLimits::check` now, not by a call the core-answered
/// version made by name.
#[test]
fn the_say_rate_limit_still_applies_on_the_reducer_path() {
    let mut h = Harness::new("voice-api-rate");
    let t0 = Instant::now();
    let me = uid();
    for n in 0..4 {
        h.core
            .api_call(me.clone(), SessionId(2), true, say("hello", "chatter"), t0)
            .unwrap_or_else(|e| panic!("say {n}: {e}"));
    }
    let err = h
        .core
        .api_call(me, SessionId(2), true, say("hello", "chatter"), t0)
        .expect_err("the fifth in one second");
    assert_eq!(err.code, ApiError::RATE_LIMITED);
    // Internal principals are never metered (§11).
    for _ in 0..10 {
        h.core
            .api_call(
                Principal::Device,
                SessionId(2),
                true,
                say("hello", "chatter"),
                t0,
            )
            .expect("the device is not metered");
    }
    h.cleanup();
}

/// The table is `/v1/schema.methods`, and B5 changed four of its rows.
#[test]
fn the_four_voice_methods_that_change_something_are_reducer_rows() {
    use deckd::api::dispatch::method;
    for name in ["voice.say", "voice.stop", "voice.listen", "voice.cancel"] {
        let spec = method(name).unwrap_or_else(|| panic!("{name} missing"));
        assert!(spec.reducer, "{name} must be a reducer method");
        assert_eq!(spec.scope, Some(deck_model::Scope::Voice), "{name}");
    }
    for name in ["voice.engines", "voice.test"] {
        let spec = method(name).unwrap_or_else(|| panic!("{name} missing"));
        assert!(!spec.reducer, "{name} only reads");
    }
    assert_eq!(
        method("voice.get").expect("voice.get").scope,
        Some(deck_model::Scope::Observe),
        "reading the state is an observe method, not a voice one"
    );
    // Every one of them really deserializes as a `Request`, or a door would
    // answer `-32602` for a well-formed call.
    for (name, params) in [
        (
            "voice.say",
            serde_json::json!({"text": "hi", "lane": "chatter"}),
        ),
        ("voice.stop", serde_json::json!({"all": true})),
        ("voice.listen", serde_json::json!({"agent": "ag_1"})),
        ("voice.cancel", serde_json::json!({})),
    ] {
        let req: Request =
            serde_json::from_value(serde_json::json!({"method": name, "params": params}))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            serde_json::to_value(&req).unwrap()["method"],
            name,
            "{name} round-trips"
        );
    }
    // §4.9's defaults: `ctx:"talk"`, `max_s:60`, `input:"deck"`.
    let req: Request = serde_json::from_value(serde_json::json!({
        "method": "voice.listen", "params": {"agent": "ag_1"},
    }))
    .expect("the minimal voice.listen");
    let Request::VoiceListen(l) = req else {
        panic!("{req:?}")
    };
    assert_eq!(l.ctx, "talk");
    assert_eq!(l.max_s, 60);
    assert_eq!(l.input, VoiceInput::Deck);
    assert_eq!(l.card, None);
}

/// B9: §7.8's two permission methods exist, with the scopes this contract
/// set, and `perm.request` is **honest about not prompting**.
///
/// Neither was in the method table, so §7.8's whole flow had no wire
/// surface and `Permissions` was never written. The scopes are this
/// contract's (§4.11 names none): `voice` for the read, because the Voice
/// screen shows it and it discloses only the daemon's own grants, and
/// `admin` for the request, because it puts a system dialog on somebody's
/// screen.
///
/// The stub is deliberate and V9 is why. A launchd-spawned `deckd` is its
/// own responsible process with its own TCC row; the only reason a probe
/// with no Info.plist opens a microphone today is that responsibility is
/// attributed to the **terminal**. A green test here would prove the
/// terminal's grant and not ours, so this row asserts that nothing claims
/// otherwise.
#[test]
fn the_permission_methods_exist_and_the_request_says_it_did_not_prompt() {
    use deckd::api::dispatch::method;
    use deckd::core::dispatch::{PERM_LIMITS, PERM_REQUEST_LIMITS};
    assert_eq!(
        method("perm.status").expect("perm.status").scope,
        Some(deck_model::Scope::Voice)
    );
    assert_eq!(
        method("perm.request").expect("perm.request").scope,
        Some(deck_model::Scope::Admin)
    );
    assert!(!method("perm.status").expect("row").reducer);
    assert!(!method("perm.request").expect("row").reducer);

    let mut h = Harness::new("voice-api-perm");
    let t0 = Instant::now();
    let ctx = |scopes: &[deck_model::Scope]| core_support::ctx(SessionId(2), scopes);

    // The read: what the daemon knows, and what it does not.
    let status = h
        .core
        .call(
            ctx(&[deck_model::Scope::Voice]),
            "perm.status",
            serde_json::json!({}),
            t0,
        )
        .result
        .expect("perm.status");
    assert_eq!(status["microphone"], "unknown");
    assert_eq!(status["speech"], "unknown");
    assert_eq!(status["bluetooth"], "unknown");
    assert!(
        status.get("automation").is_none(),
        "no Apple Event is sent any more, so there is no Automation grant to report"
    );
    assert_eq!(status["accessibility"], "unknown");
    assert_eq!(status["limits"], serde_json::json!(PERM_LIMITS));
    assert!(!PERM_LIMITS.is_empty());

    // It follows the snapshot, which is what `VoiceIn::Permissions` writes.
    h.core.step(
        deck_model::Command::Voice(deck_model::VoiceIn::Permissions(deck_model::Permissions {
            microphone: deck_model::Permission::Granted,
            speech: deck_model::Permission::Denied,
            bluetooth: deck_model::Permission::Unknown,
        })),
        t0,
    );
    // The BLE link's own report of the Bluetooth grant.
    h.core.step(
        deck_model::Command::Voice(deck_model::VoiceIn::Bluetooth {
            permission: deck_model::Permission::Granted,
        }),
        t0,
    );
    let status = h
        .core
        .call(
            ctx(&[deck_model::Scope::Voice]),
            "perm.status",
            serde_json::json!({}),
            t0,
        )
        .result
        .expect("perm.status");
    assert_eq!(status["microphone"], "granted");
    assert_eq!(status["speech"], "denied");
    assert_eq!(status["bluetooth"], "granted");

    // The request: stubbed at the prompt, and it says so.
    for which in ["microphone", "speech"] {
        let answer = h
            .core
            .call(
                ctx(&[deck_model::Scope::Admin]),
                "perm.request",
                serde_json::json!({"which": which}),
                t0,
            )
            .result
            .expect("perm.request answers");
        assert_eq!(answer["which"], which);
        assert_eq!(answer["status"], "prompt_not_shown");
        assert_eq!(
            answer["limits"],
            serde_json::json!(PERM_REQUEST_LIMITS),
            "the reason is on the wire, not only in a comment"
        );
        // Nothing here may claim a grant, a prompt or a decision.
        for forbidden in ["granted", "denied", "prompted", "shown"] {
            assert_ne!(answer["status"], forbidden, "{which}");
        }
    }
    // A grant is never invented as a side effect of asking for one.
    assert_eq!(
        h.core.store.snapshot().voice.permissions.microphone,
        deck_model::Permission::Granted,
        "unchanged by perm.request"
    );
    let err = h
        .core
        .call(
            ctx(&[deck_model::Scope::Admin]),
            "perm.request",
            serde_json::json!({"which": "camera"}),
            t0,
        )
        .result
        .expect_err("only the two §7.8 grants");
    assert_eq!(err.code, ApiError::INVALID);
    assert_eq!(err.data["field"], "which");
    h.cleanup();
}

/// B14: `[voice]` is hot, and the reload really re-reads the two things
/// the daemon has to act on.
///
/// Before this there was no `voice.*` in `HOT_KEYS` and `set_config`
/// ignored `config.voice` entirely — so `config.reload` reported a changed
/// STT locale or device selection under `applied` and then changed
/// nothing until a restart, which is the one failure mode a reload is
/// supposed to remove.
#[test]
fn the_voice_table_is_hot_and_the_reload_re_reads_it() {
    use deckd::config::Config;
    use deckd::core::dispatch::is_hot;
    use std::sync::Arc;

    // Every `[voice]` key a reload can report is applicable live.
    for key in [
        "voice.device_uid",
        "voice.fallback_output",
        "voice.mic_warm",
        "voice.speak_over_sco",
        "voice.slow_sco_ms",
        "voice.commands",
        "voice.announce_questions",
        "voice.stt.locale",
        "voice.stt.engine",
        "voice.tts.announce_max_chars",
        "voice.voices.default_pool",
    ] {
        assert!(is_hot(key), "{key} must be hot");
    }
    assert!(!is_hot("api.port"), "the port still needs a restart");

    let mut h = Harness::new("voice-api-hot");
    let t0 = Instant::now();
    let agent = h.register_agent("bot", t0);
    let mut cfg = Config::default();
    cfg.voice.stt.locale = "de-DE".into();
    cfg.voice.device_uid = Some("C2-2D-81-AB-88-81".into());
    h.core.set_config(Arc::new(cfg.clone()));

    // The device selection is **pushed**: §7 selects by UID and never
    // touches the system default, so nothing on the audio side could
    // re-derive it.
    let pushed = h.audio_cmds();
    let devices = pushed
        .iter()
        .find(|c| matches!(c, AudioCmd::Devices { .. }))
        .cloned()
        .expect("the selection reached the audio side");
    assert_eq!(
        devices,
        AudioCmd::Devices {
            device_uid: Some("C2-2D-81-AB-88-81".into()),
            fallback_output: "none".into(),
        }
    );
    // V1/V2's timing policy is pushed too, and it is a **separate**
    // command from the selection: `mic_warm`, `speak_over_sco` and
    // `slow_sco_ms` are operator settings that nothing on the audio side
    // can re-derive, and a reload that only moved a threshold must not
    // re-select a device under a live stream. The defaults here are the
    // measured ones (1031 ms bring-up, 2307 ms A2DP resume, 2026-09-21),
    // not the design's guesses.
    let policy = pushed
        .iter()
        .find(|c| matches!(c, AudioCmd::Policy { .. }))
        .cloned()
        .expect("the timing policy reached the audio side");
    assert_eq!(
        policy,
        AudioCmd::Policy {
            mic_warm: "card_pending".into(),
            speak_over_sco: true,
            slow_sco_ms: 1500,
        }
    );
    // The locale is read where it is used, so it lands on the next press
    // and `voice.get` says so at once.
    assert_eq!(h.core.store.snapshot().voice.stt.locale, "de-DE");
    h.core
        .api_call(uid(), SessionId(2), true, listen(&agent, "talk", None), t0)
        .expect("voice.listen");
    let locale = h
        .audio_cmds()
        .into_iter()
        .find_map(|c| match c {
            AudioCmd::Listen { locale, .. } => Some(locale),
            _ => None,
        })
        .expect("a capture was opened");
    assert_eq!(locale, "de-DE");

    // A reload that changed something else does not re-select the device:
    // that would close the stream the operator is listening to.
    let mut next = cfg.clone();
    next.agents.grace_s = 61;
    h.core.set_config(Arc::new(next));
    assert!(
        !h.audio_cmds()
            .iter()
            .any(|c| matches!(c, AudioCmd::Devices { .. })),
        "the selection did not change"
    );
    // …and one that does change it, does.
    let mut next = cfg;
    next.voice.device_uid = Some("OTHER".into());
    h.core.set_config(Arc::new(next));
    assert!(h
        .audio_cmds()
        .iter()
        .any(|c| matches!(c, AudioCmd::Devices { device_uid: Some(u), .. } if u == "OTHER")));
    h.cleanup();
}
