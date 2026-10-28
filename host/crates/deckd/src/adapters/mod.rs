//! The agent adapters (design §5): one per CLI, each a **pure normaliser**
//! that turns one hook payload into the §4.6 primitives the core already
//! knows how to run. An adapter owns no effect: no hold, no timer, no
//! delivery, no socket. It reads a payload, consults its own memo (the
//! dedupe windows of §5.1, the agent and card it saw last) and hands the
//! handler in `api::hooks` a plan. The handler is the only place that
//! talks to the core.
//!
//! Two shapes come out of `ingest`:
//!
//! * **report-only** — the CLI is not waiting on a decision, so the
//!   handler writes the answer **first** and enqueues the requests after
//!   (design §4.11; the http hooks are synchronous with `timeout: 2`, so
//!   a daemon that did the work first would put its own latency on every
//!   prompt the operator types). `Report` answers `{}`; `Beacon` answers a
//!   body — today only `SessionStart`'s one-shot title stamp (C25).
//! * **held** — the CLI *is* waiting: the handler parks on the card and
//!   the adapter turns the answer back into the CLI's own hook output.
//!
//! Every plan names its agent through [`PENDING`]: the core allocates
//! agent ids, so an adapter cannot know one. The first request of a plan
//! is the `agent.register` that resolves the CLI's session key to an id
//! (registration is idempotent by `key` from the owning session, §4.6),
//! and [`address`] writes that id into everything behind it.
//!
//! **Content discipline (§11).** A payload, a prompt, a tool input and a
//! title are the most sensitive data the daemon ever holds. Nothing here
//! logs any of them unless `[privacy] log_content`; the `note` an adapter
//! carries from `ingest` to `answer_to_output` is opaque to the handler
//! for the same reason.
//! **Two plan layers, one door.** This module is the layer the Claude
//! adapter is written against: `ingest` is pure and the handler in
//! `api::hooks` owns every effect. [`rt`] is the second — the Codex
//! adapter's, which resolves its agent inline and runs its own plan
//! (`rt::handle`). The two were built in parallel against the same
//! contract §5 and differ in where the effects sit, not in what reaches
//! a CLI: both answer `{}` first for a report, both park one held card
//! at a time, both degrade a released card to `⌨ Focus terminal` under
//! its own id. Folding them into one is week-4 work; nothing in either
//! door depends on the other, so the merge keeps both rather than
//! rewriting a tested adapter against the other's trait.
pub mod claude;
pub mod codex;
pub mod rt;

