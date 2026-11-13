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
//!
//! The same session asks the deck before a firmware install
//! (`Core::install_after_press`): an agent named for the deck's firmware,
//! in its language ([`Words::firmware`]), asks "Install firmware {version}?
//! Press ENTER." with one choice, Install, beside whatever test is on the
//! deck. Only ENTER on the deck confirms it ([`Press::Confirmed`]): a
//! dismissal, its minute, words said to it or an answer from anywhere but
//! the deck end it with nothing installed. The agent leaves the deck either
//! way.
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
/// the string sheet `deckd.tsv`, then the install's three.
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
    /// The question before a firmware install, `{0}` its version.
    pub install: &'static str,
    /// Its one choice.
    pub install_choice: &'static str,
    /// The name it is asked under: the deck's own word for its firmware.
    pub firmware: &'static str,
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
                install: "Install firmware {0}? Press ENTER.",
                install_choice: "Install",
                firmware: "Firmware",
            },
            Language::SimplifiedChinese => Words {
                question: "来自 Mac 的测试提问。请按 ENTER。",
                ok: "好",
                voice: "按住 PTT 并说声你好。",
                sample: "这里是 OpenWalkieTalkie。",
                install: "安装固件 {0}？请按 ENTER。",
                install_choice: "安装",
                firmware: "固件",
            },
            Language::TraditionalChinese => Words {
                question: "來自 Mac 的測試提問。請按 ENTER。",
                ok: "好",
                voice: "按住 PTT 並說聲你好。",
                sample: "這裡是 OpenWalkieTalkie。",
                install: "安裝韌體 {0}？請按 ENTER。",
                install_choice: "安裝",
                firmware: "韌體",
            },
            Language::Japanese => Words {
                question: "Mac からのテスト質問です。ENTER を押してください。",
                ok: "OK",
                voice: "PTT を押しながら「こんにちは」と話してください。",
                sample: "こちらは OpenWalkieTalkie です。",
                install: "ファームウェア {0} をインストールしますか？ENTER を押してください。",
                install_choice: "インストール",
                firmware: "ファームウェア",
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

/// How the install's card ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// ENTER on the deck: its one choice, chosen there.
    Confirmed,
    /// Anything else: dismissed on the deck, its minute over, words said
    /// to it, an answer from anywhere but the deck, or its agent gone.
    NotConfirmed,
}

/// What one step's events settled for the care session's cards.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// The agents to take off the deck: their card is over and they are
    /// still registered.
    pub release: Vec<AgentId>,
    /// How the install's card ended, when it did.
    pub install: Option<Press>,
}

/// What the core keeps about the care session's agents between calls: the
/// test agent and the install's.
#[derive(Debug, Default)]
pub struct TestAgent {
    /// The care session, once opened. It never closes.
    session: Option<SessionId>,
    /// The agent "Test", while it is registered.
    agent: Option<AgentId>,
    pending: Option<Pending>,
    question: Option<QuestionTest>,
    voice: Option<VoiceTest>,
    /// The agent that asks before a firmware install, while it is
    /// registered.
    installer: Option<AgentId>,
    /// Its card, while it waits for the press.
    install_card: Option<CardId>,
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

    /// The agent that asks before a firmware install, while it is
    /// registered.
    #[must_use]
    pub fn installer(&self) -> Option<&AgentId> {
        self.installer.as_ref()
    }

    pub fn set_installer(&mut self, agent: AgentId) {
        self.installer = Some(agent);
    }

