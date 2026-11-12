//! The Codex adapter's runtime (design §5, contract §5): the place that
//! turns Codex's hook payload into the §4.6 primitives, and an operator's
//! answer back into the JSON Codex expects on stdout.
//!
//! The sibling of [`super`], which does the same job for the Claude
//! adapter. The two differ in where the effects sit — [`handle`] resolves
//! the agent and runs the plan itself, while over there the handler in
//! `api::hooks` owns every effect — and in nothing the CLI can see. See
//! [`super`]'s own note on why the merge kept both.
//!
//! An adapter is a **normaliser**, not an actor. It owns no timers, no
//! sockets and no holds: `ingest` is a function of a payload and a
//! context, `answer_to_output` a function of a card and an answer, and
//! everything with a consequence — parking the hold, clearing the card,
//! releasing it on snooze — belongs to the core. That is what lets the
//! per-CLI behaviour be read as a table and tested from recorded fixtures.
//!
//! Three things are shared by every adapter and live here rather than in
//! each one:
//!
//! - **The internal session.** An adapter owns its agents as
//!   `Principal::Adapter(kind)` on one `SessionKind::Internal` session per
//!   CLI, opened once and never closed (an adapter's agents live by hook
//!   events, not by a lease). Registration is idempotent by `key` *from
//!   the same session*, so a second session per CLI would turn every
//!   `SessionStart` of a live agent into `-32004 conflict`.
//! - **Agent resolution.** Every hook after the first carries the CLI's
//!   session id and nothing else, so the handler resolves `key` →
//!   `AgentId` before the adapter sees the payload, registering when the
//!   event justifies it. Doing it here, synchronously, is what keeps a
//!   `UserPromptSubmit` arriving 1 ms after a `SessionStart` from missing
//!   the agent that `SessionStart` created.
//! - **The report queue.** `Ingest::Report` answers the CLI `{}` **first**
//!   and enqueues the requests behind it (contract §5): a report-only hook
//!   is synchronous in the CLI, and nothing it reports is worth a
//!   millisecond of the user's terminal. The queue is serial, so two
//!   hooks of one session stay in the order they fired.
use super::codex;
use crate::core::handle::{CoreHandle, Delivery, HoldResult};
use deck_model::{
    sanitize, AdapterKind, Agent, AgentId, AgentRegister, Answer, Card, CardId, CardKind, CardSet,
    Choice, Deliver, Origin, OriginKind, PidKind, Principal, Request, SessionId, Snapshot,
    Terminal,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

/// How many report batches may be in flight before the oldest CLI's
/// reports are dropped rather than a hook made to wait. A report is a
/// state change and a title; losing one under a flood is a stale title,
/// where blocking would be a stalled terminal.
pub const REPORT_QUEUE: usize = 1024;

/// How long any single core round trip on a hook's critical path may take
/// before the hook gives up and answers `{}`.
///
/// Every one of these — opening the adapter's session, registering the
/// agent, parking the card — is answered by the core in microseconds, so
/// this budget is never reached in a healthy daemon. It exists because the
/// alternative is a `oneshot` that never resolves: a core that is wedged
/// (or, in a unit test, not there at all) would otherwise hold the CLI's
/// hook open until the CLI's own timeout killed it, which is precisely the
/// "a hook must never wait on a bug of ours" rule.
pub const CORE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// One core round trip under `CORE_BUDGET`. `None` is "the core did not
/// answer", which every caller turns into `{}`.
///
/// `pub(crate)` because the Claude door has exactly the same three round
/// trips on exactly the same critical path (`api::hooks`), and a second
/// copy of this would be a second place to forget it.
pub(crate) async fn budgeted<T>(call: impl std::future::Future<Output = T>) -> Option<T> {
    match tokio::time::timeout(CORE_BUDGET, call).await {
        Ok(value) => Some(value),
        Err(_) => {
            tracing::warn!("the core did not answer a hook within its budget");
            None
        }
    }
}

/// What a door hands an adapter: the payload plus what the helper resolved
/// about the process that ran the hook (contract §4).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HookCall {
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub tty: Option<String>,
    #[serde(default)]
    pub cli_pid: Option<u32>,
    #[serde(default)]
    pub pid_kind: Option<PidKind>,
}

