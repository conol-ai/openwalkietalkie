//! The session registry inside the core (design §4.2, contract §3.2): ids
//! allocated monotonically from 2 (`SessionId(1)` is the demo's), one entry
//! per live connection, observer or logical HTTP session with what it may
//! do, its mailbox (poll answers, orders and snoozes waiting for an
//! `agent.wait`) and its parked waiters (one per agent). Logical sessions
//! are keyed `(token, client)` and expire `mailbox_ttl_s` after their last
//! call; mail older than that is dropped too.
use super::handle::{ClientInfo, Delivery, OpenSession, Reply, SessionKind, WaitResult};
use deck_model::{AgentId, Answer, ApiError, CardId, Principal, Scope, SessionId};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// The first id the registry hands out.
pub const FIRST_SESSION: u64 = 2;

/// `client.name` is a key of the logical-session map and is echoed in
/// `service.info`/`agent.list`: a caller may not make it arbitrarily long
/// (longer → `-32009 invalid{field:"client.name"}`).
pub const CLIENT_NAME_MAX: usize = 64;
/// How many logical HTTP sessions one bearer token may hold at once. A
/// token over the cap does not lose service: its least-recently-used
/// logical session is closed to make room (its agents are orphaned as if
/// the client had gone away), so a badly written script that varies
/// `client.name` churns instead of growing without bound.
pub const LOGICAL_PER_TOKEN_MAX: usize = 32;
/// How many logical sessions the daemon holds in total, across tokens.
/// Over it a *new* logical session is refused `-32008 rate_limited` rather
/// than evicting some other token's — one token can never push another
/// token's sessions out.
pub const LOGICAL_TOTAL_MAX: usize = 512;

/// An undelivered decision for a `poll` agent (§4.8), in the owning
/// session's mailbox until its `agent.wait` picks it up.
#[derive(Debug)]
pub struct Mail {
    pub agent: AgentId,
    /// When it was queued (pruned after `mailbox_ttl_s`).
    pub at: Instant,
    pub kind: MailKind,
}

#[derive(Debug)]
pub enum MailKind {
    Answer {
        card: CardId,
        answer: Answer,
    },
    /// An `op.order` by poll, or a chosen Ready step (`card` set, `ctx:"step"`).
    Order {
        text: String,
        ctx: String,
        choice: Option<u8>,
        card: Option<CardId>,
        /// The order's id; `None` for a step.
        order: Option<String>,
    },
    Snooze {
        card: CardId,
    },
}

impl Mail {
    /// The card the mail is about, if any.
    pub fn card(&self) -> Option<CardId> {
        match &self.kind {
            MailKind::Answer { card, .. } | MailKind::Snooze { card } => Some(*card),
            MailKind::Order { card, .. } => *card,
        }
    }

    /// A decision on a card: one per `(agent, card)` at a time (the newest
    /// wins: "dropped when its card is answered again", §4.8).
    fn is_decision(&self) -> bool {
        matches!(
            self.kind,
            MailKind::Answer { .. }
                | MailKind::Snooze { .. }
                | MailKind::Order { card: Some(_), .. }
        )
    }

    /// What the waiter receives.
    pub fn into_result(self) -> WaitResult {
        let agent = self.agent;
        match self.kind {
            MailKind::Answer { card, answer } => WaitResult::Answer {
                card,
                answer,
                delivery: Delivery { agent, card },
            },
            MailKind::Order {
                text,
                ctx,
                choice,
                card,
                order,
            } => WaitResult::Order {
                text,
                ctx,
                choice,
                card,
                order,
                delivery: card.map(|card| Delivery { agent, card }),
            },
            MailKind::Snooze { card } => WaitResult::Snooze { card },
        }
    }
}

/// The rules a queue of `Mail` follows (§4.8), shared by a session's own
/// mailbox and by the orphan mailbox a closed session leaves behind (§12).
pub mod mailbox {
    use super::{AgentId, CardId, Mail};
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    /// Queue a mail. A decision about a card replaces any earlier decision
    /// about the same `(agent, card)` (§4.8: one per decision, the late
    /// waiter and the second answer can never both fire).
    pub fn push(mailbox: &mut VecDeque<Mail>, mail: Mail) {
        if mail.is_decision() {
            let (agent, card) = (mail.agent.clone(), mail.card());
            mailbox.retain(|m| !(m.is_decision() && m.agent == agent && m.card() == card));
        }
        mailbox.push_back(mail);
    }

