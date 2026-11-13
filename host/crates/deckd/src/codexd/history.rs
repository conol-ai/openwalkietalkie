//! Restore history without asking the server for a single unbounded frame.
use super::{chat_item, questions::Questions, Effect, Model, Rpc};
use serde_json::{json, Value};

/// The chat messages a history read keeps: as many as the store does.
const KEPT: usize = 64;

#[derive(Default)]
pub(super) struct History {
    questions: Questions,
    messages: Vec<(deck_model::MessageMeta, String)>,
    /// Not even the first page came back: this read says nothing about the
    /// thread's chat (a thread with no items says it has none).
    unavailable: bool,
}

/// Every item of the thread, eight a page in `direction` (`asc` or `desc`),
/// handed to `each` until it says enough (`false`) or the pages run out.
/// Whether any page came back.
async fn walk(
    rpc: &Rpc,
    thread: &str,
    direction: &str,
    mut each: impl FnMut(&Value) -> bool,
) -> bool {
    let mut cursor = Value::Null;
    let mut answered = false;
    loop {
        let page = match rpc
            .request(
                "thread/items/list",
                json!({
                    "threadId": thread, "cursor": cursor, "limit": 8, "sortDirection": direction
                }),
            )
            .await
        {
            Ok(page) => page,
            Err(why) => {
                tracing::debug!(
                    why,
                    "codexd: history page unavailable; live prompts remain connected"
                );
                return answered;
            }
        };
        let Some(entries) = page["data"].as_array() else {
            return answered;
        };
        answered = true;
        for entry in entries {
            if !each(&entry["item"]) {
                return true;
            }
        }
        let next = &page["nextCursor"];
        if entries.is_empty() || next.is_null() || *next == cursor {
            return true;
        }
        cursor = next.clone();
    }
}

pub(super) async fn read(rpc: &Rpc, thread: &str) -> History {
    let mut history = History::default();
    let answered = walk(rpc, thread, "asc", |item| {
        history.questions.observe(item);
        if let Some((meta, text)) = chat_item(item, item["status"] == "inProgress") {
            history
                .messages
                .push((meta, deck_model::sanitize::cut(&text, 64 * 1024)));
            if history.messages.len() > KEPT {
                history.messages.remove(0);
            }
        }
        true
    })
    .await;
    history.unavailable = !answered;
    history
}

/// The thread's latest chat, for a refill when the deck starts to read it
/// (`crate::reader`): pages from the newest item back
/// (`sortDirection:"desc"`, as `read` pages with `asc` and `thread/resume`'s
/// `initialTurnsPage` with `desc`) until [`KEPT`] messages are in hand,
/// oldest first. So opening a channel never walks a long thread from its
/// start. No questions: the thread's first read restored those, and the
/// live notifications keep them since.
pub(super) async fn tail(rpc: &Rpc, thread: &str) -> History {
    let mut newest_first = Vec::new();
    let answered = walk(rpc, thread, "desc", |item| {
        if let Some((meta, text)) = chat_item(item, item["status"] == "inProgress") {
            newest_first.push((meta, deck_model::sanitize::cut(&text, 64 * 1024)));
        }
        newest_first.len() < KEPT
    })
    .await;
    newest_first.reverse();
    History {
        questions: Questions::default(),
        messages: newest_first,
        unavailable: !answered,
    }
}

impl Model {
    /// A history read of `thread` landed. Its questions always count: cards
    /// are fed whether or not the deck reads the thread. Its chat counts
    /// only for a thread the deck reads: in place of the store's copy when
    /// the deck has just started to (a refill), else into a thread that has
    /// recorded nothing yet, never over newer streamed messages. A read
    /// that got no page at all says nothing about the chat: the store's
    /// copy stays, and a refill waits for the next read that lands (until
    /// then, what streams in is added after the copy).
    pub(super) fn hydrated(&mut self, thread: &str, history: History) -> Vec<Effect> {
        let chat = self.reads(thread) && !history.unavailable;
        let Some(t) = self.threads.get_mut(thread) else {
            return Vec::new();
        };
        t.async_questions.merge_older(history.questions);
        let mut effects = Vec::new();
        if chat && std::mem::take(&mut t.refill) {
            let messages = refilled(history.messages, &t.messages);
            t.messages.clone_from(&messages);
            t.dirty_messages.clear();
            effects.push(Effect::Replace {
                thread: thread.to_string(),
                messages,
            });
        } else if chat && t.messages.is_empty() {
            for (meta, text) in history.messages {
                self.record_message(thread, meta, text);
            }
        }
        effects.extend(self.flush_messages());
        effects.extend(self.show_async_question(thread));
        effects
    }
}

