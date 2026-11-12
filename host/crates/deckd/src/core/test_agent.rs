//! The core's side of the test agent (`care::test`): the four `care.test.*`
//! methods, run on the daemon's own care session the way the demo runs its
//! roster (`Core::demo_call_as`), and the step that takes the agent off the
//! deck once its test is over.
use super::handle::{CallCtx, ClientInfo, OpenSession, Reply, SessionKind};
use super::Core;
use crate::care::test::{self, Language, Test};
use deck_model::{
    AgentId, ApiError, ApiResult, CardId, Command, Event, Link, Request, Scope, SessionId,
};
use std::time::Instant;

impl Core {
    /// `care.test.question`, `care.test.voice`, `care.test.say` and
    /// `care.test.status`, each with `{}`.
    pub(crate) fn care_test(
        &mut self,
        ctx: &CallCtx,
        method: &str,
        params: &serde_json::Value,
        now: Instant,
    ) -> Reply {
        if let Err(error) = test::no_params(params) {
            return Reply::err(error);
        }
        let result = match method {
            "care.test.question" => self.start_test(Test::Question, now),
            "care.test.voice" => self.start_test(Test::Voice, now),
            "care.test.say" => self.say_sample(ctx, now),
            "care.test.status" => Ok(self.test_agent.status()),
            other => Err(ApiError::method_not_found(other)),
        };
        match result {
            Ok(value) => Reply::ok(value),
            Err(error) => Reply::err(error),
        }
    }

    /// A test question or a voice test: `{state}`, `sent` or `waiting` once
    /// the card is on its way to the deck, `no_deck` when none is linked.
    fn start_test(&mut self, test: Test, now: Instant) -> ApiResult {
        let at = self.store.ts(now);
        self.test_agent.supersede(now);
        if self.store.snapshot().device.link == Link::Offline {
            // Nothing could reach the deck: the agent leaves with whatever it
            // still showed, and nothing is sent.
            self.release_test_agent(now);
            let state = self.test_agent.no_deck(test, at);
            return Ok(serde_json::json!({"state": state}));
        }
        let agent = self.test_agent_registered(now)?;
        let language = Language::of(self.store.snapshot());
        let set = match self.care_call(Request::CardSet(test::card(test, &agent, language)), now) {
            Ok(set) => set,
            Err(error) => {
                // A test that never reached the deck leaves no agent there.
                self.release_test_agent(now);
                return Err(error);
            }
        };
        let card: CardId = serde_json::from_value(set["card"]["id"].clone())
            .map_err(|e| ApiError::internal(&format!("agent.card.set named no card: {e}")))?;
        let state = self.test_agent.sent(test, card, now, at);
        tracing::info!(
            ?test,
            card = card.0,
            ?language,
            "care: a test is on its way to the deck"
        );
        Ok(serde_json::json!({"state": state}))
    }

    /// `care.test.say`: the sample on the `chatter` lane, `{utterance}`. It
    /// is a `voice.say`, so it takes from the caller's own `voice.say`
    /// budget.
    fn say_sample(&mut self, ctx: &CallCtx, now: Instant) -> ApiResult {
        self.guard.limits.check_say(&ctx.principal, now)?;
        let language = Language::of(self.store.snapshot());
        let said = self.care_call(Request::VoiceSay(test::sample(language)), now)?;
        Ok(serde_json::json!({"utterance": said["utterance"]}))
    }

    /// The agent "Test", registered on the care session when it is not.
    fn test_agent_registered(&mut self, now: Instant) -> Result<AgentId, ApiError> {
        let registered = self
            .test_agent
            .agent()
            .filter(|agent| self.store.snapshot().agents.contains_key(*agent))
            .cloned();
        if let Some(agent) = registered {
            return Ok(agent);
        }
        let result = self.care_call(Request::AgentRegister(test::register()), now)?;
        let agent: AgentId = serde_json::from_value(result["agent"].clone())
            .map_err(|e| ApiError::internal(&format!("agent.register named no agent: {e}")))?;
        self.test_agent.set_agent(agent.clone());
        Ok(agent)
    }

    /// Take the agent "Test" off the deck now, when it is registered.
    fn release_test_agent(&mut self, now: Instant) {
        let Some(agent) = self.test_agent.agent().cloned() else {
            return;
        };
        if let Err(error) = self.care_call(
            Request::AgentRelease {
                agent,
                reason: None,
            },
            now,
        ) {
            tracing::debug!(%error, "care: the test agent was already gone");
        }
    }

    /// The care session, opened on first use: internal, so it never closes
    /// or expires and its agents need no lease.
    fn care_session(&mut self, now: Instant) -> SessionId {
        if let Some(session) = self
            .test_agent
            .session()
            .filter(|session| self.sessions.contains(*session))
        {
            return session;
        }
        let session = self.sessions.open(
            OpenSession {
                kind: SessionKind::Internal,
                principal: test::principal(),
                scopes: vec![Scope::Agent, Scope::Observe],
                trusted: false,
                client: ClientInfo {
                    name: test::CLIENT.into(),
                    version: super::dispatch::VERSION.into(),
                },
            },
            now,
        );
        tracing::debug!(%session, "care: the test agent's session opened");
        self.test_agent.set_session(session);
        session
    }

    /// One request on the care session, like a dispatcher would run it.
    /// The guard is not asked: this is the daemon's own client.
    fn care_call(&mut self, req: Request, now: Instant) -> ApiResult {
        let session = self.care_session(now);
        let (result, mut events, effects) = self.store.call(test::principal(), session, req, now);
        self.execute_all(effects, &events, now);
        self.drain_followups(&mut events, now);
        result
    }

    /// A step's events end the test on the deck when they answer it or take
    /// its card away (`care::test`); the agent then leaves the deck, after
    /// the step's own follow-ups (the answer's delivery first).
    pub(super) fn settle_tests(&mut self, events: &[Event], now: Instant) {
        let Some(agent) = self.test_agent.observe(events, now) else {
            return;
        };
        let Some(session) = self.test_agent.session() else {
            return;
        };
        // Never what was heard: those are the owner's own words.
        tracing::info!(agent = %agent.0, "care: a test is over; the agent leaves the deck");
        self.followups.push_back(Command::Api {
            principal: test::principal(),
            session,
            req: Box::new(Request::AgentRelease {
                agent,
                reason: None,
            }),
        });
    }
}
