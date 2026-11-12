//! The test agent: what the companion app's **Send a Test Question**, **Test
//! Voice** and **Speak a Sample** do (`care.test.question`,
//! `care.test.voice`, `care.test.say`), and how the last of each went
//! (`care.test.status`) — PLAN §3.2.
//!
//! The daemon puts the card on the deck itself, from an internal session of
//! its own (client `care`, principal `adapter:virtual`, opened on first
//! use), the way the demo puts its roster there (`crate::demo`), and reads
//! the deck's answer off the events every step produces
//! ([`TestAgent::observe`]):
//!
//! - **A test question**: the agent "Test" asks "Test question from your
//!   Mac. Press ENTER." with one choice, OK. It is `sent` until the deck
//!   answers it — `answered`, with how long that took — or a minute passes
//!   (`expired`).
//! - **A voice test**: the same agent shows "Hold PTT and say hello." with
//!   no choice. It is `waiting` until a press says something to it —
//!   `heard`, with the words (none, for a press the recogniser heard nothing
//!   in) — or a minute passes (`expired`).
//! - **A sample**: "This is OpenWalkieTalkie speaking." on the `chatter`
//!   lane. No card, and nothing to wait for.
//!
//! The words are the deck's own language ([`Language::of`]). With no deck
//! linked nothing is sent and the test is `no_deck` at once. One test is on
//! the deck at a time: a new one ends the one before (a second question
//! replaces the first; a voice test ends a question as `expired`, and the
//! other way round), and the agent leaves the deck as soon as its test is
//! over, whichever way it ended. The results are kept in memory for the life
//! of the service.
//!
//! The care session is the daemon's own and never a source
//! (`care::sources`), so neither "Pause integrations" nor a source's switch
//! mutes it: the test still reaches the deck while every source is paused,
//! which is when the owner most needs to know that the deck still answers.
use deck_model::{
    AdapterKind, AgentId, AgentRegister, AnswerCap, ApiError, Caps, CardId, CardKind, CardSet,
    Choice, Deliver, Event, EventKind, Kind, OrderCap, Origin, OriginKind, Principal, SessionId,
    Snapshot, Ts, VoiceSay,
};
use serde::Serialize;
use std::time::Instant;

/// The care session's client name, as `service.info` and the log show it.
pub const CLIENT: &str = "care";
/// The agent's name on the deck: a name, like the demo's agents', so it is
/// the same in every language.
pub const AGENT: &str = "Test";
/// How long a test waits for the deck. It is the card's own timeout, so the
/// store takes the card off the deck when it runs out (`card.expired`).
pub const ANSWER_WITHIN_S: u32 = 60;
/// The lane the sample is spoken on.
pub const SAMPLE_LANE: &str = "chatter";

/// The care session's principal: internal, and allowed the question origin,
/// so the card carries no `» ` prefix (as the demo's do).
#[must_use]
pub fn principal() -> Principal {
    Principal::Adapter(AdapterKind::Virtual)
}

/// The deck's four languages, in the order of its `language` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
}

/// What the service says on the deck, in one language: the four rows of
/// the string sheet `deckd.tsv`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Words {
    /// The test question.
    pub question: &'static str,
    /// Its one choice.
    pub ok: &'static str,
    /// The voice test's card.
    pub voice: &'static str,
    /// The spoken sample.
    pub sample: &'static str,
}

impl Language {
    /// The language the deck shows: the setting it shares with the app,
    /// or English on a deck whose firmware said at hello that it has no
    /// language setting (it shows nothing else).
    #[must_use]
    pub fn of(snapshot: &Snapshot) -> Language {
        let device = &snapshot.device;
        // `fw` is the hello's and lasts as long as the link: a deck that has
        // said hello on this link and named no language has none to show.
        if device.fw.is_some() && !device.language_supported {
            return Language::English;
        }
        match snapshot.settings.language {
            1 => Language::SimplifiedChinese,
            2 => Language::TraditionalChinese,
            3 => Language::Japanese,
            _ => Language::English,
        }
    }

