//! The GUI's one event bus (§8.1, copied from openmicrokbd's `events.rs`):
//! an `async_channel::unbounded` channel, `post` on the writing side and an
//! `await` on the reading side.
//!
//! Unbounded and `try_send` on purpose. `post` is called from a tokio worker
//! (the `ApiClient` connection task) and from gpui's foreground executor, and
//! neither is allowed to block the UI thread to deliver an event; an
//! `async_channel` sender is the one primitive that sends from either without
//! knowing which executor it is on, and an unbounded one never makes the
//! caller wait for a reader. The cost is that a wedged reader grows the queue,
//! which is the same trade openmicrokbd's HID thread already makes.
use deck_model::{Event, Snapshot};
use std::sync::OnceLock;

/// What the `ApiClient` saw on the socket (§8.1). `Connected` carries the
/// whole snapshot because that is what `session.hello{snapshot:true}`
/// answers with, and the mirror is only ever replaced wholesale.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ApiEvent {
    Connected {
        snapshot: Snapshot,
    },
    /// A `state.get` reply: the mirror is replaced wholesale, and **nothing
    /// else** is claimed. It is not a `Connected` because a reply that was
    /// already in flight when the socket dropped would otherwise tell the
    /// mirror the service is up — hiding the yellow bar, re-enabling every
    /// button and toasting a reconnection that never happened.
    Snapshot {
        snapshot: Snapshot,
    },
    Event(Event),
    Disconnected,
    /// The daemon says we lagged, or the client's own event ring did:
    /// `state.get` is the only way back to a true mirror.
    Resync,
}

impl ApiEvent {
    /// A `state.get` reply (`{rev, snapshot}`, §4.5) as the event that
    /// installs it — what closes the loop a [`crate::HostEffect::StateGet`]
    /// opened. It is an [`ApiEvent::Snapshot`] and not a `Connected`: the
    /// mirror is replaced wholesale either way, but a resync has no
    /// connection to announce and must not be able to announce one.
    ///
    /// Takes the reply's `snapshot`, or the value itself when it is already
    /// one. `None` when it is not a snapshot at all (an error, or a
    /// `-32013 degraded`), which the caller shows and retries.
    pub fn snapshot_reply(result: &serde_json::Value) -> Option<ApiEvent> {
        let snapshot = result.get("snapshot").or(Some(result))?;
        serde_json::from_value::<Snapshot>(snapshot.clone())
            .ok()
            .map(|snapshot| ApiEvent::Snapshot { snapshot })
    }
}

/// Everything that reaches the GUI from outside the UI thread: the service,
/// and the menu bar. Unlike openmicrokbd this app owns no HID link, no audio
/// and no hotkeys — `deckd` owns all three and speaks through `ApiEvent` —
/// but it does own the menu bar, and `muda` calls its handler from the
/// menu's own thread.
///
/// The menu goes on this bus rather than into a poll for the reason the bus
/// exists: `try_send` on an unbounded `async_channel::Sender` needs no
/// executor and blocks nobody, so a click can be delivered from wherever
/// AppKit runs the handler without the UI thread waiting on it.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum AppEvent {
    Api(ApiEvent),
    /// An item of the status item's menu was clicked ([`crate::menu`]).
    Menu(crate::menu::MenuCommand),
}

/// The writing half. `Clone` is cheap: every producer holds one.
#[derive(Clone, Debug)]
pub struct EventSender(async_channel::Sender<AppEvent>);

/// The reading half, awaited by the GUI's one bus task.
#[derive(Clone, Debug)]
pub struct EventReceiver(async_channel::Receiver<AppEvent>);

/// A fresh bus. The app makes one at startup; a test makes one per test.
pub fn bus() -> (EventSender, EventReceiver) {
    let (tx, rx) = async_channel::unbounded();
    (EventSender(tx), EventReceiver(rx))
}

impl EventSender {
    /// Post without blocking and without an executor. `false` means the
    /// receiver is gone (the app is quitting), which is never an error here:
    /// there is no one left who wanted the event.
    pub fn post(&self, event: AppEvent) -> bool {
        self.0.try_send(event).is_ok()
    }

    /// Events posted and not yet read.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

impl EventReceiver {
    /// The next event; `None` once every sender is gone and the queue is dry.
    pub async fn recv(&self) -> Option<AppEvent> {
        self.0.recv().await.ok()
    }

    /// The next event if one is already queued. What a test drives the
    /// reducer from, and what a gpui timer would drain with.
    pub fn try_recv(&self) -> Option<AppEvent> {
        self.0.try_recv().ok()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The app-wide bus, so `events::post` reads like openmicrokbd's does from
/// anywhere in the gpui crate without threading a sender through every view.
/// Tests use [`bus`] instead: a process-global is installed once and a
/// hermetic test must not share one with the next.
static GLOBAL: OnceLock<EventSender> = OnceLock::new();

/// Install the app's bus. `false` if one was already installed (the second
/// caller's sender is dropped and the first bus stays the app's).
pub fn install(sender: EventSender) -> bool {
    GLOBAL.set(sender).is_ok()
}

pub fn installed() -> bool {
    GLOBAL.get().is_some()
}

/// Post to the app-wide bus. `false` when nothing was installed (a unit test
/// of a view, say) or when the receiver is gone.
pub fn post(event: AppEvent) -> bool {
    GLOBAL.get().is_some_and(|tx| tx.post(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_carries_events_in_order_and_needs_no_executor() {
        let (tx, rx) = bus();
        assert!(tx.is_empty() && rx.is_empty());
        assert!(tx.post(AppEvent::Api(ApiEvent::Disconnected)));
        assert!(tx.post(AppEvent::Api(ApiEvent::Resync)));
        assert_eq!(tx.len(), 2);
        assert_eq!(rx.len(), 2);
        assert!(matches!(
            rx.try_recv(),
            Some(AppEvent::Api(ApiEvent::Disconnected))
        ));
        assert!(matches!(
            rx.try_recv(),
            Some(AppEvent::Api(ApiEvent::Resync))
        ));
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn a_state_get_reply_becomes_the_event_that_installs_its_snapshot() {
        let reply = serde_json::json!({
            "rev": 1202,
            "snapshot": Snapshot { rev: 1202, ..Snapshot::default() },
        });
        let Some(ApiEvent::Snapshot { snapshot }) = ApiEvent::snapshot_reply(&reply) else {
            panic!("a state.get reply carries a snapshot");
        };
        assert_eq!(snapshot.rev, 1202);
        // An error reply, or one from a degraded daemon, is not a snapshot.
        assert!(ApiEvent::snapshot_reply(&serde_json::json!({"rev": 7})).is_none());
        assert!(ApiEvent::snapshot_reply(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn a_post_with_no_receiver_left_is_false_and_not_a_panic() {
        let (tx, rx) = bus();
        drop(rx);
        assert!(tx.is_closed());
        assert!(!tx.post(AppEvent::Api(ApiEvent::Disconnected)));
    }
}
