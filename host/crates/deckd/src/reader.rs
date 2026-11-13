//! The lazy reader feed: a session's history is read only while the deck
//! reads it (decision 2, "feed them lazily").
//!
//! History, tool rows, images and plans are collected only for the sessions
//! the deck reads: the channel open on it, the slot it last showed, and
//! every session holding a card it shows, a Ready card included
//! ([`deck_model::Store::reader`]). A session that leaves that set is still
//! fed for [`LINGER`], so turning the knob past it and back costs no
//! second read. What stays eager, read or not: a session's state, title,
//! task and subagents, its cards, and the reply each turn ends with, which
//! the adapters store before they mark the session ready, so the deck's
//! ready line never waits on a reader.
//!
//! The core decides ([`Readers`], fed the store's reader set and the
//! core's clock on every tick and every line from the deck) and publishes a
//! [`Reader`] through `adapters::rt::Adapters`, beside the dormancy gate
//! (`crate::dormancy`). Those who collect a session's history read it:
//!
//! - the Claude hook door posts a tool row, decoding its images, only for a
//!   session that is read, and starts its transcript watcher only then
//!   (`crate::chat`); a watcher whose session is no longer read stops at its
//!   next look, and [`follow`] starts one for each Claude session the deck
//!   opens, from the transcript the session's hooks named;
//! - the Codex app-server client records a thread's chat only while it is
//!   read, and reads the thread's latest items again when it starts to be
//!   (`crate::codexd`).
//!
//! A session's first read after it was not read replaces the history the
//! store kept (`Command::ChatReplace`), so messages missed meanwhile never
//! land after newer ones; the kept history stays until then. The workflow
//! runs only the Claude watcher follows go when the deck stops reading the
//! session (`Command::ReaderLeft`): the roster counts its subagents alone.
//!
//! A daemon without dormancy (`--no-device`) has no lazy feed either: its
//! reader is every session, which is how every door test, the gate's `api`
//! step and the companion's test service run.
use crate::api::Api;
use deck_model::AgentId;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long a session the deck stopped reading is still fed.
pub const LINGER: Duration = Duration::from_secs(60);

/// The sessions fed in full: every one, or the deck's reader set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reader(Option<Arc<BTreeSet<AgentId>>>);

impl Reader {
    /// Every session: a daemon with no lazy feed.
    #[must_use]
    pub fn all() -> Self {
        Reader(None)
    }

    /// Only these sessions.
    #[must_use]
    pub fn only(agents: BTreeSet<AgentId>) -> Self {
        Reader(Some(Arc::new(agents)))
    }

    /// Whether this session's history is collected.
    #[must_use]
    pub fn reads(&self, agent: &AgentId) -> bool {
        self.0.as_ref().is_none_or(|agents| agents.contains(agent))
    }

    /// Whether the feed is lazy at all.
    #[must_use]
    pub fn lazy(&self) -> bool {
        self.0.is_some()
    }

    /// The sessions read, when the feed is lazy.
    #[must_use]
    pub fn agents(&self) -> Option<&BTreeSet<AgentId>> {
        self.0.as_deref()
    }
}

/// The core's half: the store's reader set, with its linger.
#[derive(Debug, Default)]
pub struct Readers {
    /// Read now, or lingering.
    fed: BTreeSet<AgentId>,
    /// When each lingering session stopped being read.
    left: BTreeMap<AgentId, Instant>,
}

impl Readers {
    /// One look at `reading`, the store's reader set at `now`: the sessions
    /// to feed (`reading` and the ones still lingering), and the ones whose
    /// linger ran out just now. `exists` is whether a session is still on
    /// the roster; one that is gone stops at once and is not reported.
    pub fn observe(
        &mut self,
        reading: BTreeSet<AgentId>,
        now: Instant,
        exists: impl Fn(&AgentId) -> bool,
    ) -> (BTreeSet<AgentId>, Vec<AgentId>) {
        for agent in self.fed.difference(&reading) {
            self.left.entry(agent.clone()).or_insert(now);
        }
        for agent in &reading {
            self.left.remove(agent);
        }
        let mut stopped = Vec::new();
        self.left.retain(|agent, since| {
            if !exists(agent) {
                return false;
            }
            let lingering = now.saturating_duration_since(*since) < LINGER;
            if !lingering {
                stopped.push(agent.clone());
            }
            lingering
        });
        self.fed = reading
            .into_iter()
            .chain(self.left.keys().cloned())
            .collect();
        (self.fed.clone(), stopped)
    }
}