use crate::config::Config;
use arc_swap::ArcSwap;
use deck_model::{AgentId, Answer, CardSet, PidKind, Request, SessionId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// The agent id an adapter writes where the handler must substitute the
/// one its `agent.register` resolved.
///
/// The core allocates ids (`ag_…`, `AgentId::from_seed`) and a store write
/// refuses an unknown agent with `-32003`, so the empty string can never
/// name a real agent and an unaddressed request can never reach one by
/// accident.
pub const PENDING: &str = "";

/// The placeholder agent of a plan that has not been addressed yet.
#[must_use]
pub fn pending() -> AgentId {
    AgentId(PENDING.to_string())
}

/// Whether this id is still the placeholder.
#[must_use]
pub fn is_pending(id: &AgentId) -> bool {
    id.0 == PENDING
}

/// Point one request of a plan at the agent the register resolved.
///
/// Only a [`PENDING`] id is rewritten: an adapter that already knows the
/// agent (a `SessionEnd` whose session is in the memo) keeps it, and no
/// plan can be re-aimed at somebody else's agent by a later register.
pub fn address(req: &mut Request, agent: &AgentId) {
    match req {
        Request::AgentUpdate(u) => replace(&mut u.agent, agent),
        Request::CompanionSet(c) => replace(&mut c.agent, agent),
        Request::SessionSet(c) => replace(&mut c.agent, agent),
        Request::AgentRelease { agent: a, .. }
        | Request::AgentFocus { agent: a }
        | Request::OpFocus { agent: a }
        | Request::OpSnooze { agent: a, .. }
        | Request::OpAck { agent: a, .. } => replace(a, agent),
        Request::CardSet(c) => replace(&mut c.agent, agent),
        Request::CardClear(c) => replace(&mut c.agent, agent),
        Request::Reply(r) => replace(&mut r.agent, agent),
        Request::OpAnswer(a) => replace(&mut a.agent, agent),
        Request::OpOrder(o) => replace(&mut o.agent, agent),
        Request::VoiceListen(l) => replace(&mut l.agent, agent),
        Request::VoiceSay(r) => {
            if let Some(a) = r.agent.as_mut() {
                replace(a, agent);
            }
        }
        Request::AgentHeartbeat { agents } => {
            for a in agents.iter_mut() {
                replace(a, agent);
            }
        }
        Request::FocusReport(f) => {
            if let Some(a) = f.agent.as_mut() {
                replace(a, agent);
            }
        }
        // No agent to address: the register itself, a claim by key, and
        // the device surface.
        Request::AgentRegister(_)
        | Request::AgentClaim { .. }
        | Request::DeviceSet(_)
        | Request::DeviceFacesSet { .. }
        | Request::DeviceUsageSet { .. }
        | Request::DeviceMacroSet { .. }
        | Request::DeviceWsSet { .. }
        | Request::DeviceName { .. }
        // The voice methods that name nothing: a stop and a cancel are
        // about the daemon's own audio, not about an agent.
        | Request::VoiceStop { .. }
        | Request::VoiceCancel {} => {}
    }
}

fn replace(slot: &mut AgentId, agent: &AgentId) {
    if is_pending(slot) {
        *slot = agent.clone();
    }
}

/// The agent a request names, once it has been addressed.
#[must_use]
pub fn agent_of(req: &Request) -> Option<&AgentId> {
    match req {
        Request::AgentUpdate(u) => Some(&u.agent),
        Request::CompanionSet(c) => Some(&c.agent),
        Request::SessionSet(c) => Some(&c.agent),
        Request::AgentRelease { agent, .. }
        | Request::AgentFocus { agent }
        | Request::OpFocus { agent }
        | Request::OpSnooze { agent, .. }
        | Request::OpAck { agent, .. } => Some(agent),
        Request::CardSet(c) => Some(&c.agent),
        Request::CardClear(c) => Some(&c.agent),
        Request::Reply(r) => Some(&r.agent),
        Request::OpAnswer(a) => Some(&a.agent),
        Request::OpOrder(o) => Some(&o.agent),
        _ => None,
    }
}

/// One hook payload as it left the CLI: the event JSON, verbatim.
///
/// A newtype rather than a bare `Value` so a payload cannot be passed to
/// something that formats it: `Debug` prints the event name and nothing
/// else (§11).
#[derive(Clone)]
pub struct HookPayload(pub serde_json::Value);

impl HookPayload {
    #[must_use]
    pub fn event(&self) -> &str {
        self.0
            .get("hook_event_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    }

    /// A string field of the payload, empty and absent alike reported as
    /// `None`.
    #[must_use]
    pub fn str(&self, field: &str) -> Option<&str> {
        self.0
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    }

    #[must_use]
    pub fn get(&self, field: &str) -> Option<&serde_json::Value> {
        self.0.get(field)
    }
}

impl std::fmt::Debug for HookPayload {
    /// The event name only: the body is content (§11).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookPayload")
            .field("event", &self.event())
            .finish_non_exhaustive()
    }
}

/// The knobs of `[hooks.<cli>]` an adapter reads, resolved per call so a
/// hot reload reaches the next hook (§9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookCfg {
    /// The hold budget in seconds. It reaches the store as
    /// `CardSet.timeout_s` — the adapter's input, never a store setting
    /// (B2), which is what gives the two CLIs different budgets.
    pub hold_s: u32,
    /// `[hooks.claude] tool_titles`: a `PostToolUse` title per tool call.
    pub tool_titles: bool,
}

impl Default for HookCfg {
    fn default() -> Self {
        HookCfg {
            hold_s: 120,
            tool_titles: false,
        }
    }
}

/// What the helper captured around a payload (§4 of the week-3 contract).
///
/// An http hook carries none of it — Claude posts the event JSON and
/// nothing else — so every field is optional and the adapter must not
/// overwrite what an earlier `SessionStart` learned with an empty value.
#[derive(Clone, Debug, Default)]
pub struct HookCtx {
    pub env: BTreeMap<String, String>,
    pub tty: Option<String>,
    pub cli_pid: Option<u32>,
    pub pid_kind: Option<PidKind>,
    /// The clock, injected: `ingest` decides dedupe windows from it and a
    /// test drives them without sleeping.
    pub now: Option<Instant>,
    pub cfg: HookCfg,
}

impl HookCtx {
    #[must_use]
    pub fn now(&self) -> Instant {
        self.now.unwrap_or_else(Instant::now)
    }

    /// One of the captured environment variables.
    #[must_use]
    pub fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).filter(|v| !v.is_empty()).cloned()
    }
}

