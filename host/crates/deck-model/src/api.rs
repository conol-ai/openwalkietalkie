//! The API surface of the reducer (§4.6–§4.10): `Store::call` returns the
//! method result the dispatcher answers with, next to the events and
//! effects; `apply(Command::Api)` runs the same code and drops the result.
//! Scopes and trust are the dispatcher's (§4.2): the reducer enforces
//! ownership (sessions), state machines and ranges.
use crate::{
    AgentId, AnswerSource, CardId, Effect, Event, FocusSource, OpAnswer, Owner, Principal, Request,
    SessionId, Store,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Instant;

/// A JSON-RPC error of §4.4 (`code`, `message`, `data`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    pub code: i32,
    pub message: String,
    pub data: serde_json::Value,
}

impl ApiError {
    // JSON-RPC 2.0 standard codes.
    pub const PARSE: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL: i32 = -32603;
    // The server range of §4.4.
    pub const UNAUTHORIZED: i32 = -32001;
    pub const FORBIDDEN: i32 = -32002;
    pub const NOT_FOUND: i32 = -32003;
    pub const CONFLICT: i32 = -32004;
    pub const BAD_STATE: i32 = -32005;
    pub const UNDELIVERABLE: i32 = -32006;
    pub const DEVICE_OFFLINE: i32 = -32007;
    pub const RATE_LIMITED: i32 = -32008;
    pub const INVALID: i32 = -32009;
    pub const TIMEOUT: i32 = -32010;
    pub const LEASE_EXPIRED: i32 = -32011;
    pub const TOO_LARGE: i32 = -32012;
    pub const DEGRADED: i32 = -32013;

    fn new(code: i32, message: &str, data: serde_json::Value) -> Self {
        ApiError {
            code,
            message: message.to_string(),
            data,
        }
    }

    /// `-32700`: the line/frame/body was not JSON.
    pub fn parse(why: &str) -> Self {
        Self::new(Self::PARSE, "parse_error", serde_json::json!({"why": why}))
    }

    /// `-32600`: JSON, but not a JSON-RPC request (or an empty batch).
    pub fn invalid_request(why: &str) -> Self {
        Self::new(
            Self::INVALID_REQUEST,
            "invalid_request",
            serde_json::json!({"why": why}),
        )
    }

    pub fn method_not_found(name: &str) -> Self {
        Self::new(
            Self::METHOD_NOT_FOUND,
            "method_not_found",
            serde_json::json!({"method": name}),
        )
    }

    /// `-32602`: `why` is the serde message, never the raw params.
    pub fn invalid_params(why: &str) -> Self {
        Self::new(
            Self::INVALID_PARAMS,
            "invalid_params",
            serde_json::json!({"why": why}),
        )
    }

    pub fn internal(why: &str) -> Self {
        Self::new(
            Self::INTERNAL,
            "internal_error",
            serde_json::json!({"why": why}),
        )
    }

    /// `-32001`: no or a bad token, or no `session.hello` first.
    pub fn unauthorized() -> Self {
        Self::new(Self::UNAUTHORIZED, "unauthorized", serde_json::json!({}))
    }

    /// `-32001 {why}`, e.g. `why:"hello_first"`.
    pub fn unauthorized_why(why: &str) -> Self {
        Self::new(
            Self::UNAUTHORIZED,
            "unauthorized",
            serde_json::json!({"why": why}),
        )
    }

    pub fn forbidden(need: &str) -> Self {
        Self::new(
            Self::FORBIDDEN,
            "forbidden",
            serde_json::json!({"need": need}),
        )
    }

    /// A guarded operation from an untrusted principal (§4.2).
    pub fn forbidden_trusted() -> Self {
        Self::new(
            Self::FORBIDDEN,
            "forbidden",
            serde_json::json!({"need": "trusted", "how": "gui_confirm"}),
        )
    }

    /// The agent-descended cap refused `need` (§4.2).
    pub fn forbidden_ancestry(need: &str) -> Self {
        Self::new(
            Self::FORBIDDEN,
            "forbidden",
            serde_json::json!({"need": need, "reason": "agent_ancestry"}),
        )
    }

    pub fn device_offline() -> Self {
        Self::new(
            Self::DEVICE_OFFLINE,
            "device_offline",
            serde_json::json!({}),
        )
    }

    pub fn rate_limited(retry_ms: u32) -> Self {
        Self::new(
            Self::RATE_LIMITED,
            "rate_limited",
            serde_json::json!({"retry_ms": retry_ms}),
        )
    }

    pub fn timeout() -> Self {
        Self::new(Self::TIMEOUT, "timeout", serde_json::json!({}))
    }

