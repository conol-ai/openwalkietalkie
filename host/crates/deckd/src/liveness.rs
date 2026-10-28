//! The liveness pollers (design §5.1, week-3 contract §1.5 and C9).
//!
//! A CLI that is killed, crashes or is closed from the window manager
//! sends no `SessionEnd`: the hook never runs, so the deck would keep a
//! dead agent in its slot until something else evicted it. Two pollers
//! answer that, and only that.
//!
//! * **The roster poll** — `claude agents --json` every
//!   [`ROSTER_S`] seconds. It is the primary source: it says which CLI
//!   sessions exist and what each one is doing. C9: an **interactive**
//!   row carries `status` (`busy | waiting | idle`) and **no `state`**,
//!   so the reconciliation keys off `status`; `state` is a background
//!   row's field and is read only there. The call is a process spawn
//!   (measured 0.12–0.17 s with six sessions), so it runs on
//!   `spawn_blocking` — never on a tokio worker, where it would stall
//!   every other door for as long as the CLI took to print.
//! * **The pid poll** — `kill(pid, 0)` every [`PID_S`] seconds, **only**
//!   for an agent whose `pid_kind` is `cli` (B1, design §4.6). That is
//!   the one pid the hook helper's parent walk resolved to a real CLI
//!   process. A `shell` or `unknown` pid is the `sh -c` that ran the
//!   hook and exited a millisecond later, so polling it would evict a
//!   healthy session within two seconds; those agents are never polled
//!   at all, which is what `liveness::only_cli_pids_are_polled` pins.
//!
//! Three rules keep the pollers from doing harm:
//!
//! 1. **Absence of evidence is not evidence.** A probe that cannot run,
//!    exits non-zero, times out or prints something that is not a JSON
//!    array is *inconclusive*: the tick changes nothing and does not
//!    count against any agent. C11's silent no-hook has the same shape —
//!    the daemon must not read "I could not ask" as "it is gone".
//! 2. **Two strikes.** A session must be missing from
//!    [`ABSENT_BEFORE_RELEASE`] consecutive conclusive ticks before it is
//!    released, so a roster that lags a fresh `SessionStart` by one tick
//!    never flaps an agent off the deck.
//! 3. **Nothing is logged but ids and counts** (§11). A row carries the
//!    session's `cwd` and display name; neither reaches a log line.
//!
//! The poll costs nothing while no CLI session is on the roster: with no
//! adapter agent to reconcile and `[liveness] seed` off, [`run`] does not
//! spawn the probe at all.
use crate::api::Api;
use deck_model::{
    AdapterKind, Agent, AgentId, AgentRegister, AgentState, AgentUpdate, AnswerCap, Caps, Kind,
    OrderCap, PidKind, Principal, Request, SessionId, Snapshot,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the CLI roster is read (design §5.1).
pub const ROSTER_S: u64 = 5;
/// How often a resolved CLI pid is probed with `kill(pid, 0)`.
pub const PID_S: u64 = 2;
/// The longest a roster probe may take before it is killed and the tick
/// reported inconclusive. Shorter than [`ROSTER_S`], so a wedged CLI can
/// never queue two probes on the blocking pool.
pub const PROBE_BUDGET: Duration = Duration::from_secs(4);
/// How often a running probe is checked for having exited.
const PROBE_POLL: Duration = Duration::from_millis(20);
/// Consecutive conclusive ticks a session must be missing from before its
/// agent is released.
pub const ABSENT_BEFORE_RELEASE: u32 = 2;
/// The `agent.release` reason both pollers write. It is the one string
/// `deckd events` shows for "the CLI went away without saying so".
pub const GONE: &str = "gone";

/// One row of `claude agents --json` (facts-claude §4.1/§4.2).
///
/// Every field is optional on purpose: the measured interactive rows carry
/// `pid, cwd, kind, startedAt, sessionId, name, status` and nothing else,
/// while the documented set adds `id`, `state` and `waitingFor` for
/// background sessions. Unknown fields are ignored rather than refused —
/// a Claude release that adds one must not blind the poller.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Row {
    #[serde(default)]
    pub pid: Option<u32>,
    /// `interactive` or `background`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, rename = "sessionId")]
    pub session_id: Option<String>,
    /// Content: the session's display name. Never logged.
    #[serde(default)]
    pub name: Option<String>,
    /// Content: the session's working directory. Never logged.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `busy | waiting | idle`, present while the process is alive. **This
    /// is the field liveness keys off** (C9).
    #[serde(default)]
    pub status: Option<String>,
    /// Only when `status == "waiting"`.
    #[serde(default, rename = "waitingFor")]
    pub waiting_for: Option<String>,
    /// `working | blocked | done | failed | stopped` — **background rows
    /// only** (C9). Never present on an interactive session, so nothing
    /// may key off it.
    #[serde(default)]
    pub state: Option<String>,
    /// Not from the CLI: the session's message inbox socket, read by
    /// [`enrich`] from `<claude dir>/sessions/<pid>.json`
    /// (`messagingSocketPath`). It is what lets a session the daemon never
    /// saw start be **ordered** (`crate::inbox`), not merely listed.
    #[serde(skip)]
    pub inbox: Option<String>,
    /// Not from the CLI: the last assistant message in the session's
    /// transcript, read by [`enrich`] so a seeded agent has a Ready text to
    /// show and speak rather than a blank. Content, never logged.
    #[serde(skip)]
    pub last_reply: Option<String>,
}

