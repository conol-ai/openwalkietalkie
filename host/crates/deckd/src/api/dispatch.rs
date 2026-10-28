//! The one dispatch every door calls (contract §3.3, §3.4): the method
//! table (the single source of names, scopes and who answers — also served
//! in `/v1/schema.methods`), the scope check, then either the reducer
//! (`CoreMsg::Api` with the params deserialized into a `Request`) or a
//! core-answered `Call`.
use super::session::{has_scope, scope_name, ClientInfo};
use crate::core::handle::{CallCtx, CoreHandle, Reply, WaitResult};
use deck_model::{AgentId, ApiError, Principal, Request, Scope, SessionId};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::Duration;

/// `agent.wait`'s long poll when the caller names no `timeout_s`, and the
/// longest one a caller may ask for (design §4.6: `timeout_s: 600`).
pub const WAIT_DEFAULT_S: u64 = 600;
pub const WAIT_MAX_S: u64 = 600;

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    /// `None`: no scope needed (`session.*`).
    pub scope: Option<Scope>,
    /// True when the params are a `Request` for the reducer.
    pub reducer: bool,
}

const fn m(name: &'static str, scope: Option<Scope>, reducer: bool) -> MethodSpec {
    MethodSpec {
        name,
        scope,
        reducer,
    }
}

