//! Every input to the reducer, from every door (design §3).
use crate::{
    AgentId, AgentState, CardId, CardKind, Choice, Deliver, DeviceLine, FaceEntry, FocusSource,
    Kind, MacroKey, OrderSource, Origin, PidKind, Principal, SessionId, SetKey, Slot, Terminal,
    UsageItem, Window,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// `agent.card.set{free_text?:true}` (§4.6): dictated free text stays
/// available on a card unless the owner turns it off.
const fn free_text_default() -> bool {
    true
}

/// `agent.heartbeat` takes `{agent}` **or** `{agents:[…]}` (§4.6). Both land
/// in the one field, so the reducer and every caller see a list.
fn agent_or_agents<'de, D>(d: D) -> Result<Vec<AgentId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(AgentId),
        Many(Vec<AgentId>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(agent) => vec![agent],
        OneOrMany::Many(agents) => agents,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRegister {
    pub kind: Kind,
    pub name: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// What `pid` is (§5): the hook helper's parent walk resolves `cli` for
    /// the CLI process itself, `shell` for the `sh -c` in between, and
    /// `unknown` when no CLI ancestor was found. `kill(pid, 0)` liveness
    /// polling runs **only** for `cli`, so an unset value is never polled.
    #[serde(default)]
    pub pid_kind: Option<PidKind>,
    #[serde(default)]
    pub terminal: Option<Terminal>,
    /// `lease_ttl_s:30` in §4.6 is the daemon's default, not the caller's
    /// duty: 0 (the absent value) takes `[agents] lease_ttl_s`.
    #[serde(default)]
    pub lease_ttl_s: u32,
    pub caps: crate::Caps,
    #[serde(default)]
    pub slot_hint: Option<Slot>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentUpdate {
    pub agent: AgentId,
    pub state: Option<AgentState>,
    pub title: Option<String>,
    pub title_source: Option<crate::TitleSource>,
    pub since: Option<crate::Ts>,
    pub name: Option<String>,
    pub meta: Option<serde_json::Value>,
}

/// `agent.card.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CardSet {
    pub agent: AgentId,
    /// Re-push under an existing id (same-id content update, no new chime).
    pub id: Option<CardId>,
    #[serde(rename = "type")]
    pub kind: CardKind,
    pub text: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub rec: Option<u8>,
    #[serde(default = "free_text_default")]
    pub free_text: bool,
    pub deliver: Deliver,
    #[serde(default)]
    pub timeout_s: Option<u32>,
    /// §4.6: a card from any token or script is `custom` (prefixed `» ` on
    /// the deck) unless it names its own origin.
    #[serde(default = "Origin::custom")]
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CardClear {
    pub agent: AgentId,
    pub card: CardId,
    pub delivered: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReplySet {
    pub agent: AgentId,
    pub text: String,
    /// A stable provider item id updates one message as it streams.
    #[serde(default)]
    pub message: Option<crate::MessageMeta>,
    #[serde(default)]
    pub speak: bool,
    #[serde(default)]
    pub announce: Option<String>,
    #[serde(default)]
    pub lane: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpAnswer {
    pub agent: AgentId,
    pub card: CardId,
    #[serde(default)]
    pub choice: Option<u8>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpOrder {
    pub agent: AgentId,
    pub text: String,
    pub ctx: String,
    pub source: OrderSource,
    /// The PTT utterance this order was dictated from (B12).
    ///
    /// §4.12 gives `agent.order` an `utterance?` and [`crate::Order`] has
    /// carried the field since week 1, but nothing could ever fill either:
    /// the one request that produces an order had nowhere to put the id, so
    /// a client that wanted to line an order up against the
    /// `transcript.final` it came from had only the text to match on — and
    /// the text is exactly what the §5.6 grammar may have rewritten.
    ///
    /// Absent for anything typed (`source:"gui"`, `"api"`); `default` so no
    /// existing caller has to send it.
    #[serde(default)]
    pub utterance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FocusReport {
    pub agent: Option<AgentId>,
    pub source: FocusSource,
}

/// `voice.say` (§4.9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VoiceSay {
    pub text: String,
    #[serde(default)]
    pub agent: Option<AgentId>,
    pub lane: String,
    #[serde(default)]
    pub voice: Option<String>,
    /// §4.9: cannot pre-empt a higher lane; the mixer decides.
    #[serde(default)]
    pub interrupt: bool,
    /// §4.9's `ttl_s?:30` — `chatter` is dropped if it has not started by
    /// then. Carried to the mixer rather than dropped here: a caller that
    /// asked for a deadline and silently did not get one is worse than one
    /// that was refused.
    #[serde(default)]
    pub ttl_s: Option<u32>,
}

/// `voice.listen` (§4.9): the GUI/API entry to the same §7.4 machine the
/// deck's `ptt` edges drive — with **the caller's** principal and session
/// on everything it derives, never `device` (B2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VoiceListen {
    pub agent: AgentId,
    /// `"talk"` or `"answer"`. A string and not a [`crate::PttCtx`] because
    /// §4.9 admits exactly those two: `name` is a device edge and an API
    /// caller may not synthesise it.
    #[serde(default = "talk_ctx")]
    pub ctx: String,
    #[serde(default)]
    pub card: Option<CardId>,
    #[serde(default = "listen_max_s")]
    pub max_s: u32,
    /// `"deck"` (default) or `"mac"` — §4.9's `input`.
    #[serde(default)]
    pub input: crate::VoiceInput,
}

fn talk_ctx() -> String {
    "talk".to_string()
}

const fn listen_max_s() -> u32 {
    60
}

/// §4.10 names the two fields `k` and `v`, like the `set` line on the wire
/// and the `device.set` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceSet {
    #[serde(rename = "k")]
    pub key: SetKey,
    #[serde(rename = "v")]
    pub value: u16,
}

/// The API methods the reducer handles (§4.5–§4.10). Transport-level
/// methods (`session.*`, `state.*`, admin) never reach the reducer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    #[serde(rename = "agent.register")]
    AgentRegister(AgentRegister),
    #[serde(rename = "agent.heartbeat")]
    AgentHeartbeat {
        #[serde(alias = "agent", deserialize_with = "agent_or_agents")]
        agents: Vec<AgentId>,
    },
    #[serde(rename = "agent.update")]
    AgentUpdate(AgentUpdate),
    #[serde(rename = "agent.companion.set")]
    CompanionSet(crate::companion::CompanionSet),
    #[serde(rename = "agent.session.set")]
    SessionSet(crate::session_controls::SessionSet),
    #[serde(rename = "agent.release")]
    AgentRelease {
        agent: AgentId,
        reason: Option<String>,
    },
    #[serde(rename = "agent.claim")]
    AgentClaim { key: String },
    #[serde(rename = "agent.focus")]
    AgentFocus { agent: AgentId },
    #[serde(rename = "agent.card.set")]
    CardSet(CardSet),
    #[serde(rename = "agent.card.clear")]
    CardClear(CardClear),
    #[serde(rename = "agent.reply")]
    Reply(ReplySet),
    #[serde(rename = "op.answer")]
    OpAnswer(OpAnswer),
    #[serde(rename = "op.order")]
    OpOrder(OpOrder),
    #[serde(rename = "op.focus")]
    OpFocus { agent: AgentId },
    #[serde(rename = "op.snooze")]
    OpSnooze { agent: AgentId, card: CardId },
    #[serde(rename = "op.ack")]
    OpAck { agent: AgentId, card: CardId },
    #[serde(rename = "focus.report")]
    FocusReport(FocusReport),
    #[serde(rename = "device.set")]
    DeviceSet(DeviceSet),
    #[serde(rename = "device.faces.set")]
    DeviceFacesSet { list: Vec<FaceEntry> },
    #[serde(rename = "device.usage.set")]
    DeviceUsageSet {
        period: String,
        items: Vec<UsageItem>,
    },
    #[serde(rename = "device.macro.set")]
    DeviceMacroSet {
        f: u8,
        layout: String,
        src: Option<String>,
        keys: Vec<MacroKey>,
    },
    #[serde(rename = "device.ws.set")]
    DeviceWsSet {
        active: u8,
        count: u8,
        sel: u8,
        windows: Vec<Window>,
    },
    #[serde(rename = "device.name")]
    DeviceName { text: String },
    // B5: the four voice methods that **change something**. They were dead
    // ends answered `-32013` from outside the reducer, so `voice.say` could
    // not allocate an utterance or set `Speaking` and `voice.cancel` could
    // not reach `abort_ptt` — a client could hold the mic open and had no
    // way to let go. `voice.engines|test|get` stay core-answered: they read
    // the engines and the snapshot and change nothing.
    #[serde(rename = "voice.say")]
    VoiceSay(VoiceSay),
    #[serde(rename = "voice.stop")]
    VoiceStop {
        #[serde(default)]
        utterance: Option<String>,
        #[serde(default)]
        lane: Option<String>,
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "voice.listen")]
    VoiceListen(VoiceListen),
    #[serde(rename = "voice.cancel")]
    VoiceCancel {},
}

/// From the hid thread (§6.2, §6.3): what happened to the link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "link", rename_all = "snake_case")]
pub enum LinkEvent {
    /// The device was opened; `hello` is on its way.
    Linked,
    /// Read/write error, removal, or 6 hello misses: handle dropped.
    Down,
    /// 3 hello misses; handle kept.
    Degraded,
    /// A hello reply with our sid: the session is live.
    Live { sid: u32 },
    /// A hello reply with a foreign sid: a barrier is being written.
    Syncing { sid: u32, boot: bool },
    /// The barrier's `synced` was written.
    Synced { sid: u32 },
    /// The core→hid channel overflowed; the core re-projects.
    Overflow,
    /// Two drops after bursts: pace raised (ms).
    PaceBackoff { pace_ms: u16 },
}

/// What the audio side reports to the reducer (§7; B1).
///
/// Before this there was **no inbound path at all**: `Command` had no voice
/// variant and `AudioCmd::Finish` returned nothing, so a partial, a final
/// transcript, a route change, a level and a permission grant had nowhere
/// to land. The whole of §7.4 downstream of "the mic is open" — the
/// transcript on the deck, the dispatch by `ctx`, `voice.route`,
/// `voice.level`, `perm.changed` — was unreachable.
///
/// It mirrors [`LinkEvent`] deliberately: one enum, carried by one channel
/// into the core, applied by the reducer like any other command. That is
/// what makes the §15 week-4 exit tests possible against a **fake** engine
/// — a test feeds these by hand and never opens a microphone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "voice", rename_all = "snake_case")]
pub enum VoiceIn {
    /// An interim transcript. Keep-latest and paced (§7.4: ≤4/s, last
    /// 120 B) — the pacing is the outbox's (B16), not the engine's.
    Partial { text: String },
    /// The final transcript, which is what the session is dispatched on.
    /// Empty (nothing was said, or the recogniser heard only silence) is a
    /// normal outcome and not an error.
    Final { text: String },
    /// A released session failed to produce a final. Consumes the same
    /// ordered session slot as `Final`, without dispatching any text.
    FinalFailed { code: String, message: String },
    /// The route state machine of §7.2 moved.
    Route {
        route: crate::Route,
        #[serde(default)]
        output_uid: Option<String>,
        #[serde(default)]
        input_uid: Option<String>,
        /// §7.2: two failed reopens; recovers on the next rate change.
        #[serde(default)]
        stuck: bool,
        /// §7.5.4: how long this press waited for its first **non-zero**
        /// sample, when that was longer than `[voice] slow_sco_ms`.
        ///
        /// `None` on every other route report, including the fast ones —
        /// this is an exception report, not a metric. It is keyed off the
        /// first non-zero sample and not the first callback (V2), because
        /// the callbacks start at ~28 ms on a microphone that is not there.
        #[serde(default)]
        slow_sco_ms: Option<u32>,
    },
    /// **The first non-zero sample of this capture** — §7.4's `Arming →
    /// Listening` edge and the only honest liveness signal there is (V2).
    ///
    /// It is deliberately not "a buffer arrived". Measured on the deck
    /// (2026-09-21, n=16): the input callback starts at ~28 ms,
    /// `AudioUnitRender` returns `noErr`, and **every sample is an exact
    /// digital zero for about a second** — 1031 ms median to the first
    /// non-zero one (815–1246). A dead microphone looks exactly like that
    /// for the whole of the first second, so §7.4's "1.5 s without samples
    /// → mic unavailable" keyed off buffers could never fire on a genuinely
    /// dead mic, and keyed off the first non-zero sample it fires on
    /// nothing else.
    ///
    /// The engine sends it once per session; a second one is ignored.
    Live,
    /// The capture RMS meter (§7.1), the `voice.level` event's payload.
    /// Opt-in on the bus, because it is per-buffer.
    ///
    /// **Not a liveness signal**: an `rms` of 0.0 is what a dead mic and a
    /// warming SCO link both report, which is the whole of V2.
    Level { rms: f32 },
    /// The TCC state as the daemon last saw it (§7.8). A **denied** grant
    /// is silence and a `perm.changed`, never an error. Microphone and
    /// speech only: `bluetooth` is the BLE link's own report.
    Permissions(crate::Permissions),
    /// CoreBluetooth's authorization, as the BLE voice link read it when
    /// the radio last changed state.
    Bluetooth { permission: crate::Permission },
    /// The transport the engine was built for, reported once when it is.
    Transport { transport: crate::VoiceTransport },
    /// The engine reached a phase of an utterance it was asked to speak
    /// (B6's remainder).
    ///
    /// The store already emits `voice.utterance{phase:"queued"}` when it
    /// hands a `Say` to the engine and `{phase:"cut"}` when it takes one
    /// back, and those are the two phases the store can know on its own.
    /// **`started` and `ended` are facts only the engine has**: `queued`
    /// says a sentence was asked for, and on a route that spends 2.3 s
    /// reopening after every press (V1) that is a long way from "the
    /// operator heard it". Without these, `speaking` could only ever be
    /// cleared by the next `Say` or by a `Stop`.
    Utterance {
        utterance: String,
        phase: UtterancePhase,
    },
    /// An engine or device error, mid-session or outside one
    /// (`kAFAssistantErrorDomain 1110`, a dead mic, a refused
    /// `AudioCmd::Listen`). It ends a live session; the code is what the
    /// GUI shows.
    Error {
        code: String,
        #[serde(default)]
        message: Option<String>,
    },
}

/// How far the engine has got with one utterance (§4.12's
/// `voice.utterance{phase}`).
///
/// `queued` and `cut` are the store's own and are not here: an engine
/// cannot report a phase of something it was never given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UtterancePhase {
    /// The first sample is queued on its lane. On this hardware that is the
    /// earliest honest "it is being said".
    Started,
    /// The last sample is queued — or the utterance was cut or abandoned,
    /// which is the same fact for a reader waiting to know the lane is free.
    Ended,
}

/// Every input, from every door.
#[derive(Clone, Debug)]
pub enum Command {
    SystemStatus(deck_proto::SystemStatus),
    SessionControls { agent: AgentId, session: SessionId, settled: bool, controls: crate::session_controls::SessionControls },
    Api {
        principal: Principal,
        session: SessionId,
        /// Boxed: a `Request` is by far the largest thing a `Command` can
        /// carry, and every device line and tick would pay for it.
        req: Box<Request>,
    },
    // A held hook is **not** a `Command`: `apply` has nowhere to put a
    // result, and the core needs the card id the store allocated and the
    // store's own refusal (B4). It goes through `Store::hold`, the way
    // `Command::Api` has `Store::call`.
    /// The parked response was written. Holds are keyed by (agent, card):
    /// card ids are per slot (§6.5), so two agents routinely share one.
    HoldWritten {
        agent: AgentId,
        card: CardId,
    },
    /// The parked future was dropped (client timeout, Ctrl-C).
    HoldGone {
        agent: AgentId,
        card: CardId,
    },
    Device(DeviceLine),
    Link(LinkEvent),
    /// From the audio side (B1), the way `Device`/`Link` are from the hid
    /// thread.
    Voice(VoiceIn),
    /// Result of an `Effect::Deliver`.
    Delivered {
        agent: AgentId,
        card: CardId,
        ok: bool,
    },
    /// Result of an `Effect::Desktop(DesktopCmd::Inject)` — the injector
    /// chain of §5.5, which runs off the core thread because every step of
    /// it is a ~118 ms AppleScript round trip.
    ///
    /// It has to be a command rather than a return value: `op.order`
    /// answers `{via:"inject"}` and writes `order.dispatched{text}`
    /// **before** anything is typed (§5.5), so the refusal — guard 3's
    /// `not_foreground`, an `ambiguous` resolution, a `-1743` — necessarily
    /// arrives later, and only the reducer can put `order.failed` on the
    /// bus with a `rev` the mirror will accept. `ok` writes nothing: the
    /// dispatch already told that story.
    OrderResult {
        principal: Principal,
        agent: AgentId,
        text: String,
        ok: bool,
        /// The `order.failed{reason}` word (§4.12): `ambiguous`,
        /// `not_foreground`, `dialog_open`, or what the desktop side named.
        reason: Option<String>,
    },
    /// A guarded operation was refused to an untrusted `principal` (§4.2):
    /// the store emits `op.confirm_requested` so the GUI can show its sheet.
    ConfirmRequested {
        principal: Principal,
        agent: AgentId,
        card: Option<CardId>,
        order: Option<String>,
    },
    SessionClosed(SessionId),
    /// The TCP door reports its bind (§4.1): the port it listens on (`None`
    /// while it could not bind) and the degradation reason, if any
    /// (`"port_in_use"`, `"hooks_hijack_risk"`; `None` clears it).
    ServiceBound {
        port: Option<u16>,
        degraded: Option<String>,
        /// Who holds the port while degraded (§4.1: `lsof`), `None` on a
        /// clean bind — the event and `service.info` carry it.
        listener: Option<crate::Listener>,
    },
    Tick(Instant),
    /// Orderly shutdown: release every hold, mark the device offline.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_their_method_names() {
        let req = Request::OpSnooze {
            agent: AgentId("ag_1".into()),
            card: CardId(3),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["method"], "op.snooze");
        assert_eq!(json["params"]["card"], 3);
        let back: Request = serde_json::from_value(json).unwrap();
        assert_eq!(back, req);
        let set: Request = serde_json::from_str(
            r#"{"method":"agent.card.set","params":{"agent":"ag_1","id":null,"type":"q","text":"t","choices":[{"label":"a","detail":null}],"rec":0,"free_text":true,"deliver":"poll","timeout_s":null,"origin":{"kind":"custom","tool":null,"request_id":null,"group":null}}}"#,
        )
        .unwrap();
        assert!(matches!(
            set,
            Request::CardSet(CardSet {
                deliver: Deliver::Poll,
                ..
            })
        ));
    }

    /// What a door does with a call: §3.2 / `api::dispatch` builds
    /// `{"method":…,"params":…}` and lets serde pick the `Request`.
    fn posted(method: &str, params: serde_json::Value) -> Result<Request, String> {
        serde_json::from_value(serde_json::json!({"method": method, "params": params}))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn the_designs_minimal_documented_calls_are_accepted() {
        use crate::testkit::{Sim, UID};
        use crate::{AnswerCap, CardKind, Deliver, OrderCap, OriginKind, SessionId, SetKey};

        // §4.15 step 1, verbatim: the whole third-party registration.
        let req = posted(
            "agent.register",
            serde_json::json!({"kind":"other","name":"myapp","key":"myapp:42",
                               "caps":{"answer":"poll","order":"poll"},"lease_ttl_s":30}),
        )
        .expect("the §4.15 one-liner registration");
        let Request::AgentRegister(r) = req.clone() else {
            panic!("{req:?}")
        };
        assert_eq!(r.caps.answer, AnswerCap::Poll);
        assert_eq!(r.caps.order, OrderCap::Poll);
        assert!(!r.caps.focus, "an unspoken cap is not granted");
        assert_eq!(r.lease_ttl_s, 30);
        assert_eq!(
            (r.cwd, r.pid, r.terminal, r.slot_hint, r.voice, r.meta),
            (None, None, None, None, None, None)
        );
        // …and with `lease_ttl_s` left to the daemon (§4.6 `lease_ttl_s:30`).
        let req = posted(
            "agent.register",
            serde_json::json!({"kind":"other","name":"bot","caps":{"answer":"poll","order":"poll"}}),
        )
        .expect("a registration without lease_ttl_s");
        let mut sim = Sim::new();
        let res = sim.api(UID, SessionId(1), req).expect("register");
        let id = crate::AgentId(res["agent"].as_str().expect("agent id").into());
        assert!(
            sim.store.snapshot().agents[&id]
                .owner
                .lease_expires
                .is_some(),
            "0 takes the configured lease, it is not an immediate expiry"
        );

        // §4.15 step 2, verbatim: a question card and a Ready card.
        let req = posted(
            "agent.card.set",
            serde_json::json!({"agent": id, "type":"q", "text":"Run migrations?",
                               "choices":[{"label":"Yes"},{"label":"No"}], "deliver":"poll"}),
        )
        .expect("the §4.6 minimal card.set");
        let Request::CardSet(ref c) = req else {
            panic!("{req:?}")
        };
        assert_eq!(c.kind, CardKind::Q);
        assert_eq!(c.deliver, Deliver::Poll);
        assert_eq!(c.choices.len(), 2, "a choice needs a label, nothing else");
        assert_eq!(c.choices[0].detail, None);
        assert!(c.free_text, "§4.6 `free_text?:true`");
        assert_eq!(c.origin.kind, OriginKind::Custom, "§4.6: a script's card");
        assert_eq!((c.id, c.rec, c.timeout_s), (None, None, None));
        sim.api(UID, SessionId(1), req).expect("card.set");
        let card = sim.store.snapshot().agents[&id].card.clone().expect("card");
        assert_eq!(card.choices_n, 2);
        let req = posted(
            "agent.card.set",
            serde_json::json!({"agent": id, "type":"r", "text":"Tests pass",
                               "choices":[{"label":"Deploy"}], "deliver":"poll"}),
        )
        .expect("the §4.15 Ready card");
        sim.api(UID, SessionId(1), req).expect("ready card.set");

        // §4.6: `agent.reply{text}` — `speak` is optional.
        let req = posted(
            "agent.reply",
            serde_json::json!({"agent": id, "text":"done"}),
        )
        .expect("agent.reply without speak");
        let Request::Reply(ref reply) = req else {
            panic!("{req:?}")
        };
        assert!(!reply.speak);
        sim.api(UID, SessionId(1), req).expect("reply");

        // §4.6: `agent.heartbeat` takes `{agent}` as well as `{agents:[…]}`.
        let one = posted("agent.heartbeat", serde_json::json!({"agent": id}))
            .expect("agent.heartbeat{agent}");
        assert_eq!(
            one,
            Request::AgentHeartbeat {
                agents: vec![id.clone()]
            }
        );
        assert_eq!(
            posted(
                "agent.heartbeat",
                serde_json::json!({"agents": [id.clone()]})
            )
            .unwrap(),
            one
        );
        sim.api(UID, SessionId(1), one).expect("heartbeat");

        // §4.10: `device.set{k, v}`.
        let req = posted("device.set", serde_json::json!({"k":"brightness","v":3}))
            .expect("device.set{k,v}");
        assert_eq!(
            req,
            Request::DeviceSet(DeviceSet {
                key: SetKey::Brightness,
                value: 3
            })
        );
        sim.api(UID, SessionId(1), req).expect("device.set");
        assert_eq!(sim.store.snapshot().device.settings.brightness, 3);

        // The validation rules the defaults must not loosen (§4.6).
        for (method, params, field) in [
            (
                "agent.register",
                serde_json::json!({"kind":"other","name":"x","caps":{"answer":"hold","order":"poll"}}),
                "caps.answer",
            ),
            (
                "agent.register",
                serde_json::json!({"kind":"other","name":"x","caps":{"answer":"poll","order":"inject"}}),
                "caps.order",
            ),
            (
                "agent.card.set",
                serde_json::json!({"agent": id, "type":"q", "text":"t", "deliver":"hold"}),
                "deliver",
            ),
            (
                "agent.card.set",
                serde_json::json!({"agent": id, "type":"q", "text":"t", "deliver":"poll",
                                   "origin":{"kind":"permission"}}),
                "origin.kind",
            ),
        ] {
            let req = posted(method, params).expect("well-formed");
            let err = sim.api(UID, SessionId(1), req).expect_err(field);
            assert_eq!(err.code, crate::ApiError::INVALID, "{field}");
            assert_eq!(err.data["field"], field);
        }
        // `name` is not optional, and neither is the card's `deliver`.
        assert!(posted(
            "agent.register",
            serde_json::json!({"kind":"other","caps":{"answer":"none","order":"none"}})
        )
        .is_err());
        assert!(posted(
            "agent.card.set",
            serde_json::json!({"agent": id, "type":"q", "text":"t"})
        )
        .is_err());
    }

    #[test]
    fn link_events_tag_themselves() {
        let json = serde_json::to_value(LinkEvent::Syncing { sid: 5, boot: true }).unwrap();
        assert_eq!(json["link"], "syncing");
        assert_eq!(json["boot"], true);
    }
}