    /// The oldest mail for `agent`.
    pub fn pop(mailbox: &mut VecDeque<Mail>, agent: &AgentId) -> Option<Mail> {
        let at = mailbox.iter().position(|m| m.agent == *agent)?;
        mailbox.remove(at)
    }

    /// Drop every mail about `(agent, card)`; returns how many.
    pub fn prune_card(mailbox: &mut VecDeque<Mail>, agent: &AgentId, card: CardId) -> usize {
        let before = mailbox.len();
        mailbox.retain(|m| !(m.agent == *agent && m.card() == Some(card)));
        before - mailbox.len()
    }

    /// Drop every card-bound mail of `agent` except about `keep`.
    pub fn prune_other_cards(mailbox: &mut VecDeque<Mail>, agent: &AgentId, keep: CardId) -> usize {
        let before = mailbox.len();
        mailbox.retain(|m| !(m.agent == *agent && m.card().is_some_and(|c| c != keep)));
        before - mailbox.len()
    }

    /// Drop every mail of `agent` (it is gone).
    pub fn prune_agent(mailbox: &mut VecDeque<Mail>, agent: &AgentId) -> usize {
        let before = mailbox.len();
        mailbox.retain(|m| m.agent != *agent);
        before - mailbox.len()
    }

    /// Drop mail older than `ttl` (`mailbox_ttl_s`, §9).
    pub fn prune_older(mailbox: &mut VecDeque<Mail>, now: Instant, ttl: Duration) -> usize {
        let before = mailbox.len();
        mailbox.retain(|m| now.saturating_duration_since(m.at) < ttl);
        before - mailbox.len()
    }

    /// Take every mail of `agent` out (it is gone), oldest first, so the
    /// orders among it can be reported as failed.
    pub fn take_agent(mailbox: &mut VecDeque<Mail>, agent: &AgentId) -> Vec<Mail> {
        take(mailbox, |m| m.agent == *agent)
    }

    /// Take mail older than `ttl` (`mailbox_ttl_s`, §9) out, oldest first.
    pub fn take_older(mailbox: &mut VecDeque<Mail>, now: Instant, ttl: Duration) -> Vec<Mail> {
        take(mailbox, |m| now.saturating_duration_since(m.at) >= ttl)
    }

    /// The mail `taken` says to take, in order; the rest stays in order.
    fn take(mailbox: &mut VecDeque<Mail>, mut taken: impl FnMut(&Mail) -> bool) -> Vec<Mail> {
        let mut out = Vec::new();
        let mut kept = VecDeque::with_capacity(mailbox.len());
        for mail in mailbox.drain(..) {
            if taken(&mail) {
                out.push(mail);
            } else {
                kept.push_back(mail);
            }
        }
        *mailbox = kept;
        out
    }
}

/// Where a parked `agent.wait` answers: the typed `CoreMsg::Wait` reply or
/// the JSON `Reply` of a `CoreMsg::Call("agent.wait")`.
#[derive(Debug)]
pub enum WaitReply {
    Result(oneshot::Sender<WaitResult>),
    Call(oneshot::Sender<Reply>),
}

impl WaitReply {
    /// The receiver went away (the door dropped its request).
    pub fn is_closed(&self) -> bool {
        match self {
            WaitReply::Result(tx) => tx.is_closed(),
            WaitReply::Call(tx) => tx.is_closed(),
        }
    }

    pub fn resolve(self, result: WaitResult) {
        match self {
            WaitReply::Result(tx) => {
                let _ = tx.send(result);
            }
            WaitReply::Call(tx) => {
                let _ = tx.send(wait_reply(result));
            }
        }
    }

    /// A refused wait: both paths carry the error (the typed one as
    /// `WaitResult::Refused`, which the door turns back into the JSON-RPC
    /// error — §3.2's `-32004 conflict{owner}` / `-32003`).
    pub fn refuse(self, error: ApiError) {
        match self {
            WaitReply::Result(tx) => {
                let _ = tx.send(WaitResult::Refused(error));
            }
            WaitReply::Call(tx) => {
                let _ = tx.send(Reply::err(error));
            }
        }
    }
}