/// The scope table of contract §3.4.
pub const METHODS: &[MethodSpec] = &[
    m("session.hello", None, false),
    m("session.subscribe", None, false),
    m("session.unsubscribe", None, false),
    m("session.ping", None, false),
    m("state.get", Some(Scope::Observe), false),
    m("state.events", Some(Scope::Observe), false),
    m("agent.list", Some(Scope::Observe), false),
    m("agent.get", Some(Scope::Observe), false),
    m("image.get", Some(Scope::ObserveContent), false),
    m("device.get", Some(Scope::Observe), false),
    m("voice.get", Some(Scope::Observe), false),
    m("service.info", Some(Scope::Observe), false),
    m("agent.register", Some(Scope::Agent), true),
    m("agent.heartbeat", Some(Scope::Agent), true),
    m("agent.update", Some(Scope::Agent), true),
    m("agent.companion.set", Some(Scope::Agent), true),
    m("agent.session.set", Some(Scope::Agent), true),
    m("agent.release", Some(Scope::Agent), true),
    m("agent.claim", Some(Scope::Agent), true),
    m("agent.focus", Some(Scope::Agent), true),
    m("agent.card.set", Some(Scope::Agent), true),
    m("agent.card.clear", Some(Scope::Agent), true),
    m("agent.reply", Some(Scope::Agent), true),
    m("agent.wait", Some(Scope::Agent), false),
    m("op.answer", Some(Scope::Operate), true),
    m("op.order", Some(Scope::Operate), true),
    m("op.focus", Some(Scope::Operate), true),
    m("op.snooze", Some(Scope::Operate), true),
    m("op.ack", Some(Scope::Operate), true),
    // B5: the four that change something are reducer methods; their guard
    // and their rate limit run through `Guard::check` with every other
    // reducer method's, which is a **different** entry point from the
    // `check_listen` the core-answered version went through.
    m("voice.say", Some(Scope::Voice), true),
    m("voice.stop", Some(Scope::Voice), true),
    m("voice.listen", Some(Scope::Voice), true),
    m("voice.cancel", Some(Scope::Voice), true),
    // …and the two that only read stay core-answered.
    m("voice.engines", Some(Scope::Voice), false),
    m("voice.test", Some(Scope::Voice), false),
    // USB firmware: observing status is read-only; downloads and flash
    // operations require the local operator's admin scope.
    m("firmware.status", Some(Scope::Observe), false),
    m("firmware.check", Some(Scope::Admin), false),
    m("firmware.install", Some(Scope::Admin), false),
    // The local recogniser's model (§7.6): reading where it stands is
    // `voice`; fetching 1.6 GB onto the disk or deleting it is `admin`.
    m("stt.status", Some(Scope::Voice), false),
    m("stt.install", Some(Scope::Admin), false),
    m("stt.remove", Some(Scope::Admin), false),
    // The local voice's package, the same way.
    m("tts.status", Some(Scope::Voice), false),
    m("tts.install", Some(Scope::Admin), false),
    m("tts.remove", Some(Scope::Admin), false),
    // B9. §4.11 names both and gives neither a scope, so this contract
    // sets them: `perm.status` is `voice` because the Voice screen reads
    // it and it discloses nothing but the daemon's own grants;
    // `perm.request` is `admin` because it **prompts the operator** —
    // putting a system dialog on somebody's screen is not something a
    // read-only client may do.
    m("perm.status", Some(Scope::Voice), false),
    m("perm.request", Some(Scope::Admin), false),
    m("device.set", Some(Scope::Device), true),
    m("device.faces.set", Some(Scope::Device), true),
    m("device.usage.set", Some(Scope::Device), true),
    m("device.macro.set", Some(Scope::Device), true),
    m("device.ws.set", Some(Scope::Device), true),
    m("device.name", Some(Scope::Device), true),
    m("focus.report", Some(Scope::Device), true),
    m("config.get", Some(Scope::Admin), false),
    m("config.set", Some(Scope::Admin), false),
    m("config.reload", Some(Scope::Admin), false),
    m("token.create", Some(Scope::Admin), false),
    m("token.list", Some(Scope::Admin), false),
    m("token.revoke", Some(Scope::Admin), false),
    m("token.rotate", Some(Scope::Admin), false),
    m("metrics.get", Some(Scope::Admin), false),
    // `integrations.*` (§4.11) reads and writes third-party config files, so
    // it is answered on a blocking pool by `crate::integrations`, never as a
    // `CoreMsg`: a disk that stalls must not stall the core thread.
    m("integrations.status", Some(Scope::Admin), false),
    m("integrations.install", Some(Scope::Admin), false),
    m("integrations.uninstall", Some(Scope::Admin), false),
    m("integrations.repair", Some(Scope::Admin), false),
    m("log.tail", Some(Scope::Admin), false),
    m("device.flood", Some(Scope::Admin), false),
    m("device.sync", Some(Scope::Admin), false),
    // `service.restart` (exit 75) and `service.stop` (exit 0). They were
    // out of the table for as long as the daemon had no way to hand its
    // own runtime an exit code — `/v1/schema` is what a client calls from,
    // so it may not advertise a `-32601` — and `crate::service::Exit` is
    // that way. Core-answered: the code is set on the core thread and the
    // reply is written before the supervisor on `main` may act on it.
    m("service.restart", Some(Scope::Admin), false),
    m("service.stop", Some(Scope::Admin), false),
    m("hooks.claude", Some(Scope::HooksIngest), false),
    m("hooks.codex", Some(Scope::HooksIngest), false),
    m("hooks.gemini", Some(Scope::HooksIngest), false),
    m("hooks.codex_notify", Some(Scope::HooksIngest), false),
    m("hooks.claude_statusline", Some(Scope::HooksIngest), false),
];

/// The table row of a method name.
pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|m| m.name == name)
}

/// Whether this method's `Reply` may carry a `Delivery` that the door
/// confirms only **after** the response was written (§4.8 `poll`: the
/// card is cleared "when the `agent.wait` response is written").
///
/// A JSON-RPC notification is never answered, so the notification form of
/// such a method can never deliver anything: a door refuses to run it at
/// all rather than popping the session's mailbox for nobody (the answer
/// would be lost while the card reported `delivered`).
pub fn delivers_after_write(method: &str) -> bool {
    method == "agent.wait"
}