/// What a CLI reads back from its hook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOutput {
    /// `{}` — the explicit no-op. A 2xx with an empty body means the same
    /// to Claude, but `{}` is what the doors write: it is unambiguous on
    /// stdout too, where an empty write is "the hook printed nothing".
    Empty,
    Json(serde_json::Value),
}

impl HookOutput {
    #[must_use]
    pub fn json(self) -> serde_json::Value {
        match self {
            HookOutput::Empty => serde_json::json!({}),
            HookOutput::Json(value) => value,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, HookOutput::Empty)
    }
}

/// The cards after the first one of a question group, and what the
/// adapter needs to answer the CLI once they are all in.
///
/// The store gives **each** group card its own hold (there is no one
/// long-lived hold spanning a group); what it shares is the deadline,
/// inherited through `aux.hold_at` for every card whose `origin.group`
/// index is above zero. So the accumulator lives here, in the adapter,
/// and the single response is written after the last card (§5.1,
/// facts-seams §2e).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GroupPlan {
    /// Cards 1..n, in order, each still [`PENDING`].
    pub rest: Vec<CardSet>,
    /// One entry per card, in the order of `[cmd] ++ rest`.
    pub questions: Vec<GroupQuestion>,
}

/// One question of a group: where its answer is filed, and what the CLI
/// offered for it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GroupQuestion {
    /// The key the answer is filed under — for Claude the `question`
    /// text, never the `header` (§2.3).
    pub key: String,
    /// The option labels **as the CLI sent them**.
    ///
    /// The labels that reach the store are §5.6's: cut to
    /// `sanitize::LABEL_MAX` (32 bytes) with `…` and disambiguated with
    /// `#n` when two cuts collide, because that is the deck's line budget.
    /// An answer therefore carries a label the CLI has never seen, and an
    /// `AskUserQuestion` answer must be one of the options that were
    /// offered — so the originals are kept here and `Answer.choice` is
    /// mapped back through them (B10's mapping, on the label side). Empty
    /// for a question whose labels were never rewritten.
    pub options: Vec<String>,
}

impl GroupPlan {
    /// The original label of the option the operator picked on card `k`.
    ///
    /// `choice` is an index into the list the **deck** showed. `map` is
    /// the card's `choice_map` (shown → sent, B10), empty when nothing was
    /// truncated, which is every Claude card (C7 caps `options` at 4).
    #[must_use]
    pub fn option(&self, k: usize, choice: u8, map: &[u8]) -> Option<&str> {
        let shown = usize::from(choice);
        let sent = if map.is_empty() {
            shown
        } else {
            usize::from(*map.get(shown)?)
        };
        self.questions.get(k)?.options.get(sent).map(String::as_str)
    }
}

