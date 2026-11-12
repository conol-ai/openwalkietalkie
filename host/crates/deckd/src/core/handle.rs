//! The seam between the doors and the core (contract §3.2): everything the
//! API can ask the `deckd-core` thread, as messages on one bounded channel,
//! and the handle the tokio side holds. The core answers through oneshots;
//! a full channel is retried every 10 ms for at most 200 ms and then
//! refused with `-32008 rate_limited{retry_ms:100}` — the core never blocks
//! a door and a door never blocks the core.
use super::holds::HoldToken;
use crate::bus::Bus;
use crate::config::Config;
use deck_model::{
    AgentId, Answer, ApiError, ApiResult, CardId, CardSet, Principal, Release, Request, Scope,
    SessionId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// The `msgs` channel's depth.
pub const MSGS_CAPACITY: usize = 1024;
/// A full channel is retried this often, for at most `SEND_BUDGET`.
pub const SEND_RETRY: Duration = Duration::from_millis(10);
pub const SEND_BUDGET: Duration = Duration::from_millis(200);
/// What the refusal tells the caller to wait.
pub const RETRY_MS: u32 = 100;
/// How long a message that must not be lost (`CloseSession`, `HoldGone`)
/// waits for the core before it is logged as lost.
pub const MUST_SEND_BUDGET: Duration = Duration::from_secs(5);

/// `client:{name,version}` of a `session.hello`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// A card whose answer a door must confirm as written (`Delivered{ok}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub agent: AgentId,
    pub card: CardId,
}

/// The answer to a `Call`: the JSON result and, for `agent.wait`, the
/// delivery the door reports as flushed after writing the response.
#[derive(Debug)]
pub struct Reply {
    pub result: ApiResult,
    pub after_write: Option<Delivery>,
}

impl Reply {
    pub fn ok(value: serde_json::Value) -> Self {
        Reply {
            result: Ok(value),
            after_write: None,
        }
    }

    pub fn err(error: ApiError) -> Self {
        Reply {
            result: Err(error),
            after_write: None,
        }
    }
}

/// Who is calling a core-answered method. `own` (the agents the session
/// owns) is filled by the core itself; a door leaves it empty.
#[derive(Clone, Debug)]
pub struct CallCtx {
    pub principal: Principal,
    pub session: SessionId,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
    pub own: BTreeSet<AgentId>,
}

/// What `agent.wait` resolves with.
#[derive(Debug)]
pub enum WaitResult {
    Answer {
        card: CardId,
        answer: Answer,
        delivery: Delivery,
    },
    Order {
        text: String,
        ctx: String,
        choice: Option<u8>,
        card: Option<CardId>,
        delivery: Option<Delivery>,
    },
    Snooze {
        card: CardId,
    },
    Timeout,
    /// The wait itself was refused (§3.2: the session must own the agent,
    /// else `-32004 conflict{owner}` / `-32003`). Additive to the frozen
    /// seam: without it the typed path had to answer `Timeout` and a
    /// caller could not tell a quiet agent from one it may not wait on.
    Refused(ApiError),
}

/// The parked side of a held card.
pub type HoldRx = oneshot::Receiver<HoldResult>;

#[derive(Debug)]
pub enum HoldResult {
    Answer(Answer),
    Released { reason: Release },
}

/// Sends `CoreMsg::HoldGone` when dropped unless `disarm()`ed: a hook
/// handler that goes away (client timeout, Ctrl-C) releases its card.
#[derive(Debug)]
pub struct HoldGuard {
    agent: AgentId,
    card: CardId,
    /// Which park this guard is reporting: `(agent, card)` is re-used by a
    /// same-id re-push, so a stale guard must not release a live hold (B6).
    token: HoldToken,
    msgs: crossbeam_channel::Sender<CoreMsg>,
    armed: bool,
}

impl HoldGuard {
    pub fn new(
        agent: AgentId,
        card: CardId,
        token: HoldToken,
        msgs: crossbeam_channel::Sender<CoreMsg>,
    ) -> Self {
        HoldGuard {
            agent,
            card,
            token,
            msgs,
            armed: true,
        }
    }

