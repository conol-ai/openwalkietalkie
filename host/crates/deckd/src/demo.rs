//! The mockup roster, verbatim from `tools/deck-sim.py` (`src/deck/demo.rs`
//! is the firmware's copy). `deckd device dry-run` frames it exactly as the
//! reference host does; `deckd run --demo` registers it through the store's
//! API as an in-process virtual-agent client and answers the deck the way
//! the reference host does: an `answer` puts the agent to work, `refresh`
//! nudges the usage bars, a PTT release types a transcript and a reply, a
//! face-naming PTT names the face "Deploy". After the roster it registers
//! the design's §4.13 example agent (`cohdl-api`, contract §4) with a
//! **held** permission card the demo itself parks, so the trust refusal and
//! the hold path can be tried live: a resolution is logged, treated as
//! written, and the card is set again 3 s later. The dry-run parity frames
//! `roster()`, which the example agent never joins.
use crate::core::handle::{HoldResult, HoldRx};
use deck_model::{
    AdapterKind, AgentId, AgentRegister, AgentUpdate, Answer, AnswerCap, Caps, CardId, CardSet,
    Choice, Control, Deliver, DeviceOp, Event, EventKind, OrderCap, Origin, OriginKind, Principal,
    PttCtx, Release, ReplySet, Request, SessionId, Slot, TitleSource, Ts,
};
use deck_proto::{
    AgentState, Binding, CardKind, FaceEntry, HostLine, Kind, MacroKey, Template, Tone, UsageItem,
    Window,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::sync::oneshot::error::TryRecvError;

pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The demo's session: an internal client that never closes, so its agents
/// carry no lease (like an adapter's) and may use the question/ready origins
/// (no `» ` prefix on the deck).
pub const SESSION: SessionId = SessionId(1);

pub fn principal() -> Principal {
    Principal::Adapter(AdapterKind::Virtual)
}

/// The §4.13 example agent's slot, principal and card (contract §4).
pub const PERMISSION_SLOT: u8 = 6;
pub const PERMISSION_TEXT: &str = "Bash: psql -f migrate.sql staging";
pub const PERMISSION_CHOICES: [&str; 3] = ["Allow", "Allow always", "Deny"];
/// The permission card comes back this long after a resolution.
pub const REPUSH_AFTER: Duration = Duration::from_secs(3);

/// The example agent is owned by the Claude adapter (a token never owns a
/// permission card), on the demo's session.
pub fn permission_principal() -> Principal {
    Principal::Adapter(AdapterKind::Claude)
}

/// `cohdl-api`, `kind: claude`, `slot_hint 6`, hold/inject caps.
pub fn permission_register() -> AgentRegister {
    AgentRegister {
        kind: Kind::Claude,
        name: "cohdl-api".into(),
        key: Some("demo:claude:cohdl-api".into()),
        cwd: Some(format!("{PROJECT_ROOT}/cohdl")),
        pid: None,
        pid_kind: None,
        terminal: None,
        lease_ttl_s: 0,
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: false,
            transcribe: Default::default(),
        },
        slot_hint: Some(Slot(PERMISSION_SLOT)),
        voice: None,
        meta: None,
    }
}

/// State `question`, title "Run migrations on staging?" (`title_source: prompt`).
pub fn permission_update(id: &AgentId) -> AgentUpdate {
    AgentUpdate {
        agent: id.clone(),
        state: Some(AgentState::Question),
        title: Some("Run migrations on staging?".into()),
        title_source: Some(TitleSource::Prompt),
        since: None,
        name: None,
        meta: None, task: None, subagents: None,
    }
}

/// The held permission card: `Bash: psql -f migrate.sql staging`, Allow /
/// Allow always / Deny, `rec: 0`, origin permission/Bash/toolu_9.
pub fn permission_card(id: &AgentId) -> CardSet {
    CardSet {
        agent: id.clone(),
        id: None,
        kind: CardKind::Q,
        text: PERMISSION_TEXT.into(),
        choices: PERMISSION_CHOICES
            .iter()
            .map(|label| Choice {
                label: (*label).into(),
                detail: None,
            })
            .collect(),
        rec: Some(0),
        free_text: true,
        deliver: Deliver::Hold,
        timeout_s: None,
        origin: Origin {
            kind: OriginKind::Permission,
            tool: Some("Bash".into()),
            request_id: Some("toolu_9".into()),
            group: None,
        },
    }
}

