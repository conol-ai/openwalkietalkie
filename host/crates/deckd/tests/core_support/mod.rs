//! Fixtures shared by the B1 integration tests (contract §5 rows 8, 9, 12,
//! 16): a core with the hid channels detached (`--no-device` in the shape
//! the daemon uses when it never opens the deck), either on its own
//! `deckd-core` thread behind a `CoreHandle` or driven directly so a test
//! can advance a simulated clock. Nothing here binds a port, writes to the
//! real data dir or sleeps for its result.
#![allow(dead_code)]

use crossbeam_channel::{Receiver, Sender};
use deck_model::{
    AdapterKind, AgentId, AgentRegister, AnswerCap, ApiResult, Caps, CardId, CardKind, CardSet,
    CardStatus, Choice, Deliver, DeviceOp, Kind, OpAnswer, OrderCap, Origin, OriginKind, Principal,
    Request, Scope, SessionId, Store, StoreConfig, Ts,
};
use deckd::bus::Bus;
use deckd::config::Paths;
use deckd::core::{CallCtx, ClientInfo, Core, CoreHandle, CoreMsg, OpenSession, Outbound};
use deckd::core::{SessionKind, WaitResult};
use deckd::hid::{FromHid, HidDebug, HidHandle};
use deckd::metrics::Metric;
use deckd::state::State;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a helper waits for an op the core produces on another thread.
pub const OP_BUDGET: Duration = Duration::from_secs(2);

/// The uid principal the tests act as when they are not an adapter.
pub fn uid() -> Principal {
    Principal::Uid(501)
}

/// A private temp dir for one test (never `DECKD_DATA_DIR`, never `~`).
pub fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("deckd-b1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test's temp dir");
    dir
}

/// The hid side of a detached core, held by the test.
pub struct Parts {
    /// Every `DeviceOp` the core sent (what the deck would have been shown).
    pub ops: Receiver<DeviceOp>,
    pub lines: Sender<FromHid>,
    pub debug: Receiver<HidDebug>,
    pub dir: PathBuf,
}

impl Parts {
    /// Forget the ops produced so far, so a later assertion sees only new ones.
    pub fn drain(&self) {
        while self.ops.try_recv().is_ok() {}
    }

    /// The ops queued right now.
    pub fn taken(&self) -> Vec<DeviceOp> {
        self.ops.try_iter().collect()
    }
}

/// A core with detached hid channels and a bus whose ring keeps `ring`
/// events (`[privacy] event_ring`).
pub fn build(name: &str, ring: usize) -> (Core, Parts) {
    let dir = temp(name);
    let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
    let store = Store::new(StoreConfig::new(Instant::now(), Ts(1_789_760_523_000), 7));
    let (hid, ops, lines, debug) = HidHandle::detached();
    let mut core = Core::new(store, State::default(), hid, paths);
    core.set_bus(Arc::new(Bus::new(ring)));
    (
        core,
        Parts {
            ops,
            lines,
            debug,
            dir,
        },
    )
}

/// The same core on its own `deckd-core` thread: shut down and joined on
/// drop, its temp dir removed.
pub struct Running {
    pub handle: CoreHandle,
    pub parts: Parts,
    join: Option<std::thread::JoinHandle<Core>>,
}

impl Running {
    pub fn start(name: &str, ring: usize) -> Running {
        let (core, parts) = build(name, ring);
        // A `--no-device` daemon still runs its hid thread, which answers a
        // `HidDebug::Metric` with a report; this stand-in does the same, so
        // `metrics.get`, `service.info` and the shutdown never wait out the
        // core's 1.5 s metric budget. It ends when the core drops its sender.
        let debug = parts.debug.clone();
        let lines = parts.lines.clone();
        std::thread::Builder::new()
            .name("b1-hid-stub".into())
            .spawn(move || {
                while let Ok(msg) = debug.recv() {
                    if matches!(msg, HidDebug::Metric) {
                        let _ = lines.send(FromHid::Metric(Metric::default()));
                    }
                }
            })
            .expect("the hid stand-in");
        let (handle, join) = core.spawn();
        Running {
            handle,
            parts,
            join: Some(join),
        }
    }

    /// Wait for the first op matching `want`, or panic after `OP_BUDGET`.
    /// It blocks on the channel rather than polling it, so a test that
    /// measures how long the core took is not measuring a sleep as well.
    pub async fn op(&self, want: impl Fn(&DeviceOp) -> bool + Send + 'static) -> DeviceOp {
        let ops = self.parts.ops.clone();
        tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + OP_BUDGET;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match ops.recv_timeout(left) {
                    Ok(op) if want(&op) => return op,
                    Ok(_) => {}
                    Err(e) => panic!("the op never arrived: {e}"),
                }
            }
        })
        .await
        .expect("the op watcher")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.handle.send_blocking(CoreMsg::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.parts.dir);
    }
}