    /// `-32012 {max}`: a line, frame or body past the cap.
    pub fn too_large(max: usize) -> Self {
        Self::new(
            Self::TOO_LARGE,
            "too_large",
            serde_json::json!({"max": max}),
        )
    }

    /// `-32013 {reason}`, e.g. `"voice_unavailable"`, `"not_built"`.
    pub fn degraded(reason: &str) -> Self {
        Self::new(
            Self::DEGRADED,
            "degraded",
            serde_json::json!({"reason": reason}),
        )
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(
            Self::NOT_FOUND,
            "not_found",
            serde_json::json!({"what": what}),
        )
    }

    /// The agent is owned by another live session.
    pub fn conflict(owner: &Owner) -> Self {
        Self::new(
            Self::CONFLICT,
            "conflict",
            serde_json::json!({"owner": {"principal": owner.principal, "session": owner.session}}),
        )
    }

    /// `-32004`: the PTT capture belongs to another session (§4.9, M5).
    ///
    /// Same shape as [`ApiError::conflict`] so a client reads one kind of
    /// "somebody else has this", but the session is optional: a deck press
    /// is `principal:"device"` with no connection behind it at all, and
    /// that is exactly the session an API caller may never take.
    pub fn held_by(principal: &Principal, session: Option<SessionId>) -> Self {
        Self::new(
            Self::CONFLICT,
            "conflict",
            serde_json::json!({"owner": {"principal": principal, "session": session}}),
        )
    }

    /// The card id is not the agent's current card.
    pub fn stale_card(current: Option<CardId>) -> Self {
        Self::new(
            Self::CONFLICT,
            "conflict",
            serde_json::json!({"card": current}),
        )
    }

    pub fn bad_state(status: &str, reason: Option<&str>) -> Self {
        Self::new(
            Self::BAD_STATE,
            "bad_state",
            serde_json::json!({"status": status, "reason": reason}),
        )
    }

    pub fn undeliverable(reason: &str) -> Self {
        Self::new(
            Self::UNDELIVERABLE,
            "undeliverable",
            serde_json::json!({"reason": reason}),
        )
    }

    pub fn invalid(field: &str, why: &str) -> Self {
        Self::new(
            Self::INVALID,
            "invalid",
            serde_json::json!({"field": field, "why": why}),
        )
    }

    pub fn lease_expired() -> Self {
        Self::new(Self::LEASE_EXPIRED, "lease_expired", serde_json::json!({}))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}) {}", self.message, self.code, self.data)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult = Result<serde_json::Value, ApiError>;

impl Store {
    /// One API request from `principal` on `session`: the result for the
    /// caller plus the events and effects, exactly like `apply`.
    pub fn call(
        &mut self,
        principal: Principal,
        session: SessionId,
        req: Request,
        now: Instant,
    ) -> (ApiResult, Vec<Event>, Vec<Effect>) {
        self.now = now;
        let result = self.on_api(principal, session, req);
        let (events, effects) = self.take();
        (result, events, effects)
    }

    /// A held hook (§4.8 `hold`) with its result, exactly like `call`: the
    /// card id the store allocated, or the store's own refusal.
    ///
    /// B4: this used to be a `Command`, so `apply` had nowhere to put the
    /// result and dropped it — the core recovered the card id by scanning
    /// the emitted events for a `card.set` and answered every refusal with
    /// one `-32009 invalid{card, refused}`. A hook helper could not tell
    /// "the session ended" from "I built a bad card", which is exactly the
    /// difference between retrying and not.
    pub fn hold(
        &mut self,
        principal: &Principal,
        session: SessionId,
        cmd: crate::CardSet,
        hold: crate::HoldId,
        now: Instant,
    ) -> (Result<CardId, ApiError>, Vec<Event>, Vec<Effect>) {
        self.now = now;
        let result = self.set_card_held(principal, session, cmd, hold);
        let (events, effects) = self.take();
        (result, events, effects)
    }

    pub(crate) fn rev(&self) -> serde_json::Value {
        serde_json::json!({"rev": self.snapshot.rev})
    }