impl Row {
    /// Whether this row describes a session that is still running.
    ///
    /// `pid` and `status` are documented as present "while the process is
    /// alive", so a row with neither is a finished background session that
    /// `--all` kept in the list, not a live one.
    #[must_use]
    pub fn alive(&self) -> bool {
        if self.kind.as_deref() == Some("background") {
            // A background row states its own outcome.
            if matches!(self.state.as_deref(), Some("done" | "failed" | "stopped")) {
                return false;
            }
        }
        self.pid.is_some() || self.status.is_some()
    }

    /// The agent state this row implies, or `None` when the row says
    /// nothing the deck owns.
    ///
    /// `waiting` is deliberately **not** mapped: the deck's `question`
    /// state belongs to the card the adapter parked, and a `waiting` with
    /// no card is the CLI's own dialog, which the roster poll must not
    /// paint over. `state` is read for a background row only (C9).
    #[must_use]
    pub fn agent_state(&self) -> Option<AgentState> {
        match self.status.as_deref() {
            Some("busy") => return Some(AgentState::Working),
            Some("idle") => return Some(AgentState::Idle),
            Some(_) => return None,
            None => {}
        }
        match self.state.as_deref() {
            Some("working") => Some(AgentState::Working),
            Some("failed") => Some(AgentState::Error),
            _ => None,
        }
    }
}

/// How the roster is read: a program name resolved on `PATH`, plus the
/// `PATH` to resolve it against.
///
/// The override exists for the tests, which put a fake `claude` in a temp
/// directory rather than run the operator's own CLI: `Command` resolves an
/// un-pathed program against the `PATH` set **on the command**, so the
/// daemon's own environment is never touched and two tests can hold two
/// different fakes at once.
#[derive(Clone, Debug)]
pub struct Probe {
    bin: String,
    path: Option<OsString>,
}

impl Probe {
    /// The operator's own CLI, resolved against the daemon's `PATH`.
    #[must_use]
    pub fn new(bin: impl Into<String>) -> Probe {
        Probe {
            bin: bin.into(),
            path: None,
        }
    }

    /// The same, resolved against `path` instead.
    #[must_use]
    pub fn on_path(bin: impl Into<String>, path: impl Into<OsString>) -> Probe {
        Probe {
            bin: bin.into(),
            path: Some(path.into()),
        }
    }

    /// `<bin> agents --json`, parsed. **Blocking**: every caller runs it on
    /// `spawn_blocking`.
    ///
    /// An `Err` is inconclusive, never "there are no sessions": a missing
    /// binary, a non-zero exit, a timeout and unparseable output all come
    /// back here, and [`reconcile`] is never called with any of them.
    pub fn rows(&self) -> Result<Vec<Row>, String> {
        let mut cmd = std::process::Command::new(&self.bin);
        cmd.arg("agents")
            .arg("--json")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + PROBE_BUDGET;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("the roster probe timed out".into());
                    }
                    std::thread::sleep(PROBE_POLL);
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        // The output is a few hundred bytes per session, far inside the
        // pipe buffer, so reading after the exit cannot deadlock — and
        // reading before it would mean no timeout at all.
        let mut out = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            use std::io::Read;
            let _ = stdout.read_to_end(&mut out);
        }
        if !status.success() {
            return Err(format!("the roster probe exited {status}"));
        }
        serde_json::from_slice::<Vec<Row>>(&out).map_err(|e| e.to_string())
    }
}

/// One thing a tick decided, addressed to the session that owns the agent.
///
/// The poller speaks as the agent's own owner — the adapter's internal
/// session — because that is the only principal a release may come from
/// (B3): the daemon reconciling its own adapter's roster is not a reason
/// to let anything else evict an agent.
#[derive(Clone, Debug)]
pub struct Act {
    pub principal: Principal,
    pub session: SessionId,
    pub req: Request,
    /// For a seeding register: the inbox to record against the agent the
    /// register resolves (`adapters::rt::Adapters::set_inbox`). The
    /// request itself carries only the socket path, in `terminal.inbox`;
    /// this is the same path, kept beside the act so `apply` can file it
    /// once it knows the agent id.
    pub inbox: Option<crate::inbox::Inbox>,
}

/// What one tick did, for the log line and for the tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// Live CLI sessions the probe reported.
    pub seen: usize,
    /// Agents whose state or name the roster corrected.
    pub updated: Vec<AgentId>,
    /// Agents released: the CLI session is gone.
    pub released: Vec<AgentId>,
    /// CLI sessions registered by `[liveness] seed`.
    pub seeded: Vec<String>,
    /// Agents whose pid was actually probed (pid poll only). Everything
    /// not in this list was never `kill`ed.
    pub polled: Vec<AgentId>,
    /// The probe could not be trusted: nothing was changed or counted.
    pub inconclusive: bool,
}