/// Whether the **notification** form of this method must be refused
/// outright, because the one thing the call exists to produce is the
/// response nobody would read.
///
/// Two families qualify and they fail the same way. `agent.wait` pops the
/// session's mailbox: with no `id` the answer is lost behind a card that
/// reported `delivered`. A `hooks.<cli>` call parks a hold and raises a
/// card on the deck, and a held hook's *entire product* is the response
/// written back to the CLI — so a notification form of it puts a card in
/// front of the operator that no CLI is waiting on, burns one of the 64
/// `[api] max_held_hooks` slots for the whole `hold_s`, and reports the
/// operator's answer as delivered to nobody. Sixty-four of those and every
/// real hook fails open.
///
/// Both doors ask this before they dispatch (`conn::answer`,
/// `http::answer_http`); `POST /v1/hooks/{cli}` always has a response to
/// write, so it is not in this shape at all.
pub fn answers_on_write(method: &str) -> bool {
    delivers_after_write(method) || super::hooks::answered(method)
}

/// `[{name, scope, reducer}]` for `/v1/schema.methods`.
pub fn methods_json() -> Vec<serde_json::Value> {
    METHODS
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "scope": m.scope.map(scope_name),
                "reducer": m.reducer,
            })
        })
        .collect()
}

/// Who is calling, as one door resolved it at hello (or per HTTP request).
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub core: CoreHandle,
    pub principal: Principal,
    pub session: SessionId,
    pub scopes: Vec<Scope>,
    pub trusted: bool,
    pub client: ClientInfo,
    /// The daemon's adapters (§5), shared by every door: `hooks.<cli>`
    /// runs on this side of the core, and an adapter's memo — its
    /// internal session, the agent behind each CLI session, the dedupe
    /// windows — has to outlive one request and be the same on both
    /// doors.
    pub adapters: std::sync::Arc<crate::adapters::Adapters>,
}

impl ConnCtx {
    pub fn call_ctx(&self) -> CallCtx {
        CallCtx {
            principal: self.principal.clone(),
            session: self.session,
            scopes: self.scopes.clone(),
            trusted: self.trusted,
            own: BTreeSet::new(),
        }
    }
}

/// `agent.wait` params (§4.6).
#[derive(Debug, Deserialize)]
struct WaitParams {
    agent: AgentId,
    #[serde(default)]
    timeout_s: Option<u64>,
}

/// The long poll a door parks on: `CoreMsg::Wait` owns the oneshot, so the
/// core can hold the reply until the mailbox fills or the timer fires —
/// which `CoreMsg::Call` (a value returned from inside the core loop)
/// cannot do. The delivery a resolved wait carries is confirmed by the
/// door *after* the response line is written (§4.8 `poll`), which is what
/// `Reply::after_write` is for.
async fn wait(ctx: &ConnCtx, params: serde_json::Value) -> Reply {
    let params: WaitParams = match serde_json::from_value(if params.is_null() {
        serde_json::json!({})
    } else {
        params
    }) {
        Ok(params) => params,
        Err(e) => return Reply::err(ApiError::invalid_params(&e.to_string())),
    };
    let seconds = params.timeout_s.unwrap_or(WAIT_DEFAULT_S).min(WAIT_MAX_S);
    let result = ctx
        .core
        .wait(ctx.session, params.agent, Duration::from_secs(seconds))
        .await;
    wait_reply(result)
}

/// `WaitResult` as the §4.15 JSON the caller polls for.
fn wait_reply(result: WaitResult) -> Reply {
    let (value, after_write) = match result {
        WaitResult::Answer {
            card,
            answer,
            delivery,
        } => {
            let mut value = serde_json::to_value(&answer).unwrap_or(serde_json::Value::Null);
            if !value.is_object() {
                value = serde_json::json!({});
            }
            value["kind"] = "answer".into();
            value["card"] = serde_json::to_value(card).unwrap_or(serde_json::Value::Null);
            (value, Some(delivery))
        }
        WaitResult::Order {
            text,
            ctx,
            choice,
            card,
            delivery,
        } => (
            serde_json::json!({
                "kind": "order", "text": text, "ctx": ctx,
                "choice": choice, "card": card,
            }),
            delivery,
        ),
        WaitResult::Snooze { card } => (serde_json::json!({"kind": "snooze", "card": card}), None),
        WaitResult::Timeout => (serde_json::json!({"kind": "timeout"}), None),
        // The core refused the wait itself (§3.2: the session must own the
        // agent) — an error, never a `{"kind":"timeout"}` the caller would
        // read as "nothing happened".
        WaitResult::Refused(error) => return Reply::err(error),
    };
    Reply {
        result: Ok(value),
        after_write,
    }
}