/// Everything an adapter may know besides the payload.
#[derive(Clone, Debug)]
pub struct HookCtx {
    pub env: BTreeMap<String, String>,
    pub tty: Option<String>,
    pub cli_pid: Option<u32>,
    /// `unknown` unless the helper's walk found a CLI ancestor: the daemon
    /// polls `cli` pids only, so an unset value is never polled.
    pub pid_kind: PidKind,
    /// The agent this session is already on the roster as, when it is.
    pub agent: Option<AgentId>,
    /// `[hooks.<cli>] hold_s`: what a held card of this CLI is given, per
    /// card (B2), because the store has one global budget and three CLIs.
    pub hold_s: u32,
}

impl HookCtx {
    /// `pub(super)` so the adapters beside this module can build one in
    /// their own tests: they are the layer's only callers.
    pub(super) fn of(call: &HookCall, hold_s: u32) -> HookCtx {
        HookCtx {
            env: call.env.clone(),
            tty: call.tty.clone(),
            cli_pid: call.cli_pid,
            pid_kind: call.pid_kind.unwrap_or(PidKind::Unknown),
            agent: None,
            hold_s,
        }
    }

    /// The terminal the hook ran in, as `agent.register` takes it. Never a
    /// window title: §5.5 captures identity from the environment only.
    #[must_use]
    pub fn terminal(&self) -> Terminal {
        Terminal {
            program: self.env.get("TERM_PROGRAM").cloned(),
            tty: self.tty.clone(),
            tmux_pane: self.env.get("TMUX_PANE").cloned(),
            iterm_session: self.env.get("ITERM_SESSION_ID").cloned(),
            term_session: self.env.get("TERM_SESSION_ID").cloned(),
            title_stamp: None,
            // Codex exports no inbox; its orders ride the app-server
            // (`turn/steer`) or the injector.
            inbox: None,
        }
    }
}

/// What one payload normalises to.
pub enum Ingest {
    /// Report-only: the handler writes `{}` **first** and enqueues these.
    Report(Vec<Request>),
    /// A question: park a held card and answer when it resolves.
    Held {
        cmd: CardSet,
        /// The cards that follow this one in a question group, issued one
        /// at a time as each is answered. Codex has no group event in v1
        /// (`requestUserInput` is app-server only), so it is always `None`
        /// here; Claude's `AskUserQuestion` is what it exists for.
        group: Option<GroupPlan>,
    },
    /// A dedupe hit, an unknown event, a suppressed notification. Answer
    /// `{}` and touch nothing: an adapter that panicked or guessed on an
    /// event it did not know would take the CLI down with it (C10).
    Nothing,
}

/// The rest of a question group, in order.
pub struct GroupPlan {
    pub rest: Vec<CardSet>,
}

/// The JSON a CLI expects on stdout (or as the HTTP body).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOutput {
    /// `{}`: "this hook decided nothing", which every CLI reads as
    /// "carry on and show your own dialog".
    Empty,
    Json(serde_json::Value),
}

impl HookOutput {
    #[must_use]
    pub fn value(self) -> serde_json::Value {
        match self {
            HookOutput::Empty => empty(),
            HookOutput::Json(value) => value,
        }
    }
}

/// The empty hook output, the answer on every failure path.
#[must_use]
pub fn empty() -> serde_json::Value {
    serde_json::json!({})
}