/// What one payload asks the handler to do.
#[derive(Clone, Debug)]
pub enum Ingest {
    /// Report-only: the handler writes `{}` **first**, then enqueues
    /// these. The first request is the `agent.register` that resolves the
    /// agent for the rest ([`address`]).
    Report(Vec<Request>),
    /// Report-only with a body. Only `SessionStart` uses it: the one-shot
    /// `terminalSequence` beacon (C25), which must reach the CLI even
    /// though nothing waits on the daemon.
    Beacon(HookOutput, Vec<Request>),
    /// The CLI is waiting. `before` runs first (its head is the register),
    /// then `cmd` is parked; `note` is whatever `answer_to_output` needs
    /// and is opaque to the handler; `group` carries the rest of a
    /// question group.
    Held {
        before: Vec<Request>,
        cmd: CardSet,
        note: serde_json::Value,
        group: Option<GroupPlan>,
    },
    /// A dedupe hit, an unknown event, a suppressed `Notification`, an
    /// event this adapter does not act on: answer `{}` and do nothing.
    Nothing,
}

impl Ingest {
    /// The answer the handler writes before it does any work.
    #[must_use]
    pub fn report_output(&self) -> HookOutput {
        match self {
            Ingest::Beacon(out, _) => out.clone(),
            _ => HookOutput::Empty,
        }
    }
}

/// One CLI's normaliser (design §5).
///
/// Every method is pure with respect to the daemon: none touches a core,
/// a socket or a clock but the one in [`HookCtx`]. `ingest` may consult
/// and update the adapter's own memo — the §5.1 dedupe windows are a
/// memo, not an effect — which is why it takes `&self` and not `&mut
/// self`: the doors call it from many tasks at once.
///
/// The `resolved`/`carded`/`uncarded`/`group_released` group is the
/// handler telling the adapter what the core answered, so one payload's
/// result can reach the next payload. They default to doing nothing: an
/// adapter that keeps no memo implements `ingest` and the two output
/// methods and is done.
pub trait Adapter: Send + Sync {
    /// Who this adapter's agents belong to. Unforgeable from outside: no
    /// door can build a `Principal::Adapter` (facts-seams §4).
    fn principal(&self) -> deck_model::Principal;

    /// The `client.name` of its internal session.
    fn client(&self) -> &'static str;

    /// That session, once the handler has opened it.
    fn session(&self) -> Option<SessionId>;

    /// Remember it. Two hooks may race to open one; the first wins and
    /// its id is what comes back.
    fn set_session(&self, session: SessionId) -> SessionId;

    /// Normalise one payload.
    fn ingest(&self, payload: &HookPayload, ctx: &HookCtx) -> Ingest;

    /// The agent the plan's `agent.register` resolved.
    fn resolved(&self, payload: &HookPayload, agent: &AgentId) {
        let _ = (payload, agent);
    }

    /// A card the plan created, with the `CardSet` it came from.
    fn carded(&self, payload: &HookPayload, cmd: &CardSet, card: deck_model::CardId) {
        let _ = (payload, cmd, card);
    }

    /// That card is off the deck: answered and cleared by `hold_written`.
    fn uncarded(&self, payload: &HookPayload, card: deck_model::CardId) {
        let _ = (payload, card);
    }