/// The scope gate alone (the doors run it before spending a core round trip).
pub fn check_scope(ctx_scopes: &[Scope], spec: &MethodSpec) -> Result<(), ApiError> {
    match spec.scope {
        Some(need) if !has_scope(ctx_scopes, need) => Err(ApiError::forbidden(&scope_name(need))),
        _ => Ok(()),
    }
}

/// One request from a session: table, scope, reducer or core call.
pub async fn dispatch(ctx: &ConnCtx, method: &str, params: serde_json::Value) -> Reply {
    let Some(spec) = self::method(method) else {
        return Reply::err(ApiError::method_not_found(method));
    };
    // `hooks.*` is answered on the tokio side and never becomes a
    // `CoreMsg` (B5): a held hook parks for the whole hold budget, and
    // `Core::call` answers from inside the single core thread. This runs
    // before `spec.reducer` is so much as read.
    if method.starts_with("hooks.") {
        return super::hooks::dispatch(ctx, method, params).await;
    }
    if let Err(e) = check_scope(&ctx.scopes, spec) {
        return Reply::err(e);
    }
    // `integrations.*` is file I/O and subprocess probes. Like `hooks.*` it
    // is answered on the tokio side (on a blocking pool), never as a
    // `CoreMsg`, so a slow disk or a wedged CLI cannot stop the core thread.
    if let Some(verb) = method.strip_prefix("integrations.") {
        let mut result = crate::integrations::api_dispatch(verb, params).await;
        if verb == "status" {
            if let Ok(value) = &mut result {
                let hub = &ctx.core.adapters().claude_channels;
                value["claude_channels"] = hub.status();
                value["claude_channels_stale"] = hub.stale_count().into();
            }
        }
        return Reply {
            result,
            after_write: None,
        };
    }
    match method {
        "session.hello" => {
            return Reply::err(ApiError::invalid_request("session.hello was already sent"))
        }
        "session.subscribe" | "session.unsubscribe" => {
            return Reply::err(ApiError::invalid_request(
                "subscriptions belong to a WS/UDS connection",
            ))
        }
        // The only method a door answers by parking on the core rather
        // than by a value round trip.
        "agent.wait" => return wait(ctx, params).await,
        _ => {}
    }
    if spec.reducer {
        let params = if params.is_null() {
            serde_json::json!({})
        } else {
            params
        };
        let req: Request = match serde_json::from_value(serde_json::json!({
            "method": method,
            "params": params,
        })) {
            Ok(req) => req,
            Err(e) => return Reply::err(ApiError::invalid_params(&e.to_string())),
        };
        let result = ctx
            .core
            .api(ctx.principal.clone(), ctx.session, ctx.trusted, req)
            .await;
        return Reply {
            result,
            after_write: None,
        };
    }
    ctx.core.call(ctx.call_ctx(), method, params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::handle::{CoreMsg, Delivery};

    /// The method names the `Request` schema declares (`oneOf[*].properties.method.const`).
    fn request_methods() -> Vec<String> {
        let schema = deck_model::schema::schema()["request"].clone();
        let variants = schema["oneOf"]
            .as_array()
            .or_else(|| schema["anyOf"].as_array())
            .expect("the request schema is a oneOf");
        let mut names = Vec::new();
        for v in variants {
            let method = &v["properties"]["method"];
            if let Some(name) = method["const"].as_str() {
                names.push(name.to_string());
            } else if let Some(list) = method["enum"].as_array() {
                names.extend(list.iter().filter_map(|n| n.as_str().map(String::from)));
            }
        }
        names
    }

    /// The predicate both doors refuse a notification with, and the list
    /// it has to cover.
    ///
    /// `delivers_after_write` was written for `agent.wait` and its doc
    /// explains exactly why such a method's notification form must not run
    /// — "its result would go nowhere … the answer would be lost while the
    /// card reported `delivered`". Week 3 gave `hooks.<cli>` that shape
    /// and did not add it to the list: a request with no `id` parked a
    /// hold, raised a card, and both doors then threw the response away.
    /// Every CLI this daemon answers is in here, by the same list
    /// `hooks::answers` refuses from, so a fourth adapter cannot land
    /// outside it.
    #[test]
    fn every_method_whose_product_is_its_response_refuses_the_notification_form() {
        assert!(answers_on_write("agent.wait"), "the original");
        for method in METHODS.iter().map(|m| m.name) {
            let Some(cli) = method.strip_prefix("hooks.") else {
                continue;
            };
            // A CLI with an adapter answers on write; one without is
            // refused `-32601` before it can park anything.
            assert_eq!(
                answers_on_write(method),
                crate::api::hooks::answers(cli),
                "{method}"
            );
        }
        for method in [
            "agent.card.set",
            "op.answer",
            "session.ping",
            "hooks.gemini",
        ] {
            assert!(!answers_on_write(method), "{method}");
        }
    }

    #[test]
    fn every_method_name_is_unique_and_every_request_variant_is_in_the_table() {
        let names: Vec<&str> = METHODS.iter().map(|m| m.name).collect();
        let unique: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "duplicate method names");
        let requests = request_methods();
        assert!(requests.len() >= 20, "{requests:?}");
        for name in &requests {
            let spec = method(name).unwrap_or_else(|| panic!("{name} missing from the table"));
            assert!(spec.reducer, "{name} is a Request but not marked reducer");
            assert!(spec.scope.is_some(), "{name} needs a scope");
        }
        for spec in METHODS.iter().filter(|m| m.reducer) {
            assert!(
                requests.contains(&spec.name.to_string()),
                "{} marked reducer but not a Request",
                spec.name
            );
        }
        assert_eq!(method("image.get").unwrap().scope, Some(Scope::ObserveContent));
        assert_eq!(method("session.ping").unwrap().scope, None);
        assert_eq!(method("op.answer").unwrap().scope, Some(Scope::Operate));
        assert_eq!(method("device.flood").unwrap().scope, Some(Scope::Admin));
        assert_eq!(
            method("hooks.claude").unwrap().scope,
            Some(Scope::HooksIngest)
        );
        assert!(
            method("service.bound").is_none(),
            "internal, never dispatchable"
        );
        let json = methods_json();
        assert_eq!(json.len(), METHODS.len());
        assert!(json
            .iter()
            .any(|m| m["name"] == "state.get" && m["scope"] == "observe" && m["reducer"] == false));
        assert!(json.iter().any(|m| m["name"] == "agent.register"
            && m["scope"] == "agent"
            && m["reducer"] == true));
    }

    /// The table is `/v1/schema.methods`: a client reads it and calls what
    /// it names, so nothing in it may answer `-32601 method_not_found`.
    /// `hooks.*` is the one declared exception (`{available:"week 3"}`).
    #[tokio::test]
    async fn no_advertised_method_answers_method_not_found() {
        let daemon =
            crate::api::testkit::test_api("dispatch-table", crate::trust::FakeVerifier::default());
        let session = daemon
            .api
            .core
            .open_session(crate::core::handle::OpenSession {
                kind: crate::core::handle::SessionKind::Observer,
                principal: Principal::Uid(501),
                scopes: vec![Scope::Admin],
                trusted: false,
                client: ClientInfo::default(),
            })
            .await
            .unwrap();
        let ctx = ConnCtx {
            core: daemon.api.core.clone(),
            principal: Principal::Uid(501),
            session,
            scopes: vec![Scope::Admin],
            trusted: false,
            client: ClientInfo::default(),
            adapters: std::sync::Arc::default(),
        };
        let mut unanswered = Vec::new();
        for spec in METHODS.iter().filter(|m| !m.name.starts_with("hooks.")) {
            // Empty params: a method that exists refuses them with
            // `-32602`, or answers; only a missing one is `-32601`.
            let reply = dispatch(&ctx, spec.name, serde_json::json!({})).await;
            if let Err(e) = reply.result {
                if e.code == ApiError::METHOD_NOT_FOUND {
                    unanswered.push(spec.name);
                }
            }
        }
        assert!(
            unanswered.is_empty(),
            "advertised in /v1/schema but answered by nobody: {unanswered:?}"
        );
    }

    fn ctx(core: CoreHandle, scopes: &[Scope]) -> ConnCtx {
        ConnCtx {
            core,
            principal: Principal::Uid(501),
            session: SessionId(2),
            scopes: scopes.to_vec(),
            trusted: false,
            client: ClientInfo::default(),
            adapters: std::sync::Arc::default(),
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn the_gate_refuses_before_the_core_and_routes_reducer_methods_as_requests() {
        let (core, rx) = CoreHandle::detached();
        let rt = rt();
        // Unknown, hooks, missing scope, bad params: nothing reaches the core.
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Admin]),
            "nope.x",
            serde_json::json!({}),
        ));
        assert_eq!(r.result.unwrap_err().code, ApiError::METHOD_NOT_FOUND);
        // `hooks.*` never becomes a `CoreMsg` (B5): a CLI with no adapter
        // in this daemon is refused by name, right here.
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::HooksIngest]),
            "hooks.gemini",
            serde_json::json!({}),
        ));
        let err = r.result.unwrap_err();
        assert_eq!(err.code, ApiError::METHOD_NOT_FOUND);
        assert_eq!(err.data["available"], "v1.1");
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Observe]),
            "op.answer",
            serde_json::json!({}),
        ));
        let err = r.result.unwrap_err();
        assert_eq!(err.code, ApiError::FORBIDDEN);
        assert_eq!(err.data["need"], "operate");
        let r = rt.block_on(dispatch(&ctx(core.clone(), &[Scope::Observe]), "image.get", serde_json::json!({"image":"anything"})));
        assert_eq!(r.result.unwrap_err().code, ApiError::FORBIDDEN);
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Agent]),
            "agent.register",
            serde_json::json!({"kind": "other"}),
        ));
        let err = r.result.unwrap_err();
        assert_eq!(err.code, ApiError::INVALID_PARAMS);
        assert!(err.data["why"].as_str().unwrap().contains("missing field"));
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Observe]),
            "session.hello",
            serde_json::json!({}),
        ));
        assert_eq!(r.result.unwrap_err().code, ApiError::INVALID_REQUEST);
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[]),
            "session.subscribe",
            serde_json::json!({}),
        ));
        assert_eq!(r.result.unwrap_err().code, ApiError::INVALID_REQUEST);
        assert!(rx.try_recv().is_err(), "nothing reached the core");
        // A well-formed reducer call is a CoreMsg::Api; a core method a Call.
        let answerer = std::thread::spawn(move || {
            match rx.recv().unwrap() {
                CoreMsg::Api {
                    req,
                    reply,
                    session,
                    ..
                } => {
                    assert!(matches!(*req, Request::AgentClaim { ref key } if key == "k"));
                    assert_eq!(session, SessionId(2));
                    reply
                        .send(Ok(serde_json::json!({"agent": "ag_1"})))
                        .unwrap();
                }
                other => panic!("{other:?}"),
            }
            match rx.recv().unwrap() {
                CoreMsg::Call {
                    method, ctx, reply, ..
                } => {
                    assert_eq!(method, "state.get");
                    assert_eq!(ctx.scopes, vec![Scope::Observe]);
                    assert!(ctx.own.is_empty());
                    reply
                        .send(Reply::ok(serde_json::json!({"rev": 0})))
                        .unwrap();
                }
                other => panic!("{other:?}"),
            }
        });
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Agent]),
            "agent.claim",
            serde_json::json!({"key": "k"}),
        ));
        assert_eq!(r.result.unwrap()["agent"], "ag_1");
        let r = rt.block_on(dispatch(
            &ctx(core, &[Scope::Observe]),
            "state.get",
            serde_json::Value::Null,
        ));
        assert_eq!(r.result.unwrap()["rev"], 0);
        answerer.join().unwrap();
    }

    #[test]
    fn agent_wait_parks_on_the_core_and_its_result_carries_the_delivery() {
        use deck_model::{Answer, AnswerSource, CardId};
        let (core, rx) = CoreHandle::detached();
        let rt = rt();
        // Bad params never reach the core.
        let r = rt.block_on(dispatch(
            &ctx(core.clone(), &[Scope::Agent]),
            "agent.wait",
            serde_json::json!({}),
        ));
        assert_eq!(r.result.unwrap_err().code, ApiError::INVALID_PARAMS);
        assert!(rx.try_recv().is_err());
        // The timeout is the caller's, capped at WAIT_MAX_S; the default
        // applies when it is omitted.
        let answerer = std::thread::spawn(move || {
            for (want, answer) in [
                (
                    Duration::from_secs(5),
                    WaitResult::Answer {
                        card: CardId(17),
                        answer: Answer {
                            choice: Some(0),
                            label: Some("Yes".into()),
                            text: None,
                            source: AnswerSource::Deck,
                            principal: Principal::Device,
                            session: None,
                            utterance: None,
                        },
                        delivery: Delivery {
                            agent: AgentId("ag_1".into()),
                            card: CardId(17),
                        },
                    },
                ),
                (
                    Duration::from_secs(WAIT_MAX_S),
                    WaitResult::Order {
                        text: "ship it".into(),
                        ctx: "step".into(),
                        choice: Some(1),
                        card: Some(CardId(18)),
                        delivery: None,
                    },
                ),
                (
                    Duration::from_secs(WAIT_DEFAULT_S),
                    WaitResult::Snooze { card: CardId(19) },
                ),
                (Duration::from_secs(1), WaitResult::Timeout),
            ] {
                match rx.recv().unwrap() {
                    CoreMsg::Wait {
                        session,
                        agent,
                        timeout,
                        reply,
                    } => {
                        assert_eq!(session, SessionId(2));
                        assert_eq!(agent, AgentId("ag_1".into()));
                        assert_eq!(timeout, want);
                        reply.send(answer).unwrap();
                    }
                    other => panic!("{other:?}"),
                }
            }
        });
        let call = |timeout: Option<u64>| {
            let mut params = serde_json::json!({"agent": "ag_1"});
            if let Some(t) = timeout {
                params["timeout_s"] = t.into();
            }
            rt.block_on(dispatch(
                &ctx(core.clone(), &[Scope::Agent]),
                "agent.wait",
                params,
            ))
        };
        let r = call(Some(5));
        assert_eq!(
            r.after_write,
            Some(Delivery {
                agent: AgentId("ag_1".into()),
                card: CardId(17)
            }),
            "the door confirms it once the response is written"
        );
        let value = r.result.unwrap();
        assert_eq!(value["kind"], "answer");
        assert_eq!(value["card"], 17);
        assert_eq!(value["choice"], 0);
        assert_eq!(value["label"], "Yes");
        assert_eq!(value["source"], "deck");
        let r = call(Some(WAIT_MAX_S * 10));
        assert!(r.after_write.is_none());
        let value = r.result.unwrap();
        assert_eq!(value["kind"], "order");
        assert_eq!(value["text"], "ship it");
        assert_eq!(value["ctx"], "step");
        assert_eq!(value["choice"], 1);
        assert_eq!(value["card"], 18);
        let value = call(None).result.unwrap();
        assert_eq!(value["kind"], "snooze");
        assert_eq!(value["card"], 19);
        let value = call(Some(1)).result.unwrap();
        assert_eq!(value, serde_json::json!({"kind": "timeout"}));
        answerer.join().unwrap();
    }
}