/// The memo the roster poll keeps between ticks: how many conclusive ticks
/// in a row each agent has been missing from the CLI's own roster.
pub type Misses = BTreeMap<AgentId, u32>;

/// Whether this agent is one the roster poll governs: a Claude session
/// this daemon's own adapter owns and named with a `claude:<session_id>`
/// key. A virtual agent registered by a token is nobody's CLI session.
#[must_use]
pub fn claude_session(agent: &Agent) -> Option<&str> {
    if agent.kind != Kind::Claude
        || agent.owner.principal != Principal::Adapter(AdapterKind::Claude)
    {
        return None;
    }
    agent
        .session
        .key
        .as_deref()
        .and_then(|key| key.strip_prefix("claude:"))
        .filter(|id| !id.is_empty())
}

/// The roster poll's decision, as a pure function of the snapshot, the
/// rows and the miss memo (design §5.1).
///
/// `owner` is the Claude adapter's internal session, needed only to seed;
/// pass `None` when it has not been opened, and no seed is proposed.
pub fn reconcile(
    snapshot: &Snapshot,
    rows: &[Row],
    misses: &mut Misses,
    owner: Option<(&Principal, SessionId)>,
    seed: bool,
) -> (Vec<Act>, Tick) {
    let live: BTreeMap<&str, &Row> = rows
        .iter()
        .filter(|row| row.alive())
        .filter_map(|row| Some((row.session_id.as_deref()?, row)))
        .collect();
    let mut tick = Tick {
        seen: live.len(),
        ..Tick::default()
    };
    let mut acts = Vec::new();
    let mut known: BTreeSet<&str> = BTreeSet::new();
    for agent in snapshot.agents.values() {
        let Some(id) = claude_session(agent) else {
            continue;
        };
        known.insert(id);
        match live.get(id) {
            Some(row) => {
                misses.remove(&agent.id);
                if let Some(update) = correction(agent, row) {
                    tick.updated.push(agent.id.clone());
                    acts.push(Act {
                        principal: agent.owner.principal.clone(),
                        session: agent.owner.session,
                        req: Request::AgentUpdate(update),
                        inbox: None,
                    });
                }
            }
            None => {
                let strikes = misses.entry(agent.id.clone()).or_default();
                *strikes += 1;
                if *strikes >= ABSENT_BEFORE_RELEASE {
                    misses.remove(&agent.id);
                    tick.released.push(agent.id.clone());
                    acts.push(Act {
                        principal: agent.owner.principal.clone(),
                        session: agent.owner.session,
                        req: Request::AgentRelease {
                            agent: agent.id.clone(),
                            reason: Some(GONE.into()),
                        },
                        inbox: None,
                    });
                }
            }
        }
    }
    // An agent that left the roster between two ticks leaves no strike
    // behind to be counted against a later session of the same id.
    misses.retain(|id, _| snapshot.agents.contains_key(id));
    if seed {
        if let Some((principal, session)) = owner {
            for (id, row) in &live {
                if known.contains(id) || row.kind.as_deref() == Some("background") {
                    continue;
                }
                tick.seeded.push((*id).to_string());
                acts.push(Act {
                    principal: principal.clone(),
                    session,
                    req: Request::AgentRegister(seeded(id, row)),
                    inbox: row.inbox.as_deref().map(crate::inbox::Inbox::socket_only),
                });
                // The transcript's last assistant message, so the seeded
                // agent shows (and can speak) what the session last said.
                // Addressed to the register's agent by `apply`.
                if let Some(text) = &row.last_reply {
                    acts.push(Act {
                        principal: principal.clone(),
                        session,
                        req: Request::Reply(deck_model::ReplySet {
                            message: None,
                            agent: crate::adapters::pending(),
                            text: text.clone(),
                            speak: false,
                            announce: None,
                            lane: None,
                        }),
                        inbox: None,
                    });
                }
            }
        }
    }
    (acts, tick)
}

/// The `agent.update` this row asks for, or `None` when the roster agrees
/// with the deck. Only fields that actually differ are written: an update
/// per tick per agent would bump `rev` five times a minute for nothing.
fn correction(agent: &Agent, row: &Row) -> Option<AgentUpdate> {
    let state = row.agent_state().filter(|s| *s != agent.state);
    // The deck's own `question` state outranks the roster: it means a card
    // is on the deck, which the CLI's `busy`/`idle` knows nothing about.
    let state = state.filter(|_| agent.state != AgentState::Question);
    let name = row
        .name
        .as_deref()
        .filter(|n| !n.trim().is_empty() && *n != agent.name)
        .map(str::to_string);
    if state.is_none() && name.is_none() {
        return None;
    }
    Some(AgentUpdate {
        agent: agent.id.clone(),
        state,
        title: None,
        title_source: None,
        since: None,
        name,
        meta: None,
    })
}