    #[must_use]
    pub const fn words(self) -> Words {
        match self {
            Language::English => Words {
                question: "Test question from your Mac. Press ENTER.",
                ok: "OK",
                voice: "Hold PTT and say hello.",
                sample: "This is OpenWalkieTalkie speaking.",
            },
            Language::SimplifiedChinese => Words {
                question: "来自 Mac 的测试提问。请按 ENTER。",
                ok: "好",
                voice: "按住 PTT 并说声你好。",
                sample: "这里是 OpenWalkieTalkie。",
            },
            Language::TraditionalChinese => Words {
                question: "來自 Mac 的測試提問。請按 ENTER。",
                ok: "好",
                voice: "按住 PTT 並說聲你好。",
                sample: "這裡是 OpenWalkieTalkie。",
            },
            Language::Japanese => Words {
                question: "Mac からのテスト質問です。ENTER を押してください。",
                ok: "OK",
                voice: "PTT を押しながら「こんにちは」と話してください。",
                sample: "こちらは OpenWalkieTalkie です。",
            },
        }
    }
}

/// The two tests that put a card on the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    Question,
    Voice,
}

/// Where a test stands: the `state` of `care.test.status`, and the
/// `{state}` `care.test.question` and `care.test.voice` answer with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The question is on the deck.
    Sent,
    /// The voice card is on the deck.
    Waiting,
    /// The deck answered the question.
    Answered,
    /// A press said something to the voice card.
    Heard,
    /// A minute passed, or another test took its place, before the deck
    /// answered.
    Expired,
    /// No deck was linked, so nothing was sent.
    NoDeck,
}

/// The last test question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuestionTest {
    pub state: State,
    /// When it was sent, or found no deck to send to.
    pub at: Ts,
    /// How long the deck took to answer it; `null` until it has.
    pub after_ms: Option<u64>,
}

/// The last voice test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VoiceTest {
    pub state: State,
    /// When it was started, or found no deck to start on.
    pub at: Ts,
    /// What the deck heard: the words, `""` for a press the recogniser
    /// heard nothing in; `null` until a press.
    pub heard: Option<String>,
}

/// The test whose card is on the deck.
#[derive(Clone, Debug)]
struct Pending {
    test: Test,
    card: CardId,
    sent: Instant,
}

/// How a test on the deck ended.
enum Ending {
    /// The deck answered it, with these words when there were any.
    Answered(Option<String>),
    /// It is over unanswered: its time ran out, another test took its
    /// place, or the agent went.
    Gone,
}

/// What the core keeps about the test agent between calls.
#[derive(Debug, Default)]
pub struct TestAgent {
    /// The care session, once opened. It never closes.
    session: Option<SessionId>,
    /// The agent "Test", while it is registered.
    agent: Option<AgentId>,
    pending: Option<Pending>,
    question: Option<QuestionTest>,
    voice: Option<VoiceTest>,
}

impl TestAgent {
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    pub fn set_session(&mut self, session: SessionId) {
        self.session = Some(session);
    }

    #[must_use]
    pub fn agent(&self) -> Option<&AgentId> {
        self.agent.as_ref()
    }

    pub fn set_agent(&mut self, agent: AgentId) {
        self.agent = Some(agent);
    }