/// How the demo's held card was resolved.
#[derive(Debug)]
pub enum HoldOutcome {
    /// `allow` / `deny` (by the chosen label), treated as written.
    Answer {
        card: CardId,
        answer: Answer,
    },
    Released {
        card: CardId,
        reason: Release,
    },
    /// The core forgot the hold (a `HoldGone` for it).
    Gone {
        card: CardId,
    },
}

impl HoldOutcome {
    pub fn card(&self) -> CardId {
        match self {
            HoldOutcome::Answer { card, .. }
            | HoldOutcome::Released { card, .. }
            | HoldOutcome::Gone { card } => *card,
        }
    }

    /// `allow` / `deny` / `released` for the log line.
    pub fn verdict(&self) -> &'static str {
        match self {
            HoldOutcome::Answer { answer, .. } => {
                let label = answer.label.as_deref().unwrap_or_default();
                if deck_model::guard::label_is_allow(label)
                    || (answer.label.is_none()
                        && answer
                            .text
                            .as_deref()
                            .is_some_and(|t| !deck_model::guard::text_is_deny(t)))
                {
                    "allow"
                } else {
                    "deny"
                }
            }
            HoldOutcome::Released { .. } | HoldOutcome::Gone { .. } => "released",
        }
    }
}

/// Where the demo's sessions "run": the project of each slot is the last
/// component, which is what the device's `proj` and the GUI's groups show.
pub const PROJECT_ROOT: &str = "/Users/demo/projects";

#[allow(clippy::too_many_arguments)]
fn agent(
    slot: u8,
    kind: Kind,
    name: &str,
    project: &str,
    state: AgentState,
    title: &str,
    since_s: Option<u32>,
) -> HostLine {
    HostLine::Agent {
        slot,
        kind: Some(kind),
        name: Some(name.into()),
        project: Some(project.into()),
        state: Some(state),
        title: Some(title.into()),
        since_s,
        task: None,
        subs: None,
    }
}

fn reply(slot: u8, text: &str) -> HostLine {
    HostLine::Reply {
        slot,
        text: text.into(),
        rev: 1,
    }
}