/// One CLI's normaliser. Every method is a pure function of its arguments
/// except for an adapter's own bounded dedupe memo, which is the one piece
/// of state the job actually needs (two sources report the same turn).
pub trait Adapter: Sync {
    fn kind(&self) -> AdapterKind;
    /// What this adapter calls itself in `session.hello`/`agent.list`.
    fn client(&self) -> &'static str;
    /// The idempotency key of the CLI session a payload belongs to
    /// (`"codex:<session_id>"`), or `None` when the payload names none.
    fn key(&self, payload: &serde_json::Value) -> Option<String>;
    /// The registration this event justifies when the agent is not on the
    /// roster yet, or `None` when it must not create one (a `SessionEnd`
    /// for a session we never saw must not resurrect it).
    fn register(&self, payload: &serde_json::Value, ctx: &HookCtx) -> Option<AgentRegister>;
    fn ingest(&self, payload: &serde_json::Value, ctx: &HookCtx) -> Ingest;
    /// The hook output for a resolved hold: `None` is a release (snooze,
    /// expiry, the client going away), which answers `{}` and lets the
    /// CLI's own dialog appear.
    fn answer_to_output(&self, card: &Card, ans: Option<&Answer>) -> HookOutput;
}

/// The per-daemon state every adapter shares. It hangs off `CoreHandle`
/// because that is the one thing both doors already carry into
/// `api::dispatch`, and because an adapter's session must be the same one
/// for the UDS helper and for `POST /v1/hooks/{cli}`.
#[derive(Debug, Default)]
pub struct Adapters {
    sessions: std::sync::Mutex<BTreeMap<AdapterKind, SessionId>>,
    reports: OnceLock<tokio::sync::mpsc::Sender<Job>>,
    hold_s: HoldBudgets,
    pub codex: codex::Codex,
    pub claude_channels: std::sync::Arc<crate::claude_channel::Hub>,
    /// Agent → the Claude session's message inbox (`crate::inbox`): the
    /// socket path and the per-session **token**. The token is why this
    /// lives here and not on the agent: a snapshot is redacted per scope
    /// and fanned out to every observer, while this object is shared only
    /// between the doors (which learn the inbox from a hook's environment)
    /// and the core (which posts to it). Dropped with the agent.
    inboxes: std::sync::Mutex<BTreeMap<AgentId, crate::inbox::Inbox>>,
    /// Agent keys (`codex:<thread id>`) the app-server daemon client
    /// (`crate::codexd`) is driving. A hook for one of these is answered
    /// `{}` and otherwise ignored: the daemon knows more than the hook
    /// does (approvals, questions, the turn), and two owners flipping one
    /// agent's capabilities back and forth would be worse than either.
    daemon_threads: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Whether that client holds a live connection to Codex's daemon right
    /// now: set once its handshake is done, cleared when the connection
    /// ends. The Codex row of `sources.list` reads it (`care::sources`).
    codex_connected: AtomicBool,
}

/// `[hooks.<cli>] hold_s`, refreshed by `Core::set_config` so a hot
/// reload reaches the next card (B2).
#[derive(Debug)]
struct HoldBudgets {
    claude: AtomicU32,
    codex: AtomicU32,
    gemini: AtomicU32,
}

impl Default for HoldBudgets {
    fn default() -> Self {
        HoldBudgets {
            claude: AtomicU32::new(120),
            codex: AtomicU32::new(120),
            gemini: AtomicU32::new(120),
        }
    }
}

struct Job {
    principal: Principal,
    session: SessionId,
    reqs: Vec<Request>,
}

impl Adapters {
    /// The budget a held card of this CLI is given.
    #[must_use]
    pub fn hold_s(&self, kind: AdapterKind) -> u32 {
        match kind {
            AdapterKind::Codex => self.hold_s.codex.load(Ordering::Relaxed),
            AdapterKind::Gemini => self.hold_s.gemini.load(Ordering::Relaxed),
            _ => self.hold_s.claude.load(Ordering::Relaxed),
        }
    }

    /// `crate::codexd` took this thread over.
    pub fn claim_thread(&self, key: &str) {
        if let Ok(mut threads) = self.daemon_threads.lock() {
            threads.insert(key.to_string());
        }
    }