    /// The hold was resolved and written: nothing to report on drop.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        if self.armed {
            // Never `try_send`: a lost `HoldGone` is a hook parked until
            // `hold_s` and a card that stays pending on the deck. The core
            // drains this channel on every loop, so the block is bounded by
            // one step, and a drop happens off the latency path anyway.
            must_send(
                &self.msgs,
                CoreMsg::HoldGone {
                    agent: self.agent.clone(),
                    card: self.card,
                    token: self.token,
                },
                "hold gone",
            );
        }
    }
}

/// A message the daemon cannot afford to drop (a session's close, a hold's
/// release): block until the core takes it, giving up only if the core is
/// gone. Both callers run off the request path.
fn must_send(msgs: &crossbeam_channel::Sender<CoreMsg>, msg: CoreMsg, what: &str) {
    match msgs.send_timeout(msg, MUST_SEND_BUDGET) {
        Ok(()) => {}
        Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
            tracing::error!(what, "the core did not take a message it must not lose");
        }
        Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {}
    }
}

/// What a connection can receive from the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outbound {
    /// A JSON-RPC notification, already serialized (no trailing newline).
    Line(String),
    /// An `agent.answer`/`agent.order` push: written before any queued bus
    /// event, then `Delivered` is reported after the flush.
    Deliver { line: String, delivery: Delivery },
}

#[derive(Clone, Debug)]
pub enum SessionKind {
    /// A WS/UDS connection: receives notifications and deliveries.
    Live { tx: mpsc::Sender<Outbound> },
    /// SSE: events only, never owns agents.
    Observer,
    /// The demo client inside the daemon.
    Internal,
}

impl SessionKind {
    pub fn is_live(&self) -> bool {
        matches!(self, SessionKind::Live { .. })
    }
}

#[derive(Clone, Debug)]
pub struct OpenSession {
    pub kind: SessionKind,
    pub principal: Principal,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
    pub client: ClientInfo,
}

/// Everything the doors send the core.
#[derive(Debug)]
pub enum CoreMsg {
    SystemStatus(deck_proto::SystemStatus),
    /// A built-in adapter reports the session controls it actually observed.
    SessionControls { agent: AgentId, session: SessionId, settled: bool, controls: deck_model::session_controls::SessionControls },
    /// A reducer method (`Request`): `store.call`, effects, the result.
    Api {
        principal: Principal,
        session: SessionId,
        trusted: bool,
        req: Box<Request>,
        reply: oneshot::Sender<ApiResult>,
    },
    /// A method the core answers from its own state (`core/dispatch.rs`).
    Call {
        ctx: CallCtx,
        method: String,
        params: serde_json::Value,
        reply: oneshot::Sender<Reply>,
    },
    Wait {
        session: SessionId,
        agent: AgentId,
        timeout: Duration,
        reply: oneshot::Sender<WaitResult>,
    },
    /// `principal`/`session` are the adapter's own: a hold is a write like
    /// any other and the store checks ownership (B3).
    SetCardHeld {
        principal: Principal,
        session: SessionId,
        cmd: CardSet,
        reply: oneshot::Sender<Result<(CardId, HoldToken, HoldRx), ApiError>>,
    },
    HoldWritten {
        agent: AgentId,
        card: CardId,
    },
    HoldGone {
        agent: AgentId,
        card: CardId,
        token: HoldToken,
    },
    /// A usage feeder's latest windows (`crate::usage`): the core merges
    /// them with the other provider's and pushes the Usage face.
    Usage {
        /// Status-line reports omit model meters; OAuth reports replace them.
        partial: bool,
        provider: crate::usage::Provider,
        windows: Vec<crate::usage::Window>,
        /// None preserves known grants on meter-only notifications.
        resets: Option<Vec<crate::usage::ResetGrant>>,
        account: Option<String>,
    },
    /// A door flushed a delivery.
    Delivered {
        agent: AgentId,
        card: CardId,
        ok: bool,
    },
    OpenSession {
        open: OpenSession,
        /// §9: a `Result`, so the core can refuse a session it will not
        /// open (an over-long `client.name`, §3.3's caps).
        reply: oneshot::Sender<Result<SessionId, ApiError>>,
    },
    LogicalSession {
        token: String,
        client: String,
        principal: Principal,
        scopes: Vec<Scope>,
        reply: oneshot::Sender<Result<SessionId, ApiError>>,
    },
    CloseSession(SessionId),
    /// Hot reload.
    Config(Arc<Config>),
    /// What a care method answered on the blocking pool needs from the
    /// core (`crate::care`): the daemon's own paths and how it was started.
    CareContext(oneshot::Sender<crate::care::Context>),
    Shutdown,
}