/// A refill's history with what streamed in while it was read: a message
/// streamed since is newer than the read's copy of it, and one the read
/// had not seen yet comes after it. The newest [`KEPT`].
fn refilled(
    read: Vec<(deck_model::MessageMeta, String)>,
    streamed: &[(deck_model::MessageMeta, String)],
) -> Vec<(deck_model::MessageMeta, String)> {
    let mut messages = read;
    for (meta, text) in streamed {
        match messages.iter_mut().find(|(m, _)| m.id == meta.id) {
            Some(old) => *old = (meta.clone(), text.clone()),
            None => messages.push((meta.clone(), text.clone())),
        }
    }
    let excess = messages.len().saturating_sub(KEPT);
    messages.drain(..excess);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{ChatRole, MessageMeta};

    fn message(id: &str, text: &str) -> (MessageMeta, String) {
        let meta = MessageMeta {
            summary: None,
            images: Vec::new(),
            activity: None,
            runs: None,
            plan: None,
            id: id.into(),
            role: ChatRole::Assistant,
            streaming: false,
        };
        (meta, text.into())
    }

    /// What streamed in while the refill was read wins over the read's
    /// copy, comes after it when the read had not seen it, and only the
    /// newest 64 are kept.
    #[test]
    fn a_refill_keeps_what_streamed_in_while_it_was_read() {
        let read = vec![message("a", "A"), message("b", "B, half")];
        let streamed = [message("b", "B, whole"), message("c", "C")];
        let ids_and_texts = |messages: &[(MessageMeta, String)]| {
            messages
                .iter()
                .map(|(m, t)| (m.id.clone(), t.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids_and_texts(&refilled(read, &streamed)),
            [("a", "A"), ("b", "B, whole"), ("c", "C")]
                .map(|(i, t)| (i.to_string(), t.to_string()))
        );
        let long: Vec<_> = (0..70).map(|i| message(&i.to_string(), "x")).collect();
        let kept = refilled(long, &[]);
        assert_eq!(kept.len(), KEPT);
        assert_eq!(kept[0].0.id, "6", "the oldest go");
    }

    /// A history read of a thread the deck does not read still restores its
    /// questions, which are cards, and records none of its chat. Once the
    /// deck reads it, the next history that lands replaces the store's copy
    /// whole, once; the one after it adds nothing over what is recorded.
    #[test]
    fn a_refill_replaces_once_and_an_unread_thread_still_gets_its_questions() {
        use crate::reader::Reader;
        use deck_model::AgentId;
        let mut model = Model::default();
        let thread = super::super::LoadedThread::from_thread(&json!({"id": "t1"})).unwrap();
        model.loaded(&[thread]);
        model.registered("t1", AgentId("ag_1".into()));
        model.set_reader(Reader::only(Default::default()));
        let mut questions = Questions::default();
        questions.observe(
            &json!({"id": "q1", "type": "agentMessage", "delivery": "async",
            "questions": [{"title": "Which database?", "options": ["Postgres", "SQLite"]}]}),
        );
        let effects = model.hydrated(
            "t1",
            History {
                questions,
                messages: vec![message("a", "A")],
                unavailable: false,
            },
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Card { .. })),
            "{effects:?}"
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::Message { .. } | Effect::Replace { .. })));

        assert_eq!(
            model.set_reader(Reader::only([AgentId("ag_1".into())].into())),
            ["t1"]
        );
        let read = || History {
            questions: Questions::default(),
            messages: vec![message("a", "A"), message("b", "B")],
            unavailable: false,
        };
        let effects = model.hydrated("t1", read());
        assert!(
            matches!(&effects[..], [Effect::Replace { thread, messages }] if thread == "t1" && messages.len() == 2),
            "{effects:?}"
        );
        assert!(model.hydrated("t1", read()).is_empty(), "once");
    }

    /// While the deck's first read of a thread is on its way, the delta of
    /// an item the model has not seen is not taken as the item's start (it
    /// would win over the read's copy and begin mid-sentence); an item that
    /// starts meanwhile is followed whole. A read that got no page at all
    /// replaces nothing and leaves the refill to the next read; the one
    /// that lands brings the item. Its copy may predate the delta that was
    /// let go (the server read its page before the delta was produced), so
    /// the item takes no more deltas — a later one would leave a hole — and
    /// shows the read's copy until `item/completed` brings it whole. An item
    /// the read brought that had no delta meanwhile follows its deltas: each
    /// was produced after the read.
    #[test]
    fn a_refill_is_not_cut_short_by_a_delta_or_wiped_by_a_read_that_failed() {
        use crate::reader::Reader;
        use deck_model::AgentId;
        let mut model = Model::default();
        let thread = super::super::LoadedThread::from_thread(&json!({"id": "t1"})).unwrap();
        model.loaded(&[thread]);
        model.registered("t1", AgentId("ag_1".into()));
        model.set_reader(Reader::only(Default::default()));
        assert_eq!(
            model.set_reader(Reader::only([AgentId("ag_1".into())].into())),
            ["t1"]
        );
        let delta =
            |item: &str, text: &str| json!({"threadId": "t1", "itemId": item, "delta": text});
        model.notification("item/agentMessage/delta", &delta("a1", "lo, wor"));
        model.notification(
            "item/started",
            &json!({"threadId": "t1", "item": {"id": "c1", "type": "commandExecution",
                "command": "ls", "status": "inProgress"}}),
        );
        model.notification(
            "item/commandExecution/outputDelta",
            &delta("c1", "Cargo.toml"),
        );
        let flushed = model.flush_messages();
        assert!(
            matches!(&flushed[..], [Effect::Message { message, text, .. }]
                if message.id == "c1" && text.ends_with("Cargo.toml")),
            "only the item seen from its start: {flushed:?}"
        );

        let failed = History {
            unavailable: true,
            ..History::default()
        };
        assert!(
            model.hydrated("t1", failed).is_empty(),
            "the store's copy stays"
        );
        assert!(model.threads["t1"].refill, "the next read refills");

        let mut hel = message("a1", "Hel");
        hel.0.streaming = true;
        let mut hi = message("a2", "Hi");
        hi.0.streaming = true;
        let effects = model.hydrated(
            "t1",
            History {
                messages: vec![message("u1", "Say hello"), hel, hi],
                ..History::default()
            },
        );
        let [Effect::Replace { messages, .. }] = &effects[..] else {
            panic!("one replacement: {effects:?}");
        };
        let ids: Vec<&str> = messages.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, ["u1", "a1", "a2", "c1"]);
        assert_eq!(messages[1].1, "Hel", "the read's copy, from its start");
        // "lo, wor" went after the read's page: a1 takes no more deltas.
        model.notification("item/agentMessage/delta", &delta("a1", "ld"));
        let flushed = model.flush_messages();
        assert!(
            !flushed
                .iter()
                .any(|e| matches!(e, Effect::Message { message, .. } if message.id == "a1")),
            "no hole in a1: {flushed:?}"
        );
        model.notification(
            "item/completed",
            &json!({"threadId": "t1", "item": {"id": "a1", "type": "agentMessage",
                "text": "Hello, world"}}),
        );
        assert!(
            matches!(&model.flush_messages()[..], [Effect::Message { message, text, .. }]
                if message.id == "a1" && text == "Hello, world"),
            "the item, whole"
        );
        // a2 had no delta while the read was on its way.
        model.notification("item/agentMessage/delta", &delta("a2", " there"));
        assert!(
            matches!(&model.flush_messages()[..], [Effect::Message { message, text, .. }]
                if message.id == "a2" && text == "Hi there"),
        );
    }
}
