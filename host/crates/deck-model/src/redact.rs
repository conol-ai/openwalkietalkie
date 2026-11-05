//! Content redaction (design §4.3): the snapshot per scope (`redact_for`),
//! and one event at fan-out (`redact_event`). Content fields are nulled and
//! the object carries `redacted:true`; state fields, ids and counts pass.
//! Owners see their own agents whatever their scopes (§4.6, §4.12).
use crate::{Agent, AgentId, CardStatus, Event, EventKind, Scope, Snapshot, TitleSource};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// `snapshot` as a principal with `scopes` sees it: `redact_for` with no
/// agents of its own.
pub fn redact(snapshot: &Snapshot, scopes: &[Scope]) -> Snapshot {
    redact_for(snapshot, scopes, &BTreeSet::new())
}

/// `snapshot` as a principal with `scopes` sees it, keeping the content of
/// the agents in `own` (the caller's session owns them).
pub fn redact_for(snapshot: &Snapshot, scopes: &[Scope], own: &BTreeSet<AgentId>) -> Snapshot {
    let mut out = snapshot.clone();
    if Scope::grants_content(scopes) {
        return out;
    }
    for (id, agent) in out.agents.iter_mut() {
        if !own.contains(id) {
            redact_agent(agent);
        }
    }
    out
}

/// Null every content field of one agent (§4.3) and mark it.
pub fn redact_agent(agent: &mut Agent) {
    if agent.title_source.is_content() {
        agent.title = None;
    }
    agent.session.cwd = None;
    agent.session.pid = None;
    agent.session.terminal = None;
    agent.meta = Value::Null;
    agent.messages.clear();
    agent.workflow = None;
    agent.workflow_runs.clear();
    if let Some(card) = &mut agent.card {
        card.text = None;
        card.choices.clear();
        if let CardStatus::Answering { text, .. } = &mut card.status {
            *text = None;
        }
        card.redacted = true;
    }
    if let Some(reply) = &mut agent.reply {
        reply.text = None;
        reply.announce = None;
        reply.redacted = true;
    }
    agent.redacted = true;
}

/// `event` as a subscriber with `scopes` receives it, or `None` when it
/// must not: `hook.raw` needs `observe:raw`; `agent.answer`/`agent.order`
/// go to the owner only. Otherwise a copy whose content fields (by kind,
/// the table of contract §3.1) are nulled with `data.redacted = true`,
/// unless the scopes grant content or the event is about the subscriber's
/// own agent. Kinds without content pass untouched.
pub fn redact_event(event: &Event, scopes: &[Scope], own_agent: bool) -> Option<Event> {
    match event.kind {
        EventKind::HookRaw if !Scope::grants_raw(scopes) => return None,
        EventKind::AgentAnswer | EventKind::AgentOrder if !own_agent => return None,
        _ => {}
    }
    let mut out = event.clone();
    if Scope::grants_content(scopes) || own_agent || !event.kind.has_content() {
        return Some(out);
    }
    let Some(data) = out.data.as_object_mut() else {
        // Not the shape the store emits: when in doubt, null it.
        out.data = serde_json::json!({"redacted": true});
        return Some(out);
    };
    match event.kind {
        EventKind::AgentAdded => {
            if let Some(agent) = data.get_mut("agent") {
                redact_agent_json(agent);
            }
        }
        EventKind::AgentChanged => {
            if let Some(fields) = data.get_mut("fields").and_then(Value::as_object_mut) {
                // A title is content unless the event says it derives from
                // state; absent `title_source` → the stored one is unknown here.
                let title_is_state = fields
                    .get("title_source")
                    .and_then(|s| serde_json::from_value::<TitleSource>(s.clone()).ok())
                    .is_some_and(|s| !s.is_content());
                if !title_is_state {
                    null_field(fields, "title");
                }
                null_field(fields, "meta");
            }
        }
        EventKind::CardSet => {
            if let Some(card) = data.get_mut("card").and_then(Value::as_object_mut) {
                null_field(card, "text");
                if card.contains_key("choices") {
                    card.insert("choices".into(), Value::Array(Vec::new()));
                }
                card.insert("redacted".into(), Value::Bool(true));
            }
        }
        EventKind::CardAnswered => {
            null_field(data, "label");
            null_field(data, "text");
        }
        EventKind::OrderDispatched
        | EventKind::OrderFailed
        | EventKind::TranscriptPartial
        | EventKind::TranscriptFinal => null_field(data, "text"),
        EventKind::ReplySet => {
            null_field(data, "text");
            null_field(data, "announce");
            if let Some(message) = data.get_mut("message").and_then(Value::as_object_mut) {
                null_field(message, "plan");
                message.remove("images");
                null_field(message, "runs");
                null_field(message, "summary");
            }
        }
        EventKind::OpConfirmRequested => null_field(data, "order"),
        // `hook.raw` with `observe:raw` and the owner-only kinds returned above.
        _ => {}
    }
    data.insert("redacted".into(), Value::Bool(true));
    Some(out)
}

