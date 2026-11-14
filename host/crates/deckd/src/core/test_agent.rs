//! The core's side of the test agent (`care::test`): the four `care.test.*`
//! methods, run on the daemon's own care session the way the demo runs its
//! roster (`Core::demo_call_as`), and the step that takes the agent off the
//! deck once its test is over.
//!
//! The same session asks the deck before a firmware install
//! ([`Core::install_after_press`]): the install waits in the updater until
//! ENTER on the deck starts it ([`Core::start_confirmed_install`]) or its
//! card goes unanswered.
use super::handle::{CallCtx, ClientInfo, OpenSession, Reply, SessionKind};
use super::Core;
use crate::care::test::{self, Language, Press, Test};
use crate::firmware::{self, Install};
use deck_model::{
    AgentId, AgentRegister, ApiError, ApiResult, CardId, Command, Event, Link, Request, Scope,
    SessionId,
};
use std::time::{Duration, Instant};

/// How long an install the deck confirmed waits for the voice to go quiet
/// before it is given up, with the reason `firmware.install` gives.
const QUIET_WITHIN: Duration = Duration::from_secs(test::ANSWER_WITHIN_S as u64);

/// What `firmware.install` does about the press on the deck
/// ([`Core::install_gate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallGate {
    /// The deck has a session with the service and its firmware answers the
    /// install's card with ENTER ([`can_confirm`]), so it is asked
    /// ([`Core::install_after_press`]).
    AskTheDeck,
    /// The deck has a session, but its firmware predates the card's ENTER
    /// ([`CONFIRMS_SINCE`]): asked, it could never answer, and the install
    /// would time out with nothing installed. The app's own Review → Install
    /// is the confirmation, and the install starts as it did before the
    /// card existed.
    AppConfirmed,
    /// The deck on the cable is in recovery, as the last check found it,
    /// and has no session: it cannot show a card, and installs as it always
    /// has, without a press, like [`InstallGate::AppConfirmed`].
    Recovery,
    /// Neither: a deck that runs its firmware but has not linked, which
    /// cannot be asked yet. Refused with [`firmware::WAIT_FOR_LINK`].
    NotLinked,
}

/// The first deck firmware whose install card ENTER answers. Every release
/// before it shows no answerable card, so a deck that runs one is not
/// asked ([`InstallGate::AppConfirmed`]).
pub(crate) const CONFIRMS_SINCE: (u64, u64, u64) = (0, 14, 0);