/// The JSON of an `agent.wait` result (§4.15: `{kind:"answer"|"order"|
/// "snooze"|"timeout", …}`) with the delivery the door confirms after
/// writing it.
pub fn wait_reply(result: WaitResult) -> Reply {
    match result {
        WaitResult::Answer {
            card,
            answer,
            delivery,
        } => {
            let mut value = serde_json::to_value(&answer).unwrap_or_default();
            if let Some(obj) = value.as_object_mut() {
                obj.insert("kind".into(), "answer".into());
                obj.insert("card".into(), serde_json::json!(card));
            }
            Reply {
                result: Ok(value),
                after_write: Some(delivery),
            }
        }
        WaitResult::Order {
            text,
            ctx,
            choice,
            card,
            order,
            delivery,
        } => Reply {
            result: Ok(serde_json::json!({
                "kind": "order", "text": text, "ctx": ctx, "choice": choice, "card": card,
                "order": order,
            })),
            after_write: delivery,
        },
        WaitResult::Snooze { card } => {
            Reply::ok(serde_json::json!({"kind": "snooze", "card": card}))
        }
        WaitResult::Timeout => Reply::ok(serde_json::json!({"kind": "timeout"})),
        WaitResult::Refused(error) => Reply::err(error),
    }
}

/// A parked `agent.wait` (one per `(session, agent)`).
#[derive(Debug)]
pub struct Waiter {
    pub reply: WaitReply,
    pub deadline: Instant,
    /// Matches the core's wait timer to this waiter (a replaced waiter's
    /// timer must not time out its successor).
    pub seq: u64,
}

#[derive(Debug)]
pub struct SessionEntry {
    pub kind: SessionKind,
    pub principal: Principal,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
    pub client: ClientInfo,
    pub mailbox: VecDeque<Mail>,
    pub waiters: HashMap<AgentId, Waiter>,
    pub last_seen: Instant,
    /// `(token, client)` for a logical HTTP session.
    pub logical: Option<(String, String)>,
}

impl SessionEntry {
    pub fn is_logical(&self) -> bool {
        self.logical.is_some()
    }

    /// A WS/UDS connection: deliveries and notifications have somewhere to go.
    pub fn is_live(&self) -> bool {
        self.kind.is_live()
    }

    /// Heartbeated implicitly (§3.2): a connection, or the demo.
    pub fn heartbeats(&self) -> bool {
        matches!(self.kind, SessionKind::Live { .. } | SessionKind::Internal)
    }

    /// Queue a mail (`mailbox::push`).
    pub fn push_mail(&mut self, mail: Mail) {
        mailbox::push(&mut self.mailbox, mail);
    }

    /// The oldest mail for `agent`.
    pub fn pop_mail(&mut self, agent: &AgentId) -> Option<Mail> {
        mailbox::pop(&mut self.mailbox, agent)
    }

    /// Drop every mail about `(agent, card)`; returns how many.
    pub fn prune_card(&mut self, agent: &AgentId, card: CardId) -> usize {
        mailbox::prune_card(&mut self.mailbox, agent, card)
    }

    /// Drop every card-bound mail of `agent` except about `keep` (a new
    /// card replaced the old one); orders without a card stay.
    pub fn prune_other_cards(&mut self, agent: &AgentId, keep: CardId) -> usize {
        mailbox::prune_other_cards(&mut self.mailbox, agent, keep)
    }

    /// Drop every mail of `agent` (it is gone).
    pub fn prune_agent(&mut self, agent: &AgentId) -> usize {
        mailbox::prune_agent(&mut self.mailbox, agent)
    }

    /// Drop mail older than `ttl` (`mailbox_ttl_s`, §9).
    pub fn prune_older(&mut self, now: Instant, ttl: Duration) -> usize {
        mailbox::prune_older(&mut self.mailbox, now, ttl)
    }

    /// Park a waiter for `agent`; the one it replaces is returned.
    pub fn park(&mut self, agent: AgentId, waiter: Waiter) -> Option<Waiter> {
        self.waiters.insert(agent, waiter)
    }

    pub fn take_waiter(&mut self, agent: &AgentId) -> Option<Waiter> {
        self.waiters.remove(agent)
    }
}

#[derive(Debug)]
pub struct Sessions {
    next: u64,
    entries: HashMap<SessionId, SessionEntry>,
    logical: HashMap<(String, String), SessionId>,
    /// The mail of closed sessions (§12: "`poll` answers wait in the
    /// mailbox for `mailbox_ttl_s` — a reconnecting session `agent.claim`s
    /// and `agent.wait`s them"). Keyed by the agent on each `Mail`, so the
    /// session that takes the agent over takes its answers over too; the
    /// same `mailbox_ttl_s` pruning applies.
    orphans: VecDeque<Mail>,
}

