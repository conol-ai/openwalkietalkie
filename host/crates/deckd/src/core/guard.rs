//! Guarded operations and rate limits inside the core (design §4.2, §11;
//! contract §3.2), run right before `store.call` so there is no TOCTOU:
//! an allow on a permission/elicitation card, an `inject` order or a
//! `voice.listen{ctx:"answer"}` on such a card from an untrusted external
//! principal is refused `-32002 {need:"trusted", how:"gui_confirm"}` and the
//! core applies `Command::ConfirmRequested` (so `op.confirm_requested` is on
//! the bus); external principals (`uid:*`, `token:*`) are token-bucketed.
//! Deny and snooze are never guarded; `device` and the adapters never are.
use deck_model::guard::is_guarded_allow;
use deck_model::{AgentId, ApiError, CardId, OrderCap, OriginKind, Principal, Request, Snapshot};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A bucket untouched for this long, and full again, is forgotten.
const IDLE: Duration = Duration::from_secs(300);

/// A classic token bucket: `capacity` tokens, refilled at `per_second`.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: f64,
    per_second: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, per_second: f64, now: Instant) -> Self {
        TokenBucket {
            capacity: f64::from(capacity),
            per_second,
            tokens: f64::from(capacity),
            last: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        self.last = now;
    }

    /// Full again, and idle since: the bucket carries no state a fresh one
    /// would not have, so the map may forget it (`RateLimits::prune`).
    fn is_idle(&self, now: Instant) -> bool {
        let refilled =
            self.tokens + now.saturating_duration_since(self.last).as_secs_f64() * self.per_second;
        refilled >= self.capacity && now.saturating_duration_since(self.last) >= IDLE
    }

    /// Take one token, or say how long until one is available.
    pub fn try_take(&mut self, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - self.tokens;
            Err(Duration::from_secs_f64(missing / self.per_second))
        }
    }
}

/// The limits of §11: `agent.register` 30/min per principal, `agent.update`
/// 10/s per agent, `op.order` 2/s per principal, `voice.say` 4/s per
/// principal. Only external principals are metered.
#[derive(Debug, Default)]
pub struct RateLimits {
    register: HashMap<Principal, TokenBucket>,
    update: HashMap<String, TokenBucket>,
    order: HashMap<Principal, TokenBucket>,
    say: HashMap<Principal, TokenBucket>,
}

impl RateLimits {
    pub fn new() -> Self {
        RateLimits::default()
    }

    /// `uid:*` and `token:*`: what the limits and the trust guard apply to.
    pub fn is_external(principal: &Principal) -> bool {
        matches!(principal, Principal::Uid(_) | Principal::Token(_))
    }

    fn take(
        buckets: &mut HashMap<Principal, TokenBucket>,
        principal: &Principal,
        capacity: u32,
        per_second: f64,
        now: Instant,
    ) -> Result<(), ApiError> {
        buckets
            .entry(principal.clone())
            .or_insert_with(|| TokenBucket::new(capacity, per_second, now))
            .try_take(now)
            .map_err(|wait| ApiError::rate_limited(wait.as_millis().max(1) as u32))
    }

    /// Meter `req` from `principal`; `Ok` for every internal principal.
    pub fn check(
        &mut self,
        principal: &Principal,
        req: &Request,
        now: Instant,
    ) -> Result<(), ApiError> {
        if !Self::is_external(principal) {
            return Ok(());
        }
        match req {
            Request::AgentRegister(_) => Self::take(&mut self.register, principal, 30, 0.5, now),
            Request::AgentUpdate(u) => self
                .update
                .entry(u.agent.0.clone())
                .or_insert_with(|| TokenBucket::new(10, 10.0, now))
                .try_take(now)
                .map_err(|wait| ApiError::rate_limited(wait.as_millis().max(1) as u32)),
            Request::OpOrder(_) => Self::take(&mut self.order, principal, 2, 2.0, now),
            // B5: `voice.say` became a reducer method, so its 4/s limit
            // has to be taken **here** — the `check_say` entry point the
            // core-answered version used is not on this path at all, and a
            // limit that silently stopped applying is the kind of thing
            // nobody notices until a token is shouting at a headset.
            Request::VoiceSay(_) => self.check_say(principal, now),
            _ => Ok(()),
        }
    }

