//! What puts sessions on the deck, and the owner's switch for each:
//! `sources.list`, the `[sources]` table and `[codex] daemon` (PLAN §2
//! "Pause", §3.2, §3.3).
//!
//! A **source** is where a session comes from, and the session that owns
//! an agent says which:
//!
//! - **Claude Code**: the Claude adapter's internal session, which owns
//!   every agent the hooks and the roster seed register.
//! - **Codex**: the Codex adapter's internal session, shared by the
//!   app-server client (`crate::codexd`) and the hooks fallback.
//! - **A connected app**: a session a client opened on a door, by the
//!   `client.name` it gave, or by its principal when it gave none, so that
//!   an unnamed client is one app and not one per connection.
//! - **Other**: a Gemini or MCP adapter, which the reply counts and never
//!   names.
//!
//! The daemon's own internal sessions (the demo, the test agent) are never
//! a source: what they put on the deck is the daemon's own. The demo's
//! example agent speaks as the Claude adapter and is still not Claude
//! Code's, which is why a session is classified by how it was opened and
//! never an agent by its principal. Claude Channels is a row but not a
//! source of sessions: its helpers carry orders to Claude sessions the hooks
//! registered, so its counts are theirs.
//!
//! **Pausing a source never refuses it.** Its sessions are muted
//! (`Command::Mute`): kept, served and published, but off the deck, with a
//! question parked for one handed back to its tool and nothing of it
//! spoken. Its hooks are answered `{}` (`config::Sources::hooks_answer`)
//! and the roster seed waits (`liveness::seeds`). The muted set is worked
//! out whole, here, whenever the switches or the sessions change. A session
//! that closed stays known while an agent it left behind (orphaned, in its
//! grace) still names it: a paused app's agents must not come back on the
//! deck because its connection dropped.
//!
//! The reply is counts, times and the names of programs. No title, prompt
//! or reply can be in it, which is why `sources.list` is an `observe`
//! method.
use crate::config::{self, Config};
use crate::core::handle::SessionKind;
use crate::core::sessions::{SessionEntry, Sessions};
use deck_model::{AdapterKind, Agent, Principal, Request, Scope, SessionId, Snapshot, Ts};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Where a session comes from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Claude Code: the Claude adapter's internal session.
    Claude,
    /// Codex: the Codex adapter's internal session.
    Codex,
    /// A Gemini or MCP adapter: counted, never named.
    Other,
    /// A connected app, by the name it is listed and paused under.
    App(String),
}

impl Source {
    /// The source `session` belongs to, by how it was opened; `None` for
    /// the daemon's own internal sessions, which are never one.
    #[must_use]
    pub fn of(session: &SessionEntry) -> Option<Source> {
        match (&session.kind, &session.principal) {
            (SessionKind::Internal, Principal::Adapter(AdapterKind::Claude)) => {
                Some(Source::Claude)
            }
            (SessionKind::Internal, Principal::Adapter(AdapterKind::Codex)) => Some(Source::Codex),
            (SessionKind::Internal, Principal::Adapter(AdapterKind::Gemini | AdapterKind::Mcp)) => {
                Some(Source::Other)
            }
            // The demo and the test agent (`adapter:virtual`).
            (SessionKind::Internal, _) => None,
            (_, principal) => Some(Source::App(app_name(&session.client.name, principal))),
        }
    }

    /// The word the delivery record (`crate::deliveries`) files this
    /// source's cards under: a connected app is `app`, never its name.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Other => "other",
            Source::App(_) => "app",
        }
    }

    /// Whether the owner's switches pause it.
    #[must_use]
    pub fn paused(&self, switches: &config::Sources) -> bool {
        switches.paused
            || match self {
                Source::Claude => !switches.claude,
                Source::App(name) => switches.app_paused(name),
                // The Codex switch (`[codex] daemon`) ends the app-server
                // connection instead, and its documented "off leaves Codex
                // to the hooks alone" still holds.
                Source::Codex | Source::Other => false,
            }
    }
}

/// Whether a reducer request is something its source did, the only kind
/// that stamps `last_event_at`. Two requests are bookkeeping and say
/// nothing about activity: `agent.heartbeat` renews a lease (an idle client
/// that holds agents sends one well inside every 30 s, so a stamp from it
/// would read "just now" for as long as the client is connected), and
/// `agent.claim` takes agents back after a reconnect.
#[must_use]
pub fn activity(req: &Request) -> bool {
    !matches!(
        req,
        Request::AgentHeartbeat { .. } | Request::AgentClaim { .. }
    )
}

/// The name a connected app is listed and paused under: its `client.name`,
/// or its principal (`uid:501`, `token:<name>`) when it gave none — a hello
/// without one, or a call on the HTTP door that names none.
#[must_use]
pub fn app_name(client: &str, principal: &Principal) -> String {
    let client = client.trim();
    if client.is_empty() || client == crate::api::http::DEFAULT_CLIENT {
        principal.to_string()
    } else {
        client.to_string()
    }
}

/// What a session is, as far as the sources go.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    Source(Source),
    /// One of the daemon's own internal sessions.
    Daemon,
    /// Neither open nor remembered (no session at all, which is how the
    /// Usage face's feed calls): it belongs to no row.
    Unknown,
}

/// What was seen of one source.
#[derive(Debug, Default)]
struct Seen {
    /// When it last did something: a reducer call (`Core::api_call`) that
    /// is not bookkeeping ([`activity`]) or a held card, from any of its
    /// sessions.
    last_event: Option<Ts>,
    /// It drew a deck face with `device.ws.set`, as the Omarchy bridge does.
    face: bool,
}

/// What `sources.list` reads from beyond the core's own tables.
pub struct Around<'a> {
    /// The Claude Channels door's helpers.
    pub channels: &'a crate::claude_channel::Hub,
    /// The owner's Claude Channels switch, resolved
    /// (`integrations::claude::channels_on`).
    pub channels_on: bool,
    /// Whether `crate::codexd` is connected to Codex's daemon now.
    pub codex_connected: bool,
}