/// The tokio side's handle on the core thread. Cheap to clone.
#[derive(Clone)]
pub struct CoreHandle {
    msgs: crossbeam_channel::Sender<CoreMsg>,
    bus: Arc<Bus>,
    /// The adapters' own per-daemon state (their internal sessions, the
    /// report queue, the per-CLI hold budgets). It rides here because a
    /// handle is the one thing both doors already carry into
    /// `api::dispatch`, and because the UDS helper and `POST
    /// /v1/hooks/{cli}` must reach the *same* adapter session — a second
    /// session per CLI turns every `SessionStart` of a live agent into
    /// `-32004 conflict`. Nothing in it is a core concern, so it is an
    /// `Arc` beside the channel rather than state behind it.
    adapters: Arc<crate::adapters::rt::Adapters>,
}

impl std::fmt::Debug for CoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreHandle")
            .field("queued", &self.msgs.len())
            .finish()
    }
}

/// The core is gone (the daemon is stopping) or its channel stayed full.
fn unavailable() -> ApiError {
    ApiError::internal("core unavailable")
}

impl CoreHandle {
    pub fn new(msgs: crossbeam_channel::Sender<CoreMsg>, bus: Arc<Bus>) -> Self {
        CoreHandle::with_adapters(msgs, bus, Arc::default())
    }

    /// A handle onto a core that already owns its adapter state, so a
    /// `[hooks.*]` hot reload reaches the next held card (B2).
    pub fn with_adapters(
        msgs: crossbeam_channel::Sender<CoreMsg>,
        bus: Arc<Bus>,
        adapters: Arc<crate::adapters::rt::Adapters>,
    ) -> Self {
        CoreHandle {
            msgs,
            bus,
            adapters,
        }
    }

    /// A handle with no core behind it: the test holds the receiver.
    pub fn detached() -> (Self, crossbeam_channel::Receiver<CoreMsg>) {
        let (tx, rx) = crossbeam_channel::bounded(MSGS_CAPACITY);
        (CoreHandle::new(tx, Arc::new(Bus::default())), rx)
    }