    /// `voice.say`'s own 4/s bucket (§11), kept as its own entry point so
    /// that both routes into it — the `Request` above, and any caller that
    /// still meters by name — take from the same bucket.
    pub fn check_say(&mut self, principal: &Principal, now: Instant) -> Result<(), ApiError> {
        if !Self::is_external(principal) {
            return Ok(());
        }
        Self::take(&mut self.say, principal, 4, 4.0, now)
    }

    /// Drop the buckets nobody has touched for `IDLE`: a full bucket is the
    /// same as no bucket at all, and a map keyed by agent id (or by a
    /// stranger's token name) must not grow for the daemon's whole uptime.
    /// Called from the core's periodic housekeeping.
    pub fn prune(&mut self, now: Instant) {
        fn keep<K>(buckets: &mut HashMap<K, TokenBucket>, now: Instant) {
            buckets.retain(|_, bucket| !bucket.is_idle(now));
        }
        keep(&mut self.register, now);
        keep(&mut self.update, now);
        keep(&mut self.order, now);
        keep(&mut self.say, now);
    }

    /// Buckets held, across every limit (tests, `metrics.get`).
    pub fn tracked(&self) -> usize {
        self.register.len() + self.update.len() + self.order.len() + self.say.len()
    }
}

/// Why the guard refused a request.
#[derive(Debug)]
pub enum Refusal {
    RateLimited(ApiError),
    /// A guarded operation from an untrusted principal: the caller gets
    /// `forbidden_trusted()` and the store `Command::ConfirmRequested`.
    Untrusted {
        agent: AgentId,
        card: Option<CardId>,
        order: Option<String>,
    },
}

impl Refusal {
    /// The error the caller sees.
    pub fn error(&self) -> ApiError {
        match self {
            Refusal::RateLimited(e) => e.clone(),
            Refusal::Untrusted { .. } => ApiError::forbidden_trusted(),
        }
    }
}

/// The guard: the rate limits, then the trust check of guarded operations
/// (`[trust] require_trusted_for_allow`, hot-reloaded).
#[derive(Debug)]
pub struct Guard {
    pub limits: RateLimits,
    pub require_trusted: bool,
}

impl Default for Guard {
    fn default() -> Self {
        Guard::new()
    }
}

impl Guard {
    pub fn new() -> Self {
        Guard {
            limits: RateLimits::new(),
            require_trusted: true,
        }
    }

    /// The trust guard applies: configured, the caller is not trusted and it
    /// is an external principal (`device` and the adapters are never guarded).
    fn applies(&self, principal: &Principal, trusted: bool) -> bool {
        self.require_trusted && !trusted && RateLimits::is_external(principal)
    }

    /// Refuse `req` before the reducer sees it (rate limit first: a flood of
    /// guarded calls must not flood the bus with `op.confirm_requested`), or
    /// let it through.
    pub fn check(
        &mut self,
        principal: &Principal,
        trusted: bool,
        req: &Request,
        snapshot: &Snapshot,
        now: Instant,
    ) -> Result<(), Refusal> {
        self.limits
            .check(principal, req, now)
            .map_err(Refusal::RateLimited)?;
        self.guarded(principal, trusted, req, snapshot)
    }