/// The registration `[liveness] seed` writes for a CLI session that was
/// already running when the daemon started.
///
/// It carries what the roster knows and nothing it does not: no terminal
/// identity (§5.5's comes from the hook helper's environment, which no
/// poll can see) — but the session's **inbox** when [`enrich`] found it,
/// which is the one route an order can take to a session like this — and
/// `pid_kind: cli`, because the pid on the row **is** the CLI's own.
fn seeded(session_id: &str, row: &Row) -> AgentRegister {
    let terminal = row.inbox.as_ref().map(|inbox| deck_model::Terminal {
        inbox: Some(inbox.clone()),
        ..deck_model::Terminal::default()
    });
    AgentRegister {
        kind: Kind::Claude,
        name: row
            .name
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .map_or_else(
                || crate::adapters::claude::name_of(row.cwd.as_deref(), session_id),
                str::to_string,
            ),
        key: Some(format!("claude:{session_id}")),
        cwd: row.cwd.clone(),
        pid: row.pid,
        pid_kind: row.pid.map(|_| PidKind::Cli),
        terminal,
        lease_ttl_s: 0,
        caps: Caps {
            answer: AnswerCap::Hold,
            order: OrderCap::Inject,
            focus: true,
        },
        slot_hint: None,
        voice: None,
        meta: None,
    }
}

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`: where Claude Code keeps its
/// session registry (`sessions/<pid>.json`) and transcripts
/// (`projects/<cwd slug>/<session id>.jsonl`).
#[must_use]
pub fn claude_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| std::path::PathBuf::from(home).join(".claude"))
}

/// Does Claude's session registry hold any entry at all?
#[must_use]
pub fn sessions_registered(claude_dir: &std::path::Path) -> bool {
    std::fs::read_dir(claude_dir.join("sessions")).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "json"))
    })
}

/// How much of a transcript's tail is read for the last assistant message.
/// A turn's final text is in the last few lines; 256 KB covers a long tool
/// burst before it without reading a multi-megabyte file.
pub const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;
/// A reply longer than this is cut (the deck shows ≤200 B, the GUI more).
pub const REPLY_MAX_CHARS: usize = 2_000;

/// Fill in what the CLI's roster does not say about a session the daemon
/// has no agent for: its inbox socket and its last assistant message. Pure
/// over `claude_dir`, so a test points it at a temp tree. A row whose files
/// are missing or unreadable is left as it was — the seed still happens,
/// only poorer.
pub fn enrich<'a>(rows: impl IntoIterator<Item = &'a mut Row>, claude_dir: &std::path::Path) {
    for row in rows {
        if !row.alive() || row.kind.as_deref() == Some("background") {
            continue;
        }
        if let Some(pid) = row.pid {
            row.inbox = inbox_of(claude_dir, pid);
        }
        if let (Some(cwd), Some(id)) = (row.cwd.as_deref(), row.session_id.as_deref()) {
            row.last_reply = last_assistant_text(&transcript_path(claude_dir, cwd, id));
        }
    }
}

/// `messagingSocketPath` from `<claude dir>/sessions/<pid>.json`, when the
/// registry entry is for this pid and names one.
fn inbox_of(claude_dir: &std::path::Path, pid: u32) -> Option<String> {
    let bytes = std::fs::read(claude_dir.join("sessions").join(format!("{pid}.json"))).ok()?;
    let entry: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if entry["pid"].as_u64() != Some(u64::from(pid)) {
        return None;
    }
    entry["messagingSocketPath"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// `<claude dir>/projects/<slug>/<session id>.jsonl`, where the slug is the
/// cwd with every character outside `[A-Za-z0-9]` replaced by `-`.
#[must_use]
pub fn transcript_path(
    claude_dir: &std::path::Path,
    cwd: &str,
    session_id: &str,
) -> std::path::PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    claude_dir
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"))
}

/// The text of the last `assistant` entry in a transcript's tail: the
/// `text` blocks of its `message.content`, joined. `None` when the file is
/// missing, unreadable or has no assistant text in its last
/// [`TRANSCRIPT_TAIL_BYTES`].
#[must_use]
pub fn last_assistant_text(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // The first line of a mid-file read is a torn one; `rev()` plus a
    // parse failure skips it like any other non-entry.
    text.lines().rev().find_map(|line| {
        let entry: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if entry["type"].as_str() != Some("assistant") {
            return None;
        }
        let content = &entry["message"]["content"];
        let joined = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b["type"].as_str() == Some("text"))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<&str>>()
                .join("\n"),
            _ => return None,
        };
        let joined = joined.trim();
        if joined.is_empty() {
            return None;
        }
        Some(joined.chars().take(REPLY_MAX_CHARS).collect())
    })
}

/// Whether a process exists, by `kill(pid, 0)`.
///
/// `EPERM` is **alive**: the process is there and belongs to somebody
/// else. Only `ESRCH` — no such process — is death, and pid 0 (the
/// process group) is never a CLI.
#[must_use]
pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` with signal 0 performs the permission and existence
    // checks and sends nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The pid poll's decision, as a pure function of the snapshot and an