fn null_field(obj: &mut Map<String, Value>, key: &str) {
    if let Some(v) = obj.get_mut(key) {
        *v = Value::Null;
    }
}

/// The `agent.added` payload is a whole `Agent`: run the typed redaction on
/// it; a payload that is not one keeps only its id.
fn redact_agent_json(value: &mut Value) {
    match serde_json::from_value::<Agent>(value.clone()) {
        Ok(mut agent) => {
            redact_agent(&mut agent);
            *value = serde_json::to_value(agent).unwrap_or(Value::Null);
        }
        Err(_) => {
            *value = serde_json::json!({"id": value.get("id").cloned(), "redacted": true});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::fixture_agent as agent;
    use crate::{AgentState, OriginKind, Principal, Ts};

    const OBSERVER: &[Scope] = &[Scope::Observe, Scope::Agent, Scope::Voice];

    fn snap() -> Snapshot {
        let mut snap = Snapshot::default();
        snap.agents.insert(
            AgentId("ag_1".into()),
            agent("ag_1", 1, AgentState::Question),
        );
        snap
    }

    fn event(kind: EventKind, data: Value) -> Event {
        Event {
            rev: 7,
            ts: Ts(1_789_760_523_120),
            kind,
            principal: Principal::Token("hooks".into()),
            data,
        }
    }

    #[test]
    fn redaction_nulls_content_and_keeps_state() {
        let snap = snap();
        let observer = redact(&snap, OBSERVER);
        let a = &observer.agents[&AgentId("ag_1".into())];
        assert!(a.redacted);
        assert_eq!(a.title, None, "prompt-derived title is content");
        assert_eq!(a.title_source, TitleSource::Prompt);
        assert_eq!(a.state, AgentState::Question);
        assert_eq!(a.session.key.as_deref(), Some("claude:1"));
        assert_eq!(a.session.cwd, None);
        assert_eq!(a.session.pid, None);
        assert_eq!(a.meta, Value::Null);
        let card = a.card.as_ref().unwrap();
        assert!(card.redacted && card.text.is_none() && card.choices.is_empty());
        assert_eq!(card.choices_n, 2);
        assert_eq!(card.origin.kind, OriginKind::Permission);
        assert!(a.reply.as_ref().unwrap().redacted);
        let json = serde_json::to_value(a).unwrap();
        assert_eq!(json["redacted"], true);
        assert_eq!(json["card"]["redacted"], true);
        for scopes in [
            &[Scope::ObserveContent][..],
            &[Scope::Operate],
            &[Scope::Admin],
            &[Scope::ObserveRaw],
        ] {
            assert_eq!(redact(&snap, scopes), snap, "{scopes:?}");
        }
        let mut cwd_titled = snap.clone();
        cwd_titled
            .agents
            .get_mut(&AgentId("ag_1".into()))
            .unwrap()
            .title_source = TitleSource::Cwd;
        let observer = redact(&cwd_titled, &[Scope::Observe]);
        assert_eq!(
            observer.agents[&AgentId("ag_1".into())].title.as_deref(),
            Some("Header refactor")
        );
    }

    #[test]
    fn redaction_drops_a_free_text_answer_in_flight() {
        let mut snap = Snapshot::default();
        let mut a = agent("ag_1", 1, AgentState::Question);
        a.card.as_mut().unwrap().status = CardStatus::Answering {
            choice: None,
            text: Some("run it on staging".into()),
        };
        snap.agents.insert(a.id.clone(), a);
        let observer = redact(&snap, &[Scope::Observe]);
        let card = observer.agents[&AgentId("ag_1".into())]
            .card
            .as_ref()
            .unwrap();
        assert_eq!(
            card.status,
            CardStatus::Answering {
                choice: None,
                text: None
            }
        );
    }

    #[test]
    fn owners_keep_the_content_of_their_own_agents() {
        let mut snap = snap();
        snap.agents.insert(
            AgentId("ag_2".into()),
            agent("ag_2", 2, AgentState::Working),
        );
        let own: BTreeSet<AgentId> = [AgentId("ag_2".into())].into_iter().collect();
        let view = redact_for(&snap, OBSERVER, &own);
        assert!(view.agents[&AgentId("ag_1".into())].redacted);
        let mine = &view.agents[&AgentId("ag_2".into())];
        assert!(!mine.redacted);
        assert_eq!(*mine, snap.agents[&AgentId("ag_2".into())]);
        assert_eq!(mine.title.as_deref(), Some("Header refactor"));
        assert_eq!(redact_for(&snap, &[Scope::Operate], &own), snap);
        let nobody = redact_for(&snap, OBSERVER, &BTreeSet::new());
        assert_eq!(nobody, redact(&snap, OBSERVER));
        assert!(nobody.agents[&AgentId("ag_2".into())].redacted);
    }

    #[test]
    fn agent_added_carries_a_redacted_agent() {
        let a = agent("ag_1", 1, AgentState::Question);
        let e = event(
            EventKind::AgentAdded,
            serde_json::json!({"agent": serde_json::to_value(&a).unwrap()}),
        );
        let r = redact_event(&e, OBSERVER, false).unwrap();
        assert_eq!(r.rev, 7);
        assert_eq!(r.kind, EventKind::AgentAdded);
        assert_eq!(r.data["redacted"], true);
        let got: Agent = serde_json::from_value(r.data["agent"].clone()).unwrap();
        let mut want = a.clone();
        redact_agent(&mut want);
        assert_eq!(got, want);
        assert_eq!(r.data["agent"]["title"], Value::Null);
        assert_eq!(r.data["agent"]["card"]["choices_n"], 2);
        assert_eq!(r.data["agent"]["card"]["choices"], serde_json::json!([]));
        assert_eq!(r.data["agent"]["session"]["key"], "claude:1");
        assert_eq!(r.data["agent"]["session"]["cwd"], Value::Null);
        // Content scopes and owners see it whole.
        assert_eq!(redact_event(&e, &[Scope::Operate], false).unwrap(), e);
        assert_eq!(redact_event(&e, OBSERVER, true).unwrap(), e);
        // Not an agent at all: only the id survives.
        let odd = event(
            EventKind::AgentAdded,
            serde_json::json!({"agent": {"id": "ag_9", "x": 1}}),
        );
        let r = redact_event(&odd, OBSERVER, false).unwrap();
        assert_eq!(
            r.data["agent"],
            serde_json::json!({"id": "ag_9", "redacted": true})
        );
        let bare = event(EventKind::AgentAdded, serde_json::json!("nope"));
        assert_eq!(
            redact_event(&bare, OBSERVER, false).unwrap().data,
            serde_json::json!({"redacted": true})
        );
    }

    #[test]
    fn agent_changed_nulls_content_titles_and_meta() {
        let content = event(
            EventKind::AgentChanged,
            serde_json::json!({"agent": "ag_1", "fields": {"title": "Run migrations?", "title_source": "prompt", "state": "question", "meta": {"k": 1}}}),
        );
        let r = redact_event(&content, OBSERVER, false).unwrap();
        assert_eq!(r.data["fields"]["title"], Value::Null);
        assert_eq!(r.data["fields"]["title_source"], "prompt");
        assert_eq!(r.data["fields"]["state"], "question");
        assert_eq!(r.data["fields"]["meta"], Value::Null);
        assert_eq!(r.data["redacted"], true);
        let absent = event(
            EventKind::AgentChanged,
            serde_json::json!({"agent": "ag_1", "fields": {"title": "(disconnected)", "orphaned": true}}),
        );
        let r = redact_event(&absent, OBSERVER, false).unwrap();
        assert_eq!(
            r.data["fields"]["title"],
            Value::Null,
            "no source: in doubt"
        );
        assert_eq!(r.data["fields"]["orphaned"], true);
        for state in ["cwd", "session_name", "api"] {
            let e = event(
                EventKind::AgentChanged,
                serde_json::json!({"agent": "ag_1", "fields": {"title": "openmicro", "title_source": state}}),
            );
            let r = redact_event(&e, OBSERVER, false).unwrap();
            assert_eq!(r.data["fields"]["title"], "openmicro", "{state}");
        }
        let owner = event(
            EventKind::AgentChanged,
            serde_json::json!({"agent": "ag_1", "fields": {"owner": {"principal": "uid:501", "session": "sess_2"}}}),
        );
        let r = redact_event(&owner, OBSERVER, false).unwrap();
        assert_eq!(r.data["fields"]["owner"]["session"], "sess_2");
        assert_eq!(redact_event(&content, OBSERVER, true).unwrap(), content);
    }

    #[test]
    fn card_set_keeps_the_count_and_drops_text_and_choices() {
        let card =
            serde_json::to_value(agent("ag_1", 1, AgentState::Question).card.unwrap()).unwrap();
        let e = event(
            EventKind::CardSet,
            serde_json::json!({"agent": "ag_1", "card": card}),
        );
        let r = redact_event(&e, OBSERVER, false).unwrap();
        let c = &r.data["card"];
        assert_eq!(c["text"], Value::Null);
        assert_eq!(c["choices"], serde_json::json!([]));
        assert_eq!(c["choices_n"], 2);
        assert_eq!(c["id"], 17);
        assert_eq!(c["origin"]["kind"], "permission");
        assert_eq!(c["redacted"], true);
        assert_eq!(r.data["redacted"], true);
        assert_eq!(
            redact_event(&e, &[Scope::ObserveContent], false).unwrap(),
            e
        );
        assert_eq!(redact_event(&e, OBSERVER, true).unwrap(), e);
    }

    #[test]
    fn answers_orders_replies_and_transcripts_lose_their_text() {
        let answered = event(
            EventKind::CardAnswered,
            serde_json::json!({"agent": "ag_1", "card": 17, "choice": 0, "label": "Allow", "text": null, "via": "hold", "source": "deck", "principal": "device"}),
        );
        let r = redact_event(&answered, OBSERVER, false).unwrap();
        assert_eq!(r.data["label"], Value::Null);
        assert_eq!(r.data["text"], Value::Null);
        assert_eq!(r.data["choice"], 0);
        assert_eq!(r.data["via"], "hold");
        assert_eq!(r.data["redacted"], true);
        for kind in [EventKind::OrderDispatched, EventKind::OrderFailed] {
            let e = event(
                kind,
                serde_json::json!({"agent": "ag_1", "via": "poll", "text": "run the smoke tests", "reason": "no_route"}),
            );
            let r = redact_event(&e, OBSERVER, false).unwrap();
            assert_eq!(r.data["text"], Value::Null, "{kind:?}");
            assert_eq!(r.data["via"], "poll");
            assert_eq!(r.data["reason"], "no_route");
            assert_eq!(r.data["redacted"], true);
            assert_eq!(redact_event(&e, OBSERVER, true).unwrap(), e);
        }
        let reply = event(
            EventKind::ReplySet,
            serde_json::json!({"agent": "ag_1", "text": "Tests fail on main.", "announce": "tests fail"}),
        );
        let r = redact_event(&reply, OBSERVER, false).unwrap();
        assert_eq!(r.data["text"], Value::Null);
        assert_eq!(r.data["announce"], Value::Null);
        assert_eq!(r.data["agent"], "ag_1");
        for kind in [EventKind::TranscriptPartial, EventKind::TranscriptFinal] {
            let e = event(
                kind,
                serde_json::json!({"agent": "ag_1", "ctx": "talk", "text": "also run", "utterance": "ut_3", "intent": "report"}),
            );
            let r = redact_event(&e, OBSERVER, false).unwrap();
            assert_eq!(r.data["text"], Value::Null, "{kind:?}");
            assert_eq!(r.data["intent"], "report");
            assert_eq!(r.data["utterance"], "ut_3");
        }
        let confirm = event(
            EventKind::OpConfirmRequested,
            serde_json::json!({"agent": "ag_1", "card": 17, "order": "fix it", "principal": "token:mcp", "need": "trusted"}),
        );
        let r = redact_event(&confirm, OBSERVER, false).unwrap();
        assert_eq!(r.data["order"], Value::Null);
        assert_eq!(r.data["card"], 17);
        assert_eq!(r.data["need"], "trusted");
    }

    #[test]
    fn public_activity_previews_still_require_content_scope() {
        let e = event(EventKind::ReplySet, serde_json::json!({
            "agent":"ag_1", "text":"Running a command", "message":{
                "id":"activity:1", "role":"tool", "activity":"running",
                "summary":{"label":"Running a command", "detail":"private-project/build.sh"}
            }
        }));
        let r = redact_event(&e, OBSERVER, false).unwrap();
        assert!(r.data["message"]["summary"].is_null());
        assert_eq!(r.data["message"]["activity"], "running");
        assert_eq!(redact_event(&e, OBSERVER, true).unwrap(), e);
        assert_eq!(redact_event(&e, &[Scope::ObserveContent], false).unwrap(), e);
    }

    #[test]
    fn owner_only_and_raw_events_are_withheld() {
        for kind in [EventKind::AgentAnswer, EventKind::AgentOrder] {
            let e = event(kind, serde_json::json!({"agent": "ag_1", "text": "x"}));
            assert_eq!(redact_event(&e, OBSERVER, false), None, "{kind:?}");
            assert_eq!(
                redact_event(&e, &[Scope::Admin], false),
                None,
                "{kind:?} even admin"
            );
            assert_eq!(
                redact_event(&e, OBSERVER, true),
                Some(e.clone()),
                "{kind:?} owner"
            );
        }
        let raw = event(
            EventKind::HookRaw,
            serde_json::json!({"cli": "claude", "event": "PreToolUse", "payload_summary": "Bash"}),
        );
        assert_eq!(redact_event(&raw, OBSERVER, false), None);
        assert_eq!(
            redact_event(&raw, &[Scope::Admin], false),
            None,
            "admin is not raw"
        );
        assert_eq!(redact_event(&raw, OBSERVER, true), None, "owners neither");
        assert_eq!(
            redact_event(&raw, &[Scope::ObserveRaw], false),
            Some(raw.clone())
        );
    }

    #[test]
    fn state_events_pass_untouched() {
        for (kind, data) in [
            (
                EventKind::DeviceLink,
                serde_json::json!({"link": "connected", "sid": 3}),
            ),
            (
                EventKind::AgentSlot,
                serde_json::json!({"agent": "ag_1", "slot": 2}),
            ),
            (
                EventKind::CardCleared,
                serde_json::json!({"agent": "ag_1", "card": 17, "delivered": true}),
            ),
            (
                EventKind::AgentRemoved,
                serde_json::json!({"agent": "ag_1", "reason": "released"}),
            ),
            (
                EventKind::Error,
                serde_json::json!({"context": "device.err", "why": "range"}),
            ),
            (EventKind::ServiceShutdown, serde_json::json!({})),
        ] {
            let e = event(kind, data);
            let r = redact_event(&e, OBSERVER, false).unwrap();
            assert_eq!(r, e, "{kind:?}");
            assert!(r.data.get("redacted").is_none(), "{kind:?}");
        }
    }
}