    /// The trust check alone (§4.2): `op.answer` that is an allow on a
    /// permission/elicitation card, `op.order` whose route is `inject`.
    pub fn guarded(
        &self,
        principal: &Principal,
        trusted: bool,
        req: &Request,
        snapshot: &Snapshot,
    ) -> Result<(), Refusal> {
        if !self.applies(principal, trusted) {
            return Ok(());
        }
        match req {
            Request::OpAnswer(a) => {
                let card = snapshot
                    .agents
                    .get(&a.agent)
                    .and_then(|ag| ag.card.as_ref())
                    .filter(|c| c.id == a.card);
                match card {
                    Some(card) if is_guarded_allow(card, a.choice, a.text.as_deref()) => {
                        Err(Refusal::Untrusted {
                            agent: a.agent.clone(),
                            card: Some(card.id),
                            order: None,
                        })
                    }
                    _ => Ok(()),
                }
            }
            Request::OpOrder(o) => {
                let inject = snapshot
                    .agents
                    .get(&o.agent)
                    .is_some_and(|ag| ag.caps.order == OrderCap::Inject);
                if inject {
                    Err(Refusal::Untrusted {
                        agent: o.agent.clone(),
                        card: None,
                        order: Some(o.text.clone()),
                    })
                } else {
                    Ok(())
                }
            }
            // **B5's whole point.** `voice.listen` used to be answered
            // outside the reducer, where `Core::voice_call` called
            // [`Guard::check_listen`] by hand. Making it a reducer method
            // moves it onto this path — and `guarded` is a *different entry
            // point*, so the refusal is **not inherited**: without this arm
            // the method would simply stop being guarded, silently, and an
            // untrusted token could dictate an allow onto a permission
            // card. The check itself is unchanged; only the door it comes
            // through is.
            Request::VoiceListen(l) => {
                // **Guard 1's trust half, by its second door.** A capture
                // this caller starts is dispatched by
                // `Store::dispatch_order`, which calls `op_order`
                // **directly** rather than through `Request::OpOrder` — so
                // the arm above never sees it and an untrusted `voice`
                // caller could aim the operator's dictation at an
                // `inject` agent of its choosing. §5.5 guard 1 admits only
                // "a transcript from a `voice.listen` the trusted GUI
                // started", and this is that sentence.
                //
                // It is checked for **every** `ctx`, not only `answer`:
                // `ctx:"talk"` is an order by definition, and
                // `dispatch_answer` falls back to `dispatch_order` when
                // the agent has no card, so `ctx:"answer"` is one too.
                let inject = snapshot
                    .agents
                    .get(&l.agent)
                    .is_some_and(|ag| ag.caps.order == OrderCap::Inject);
                if inject {
                    return Err(Refusal::Untrusted {
                        agent: l.agent.clone(),
                        card: l.card.or_else(|| {
                            snapshot
                                .agents
                                .get(&l.agent)
                                .and_then(|ag| ag.card.as_ref())
                                .map(|c| c.id)
                        }),
                        order: None,
                    });
                }
                self.check_listen(principal, trusted, &l.agent, &l.ctx, l.card, snapshot)
            }
            _ => Ok(()),
        }
    }