impl Default for Sessions {
    fn default() -> Self {
        Sessions::new()
    }
}

impl Sessions {
    pub fn new() -> Self {
        Sessions {
            next: FIRST_SESSION,
            entries: HashMap::new(),
            logical: HashMap::new(),
            orphans: VecDeque::new(),
        }
    }

    fn allocate(&mut self) -> SessionId {
        let id = SessionId(self.next);
        self.next += 1;
        id
    }

    fn entry(kind: SessionKind, open: OpenSession, now: Instant) -> SessionEntry {
        SessionEntry {
            kind,
            principal: open.principal,
            scopes: open.scopes,
            trusted: open.trusted,
            client: open.client,
            mailbox: VecDeque::new(),
            waiters: HashMap::new(),
            last_seen: now,
            logical: None,
        }
    }

    /// A new connection/observer/internal session.
    pub fn open(&mut self, open: OpenSession, now: Instant) -> SessionId {
        let id = self.allocate();
        let kind = open.kind.clone();
        self.entries.insert(id, Self::entry(kind, open, now));
        id
    }

    /// An internal session under a fixed id (the demo's `SessionId(1)`):
    /// never closes, never expires, heartbeated like a connection. A second
    /// call with the same id keeps the first entry.
    pub fn open_internal(
        &mut self,
        id: SessionId,
        principal: Principal,
        client: &str,
        now: Instant,
    ) {
        self.entries.entry(id).or_insert_with(|| {
            Self::entry(
                SessionKind::Internal,
                OpenSession {
                    kind: SessionKind::Internal,
                    principal,
                    scopes: vec![Scope::Admin],
                    trusted: false,
                    client: ClientInfo {
                        name: client.into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                },
                now,
            )
        });
    }

    /// The logical session `(token, client)`: created on first use,
    /// `last_seen` (and the scopes, which follow the token) refreshed on
    /// every later call. Never trusted, never `Live`.
    pub fn logical(
        &mut self,
        token: String,
        client: String,
        principal: Principal,
        scopes: Vec<Scope>,
        now: Instant,
    ) -> SessionId {
        let key = (token, client);
        if let Some(id) = self.logical.get(&key).copied() {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.last_seen = now;
                entry.scopes = scopes;
                entry.principal = principal;
                return id;
            }
        }
        let id = self.allocate();
        self.entries.insert(
            id,
            SessionEntry {
                kind: SessionKind::Observer,
                principal,
                scopes,
                trusted: false,
                client: ClientInfo {
                    name: key.1.clone(),
                    version: String::new(),
                },
                mailbox: VecDeque::new(),
                waiters: HashMap::new(),
                last_seen: now,
                logical: Some(key.clone()),
            },
        );
        self.logical.insert(key, id);
        id
    }

    /// Remove a session; its parked waiters resolve `Timeout` on drop.
    pub fn close(&mut self, id: SessionId) -> Option<SessionEntry> {
        let entry = self.entries.remove(&id)?;
        if let Some(key) = &entry.logical {
            self.logical.remove(key);
        }
        Some(entry)
    }

    pub fn get(&self, id: SessionId) -> Option<&SessionEntry> {
        self.entries.get(&id)
    }

    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut SessionEntry> {
        self.entries.get_mut(&id)
    }

    pub fn contains(&self, id: SessionId) -> bool {
        self.entries.contains_key(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SessionId, &SessionEntry)> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&SessionId, &mut SessionEntry)> {
        self.entries.iter_mut()
    }