/// Whether the firmware a deck named in its hello (`0.14.0`, or with the
/// build's suffix, `0.14.0-openwalkietalkie`) answers the install's card.
/// Only major.minor.patch counts: semver would rank the suffixed release
/// below the bare one, as a pre-release. A version that does not parse is
/// firmware nobody can say answers, so it is not asked.
pub(crate) fn can_confirm(fw: &str) -> bool {
    let release = fw.split(['-', '+']).next().unwrap_or_default();
    let mut parts = release.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
            (major, minor, patch) >= CONFIRMS_SINCE
        }
        _ => false,
    }
}

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

    /// How `firmware.install` treats the press on the deck. A session with
    /// the service — the deck's firmware said hello on this link — means it
    /// runs its firmware; one recent enough to answer the card
    /// ([`can_confirm`]) is asked, whatever the last check found, and an
    /// older one installs on the app's confirmation. A deck in recovery
    /// never says hello, so a session proves the recovery flag stale (the
    /// deck restarted into its firmware since), and a stale flag never skips
    /// the card. With no session, only a deck the last check found in
    /// recovery installs without a press, as it always has; any other is
    /// refused until it links.
    pub(crate) fn install_gate(&self) -> InstallGate {
        if let Some(fw) = self.store.snapshot().device.fw.as_deref() {
            return if can_confirm(fw) {
                InstallGate::AskTheDeck
            } else {
                InstallGate::AppConfirmed
            };
        }
        let in_recovery = self
            .firmware
            .status()
            .device
            .is_some_and(|deck| deck.recovery);
        if in_recovery {
            InstallGate::Recovery
        } else {
            InstallGate::NotLinked
        }
    }

    /// `firmware.install` for a deck that cannot be asked yet
    /// ([`InstallGate::NotLinked`]): every refusal the install itself would
    /// answer first, as on a deck that can be asked — so a service that
    /// never opens the deck still says so — and otherwise
    /// [`firmware::WAIT_FOR_LINK`]. Nothing starts, and nothing is put on
    /// the deck.
    pub(crate) fn install_refused_until_linked(&self, install: &Install) -> Reply {
        let why = match self.firmware.vet(install) {
            Err(why) => why,
            Ok(()) => firmware::WAIT_FOR_LINK.to_string(),
        };
        tracing::info!(%why, "firmware: the install was refused before any press");
        Reply::err(ApiError::invalid("firmware", &why))
    }

    /// `firmware.install` on a deck that can answer it. Every refusal the
    /// install would answer is answered now, with no card; otherwise the
    /// deck asks "Install firmware {version}? Press ENTER." with one
    /// choice, Install, and the reply is the status at once, `confirming`.
    /// Nothing is downloaded or written until ENTER on the deck
    /// ([`Core::start_confirmed_install`]); a dismissal, the card's minute
    /// or any other answer leaves the updater idle with the reason
    /// ([`firmware::Updater::not_confirmed`]). A second install while one
    /// waits is refused as busy, so cards never stack.
    pub(crate) fn install_after_press(&mut self, install: Install, now: Instant) -> Reply {
        if let Err(why) = self.firmware.vet(&install) {
            return Reply::err(ApiError::invalid("firmware", &why));
        }
        match self.ask_to_install(&install.version, now) {
            Ok(card) => {
                self.test_agent.install_asked(card);
                let status = self.firmware.await_confirmation(install);
                Reply::ok(self.firmware_reply(status))
            }
            Err(error) => Reply::err(error),
        }
    }

    /// The install's card on the deck, in its language, from the agent
    /// named for its firmware ([`test::register_installer`]).
    fn ask_to_install(&mut self, version: &str, now: Instant) -> Result<CardId, ApiError> {
        let language = Language::of(self.store.snapshot());
        let held = self.test_agent.installer().cloned();
        let agent = self.care_agent(held, test::register_installer(language), now)?;
        self.test_agent.set_installer(agent.clone());
        let card = test::install_card(&agent, version, language);
        let set = match self.care_call(Request::CardSet(card), now) {
            Ok(set) => set,
            Err(error) => {
                // A question that never reached the deck leaves no agent
                // there.
                self.release_care_agent(agent, now);
                return Err(error);
            }
        };
        let card: CardId = serde_json::from_value(set["card"]["id"].clone())
            .map_err(|e| ApiError::internal(&format!("agent.card.set named no card: {e}")))?;
        tracing::info!(
            card = card.0,
            ?language,
            "firmware: the install waits for a press on the deck"
        );
        Ok(card)
    }

    /// The install's card is over: ENTER on the deck starts the install,
    /// anything else leaves nothing installed.
    fn settle_install(&mut self, press: Press, now: Instant) {
        match press {
            Press::Confirmed => {
                tracing::info!("firmware: the deck confirmed the install");
                self.install_pressed = Some(now);
                self.start_confirmed_install(now);
            }
            Press::NotConfirmed => {
                tracing::info!("firmware: not confirmed on the deck; nothing was installed");
                self.firmware.not_confirmed();
            }
        }
    }

    /// The install the deck confirmed starts the way `firmware.install`
    /// starts one: once nobody is talking ([`Core::quiet`]), because it
    /// pauses the deck's link. The press may come while the card is still
    /// being read out, so the install waits for the speech, a held press or
    /// a transcription to end — at the press and on every tick after — and
    /// is given up with `firmware.install`'s own reason after
    /// [`QUIET_WITHIN`]. Meanwhile the status says it waits for that
    /// ([`firmware::Updater::waits_for_quiet`]), not for a press it has had.
    pub(super) fn start_confirmed_install(&mut self, now: Instant) {
        let Some(pressed) = self.install_pressed else {
            return;
        };
        if !self.quiet() {
            if now.saturating_duration_since(pressed) >= QUIET_WITHIN {
                self.install_pressed = None;
                tracing::warn!("firmware: the voice never went quiet; the install did not start");
                self.firmware.abandon(firmware::VOICE_BUSY);
            } else {
                self.firmware.waits_for_quiet();
            }
            return;
        }
        self.install_pressed = None;
        let cache = self.paths.caches.join(firmware::CACHE_DIR);
        match self.firmware.confirmed(cache, self.hid.maintenance.clone()) {
            Ok(_) => tracing::info!("firmware: the confirmed install started"),
            Err(why) => tracing::warn!(%why, "firmware: the confirmed install did not start"),
        }
    }

    /// Nobody is talking: no press is held, no transcription is on its way
    /// and nothing is being said. An install waits for this.
    pub(crate) fn quiet(&self) -> bool {
        let voice = &self.store.snapshot().voice;
        voice.ptt.is_none() && voice.speaking.is_none() && !self.store.is_finalizing()
    }

    /// The agent "Test", registered on the care session when it is not.
    fn test_agent_registered(&mut self, now: Instant) -> Result<AgentId, ApiError> {
        let held = self.test_agent.agent().cloned();
        let agent = self.care_agent(held, test::register(), now)?;
        self.test_agent.set_agent(agent.clone());
        Ok(agent)
    }

    /// `held`, when it is still registered; else `register`'s agent, new on
    /// the care session.
    fn care_agent(
        &mut self,
        held: Option<AgentId>,
        register: AgentRegister,
        now: Instant,
    ) -> Result<AgentId, ApiError> {
        if let Some(agent) = held.filter(|agent| self.store.snapshot().agents.contains_key(agent)) {
            return Ok(agent);
        }
        let result = self.care_call(Request::AgentRegister(register), now)?;
        serde_json::from_value(result["agent"].clone())
            .map_err(|e| ApiError::internal(&format!("agent.register named no agent: {e}")))
    }

    /// Take the agent "Test" off the deck now, when it is registered.
    fn release_test_agent(&mut self, now: Instant) {
        let Some(agent) = self.test_agent.agent().cloned() else {
            return;
        };
        self.release_care_agent(agent, now);
    }

    /// Take `agent` of the care session off the deck now.
    fn release_care_agent(&mut self, agent: AgentId, now: Instant) {
        if let Err(error) = self.care_call(
            Request::AgentRelease {
                agent,
                reason: None,
            },
            now,
        ) {
            tracing::debug!(%error, "care: the agent was already gone");
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

    /// A step's events end a card of the care session when they answer it
    /// or take it away (`care::test`): the install's answer starts or ends
    /// the install, and the agent leaves the deck, after the step's own
    /// follow-ups (the answer's delivery first).
    pub(super) fn settle_tests(&mut self, events: &[Event], now: Instant) {
        let settled = self.test_agent.observe(events, now);
        if let Some(press) = settled.install {
            self.settle_install(press, now);
        }
        let Some(session) = self.test_agent.session() else {
            return;
        };
        for agent in settled.release {
            // Never what was heard: those are the owner's own words.
            tracing::info!(agent = %agent.0, "care: a card is over; its agent leaves the deck");
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
}

#[cfg(test)]
mod tests {
    use super::can_confirm;

    /// The cutoff is the release, whatever the build's suffix: 0.14.0 and
    /// later answer the install's card, earlier ones and versions that do
    /// not parse do not.
    #[test]
    fn the_first_firmware_that_confirms_on_the_deck_is_0_14_0() {
        for fw in ["0.14.0", "0.14.0-openwalkietalkie", "0.14.1", "0.15.0+build", "1.0.0"] {
            assert!(can_confirm(fw), "{fw}");
        }
        for fw in ["0.13.0", "0.13.99-openwalkietalkie", "0.9.0", "", "0.14", "next", "0.14.0.1"] {
            assert!(!can_confirm(fw), "{fw}");
        }
    }
}