    pub(crate) fn on_api(
        &mut self,
        principal: Principal,
        session: SessionId,
        req: Request,
    ) -> ApiResult {
        match req {
            Request::AgentRegister(r) => self.register(principal, session, r),
            Request::AgentHeartbeat { agents } => self.heartbeat(session, &agents),
            Request::AgentUpdate(u) => self.update_agent(session, u),
            Request::CompanionSet(r) => self.set_companion(session, r),
            Request::SessionSet(r) => self.set_session_controls(session, r),
            Request::AgentRelease { agent, reason } => {
                self.owned(&agent, session)?;
                self.remove_agent(&agent, reason.as_deref().unwrap_or("released"));
                Ok(self.rev())
            }
            Request::AgentClaim { key } => self.claim(principal, session, &key),
            Request::AgentFocus { agent } => {
                self.owned(&agent, session)?;
                self.focus_agent(&agent, FocusSource::Api, principal)
            }
            Request::CardSet(c) => {
                self.owned(&c.agent, session)?;
                let id = self.card_set(&principal, c, false)?;
                Ok(serde_json::json!({"rev": self.snapshot.rev, "card": {"id": id}}))
            }
            Request::CardClear(c) => {
                self.owned(&c.agent, session)?;
                self.card_clear_api(c)
            }
            Request::Reply(r) => {
                self.owned(&r.agent, session)?;
                self.reply(r)
            }
            Request::OpAnswer(OpAnswer {
                agent,
                card,
                choice,
                text,
            }) => self.answer(
                &agent,
                card,
                crate::cards::AnswerInput {
                    // `op.answer` is typed or chosen; a dictated one comes
                    // through §7.4's dispatch with its session's id.
                    utterance: None,
                    choice,
                    text,
                    source: AnswerSource::Api,
                    principal,
                    session: Some(session),
                },
            ),
            Request::OpOrder(o) => self.op_order(principal, session, o),
            Request::OpFocus { agent } => self.focus_agent(&agent, FocusSource::Api, principal),
            Request::OpSnooze { agent, card } => self.snooze(&agent, card, principal),
            Request::OpAck { agent, card } => self.ack(&agent, card, principal),
            Request::FocusReport(f) => self.focus_report(f, principal),
            Request::DeviceSet(d) => self.device_set(d.key, d.value, principal),
            Request::DeviceFacesSet { list } => self.device_faces_set(list),
            Request::DeviceUsageSet { period, items } => {
                self.device_usage_set(period, items, principal)
            }
            Request::DeviceMacroSet {
                f,
                layout,
                src,
                keys,
            } => self.device_macro_set(f, layout, src, keys),
            Request::DeviceWsSet {
                active,
                count,
                sel,
                windows,
            } => self.device_ws_set(active, count, sel, windows),
            Request::DeviceName { text } => self.device_name(text),
            // B5: the four voice methods that change something. The trust
            // guard and the rate limit run in the core before this, through
            // `Guard::check` — the same entry point every other reducer
            // method uses, and a **different** one from the `check_listen`
            // the core-answered `voice.listen` used to go through.
            Request::VoiceSay(r) => self.voice_say(r),
            Request::VoiceStop {
                utterance,
                lane,
                all,
            } => self.voice_stop(utterance, lane, all),
            Request::VoiceListen(r) => self.voice_listen(principal, session, r),
            Request::VoiceCancel {} => self.voice_cancel(session),
        }
    }

    /// The agent exists and `session` owns it (§4.2: sessions, never uids).
    /// Any owner call refreshes the lease.
    pub(crate) fn owned(&mut self, id: &AgentId, session: SessionId) -> Result<(), ApiError> {
        let agent = self
            .snapshot
            .agents
            .get(id)
            .ok_or_else(|| ApiError::not_found("agent"))?;
        if agent.owner.session != session {
            return Err(ApiError::conflict(&agent.owner));
        }
        self.touch(id);
        Ok(())
    }