/// The roster lines in the reference host's order.
pub fn roster() -> Vec<HostLine> {
    vec![
        agent(
            1,
            Kind::Codex,
            "cohdl-api",
            "cohdl",
            AgentState::Question,
            "Run migrations on staging?",
            Some(120),
        ),
        HostLine::Card {
            slot: 1,
            id: 1,
            kind: CardKind::Q,
            text: "Tests fail on main. Run pending migrations on staging first?".into(),
            choices: vec![
                "Run on staging".into(),
                "Skip migrations".into(),
                "Abort task".into(),
            ],
            rec: Some(0),
        },
        reply(
            1,
            "Tests fail on main: 3 pending migrations are not applied on staging.",
        ),
        agent(
            2,
            Kind::Claude,
            "web-ui",
            "cohdl",
            AgentState::Ready,
            "Header refactor done · 34 files",
            None,
        ),
        HostLine::Card {
            slot: 2,
            id: 2,
            kind: CardKind::R,
            text: "Header refactor done · 34 files".into(),
            choices: vec!["Run the test suite".into(), "Open a pull request".into()],
            rec: None,
        },
        reply(2, "Header refactor done, 34 files changed"),
        agent(
            3,
            Kind::Codex,
            "firmware",
            "cohdl",
            AgentState::Working,
            "Compiling openmicro",
            Some(120),
        ),
        reply(3, "Building the native image for both boards."),
        agent(4, Kind::Gemini, "research", "openmicro", AgentState::Idle, "Idle", None),
        agent(
            5,
            Kind::Claude,
            "infra",
            "openmicro",
            AgentState::Error,
            "Deploy failed · exit 1",
            None,
        ),
        reply(5, "terraform apply exited with status 1."),
        HostLine::Ws {
            active: 2,
            count: 4,
            sel: 0,
            windows: vec![
                Window {
                    tone: Some(Tone::Blue),
                    title: "term · claude — header refactor".into(),
                },
                Window {
                    tone: Some(Tone::Amber),
                    title: "firefox — github · pulls".into(),
                },
                Window {
                    tone: Some(Tone::Green),
                    title: "term · codex — compiling...".into(),
                },
            ],
        },
        HostLine::Macro {
            face: 3,
            src: Some("deck.toml".into()),
            keys: vec![
                MacroKey {
                    label: "build".into(),
                    binding: Some(Binding::Left),
                    keys: None,
                },
                MacroKey {
                    label: "test".into(),
                    binding: Some(Binding::Right),
                    keys: None,
                },
                MacroKey {
                    label: "deploy".into(),
                    binding: Some(Binding::Enter),
                    keys: None,
                },
                MacroKey {
                    label: "git push".into(),
                    binding: Some(Binding::ShiftLeft),
                    keys: None,
                },
                MacroKey {
                    label: "screenshot".into(),
                    binding: None,
                    keys: None,
                },
                MacroKey {
                    label: "lock".into(),
                    binding: None,
                    keys: None,
                },
                MacroKey {
                    label: "mute".into(),
                    binding: Some(Binding::ShiftRight),
                    keys: None,
                },
                MacroKey {
                    label: "term".into(),
                    binding: Some(Binding::ShiftEnter),
                    keys: None,
                },
            ],
        },
        usage(0),
    ]
}

/// The usage line; `nudge` is added to every bar (the `refresh` demo).
pub fn usage(nudge: u8) -> HostLine {
    let item = |name: &str, percent: u8, reset: &str| UsageItem {
        reset_tip: None,
        name: name.into(),
        account: None,
        percent: percent.saturating_add(nudge).min(100),
        reset: reset.into(),
        tone: None,
    };
    HostLine::Usage {
        period: "SEP 2026".into(),
        items: vec![
            item("Claude Pro", 62, "resets in 4 days"),
            item("Codex · OpenAI", 81, "resets tomorrow"),
            item("Gemini", 12, "resets in 11 days"),
        ],
    }
}

pub fn default_faces() -> Vec<FaceEntry> {
    let face = |f: u8, t: Template, n: &str| FaceEntry { f, t, n: n.into() };
    vec![
        face(1, Template::Agents, "Agents"),
        face(2, Template::Omarchy, "Omarchy"),
        face(3, Template::Macropad, "Macropad"),
        face(4, Template::Usage, "Usage"),
        face(7, Template::System, "System"),
        face(6, Template::Arcade, "Arcade"),
        face(5, Template::Settings, "Settings"),
    ]
}

/// The `i`-th spinner title of the flood.
pub fn flood_title(i: usize) -> HostLine {
    HostLine::Agent {
        slot: 3,
        kind: None,
        name: None,
        project: None,
        state: None,
        title: Some(format!(
            "Compiling openmicro {} {i}",
            SPINNER[i % SPINNER.len()]
        )),
        since_s: None,
        task: None,
        subs: None,
    }
}

/// One mockup agent as the API calls that put it on the deck: register,
/// then its state and title, its card, its reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    pub slot: u8,
    pub kind: Kind,
    pub name: String,
    /// The project the session runs in (`PROJECT_ROOT/<project>` is its cwd).
    pub project: String,
    pub state: AgentState,
    pub title: String,
    pub since_s: Option<u32>,
    pub card: Option<ScriptCard>,
    pub reply: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptCard {
    pub kind: CardKind,
    pub text: String,
    pub choices: Vec<String>,
    pub rec: Option<u8>,
}

impl Script {
    pub fn register(&self) -> AgentRegister {
        AgentRegister {
            kind: self.kind,
            name: self.name.clone(),
            key: Some(format!("demo:{}", self.name)),
            cwd: (!self.project.is_empty())
                .then(|| format!("{PROJECT_ROOT}/{}", self.project)),
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
            slot_hint: Some(Slot(self.slot)),
            voice: None,
            meta: None,
        }
    }