    /// A handle with no core and a bus the test can read.
    pub fn detached_with(bus: Arc<Bus>) -> (Self, crossbeam_channel::Receiver<CoreMsg>) {
        let (tx, rx) = crossbeam_channel::bounded(MSGS_CAPACITY);
        (CoreHandle::new(tx, bus), rx)
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    pub fn adapters(&self) -> &Arc<crate::adapters::rt::Adapters> {
        &self.adapters
    }

    /// The channel's sender, for a guard that must report on drop.
    pub fn sender(&self) -> crossbeam_channel::Sender<CoreMsg> {
        self.msgs.clone()
    }

    /// `try_send`, retried every 10 ms for at most 200 ms.
    pub async fn send(&self, msg: CoreMsg) -> Result<(), ApiError> {
        self.send_with(msg, Instant::now, tokio::time::sleep).await
    }

    /// `send` with the clock and the sleeper injected (the tests drive a
    /// simulated clock; the daemon passes the real ones).
    async fn send_with<N, S, F>(
        &self,
        mut msg: CoreMsg,
        mut now: N,
        sleep: S,
    ) -> Result<(), ApiError>
    where
        N: FnMut() -> Instant,
        S: Fn(Duration) -> F,
        F: std::future::Future<Output = ()>,
    {
        let deadline = now() + SEND_BUDGET;
        loop {
            match self.msgs.try_send(msg) {
                Ok(()) => return Ok(()),
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => return Err(unavailable()),
                Err(crossbeam_channel::TrySendError::Full(back)) => {
                    if now() >= deadline {
                        return Err(ApiError::rate_limited(RETRY_MS));
                    }
                    msg = back;
                    sleep(SEND_RETRY).await;
                }
            }
        }
    }

    /// Fire-and-forget from a non-async context (the main thread's shutdown).
    pub fn send_blocking(&self, msg: CoreMsg) -> bool {
        self.msgs.send(msg).is_ok()
    }

    pub async fn api(
        &self,
        principal: Principal,
        session: SessionId,
        trusted: bool,
        req: Request,
    ) -> ApiResult {
        let (tx, rx) = oneshot::channel();
        self.send(CoreMsg::Api {
            principal,
            session,
            trusted,
            req: Box::new(req),
            reply: tx,
        })
        .await?;
        rx.await.unwrap_or_else(|_| Err(unavailable()))
    }

    pub async fn call(&self, ctx: CallCtx, method: &str, params: serde_json::Value) -> Reply {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .send(CoreMsg::Call {
                ctx,
                method: method.to_string(),
                params,
                reply: tx,
            })
            .await
        {
            return Reply::err(e);
        }
        rx.await.unwrap_or_else(|_| Reply::err(unavailable()))
    }

    /// `WaitResult::Timeout` when the core is gone or its channel stayed full.
    pub async fn wait(&self, session: SessionId, agent: AgentId, timeout: Duration) -> WaitResult {
        let (tx, rx) = oneshot::channel();
        if self
            .send(CoreMsg::Wait {
                session,
                agent,
                timeout,
                reply: tx,
            })
            .await
            .is_err()
        {
            return WaitResult::Timeout;
        }
        rx.await.unwrap_or(WaitResult::Timeout)
    }

    /// Park a held card for `principal`'s own agent. The store refuses a
    /// hold on an agent `session` does not own (B3), and the refusal that
    /// comes back is the store's own (B4).
    pub async fn set_card_held(
        &self,
        principal: Principal,
        session: SessionId,
        cmd: CardSet,
    ) -> Result<(CardId, HoldRx, HoldGuard), ApiError> {
        let agent = cmd.agent.clone();
        let (tx, rx) = oneshot::channel();
        self.send(CoreMsg::SetCardHeld {
            principal,
            session,
            cmd,
            reply: tx,
        })
        .await?;
        let (card, token, hold) = rx.await.map_err(|_| unavailable())??;
        Ok((
            card,
            hold,
            HoldGuard::new(agent, card, token, self.msgs.clone()),
        ))
    }

    pub async fn hold_written(&self, agent: AgentId, card: CardId) {
        let _ = self.send(CoreMsg::HoldWritten { agent, card }).await;
    }

    pub async fn delivered(&self, agent: AgentId, card: CardId, ok: bool) {
        let _ = self.send(CoreMsg::Delivered { agent, card, ok }).await;
    }

    /// The new session's id, or the error when the core is unavailable
    /// (override of the contract's bare `SessionId`: a door must not be
    /// handed a session that does not exist).
    pub async fn open_session(&self, open: OpenSession) -> Result<SessionId, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.send(CoreMsg::OpenSession { open, reply: tx }).await?;
        rx.await.map_err(|_| unavailable())?
    }