/// The reader's next change. A reader whose sender is gone never changes
/// again: the sender lives in `rt::Adapters`, which outlives every
/// follower, so that is only ever the daemon on its way out.
pub async fn changed(reader: &mut watch::Receiver<Reader>) {
    if reader.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The Claude half of the feed, until `stop`: for the reader set as it
/// stands and each time it changes, a transcript watcher for every Claude
/// session in it that has none, from the transcript its hooks named, else
/// the one its key and working directory name (`crate::chat`). The hooks
/// start watchers too, for a session that is read; this is what starts one
/// the moment the deck opens a session that is quiet.
pub async fn follow(api: Arc<Api>, mut stop: watch::Receiver<bool>) {
    let mut reader = api.core.adapters().watch_reader();
    loop {
        let now = reader.borrow_and_update().clone();
        crate::chat::open(&api, &now);
        tokio::select! {
            _ = stop.changed() => return,
            () = changed(&mut reader) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentId {
        AgentId(id.into())
    }

    fn set(ids: &[&str]) -> BTreeSet<AgentId> {
        ids.iter().map(|id| agent(id)).collect()
    }

    #[test]
    fn every_session_is_read_until_a_core_with_a_deck_says_otherwise() {
        let all = Reader::default();
        assert_eq!(all, Reader::all());
        assert!(!all.lazy() && all.reads(&agent("ag_1")) && all.agents().is_none());
        let some = Reader::only(set(&["ag_1"]));
        assert!(some.lazy() && some.reads(&agent("ag_1")) && !some.reads(&agent("ag_2")));
        assert_eq!(
            some,
            Reader::only(set(&["ag_1"])),
            "compared by the sessions"
        );
    }

    /// A session the deck stops reading is fed for the linger, one that
    /// comes back inside it never stops, and one removed stops at once.
    #[test]
    fn a_session_left_lingers_for_a_minute_then_stops_once() {
        let t0 = Instant::now();
        let mut readers = Readers::default();
        let on_roster = |_: &AgentId| true;
        let (fed, stopped) = readers.observe(set(&["a", "b"]), t0, on_roster);
        assert_eq!((fed, stopped), (set(&["a", "b"]), vec![]));
        // B leaves: still fed for 59 s, stopped at 60.
        let (fed, stopped) = readers.observe(set(&["a"]), t0, on_roster);
        assert_eq!((fed, stopped), (set(&["a", "b"]), vec![]));
        let almost = t0 + LINGER - Duration::from_secs(1);
        assert_eq!(
            readers.observe(set(&["a"]), almost, on_roster).0,
            set(&["a", "b"])
        );
        let (fed, stopped) = readers.observe(set(&["a"]), t0 + LINGER, on_roster);
        assert_eq!((fed, stopped), (set(&["a"]), vec![agent("b")]));
        let (_, stopped) = readers.observe(set(&["a"]), t0 + LINGER * 2, on_roster);
        assert!(stopped.is_empty(), "reported once");
        // A leaves and is back inside the minute: it never stopped, and its
        // next leave lingers a whole minute again.
        let t1 = t0 + LINGER * 3;
        readers.observe(set(&[]), t1, on_roster);
        readers.observe(set(&["a"]), t1 + Duration::from_secs(30), on_roster);
        let t2 = t1 + Duration::from_secs(50);
        readers.observe(set(&[]), t2, on_roster);
        let late = t2 + Duration::from_secs(59);
        let (fed, stopped) = readers.observe(set(&[]), late, on_roster);
        assert_eq!(
            (fed, stopped),
            (set(&["a"]), vec![]),
            "a whole minute again"
        );
        // Removed from the roster inside its linger: it stops at once, and
        // is not reported, since nothing is left of it to stop.
        let gone = |id: &AgentId| id.0 != "a";
        let (fed, stopped) = readers.observe(set(&[]), late, gone);
        assert_eq!((fed, stopped), (set(&[]), vec![]));
    }
}