    pub(crate) fn exists(&self, id: &AgentId) -> Result<(), ApiError> {
        if self.snapshot.agents.contains_key(id) {
            Ok(())
        } else {
            Err(ApiError::not_found("agent"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{claude_adapter, register, Sim};
    use crate::{AgentUpdate, Slot};

    #[test]
    fn errors_carry_the_design_codes() {
        let owner = Owner {
            principal: Principal::Uid(501),
            session: SessionId(3),
            lease_expires: None,
        };
        let e = ApiError::conflict(&owner);
        assert_eq!(e.code, -32004);
        assert_eq!(e.data["owner"]["principal"], "uid:501");
        assert_eq!(e.data["owner"]["session"], "sess_3");
        assert_eq!(ApiError::invalid("f", "w").code, -32009);
        assert_eq!(
            ApiError::bad_state("failed", Some("hold_gone")).code,
            -32005
        );
        assert_eq!(ApiError::undeliverable("dialog_open").code, -32006);
        assert_eq!(ApiError::not_found("agent").code, -32003);
        assert_eq!(ApiError::forbidden("trusted").code, -32002);
        assert_eq!(ApiError::lease_expired().code, -32011);
        assert!(ApiError::stale_card(Some(CardId(4)))
            .to_string()
            .contains("conflict"));
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["message"], "conflict");
    }

    #[test]
    fn the_week_two_errors_carry_their_codes_and_data() {
        let e = ApiError::unauthorized();
        assert_eq!((e.code, e.message.as_str()), (-32001, "unauthorized"));
        assert_eq!(e.data, serde_json::json!({}));
        assert_eq!(
            ApiError::unauthorized_why("hello_first").data["why"],
            "hello_first"
        );
        let e = ApiError::forbidden_trusted();
        assert_eq!(e.code, -32002);
        assert_eq!(
            e.data,
            serde_json::json!({"need": "trusted", "how": "gui_confirm"})
        );
        let e = ApiError::forbidden_ancestry("operate");
        assert_eq!(e.code, -32002);
        assert_eq!(
            e.data,
            serde_json::json!({"need": "operate", "reason": "agent_ancestry"})
        );
        assert_eq!(ApiError::device_offline().code, -32007);
        let e = ApiError::rate_limited(100);
        assert_eq!((e.code, e.message.as_str()), (-32008, "rate_limited"));
        assert_eq!(e.data, serde_json::json!({"retry_ms": 100}));
        assert_eq!(ApiError::timeout().code, -32010);
        let e = ApiError::too_large(262_144);
        assert_eq!(e.code, -32012);
        assert_eq!(e.data["max"], 262_144);
        let e = ApiError::degraded("voice_unavailable");
        assert_eq!((e.code, e.message.as_str()), (-32013, "degraded"));
        assert_eq!(e.data["reason"], "voice_unavailable");
        assert_eq!(ApiError::parse("eof").code, -32700);
        assert_eq!(ApiError::invalid_request("empty batch").code, -32600);
        let e = ApiError::method_not_found("hooks.claude");
        assert_eq!((e.code, e.message.as_str()), (-32601, "method_not_found"));
        assert_eq!(e.data["method"], "hooks.claude");
        let e = ApiError::invalid_params("missing field `agent`");
        assert_eq!(e.code, -32602);
        assert_eq!(e.data["why"], "missing field `agent`");
        assert_eq!(ApiError::internal("x").code, -32603);
        assert_eq!(ApiError::PARSE, -32700);
        assert_eq!(ApiError::INVALID_REQUEST, -32600);
        assert_eq!(ApiError::METHOD_NOT_FOUND, -32601);
        assert_eq!(ApiError::INVALID_PARAMS, -32602);
        assert_eq!(ApiError::INTERNAL, -32603);
        assert_eq!(ApiError::UNAUTHORIZED, -32001);
        assert_eq!(ApiError::DEVICE_OFFLINE, -32007);
        assert_eq!(ApiError::RATE_LIMITED, -32008);
        assert_eq!(ApiError::TIMEOUT, -32010);
        assert_eq!(ApiError::TOO_LARGE, -32012);
        assert_eq!(ApiError::DEGRADED, -32013);
    }

    #[test]
    fn call_returns_the_result_and_the_events_of_the_same_apply() {
        let mut sim = Sim::new();
        let (result, events, effects) = sim.store.call(
            Principal::Uid(1),
            SessionId(1),
            Request::AgentRegister(register("bot", None)),
            sim.t,
        );
        let result = result.unwrap();
        assert!(result["agent"].as_str().unwrap().starts_with("ag_"));
        assert_eq!(result["slot"], 1);
        assert!(events
            .iter()
            .any(|e| e.kind == crate::EventKind::AgentAdded));
        assert!(effects.iter().any(|e| matches!(e, Effect::Timer { .. })));
        // The same request through `apply` produces no result but the same state.
        let (events, _) = sim.store.apply(
            crate::Command::Api {
                principal: Principal::Uid(1),
                session: SessionId(1),
                req: Box::new(Request::AgentRegister(register("bot2", None))),
            },
            sim.t,
        );
        assert_eq!(sim.store.snapshot().agents.len(), 2);
        assert!(events
            .iter()
            .any(|e| e.kind == crate::EventKind::AgentAdded));
    }

    #[test]
    fn ownership_is_per_session_not_per_principal() {
        let mut sim = Sim::new();
        let id = sim.register(Principal::Uid(1), SessionId(1), "bot", Some("myapp:1"));
        let err = sim
            .api(
                Principal::Uid(1),
                SessionId(2),
                Request::AgentUpdate(AgentUpdate {
                    agent: id.clone(),
                    state: Some(crate::AgentState::Working),
                    title: None,
                    title_source: None,
                    since: None,
                    name: None,
                    meta: None,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["session"], "sess_1");
        let err = sim
            .api(
                claude_adapter(),
                SessionId(7),
                Request::AgentRelease {
                    agent: AgentId("ag_nope".into()),
                    reason: None,
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ApiError::NOT_FOUND);
        assert!(sim
            .api(
                Principal::Uid(1),
                SessionId(1),
                Request::AgentRelease {
                    agent: id,
                    reason: None
                }
            )
            .is_ok());
        assert!(sim.store.snapshot().agents.is_empty());
        assert_eq!(sim.store.snapshot().agent_at(Slot(1)), None);
    }
}