    /// State and title; `since` is backdated so the deck shows the mockup's age.
    pub fn update(&self, id: &AgentId, now: Ts) -> AgentUpdate {
        let age_ms = u64::from(self.since_s.unwrap_or(0)) * 1000;
        AgentUpdate {
            agent: id.clone(),
            state: Some(self.state),
            title: Some(self.title.clone()),
            title_source: Some(TitleSource::Api),
            since: Some(Ts::from_millis(now.millis().saturating_sub(age_ms))),
            name: None,
            meta: None, task: None, subagents: None,
        }
    }

    pub fn card(&self, id: &AgentId) -> Option<CardSet> {
        let card = self.card.as_ref()?;
        let kind = match card.kind {
            CardKind::Q => OriginKind::Question,
            CardKind::R => OriginKind::Ready,
        };
        Some(CardSet {
            agent: id.clone(),
            id: None,
            kind: card.kind,
            text: card.text.clone(),
            choices: card
                .choices
                .iter()
                .map(|label| Choice {
                    label: label.clone(),
                    detail: None,
                })
                .collect(),
            rec: card.rec,
            free_text: false,
            deliver: deck_model::Deliver::Poll,
            timeout_s: None,
            origin: Origin {
                kind,
                tool: None,
                request_id: None,
                group: None,
            },
        })
    }

    pub fn reply(&self, id: &AgentId) -> Option<ReplySet> {
        Some(ReplySet {
            message: None,
            agent: id.clone(),
            text: self.reply.clone()?,
            speak: false,
            announce: None,
            lane: None,
        })
    }
}

/// The roster's agents as scripts, in slot order.
pub fn scripts() -> Vec<Script> {
    let mut scripts: Vec<Script> = Vec::new();
    for line in roster() {
        match line {
            HostLine::Agent {
                slot,
                kind,
                name,
                project,
                state,
                title,
                since_s,
                ..
            } => scripts.push(Script {
                slot,
                kind: kind.unwrap_or(Kind::Other),
                name: name.unwrap_or_default(),
                project: project.unwrap_or_default(),
                state: state.unwrap_or(AgentState::Idle),
                title: title.unwrap_or_default(),
                since_s,
                card: None,
                reply: None,
            }),
            HostLine::Card {
                slot,
                kind,
                text,
                choices,
                rec,
                ..
            } => {
                if let Some(s) = scripts.iter_mut().find(|s| s.slot == slot) {
                    s.card = Some(ScriptCard {
                        kind,
                        text,
                        choices,
                        rec,
                    });
                }
            }
            HostLine::Reply { slot, text, rev: 1 } => {
                if let Some(s) = scripts.iter_mut().find(|s| s.slot == slot) {
                    s.reply = Some(text);
                }
            }
            _ => {}
        }
    }
    scripts
}

/// The device-only roster content (`ws`, `macro`) and the usage bars at `nudge`.
pub fn device_requests(nudge: u8) -> Vec<Request> {
    let mut reqs = Vec::new();
    for line in roster() {
        match line {
            HostLine::Ws {
                active,
                count,
                sel,
                windows,
            } => reqs.push(Request::DeviceWsSet {
                active,
                count,
                sel,
                windows,
            }),
            HostLine::Macro { face, src, keys } => reqs.push(Request::DeviceMacroSet {
                f: face,
                layout: src.clone().unwrap_or_else(|| "mockup".into()),
                src,
                keys,
            }),
            _ => {}
        }
    }
    reqs.push(usage_request(nudge));
    reqs
}

fn usage_request(nudge: u8) -> Request {
    match usage(nudge) {
        HostLine::Usage { period, items } => Request::DeviceUsageSet { period, items },
        // `usage()` builds a Usage line by construction.
        _ => unreachable!("usage() is a usage line"),
    }
}