    /// …and let it go (the thread unloaded, or the daemon connection ended).
    pub fn release_thread(&self, key: &str) {
        if let Ok(mut threads) = self.daemon_threads.lock() {
            threads.remove(key);
        }
    }

    /// Is this agent key the daemon client's to drive?
    #[must_use]
    pub fn daemon_owns(&self, key: &str) -> bool {
        self.daemon_threads
            .lock()
            .is_ok_and(|threads| threads.contains(key))
    }

    /// `crate::codexd` finished its handshake with Codex's daemon (`true`),
    /// or its connection ended (`false`).
    pub fn set_codex_connected(&self, connected: bool) {
        self.codex_connected.store(connected, Ordering::Relaxed);
    }

    /// Whether `crate::codexd` is connected to Codex's daemon now.
    #[must_use]
    pub fn codex_connected(&self) -> bool {
        self.codex_connected.load(Ordering::Relaxed)
    }

    /// Remember where this agent's session reads peer messages. A later
    /// hook from the same session overwrites: a token rotates with a
    /// resumed session, and the newest hook saw the newest environment.
    pub fn set_inbox(&self, agent: AgentId, inbox: crate::inbox::Inbox) {
        if let Ok(mut inboxes) = self.inboxes.lock() {
            inboxes.insert(agent, inbox);
        }
    }

    /// The inbox the core posts an order to, if the agent has one.
    #[must_use]
    pub fn inbox(&self, agent: &AgentId) -> Option<crate::inbox::Inbox> {
        self.inboxes.lock().ok()?.get(agent).cloned()
    }

    /// The agent is gone (`agent.removed`): its token must not outlive it.
    pub fn forget_inbox(&self, agent: &AgentId) {
        if let Ok(mut inboxes) = self.inboxes.lock() {
            inboxes.remove(agent);
        }
    }

    /// Called from `Core::set_config`, on startup and on every hot reload.
    pub fn set_hold_s(&self, claude: u32, codex: u32, gemini: u32) {
        self.hold_s.claude.store(claude, Ordering::Relaxed);
        self.hold_s.codex.store(codex, Ordering::Relaxed);
        self.hold_s.gemini.store(gemini, Ordering::Relaxed);
    }