/// A `CallCtx` for a core-answered method (`admin` sees unredacted content).
pub fn ctx(session: SessionId, scopes: &[Scope]) -> CallCtx {
    CallCtx {
        principal: uid(),
        session,
        scopes: scopes.to_vec(),
        trusted: false,
        own: BTreeSet::new(),
    }
}

/// A `session.hello`-style session with a connection behind it.
pub fn live_session(scopes: &[Scope]) -> (OpenSession, tokio::sync::mpsc::Receiver<Outbound>) {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    (
        OpenSession {
            kind: SessionKind::Live { tx },
            principal: uid(),
            scopes: scopes.to_vec(),
            trusted: false,
            client: ClientInfo {
                name: "b1-test".into(),
                version: "0".into(),
            },
        },
        rx,
    )
}

/// An SSE-style observer (never owns agents, never receives deliveries).
pub fn observer_session(scopes: &[Scope]) -> OpenSession {
    OpenSession {
        kind: SessionKind::Observer,
        principal: uid(),
        scopes: scopes.to_vec(),
        trusted: false,
        client: ClientInfo {
            name: "b1-observer".into(),
            version: "0".into(),
        },
    }
}

/// A virtual agent that polls for both answers and orders (§4.15).
pub fn poll_register(name: &str) -> AgentRegister {
    AgentRegister {
        kind: Kind::Other,
        name: name.into(),
        key: Some(format!("b1:{name}")),
        cwd: None,
        pid: None,
        pid_kind: None,
        terminal: None,
        lease_ttl_s: 30,
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

/// The adapter-shaped agent a held card needs (`caps.answer:"hold"`).
pub fn hold_register(name: &str) -> AgentRegister {
    AgentRegister {
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: false,
            transcribe: Default::default(),
        },
        kind: Kind::Claude,
        ..poll_register(name)
    }
}

/// A two-choice custom question delivered by `poll`.
pub fn poll_card(agent: &AgentId, text: &str) -> CardSet {
    CardSet {
        agent: agent.clone(),
        id: None,
        kind: CardKind::Q,
        text: text.into(),
        choices: vec![
            Choice {
                label: "Yes".into(),
                detail: None,
            },
            Choice {
                label: "No".into(),
                detail: None,
            },
        ],
        rec: None,
        free_text: false,
        deliver: Deliver::Poll,
        timeout_s: None,
        origin: Origin::custom(),
    }
}

/// The design's §4.13 permission card, as the Claude adapter would hold it.
pub fn permission_card(agent: &AgentId) -> CardSet {
    CardSet {
        agent: agent.clone(),
        id: None,
        kind: CardKind::Q,
        text: "Bash: psql -f migrate.sql staging".into(),
        choices: ["Allow", "Allow always", "Deny"]
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

pub fn claude() -> Principal {
    Principal::Adapter(AdapterKind::Claude)
}

/// The agent id of an `agent.register` result.
pub fn agent_of(result: &ApiResult) -> AgentId {
    let value = result.as_ref().expect("agent.register");
    serde_json::from_value(value["agent"].clone()).expect("agent id")
}

/// The card id of an `agent.card.set` result.
pub fn card_of(result: &ApiResult) -> CardId {
    let value = result.as_ref().expect("agent.card.set");
    serde_json::from_value(value["card"]["id"].clone()).expect("card id")
}

/// `{agent, card, choice}` from an operator.
pub fn answer(agent: &AgentId, card: CardId, choice: u8) -> Request {
    Request::OpAnswer(OpAnswer {
        agent: agent.clone(),
        card,
        choice: Some(choice),
        text: None,
    })
}

/// The status of the agent's current card, straight from the store.
pub fn card_status(core: &Core, agent: &AgentId) -> Option<CardStatus> {
    core.store.snapshot().agents[agent]
        .card
        .as_ref()
        .map(|c| c.status.clone())
}

/// Is this op a `card clear` control?
pub fn is_card_clear(op: &DeviceOp) -> bool {
    matches!(op, DeviceOp::Control(deck_model::Control::CardClear(_, _)))
}

/// The `card` line of a `Put(Key::Card(..))`, if this op is one.
pub fn card_put(op: &DeviceOp) -> Option<(u16, String, Vec<String>)> {
    match op {
        DeviceOp::Put(
            deck_model::Key::Card(_, _),
            deck_model::HostLine::Card {
                id, text, choices, ..
            },
        ) => Some((*id, text.clone(), choices.clone())),
        _ => None,
    }
}

/// Park an `agent.wait` on the core's own ordered channel, so the next
/// message a test sends is handled strictly after it (no sleep decides a
/// test): the receiver resolves when the core answers.
pub fn park_wait(
    handle: &CoreHandle,
    session: SessionId,
    agent: &AgentId,
    timeout: Duration,
) -> tokio::sync::oneshot::Receiver<WaitResult> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    handle
        .sender()
        .send(CoreMsg::Wait {
            session,
            agent: agent.clone(),
            timeout,
            reply,
        })
        .expect("the core thread is running");
    rx
}