/// What the core keeps about the sources between calls.
#[derive(Debug, Default)]
pub struct Ledger {
    /// The source of every session known to be one: recorded once it is
    /// seen open, kept after it closes while an agent still names it.
    sessions: BTreeMap<SessionId, Source>,
    /// What each source did. An app's is forgotten with its last session;
    /// Claude's, Codex's and Other's are kept for the life of the daemon.
    seen: BTreeMap<Source, Seen>,
    /// The muted set last applied (`Command::Mute`).
    muted: BTreeSet<SessionId>,
}

impl Ledger {
    /// What `id` is: its recorded source, else the open session's, which is
    /// recorded now.
    fn origin(&mut self, sessions: &Sessions, id: SessionId) -> Origin {
        if let Some(source) = self.sessions.get(&id) {
            return Origin::Source(source.clone());
        }
        let Some(entry) = sessions.get(id) else {
            return Origin::Unknown;
        };
        match Source::of(entry) {
            Some(source) => {
                self.sessions.insert(id, source.clone());
                Origin::Source(source)
            }
            None => Origin::Daemon,
        }
    }

    /// Record `id` while it is still open, so a session about to close
    /// keeps its source for the agents it leaves behind.
    pub fn remember(&mut self, sessions: &Sessions, id: SessionId) {
        self.origin(sessions, id);
    }

    /// What the delivery record (`crate::deliveries`) calls the source of
    /// session `id`, read without recording anything: its source's
    /// [`Source::word`], `daemon` for one of the daemon's own sessions, and
    /// `unknown` for a session neither open nor remembered. A session that
    /// closed is still its source's while an agent it left names it.
    #[must_use]
    pub fn word(&self, sessions: &Sessions, id: SessionId) -> &'static str {
        if let Some(source) = self.sessions.get(&id) {
            return source.word();
        }
        match sessions.get(id).map(Source::of) {
            Some(Some(source)) => source.word(),
            Some(None) => "daemon",
            None => "unknown",
        }
    }

    /// `session` did something at `at` (a call that passes [`activity`], or
    /// a held card). The daemon's own sessions, and calls made under no
    /// session at all, are nobody's.
    pub fn heard(&mut self, sessions: &Sessions, session: SessionId, at: Ts) {
        if let Origin::Source(source) = self.origin(sessions, session) {
            self.seen.entry(source).or_default().last_event = Some(at);
        }
    }

    /// `session` drew a face (`device.ws.set`).
    pub fn drew_face(&mut self, sessions: &Sessions, session: SessionId) {
        if let Origin::Source(source) = self.origin(sessions, session) {
            self.seen.entry(source).or_default().face = true;
        }
    }

    /// The sessions these switches mute, every session of a paused source,
    /// when they are not the set last applied.
    pub fn mute(
        &mut self,
        sessions: &Sessions,
        snapshot: &Snapshot,
        switches: &config::Sources,
    ) -> Option<BTreeSet<SessionId>> {
        self.refresh(sessions, snapshot);
        let muted: BTreeSet<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, source)| source.paused(switches))
            .map(|(id, _)| *id)
            .collect();
        if muted == self.muted {
            return None;
        }
        self.muted.clone_from(&muted);
        Some(muted)
    }

    /// Record every open session; then forget the closed ones no agent
    /// names any more, and what was seen of an app none of whose sessions
    /// is known.
    fn refresh(&mut self, sessions: &Sessions, snapshot: &Snapshot) {
        for (id, _) in sessions.iter() {
            self.origin(sessions, *id);
        }
        let named: BTreeSet<SessionId> = snapshot
            .agents
            .values()
            .map(|agent| agent.owner.session)
            .collect();
        self.sessions
            .retain(|id, _| sessions.contains(*id) || named.contains(id));
        let known: BTreeSet<&Source> = self.sessions.values().collect();
        self.seen
            .retain(|source, _| !matches!(source, Source::App(_)) || known.contains(source));
    }

    /// `sources.list`: `{paused, sources, other}`, the three tools first and
    /// then the connected apps by name.
    pub fn list(
        &mut self,
        sessions: &Sessions,
        snapshot: &Snapshot,
        config: &Config,
        around: &Around<'_>,
    ) -> serde_json::Value {
        self.refresh(sessions, snapshot);
        let mut tally: BTreeMap<Source, Tally> = BTreeMap::new();
        let mut other = 0;
        let mut channels_on_deck = 0;
        for agent in snapshot.agents.values() {
            match self.origin(sessions, agent.owner.session) {
                Origin::Daemon => {}
                Origin::Unknown | Origin::Source(Source::Other) => other += 1,
                Origin::Source(source) => {
                    if source == Source::Claude
                        && agent.slot.is_some()
                        && around.channels.ready(agent)
                    {
                        channels_on_deck += 1;
                    }
                    tally.entry(source).or_default().add(agent);
                }
            }
        }
        let mut rows = vec![
            self.claude_row(config, tally.get(&Source::Claude)),
            channels_row(around, channels_on_deck),
            self.codex_row(config, around, tally.get(&Source::Codex)),
        ];
        rows.extend(self.app_rows(sessions, config, &tally));
        serde_json::json!({
            "paused": config.sources.paused,
            "sources": rows,
            "other": other,
        })
    }

    fn last_event(&self, source: &Source) -> Option<Ts> {
        self.seen.get(source).and_then(|seen| seen.last_event)
    }

    fn claude_row(&self, config: &Config, tally: Option<&Tally>) -> Row {
        let (on_deck, sessions) = Tally::counts(tally);
        Row {
            id: "claude".into(),
            kind: "tool",
            name: "Claude Code".into(),
            enabled: config.sources.claude,
            state: None,
            on_deck,
            sessions,
            unit: "sessions",
            last_event_at: self.last_event(&Source::Claude),
            // A hook-registered session is brought forward by raising the
            // app it runs in, on a press of its `Open app` card.
            handoff: Some("app"),
            hooks: Some(crate::integrations::claude::managed_hooks(
                config.hooks.claude.tool_titles,
            )),
            stale: None,
            does: None,
        }
    }

    fn codex_row(&self, config: &Config, around: &Around<'_>, tally: Option<&Tally>) -> Row {
        let (on_deck, sessions) = Tally::counts(tally);
        let state = if !config.codex.daemon {
            "off"
        } else if around.codex_connected {
            "connected"
        } else {
            "waiting"
        };
        Row {
            id: "codex".into(),
            kind: "tool",
            name: "Codex CLI".into(),
            enabled: config.codex.daemon,
            state: Some(state),
            on_deck,
            sessions,
            unit: "threads",
            last_event_at: self.last_event(&Source::Codex),
            // An app-server thread has no terminal this daemon knows of.
            handoff: Some("none"),
            hooks: None,
            stale: None,
            does: None,
        }
    }

    /// One row per connected app that puts something on the deck: it has
    /// agents, one of its open sessions asked for `agent` by name, or it
    /// drew a face. A client that only reads and sets (the companion app
    /// itself, a `deckd ctl` call) is no source and is not listed.
    fn app_rows(
        &self,
        sessions: &Sessions,
        config: &Config,
        tally: &BTreeMap<Source, Tally>,
    ) -> Vec<Row> {
        // The apps with a session open now, and whether one of those may
        // register agents.
        let mut open: BTreeMap<&str, bool> = BTreeMap::new();
        for (id, entry) in sessions.iter() {
            if let Some(Source::App(name)) = self.sessions.get(id) {
                *open.entry(name.as_str()).or_default() |= entry.scopes.contains(&Scope::Agent);
            }
        }
        let names: BTreeSet<&str> = tally
            .keys()
            .filter_map(|source| match source {
                Source::App(name) => Some(name.as_str()),
                _ => None,
            })
            .chain(open.keys().copied())
            .collect();
        let none = Tally::default();
        names
            .into_iter()
            .filter_map(|name| {
                let source = Source::App(name.to_string());
                let tally = tally.get(&source).unwrap_or(&none);
                let face = self.seen.get(&source).is_some_and(|seen| seen.face);
                let agents = tally.sessions > 0 || open.get(name).copied().unwrap_or(false);
                if !agents && !face {
                    return None;
                }
                let does = [("sessions", agents), ("voice", tally.voice), ("face", face)]
                    .into_iter()
                    .filter_map(|(what, done)| done.then_some(what))
                    .collect();
                Some(Row {
                    id: format!("app:{name}"),
                    kind: "app",
                    name: name.to_string(),
                    enabled: !config.sources.app_paused(name),
                    state: Some(if open.contains_key(name) {
                        "connected"
                    } else {
                        "waiting"
                    }),
                    on_deck: tally.on_deck,
                    sessions: tally.sessions,
                    unit: if tally.chats { "chats" } else { "sessions" },
                    last_event_at: self.last_event(&source),
                    // An app brings its own sessions forward.
                    handoff: Some("self"),
                    hooks: None,
                    stale: None,
                    does: Some(does),
                })
            })
            .collect()
    }
}