/// aliveness test (design §5.1, B1).
///
/// **Only** an agent with `pid_kind == cli` and a pid is probed, and
/// `Tick::polled` names exactly those: an agent whose `pid_kind` is
/// `shell`, `unknown` or unset is not passed to `alive` at all. The
/// helper's parent walk is what resolves `cli`, so an agent that never
/// went through it is never evicted by a pid that was only ever the shell
/// the hook ran in.
pub fn sweep(snapshot: &Snapshot, alive: impl Fn(u32) -> bool) -> (Vec<Act>, Tick) {
    let mut tick = Tick::default();
    let mut acts = Vec::new();
    for agent in snapshot.agents.values() {
        if !matches!(agent.owner.principal, Principal::Adapter(_)) {
            continue;
        }
        if agent.session.pid_kind != Some(PidKind::Cli) {
            continue;
        }
        let Some(pid) = agent.session.pid else {
            continue;
        };
        tick.polled.push(agent.id.clone());
        if alive(pid) {
            continue;
        }
        tick.released.push(agent.id.clone());
        acts.push(Act {
            principal: agent.owner.principal.clone(),
            session: agent.owner.session,
            req: Request::AgentRelease {
                agent: agent.id.clone(),
                reason: Some(GONE.into()),
            },
            inbox: None,
        });
    }
    (acts, tick)
}

/// Run one tick's acts against the core.
///
/// A refusal is the agent having gone away between the snapshot and the
/// send, which is the outcome the act asked for anyway; it is logged at
/// debug with its code and nothing else.
pub async fn apply(api: &Api, acts: Vec<Act>) {
    // The agent the last seeding register resolved: a `Reply` queued right
    // behind it carries the placeholder id and is addressed here, the way
    // `api::hooks::run` addresses a hook plan.
    let mut registered: Option<AgentId> = None;
    for act in acts {
        let Act {
            principal,
            session,
            mut req,
            inbox,
        } = act;
        let is_register = matches!(req, Request::AgentRegister(_));
        if !is_register {
            if let Some(agent) = &registered {
                crate::adapters::address(&mut req, agent);
            }
            if crate::adapters::agent_of(&req).is_some_and(crate::adapters::is_pending) {
                continue; // a reply with no register in front of it
            }
        }
        match api.core.api(principal, session, false, req).await {
            Ok(result) => {
                if is_register {
                    registered = result["agent"].as_str().map(|id| AgentId(id.to_string()));
                    if let (Some(agent), Some(inbox)) = (&registered, inbox) {
                        api.core.adapters().set_inbox(agent.clone(), inbox);
                    }
                }
            }
            Err(e) => {
                if is_register {
                    registered = None;
                }
                tracing::debug!(code = e.code, "a liveness act was refused");
            }
        }
    }
}

/// One roster tick: probe on `spawn_blocking` (C9), reconcile, apply.
pub async fn roster_tick(api: &Api, probe: &Probe, misses: &mut Misses, seed: bool) -> Tick {
    let probe = probe.clone();
    let rows = match tokio::task::spawn_blocking(move || probe.rows()).await {
        Ok(Ok(rows)) => rows,
        Ok(Err(why)) => {
            tracing::debug!(why, "the roster probe was inconclusive");
            return Tick {
                inconclusive: true,
                ..Tick::default()
            };
        }
        Err(_) => {
            return Tick {
                inconclusive: true,
                ..Tick::default()
            }
        }
    };
    let snapshot = api.bus.snapshot();
    let mut rows = rows;
    let total = rows.len();
    let mut owner = api.adapters.claude_session();
    if seed {
        let known: BTreeSet<String> = snapshot
            .agents
            .values()
            .filter_map(claude_session)
            .map(str::to_string)
            .collect();
        let unknown: Vec<&mut Row> = rows
            .iter_mut()
            .filter(|row| {
                row.alive()
                    && row
                        .session_id
                        .as_deref()
                        .is_some_and(|id| !known.contains(id))
            })
            .collect();
        // The seed speaks as the Claude adapter, from its internal
        // session. A daemon started after its CLI sessions — the case the
        // seed exists for — has seen no hook yet, so no hook has opened
        // that session: open it here, exactly as a hook would, or there is
        // nobody to seed as.
        if owner.is_none() && !unknown.is_empty() {
            owner = api.adapters.claude_session_or_open(&api.core).await;
            if owner.is_none() {
                tracing::warn!(
                    unknown = unknown.len(),
                    "the claude adapter's session could not be opened: nothing seeded this tick"
                );
            }
        }
        tracing::debug!(
            rows = total,
            unknown = unknown.len(),
            owner = owner.is_some(),
            "roster tick"
        );
        if let Some(dir) = claude_dir() {
            // File reads for a handful of sessions, on the blocking pool
            // like the probe: a transcript tail is 256 KB at most.
            tokio::task::block_in_place(|| enrich(unknown, &dir));
        }
    }
    let owner = owner.map(|session| (Principal::Adapter(AdapterKind::Claude), session));
    let (acts, tick) = reconcile(
        &snapshot,
        &rows,
        misses,
        owner.as_ref().map(|(p, s)| (p, *s)),
        seed,
    );
    apply(api, acts).await;
    if let Some((principal, session)) = owner {
        let current = api.bus.snapshot();
        if let Ok(mut watches) = api.adapters.transcripts.lock() {
            watches.retain(|id, _| current.agents.contains_key(id));
        }
        for agent in current.agents.values().filter(|a| a.owner.principal == principal && a.owner.session == session) {
            crate::chat::seed(&api.adapters, &api.core, session, agent);
        }
    }
    tick
}