    /// A hold ended with **no decision** — a snooze, the budget, the
    /// handler going away, or the card being taken off the deck under it.
    ///
    /// `reason` is the difference that matters, and it is why this takes
    /// one: [`Release::degrades_card`] says whether the core left the card
    /// on the deck as `⌨ Focus terminal` under the same id, pointing at
    /// the dialog the CLI is still showing until the CLI's own turn moves
    /// on (§5.6), or whether the card is simply gone
    /// (`agent.card.clear`, a retire, an ack). An adapter that filed a
    /// cleared card as "still on the deck" would believe that session was
    /// carded for ever.
    ///
    /// [`Release::degrades_card`]: deck_model::Release::degrades_card
    fn released(
        &self,
        payload: &HookPayload,
        cmd: &CardSet,
        card: deck_model::CardId,
        reason: deck_model::Release,
        now: Instant,
    ) {
        let _ = (payload, cmd, card, reason, now);
    }

    /// The CLI's hook output for a resolved hold: the answer, or `None`
    /// for a release (snooze, expiry, the client going away), which is
    /// always `{}` — the CLI's own dialog is on screen and a second
    /// decision now would be an unattended approval.
    ///
    /// `note` and `cmd` are what `ingest` produced for this card: the
    /// handler never holds a `deck_model::Card` (the store owns it) and
    /// the response needs the event name (C2) and the input to echo
    /// (§2.3), neither of which a `Card` carries.
    fn answer_to_output(
        &self,
        note: &serde_json::Value,
        cmd: &CardSet,
        ans: Option<&Answer>,
    ) -> HookOutput;

    /// The one response written after **every** card of a question group.
    /// `answers[k]` is `None` for a card that was released.
    fn group_to_output(
        &self,
        note: &serde_json::Value,
        plan: &GroupPlan,
        answers: &[Option<Answer>],
    ) -> HookOutput {
        let _ = (note, plan, answers);
        HookOutput::Empty
    }
}

/// The adapters this daemon has, and the config they read.
///
/// One per `Api`, created at start-up and shared by both doors: an
/// adapter's memo (its internal session, the agent behind each CLI
/// session, the dedupe windows) has to outlive a single hook request, and
/// the two doors must share it — a `PermissionRequest` posted over TCP and
/// the `SessionStart` that came through the UDS helper are the same CLI
/// session.
pub struct Adapters {
    pub(crate) transcripts: crate::chat::Watches,
    pub(crate) config: Arc<ArcSwap<Config>>,
    /// Report-only plans run behind the answer that already went out
    /// (§4.11), so they are not on the hook's latency path — but they are
    /// still in an order that means something. This serialises them: a
    /// `Stop` that overtook its own `UserPromptSubmit` would leave the
    /// agent reading `working` after the turn ended.
    order: tokio::sync::Mutex<()>,
    pub claude: claude::Claude,
}

impl Default for Adapters {
    fn default() -> Self {
        Adapters::new(Arc::new(ArcSwap::from_pointee(Config::default())))
    }
}

impl std::fmt::Debug for Adapters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adapters").finish_non_exhaustive()
    }
}

impl Adapters {
    #[must_use]
    pub fn new(config: Arc<ArcSwap<Config>>) -> Self {
        Adapters {
            transcripts: Default::default(),
            config,
            order: tokio::sync::Mutex::new(()),
            claude: claude::Claude::default(),
        }
    }

    /// Hold this while a plan runs against the core, so plans land in the
    /// order their events arrived.
    pub async fn order(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.order.lock().await
    }

    /// `[hooks.<cli>]` as it stands right now (hot reload, §9), for the
    /// CLI the door routed.
    ///
    /// Per CLI, never per adapter: `hold_s` is the **adapter's** input
    /// (B2) and each CLI has its own key, so a door that read
    /// `[hooks.claude]` for every call would hand a Codex hook Claude's
    /// budget the moment a second adapter answered here. It cannot today
    /// — `adapter_of` maps `claude` alone and Codex is served on the other
    /// plan layer, which reads `rt::Adapters::hold_s(AdapterKind::Codex)`
    /// — and that is exactly why the lookup should not be spelled as a
    /// constant waiting for the third adapter to land on it.
    ///
    /// An unknown name falls back to the defaults rather than to Claude's
    /// config: a CLI this daemon has no `[hooks.<cli>]` table for has not
    /// opted into anybody else's budget.
    #[must_use]
    pub fn cfg(&self, cli: &str) -> HookCfg {
        let config = self.config.load();
        match cli {
            // The chained status line is the same CLI on another door.
            "claude" | "claude_statusline" => HookCfg {
                hold_s: config.hooks.claude.hold_s,
                tool_titles: config.hooks.claude.tool_titles,
            },
            // One adapter behind two doors (C17): the notify channel is
            // the same CLI and reads the same table.
            "codex" | "codex_notify" => HookCfg {
                hold_s: config.hooks.codex.hold_s,
                // `[hooks.codex]` has no `tool_titles`: Codex reports its
                // tool calls through `Stop`, not per call.
                tool_titles: false,
            },
            _ => HookCfg::default(),
        }
    }