    /// The install's `card` is on its way to the deck: its answer settles
    /// the install ([`TestAgent::observe`]).
    pub fn install_asked(&mut self, card: CardId) {
        self.install_card = Some(card);
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

    /// What one step's events settled for the cards of the care session:
    /// the test on the deck ([`TestAgent::observe_test`]) and the install's
    /// card ([`TestAgent::observe_install`]), each with the agent to take
    /// off the deck once its card is over.
    pub fn observe(&mut self, events: &[Event], now: Instant) -> Settled {
        let mut settled = Settled::default();
        settled.release.extend(self.observe_test(events, now));
        self.observe_install(events, &mut settled);
        settled
    }

    /// What one step's events mean for the test on the deck: its card
    /// answered (a choice, or words said to it), the words of a press
    /// addressed to the agent during a voice test, or the card gone
    /// unanswered (`card.expired`, a clear, the agent removed). Returns the
    /// agent to take off the deck when a test ended and its agent is still
    /// registered.
    fn observe_test(&mut self, events: &[Event], now: Instant) -> Option<AgentId> {
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

    /// What one step's events mean for the install's card: ENTER on the
    /// deck confirms it ([`pressed_on_deck`]); whatever else ends it — a
    /// dismissal (`card.snoozed`), its minute (`card.expired`), an answer
    /// in words or from anywhere but the deck, a clear, its agent removed —
    /// does not.
    fn observe_install(&mut self, events: &[Event], settled: &mut Settled) {
        for event in events {
            let Some(agent) = self.installer.clone() else {
                return;
            };
            if crate::core::event_agent(event).as_ref() != Some(&agent) {
                continue;
            }
            if event.kind == EventKind::AgentRemoved {
                // Nothing left to release, and a card still on it is over.
                self.installer = None;
                settled.release.retain(|gone| *gone != agent);
                if self.install_card.take().is_some() {
                    settled.install = Some(Press::NotConfirmed);
                }
                continue;
            }
            if self.install_card.is_none() || card_of(event) != self.install_card {
                continue;
            }
            let press = match event.kind {
                EventKind::CardAnswered if pressed_on_deck(event) => Press::Confirmed,
                EventKind::CardAnswered
                | EventKind::CardSnoozed
                | EventKind::CardExpired
                | EventKind::CardCleared => Press::NotConfirmed,
                _ => continue,
            };
            self.install_card = None;
            settled.install = Some(press);
            settled.release.push(agent);
        }
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

/// ENTER on the deck: the card's one choice, chosen on the deck itself
/// (`device.answer`). An answer anyone else gives — `op.answer` from the
/// app or a script — or words said to the card confirm nothing: the press
/// is the point.
fn pressed_on_deck(event: &Event) -> bool {
    event.principal == Principal::Device
        && event.data.get("source").and_then(serde_json::Value::as_str) == Some("deck")
        && event.data.get("choice").and_then(serde_json::Value::as_u64) == Some(0)
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
    care_agent(AGENT)
}

/// The agent that asks before a firmware install: the test agent's kind
/// and caps under the deck's own word for its firmware, in `language`.
#[must_use]
pub fn register_installer(language: Language) -> AgentRegister {
    care_agent(language.words().firmware)
}

/// An agent of the care session named `name` (see [`register`]).
fn care_agent(name: &str) -> AgentRegister {
    AgentRegister {
        kind: Kind::Other,
        name: name.into(),
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
    match test {
        Test::Question => question(agent, words.question.into(), Some(words.ok)),
        Test::Voice => question(agent, words.voice.into(), None),
    }
}

/// The install's card for `agent`, in `language`: "Install firmware
/// {version}? Press ENTER." with one choice, Install, gone after
/// [`ANSWER_WITHIN_S`] as a test's is.
#[must_use]
pub fn install_card(agent: &AgentId, version: &str, language: Language) -> CardSet {
    let words = language.words();
    let text = words.install.replace("{0}", version);
    question(agent, text, Some(words.install_choice))
}

/// A question of the care session: with one `choice`, recommended, which
/// ENTER picks; or with none, answered by what a press says to it.
fn question(agent: &AgentId, text: String, choice: Option<&str>) -> CardSet {
    CardSet {
        agent: agent.clone(),
        id: None,
        kind: CardKind::Q,
        text,
        choices: choice
            .into_iter()
            .map(|label| Choice {
                label: label.into(),
                detail: None,
            })
            .collect(),
        rec: choice.map(|_| 0),
        free_text: choice.is_none(),
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
    use crate::firmware;
    use crossbeam_channel::Receiver;
    use deck_model::{
        Agent, AudioCmd, Command, DeviceLine, DeviceOp, DeviceSet, HostLine, Key, LinkEvent,
        OpAnswer, PttCtx, Request, Scope, SetKey, Slot, UtterancePhase, VoiceIn,
    };
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
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

    // ---- the press before a firmware install ----------------------------------

    /// What the not-confirmed install says.
    const NOT_CONFIRMED: &str = "Not confirmed on the deck. Nothing was installed.";

    /// What an install the deck confirmed says while the voice is not yet
    /// quiet.
    const STARTS_WHEN_QUIET: &str = "Starting the install when the deck stops talking…";

    /// The core's updater has checked: `deck` on the cable and the test
    /// release (0.2.0, Rev A) found. What it starts is listed, never run.
    fn checked(c: &mut Core, deck: firmware::Device) -> Arc<Mutex<Vec<firmware::Install>>> {
        let (updater, started) = firmware::testkit::checked(deck);
        c.firmware = updater;
        started
    }

    /// `firmware.install` of the test release on the test deck, as the app
    /// sends it.
    fn install(c: &mut Core, now: Instant) -> Result<Value, ApiError> {
        let params = json!({
            "board": "hw-a", "serial": "USB-A", "version": "0.2.0",
            "allow_bootloader_update": false,
        });
        let ctx = testkit::ctx(99, &[Scope::Admin]);
        c.call(ctx, "firmware.install", params, now).result
    }

    /// `firmware.status`, as the app reads it.
    fn firmware_status(c: &mut Core, now: Instant) -> Value {
        let ctx = testkit::ctx(99, &[Scope::Observe]);
        let status = c.call(ctx, "firmware.status", json!({}), now).result;
        status.expect("firmware.status answers")
    }

    /// The agent that asks before an install, in any of the four
    /// languages, while it is registered.
    fn installer(c: &Core) -> Option<Agent> {
        let names = [
            Language::English,
            Language::SimplifiedChinese,
            Language::TraditionalChinese,
            Language::Japanese,
        ]
        .map(|language| language.words().firmware);
        let agents = &c.store.snapshot().agents;
        let asking = agents.values().find(|a| names.contains(&a.name.as_str()));
        asking.cloned()
    }

    /// The owner's PTT on `slot`, talking: down (`1`) or up (`0`).
    fn talk(c: &mut Core, slot: Slot, act: u8, now: Instant) {
        let edge = DeviceLine::Ptt {
            act,
            slot: slot.0,
            ctx: PttCtx::Talk,
            card: None,
            abort: false,
            press: None,
        };
        c.on_device_line(edge, now);
    }

    /// A row of the deck other than `slot`'s.
    fn other_than(slot: Slot) -> Slot {
        Slot(if slot.0 == 1 { 2 } else { 1 })
    }

    /// The engine says the sentence being read out has ended, as the audio
    /// side reports it (a unit-test core has no engine to).
    fn said(c: &mut Core, now: Instant) {
        let speaking = c.store.snapshot().voice.speaking.clone();
        let utterance = speaking.expect("something is being said").utterance;
        let ended = VoiceIn::Utterance {
            utterance,
            phase: UtterancePhase::Ended,
        };
        c.step(Command::Voice(ended), now);
    }

    /// On a linked deck an install waits for ENTER on it. The reply is
    /// `confirming` at once; the deck asks "Install firmware 0.2.0? Press
    /// ENTER." with one choice, Install, from an agent named for its
    /// firmware, and reads the card out as it reads any question; nothing
    /// starts before the press. ENTER starts the install the app asked for,
    /// and the agent leaves the deck.
    #[test]
    fn on_a_linked_deck_an_install_waits_for_enter_and_starts_with_it() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-confirmed");
        let started = checked(&mut c, firmware::testkit::deck());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let _ = ops.try_iter().count();
        let reply = install(&mut c, t0).unwrap();
        assert_eq!(reply["phase"], "confirming", "{reply}");
        assert_eq!(
            reply["message"],
            "Press ENTER on the deck to start the install."
        );
        assert_eq!(reply["available"], true, "{reply}");
        let (slot, card, text, choices) = card_sent(&ops).expect("the card went to the deck");
        assert_eq!(text, "Install firmware 0.2.0? Press ENTER.");
        assert_eq!(choices, ["Install"]);
        let agent = installer(&c).expect("the agent is registered");
        assert_eq!(
            (agent.name.as_str(), agent.kind, &agent.owner.principal),
            ("Firmware", Kind::Other, &principal())
        );
        assert_eq!(agent.slot, Some(slot));
        let reading = c.store.snapshot().voice.speaking.clone().unwrap();
        assert_eq!(
            (reading.lane.as_str(), reading.agent),
            ("question", Some(agent.id))
        );
        assert!(
            started.lock().unwrap().is_empty(),
            "nothing before the press"
        );
        assert_eq!(firmware_status(&mut c, t0)["phase"], "confirming");
        assert!(test_agent(&c).is_none(), "no test is involved");

        said(&mut c, after(t0, 2_000));
        enter(&mut c, slot, card, after(t0, 2_500));
        assert_eq!(*started.lock().unwrap(), [firmware::testkit::install()]);
        assert_eq!(firmware_status(&mut c, t0)["phase"], "downloading");
        assert!(installer(&c).is_none(), "the agent left the deck");
        let gone = DeviceOp::Put(Key::AgentGone(slot), HostLine::AgentGone { slot: slot.0 });
        assert!(ops.try_iter().any(|op| op == gone), "its slot was cleared");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every way the card ends but ENTER on the deck leaves nothing
    /// installed and the updater idle, saying so: CANCEL on the deck, its
    /// minute over, words said to it, or an answer from the app — even a
    /// trusted one: the press is the deck's alone. The agent leaves the
    /// deck each time.
    #[test]
    fn an_install_not_confirmed_on_the_deck_is_idle_with_the_reason() {
        type End = fn(&mut Core, Slot, AgentId, u16, Instant);
        let endings: [(&str, End); 4] = [
            ("dismissed", |c, slot, _, card, t0| {
                let cancel = DeviceLine::Snooze { slot: slot.0, card };
                c.on_device_line(cancel, after(t0, 3_000));
            }),
            ("expired", |c, _, _, _, t0| {
                c.tick(after(t0, 59_000));
                assert_eq!(firmware_status(c, t0)["phase"], "confirming", "not yet");
                c.tick(after(t0, 60_000));
            }),
            ("said", |c, slot, _, card, t0| {
                press(c, slot, Some(card), "install it", after(t0, 3_000));
            }),
            ("answered by the app", |c, _, agent, card, t0| {
                let answer = Request::OpAnswer(OpAnswer {
                    agent,
                    card: CardId(card),
                    choice: Some(0),
                    text: None,
                });
                let app = Principal::Uid(501);
                c.api_call(app, SessionId(42), true, answer, after(t0, 3_000))
                    .unwrap();
            }),
        ];
        for (how, end) in endings {
            let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-unconfirmed");
            let started = checked(&mut c, firmware::testkit::deck());
            let t0 = Instant::now();
            link(&mut c, None, t0);
            install(&mut c, t0).unwrap();
            let (slot, card, _, _) = card_sent(&ops).unwrap();
            let agent = installer(&c).unwrap().id;
            end(&mut c, slot, agent, card, t0);
            let status = firmware_status(&mut c, t0);
            assert_eq!(status["phase"], "idle", "{how}: {status}");
            assert_eq!(status["message"], NOT_CONFIRMED, "{how}");
            assert!(status["error"].is_null(), "{how}: {status}");
            assert_eq!(status["available"], true, "{how}: it can be asked again");
            assert!(
                started.lock().unwrap().is_empty(),
                "{how}: nothing installed"
            );
            assert!(installer(&c).is_none(), "{how}: the agent left the deck");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A second install while one waits for the deck puts no second card
    /// there: refused while the card is read out, as any install is while
    /// the deck talks, then as busy; a check waits too, and one press
    /// starts one install.
    #[test]
    fn a_second_install_while_one_waits_puts_no_second_card_on_the_deck() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-again");
        let started = checked(&mut c, firmware::testkit::deck());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        install(&mut c, t0).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();

        let reading = install(&mut c, after(t0, 500)).unwrap_err();
        assert_eq!(reading.data["why"], firmware::VOICE_BUSY);
        let t1 = after(t0, 1_000);
        said(&mut c, t1);
        let again = install(&mut c, t1).unwrap_err();
        assert_eq!(again.code, ApiError::INVALID);
        assert_eq!(again.data["why"], "An update operation is already running.");
        let ctx = testkit::ctx(99, &[Scope::Admin]);
        firmware::testkit::found_app(&c.firmware, "9.9.9");
        let check = c.call(ctx, "firmware.check", json!({"force": true}), t1);
        let check = check.result.unwrap();
        assert_eq!(check["phase"], "confirming");
        // Answered as `firmware.status` says it: the release is newer than
        // the deck (0.2.0 against 0.1.0), and the feed named an app.
        assert_eq!(check["available"], true, "{check}");
        assert_eq!(check["app"]["version"], "9.9.9", "{check}");
        let cards = ops
            .try_iter()
            .filter(|op| matches!(op, DeviceOp::Put(Key::Card(..), _)))
            .count();
        assert_eq!(cards, 0, "no second card");
        let agents = &c.store.snapshot().agents;
        let names: Vec<&str> = agents.values().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Firmware"], "one agent asks");

        enter(&mut c, slot, card, after(t1, 1_000));
        assert_eq!(started.lock().unwrap().len(), 1, "one press, one install");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Only a deck in recovery installs at once, as it always has, and
    /// nothing is put on the deck: it cannot show the card, whether it is
    /// not linked at all or opened by the hid thread but never saying
    /// hello. The app asks the owner for the board of a deck in recovery,
    /// and the install answers what it always answered.
    #[test]
    fn a_deck_in_recovery_installs_at_once_as_before() {
        type Session = fn(&mut Core, Instant);
        let cases: [(&str, Session); 2] = [
            ("no link", |_, _| {}),
            ("no hello", |c, t0| {
                c.on_link(LinkEvent::Linked, t0);
            }),
        ];
        for (how, session) in cases {
            let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-recovery");
            let started = checked(&mut c, firmware::testkit::deck_in_recovery());
            let t0 = Instant::now();
            session(&mut c, t0);
            let _ = ops.try_iter().count();
            let reply = install(&mut c, t0).unwrap();
            assert_eq!(reply["phase"], "downloading", "{how}: {reply}");
            let installs = started.lock().unwrap().clone();
            assert_eq!(installs, [firmware::testkit::install()], "{how}");
            assert!(card_sent(&ops).is_none(), "{how}: no card");
            assert!(installer(&c).is_none(), "{how}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A deck that runs its firmware — the last check did not find it in
    /// recovery — but has no session with the service cannot be asked, and
    /// nothing installs without the press: not linked, opened by the hid
    /// thread but never saying hello, or linked and gone again. The install
    /// is refused with the reason to try again once the deck links; nothing
    /// starts, nothing is put on the deck, and the release can be asked for
    /// again. Once the deck links, the same install asks it.
    #[test]
    fn a_deck_out_of_recovery_with_no_session_is_refused_until_it_links() {
        type Session = fn(&mut Core, Instant);
        let cases: [(&str, Session); 3] = [
            ("no link", |_, _| {}),
            ("no hello", |c, t0| {
                c.on_link(LinkEvent::Linked, t0);
            }),
            ("linked, then gone", |c, t0| {
                link(c, None, t0);
                c.on_link(LinkEvent::Down, t0);
            }),
        ];
        for (how, session) in cases {
            let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-unlinked");
            let started = checked(&mut c, firmware::testkit::deck());
            let t0 = Instant::now();
            session(&mut c, t0);
            let _ = ops.try_iter().count();
            let refused = install(&mut c, t0).unwrap_err();
            assert_eq!(refused.code, ApiError::INVALID, "{how}");
            assert_eq!(refused.data["why"], firmware::WAIT_FOR_LINK, "{how}");
            assert!(started.lock().unwrap().is_empty(), "{how}: nothing installed");
            assert!(card_sent(&ops).is_none(), "{how}: no card");
            assert!(installer(&c).is_none(), "{how}");
            let status = firmware_status(&mut c, t0);
            assert_eq!(status["phase"], "ready", "{how}: {status}");
            assert_eq!(status["available"], true, "{how}: it can be asked again");

            let t1 = after(t0, 1_000);
            link(&mut c, None, t1);
            let _ = ops.try_iter().count();
            let reply = install(&mut c, t1).unwrap();
            assert_eq!(reply["phase"], "confirming", "{how}, linked: {reply}");
            assert!(card_sent(&ops).is_some(), "{how}, linked: the card went");
            assert!(started.lock().unwrap().is_empty(), "{how}: not before the press");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A session proves the deck runs its firmware, since a deck in
    /// recovery never says hello: a check that found the deck in recovery,
    /// followed by a session (it restarted into its firmware since), still
    /// asks the deck. The stale flag never skips the card, and ENTER starts
    /// the install.
    #[test]
    fn a_live_session_asks_the_deck_even_when_the_last_check_found_recovery() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-stale-recovery");
        let started = checked(&mut c, firmware::testkit::deck_in_recovery());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let _ = ops.try_iter().count();
        let reply = install(&mut c, t0).unwrap();
        assert_eq!(reply["phase"], "confirming", "{reply}");
        let (slot, card, text, choices) = card_sent(&ops).expect("the card went to the deck");
        assert_eq!(text, "Install firmware 0.2.0? Press ENTER.");
        assert_eq!(choices, ["Install"]);
        assert!(
            started.lock().unwrap().is_empty(),
            "nothing before the press"
        );
        said(&mut c, after(t0, 2_000));
        enter(&mut c, slot, card, after(t0, 2_500));
        assert_eq!(*started.lock().unwrap(), [firmware::testkit::install()]);
        assert_eq!(firmware_status(&mut c, t0)["phase"], "downloading");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What the install would refuse is refused at once and asks the deck
    /// nothing: a service that never opens the deck (a unit-test core's
    /// updater is offline, as `--no-device`'s is), and a board the deck is
    /// not. The board check, the downgrade refusal and the bootloader's
    /// consent are the install's own (`firmware::Updater::vet`).
    #[test]
    fn an_install_that_would_be_refused_is_refused_at_once_without_a_card() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-refused");
        let t0 = Instant::now();
        link(&mut c, None, t0);
        let _ = ops.try_iter().count();
        let offline = install(&mut c, t0).unwrap_err();
        assert_eq!(offline.data["why"], firmware::NO_DEVICE);
        let started = checked(&mut c, firmware::testkit::deck());
        let rev_b = json!({"board": "hw-b", "serial": "USB-A", "version": "0.2.0"});
        let ctx = testkit::ctx(99, &[Scope::Admin]);
        let refused = c
            .call(ctx, "firmware.install", rev_b, t0)
            .result
            .unwrap_err();
        assert_eq!(
            refused.data["why"],
            "This firmware is for a different board."
        );
        assert!(card_sent(&ops).is_none(), "no card for a refusal");
        assert!(installer(&c).is_none());
        assert_eq!(firmware_status(&mut c, t0)["phase"], "ready");
        assert!(started.lock().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The install's card speaks the deck's language, under the deck's own
    /// word for its firmware.
    #[test]
    fn the_install_card_speaks_the_decks_language() {
        let cases = [
            (Some(1), "安装固件 0.2.0？请按 ENTER。", "安装", "固件"),
            (Some(2), "安裝韌體 0.2.0？請按 ENTER。", "安裝", "韌體"),
            (
                Some(3),
                "ファームウェア 0.2.0 をインストールしますか？ENTER を押してください。",
                "インストール",
                "ファームウェア",
            ),
            (
                None,
                "Install firmware 0.2.0? Press ENTER.",
                "Install",
                "Firmware",
            ),
        ];
        for (language, question, choice, name) in cases {
            let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-language");
            checked(&mut c, firmware::testkit::deck());
            let t0 = Instant::now();
            link(&mut c, language, t0);
            install(&mut c, t0).unwrap();
            let (_, _, text, choices) = card_sent(&ops).unwrap();
            assert_eq!(
                (text.as_str(), choices),
                (question, vec![choice.to_string()])
            );
            assert_eq!(installer(&c).unwrap().name, name, "{language:?}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// The press may come while the deck still reads the card out. The
    /// install waits for quiet, as `firmware.install` does (it pauses the
    /// deck's link), and starts on the first tick after the sentence ends.
    /// Meanwhile the status says the press was taken and what the install
    /// waits for, not to press ENTER again.
    #[test]
    fn a_press_while_the_card_is_read_out_starts_the_install_when_it_ends() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-reading");
        let started = checked(&mut c, firmware::testkit::deck());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        install(&mut c, t0).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();
        enter(&mut c, slot, card, after(t0, 1_500));
        assert!(installer(&c).is_none(), "the card is answered and gone");
        let waiting = firmware_status(&mut c, t0);
        assert_eq!(waiting["phase"], "confirming", "{waiting}");
        assert_eq!(waiting["message"], STARTS_WHEN_QUIET, "at the press");
        c.tick(after(t0, 2_000));
        assert!(
            started.lock().unwrap().is_empty(),
            "not while it is read out"
        );
        let waiting = firmware_status(&mut c, t0);
        assert_eq!(waiting["phase"], "confirming");
        assert_eq!(waiting["message"], STARTS_WHEN_QUIET, "and on every tick");
        said(&mut c, after(t0, 4_000));
        assert!(
            started.lock().unwrap().is_empty(),
            "the next tick starts it"
        );
        c.tick(after(t0, 4_250));
        assert_eq!(*started.lock().unwrap(), [firmware::testkit::install()]);
        assert_eq!(firmware_status(&mut c, t0)["phase"], "downloading");
        c.tick(after(t0, 4_500));
        assert_eq!(started.lock().unwrap().len(), 1, "once");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The owner's own words hold the install back too: a press held on
    /// another row, then its transcription.
    #[test]
    fn a_press_while_the_owner_talks_starts_the_install_once_it_is_quiet() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-talking");
        let started = checked(&mut c, firmware::testkit::deck());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        install(&mut c, t0).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();
        said(&mut c, after(t0, 500));
        let row = other_than(slot);
        talk(&mut c, row, 1, after(t0, 1_000));
        enter(&mut c, slot, card, after(t0, 1_500));
        c.tick(after(t0, 2_000));
        assert!(started.lock().unwrap().is_empty(), "not while PTT is held");
        assert_eq!(firmware_status(&mut c, t0)["message"], STARTS_WHEN_QUIET);
        talk(&mut c, row, 0, after(t0, 2_500));
        c.tick(after(t0, 3_000));
        assert!(
            started.lock().unwrap().is_empty(),
            "nor while it is transcribed"
        );
        let heard = VoiceIn::Final {
            text: "Ship it.".into(),
        };
        c.step(Command::Voice(heard), after(t0, 3_500));
        assert!(c.quiet(), "the words went to no agent, and nothing is said");
        c.tick(after(t0, 3_750));
        assert_eq!(*started.lock().unwrap(), [firmware::testkit::install()]);
        assert_eq!(firmware_status(&mut c, t0)["phase"], "downloading");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A press the voice never lets start — here a reading the engine
    /// never finishes — is given up a minute after it, with
    /// `firmware.install`'s own reason; quiet after that starts nothing.
    #[test]
    fn a_confirmed_install_the_voice_never_lets_start_is_given_up() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("care-install-never-quiet");
        let started = checked(&mut c, firmware::testkit::deck());
        let t0 = Instant::now();
        link(&mut c, None, t0);
        install(&mut c, t0).unwrap();
        let (slot, card, _, _) = card_sent(&ops).unwrap();
        enter(&mut c, slot, card, after(t0, 1_000));
        c.tick(after(t0, 60_999));
        assert_eq!(firmware_status(&mut c, t0)["phase"], "confirming");
        c.tick(after(t0, 61_000));
        let status = firmware_status(&mut c, t0);
        assert_eq!(status["phase"], "error", "{status}");
        assert_eq!(status["error"], firmware::VOICE_BUSY);
        said(&mut c, after(t0, 62_000));
        c.tick(after(t0, 62_250));
        assert!(started.lock().unwrap().is_empty(), "nothing starts late");
        assert_eq!(firmware_status(&mut c, t0)["phase"], "error");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