    /// A call from `id` happened now.
    pub fn touch(&mut self, id: SessionId, now: Instant) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.last_seen = now;
        }
    }

    /// Is `(token, client)` already a logical session?
    pub fn has_logical(&self, token: &str, client: &str) -> bool {
        self.logical
            .get(&(token.to_string(), client.to_string()))
            .is_some_and(|id| self.entries.contains_key(id))
    }

    /// How many logical sessions exist, in total and for one token.
    pub fn logical_len(&self) -> usize {
        self.logical.len()
    }

    pub fn logical_of_token(&self, token: &str) -> usize {
        self.logical.keys().filter(|(t, _)| t == token).count()
    }

    /// The token's least-recently-used logical session (the one evicted to
    /// make room, `LOGICAL_PER_TOKEN_MAX`).
    pub fn logical_lru_of_token(&self, token: &str) -> Option<SessionId> {
        self.logical
            .iter()
            .filter(|((t, _), _)| t == token)
            .filter_map(|(_, id)| self.entries.get(id).map(|e| (e.last_seen, *id)))
            .min_by_key(|(last_seen, id)| (*last_seen, *id))
            .map(|(_, id)| id)
    }

    /// Logical sessions idle for longer than `ttl`.
    pub fn expired_logical(&self, now: Instant, ttl: Duration) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_logical() && now.duration_since(e.last_seen) >= ttl)
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// Ids of the live and internal sessions (the implicit heartbeat).
    pub fn heartbeat_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.heartbeats())
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// Keep a closed session's mail for whoever takes its agents over
    /// (§12). Returns how much was kept.
    pub fn orphan(&mut self, mail: impl IntoIterator<Item = Mail>) -> usize {
        let before = self.orphans.len();
        for m in mail {
            mailbox::push(&mut self.orphans, m);
        }
        self.orphans.len() - before
    }

    /// Everything the orphan mailbox holds for `agent`, oldest first, taken
    /// out of it: the new owner's mailbox adopts it.
    pub fn take_orphans(&mut self, agent: &AgentId) -> Vec<Mail> {
        let mut taken = Vec::new();
        while let Some(mail) = mailbox::pop(&mut self.orphans, agent) {
            taken.push(mail);
        }
        taken
    }

    /// The orphan mailbox (pruned by the same rules as a session's own).
    pub fn orphans_mut(&mut self) -> &mut VecDeque<Mail> {
        &mut self.orphans
    }

    pub fn orphan_len(&self) -> usize {
        self.orphans.len()
    }

    /// Mails and waiters across every session, orphaned mail included
    /// (`metrics.get`).
    pub fn queued(&self) -> (usize, usize) {
        self.entries
            .values()
            .fold((self.orphans.len(), 0), |(m, w), e| {
                (m + e.mailbox.len(), w + e.waiters.len())
            })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::AnswerSource;

    fn open(kind: SessionKind) -> OpenSession {
        OpenSession {
            kind,
            principal: Principal::Uid(501),
            scopes: vec![Scope::Observe],
            trusted: false,
            client: ClientInfo {
                name: "t".into(),
                version: "1".into(),
            },
        }
    }

    fn answer(choice: u8) -> Answer {
        Answer {
            choice: Some(choice),
            label: Some(format!("c{choice}")),
            text: None,
            source: AnswerSource::Deck,
            principal: Principal::Device,
            session: None,
            utterance: None,
        }
    }

    fn mail(agent: &str, kind: MailKind, at: Instant) -> Mail {
        Mail {
            agent: AgentId(agent.into()),
            at,
            kind,
        }
    }

    #[test]
    fn ids_start_at_two_and_logical_sessions_are_keyed_by_token_and_client() {
        let mut s = Sessions::new();
        let t0 = Instant::now();
        let a = s.open(open(SessionKind::Observer), t0);
        let b = s.open(open(SessionKind::Internal), t0);
        assert_eq!((a, b), (SessionId(2), SessionId(3)));
        assert_eq!(a.to_string(), "sess_2");
        let l1 = s.logical(
            "hooks".into(),
            "bot".into(),
            Principal::Token("hooks".into()),
            vec![Scope::Agent],
            t0,
        );
        let l2 = s.logical(
            "hooks".into(),
            "bot".into(),
            Principal::Token("hooks".into()),
            vec![Scope::Agent, Scope::Voice],
            t0 + Duration::from_secs(1),
        );
        assert_eq!(l1, l2, "same (token, client) → same session");
        assert_eq!(s.get(l1).unwrap().scopes, vec![Scope::Agent, Scope::Voice]);
        assert_eq!(s.get(l1).unwrap().last_seen, t0 + Duration::from_secs(1));
        assert_eq!(s.get(l1).unwrap().client.name, "bot");
        assert!(s.get(l1).unwrap().is_logical() && !s.get(l1).unwrap().trusted);
        assert!(!s.get(l1).unwrap().is_live() && !s.get(l1).unwrap().heartbeats());
        let other = s.logical(
            "hooks".into(),
            "other".into(),
            Principal::Token("hooks".into()),
            vec![Scope::Agent],
            t0,
        );
        assert_ne!(other, l1);
        assert_eq!(s.len(), 4);
        assert_eq!(s.heartbeat_sessions(), vec![b]);
        // Close: gone from both maps; a re-created logical session is new.
        assert!(s.close(l1).is_some());
        assert!(s.close(l1).is_none());
        assert!(!s.contains(l1));
        let l3 = s.logical(
            "hooks".into(),
            "bot".into(),
            Principal::Token("hooks".into()),
            vec![Scope::Agent],
            t0,
        );
        assert!(l3 > other);
    }

    #[test]
    fn the_internal_session_keeps_id_one_and_never_shifts_the_counter() {
        let mut s = Sessions::new();
        let t0 = Instant::now();
        s.open_internal(SessionId(1), Principal::Device, "demo", t0);
        s.open_internal(SessionId(1), Principal::Uid(9), "again", t0);
        let entry = s.get(SessionId(1)).unwrap();
        assert!(matches!(entry.kind, SessionKind::Internal));
        assert_eq!(entry.principal, Principal::Device, "the first entry stays");
        assert_eq!(entry.client.name, "demo");
        assert!(entry.heartbeats() && !entry.is_live() && !entry.is_logical());
        assert_eq!(s.open(open(SessionKind::Observer), t0), SessionId(2));
        assert_eq!(s.heartbeat_sessions(), vec![SessionId(1)]);
        assert!(s
            .expired_logical(t0 + Duration::from_secs(100_000), Duration::from_secs(1))
            .is_empty());
    }

    #[test]
    fn logical_sessions_expire_after_the_ttl_and_touch_refreshes_them() {
        let mut s = Sessions::new();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(600);
        let live = s.open(open(SessionKind::Observer), t0);
        let l = s.logical(
            "t".into(),
            "c".into(),
            Principal::Token("t".into()),
            vec![],
            t0,
        );
        assert!(s
            .expired_logical(t0 + ttl - Duration::from_secs(1), ttl)
            .is_empty());
        assert_eq!(s.expired_logical(t0 + ttl, ttl), vec![l]);
        s.touch(l, t0 + ttl);
        assert!(s.expired_logical(t0 + ttl, ttl).is_empty());
        s.touch(live, t0 + ttl * 2);
        assert_eq!(
            s.expired_logical(t0 + ttl * 2, ttl),
            vec![l],
            "live sessions never expire"
        );
        assert!(s.get_mut(live).unwrap().mailbox.is_empty());
        assert!(!s.is_empty());
    }

    #[test]
    fn a_mailbox_keeps_one_decision_per_card_and_prunes_by_card_agent_and_age() {
        let mut s = Sessions::new();
        let t0 = Instant::now();
        let id = s.open(open(SessionKind::Observer), t0);
        let e = s.get_mut(id).unwrap();
        let a = AgentId("ag_a".into());
        let b = AgentId("ag_b".into());
        e.push_mail(mail(
            "ag_a",
            MailKind::Answer {
                card: CardId(1),
                answer: answer(0),
            },
            t0,
        ));
        // The same card answered again: the first decision is dropped.
        e.push_mail(mail(
            "ag_a",
            MailKind::Answer {
                card: CardId(1),
                answer: answer(1),
            },
            t0 + Duration::from_secs(1),
        ));
        assert_eq!(e.mailbox.len(), 1);
        // A snooze on the same card replaces the answer too; another card and
        // agent stack up; an order without a card is never deduplicated.
        e.push_mail(mail("ag_a", MailKind::Snooze { card: CardId(1) }, t0));
        e.push_mail(mail(
            "ag_a",
            MailKind::Answer {
                card: CardId(2),
                answer: answer(0),
            },
            t0,
        ));
        e.push_mail(mail(
            "ag_b",
            MailKind::Answer {
                card: CardId(1),
                answer: answer(0),
            },
            t0,
        ));
        for _ in 0..2 {
            e.push_mail(mail(
                "ag_a",
                MailKind::Order {
                    text: "go".into(),
                    ctx: "talk".into(),
                    choice: None,
                    card: None,
                    order: Some("or_1".into()),
                },
                t0 + Duration::from_secs(2),
            ));
        }
        assert_eq!(e.mailbox.len(), 5);
        // A chosen Ready step is a decision about its card.
        e.push_mail(mail(
            "ag_b",
            MailKind::Order {
                text: "Run tests".into(),
                ctx: "step".into(),
                choice: Some(0),
                card: Some(CardId(1)),
                order: None,
            },
            t0,
        ));
        assert_eq!(e.mailbox.len(), 5, "replaced ag_b's answer on card 1");
        // Pop is per agent, oldest first.
        let first = e.pop_mail(&a).unwrap();
        assert!(matches!(first.kind, MailKind::Snooze { card: CardId(1) }));
        assert_eq!(e.pop_mail(&AgentId("ag_zzz".into())).map(|m| m.at), None);
        assert_eq!(e.prune_card(&a, CardId(2)), 1);
        assert_eq!(e.prune_card(&a, CardId(2)), 0);
        assert_eq!(e.mailbox.len(), 3);
        // A new card for ag_b: the step about card 1 goes, card-less orders stay.
        e.push_mail(mail(
            "ag_b",
            MailKind::Order {
                text: "x".into(),
                ctx: "talk".into(),
                choice: None,
                card: None,
                order: Some("or_2".into()),
            },
            t0,
        ));
        assert_eq!(e.prune_other_cards(&b, CardId(2)), 1);
        assert_eq!(e.mailbox.len(), 3);
        assert_eq!(e.prune_agent(&b), 1);
        // Age: the two orders were queued at t0+2 s.
        assert_eq!(
            e.prune_older(t0 + Duration::from_secs(601), Duration::from_secs(600)),
            0
        );
        assert_eq!(
            e.prune_older(t0 + Duration::from_secs(602), Duration::from_secs(600)),
            2
        );
        assert!(e.mailbox.is_empty());
        assert_eq!(s.queued(), (0, 0));
    }

    /// What a pruning takes out is handed back, so the core can report the
    /// orders among it; what stays keeps its order.
    #[test]
    fn mail_taken_out_by_agent_or_age_is_handed_back_in_order() {
        let t0 = Instant::now();
        let order = |agent: &str, id: &str, at| {
            mail(
                agent,
                MailKind::Order {
                    text: id.into(),
                    ctx: "talk".into(),
                    choice: None,
                    card: None,
                    order: Some(id.into()),
                },
                at,
            )
        };
        let text = |m: &Mail| match &m.kind {
            MailKind::Order { text, .. } => text.clone(),
            other => panic!("{other:?}"),
        };
        let mut mailbox: VecDeque<Mail> = [
            order("ag_a", "or_1", t0),
            order("ag_b", "or_2", t0),
            order("ag_a", "or_3", t0 + Duration::from_secs(5)),
            order("ag_b", "or_4", t0 + Duration::from_secs(5)),
        ]
        .into();
        let gone = mailbox::take_agent(&mut mailbox, &AgentId("ag_a".into()));
        assert_eq!(gone.iter().map(text).collect::<Vec<_>>(), ["or_1", "or_3"]);
        assert_eq!(
            mailbox.iter().map(text).collect::<Vec<_>>(),
            ["or_2", "or_4"]
        );
        let ttl = Duration::from_secs(600);
        assert!(mailbox::take_older(&mut mailbox, t0 + Duration::from_secs(599), ttl).is_empty());
        let old = mailbox::take_older(&mut mailbox, t0 + Duration::from_secs(600), ttl);
        assert_eq!(old.iter().map(text).collect::<Vec<_>>(), ["or_2"]);
        assert_eq!(mailbox.iter().map(text).collect::<Vec<_>>(), ["or_4"]);
    }

    #[test]
    fn mail_turns_into_wait_results_and_json_replies_with_their_deliveries() {
        let t0 = Instant::now();
        let m = mail(
            "ag_a",
            MailKind::Answer {
                card: CardId(3),
                answer: answer(1),
            },
            t0,
        );
        let r = m.into_result();
        let WaitResult::Answer {
            card,
            answer,
            delivery,
        } = r
        else {
            panic!("{r:?}");
        };
        assert_eq!(card, CardId(3));
        assert_eq!(answer.label.as_deref(), Some("c1"));
        assert_eq!(
            delivery,
            Delivery {
                agent: AgentId("ag_a".into()),
                card: CardId(3)
            }
        );
        let reply = wait_reply(WaitResult::Answer {
            card,
            answer,
            delivery: delivery.clone(),
        });
        let v = reply.result.unwrap();
        assert_eq!(v["kind"], "answer");
        assert_eq!(v["card"], 3);
        assert_eq!(v["choice"], 1);
        assert_eq!(v["label"], "c1");
        assert_eq!(v["principal"], "device");
        assert_eq!(reply.after_write, Some(delivery));
        let step = mail(
            "ag_a",
            MailKind::Order {
                text: "Run tests".into(),
                ctx: "step".into(),
                choice: Some(0),
                card: Some(CardId(4)),
                order: None,
            },
            t0,
        )
        .into_result();
        assert!(matches!(
            &step,
            WaitResult::Order {
                delivery: Some(Delivery {
                    card: CardId(4),
                    ..
                }),
                ..
            }
        ));
        let reply = wait_reply(step);
        assert_eq!(reply.result.as_ref().unwrap()["kind"], "order");
        assert_eq!(reply.result.as_ref().unwrap()["text"], "Run tests");
        assert!(reply.after_write.is_some());
        let order = mail(
            "ag_a",
            MailKind::Order {
                text: "go".into(),
                ctx: "talk".into(),
                choice: None,
                card: None,
                order: Some("or_7".into()),
            },
            t0,
        )
        .into_result();
        assert!(matches!(&order, WaitResult::Order { delivery: None, .. }));
        let reply = wait_reply(order);
        assert!(reply.after_write.is_none());
        // The order's id, for `agent.order.result`; a step has none.
        assert_eq!(reply.result.as_ref().unwrap()["order"], "or_7");
        let step = mail(
            "ag_a",
            MailKind::Order {
                text: "Run tests".into(),
                ctx: "step".into(),
                choice: Some(0),
                card: Some(CardId(4)),
                order: None,
            },
            t0,
        );
        let reply = wait_reply(step.into_result());
        assert_eq!(reply.result.unwrap()["order"], serde_json::Value::Null);
        let snooze =
            wait_reply(mail("ag_a", MailKind::Snooze { card: CardId(1) }, t0).into_result());
        assert_eq!(
            snooze.result.unwrap(),
            serde_json::json!({"kind": "snooze", "card": 1})
        );
        assert_eq!(
            wait_reply(WaitResult::Timeout).result.unwrap(),
            serde_json::json!({"kind": "timeout"})
        );
    }

    #[test]
    fn wait_replies_resolve_or_refuse_on_either_path() {
        // Both paths carry the refusal: the typed one as `Refused`, so the
        // door can answer `-32003`/`-32004` instead of a false timeout.
        let (tx, rx) = oneshot::channel();
        WaitReply::Result(tx).refuse(ApiError::not_found("agent"));
        let refused = rx.blocking_recv().unwrap();
        let WaitResult::Refused(error) = refused else {
            panic!("{refused:?}");
        };
        assert_eq!(error.code, ApiError::NOT_FOUND);
        assert_eq!(error.data["what"], "agent");
        assert_eq!(
            wait_reply(WaitResult::Refused(error))
                .result
                .unwrap_err()
                .code,
            ApiError::NOT_FOUND
        );
        let (tx, rx) = oneshot::channel();
        WaitReply::Call(tx).refuse(ApiError::not_found("agent"));
        assert_eq!(
            rx.blocking_recv().unwrap().result.unwrap_err().code,
            ApiError::NOT_FOUND
        );
        let (tx, rx) = oneshot::channel();
        let reply = WaitReply::Call(tx);
        assert!(!reply.is_closed());
        drop(rx);
        assert!(reply.is_closed());
        reply.resolve(WaitResult::Timeout);
        let (tx, rx) = oneshot::channel();
        WaitReply::Result(tx).resolve(WaitResult::Snooze { card: CardId(2) });
        assert!(matches!(
            rx.blocking_recv().unwrap(),
            WaitResult::Snooze { card: CardId(2) }
        ));
        // Parking replaces, and the replaced waiter comes back.
        let mut s = Sessions::new();
        let id = s.open(open(SessionKind::Observer), Instant::now());
        let e = s.get_mut(id).unwrap();
        let a = AgentId("ag_a".into());
        let (tx1, _rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        assert!(e
            .park(
                a.clone(),
                Waiter {
                    reply: WaitReply::Result(tx1),
                    deadline: Instant::now(),
                    seq: 1
                }
            )
            .is_none());
        let old = e
            .park(
                a.clone(),
                Waiter {
                    reply: WaitReply::Result(tx2),
                    deadline: Instant::now(),
                    seq: 2,
                },
            )
            .unwrap();
        assert_eq!(old.seq, 1);
        assert_eq!(e.take_waiter(&a).unwrap().seq, 2);
        assert!(e.take_waiter(&a).is_none());
        assert_eq!(s.queued(), (0, 0));
    }
}
