//! A simulated clock around the store for the unit and property tests.
use crate::api::ApiResult;
use crate::{
    Agent, AgentId, AgentRegister, AgentState, AnswerCap, Caps, Card, CardId, CardKind, CardSet,
    CardStatus, Choice, Command, Deliver, DeviceOp, Effect, Event, EventKind, HostLine, Key, Kind,
    OrderCap, Origin, OriginKind, Owner, Page, Principal, Reply, Request, SessionId, SessionInfo,
    Slot, Source, Store, StoreConfig, TitleSource, Ts,
};
use std::time::{Duration, Instant};

pub const UID: Principal = Principal::Uid(501);
pub const UID2: Principal = Principal::Uid(502);

pub fn claude_adapter() -> Principal {
    Principal::Adapter(crate::AdapterKind::Claude)
}

/// A hook-registered Claude agent with a prompt title, a held permission
/// card, a reply and `meta`: every content field of §4.3 populated.
pub fn fixture_agent(id: &str, slot: u8, state: AgentState) -> Agent {
    Agent {
        id: AgentId(id.into()),
        slot: Some(Slot(slot)),
        kind: Kind::Claude,
        name: "web-ui".into(),
        state,
        title: Some("Header refactor".into()),
        title_source: TitleSource::Prompt,
        since: Ts(0),
        source: Source::Hook,
        owner: Owner {
            principal: claude_adapter(),
            session: SessionId(1),
            lease_expires: None,
        },
        session: SessionInfo {
            key: Some("claude:1".into()),
            cwd: Some("/tmp".into()),
            pid: Some(1),
            pid_kind: None,
            terminal: None,
        },
        companion: None,
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: true,
        },
        card: Some(Card {
            id: CardId(17),
            kind: CardKind::Q,
            status: CardStatus::Pending,
            deliver: Deliver::Hold,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            },
            text: Some("Bash: psql".into()),
            choices: vec![
                Choice {
                    label: "Allow".into(),
                    detail: None,
                },
                Choice {
                    label: "Deny".into(),
                    detail: None,
                },
            ],
            choices_n: 2,
            choice_map: Vec::new(),
            rec: Some(0),
            free_text: true,
            page: Page { index: 0, count: 1 },
            created: Ts(0),
            expires: None,
            redacted: false,
        }),
        reply: Some(Reply {
            text: Some("done".into()),
            at: Ts(0),
            announce: None,
            redacted: false,
        }),
        messages: Vec::new(),
        workflow_runs: Vec::new(), workflow: None,
        chat_rev: 0,
        reply_rev: 0,
        voice: None,
        pinned: false,
        meta: serde_json::json!({"secret": 1}),
        redacted: false,
    }
}

/// A virtual agent registration: `kind:other`, poll/poll, 30 s lease.
pub fn register(name: &str, key: Option<&str>) -> AgentRegister {
    AgentRegister {
        kind: Kind::Other,
        name: name.into(),
        key: key.map(String::from),
        cwd: None,
        pid: None,
        pid_kind: None,
        terminal: None,
        lease_ttl_s: 30,
        caps: Caps {
            answer: AnswerCap::Poll,
            order: OrderCap::Poll,
            focus: false,
        },
        slot_hint: None,
        voice: None,
        meta: None,
    }
}

pub fn card_set(agent: &AgentId, kind: CardKind, deliver: Deliver, labels: &[&str]) -> CardSet {
    CardSet {
        agent: agent.clone(),
        id: None,
        kind,
        text: "question?".into(),
        choices: labels
            .iter()
            .map(|l| Choice {
                label: (*l).into(),
                detail: None,
            })
            .collect(),
        rec: None,
        free_text: true,
        deliver,
        timeout_s: None,
        origin: Origin::custom(),
    }
}

pub struct Sim {
    pub store: Store,
    pub t: Instant,
    /// Of the last `api`/`apply`/`advance`.
    pub events: Vec<Event>,
    pub effects: Vec<Effect>,
}

impl Sim {
    pub fn new() -> Self {
        Self::with(|_| {})
    }

    pub fn with(f: impl FnOnce(&mut StoreConfig)) -> Self {
        let t = Instant::now();
        let mut cfg = StoreConfig::new(t, Ts(1_789_760_523_000), 314_159);
        f(&mut cfg);
        Sim {
            store: Store::new(cfg),
            t,
            events: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn api(&mut self, principal: Principal, session: SessionId, req: Request) -> ApiResult {
        let (result, events, effects) = self.store.call(principal, session, req, self.t);
        self.events = events;
        self.effects = effects;
        result
    }

    pub fn apply(&mut self, cmd: Command) {
        let (events, effects) = self.store.apply(cmd, self.t);
        self.events = events;
        self.effects = effects;
    }

    /// Move the clock and tick once.
    pub fn advance(&mut self, d: Duration) {
        self.t += d;
        self.apply(Command::Tick(self.t));
    }

    /// Collect what a direct store call left behind.
    pub fn drain(&mut self) {
        let (events, effects) = self.store.take();
        self.events = events;
        self.effects = effects;
    }

    pub fn register(
        &mut self,
        principal: Principal,
        session: SessionId,
        name: &str,
        key: Option<&str>,
    ) -> AgentId {
        let res = self
            .api(
                principal,
                session,
                Request::AgentRegister(register(name, key)),
            )
            .expect("register");
        AgentId(res["agent"].as_str().expect("agent id").to_string())
    }

    /// `agent.card.set` from the owner.
    pub fn set_card(
        &mut self,
        agent: &AgentId,
        kind: CardKind,
        deliver: Deliver,
        labels: Vec<&str>,
    ) -> CardId {
        let owner = self.store.snapshot().agents[agent].owner.clone();
        let res = self
            .api(
                owner.principal,
                owner.session,
                Request::CardSet(card_set(agent, kind, deliver, &labels)),
            )
            .expect("card.set");
        CardId(res["card"]["id"].as_u64().expect("card id") as u16)
    }

    /// A held card parked by the agent's **own owner**, which is what an
    /// adapter does (B3: `set_card_held` now checks ownership like every
    /// other write). Call `Store::set_card_held` directly to test a caller
    /// that does not own the agent.
    pub fn hold(&mut self, cmd: CardSet, hold: crate::HoldId) -> Result<CardId, crate::ApiError> {
        let owner = self.store.snapshot().agents[&cmd.agent].owner.clone();
        self.store
            .set_card_held(&owner.principal, owner.session, cmd, hold)
    }

    pub fn kinds(&self) -> Vec<EventKind> {
        self.events.iter().map(|e| e.kind).collect()
    }

    /// The `Put` keys of the last step, in order.
    pub fn puts(&self) -> Vec<Key> {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Device(DeviceOp::Put(k, _)) => Some(*k),
                _ => None,
            })
            .collect()
    }

    /// The last `Put` line for `key` in the last step.
    pub fn line(&self, key: Key) -> Option<HostLine> {
        self.effects.iter().rev().find_map(|e| match e {
            Effect::Device(DeviceOp::Put(k, line)) if *k == key => Some(line.clone()),
            _ => None,
        })
    }

    pub fn slot_of(&self, id: &AgentId) -> Option<Slot> {
        self.store.snapshot().agents.get(id).and_then(|a| a.slot)
    }
}