    /// The Claude adapter's internal session, when a hook or the roster
    /// seed has already opened one.
    ///
    /// `None` is "no Claude hook has reached this daemon yet": with
    /// `[liveness] seed` off that is exactly when the poll has nothing to
    /// reconcile, and a daemon no CLI has spoken to keeps an empty session
    /// table. With seeding on the poll opens it through
    /// [`Self::claude_session_or_open`] the moment it finds a session to
    /// seed.
    #[must_use]
    pub fn claude_session(&self) -> Option<SessionId> {
        self.claude.session()
    }

    /// The Claude adapter's internal session, opened if no hook has done
    /// so yet — the same way the hook door opens it, so the agents the
    /// roster seeds and the hooks that follow share one owner. A daemon
    /// started after its CLI sessions has seen no hook at all, and a seed
    /// under any other session would meet `-32004 conflict` on the first
    /// hook of that session.
    pub async fn claude_session_or_open(
        &self,
        core: &crate::core::handle::CoreHandle,
    ) -> Option<SessionId> {
        crate::api::hooks::session_of(&self.claude, core).await.ok()
    }

    /// Whether the hook doors answer at all (`[hooks] enabled`).
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.config.load().hooks.enabled
    }

    /// Whether `[usage] providers` names `provider` (`crate::usage`).
    #[must_use]
    pub fn usage_enabled(&self, provider: crate::usage::Provider) -> bool {
        provider.enabled(&self.config.load().usage)
    }

    /// Whether a payload may be logged (`[privacy] log_content`, §11).
    #[must_use]
    pub fn log_content(&self) -> bool {
        self.config.load().privacy.log_content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{AgentUpdate, CardClear, CardId, ReplySet};

    fn update() -> Request {
        Request::AgentUpdate(AgentUpdate {
            agent: pending(),
            state: None,
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None,
        })
    }

    /// A plan names its agent through the placeholder, and addressing it
    /// is the only way a request in it can reach a real agent.
    #[test]
    fn addressing_a_plan_replaces_every_placeholder_and_nothing_else() {
        let mine = AgentId("ag_1".into());
        let theirs = AgentId("ag_2".into());
        let mut plan = vec![
            update(),
            Request::CardClear(CardClear {
                agent: pending(),
                card: CardId(7),
                delivered: false,
                reason: None,
            }),
            Request::Reply(ReplySet {
                message: None,
                agent: theirs.clone(),
                text: "already addressed".into(),
                speak: false,
                announce: None,
                lane: None,
            }),
            Request::AgentRelease {
                agent: pending(),
                reason: Some("session_end".into()),
            },
        ];
        assert!(plan.iter().filter_map(agent_of).any(is_pending));
        for req in &mut plan {
            address(req, &mine);
        }
        let named: Vec<&AgentId> = plan.iter().filter_map(agent_of).collect();
        assert_eq!(named, vec![&mine, &mine, &theirs, &mine]);
        assert!(
            !named.iter().any(|a| is_pending(a)),
            "an addressed plan has no placeholder left"
        );
        // A second register never re-aims a plan that is already pointed.
        for req in &mut plan {
            address(req, &AgentId("ag_9".into()));
        }
        assert_eq!(
            plan.iter().filter_map(agent_of).collect::<Vec<_>>(),
            vec![&mine, &mine, &theirs, &mine]
        );
    }

    /// §11: a payload never reaches a log line by being formatted.
    #[test]
    fn a_payload_debugs_as_its_event_name_alone() {
        let payload = HookPayload(serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_input": {"command": "rm -rf /secret"},
            "prompt": "the operator's own words",
        }));
        let printed = format!("{payload:?}");
        assert!(printed.contains("PreToolUse"), "{printed}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(!printed.contains("operator"), "{printed}");
        assert_eq!(payload.str("hook_event_name"), Some("PreToolUse"));
        assert_eq!(payload.str("nope"), None);
    }

    #[test]
    fn the_empty_output_is_the_empty_object() {
        assert_eq!(HookOutput::Empty.json(), serde_json::json!({}));
        assert_eq!(
            HookOutput::Json(serde_json::json!({"a": 1})).json(),
            serde_json::json!({"a": 1})
        );
        assert!(Ingest::Report(Vec::new()).report_output().is_empty());
        assert!(Ingest::Nothing.report_output().is_empty());
        let beacon = Ingest::Beacon(
            HookOutput::Json(serde_json::json!({"terminalSequence": "x"})),
            Vec::new(),
        );
        assert_eq!(
            beacon.report_output().json(),
            serde_json::json!({"terminalSequence": "x"})
        );
    }

    /// The per-CLI budget is the adapter's input (B2): it comes from the
    /// live config, so a `config.reload` reaches the next hook.
    #[test]
    fn the_hold_budget_follows_the_live_config() {
        let mut config = Config::default();
        config.hooks.claude.hold_s = 17;
        config.hooks.claude.tool_titles = true;
        let swap = Arc::new(ArcSwap::from_pointee(config));
        let adapters = Adapters::new(swap.clone());
        assert_eq!(
            adapters.cfg("claude"),
            HookCfg {
                hold_s: 17,
                tool_titles: true
            }
        );
        let mut next = Config::default();
        next.hooks.claude.hold_s = 5;
        next.hooks.enabled = false;
        swap.store(Arc::new(next));
        assert_eq!(adapters.cfg("claude").hold_s, 5);
        assert!(!adapters.enabled());
        assert!(!adapters.log_content());
        assert_eq!(Adapters::default().cfg("claude"), HookCfg::default());
    }

    /// The budget is looked up by the CLI the door routed, never by the
    /// adapter that happens to be first in the file. `[hooks.codex]
    /// hold_s` is its own key and a Codex hook must read *that* one: the
    /// CLIs' hook `timeout`s differ, and a card given the wrong budget
    /// either outlives the CLI's own timeout or is released under an
    /// operator who still had time.
    #[test]
    fn each_cli_reads_its_own_hooks_table() {
        let mut config = Config::default();
        config.hooks.claude.hold_s = 200;
        config.hooks.claude.tool_titles = true;
        config.hooks.codex.hold_s = 45;
        let adapters = Adapters::new(Arc::new(ArcSwap::from_pointee(config)));
        assert_eq!(adapters.cfg("claude").hold_s, 200);
        assert_eq!(adapters.cfg("codex").hold_s, 45);
        // The notify channel is the same CLI and the same table (C17).
        assert_eq!(adapters.cfg("codex_notify").hold_s, 45);
        // `tool_titles` is Claude's key alone; nothing else inherits it.
        assert!(adapters.cfg("claude").tool_titles);
        assert!(!adapters.cfg("codex").tool_titles);
        // A CLI with no table of its own gets the defaults, never
        // Claude's 200.
        assert_eq!(adapters.cfg("gemini"), HookCfg::default());
        assert_eq!(adapters.cfg(""), HookCfg::default());
    }
}