fn channels_row(around: &Around<'_>, on_deck: usize) -> Row {
    let ready = around.channels.ready_count();
    Row {
        id: "claude_channels".into(),
        kind: "channel",
        name: "Claude Channels".into(),
        // `[sources] claude_channels`: off, the door is closed.
        enabled: around.channels_on,
        state: Some(if !around.channels_on {
            "off"
        } else if ready > 0 {
            "connected"
        } else {
            "waiting"
        }),
        on_deck,
        sessions: ready,
        unit: "sessions",
        last_event_at: None,
        handoff: None,
        hooks: None,
        stale: Some(around.channels.stale_count()),
        does: None,
    }
}

/// One source's agents.
#[derive(Debug, Default)]
struct Tally {
    sessions: usize,
    on_deck: usize,
    /// One of them is a native companion's chat (kind `conol`, or a
    /// companion profile).
    chats: bool,
    /// One of them has its owner transcribe what is said to it.
    voice: bool,
}

impl Tally {
    fn add(&mut self, agent: &Agent) {
        self.sessions += 1;
        self.on_deck += usize::from(agent.slot.is_some());
        self.chats |= agent.native_companion();
        self.voice |= !agent.caps.transcribe.is_none();
    }

    /// `(on_deck, sessions)`, nought for a source with no agents.
    fn counts(tally: Option<&Tally>) -> (usize, usize) {
        tally.map_or((0, 0), |tally| (tally.on_deck, tally.sessions))
    }
}

/// One row of `sources.list`. Every key is always there, `null` where it
/// does not apply, so a client reads one shape for every kind of source.
#[derive(Debug, Serialize)]
struct Row {
    /// `claude`, `claude_channels`, `codex`, or `app:<name>`.
    id: String,
    /// `tool`, `channel` or `app`.
    kind: &'static str,
    name: String,
    /// The source's own switch; the global pause is `paused` beside the
    /// list.
    enabled: bool,
    /// `connected`, `waiting` or `off`; `null` for Claude Code, whose state
    /// is its installer's (`integrations.status`).
    state: Option<&'static str>,
    /// Its agents that hold a deck slot.
    on_deck: usize,
    /// All its agents.
    sessions: usize,
    /// What the counts count: `sessions`, `threads` (Codex) or `chats`.
    unit: &'static str,
    last_event_at: Option<Ts>,
    /// What a press on the deck does to bring a session forward: `app`
    /// (raises the app the session runs in), `none` (nothing yet), or
    /// `self` (the app does it itself). `terminal` (focused its Ghostty
    /// tab) is what a 0.13 service said and this one never does.
    handoff: Option<&'static str>,
    /// The hook events the plugin installs (Claude Code only).
    hooks: Option<usize>,
    /// Helpers refused for running another `deckd` than this one, such as
    /// the one before an update (Claude Channels only).
    stale: Option<usize>,
    /// What a connected app does: `sessions`, `voice`, `face`.
    does: Option<Vec<&'static str>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::handle::{ClientInfo, CoreMsg, OpenSession};
    use crate::core::testkit::{self, detached_core};
    use crate::core::Core;
    use deck_model::{AgentId, DeviceOp, HostLine, Key, Kind, Request, Slot, TranscribeCap};
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Instant;