    /// `care.test.status`: `{question, voice}`, each `null` before its first
    /// test since the service started.
    #[must_use]
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"question": self.question, "voice": self.voice})
    }

    /// A new test is starting, so the one on the deck now, if any, is over
    /// without an answer: `expired`, until the new test's record takes its
    /// place when it is of the same kind. The agent stays for the new card.
    pub fn supersede(&mut self, now: Instant) {
        self.end(Ending::Gone, now);
    }

    /// The `test` found no deck to send to.
    pub fn no_deck(&mut self, test: Test, at: Ts) -> State {
        self.record(test, State::NoDeck, at);
        State::NoDeck
    }

    /// The `test`'s `card` is on its way to the deck.
    pub fn sent(&mut self, test: Test, card: CardId, now: Instant, at: Ts) -> State {
        let state = match test {
            Test::Question => State::Sent,
            Test::Voice => State::Waiting,
        };
        self.record(test, state, at);
        self.pending = Some(Pending {
            test,
            card,
            sent: now,
        });
        state
    }

    fn record(&mut self, test: Test, state: State, at: Ts) {
        match test {
            Test::Question => {
                self.question = Some(QuestionTest {
                    state,
                    at,
                    after_ms: None,
                });
            }
            Test::Voice => {
                self.voice = Some(VoiceTest {
                    state,
                    at,
                    heard: None,
                });
            }
        }
    }

    /// What one step's events mean for the test on the deck: its card
    /// answered (a choice, or words said to it), the words of a press
    /// addressed to the agent during a voice test, or the card gone
    /// unanswered (`card.expired`, a clear, the agent removed). Returns the
    /// agent to take off the deck when a test ended and its agent is still
    /// registered.
    pub fn observe(&mut self, events: &[Event], now: Instant) -> Option<AgentId> {
        let mut release = None;
        for event in events {
            let Some(agent) = self.agent.clone() else {
                break;
            };
            if crate::core::event_agent(event).as_ref() != Some(&agent) {
                continue;
            }
            if event.kind == EventKind::AgentRemoved {
                // However it went, there is nothing left to release, and a
                // test still on it is over.
                self.end(Ending::Gone, now);
                self.agent = None;
                release = None;
                continue;
            }
            let Some(pending) = &self.pending else {
                continue;
            };
            let on_card = card_of(event) == Some(pending.card);
            let ending = match event.kind {
                EventKind::CardAnswered if on_card => Ending::Answered(words_of(event)),
                // A press talking to the agent rather than answering its
                // card still said something to it.
                EventKind::TranscriptFinal if pending.test == Test::Voice => {
                    Ending::Answered(words_of(event))
                }
                EventKind::CardExpired | EventKind::CardCleared if on_card => Ending::Gone,
                _ => continue,
            };
            self.end(ending, now);
            release = Some(agent);
        }
        release
    }

    /// The test on the deck is over, the way `ending` says.
    fn end(&mut self, ending: Ending, now: Instant) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending.test {
            Test::Question => {
                let Some(test) = &mut self.question else {
                    return;
                };
                match ending {
                    Ending::Answered(_) => {
                        let took = now.saturating_duration_since(pending.sent);
                        test.state = State::Answered;
                        test.after_ms = Some(u64::try_from(took.as_millis()).unwrap_or(u64::MAX));
                    }
                    Ending::Gone => test.state = State::Expired,
                }
            }
            Test::Voice => {
                let Some(test) = &mut self.voice else {
                    return;
                };
                match ending {
                    Ending::Answered(words) => {
                        test.state = State::Heard;
                        test.heard = Some(words.unwrap_or_default());
                    }
                    Ending::Gone => test.state = State::Expired,
                }
            }
        }
    }
}

/// `data.card` of a card event.
fn card_of(event: &Event) -> Option<CardId> {
    serde_json::from_value(event.data.get("card")?.clone()).ok()
}

/// The words of an answer or a transcript: what was said, else the chosen
/// label.
fn words_of(event: &Event) -> Option<String> {
    ["text", "label"]
        .into_iter()
        .find_map(|key| event.data.get(key)?.as_str().map(str::to_string))
}

/// The four methods take nothing: `{}`, or no params at all.
pub fn no_params(params: &serde_json::Value) -> Result<(), ApiError> {
    match params {
        serde_json::Value::Null => Ok(()),
        serde_json::Value::Object(map) if map.is_empty() => Ok(()),
        _ => Err(ApiError::invalid_params("care.test.* takes no params")),
    }
}

/// The agent "Test", kind other, with no key (nothing of it is remembered
/// in `state.json`). Its answers and orders are polled, as the demo's are:
/// an internal session with nobody parked in `agent.wait` takes an answer
/// as delivered the moment it arrives, so the store clears the card at
/// once, and anything else left for the agent goes with it.
#[must_use]
pub fn register() -> AgentRegister {
    AgentRegister {
        kind: Kind::Other,
        name: AGENT.into(),
        key: None,
        cwd: None,
        pid: None,
        pid_kind: None,
        terminal: None,
        lease_ttl_s: 0,
        caps: Caps {
            answer: AnswerCap::Poll,
            order: OrderCap::Poll,
            focus: false,
            transcribe: Default::default(),
        },
        slot_hint: None,
        voice: None,
        meta: None,
    }
}

/// `test`'s card for `agent`, in `language`: a question with one choice,
/// or the voice test's card with none, either gone after
/// [`ANSWER_WITHIN_S`].
#[must_use]
pub fn card(test: Test, agent: &AgentId, language: Language) -> CardSet {
    let words = language.words();
    let (text, choices, rec, free_text) = match test {
        Test::Question => (words.question, vec![words.ok], Some(0), false),
        Test::Voice => (words.voice, Vec::new(), None, true),
    };
    CardSet {
        agent: agent.clone(),
        id: None,
        kind: CardKind::Q,
        text: text.into(),
        choices: choices
            .into_iter()
            .map(|label| Choice {
                label: label.into(),
                detail: None,
            })
            .collect(),
        rec,
        free_text,
        deliver: Deliver::Poll,
        timeout_s: Some(ANSWER_WITHIN_S),
        origin: Origin {
            kind: OriginKind::Question,
            tool: None,
            request_id: None,
            group: None,
        },
    }
}