fn working(id: &AgentId, title: &str) -> Request {
    Request::AgentUpdate(AgentUpdate {
        agent: id.clone(),
        state: Some(AgentState::Working),
        title: Some(title.into()),
        title_source: Some(TitleSource::Api),
        since: None,
        name: None,
        meta: None, task: None, subagents: None,
    })
}

fn agent_of(data: &serde_json::Value) -> Option<AgentId> {
    data.get("agent")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// What the demo does in reaction to an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Api(Box<Request>),
    Op(DeviceOp),
    /// A typed PTT transcript for the agent (the core knows its slot).
    Transcript {
        agent: AgentId,
        text: String,
    },
}

/// The in-process demo client: which agents it owns and how it reacts.
#[derive(Debug, Default)]
pub struct Demo {
    agents: BTreeMap<AgentId, u8>,
    usage_nudge: u8,
    /// The §4.13 example agent, once registered.
    permission: Option<AgentId>,
    /// Its held card and the parked hook response the demo owns.
    hold: Option<(CardId, HoldRx)>,
    /// When the permission card is set again after a resolution.
    repush_at: Option<Instant>,
}

impl Demo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, id: AgentId, slot: u8) {
        self.agents.insert(id, slot);
    }

    pub fn owns(&self, id: &AgentId) -> bool {
        self.agents.contains_key(id)
    }

    pub fn agents(&self) -> usize {
        self.agents.len()
    }

    /// The usage line as last pushed.
    pub fn usage_line(&self) -> HostLine {
        usage(self.usage_nudge)
    }

    /// The §4.13 example agent was registered.
    pub fn set_permission(&mut self, agent: AgentId) {
        self.permission = Some(agent);
    }

    pub fn permission_agent(&self) -> Option<&AgentId> {
        self.permission.as_ref()
    }

    /// The demo owns the `HoldRx` of the card it just set.
    pub fn park(&mut self, card: CardId, hold: HoldRx) {
        self.hold = Some((card, hold));
        self.repush_at = None;
    }

    pub fn held_card(&self) -> Option<CardId> {
        self.hold.as_ref().map(|(card, _)| *card)
    }

    /// How the held card was resolved, once it has been: the card comes
    /// back `REPUSH_AFTER` later, so the deck always has one to try.
    pub fn poll_hold(&mut self, now: Instant) -> Option<HoldOutcome> {
        let (card, rx) = self.hold.as_mut()?;
        let card = *card;
        let outcome = match rx.try_recv() {
            Ok(HoldResult::Answer(answer)) => HoldOutcome::Answer { card, answer },
            Ok(HoldResult::Released { reason }) => HoldOutcome::Released { card, reason },
            Err(TryRecvError::Closed) => HoldOutcome::Gone { card },
            Err(TryRecvError::Empty) => return None,
        };
        self.hold = None;
        self.repush_at = Some(now + REPUSH_AFTER);
        Some(outcome)
    }

    /// True once (per resolution) when the card is due again.
    pub fn repush_due(&mut self, now: Instant) -> bool {
        if self.repush_at.is_some_and(|at| at <= now) {
            self.repush_at = None;
            true
        } else {
            false
        }
    }

    /// The reference host's answers to what just happened, in order.
    pub fn react(&mut self, events: &[Event]) -> Vec<Act> {
        let mut acts = Vec::new();
        for e in events {
            match e.kind {
                EventKind::CardAnswered => {
                    let Some(id) = agent_of(&e.data).filter(|id| self.owns(id)) else {
                        continue;
                    };
                    let title = match e.data["choice"].as_u64() {
                        Some(choice) => format!("Working on choice {choice}"),
                        None => "Working on it".to_string(),
                    };
                    acts.push(Act::Api(Box::new(working(&id, &title))));
                }
                EventKind::UsageRefreshRequested => {
                    self.usage_nudge = self.usage_nudge.saturating_add(1);
                    acts.push(Act::Api(Box::new(usage_request(self.usage_nudge))));
                }
                EventKind::VoiceState if e.data["state"] == "finalizing" => {
                    let ctx: Option<PttCtx> = serde_json::from_value(e.data["ctx"].clone()).ok();
                    match ctx {
                        Some(PttCtx::Name) => {
                            acts.push(Act::Op(DeviceOp::Control(Control::Name("Deploy".into()))));
                        }
                        Some(PttCtx::Talk | PttCtx::Answer) => {
                            let Some(id) = agent_of(&e.data).filter(|id| self.owns(id)) else {
                                continue;
                            };
                            acts.push(Act::Transcript {
                                agent: id.clone(),
                                text: "Refactor the auth module, then run the test suite.".into(),
                            });
                            acts.push(Act::Api(Box::new(Request::Reply(ReplySet {
                                message: None,
                                agent: id.clone(),
                                text: "On it.".into(),
                                speak: false,
                                announce: None,
                                lane: None,
                            }))));
                            acts.push(Act::Api(Box::new(working(&id, "Refactoring auth"))));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roster_encodes_like_deck_sim() {
        let lines: Vec<String> = roster()
            .iter()
            .map(|l| String::from_utf8(deck_proto::encode(l).unwrap()).unwrap())
            .collect();
        assert_eq!(lines.len(), 14);
        assert_eq!(
            lines[0],
            "{\"m\":\"agent\",\"id\":1,\"kind\":\"codex\",\"name\":\"cohdl-api\",\"proj\":\"cohdl\",\"state\":\"question\",\"title\":\"Run migrations on staging?\",\"since\":120}\n"
        );
        assert_eq!(
            lines[4],
            "{\"m\":\"card\",\"agent\":2,\"id\":2,\"type\":\"r\",\"text\":\"Header refactor done · 34 files\",\"choices\":[\"Run the test suite\",\"Open a pull request\"]}\n"
        );
        assert_eq!(
            lines[13],
            "{\"m\":\"usage\",\"period\":\"SEP 2026\",\"items\":[{\"n\":\"Claude Pro\",\"p\":62,\"r\":\"resets in 4 days\"},{\"n\":\"Codex · OpenAI\",\"p\":81,\"r\":\"resets tomorrow\"},{\"n\":\"Gemini\",\"p\":12,\"r\":\"resets in 11 days\"}]}\n"
        );
        let flood = String::from_utf8(deck_proto::encode(&flood_title(11)).unwrap()).unwrap();
        assert_eq!(
            flood,
            "{\"m\":\"agent\",\"id\":3,\"title\":\"Compiling openmicro ⠙ 11\"}\n"
        );
        assert_eq!(default_faces().len(), 7);
        if let HostLine::Usage { items, .. } = usage(30) {
            assert_eq!(items[1].percent, 100);
        }
    }

    #[test]
    fn the_scripts_are_the_roster_as_api_calls() {
        let scripts = scripts();
        assert_eq!(scripts.len(), 5);
        assert_eq!(
            scripts.iter().map(|s| s.slot).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        let id = AgentId("ag_demo".into());
        let s1 = &scripts[0];
        let reg = s1.register();
        assert_eq!(reg.kind, Kind::Codex);
        assert_eq!(reg.key.as_deref(), Some("demo:cohdl-api"));
        assert_eq!(reg.slot_hint, Some(Slot(1)));
        assert_eq!(reg.caps.answer, AnswerCap::Poll);
        assert_eq!(reg.caps.order, OrderCap::Poll);
        let up = s1.update(&id, Ts::from_millis(500_000));
        assert_eq!(up.state, Some(AgentState::Question));
        assert_eq!(up.since, Some(Ts::from_millis(380_000)), "120 s ago");
        assert_eq!(up.title.as_deref(), Some("Run migrations on staging?"));
        let card = s1.card(&id).unwrap();
        assert_eq!(card.kind, CardKind::Q);
        assert_eq!(card.origin.kind, OriginKind::Question, "no » prefix");
        assert_eq!(card.deliver, deck_model::Deliver::Poll);
        assert_eq!(card.choices.len(), 3);
        assert_eq!(card.rec, Some(0));
        assert!(s1.reply(&id).unwrap().text.starts_with("Tests fail"));
        let s2 = &scripts[1];
        assert_eq!(s2.card(&id).unwrap().origin.kind, OriginKind::Ready);
        assert_eq!(s2.update(&id, Ts(9)).since, Some(Ts(9)), "no age");
        assert!(scripts[2].card.is_none() && scripts[2].reply.is_some());
        assert!(scripts[3].card.is_none() && scripts[3].reply.is_none());
        let dev = device_requests(0);
        assert!(matches!(dev[0], Request::DeviceWsSet { count: 4, .. }));
        assert!(
            matches!(&dev[1], Request::DeviceMacroSet { f: 3, layout, .. } if layout == "deck.toml")
        );
        assert!(matches!(&dev[2], Request::DeviceUsageSet { items, .. } if items[0].percent == 62));
        assert_eq!(dev.len(), 3);
    }

    fn event(kind: EventKind, data: serde_json::Value) -> Event {
        Event {
            rev: 1,
            ts: Ts(0),
            kind,
            principal: Principal::Device,
            data,
        }
    }

    #[test]
    fn the_demo_reacts_like_deck_sim() {
        let mut demo = Demo::new();
        let id = AgentId("ag_1".into());
        demo.add(id.clone(), 3);
        assert_eq!(demo.agents(), 1);
        assert!(demo.owns(&id) && !demo.owns(&AgentId("ag_2".into())));
        let acts = demo.react(&[event(
            EventKind::CardAnswered,
            serde_json::json!({"agent": "ag_1", "card": 1, "choice": 2}),
        )]);
        assert_eq!(
            acts,
            vec![Act::Api(Box::new(working(&id, "Working on choice 2")))]
        );
        let acts = demo.react(&[event(
            EventKind::CardAnswered,
            serde_json::json!({"agent": "ag_1", "card": 1, "choice": null}),
        )]);
        assert_eq!(
            acts,
            vec![Act::Api(Box::new(working(&id, "Working on it")))]
        );
        assert!(
            demo.react(&[event(
                EventKind::CardAnswered,
                serde_json::json!({"agent": "ag_9", "choice": 0}),
            )])
            .is_empty(),
            "not ours"
        );
        assert!(demo
            .react(&[event(
                EventKind::CardCleared,
                serde_json::json!({"agent": null})
            )])
            .is_empty());
        let acts = demo.react(&[event(
            EventKind::UsageRefreshRequested,
            serde_json::json!({"what": "usage"}),
        )]);
        assert_eq!(acts, vec![Act::Api(Box::new(usage_request(1)))]);
        assert_eq!(demo.usage_line(), usage(1));
        let acts = demo.react(&[event(
            EventKind::VoiceState,
            serde_json::json!({"state": "finalizing", "agent": "ag_1", "ctx": "talk", "card": null}),
        )]);
        assert_eq!(acts.len(), 3);
        assert!(matches!(&acts[0], Act::Transcript { agent, .. } if *agent == id));
        assert!(
            matches!(&acts[1], Act::Api(req) if matches!(&**req, Request::Reply(r) if r.text == "On it." && !r.speak))
        );
        assert_eq!(
            acts[2],
            Act::Api(Box::new(working(&id, "Refactoring auth")))
        );
        assert!(
            demo.react(&[event(
                EventKind::VoiceState,
                serde_json::json!({"state": "aborted", "agent": "ag_1", "ctx": "talk"}),
            )])
            .is_empty(),
            "an aborted PTT types nothing"
        );
        assert!(demo
            .react(&[event(
                EventKind::VoiceState,
                serde_json::json!({"state": "listening", "agent": "ag_1", "ctx": "talk"}),
            )])
            .is_empty());
        let acts = demo.react(&[event(
            EventKind::VoiceState,
            serde_json::json!({"state": "finalizing", "agent": null, "ctx": "name"}),
        )]);
        assert_eq!(
            acts,
            vec![Act::Op(DeviceOp::Control(Control::Name("Deploy".into())))]
        );
        assert!(demo
            .react(&[event(
                EventKind::VoiceState,
                serde_json::json!({"state": "finalizing", "agent": "ag_7", "ctx": "answer"}),
            )])
            .is_empty());
    }
}