    /// Open a session the way a door does (`CoreMsg::OpenSession`), so the
    /// core works its pauses out on the way.
    fn open(
        c: &mut Core,
        kind: SessionKind,
        principal: Principal,
        client: &str,
        scopes: &[Scope],
        now: Instant,
    ) -> SessionId {
        let (reply, mut rx) = tokio::sync::oneshot::channel();
        let open = OpenSession {
            kind,
            principal,
            scopes: scopes.to_vec(),
            trusted: false,
            client: ClientInfo {
                name: client.into(),
                version: "1".into(),
            },
        };
        c.on_msg(CoreMsg::OpenSession { open, reply }, now);
        rx.try_recv()
            .expect("answered in the step")
            .expect("opened")
    }

    /// An adapter's internal session.
    fn adapter(c: &mut Core, kind: AdapterKind, now: Instant) -> SessionId {
        let scopes = [Scope::Agent, Scope::Observe];
        let client = format!("adapter:{kind:?}");
        open(
            c,
            SessionKind::Internal,
            Principal::Adapter(kind),
            &client,
            &scopes,
            now,
        )
    }

    /// A connected app's connection, as the UDS door opens it.
    fn app(c: &mut Core, name: &str, scopes: &[Scope], now: Instant) -> SessionId {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let live = SessionKind::Live { tx };
        open(c, live, Principal::Uid(501), name, scopes, now)
    }

    /// Register an agent on `session` as its owner.
    fn agent(c: &mut Core, session: SessionId, name: &str, kind: Kind, now: Instant) -> AgentId {
        let principal = c.sessions.get(session).expect("open").principal.clone();
        let mut register = testkit::register(name, Some(&format!("k:{name}")));
        register.kind = kind;
        if kind == Kind::Conol {
            register.caps.transcribe = TranscribeCap::Push;
        }
        let result = c
            .api_call(
                principal,
                session,
                false,
                Request::AgentRegister(register),
                now,
            )
            .expect("registered");
        serde_json::from_value(result["agent"].clone()).unwrap()
    }

    /// `sources.list` as an `observe` caller with `{}`.
    fn list(c: &mut Core, now: Instant) -> Value {
        c.call(
            testkit::ctx(99, &[Scope::Observe]),
            "sources.list",
            json!({}),
            now,
        )
        .result
        .expect("sources.list answers")
    }