    /// The logical session `(token, client)`, created on first use.
    pub async fn logical_session(
        &self,
        token: String,
        client: String,
        principal: Principal,
        scopes: Vec<Scope>,
    ) -> Result<SessionId, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.send(CoreMsg::LogicalSession {
            token,
            client,
            principal,
            scopes,
            reply: tx,
        })
        .await?;
        rx.await.map_err(|_| unavailable())?
    }

    /// A closed connection: its agents are orphaned and its mail is kept
    /// for `mailbox_ttl_s` (§4.8). Losing this leaks a session, so it
    /// blocks rather than dropping; it never runs on a request's path.
    pub fn close_session(&self, id: SessionId) {
        must_send(&self.msgs, CoreMsg::CloseSession(id), "session closed");
    }

    pub async fn config(&self, config: Arc<Config>) {
        let _ = self.send(CoreMsg::Config(config)).await;
    }

    /// The core's side of a care method ([`crate::care::Context`]); the
    /// door does the rest off the core thread.
    pub async fn care_context(&self) -> Result<crate::care::Context, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.send(CoreMsg::CareContext(tx)).await?;
        rx.await.map_err(|_| unavailable())
    }

    /// Queued messages (metrics, tests).
    pub fn queued(&self) -> usize {
        self.msgs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn a_full_channel_is_refused_as_rate_limited_after_the_budget() {
        let (handle, rx) = CoreHandle::detached();
        for _ in 0..MSGS_CAPACITY {
            handle.close_session(SessionId(9));
        }
        assert_eq!(handle.queued(), MSGS_CAPACITY);
        // A simulated clock: every "sleep" advances it, no real time passes.
        let t0 = Instant::now();
        let clock = std::cell::Cell::new(t0);
        let sleeps = std::cell::Cell::new(0u32);
        let started = Instant::now();
        let err = rt()
            .block_on(handle.send_with(
                CoreMsg::CloseSession(SessionId(2)),
                || clock.get(),
                |d| {
                    clock.set(clock.get() + d);
                    sleeps.set(sleeps.get() + 1);
                    std::future::ready(())
                },
            ))
            .unwrap_err();
        assert_eq!(err.code, ApiError::RATE_LIMITED);
        assert_eq!(err.data["retry_ms"], RETRY_MS);
        assert_eq!(
            u128::from(sleeps.get()),
            SEND_BUDGET.as_millis() / SEND_RETRY.as_millis(),
            "retried every 10 ms for the whole budget"
        );
        assert_eq!(clock.get(), t0 + SEND_BUDGET);
        assert!(started.elapsed() < SEND_BUDGET, "no real sleep");
        assert_eq!(handle.queued(), MSGS_CAPACITY, "nothing was queued");
        // Room appears while retrying: the message gets through.
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreMsg::CloseSession(SessionId(9)))
        ));
        let clock = std::cell::Cell::new(t0);
        rt().block_on(handle.send_with(
            CoreMsg::CloseSession(SessionId(3)),
            || clock.get(),
            |d| {
                clock.set(clock.get() + d);
                std::future::ready(())
            },
        ))
        .unwrap();
        assert_eq!(handle.queued(), MSGS_CAPACITY);
        // A gone core is an internal error, not a rate limit.
        drop(rx);
        let reply = rt().block_on(handle.call(
            CallCtx {
                principal: Principal::Uid(1),
                session: SessionId(2),
                scopes: vec![],
                trusted: false,
                own: BTreeSet::new(),
            },
            "session.ping",
            serde_json::json!({}),
        ));
        assert_eq!(reply.result.unwrap_err().code, ApiError::INTERNAL);
        assert!(matches!(
            rt().block_on(handle.wait(SessionId(2), AgentId("ag_1".into()), Duration::ZERO)),
            WaitResult::Timeout
        ));
    }

    #[test]
    fn a_dropped_reply_is_an_internal_error_and_the_hold_guard_reports_on_drop() {
        let (handle, rx) = CoreHandle::detached();
        let answerer = std::thread::spawn(move || {
            // Drop the first reply unanswered; the second is a session id.
            match rx.recv().unwrap() {
                CoreMsg::OpenSession { reply, .. } => drop(reply),
                other => panic!("{other:?}"),
            }
            match rx.recv().unwrap() {
                CoreMsg::LogicalSession {
                    token,
                    client,
                    reply,
                    ..
                } => {
                    assert_eq!((token.as_str(), client.as_str()), ("hooks", "bot"));
                    reply.send(Ok(SessionId(5))).unwrap();
                }
                other => panic!("{other:?}"),
            }
            let gone = rx.recv().unwrap();
            assert!(
                matches!(gone, CoreMsg::HoldGone { ref agent, card, .. } if agent.0 == "ag_1" && card == CardId(3)),
                "{gone:?}"
            );
            assert!(
                rx.recv_timeout(Duration::from_millis(20)).is_err(),
                "a disarmed guard is silent"
            );
        });
        let open = OpenSession {
            kind: SessionKind::Observer,
            principal: Principal::Uid(1),
            scopes: vec![Scope::Observe],
            trusted: false,
            client: ClientInfo::default(),
        };
        let err = rt().block_on(handle.open_session(open)).unwrap_err();
        assert_eq!(err.code, ApiError::INTERNAL);
        let id = rt()
            .block_on(handle.logical_session(
                "hooks".into(),
                "bot".into(),
                Principal::Token("hooks".into()),
                vec![Scope::Agent],
            ))
            .unwrap();
        assert_eq!(id, SessionId(5));
        let mut holds = crate::core::holds::Holds::new();
        let (token, _rx) = holds.park(AgentId("ag_1".into()), CardId(3));
        let guard = HoldGuard::new(AgentId("ag_1".into()), CardId(3), token, handle.sender());
        assert!(guard.is_armed());
        drop(guard);
        let mut quiet = HoldGuard::new(AgentId("ag_2".into()), CardId(4), token, handle.sender());
        quiet.disarm();
        drop(quiet);
        answerer.join().unwrap();
    }
}