/// The sample, in `language`.
#[must_use]
pub fn sample(language: Language) -> VoiceSay {
    VoiceSay {
        text: language.words().sample.into(),
        agent: None,
        lane: SAMPLE_LANE.into(),
        voice: None,
        interrupt: false,
        ttl_s: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioHandle, FromAudio};
    use crate::config::Config;
    use crate::core::handle::{ClientInfo, CoreMsg, OpenSession, SessionKind};
    use crate::core::testkit::{self, detached_core};
    use crate::core::Core;
    use crossbeam_channel::Receiver;
    use deck_model::{
        Agent, AudioCmd, DeviceLine, DeviceOp, DeviceSet, HostLine, Key, LinkEvent, PttCtx,
        Request, Scope, SetKey, Slot, VoiceIn,
    };
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;

    const METHODS: [&str; 4] = [
        "care.test.question",
        "care.test.voice",
        "care.test.say",
        "care.test.status",
    ];

    /// A care method as the app calls it: `admin`, `{}`.
    fn call(c: &mut Core, method: &str, now: Instant) -> Result<Value, ApiError> {
        c.call(testkit::ctx(99, &[Scope::Admin]), method, json!({}), now)
            .result
    }

    fn status(c: &mut Core, now: Instant) -> Value {
        call(c, "care.test.status", now).expect("care.test.status answers")
    }

    /// The deck links: opened, its hello (with the language a deck that has
    /// the setting sends), live.
    fn link(c: &mut Core, language: Option<u8>, now: Instant) {
        c.on_link(LinkEvent::Linked, now);
        c.on_device_line(
            DeviceLine::Hello {
                v: 1,
                fw: "0.13.0".into(),
                sid: 7,
                boot: false,
                rx: deck_model::LINE_MAX_U16,
                agents: None,
                lang: language,
                lang_rev: language.map(|_| 1),
                faces: Vec::new(),
            },
            now,
        );
        c.on_link(LinkEvent::Live { sid: 7 }, now);
    }

    /// The agent "Test", while it is registered.
    fn test_agent(c: &Core) -> Option<Agent> {
        let agents = &c.store.snapshot().agents;
        agents.values().find(|agent| agent.name == AGENT).cloned()
    }

    /// The last card put on the deck since the last look: `(slot, id, text,
    /// choices)`.
    fn card_sent(ops: &Receiver<DeviceOp>) -> Option<(Slot, u16, String, Vec<String>)> {
        ops.try_iter()
            .filter_map(|op| match op {
                DeviceOp::Put(Key::Card(slot, id), HostLine::Card { text, choices, .. }) => {
                    Some((slot, id, text, choices))
                }
                _ => None,
            })
            .last()
    }

    /// ENTER on card `card` of `slot`.
    fn enter(c: &mut Core, slot: Slot, card: u16, now: Instant) -> Vec<Event> {
        let answer = DeviceLine::Answer {
            slot: slot.0,
            card,
            choice: 0,
        };
        c.on_device_line(answer, now)
    }

    /// A press on `slot`, on its card or talking to it, that the recogniser
    /// hears as `words`.
    fn press(c: &mut Core, slot: Slot, card: Option<u16>, words: &str, now: Instant) {
        let ctx = if card.is_some() {
            PttCtx::Answer
        } else {
            PttCtx::Talk
        };
        for act in [1, 0] {
            let edge = DeviceLine::Ptt {
                act,
                slot: slot.0,
                ctx,
                card,
                abort: false,
                press: None,
            };
            c.on_device_line(edge, now);
        }
        let heard = VoiceIn::Final { text: words.into() };
        c.on_from_audio(FromAudio::Voice(heard), now);
    }

    fn reconfigure(c: &mut Core, now: Instant, edit: impl FnOnce(&mut Config)) {
        let mut config = c.config().clone();
        edit(&mut config);
        c.on_msg(CoreMsg::Config(Arc::new(config)), now);
    }

    fn after(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// Before any test the status is two nulls. Every method takes `{}` or
    /// nothing, and refuses anything else before it does anything at all.
    #[test]
    fn before_any_test_the_status_is_two_nulls_and_every_method_takes_nothing() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-fresh");
        let t0 = Instant::now();
        assert_eq!(status(&mut c, t0), json!({"question": null, "voice": null}));
        let sessions = c.sessions.len();
        for method in METHODS {
            let params = json!({"lang": "ja"});
            let reply = c.call(testkit::ctx(99, &[Scope::Admin]), method, params, t0);
            let error = reply.result.unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{method}");
        }
        assert_eq!(c.sessions.len(), sessions, "no care session for a refusal");
        assert_eq!(status(&mut c, t0), json!({"question": null, "voice": null}));
        for method in METHODS {
            let reply = c.call(testkit::ctx(99, &[Scope::Admin]), method, Value::Null, t0);
            assert!(reply.result.is_ok(), "{method}: {:?}", reply.result);
        }
        assert!(card_sent(&ops).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With no deck linked nothing is sent: both tests are `no_deck` at
    /// once, and no agent is registered.
    #[test]
    fn with_no_deck_linked_nothing_is_sent_and_the_test_is_no_deck() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-no-deck");
        let t0 = Instant::now();
        let t1 = after(t0, 2_000);
        let no_deck = json!({"state": "no_deck"});
        assert_eq!(call(&mut c, "care.test.question", t0).unwrap(), no_deck);
        assert_eq!(call(&mut c, "care.test.voice", t1).unwrap(), no_deck);
        assert!(test_agent(&c).is_none());
        assert!(card_sent(&ops).is_none());
        assert_eq!(
            status(&mut c, t1),
            json!({
                "question": {"state": "no_deck", "at": c.store.ts(t0), "after_ms": null},
                "voice": {"state": "no_deck", "at": c.store.ts(t1), "heard": null},
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The test question reaches the deck as the agent "Test" with one
    /// choice, OK. ENTER answers it: the answer is delivered, then the agent
    /// leaves the deck, and the status says how long the deck took.
    #[test]
    fn a_question_answered_on_the_deck_is_answered_after_the_time_it_took() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-answered");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let _ = ops.try_iter().count();
        assert_eq!(
            call(&mut c, "care.test.question", t0).unwrap(),
            json!({"state": "sent"})
        );
        let agent = test_agent(&c).expect("the agent is registered");
        assert_eq!(
            (agent.kind, &agent.owner.principal),
            (Kind::Other, &principal())
        );
        let slot = agent.slot.expect("it is on the deck");
        let (on, card, text, choices) = card_sent(&ops).expect("a card line");
        assert_eq!(on, slot);
        assert_eq!(text, "Test question from your Mac. Press ENTER.");
        assert_eq!(choices, ["OK"]);
        assert_eq!(
            status(&mut c, t0)["question"],
            json!({"state": "sent", "at": c.store.ts(t0), "after_ms": null})
        );

        let events = enter(&mut c, slot, card, after(t0, 3_120));
        let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
        let at = |kind| kinds.iter().position(|k| *k == kind);
        let order = [
            at(EventKind::CardAnswered),
            at(EventKind::CardDelivered),
            at(EventKind::CardCleared),
            at(EventKind::AgentRemoved),
        ];
        assert!(
            order.iter().all(Option::is_some) && order.is_sorted(),
            "answered, delivered, cleared, then gone: {kinds:?}"
        );
        assert_eq!(
            status(&mut c, t0)["question"],
            json!({"state": "answered", "at": c.store.ts(t0), "after_ms": 3120})
        );
        assert!(test_agent(&c).is_none(), "the agent left the deck");
        let gone = DeviceOp::Put(Key::AgentGone(slot), HostLine::AgentGone { slot: slot.0 });
        assert!(ops.try_iter().any(|op| op == gone), "its slot was cleared");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A question nobody answers is `expired` after a minute, and the agent
    /// leaves the deck with its card.
    #[test]
    fn an_unanswered_question_expires_after_a_minute_and_the_agent_leaves() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("care-test-expired");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        call(&mut c, "care.test.question", t0).unwrap();
        c.tick(after(t0, 59_000));
        assert_eq!(status(&mut c, t0)["question"]["state"], "sent");
        assert!(test_agent(&c).is_some());
        c.tick(after(t0, 60_000));
        assert_eq!(
            status(&mut c, t0)["question"],
            json!({"state": "expired", "at": c.store.ts(t0), "after_ms": null})
        );
        assert!(test_agent(&c).is_none(), "the agent left the deck");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A second question while one is on the deck replaces it: one agent,
    /// the old card off the deck and refused, the new one timed from when it
    /// was sent.
    #[test]
    fn a_second_question_while_one_is_pending_replaces_it() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-again");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        call(&mut c, "care.test.question", t0).unwrap();
        let first = test_agent(&c).unwrap();
        let slot = first.slot.unwrap();
        let (_, old, _, _) = card_sent(&ops).unwrap();

        let t1 = after(t0, 5_000);
        assert_eq!(
            call(&mut c, "care.test.question", t1).unwrap(),
            json!({"state": "sent"})
        );
        let agents = &c.store.snapshot().agents;
        let tests = agents.values().filter(|a| a.name == AGENT).count();
        assert_eq!(tests, 1, "the same agent asks again");
        assert_eq!(test_agent(&c).unwrap().id, first.id);
        let ops: Vec<DeviceOp> = ops.try_iter().collect();
        assert!(
            ops.contains(&DeviceOp::Remove(Key::Card(slot, old))),
            "{ops:?}"
        );
        let new = ops
            .iter()
            .find_map(|op| match op {
                DeviceOp::Put(Key::Card(_, id), _) => Some(*id),
                _ => None,
            })
            .expect("the new card");
        assert_ne!(new, old);
        assert_eq!(status(&mut c, t1)["question"]["at"], json!(c.store.ts(t1)));

        // The old card is gone: the deck's answer to it changes nothing.
        let events = enter(&mut c, slot, old, after(t1, 1_000));
        assert!(events.iter().any(|e| e.kind == EventKind::Error));
        assert_eq!(status(&mut c, t1)["question"]["state"], "sent");
        enter(&mut c, slot, new, after(t1, 2_000));
        assert_eq!(
            status(&mut c, t1)["question"],
            json!({"state": "answered", "at": c.store.ts(t1), "after_ms": 2000})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The voice test's card asks for no choice. What a press says to it is
    /// `heard`, and a press the recogniser heard nothing in is heard too,
    /// as nothing; either way the agent leaves the deck.
    #[test]
    fn a_voice_test_hears_what_a_press_says_and_hears_nothing_as_nothing() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-voice");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let _ = ops.try_iter().count();
        assert_eq!(
            call(&mut c, "care.test.voice", t0).unwrap(),
            json!({"state": "waiting"})
        );
        let slot = test_agent(&c).unwrap().slot.unwrap();
        let (_, card, text, choices) = card_sent(&ops).unwrap();
        assert_eq!(text, "Hold PTT and say hello.");
        assert!(choices.is_empty(), "{choices:?}");
        assert_eq!(
            status(&mut c, t0)["voice"],
            json!({"state": "waiting", "at": c.store.ts(t0), "heard": null})
        );
        press(&mut c, slot, Some(card), "hello", after(t0, 4_000));
        assert_eq!(
            status(&mut c, t0)["voice"],
            json!({"state": "heard", "at": c.store.ts(t0), "heard": "hello"})
        );
        assert!(test_agent(&c).is_none(), "the agent left the deck");

        // Talking to the agent rather than to its card is heard as well;
        // an empty transcript is a press that said nothing.
        let t1 = after(t0, 10_000);
        call(&mut c, "care.test.voice", t1).unwrap();
        let slot = test_agent(&c).unwrap().slot.unwrap();
        press(&mut c, slot, None, "", after(t1, 3_000));
        assert_eq!(
            status(&mut c, t1)["voice"],
            json!({"state": "heard", "at": c.store.ts(t1), "heard": ""})
        );
        assert!(test_agent(&c).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A voice test nobody speaks to is `expired` after a minute, with
    /// nothing heard.
    #[test]
    fn an_unanswered_voice_test_expires_after_a_minute() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("care-test-silence");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        call(&mut c, "care.test.voice", t0).unwrap();
        c.tick(after(t0, 60_000));
        assert_eq!(
            status(&mut c, t0)["voice"],
            json!({"state": "expired", "at": c.store.ts(t0), "heard": null})
        );
        assert!(test_agent(&c).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One test on the deck at a time: a voice test ends a question that
    /// was not answered as `expired`, and the other way round; the same
    /// agent shows the new card.
    #[test]
    fn a_new_test_of_the_other_kind_ends_the_one_on_the_deck_as_expired() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("care-test-switch");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        call(&mut c, "care.test.question", t0).unwrap();
        let agent = test_agent(&c).unwrap().id;
        let t1 = after(t0, 2_000);
        call(&mut c, "care.test.voice", t1).unwrap();
        let now = status(&mut c, t1);
        assert_eq!(now["question"]["state"], "expired");
        assert_eq!(now["voice"]["state"], "waiting");
        let shown = test_agent(&c).unwrap();
        assert_eq!(shown.id, agent);
        let card = shown.card.expect("the voice card");
        assert_eq!(card.text.as_deref(), Some(Language::English.words().voice));

        let t2 = after(t1, 2_000);
        call(&mut c, "care.test.question", t2).unwrap();
        let now = status(&mut c, t2);
        assert_eq!(
            now["voice"],
            json!({"state": "expired", "at": c.store.ts(t1), "heard": null})
        );
        assert_eq!(
            now["question"],
            json!({"state": "sent", "at": c.store.ts(t2), "after_ms": null})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The deck's language: the shared setting, or English on a deck whose
    /// hello on this link named no language setting.
    #[test]
    fn the_language_is_the_decks_own() {
        let mut snapshot = Snapshot::default();
        assert_eq!(Language::of(&snapshot), Language::English);
        snapshot.settings.language = 3;
        assert_eq!(
            Language::of(&snapshot),
            Language::Japanese,
            "no hello yet: the setting it will show"
        );
        snapshot.device.fw = Some("0.12.0".into());
        assert_eq!(
            Language::of(&snapshot),
            Language::English,
            "its hello named no language"
        );
        snapshot.device.link = deck_model::Link::Degraded;
        assert_eq!(Language::of(&snapshot), Language::English, "still");
        snapshot.device.language_supported = true;
        assert_eq!(Language::of(&snapshot), Language::Japanese);
        snapshot.settings.language = 9;
        assert_eq!(Language::of(&snapshot), Language::English);
    }

    /// The cards are in the deck's language, which its hello names; a deck
    /// whose firmware has no language setting is asked in English whatever
    /// the setting says.
    #[test]
    fn the_cards_speak_the_decks_language() {
        let cases = [
            (
                Some(1),
                "来自 Mac 的测试提问。请按 ENTER。",
                "好",
                "按住 PTT 并说声你好。",
            ),
            (
                Some(2),
                "來自 Mac 的測試提問。請按 ENTER。",
                "好",
                "按住 PTT 並說聲你好。",
            ),
            (
                Some(3),
                "Mac からのテスト質問です。ENTER を押してください。",
                "OK",
                "PTT を押しながら「こんにちは」と話してください。",
            ),
            (
                None,
                "Test question from your Mac. Press ENTER.",
                "OK",
                "Hold PTT and say hello.",
            ),
        ];
        for (language, question, ok, voice) in cases {
            let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-language");
            let t0 = Instant::now();
            if language.is_none() {
                // The owner chose Japanese for a deck that cannot show it.
                let japanese = Request::DeviceSet(DeviceSet {
                    key: SetKey::Language,
                    value: 3,
                });
                let principal = Principal::Uid(501);
                c.api_call(principal, SessionId(42), false, japanese, t0)
                    .unwrap();
            }
            link(&mut c, language, t0);
            let _ = ops.try_iter().count();
            call(&mut c, "care.test.question", t0).unwrap();
            let (_, _, text, choices) = card_sent(&ops).unwrap();
            assert_eq!((text.as_str(), choices), (question, vec![ok.to_string()]));
            call(&mut c, "care.test.voice", t0).unwrap();
            let (_, _, text, _) = card_sent(&ops).unwrap();
            assert_eq!(text, voice, "{language:?}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// The sample is spoken on `chatter` in the deck's language, with no
    /// card and no agent; it is a `voice.say`, so the caller's four a
    /// second apply.
    #[test]
    fn the_sample_is_spoken_on_chatter_in_the_decks_language() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("care-test-say");
        let (audio, said, _events) = AudioHandle::detached();
        c.set_audio(audio);
        let t0 = Instant::now();
        link(&mut c, Some(2), t0);
        let reply = call(&mut c, "care.test.say", t0).unwrap();
        let utterance = reply["utterance"].as_str().expect("{reply}").to_string();
        assert!(utterance.starts_with("ut_"), "{reply}");
        let spoken = said.try_iter().find_map(|cmd| match cmd {
            AudioCmd::Say {
                utterance,
                text,
                lane,
                ..
            } => Some((utterance, text, lane)),
            _ => None,
        });
        assert_eq!(
            spoken,
            Some((
                utterance,
                "這裡是 OpenWalkieTalkie。".into(),
                "chatter".into()
            ))
        );
        assert!(test_agent(&c).is_none(), "no card, no agent");
        for _ in 0..3 {
            call(&mut c, "care.test.say", t0).unwrap();
        }
        let error = call(&mut c, "care.test.say", t0).unwrap_err();
        assert_eq!(error.code, ApiError::RATE_LIMITED);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A connected app's connection, as the UDS door opens it.
    fn open_app(c: &mut Core, name: &str, now: Instant) -> SessionId {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let (reply, mut opened) = tokio::sync::oneshot::channel();
        let open = OpenSession {
            kind: SessionKind::Live { tx },
            principal: Principal::Uid(501),
            scopes: vec![Scope::Agent, Scope::Observe],
            trusted: false,
            client: ClientInfo {
                name: name.into(),
                version: "1".into(),
            },
        };
        c.on_msg(CoreMsg::OpenSession { open, reply }, now);
        opened
            .try_recv()
            .expect("answered in the step")
            .expect("opened")
    }

    /// The care session is the daemon's own and never a source. With every
    /// source paused, Claude Code off and an app that calls itself `care`
    /// paused by name, that app's agent leaves the deck while the test still
    /// reaches it, stays through a change of what is muted, and is answered.
    /// No source counts the test agent either.
    #[test]
    fn a_paused_daemon_still_runs_the_test_because_the_care_session_is_never_muted() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-paused");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let app = open_app(&mut c, CLIENT, t0);
        let register = Request::AgentRegister(testkit::register("chat", Some("k:chat")));
        let registered = c.api_call(Principal::Uid(501), app, false, register, t0);
        let chat: AgentId = serde_json::from_value(registered.unwrap()["agent"].clone()).unwrap();
        let slot_of = |c: &Core, id: &AgentId| c.store.snapshot().agents[id].slot;
        assert!(slot_of(&c, &chat).is_some());
        reconfigure(&mut c, t0, |config| {
            config.sources.paused = true;
            config.sources.claude = false;
            config.sources.apps_paused = vec![CLIENT.into()];
        });
        assert_eq!(slot_of(&c, &chat), None, "the app is paused");

        let _ = ops.try_iter().count();
        call(&mut c, "care.test.question", t0).unwrap();
        let agent = test_agent(&c).unwrap();
        let slot = agent.slot.expect("the test is on the deck while paused");
        let (_, card, _, _) = card_sent(&ops).expect("its card went to the deck");
        let sources = c
            .call(
                testkit::ctx(98, &[Scope::Observe]),
                "sources.list",
                json!({}),
                t0,
            )
            .result
            .unwrap();
        let rows = sources["sources"].as_array().unwrap();
        let counted: u64 = rows.iter().filter_map(|row| row["sessions"].as_u64()).sum();
        assert_eq!(counted, 1, "the app's own agent alone: {sources}");
        assert_eq!(sources["other"], 0, "{sources}");

        // What is muted changes while the test is on the deck: the app comes
        // back, and the test agent stays where it is.
        reconfigure(&mut c, t0, |config| {
            config.sources.paused = false;
            config.sources.apps_paused.clear();
        });
        assert!(slot_of(&c, &chat).is_some(), "the app is back");
        assert_eq!(slot_of(&c, &agent.id), Some(slot), "the test stayed");
        enter(&mut c, slot, card, after(t0, 1_500));
        assert_eq!(
            status(&mut c, t0)["question"],
            json!({"state": "answered", "at": c.store.ts(t0), "after_ms": 1500})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The JSON type of a value.
    fn json_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// `got` has exactly `want`'s keys, all the way down, each with the
    /// same JSON type.
    fn same_shape(at: &str, want: &Value, got: &Value) {
        assert_eq!(json_type(got), json_type(want), "{at}: {got} where {want}");
        let (Value::Object(want), Value::Object(got)) = (want, got) else {
            return;
        };
        let keys = |map: &serde_json::Map<String, Value>| map.keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(got), keys(want), "{at}: the keys");
        for (key, value) in want {
            same_shape(&format!("{at}.{key}"), value, &got[key]);
        }
    }

    /// A2/B3.3: the real reply, after a question answered and a voice test
    /// heard, has exactly the keys of the app's fixture
    /// (`GC/tests/fixtures/daemon/care_test_status.json`) with the same JSON
    /// types, and the same states, time and words for the same deck.
    #[test]
    fn the_status_has_every_key_of_the_apps_fixture_with_the_same_types() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/care_test_status.json"
        ))
        .unwrap();
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-test-fixture");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        call(&mut c, "care.test.question", t0).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();
        enter(&mut c, slot, card, after(t0, 3_120));
        let t1 = after(t0, 60_000);
        call(&mut c, "care.test.voice", t1).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();
        press(&mut c, slot, Some(card), "hello", after(t1, 5_000));
        let reply = status(&mut c, t1);
        same_shape("care.test.status", &fixture, &reply);
        for (test, key) in [("question", "after_ms"), ("voice", "heard")] {
            for key in ["state", key] {
                assert_eq!(reply[test][key], fixture[test][key], "{test}.{key}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