    fn row<'a>(reply: &'a Value, id: &str) -> &'a Value {
        reply["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("no {id} row in {reply}"))
    }

    fn ids(reply: &Value) -> Vec<&str> {
        let rows = reply["sources"].as_array().unwrap();
        rows.iter().map(|row| row["id"].as_str().unwrap()).collect()
    }

    fn reconfigure(c: &mut Core, now: Instant, edit: impl FnOnce(&mut Config)) {
        let mut config = c.config().clone();
        edit(&mut config);
        c.on_msg(CoreMsg::Config(Arc::new(config)), now);
    }

    fn slot(c: &Core, id: &AgentId) -> Option<Slot> {
        c.store.snapshot().agents[id].slot
    }

    /// A freshly started daemon lists the three tools, nothing counted, and
    /// no app: Claude Code and Codex on, Claude Channels off until the owner
    /// opts in (a new install's plugin has no helper).
    #[test]
    fn an_empty_daemon_lists_the_three_tools_and_nothing_else() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-empty");
        let reply = list(&mut c, Instant::now());
        assert_eq!(reply["paused"], false);
        assert_eq!(reply["other"], 0);
        assert_eq!(ids(&reply), ["claude", "claude_channels", "codex"]);
        let claude = row(&reply, "claude");
        assert_eq!(
            *claude,
            json!({
                "id": "claude", "kind": "tool", "name": "Claude Code", "enabled": true,
                "state": null, "on_deck": 0, "sessions": 0, "unit": "sessions",
                "last_event_at": null, "handoff": "app", "hooks": 13, "stale": null,
                "does": null,
            })
        );
        let channels = row(&reply, "claude_channels");
        assert_eq!(
            (&channels["kind"], &channels["state"]),
            (&json!("channel"), &json!("off"))
        );
        assert_eq!(
            (&channels["enabled"], &channels["stale"]),
            (&json!(false), &json!(0))
        );
        let codex = row(&reply, "codex");
        assert_eq!(
            (&codex["name"], &codex["unit"]),
            (&json!("Codex CLI"), &json!("threads"))
        );
        assert_eq!(
            (&codex["state"], &codex["handoff"]),
            (&json!("waiting"), &json!("none"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The Claude adapter's agents are Claude Code's and Codex's are
    /// Codex's, each stamped when it called in; a Gemini adapter's are
    /// counted under `other` only.
    #[test]
    fn a_claude_session_is_counted_and_stamped_under_claude_code() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-claude");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        agent(&mut c, claude, "api", Kind::Claude, t0);
        let t1 = t0 + std::time::Duration::from_secs(3);
        agent(&mut c, claude, "web", Kind::Claude, t1);
        let codex = adapter(&mut c, AdapterKind::Codex, t0);
        agent(&mut c, codex, "thread", Kind::Codex, t0);
        let gemini = adapter(&mut c, AdapterKind::Gemini, t0);
        agent(&mut c, gemini, "gem", Kind::Other, t0);
        let reply = list(&mut c, t1);
        let row_of = |id| row(&reply, id).clone();
        let claude = row_of("claude");
        assert_eq!(
            (&claude["sessions"], &claude["on_deck"]),
            (&json!(2), &json!(2))
        );
        assert_eq!(
            claude["last_event_at"],
            json!(c.store.ts(t1)),
            "the latest call"
        );
        let codex = row_of("codex");
        assert_eq!(
            (&codex["sessions"], &codex["on_deck"]),
            (&json!(1), &json!(1))
        );
        assert_eq!(codex["last_event_at"], json!(c.store.ts(t0)));
        assert_eq!(reply["other"], 1, "the Gemini agent, under no row");
        assert_eq!(
            ids(&reply),
            ["claude", "claude_channels", "codex"],
            "{reply}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R3: `last_event_at` is when a source last did something. A lease
    /// renewal and the reclaim after a reconnect are bookkeeping: an idle
    /// app that only keeps its agents alive keeps the time of its last real
    /// call, and the next real call moves it.
    #[test]
    fn a_heartbeat_or_a_reclaim_leaves_the_last_event_where_it_was() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-bookkeeping");
        let t0 = Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let last = |c: &mut Core, now: Instant| {
            row(&list(c, now), "app:Conol")["last_event_at"].clone()
        };
        let conol = app(&mut c, "Conol", &[Scope::Agent], t0);
        let chat = agent(&mut c, conol, "chat", Kind::Conol, t0);
        let registered = json!(c.store.ts(t0));
        assert_eq!(last(&mut c, t0), registered);

        let renew = Request::AgentHeartbeat {
            agents: vec![chat.clone()],
        };
        c.api_call(Principal::Uid(501), conol, false, renew, at(25))
            .expect("renewed");
        c.on_msg(CoreMsg::CloseSession(conol), at(26));
        let again = app(&mut c, "Conol", &[Scope::Agent], at(26));
        let reclaim = Request::AgentClaim {
            key: "k:chat".into(),
        };
        c.api_call(Principal::Uid(501), again, false, reclaim, at(26))
            .expect("reclaimed");
        assert_eq!(last(&mut c, at(27)), registered, "bookkeeping says nothing");

        let busy = Request::AgentUpdate(deck_model::AgentUpdate {
            agent: chat,
            state: Some(deck_model::AgentState::Working),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None,
            task: None,
            subagents: None,
        });
        c.api_call(Principal::Uid(501), again, false, busy, at(30))
            .expect("served");
        assert_eq!(last(&mut c, at(30)), json!(c.store.ts(at(30))));
        // The rule on its own: everything a source does but the two.
        assert!(!activity(&Request::AgentHeartbeat { agents: Vec::new() }));
        assert!(!activity(&Request::AgentClaim { key: "k".into() }));
        assert!(activity(&Request::AgentRegister(testkit::register(
            "x", None
        ))));
        assert!(activity(&Request::DeviceWsSet {
            active: 0,
            count: 0,
            sel: 0,
            windows: Vec::new(),
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A connected app is listed by the name it connected with, with what
    /// its sessions are called and what it does; a client that registers
    /// nothing and may not (the companion app itself) is not a source.
    #[test]
    fn a_connected_app_is_listed_by_client_name_with_its_unit_and_what_it_does() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-apps");
        let t0 = Instant::now();
        let conol = app(&mut c, "Conol", &[Scope::Agent, Scope::Observe], t0);
        agent(&mut c, conol, "chat", Kind::Conol, t0);
        let omarchy = app(
            &mut c,
            "Omarchy remote",
            &[Scope::Device, Scope::Observe, Scope::Agent],
            t0,
        );
        agent(&mut c, omarchy, "kimi", Kind::Other, t0);
        let ws = Request::DeviceWsSet {
            active: 1,
            count: 1,
            sel: 0,
            windows: Vec::new(),
        };
        c.api_call(Principal::Uid(501), omarchy, false, ws, t0)
            .expect("a face");
        // Connected, registering nothing and holding `agent` only through
        // `admin`: the companion app.
        let gui = app(
            &mut c,
            "OpenWalkieTalkie",
            &[Scope::ObserveContent, Scope::Admin],
            t0,
        );
        c.api_call(
            Principal::Uid(501),
            gui,
            false,
            Request::DeviceSet(deck_model::DeviceSet {
                key: deck_model::SetKey::Brightness,
                value: 3,
            }),
            t0,
        )
        .expect("a setting");
        // Connected with `agent`, nothing registered yet.
        app(&mut c, "Acme", &[Scope::Agent], t0);
        let reply = list(&mut c, t0);
        assert_eq!(
            ids(&reply),
            [
                "claude",
                "claude_channels",
                "codex",
                "app:Acme",
                "app:Conol",
                "app:Omarchy remote"
            ]
        );
        let conol = row(&reply, "app:Conol");
        assert_eq!(conol["name"], "Conol");
        assert_eq!(
            (&conol["kind"], &conol["unit"]),
            (&json!("app"), &json!("chats"))
        );
        assert_eq!(conol["does"], json!(["sessions", "voice"]));
        assert_eq!(
            (&conol["state"], &conol["handoff"]),
            (&json!("connected"), &json!("self"))
        );
        assert_eq!(
            (&conol["sessions"], &conol["on_deck"]),
            (&json!(1), &json!(1))
        );
        assert_eq!(conol["last_event_at"], json!(c.store.ts(t0)));
        let omarchy = row(&reply, "app:Omarchy remote");
        assert_eq!(omarchy["unit"], "sessions");
        assert_eq!(omarchy["does"], json!(["sessions", "face"]));
        let acme = row(&reply, "app:Acme");
        assert_eq!(
            (&acme["sessions"], &acme["does"]),
            (&json!(0), &json!(["sessions"]))
        );
        assert_eq!(acme["last_event_at"], Value::Null, "never called in");
        // Gone with its last session, while it had no agent left behind.
        let acme = c
            .sessions
            .iter()
            .find(|(_, e)| e.client.name == "Acme")
            .map(|(id, _)| *id);
        c.on_msg(CoreMsg::CloseSession(acme.unwrap()), t0);
        assert!(!ids(&list(&mut c, t0)).contains(&"app:Acme"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// O6: the daemon's own sessions are never a source, not even the demo's
    /// example agent that speaks as the Claude adapter; an unnamed client is
    /// one app, listed by its principal.
    #[test]
    fn the_daemons_own_sessions_are_never_a_source_and_an_unnamed_client_goes_by_its_principal() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-daemon");
        let t0 = Instant::now();
        c.enable_demo(t0);
        assert!(c.store.snapshot().agents.len() >= 6, "the demo's roster");
        let reply = list(&mut c, t0);
        assert_eq!(
            ids(&reply),
            ["claude", "claude_channels", "codex"],
            "{reply}"
        );
        assert_eq!(
            row(&reply, "claude")["sessions"],
            0,
            "the example agent is the demo's"
        );
        assert_eq!(reply["other"], 0);
        assert_eq!(row(&reply, "claude")["last_event_at"], Value::Null);
        // Two connections that give no name are one app.
        let first = app(&mut c, "", &[Scope::Agent], t0);
        agent(&mut c, first, "one", Kind::Other, t0);
        let second = app(&mut c, "  ", &[Scope::Agent], t0);
        agent(&mut c, second, "two", Kind::Other, t0);
        let reply = list(&mut c, t0);
        assert_eq!(
            ids(&reply),
            ["claude", "claude_channels", "codex", "app:uid:501"]
        );
        let unnamed = row(&reply, "app:uid:501");
        assert_eq!(
            (&unnamed["name"], &unnamed["sessions"]),
            (&json!("uid:501"), &json!(2))
        );
        assert_eq!(
            app_name(
                crate::api::http::DEFAULT_CLIENT,
                &Principal::Token("ci".into())
            ),
            "token:ci"
        );
        assert_eq!(app_name(" Conol ", &Principal::Uid(501)), "Conol");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The word the delivery record files a session's cards under: the
    /// source, an app as `app` and never its name, the daemon's own as
    /// `daemon` (the demo's example agent speaks as Claude and is still the
    /// demo's), and a closed session's while an agent it left names it.
    #[test]
    fn the_delivery_record_names_a_source_by_its_word_and_never_an_app_by_name() {
        let t0 = Instant::now();
        let mut sessions = Sessions::new();
        let mut ledger = Ledger::default();
        for (id, kind) in [
            (50, AdapterKind::Claude),
            (51, AdapterKind::Codex),
            (52, AdapterKind::Gemini),
            // The demo's and the test agent's.
            (53, AdapterKind::Virtual),
        ] {
            sessions.open_internal(SessionId(id), Principal::Adapter(kind), "internal", t0);
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let conol = sessions.open(
            OpenSession {
                kind: SessionKind::Live { tx },
                principal: Principal::Uid(501),
                scopes: vec![Scope::Agent],
                trusted: false,
                client: ClientInfo {
                    name: "Conol".into(),
                    version: "1".into(),
                },
            },
            t0,
        );
        let words = [50, 51, 52, 53].map(|id| ledger.word(&sessions, SessionId(id)));
        assert_eq!(words, ["claude", "codex", "other", "daemon"]);
        assert_eq!(ledger.word(&sessions, conol), "app");
        assert_eq!(ledger.word(&sessions, SessionId(4242)), "unknown");
        // The app's connection closes with an agent left in its grace, the
        // way `Core::close_session` remembers it first.
        ledger.remember(&sessions, conol);
        sessions.close(conol);
        assert_eq!(ledger.word(&sessions, conol), "app");
    }

    /// "Pause integrations": every source's sessions leave the deck and the
    /// slots they free are put back on the wire as empty; the daemon's own
    /// demo stays. Resuming gives every slot back.
    #[test]
    fn pausing_mutes_every_source_and_takes_its_agents_off_the_deck() {
        let (mut c, ops, _lines, _debug, dir) = detached_core("sources-pause");
        let t0 = Instant::now();
        c.enable_demo(t0);
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        let api = agent(&mut c, claude, "api", Kind::Claude, t0);
        let conol = app(&mut c, "Conol", &[Scope::Agent], t0);
        let chat = agent(&mut c, conol, "chat", Kind::Conol, t0);
        let demo: Vec<AgentId> = c
            .store
            .snapshot()
            .agents
            .values()
            .filter(|a| a.owner.session == crate::demo::SESSION)
            .map(|a| a.id.clone())
            .collect();
        let slots: Vec<Slot> = [&api, &chat]
            .iter()
            .map(|id| slot(&c, id).expect("on the deck"))
            .collect();
        let _ = ops.try_iter().count();

        reconfigure(&mut c, t0, |config| config.sources.paused = true);
        assert_eq!(
            (slot(&c, &api), slot(&c, &chat)),
            (None, None),
            "off the deck"
        );
        assert!(
            c.store.snapshot().agents.contains_key(&api)
                && c.store.snapshot().agents.contains_key(&chat),
            "and kept"
        );
        assert!(
            demo.iter().all(|id| slot(&c, id).is_some()),
            "the demo is no source"
        );
        let gone: Vec<DeviceOp> = ops.try_iter().collect();
        for slot in &slots {
            assert!(
                gone.contains(&DeviceOp::Put(
                    Key::AgentGone(*slot),
                    HostLine::AgentGone { slot: slot.0 }
                )),
                "slot {} cleared on the deck: {gone:?}",
                slot.0
            );
        }
        let reply = list(&mut c, t0);
        assert_eq!(reply["paused"], true);
        assert_eq!(
            row(&reply, "claude")["enabled"],
            true,
            "its own switch did not move"
        );
        let conol_row = row(&reply, "app:Conol");
        assert_eq!(
            (&conol_row["on_deck"], &conol_row["sessions"]),
            (&json!(0), &json!(1))
        );
        // Its owner's calls are still served while paused, and bring
        // nothing back.
        let busy = Request::AgentUpdate(deck_model::AgentUpdate {
            agent: api.clone(),
            state: Some(deck_model::AgentState::Working),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None,
            task: None,
            subagents: None,
        });
        let principal = Principal::Adapter(AdapterKind::Claude);
        c.api_call(principal, claude, false, busy, t0)
            .expect("served");
        assert_eq!(slot(&c, &api), None);

        reconfigure(&mut c, t0, |config| config.sources.paused = false);
        assert_eq!(slot(&c, &api), Some(slots[0]), "the slot it had");
        assert_eq!(slot(&c, &chat), Some(slots[1]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One app paused alone: only its sessions leave the deck, they stay
    /// off while its connection drops and comes back, and resuming it gives
    /// them back.
    #[test]
    fn one_app_paused_alone_stays_paused_across_a_reconnect() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-one-app");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        let api = agent(&mut c, claude, "api", Kind::Claude, t0);
        let conol = app(&mut c, "Conol", &[Scope::Agent], t0);
        let chat = agent(&mut c, conol, "chat", Kind::Conol, t0);
        let omarchy = app(&mut c, "Omarchy remote", &[Scope::Agent], t0);
        let kimi = agent(&mut c, omarchy, "kimi", Kind::Other, t0);

        reconfigure(&mut c, t0, |config| {
            config.sources.apps_paused = vec!["Conol".into()]
        });
        assert_eq!(slot(&c, &chat), None);
        assert!(
            slot(&c, &api).is_some() && slot(&c, &kimi).is_some(),
            "the others stay"
        );
        let reply = list(&mut c, t0);
        assert_eq!(reply["paused"], false);
        assert_eq!(row(&reply, "app:Conol")["enabled"], false);
        assert_eq!(row(&reply, "app:Omarchy remote")["enabled"], true);

        // The connection drops: its chat is orphaned and stays off the deck.
        c.on_msg(CoreMsg::CloseSession(conol), t0);
        assert_eq!(slot(&c, &chat), None, "orphaned, still paused");
        let waiting = list(&mut c, t0);
        assert_eq!(row(&waiting, "app:Conol")["state"], "waiting");
        assert_eq!(row(&waiting, "app:Conol")["sessions"], 1);
        // It comes back under a new session, which is muted before it can
        // register anything.
        let again = app(&mut c, "Conol", &[Scope::Agent], t0);
        let claimed = c.api_call(
            Principal::Uid(501),
            again,
            false,
            Request::AgentClaim {
                key: "k:chat".into(),
            },
            t0,
        );
        assert!(claimed.is_ok(), "{claimed:?}");
        let fresh = agent(&mut c, again, "fresh", Kind::Conol, t0);
        assert_eq!((slot(&c, &chat), slot(&c, &fresh)), (None, None));
        assert_eq!(row(&list(&mut c, t0), "app:Conol")["state"], "connected");

        reconfigure(&mut c, t0, |config| config.sources.apps_paused.clear());
        assert!(slot(&c, &chat).is_some() && slot(&c, &fresh).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A question parked for a Claude Code session goes back to its tool
    /// the moment Claude Code is paused: the hook is released, so the CLI
    /// shows its own dialog, and the card leaves the deck with the agent.
    #[test]
    fn pausing_claude_code_hands_a_parked_question_back_to_its_tool() {
        use crate::core::handle::HoldResult;
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-held");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        let principal = Principal::Adapter(AdapterKind::Claude);
        let register = Request::AgentRegister(crate::demo::permission_register());
        let registered = c
            .api_call(principal.clone(), claude, false, register, t0)
            .unwrap();
        let id: AgentId = serde_json::from_value(registered["agent"].clone()).unwrap();
        let (reply, mut parked) = tokio::sync::oneshot::channel();
        let cmd = crate::demo::permission_card(&id);
        c.on_msg(
            CoreMsg::SetCardHeld {
                principal,
                session: claude,
                cmd,
                reply,
            },
            t0,
        );
        let (_card, _token, mut hold) = parked.try_recv().unwrap().expect("parked");
        assert!(hold.try_recv().is_err(), "waiting on the deck");

        reconfigure(&mut c, t0, |config| config.sources.claude = false);
        match hold.try_recv() {
            Ok(HoldResult::Released { .. }) => {}
            other => panic!("the hook is still held: {other:?}"),
        }
        assert_eq!(slot(&c, &id), None);
        assert!(
            c.store.snapshot().agents[&id].card.is_none(),
            "the card left with it"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Claude Code switched off mutes the Claude adapter's session and
    /// nothing else; the Codex switch mutes nothing, because it ends the
    /// app-server connection instead.
    #[test]
    fn claude_code_off_mutes_its_session_alone_and_the_codex_switch_mutes_nothing() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-claude-off");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        let api = agent(&mut c, claude, "api", Kind::Claude, t0);
        let codex = adapter(&mut c, AdapterKind::Codex, t0);
        let thread = agent(&mut c, codex, "thread", Kind::Codex, t0);
        let conol = app(&mut c, "Conol", &[Scope::Agent], t0);
        let chat = agent(&mut c, conol, "chat", Kind::Conol, t0);

        reconfigure(&mut c, t0, |config| config.sources.claude = false);
        assert_eq!(slot(&c, &api), None);
        assert!(slot(&c, &thread).is_some() && slot(&c, &chat).is_some());
        let reply = list(&mut c, t0);
        assert_eq!(row(&reply, "claude")["enabled"], false);
        assert_eq!(reply["paused"], false);

        reconfigure(&mut c, t0, |config| {
            config.sources.claude = true;
            config.codex.daemon = false;
        });
        assert!(slot(&c, &api).is_some() && slot(&c, &thread).is_some());
        let codex = row(&list(&mut c, t0), "codex").clone();
        assert_eq!(
            (&codex["enabled"], &codex["state"]),
            (&json!(false), &json!("off"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R3: a pause written through `config.set` is in force when the call
    /// answers, with no reload and no watcher in between. `config.get` and
    /// `sources.list` say paused at once, the app's agent has left the deck
    /// in the same step, and one it registers straight after gets no slot;
    /// resuming the same way gives the slots back.
    #[test]
    fn a_pause_set_through_config_set_is_in_force_before_the_call_answers() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-config-set");
        let t0 = Instant::now();
        let admin = || testkit::ctx(2, &[Scope::Admin]);
        let set = |c: &mut Core, paused: bool| {
            let patch = json!({"patch": {"sources.paused": paused}});
            c.call(admin(), "config.set", patch, t0)
                .result
                .expect("written")
        };
        let conol = app(&mut c, "Conol", &[Scope::Agent, Scope::Observe], t0);
        let chat = agent(&mut c, conol, "chat", Kind::Conol, t0);
        assert!(slot(&c, &chat).is_some(), "on the deck");

        let ok = set(&mut c, true);
        assert_eq!(ok["applied"], json!(["sources.paused"]));
        let published = c
            .call(admin(), "config.get", json!({}), t0)
            .result
            .expect("config.get answers");
        assert_eq!(published["sources"]["paused"], true);
        let reply = list(&mut c, t0);
        assert_eq!(reply["paused"], true);
        let conol_row = row(&reply, "app:Conol");
        assert_eq!(
            (&conol_row["on_deck"], &conol_row["sessions"]),
            (&json!(0), &json!(1))
        );
        assert_eq!(slot(&c, &chat), None, "muted in the same step");
        let late = agent(&mut c, conol, "late", Kind::Conol, t0);
        assert_eq!(slot(&c, &late), None, "a newcomer is muted before it lands");

        set(&mut c, false);
        assert_eq!(list(&mut c, t0)["paused"], false);
        assert!(slot(&c, &chat).is_some() && slot(&c, &late).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D4: the Claude Channels row's switch is the owner's opt-in, resolved
    /// as `config.get` resolves it: unset, it is whatever this daemon's
    /// plugin carries; set, the file's word wins either way. It is no
    /// pause: Claude Code's agents keep their slots whichever way it goes.
    #[test]
    fn the_channels_row_follows_the_claude_channels_switch() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-channels");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        let api = agent(&mut c, claude, "api", Kind::Claude, t0);
        let channels = |c: &mut Core| {
            let row = row(&list(c, t0), "claude_channels").clone();
            (row["enabled"].clone(), row["state"].clone())
        };
        let (off, on) = (
            (json!(false), json!("off")),
            (json!(true), json!("waiting")),
        );
        assert_eq!(channels(&mut c), off, "a new install");
        // The test core's data directory is `dir`.
        let helper = crate::integrations::claude::mcp_json_in(&dir);
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, b"{}\n").unwrap();
        assert_eq!(channels(&mut c), on, "a plugin from before the switch");
        reconfigure(&mut c, t0, |config| {
            config.sources.claude_channels = Some(false)
        });
        assert_eq!(channels(&mut c), off, "the owner's word over the plugin");
        assert!(slot(&c, &api).is_some(), "an opt-in, not a pause");
        std::fs::remove_file(&helper).unwrap();
        reconfigure(&mut c, t0, |config| {
            config.sources.claude_channels = Some(true)
        });
        assert_eq!(channels(&mut c), on);
        assert!(slot(&c, &api).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The shape of a column, for comparing the reply with the fixture the
    /// app's parser is tested on.
    fn json_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Every key of `want`, with the same JSON type, in `got`, and no key
    /// `want` does not have.
    fn same_shape(what: &str, want: &Value, got: &Value) {
        let (want, got) = (want.as_object().unwrap(), got.as_object().unwrap());
        for (key, value) in want {
            let Some(mine) = got.get(key) else {
                panic!("{what}: no {key:?}")
            };
            assert_eq!(
                json_type(mine),
                json_type(value),
                "{what}.{key}: {mine} where {value}"
            );
        }
        for key in got.keys() {
            assert!(
                want.contains_key(key),
                "{what}: {key:?} is not in the fixture"
            );
        }
    }

    /// B3.3: the real reply, for a state like the one the app's fixture
    /// describes, has every key of `GC/tests/fixtures/daemon/sources_list.json`
    /// with the same JSON type, row by row, and what the apps do reads the
    /// same.
    #[test]
    fn the_reply_has_every_key_of_the_apps_fixture_with_the_same_types() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/sources_list.json"
        ))
        .unwrap();
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-fixture");
        let t0 = Instant::now();
        let claude = adapter(&mut c, AdapterKind::Claude, t0);
        agent(&mut c, claude, "api", Kind::Claude, t0);
        let codex = adapter(&mut c, AdapterKind::Codex, t0);
        agent(&mut c, codex, "thread", Kind::Codex, t0);
        let conol = app(&mut c, "Conol", &[Scope::Agent, Scope::Observe], t0);
        agent(&mut c, conol, "chat", Kind::Conol, t0);
        let omarchy = app(
            &mut c,
            "Omarchy remote",
            &[Scope::Device, Scope::Observe, Scope::Agent],
            t0,
        );
        agent(&mut c, omarchy, "kimi", Kind::Other, t0);
        let ws = Request::DeviceWsSet {
            active: 1,
            count: 1,
            sel: 0,
            windows: Vec::new(),
        };
        c.api_call(Principal::Uid(501), omarchy, false, ws, t0)
            .unwrap();
        let reply = list(&mut c, t0);
        same_shape("sources.list", &fixture, &reply);
        let rows = fixture["sources"].as_array().unwrap();
        assert_eq!(
            ids(&reply),
            rows.iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>()
        );
        for want in rows {
            let id = want["id"].as_str().unwrap();
            let got = row(&reply, id);
            same_shape(id, want, got);
            for key in ["kind", "unit", "does"] {
                assert_eq!(got[key], want[key], "{id}.{key}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// B3.3 for `config.get`: the `[sources]` and `[codex]` tables of the
    /// real reply have every key of
    /// `GC/tests/fixtures/daemon/config_get_sources.json` with the same JSON
    /// type and none it lacks, `sources.claude_channels` among them (D4):
    /// published resolved, a bool and never `null`, whether the file sets
    /// the switch or the plugin decides it.
    #[test]
    fn config_get_publishes_the_switches_the_apps_fixture_reads() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json"
        ))
        .unwrap();
        let (mut c, _ops, _lines, _debug, dir) = detached_core("sources-config-get");
        let t0 = Instant::now();
        let get = |c: &mut Core| {
            c.call(
                testkit::ctx(2, &[Scope::Admin]),
                "config.get",
                json!({}),
                t0,
            )
            .result
            .expect("config.get answers")
        };
        let published = get(&mut c);
        assert_eq!(published["sources"]["apps_paused"], json!([]));
        assert_eq!(published["codex"]["daemon"], true);
        // The test core's data directory is `dir`.
        let helper = crate::integrations::claude::mcp_json_in(&dir);
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        for (case, switch, plugin) in [
            ("a new install", None, false),
            ("an install from before the switch", None, true),
            ("switched on", Some(true), false),
            ("switched off", Some(false), true),
        ] {
            if plugin {
                std::fs::write(&helper, b"{}\n").unwrap();
            } else {
                let _ = std::fs::remove_file(&helper);
            }
            reconfigure(&mut c, t0, |config| {
                config.sources.claude_channels = switch
            });
            let published = get(&mut c);
            for table in ["sources", "codex"] {
                same_shape(
                    &format!("{case}: {table}"),
                    &fixture[table],
                    &published[table],
                );
            }
            assert_eq!(
                published["sources"]["claude_channels"],
                switch.unwrap_or(plugin),
                "{case}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