    /// This adapter's internal session, opened on first use.
    pub async fn session(
        &self,
        core: &CoreHandle,
        kind: AdapterKind,
        client: &str,
    ) -> Option<SessionId> {
        if let Some(id) = self.sessions.lock().ok()?.get(&kind) {
            return Some(*id);
        }
        let open = crate::core::handle::OpenSession {
            kind: crate::core::handle::SessionKind::Internal,
            principal: Principal::Adapter(kind),
            scopes: vec![deck_model::Scope::Agent, deck_model::Scope::Observe],
            trusted: false,
            client: crate::api::session::ClientInfo {
                name: client.into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };
        let id = budgeted(core.open_session(open)).await?.ok()?;
        // Another hook may have raced us here; the first id wins so the
        // roster never sees two owners for one CLI.
        let mut sessions = self.sessions.lock().ok()?;
        Some(*sessions.entry(kind).or_insert(id))
    }

    /// Enqueue report-only requests. Returns at once: the CLI is waiting.
    pub fn report(
        &self,
        core: &CoreHandle,
        principal: Principal,
        session: SessionId,
        reqs: Vec<Request>,
    ) {
        if reqs.is_empty() {
            return;
        }
        let tx = self.reports.get_or_init(|| {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Job>(REPORT_QUEUE);
            let core = core.clone();
            tokio::spawn(async move {
                while let Some(job) = rx.recv().await {
                    for req in job.reqs {
                        // A refusal here is the agent having gone away
                        // between the hook and the queue; it is not the
                        // CLI's problem and was answered `{}` long ago.
                        if let Err(e) = core
                            .api(job.principal.clone(), job.session, false, req)
                            .await
                        {
                            tracing::debug!(code = e.code, "an adapter report was refused");
                        }
                    }
                }
            });
            tx
        });
        if tx
            .try_send(Job {
                principal,
                session,
                reqs,
            })
            .is_err()
        {
            tracing::warn!("the adapter report queue is full; a report was dropped");
        }
    }
}

/// The agent a CLI session is on the roster as.
#[must_use]
pub fn agent_by_key(snapshot: &Snapshot, key: &str) -> Option<AgentId> {
    snapshot
        .agents
        .values()
        .find(|a| a.session.key.as_deref() == Some(key))
        .map(|a| a.id.clone())
}

/// One hook event, end to end: resolve the session and the agent,
/// normalise, then either answer `{}` and enqueue, or park.
///
/// The [`Delivery`] beside the JSON is §4.9's seam: a held card is cleared
/// by the door that wrote the response, never here (see [`held`]).
pub async fn handle(
    core: &CoreHandle,
    adapter: &dyn Adapter,
    call: HookCall,
) -> (serde_json::Value, Option<Delivery>) {
    let kind = adapter.kind();
    let principal = Principal::Adapter(kind);
    let adapters = core.adapters();
    let Some(session) = adapters.session(core, kind, adapter.client()).await else {
        // No core, no session, no card — and a CLI that is not kept
        // waiting for any of it.
        return (empty(), None);
    };
    let mut ctx = HookCtx::of(&call, adapters.hold_s(kind));
    if let Some(key) = adapter.key(&call.payload) {
        // A thread the app-server daemon client is driving: the hook has
        // nothing to add, and must not answer a permission the daemon
        // path is already asking about.
        if adapters.daemon_owns(&key) {
            return (empty(), None);
        }
        ctx.agent = agent_by_key(&core.bus().snapshot(), &key);
        if ctx.agent.is_none() {
            if let Some(register) = adapter.register(&call.payload, &ctx) {
                ctx.agent = registered(core, &principal, session, register).await;
            }
        }
    }
    if kind == AdapterKind::Claude {
        if let (Some(agent), Some(model)) = (ctx.agent.clone(), call.payload.get("model").and_then(|m|
            m.get("display_name").or_else(|| m.get("id")).and_then(serde_json::Value::as_str))) {
            let controls = deck_model::session_controls::SessionControls {
                model: model.into(),
                effort: call.payload.get("effort").and_then(serde_json::Value::as_str).unwrap_or_default().into(),
                note: "Use /model or /effort in Claude Code. Channels does not enable these controls.".into(),
                ..Default::default()
            };
            let _ = core.send(crate::core::CoreMsg::SessionControls { agent, session, settled: false, controls }).await;
        }
    }
    match adapter.ingest(&call.payload, &ctx) {
        Ingest::Nothing => (empty(), None),
        Ingest::Report(reqs) => {
            adapters.report(core, principal, session, reqs);
            (empty(), None)
        }
        Ingest::Held { cmd, group } => held(core, adapter, principal, session, cmd, group).await,
    }
}

/// `agent.register`, returning the id it allocated.
async fn registered(
    core: &CoreHandle,
    principal: &Principal,
    session: SessionId,
    register: AgentRegister,
) -> Option<AgentId> {
    let call = core.api(
        principal.clone(),
        session,
        false,
        Request::AgentRegister(register),
    );
    match budgeted(call).await? {
        Ok(result) => serde_json::from_value(result["agent"].clone()).ok(),
        Err(e) => {
            tracing::debug!(code = e.code, "a hook could not register its agent");
            None
        }
    }
}

/// Park a held card and answer when the operator does — or when the hold
/// is released without a decision, which is the CLI's own dialog's cue.
///
/// The answer's card is **not** cleared here. What the door does with the
/// [`Delivery`] that comes back is report `core.delivered(agent, card,
/// ok)` once the response is on the wire, so a write that failed fails the
/// card `undeliverable` rather than clearing it as delivered (§4.9).
async fn held(
    core: &CoreHandle,
    adapter: &dyn Adapter,
    principal: Principal,
    session: SessionId,
    cmd: CardSet,
    group: Option<GroupPlan>,
) -> (serde_json::Value, Option<Delivery>) {
    // v1 issues one card at a time; a group's later cards are the Claude
    // adapter's business and are not reachable from here yet.
    debug_assert!(group.is_none() || !group.as_ref().is_some_and(|g| g.rest.is_empty()));
    let agent = cmd.agent.clone();
    let sent = cmd.clone();
    let parked = budgeted(core.set_card_held(principal.clone(), session, cmd)).await;
    let (card, hold, mut guard) =
        match parked.unwrap_or_else(|| Err(deck_model::ApiError::internal("core unavailable"))) {
            Ok(parked) => parked,
            Err(e) => {
                // Over `max_held_hooks`, the agent gone, a card we built
                // wrong: all three fail open (B4 makes the reason legible).
                tracing::debug!(code = e.code, "a held hook was refused; failing open");
                return (empty(), None);
            }
        };
    match hold.await {
        Ok(HoldResult::Answer(answer)) => {
            let card_now =
                card_as_stored(core, &agent, card).unwrap_or_else(|| as_card(card, &sent));
            let out = adapter.answer_to_output(&card_now, Some(&answer));
            // The card is cleared only once the decision is written: an
            // `op.answer` does not return `delivered:true` until then, and
            // the door is the only thing that knows whether it was.
            guard.disarm();
            (out.value(), Some(Delivery { agent, card }))
        }
        Ok(HoldResult::Released { reason }) => {
            guard.disarm();
            tracing::debug!(?reason, "a held hook was released without a decision");
            // §5.6's degraded card is the **store's** own work: every
            // release that leaves a card on the deck runs
            // `Store::degrade_card`, which keeps the id (no new chime, the
            // drawer entry keeps its place) and makes the wire encoder
            // draw it as `⌨ <text>` with the one `Focus terminal` row.
            //
            // So there is nothing to re-push here. A re-push that added
            // the `⌨ ` itself put a *second* prefix on the line the deck
            // received, because the encoder adds one to every degraded
            // card and a same-id re-push does not change the status. It
            // also spent a whole core round trip on the CLI's hook latency
            // path, and the Claude adapter — which does not re-push — drew
            // the correct single prefix, so the two doors disagreed.
            (empty(), None)
        }
        // The core dropped the sender (shutdown, or a park that displaced
        // this one). The guard stays armed so the core hears about it.
        Err(_) => (empty(), None),
    }
}

/// The card as the store holds it — the source of truth for `choice_map`
/// when more options were sent than the deck can show.
fn card_as_stored(core: &CoreHandle, agent: &AgentId, card: CardId) -> Option<Card> {
    core.bus()
        .snapshot()
        .agents
        .get(agent)
        .and_then(|a: &Agent| a.card.clone())
        .filter(|c| c.id == card)
}

/// The card we asked for, when the store's copy is already gone. Only the
/// fields an adapter reads back are meaningful; `choice_map` is empty,
/// which is the identity mapping and correct for every card with at most
/// `MAX_CHOICES` options.
fn as_card(id: CardId, sent: &CardSet) -> Card {
    Card {
        id,
        kind: sent.kind,
        status: deck_model::CardStatus::Pending,
        deliver: Deliver::Hold,
        origin: sent.origin.clone(),
        text: Some(sent.text.clone()),
        choices: sent.choices.clone(),
        choices_n: u8::try_from(sent.choices.len()).unwrap_or(u8::MAX),
        choice_map: Vec::new(),
        rec: sent.rec,
        free_text: sent.free_text,
        page: deck_model::Page { index: 0, count: 1 },
        created: deck_model::Ts(0),
        expires: None,
        redacted: false,
    }
}

/// What a degraded card's line starts with (§5.6).
///
/// The **wire encoder** puts it there, once, for every card the store has
/// degraded (`deck-model::store::card_line`): an adapter never writes it
/// into a card's text, or the deck draws it twice.
pub const DEGRADED_PREFIX: &str = "⌨ ";
/// The one choice a degraded card offers.
pub const FOCUS_TERMINAL: &str = "Focus terminal";

/// The §5.6 permission card every adapter builds the same way: two or
/// three choices, `Allow` recommended, held, `origin.kind: permission`.
#[must_use]
pub fn permission_card(
    agent: AgentId,
    text: &str,
    labels: &[&str],
    tool: Option<String>,
    request_id: Option<String>,
    hold_s: u32,
) -> CardSet {
    CardSet {
        agent,
        id: None,
        kind: CardKind::Q,
        text: sanitize::text(text),
        choices: sanitize::labels(&labels.iter().map(|l| (*l).to_string()).collect::<Vec<_>>())
            .into_iter()
            .map(|label| Choice {
                label,
                detail: None,
            })
            .collect(),
        rec: Some(0),
        free_text: true,
        deliver: Deliver::Hold,
        timeout_s: Some(hold_s),
        origin: Origin {
            kind: OriginKind::Permission,
            tool,
            request_id,
            group: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::CardStatus;

    fn a_set() -> CardSet {
        permission_card(
            AgentId("ag_1".into()),
            "Bash: rm -rf build",
            &["Allow", "Deny"],
            Some("Bash".into()),
            Some("turn_1".into()),
            90,
        )
    }

    #[test]
    fn a_permission_card_is_held_recommends_allow_and_is_permission_origin() {
        let set = a_set();
        assert_eq!(set.deliver, Deliver::Hold);
        assert_eq!(set.origin.kind, OriginKind::Permission);
        assert_eq!(set.rec, Some(0));
        assert_eq!(
            set.timeout_s,
            Some(90),
            "the CLI's own budget, not the store's"
        );
        assert_eq!(
            set.choices
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Allow", "Deny"]
        );
        assert!(set.free_text, "PTT answers a permission card by voice");
    }

    /// The card an adapter sends carries **no** `⌨ `: the prefix belongs
    /// to the encoder, which adds one to every degraded card. An adapter
    /// that wrote it into the text too would put two on the deck's line.
    #[test]
    fn a_sent_card_never_carries_the_degrade_prefix_itself() {
        let set = a_set();
        assert!(!set.text.starts_with(DEGRADED_PREFIX), "{}", set.text);
        assert_eq!(set.text, "Bash: rm -rf build");
        // And the one degraded card an adapter does build by hand — the
        // Claude focus card — says the same.
        assert_eq!(FOCUS_TERMINAL, "Focus terminal");
    }

    #[test]
    fn the_synthesised_card_carries_what_an_adapter_reads_back() {
        let set = a_set();
        let card = as_card(CardId(3), &set);
        assert_eq!(card.id, CardId(3));
        assert_eq!(card.choices.len(), 2);
        assert_eq!(card.choices_n, 2);
        assert!(card.choice_map.is_empty(), "identity for ≤4 options");
        assert_eq!(card.origin.kind, OriginKind::Permission);
        assert!(matches!(card.status, CardStatus::Pending));
    }

    #[test]
    fn the_hold_budget_is_per_cli_and_hot() {
        let adapters = Adapters::default();
        assert_eq!(adapters.hold_s(AdapterKind::Codex), 120);
        adapters.set_hold_s(30, 200, 45);
        assert_eq!(adapters.hold_s(AdapterKind::Claude), 30);
        assert_eq!(adapters.hold_s(AdapterKind::Codex), 200);
        assert_eq!(adapters.hold_s(AdapterKind::Gemini), 45);
    }

    #[test]
    fn nothing_is_found_in_an_empty_roster() {
        // The populated case needs a live store and is covered end to end
        // in `tests/adapters_codex.rs`.
        assert_eq!(agent_by_key(&Snapshot::default(), "codex:none"), None);
    }

    #[test]
    fn an_empty_output_is_the_empty_object() {
        assert_eq!(HookOutput::Empty.value(), serde_json::json!({}));
    }
}