    /// `voice.listen{ctx:"answer", card}` on a permission/elicitation card:
    /// the transcript would be an allow attributed to the caller, so it is
    /// refused before capture starts (§4.9). `card` absent → the agent's
    /// current card.
    pub fn check_listen(
        &self,
        principal: &Principal,
        trusted: bool,
        agent: &AgentId,
        ctx: &str,
        card: Option<CardId>,
        snapshot: &Snapshot,
    ) -> Result<(), Refusal> {
        if ctx != "answer" || !self.applies(principal, trusted) {
            return Ok(());
        }
        let current = snapshot
            .agents
            .get(agent)
            .and_then(|a| a.card.as_ref())
            .filter(|c| card.is_none_or(|id| id == c.id))
            .filter(|c| {
                matches!(
                    c.origin.kind,
                    OriginKind::Permission | OriginKind::Elicitation
                )
            });
        match current {
            Some(c) => Err(Refusal::Untrusted {
                agent: agent.clone(),
                card: Some(c.id),
                order: None,
            }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_buckets_are_forgotten_and_metered_ones_are_kept() {
        let t0 = Instant::now();
        let mut limits = RateLimits::new();
        let bot = Principal::Token("bot".into());
        let update = |agent: &str| {
            Request::AgentUpdate(deck_model::AgentUpdate {
                agent: AgentId(agent.into()),
                state: None,
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None,
            })
        };
        for n in 0..50 {
            limits
                .check(&bot, &update(&format!("ag_{n}")), t0)
                .expect("the first update of each agent is free");
        }
        assert_eq!(limits.tracked(), 50, "one bucket per agent");
        // Still inside the idle window: nothing is dropped.
        limits.prune(t0 + Duration::from_secs(60));
        assert_eq!(limits.tracked(), 50);
        // Past it, every bucket has refilled and carries no state.
        limits.prune(t0 + IDLE + Duration::from_secs(1));
        assert_eq!(limits.tracked(), 0, "an idle map does not grow forever");
        // A bucket that is still metering someone survives a prune.
        for _ in 0..10 {
            let _ = limits.check(&bot, &update("ag_busy"), t0);
        }
        assert!(
            limits.check(&bot, &update("ag_busy"), t0).is_err(),
            "drained"
        );
        limits.prune(t0 + IDLE + Duration::from_secs(1));
        assert_eq!(limits.tracked(), 0, "and is forgotten once it has refilled");
    }

    use deck_model::{
        AdapterKind, Agent, AgentState, AgentUpdate, AnswerCap, Caps, Card, CardId, CardKind,
        CardStatus, Choice, Deliver, Kind, OpAnswer, OpOrder, OrderSource, Origin, Owner, Page,
        SessionId, SessionInfo, Source, TitleSource, Ts,
    };

    #[test]
    fn the_bucket_refills_at_its_rate_with_a_simulated_clock() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2, 1.0, t0);
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0).is_ok());
        let wait = b.try_take(t0).unwrap_err();
        assert!((wait.as_secs_f64() - 1.0).abs() < 1e-6, "{wait:?}");
        assert!(b.try_take(t0 + Duration::from_millis(500)).is_err());
        assert!(b.try_take(t0 + Duration::from_secs(1)).is_ok());
        // Never above capacity.
        assert!(b.try_take(t0 + Duration::from_secs(100)).is_ok());
        assert!(b.try_take(t0 + Duration::from_secs(100)).is_ok());
        assert!(b.try_take(t0 + Duration::from_secs(100)).is_err());
    }

    fn order(text: &str) -> Request {
        Request::OpOrder(OpOrder {
            agent: AgentId("ag_1".into()),
            text: text.into(),
            ctx: "talk".into(),
            source: OrderSource::Api,
            utterance: None,
        })
    }

    #[test]
    fn only_external_principals_are_metered_per_method() {
        let t0 = Instant::now();
        let mut limits = RateLimits::new();
        let order = order("go");
        let uid = Principal::Uid(501);
        assert!(limits.check(&uid, &order, t0).is_ok());
        assert!(limits.check(&uid, &order, t0).is_ok());
        let err = limits.check(&uid, &order, t0).unwrap_err();
        assert_eq!(err.code, ApiError::RATE_LIMITED);
        assert_eq!(err.data["retry_ms"], 500);
        assert!(
            limits
                .check(&Principal::Token("t".into()), &order, t0)
                .is_ok(),
            "per principal"
        );
        for _ in 0..5 {
            assert!(limits.check(&Principal::Device, &order, t0).is_ok());
            assert!(limits
                .check(&Principal::Adapter(AdapterKind::Claude), &order, t0)
                .is_ok());
        }
        assert!(limits
            .check(&uid, &order, t0 + Duration::from_secs(1))
            .is_ok());
        let update = |agent: &str| {
            Request::AgentUpdate(AgentUpdate {
                agent: AgentId(agent.into()),
                state: None,
                title: None,
                title_source: None,
                since: None,
                name: None,
                meta: None,
            })
        };
        for _ in 0..10 {
            assert!(limits.check(&uid, &update("ag_1"), t0).is_ok());
        }
        assert!(limits.check(&uid, &update("ag_1"), t0).is_err());
        assert!(limits.check(&uid, &update("ag_2"), t0).is_ok(), "per agent");
        // 30 registrations a minute, then a 2 s wait per extra one.
        let register = Request::AgentRegister(crate::core::testkit::register("bot", None));
        for _ in 0..30 {
            assert!(limits.check(&uid, &register, t0).is_ok());
        }
        let err = limits.check(&uid, &register, t0).unwrap_err();
        assert_eq!(err.data["retry_ms"], 2000);
        assert!(limits
            .check(&uid, &register, t0 + Duration::from_secs(2))
            .is_ok());
        for _ in 0..4 {
            assert!(limits.check_say(&uid, t0).is_ok());
        }
        assert!(limits.check_say(&uid, t0).is_err());
        assert!(limits.check_say(&Principal::Device, t0).is_ok());
        // The guard meters first: the third order in a second is rate
        // limited, not confirmation-requested, whatever the trust says.
        let mut guard = Guard::new();
        assert!(guard
            .check(&uid, false, &order, &Snapshot::default(), t0)
            .is_ok());
        assert!(guard
            .check(&uid, false, &order, &Snapshot::default(), t0)
            .is_ok());
        let refusal = guard
            .check(&uid, false, &order, &Snapshot::default(), t0)
            .unwrap_err();
        assert!(matches!(refusal, Refusal::RateLimited(_)));
        assert_eq!(refusal.error().code, ApiError::RATE_LIMITED);
    }

    /// An agent with a card of `kind` and `labels`, `caps.order` as given.
    fn agent_with(kind: OriginKind, labels: &[&str], order: OrderCap) -> Agent {
        Agent {
            id: AgentId("ag_1".into()),
            slot: Some(deck_model::Slot(1)),
            kind: Kind::Claude,
            name: "cohdl-api".into(),
            state: AgentState::Question,
            title: None,
            title_source: TitleSource::SessionName,
            since: Ts(0),
            source: Source::Hook,
            owner: Owner {
                principal: Principal::Adapter(AdapterKind::Claude),
                session: SessionId(1),
                lease_expires: None,
            },
            session: SessionInfo::default(),
            caps: Caps {
                answer: AnswerCap::Hold,
                order,
                focus: true,
            },
            companion: None,
            card: Some(Card {
                id: CardId(17),
                kind: CardKind::Q,
                status: CardStatus::Pending,
                deliver: Deliver::Hold,
                origin: Origin {
                    kind,
                    tool: Some("Bash".into()),
                    request_id: None,
                    group: None,
                },
                text: Some("Bash: psql".into()),
                choices: labels
                    .iter()
                    .map(|l| Choice {
                        label: (*l).into(),
                        detail: None,
                    })
                    .collect(),
                choices_n: labels.len() as u8,
                choice_map: Vec::new(),
                rec: Some(0),
                free_text: true,
                page: Page { index: 0, count: 1 },
                created: Ts(0),
                expires: None,
                redacted: false,
            }),
            reply: None,
            messages: Vec::new(),
            workflow_runs: Vec::new(), workflow: None,
            chat_rev: 0,
            reply_rev: 0,
            voice: None,
            pinned: false,
            meta: serde_json::Value::Null,
            redacted: false,
        }
    }

    fn snapshot_with(agent: Agent) -> Snapshot {
        let mut snap = Snapshot::default();
        snap.agents.insert(agent.id.clone(), agent);
        snap
    }

    fn answer(card: u16, choice: Option<u8>, text: Option<&str>) -> Request {
        Request::OpAnswer(OpAnswer {
            agent: AgentId("ag_1".into()),
            card: CardId(card),
            choice,
            text: text.map(String::from),
        })
    }

    #[test]
    fn allows_on_permission_cards_and_inject_orders_need_trust_denies_do_not() {
        let t0 = Instant::now();
        let snap = snapshot_with(agent_with(
            OriginKind::Permission,
            &["Allow", "Allow always", "Deny"],
            OrderCap::Inject,
        ));
        let mut guard = Guard::new();
        let token = Principal::Token("mcp".into());
        for (req, guarded, why) in [
            (answer(17, Some(0), None), true, "Allow"),
            (answer(17, Some(1), None), true, "Allow always"),
            (answer(17, Some(2), None), false, "Deny"),
            (answer(17, None, Some("yes")), true, "free-text yes"),
            (answer(17, None, Some("no thanks")), false, "free-text no"),
            (
                answer(16, Some(0), None),
                false,
                "stale card: the store refuses it",
            ),
            (order("rm -rf build"), true, "inject route"),
            (
                Request::OpSnooze {
                    agent: AgentId("ag_1".into()),
                    card: CardId(17),
                },
                false,
                "snooze",
            ),
        ] {
            let got = guard.check(&token, false, &req, &snap, t0);
            assert_eq!(got.is_err(), guarded, "{why}: {got:?}");
            if let Err(refusal) = got {
                let e = refusal.error();
                assert_eq!(e.code, ApiError::FORBIDDEN);
                assert_eq!(e.data["need"], "trusted");
                assert_eq!(e.data["how"], "gui_confirm");
                match (&refusal, &req) {
                    (
                        Refusal::Untrusted {
                            agent,
                            card: Some(CardId(17)),
                            order: None,
                        },
                        Request::OpAnswer(_),
                    ) => assert_eq!(agent.0, "ag_1"),
                    (
                        Refusal::Untrusted {
                            card: None,
                            order: Some(text),
                            ..
                        },
                        Request::OpOrder(_),
                    ) => assert_eq!(text, "rm -rf build"),
                    other => panic!("{other:?}"),
                }
            }
        }
        // Trusted, device and adapters pass; so does everything once the
        // config switches the guard off.
        let allow = answer(17, Some(0), None);
        assert!(guard.check(&token, true, &allow, &snap, t0).is_ok());
        assert!(guard
            .check(&Principal::Device, false, &allow, &snap, t0)
            .is_ok());
        assert!(guard
            .check(
                &Principal::Adapter(AdapterKind::Claude),
                false,
                &allow,
                &snap,
                t0
            )
            .is_ok());
        assert!(guard
            .check(&Principal::Uid(501), false, &allow, &snap, t0)
            .is_err());
        guard.require_trusted = false;
        assert!(guard.check(&token, false, &allow, &snap, t0).is_ok());
        assert!(guard.check(&token, false, &order("x"), &snap, t0).is_ok());
    }

    #[test]
    fn custom_cards_and_push_poll_orders_are_never_guarded() {
        let t0 = Instant::now();
        let mut guard = Guard::new();
        let uid = Principal::Uid(501);
        for kind in [OriginKind::Custom, OriginKind::Question, OriginKind::Ready] {
            let snap = snapshot_with(agent_with(kind, &["Allow", "Yes"], OrderCap::Poll));
            assert!(
                guard
                    .check(&uid, false, &answer(17, Some(0), None), &snap, t0)
                    .is_ok(),
                "{kind:?}"
            );
            assert!(guard
                .check(&uid, false, &answer(17, None, Some("go")), &snap, t0)
                .is_ok());
        }
        for cap in [OrderCap::Push, OrderCap::Poll, OrderCap::None] {
            let snap = snapshot_with(agent_with(OriginKind::Permission, &["Allow"], cap));
            let mut fresh = Guard::new();
            assert!(
                fresh.check(&uid, false, &order("x"), &snap, t0).is_ok(),
                "{cap:?}"
            );
        }
        // An unknown agent is the store's not_found, never a confirmation.
        assert!(guard
            .check(
                &uid,
                false,
                &answer(17, Some(0), None),
                &Snapshot::default(),
                t0
            )
            .is_ok());
        assert!(guard
            .check(&uid, false, &order("x"), &Snapshot::default(), t0)
            .is_ok());
    }

    #[test]
    fn listening_for_an_answer_on_a_permission_card_is_guarded_before_capture() {
        let guard = Guard::new();
        let agent = AgentId("ag_1".into());
        let token = Principal::Token("mcp".into());
        let snap = snapshot_with(agent_with(
            OriginKind::Elicitation,
            &["Allow", "Deny"],
            OrderCap::None,
        ));
        let refused = guard
            .check_listen(&token, false, &agent, "answer", None, &snap)
            .unwrap_err();
        assert!(matches!(
            refused,
            Refusal::Untrusted {
                card: Some(CardId(17)),
                order: None,
                ..
            }
        ));
        assert!(guard
            .check_listen(&token, false, &agent, "answer", Some(CardId(17)), &snap)
            .is_err());
        assert!(
            guard
                .check_listen(&token, false, &agent, "answer", Some(CardId(3)), &snap)
                .is_ok(),
            "not the current card"
        );
        assert!(guard
            .check_listen(&token, false, &agent, "talk", None, &snap)
            .is_ok());
        assert!(guard
            .check_listen(&token, true, &agent, "answer", None, &snap)
            .is_ok());
        assert!(guard
            .check_listen(&Principal::Device, false, &agent, "answer", None, &snap)
            .is_ok());
        let custom = snapshot_with(agent_with(OriginKind::Custom, &["a"], OrderCap::None));
        assert!(guard
            .check_listen(&token, false, &agent, "answer", None, &custom)
            .is_ok());
        assert!(guard
            .check_listen(&token, false, &agent, "answer", None, &Snapshot::default())
            .is_ok());
    }
}