/// One pid tick: `kill(pid, 0)` for the resolved CLI pids, and nothing
/// else (B1).
pub async fn pid_tick(api: &Api) -> Tick {
    let snapshot = api.bus.snapshot();
    let (acts, tick) = sweep(&snapshot, alive);
    apply(api, acts).await;
    tick
}

/// Both pollers, until `stop`. One task: the two periods are 5 s and 2 s
/// and neither tick ever blocks the other (the roster probe is on the
/// blocking pool), so a second task would buy nothing.
pub async fn run(api: Arc<Api>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut misses = Misses::new();
    let mut roster = tokio::time::interval(Duration::from_secs(
        api.config.load().liveness.roster_s.max(1).into(),
    ));
    let mut pids = tokio::time::interval(Duration::from_secs(
        api.config.load().liveness.pid_s.max(1).into(),
    ));
    roster.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    pids.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Logged on change only: an idle daemon says so once, not every 5 s.
    let mut was_idle: Option<bool> = None;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = roster.tick() => {
                let cfg = api.config.load().liveness.clone();
                if !cfg.enabled {
                    continue;
                }
                // Nothing to reconcile and nothing to seed: do not spawn
                // the CLI at all. A daemon with no Claude session on the
                // roster costs no processes.
                let on_roster = api
                    .bus
                    .snapshot()
                    .agents
                    .values()
                    .any(|a| claude_session(a).is_some());
                // With seeding on, a probe is still skipped while Claude's
                // own session registry is empty: `claude agents --json`
                // reads `<claude dir>/sessions/*.json`, and an empty
                // directory is a free "no sessions" answer that costs no
                // process. (The directory alone is not trusted for
                // liveness — a crashed CLI leaves its file behind — so a
                // non-empty one still runs the real probe.)
                let idle = !on_roster
                    && (!cfg.seed || !claude_dir().is_some_and(|dir| sessions_registered(&dir)));
                if was_idle != Some(idle) {
                    was_idle = Some(idle);
                    tracing::debug!(
                        idle,
                        on_roster,
                        seed = cfg.seed,
                        "roster poll {}",
                        if idle { "paused: nothing to reconcile or seed" } else { "running" }
                    );
                }
                if idle {
                    misses.clear();
                    continue;
                }
                let probe = Probe::new(cfg.claude_bin.clone());
                let tick = roster_tick(&api, &probe, &mut misses, cfg.seed).await;
                if !tick.released.is_empty() || !tick.seeded.is_empty() {
                    tracing::info!(
                        seen = tick.seen,
                        released = tick.released.len(),
                        seeded = tick.seeded.len(),
                        "the claude roster was reconciled"
                    );
                }
            }
            _ = pids.tick() => {
                if !api.config.load().liveness.enabled {
                    continue;
                }
                let tick = pid_tick(&api).await;
                if !tick.released.is_empty() {
                    tracing::info!(
                        polled = tick.polled.len(),
                        released = tick.released.len(),
                        "a resolved cli pid is gone"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{Owner, SessionInfo, Ts};

    const WALL: Ts = Ts(1_789_760_523_000);

    fn snapshot(agents: Vec<Agent>) -> Snapshot {
        Snapshot {
            agents: agents.into_iter().map(|a| (a.id.clone(), a)).collect(),
            ..Snapshot::default()
        }
    }

    fn agent(id: &str, key: Option<&str>) -> Agent {
        Agent {
            id: AgentId(id.into()),
            slot: None,
            kind: Kind::Claude,
            name: id.into(),
            state: AgentState::Idle,
            title: None,
            title_source: deck_model::TitleSource::Cwd,
            since: WALL,
            source: deck_model::Source::Hook,
            owner: Owner {
                principal: Principal::Adapter(AdapterKind::Claude),
                session: SessionId(9),
                lease_expires: None,
            },
            session: SessionInfo {
                key: key.map(str::to_string),
                cwd: None,
                pid: None,
                pid_kind: None,
                terminal: None,
            },
            caps: Caps::default(),
            companion: None,
            card: None,
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

    fn row(session_id: &str, status: &str) -> Row {
        Row {
            pid: Some(4242),
            kind: Some("interactive".into()),
            session_id: Some(session_id.into()),
            name: Some("openwalkietalkie-92".into()),
            cwd: Some("/Users/x/p".into()),
            status: Some(status.into()),
            waiting_for: None,
            state: None,
            ..Row::default()
        }
    }

    /// The measured payload of facts-claude §4.1 parses as it stands, with
    /// no `state`, no `waitingFor` and no `id` (C9).
    #[test]
    fn the_measured_interactive_rows_parse_and_key_off_status() {
        let rows: Vec<Row> = serde_json::from_str(
            r#"[
              {"pid":43127,"cwd":"/Users/tony/projects/conol/openwalkietalkie","kind":"interactive",
               "startedAt":1789629157246,"sessionId":"753eb9de-97f0-480e-9319-617fbf0a5d73",
               "name":"openwalkietalkie-92","status":"busy"},
              {"pid":70827,"cwd":"/Users/tony/projects/conol/openwalkietalkie","kind":"interactive",
               "startedAt":1789648056873,"sessionId":"6129ffce-0642-438d-9251-fbcad35223ec",
               "name":"openwalkietalkie-eb","status":"idle"}
            ]"#,
        )
        .expect("the measured shape");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.state.is_none() && r.alive()));
        assert_eq!(rows[0].agent_state(), Some(AgentState::Working));
        assert_eq!(rows[1].agent_state(), Some(AgentState::Idle));
        // `startedAt` is an unknown field here and is ignored, not refused.
        assert_eq!(rows[0].pid, Some(43127));
        // `waiting` is the CLI's own dialog: the deck does not repaint it.
        let mut waiting = row("s", "waiting");
        waiting.waiting_for = Some("permission prompt".into());
        assert_eq!(waiting.agent_state(), None);
        assert!(waiting.alive());
        // A background row states its outcome in `state`, never `status`.
        let done = Row {
            pid: None,
            kind: Some("background".into()),
            session_id: Some("bg".into()),
            state: Some("done".into()),
            ..Row::default()
        };
        assert!(!done.alive());
        let working = Row {
            kind: Some("background".into()),
            state: Some("working".into()),
            pid: Some(3),
            ..Row::default()
        };
        assert!(working.alive());
        assert_eq!(working.agent_state(), Some(AgentState::Working));
        // Nothing at all: not a live session.
        assert!(!Row::default().alive());
    }

    /// Two strikes before a release, and a probe that cannot be trusted
    /// costs nobody a strike.
    #[test]
    fn a_session_must_be_missing_twice_before_its_agent_is_released() {
        let snap = snapshot(vec![agent("ag_1", Some("claude:s1"))]);
        let mut misses = Misses::new();
        let (acts, tick) = reconcile(&snap, &[], &mut misses, None, false);
        assert!(acts.is_empty() && tick.released.is_empty(), "{tick:?}");
        assert_eq!(misses.get(&AgentId("ag_1".into())), Some(&1));
        let (acts, tick) = reconcile(&snap, &[], &mut misses, None, false);
        assert_eq!(tick.released, vec![AgentId("ag_1".into())]);
        assert!(matches!(
            acts.first().map(|a| &a.req),
            Some(Request::AgentRelease { reason, .. }) if reason.as_deref() == Some(GONE)
        ));
        // The act speaks as the agent's own owner (B3), never as anybody else.
        assert_eq!(acts[0].session, SessionId(9));
        assert_eq!(acts[0].principal, Principal::Adapter(AdapterKind::Claude));
        assert!(misses.is_empty(), "a released agent keeps no strike");
        // One sighting clears the count.
        let mut misses = Misses::new();
        reconcile(&snap, &[], &mut misses, None, false);
        let (_, tick) = reconcile(&snap, &[row("s1", "busy")], &mut misses, None, false);
        assert!(misses.is_empty(), "{tick:?}");
        assert_eq!(tick.seen, 1);
    }

    /// Only an agent this adapter owns under a `claude:` key is governed,
    /// and only a field that actually differs is written.
    #[test]
    fn the_roster_corrects_state_and_name_and_leaves_everything_else_alone() {
        let mut mine = agent("ag_1", Some("claude:s1"));
        mine.name = "old".into();
        let mut foreign = agent("ag_2", Some("claude:s2"));
        foreign.owner.principal = Principal::Uid(501);
        let mut keyless = agent("ag_3", None);
        keyless.name = "virtual".into();
        // A card is on the deck: the roster's `idle` must not repaint it,
        // and with the name already agreed nothing at all is written.
        let mut carded = agent("ag_4", Some("claude:s4"));
        carded.state = AgentState::Question;
        carded.name = "openwalkietalkie-92".into();
        let snap = snapshot(vec![mine, foreign, keyless, carded]);
        let mut misses = Misses::new();
        let rows = vec![row("s1", "busy"), row("s2", "busy"), row("s4", "idle")];
        let (acts, tick) = reconcile(&snap, &rows, &mut misses, None, false);
        assert_eq!(tick.updated, vec![AgentId("ag_1".into())], "{tick:?}");
        assert_eq!(acts.len(), 1);
        let Request::AgentUpdate(update) = &acts[0].req else {
            panic!("an update");
        };
        assert_eq!(update.state, Some(AgentState::Working));
        assert_eq!(update.name.as_deref(), Some("openwalkietalkie-92"));
        // A second tick with the same rows writes nothing: the deck agrees.
        let mut agreed = agent("ag_1", Some("claude:s1"));
        agreed.name = "openwalkietalkie-92".into();
        agreed.state = AgentState::Working;
        let snap = snapshot(vec![agreed]);
        let (acts, tick) = reconcile(&snap, &rows, &mut misses, None, false);
        assert!(acts.is_empty() && tick.updated.is_empty(), "{tick:?}");
        // Neither the foreign-principal agent nor the keyless one is
        // counted a strike when its row disappears.
        let (acts, _) = reconcile(&snapshot(vec![]), &[], &mut misses, None, false);
        assert!(acts.is_empty());
    }

    /// `[liveness] seed` registers only interactive sessions the deck does
    /// not have, as the adapter itself — and nothing at all when it is off.
    #[test]
    fn seeding_is_only_for_unknown_interactive_sessions_and_only_when_on() {
        let snap = snapshot(vec![agent("ag_1", Some("claude:s1"))]);
        let rows = vec![
            row("s1", "busy"),
            row("s2", "idle"),
            Row {
                kind: Some("background".into()),
                session_id: Some("s3".into()),
                pid: Some(7),
                state: Some("working".into()),
                ..Row::default()
            },
        ];
        let mut misses = Misses::new();
        let owner = Principal::Adapter(AdapterKind::Claude);
        let (acts, tick) = reconcile(
            &snap,
            &rows,
            &mut misses,
            Some((&owner, SessionId(9))),
            false,
        );
        assert!(tick.seeded.is_empty(), "seed off: nothing registered");
        assert!(acts
            .iter()
            .all(|a| !matches!(a.req, Request::AgentRegister(_))));
        let (acts, tick) = reconcile(
            &snap,
            &rows,
            &mut misses,
            Some((&owner, SessionId(9))),
            true,
        );
        assert_eq!(tick.seeded, vec!["s2".to_string()], "{tick:?}");
        let Some(Request::AgentRegister(register)) = acts
            .iter()
            .find(|a| matches!(a.req, Request::AgentRegister(_)))
            .map(|a| a.req.clone())
        else {
            panic!("a register");
        };
        assert_eq!(register.key.as_deref(), Some("claude:s2"));
        assert_eq!(register.pid_kind, Some(PidKind::Cli));
        assert_eq!(register.lease_ttl_s, 0);
        assert!(
            register.terminal.is_none(),
            "an unenriched row knows no terminal and no inbox"
        );
        // With no adapter session open there is nothing to seed as.
        let (_, tick) = reconcile(&snap, &rows, &mut misses, None, true);
        assert!(tick.seeded.is_empty());
    }

    /// The pid sweep probes `cli` pids and nothing else (B1).
    #[test]
    fn the_sweep_probes_only_resolved_cli_pids() {
        let mut cli = agent("ag_cli", Some("claude:s1"));
        cli.session.pid = Some(4242);
        cli.session.pid_kind = Some(PidKind::Cli);
        let mut shell = agent("ag_shell", Some("claude:s2"));
        shell.session.pid = Some(4243);
        shell.session.pid_kind = Some(PidKind::Shell);
        let mut unknown = agent("ag_unknown", Some("claude:s3"));
        unknown.session.pid = Some(4244);
        unknown.session.pid_kind = Some(PidKind::Unknown);
        let mut unset = agent("ag_unset", Some("claude:s4"));
        unset.session.pid = Some(4245);
        let mut pidless = agent("ag_pidless", Some("claude:s5"));
        pidless.session.pid_kind = Some(PidKind::Cli);
        let mut token = agent("ag_token", Some("claude:s6"));
        token.owner.principal = Principal::Uid(501);
        token.session.pid = Some(4246);
        token.session.pid_kind = Some(PidKind::Cli);
        let snap = snapshot(vec![cli, shell, unknown, unset, pidless, token]);
        let asked = std::sync::Mutex::new(Vec::new());
        let (acts, tick) = sweep(&snap, |pid| {
            asked.lock().unwrap().push(pid);
            false
        });
        assert_eq!(tick.polled, vec![AgentId("ag_cli".into())], "{tick:?}");
        assert_eq!(*asked.lock().unwrap(), vec![4242]);
        assert_eq!(tick.released, vec![AgentId("ag_cli".into())]);
        assert_eq!(acts.len(), 1);
        // A live pid is left alone.
        let (acts, tick) = sweep(&snap, |_| true);
        assert!(acts.is_empty() && tick.released.is_empty(), "{tick:?}");
        assert_eq!(tick.polled.len(), 1);
    }

    /// `kill(pid, 0)`: this process is alive, pid 0 is never a CLI, and a
    /// pid that cannot exist is dead.
    #[test]
    fn aliveness_is_kill_zero_with_eperm_counted_as_alive() {
        assert!(alive(std::process::id()));
        assert!(!alive(0));
        assert!(!alive(u32::MAX));
        // launchd is pid 1 and belongs to root: EPERM, which is alive.
        assert!(alive(1));
    }

    /// A probe that cannot run is inconclusive, never "no sessions".
    #[test]
    fn a_probe_that_cannot_run_is_inconclusive() {
        let probe = Probe::on_path("claude", "/nonexistent-deckd-probe");
        let why = probe.rows().expect_err("no such binary");
        assert!(!why.is_empty());
        // A binary that prints something else is inconclusive too.
        let probe = Probe::new("/bin/echo");
        assert!(probe.rows().is_err());
    }
}
