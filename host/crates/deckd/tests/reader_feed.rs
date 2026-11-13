//! R6, the lazy reader feed (`deckd::reader`), end to end on the Claude
//! side: an in-process daemon with dormancy on and a deck linked, two Claude
//! sessions reporting through the hook door, and each session's transcript
//! under a `CLAUDE_CONFIG_DIR` of the test's own.
//!
//! Neither session is read at first, so neither gets a tool row or a line of
//! its transcript; a `channel` line from the deck fills the one it opens,
//! in order and with nothing of the other; a permission card fills the
//! other; and the session the deck turned away from is still read for the
//! linger. A follower that starts after the deck opened a quiet session
//! fills it at once.
//!
//! A process of its own, because `CLAUDE_CONFIG_DIR` is process-global: it
//! is set once, to a temp dir, before any daemon starts. Nothing here reads
//! the operator's own `~/.claude`.
mod common;

use common::{Daemon, Options, PATIENCE};
use deck_model::{AgentId, AgentState, DeviceLine, Scope, Snapshot};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The Claude config dir every transcript of this binary is under.
fn claude_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("dkrf{}-claude", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("projects")).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        dir
    })
    .clone()
}

/// One Claude session: its id, working directory and transcript.
struct Session {
    id: &'static str,
    cwd: &'static str,
    transcript: PathBuf,
}

impl Session {
    fn new(id: &'static str, cwd: &'static str) -> Session {
        let slug: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = claude_dir().join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join(format!("{id}.jsonl"));
        std::fs::write(&transcript, "").unwrap();
        Session {
            id,
            cwd,
            transcript,
        }
    }

    /// A hook of this session, `fields` merged in.
    fn hook(&self, event: &str, fields: serde_json::Value) -> serde_json::Value {
        let mut payload = json!({
            "session_id": self.id,
            "transcript_path": self.transcript,
            "cwd": self.cwd,
            "permission_mode": "default",
            "hook_event_name": event,
        });
        for (key, value) in fields.as_object().unwrap() {
            payload[key] = value.clone();
        }
        payload
    }

    /// Append a user prompt, an assistant answer with a tool call, and the
    /// call's result, all named after `tag`: the messages `tag-u`, `tag-a`
    /// and `tool:tag-t`, in that order.
    fn write_turn(&self, tag: &str) {
        let lines = [
            json!({"type": "user", "uuid": format!("{tag}-u"), "sessionId": self.id,
                "message": {"content": format!("Prompt {tag}")}}),
            json!({"type": "assistant", "uuid": format!("{tag}-a"), "sessionId": self.id,
                "message": {"stop_reason": "tool_use", "content": [
                    {"type": "text", "text": format!("Answer {tag}")},
                    {"type": "tool_use", "id": format!("{tag}-t"), "name": "Bash", "input": {"command": "ls"}}]}}),
            json!({"type": "user", "uuid": format!("{tag}-r"), "sessionId": self.id,
                "message": {"content": [{"type": "tool_result", "tool_use_id": format!("{tag}-t"), "content": "a\nb"}]}}),
        ];
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .unwrap();
        for line in lines {
            std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
        }
    }
}

async fn hook(daemon: &Daemon, auth: &str, payload: &serde_json::Value) {
    let (status, _) = daemon
        .http(
            "POST",
            "/v1/hooks/claude",
            &[("Authorization", auth)],
            &payload.to_string(),
        )
        .await;
    assert_eq!(status, 200);
}

/// The agent a Claude session is on the roster as.
async fn agent_of(daemon: &Daemon, session: &Session) -> AgentId {
    let key = format!("claude:{}", session.id);
    daemon
        .until("the session is on the roster", |s| {
            s.agents
                .values()
                .any(|a| a.session.key.as_deref() == Some(&key))
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    let agent = snapshot
        .agents
        .values()
        .find(|a| a.session.key.as_deref() == Some(&key));
    agent.unwrap().id.clone()
}

fn ids(snapshot: &Snapshot, agent: &AgentId) -> Vec<String> {
    snapshot.agents[agent]
        .messages
        .iter()
        .map(|m| m.message.id.clone())
        .collect()
}

fn names(tags: &[&str]) -> Vec<String> {
    tags.iter()
        .flat_map(|tag| {
            [
                format!("{tag}-u"),
                format!("{tag}-a"),
                format!("tool:{tag}-t"),
            ]
        })
        .collect()
}

/// Until the agent's history is exactly `want`, in order.
async fn history_is(daemon: &Daemon, agent: &AgentId, want: &[String]) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let seen = ids(&daemon.api.bus.snapshot(), agent);
        if seen == want {
            return;
        }
        assert!(Instant::now() < deadline, "history {seen:?}, want {want:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_sessions_the_deck_reads_are_fed_and_each_is_filled_whole_when_it_is() {
    claude_dir();
    let daemon = Daemon::with(Options {
        lazy: true,
        ..Options::default()
    })
    .await;
    daemon
        .until("the deck is linked and the collectors run", |s| {
            s.service.dormancy.is_some_and(|d| d.collecting)
        })
        .await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let follower = tokio::spawn(deckd::reader::follow(Arc::clone(&daemon.api), stopped));
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let a = Session::new("sess-a", "/w/alpha");
    let b = Session::new("sess-b", "/w/beta");
    a.write_turn("a1");
    b.write_turn("b1");

    // Two sessions at work, neither read: no transcript, no tool row.
    hook(
        &daemon,
        &auth,
        &a.hook("UserPromptSubmit", json!({"prompt": "Prompt a1"})),
    )
    .await;
    hook(
        &daemon,
        &auth,
        &b.hook("UserPromptSubmit", json!({"prompt": "Prompt b1"})),
    )
    .await;
    let (agent_a, agent_b) = (agent_of(&daemon, &a).await, agent_of(&daemon, &b).await);
    let tool = json!({"tool_name": "Bash", "tool_use_id": "a-hook", "tool_input": {"command": "ls"},
        "tool_response": "a\nb"});
    hook(&daemon, &auth, &a.hook("PostToolUse", tool)).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let snapshot = daemon.api.bus.snapshot();
    assert_eq!(snapshot.agents[&agent_a].state, AgentState::Working);
    assert!(ids(&snapshot, &agent_a).is_empty(), "A is not read");
    assert!(ids(&snapshot, &agent_b).is_empty(), "B is not read");

    // The deck opens A: A's transcript, in order, and nothing of B.
    let slot_a = snapshot.agents[&agent_a].slot.expect("A is on the deck");
    daemon.device_input(DeviceLine::Channel {
        slot: slot_a.0,
        seq: 1,
    });
    history_is(&daemon, &agent_a, &names(&["a1"])).await;
    assert!(
        ids(&daemon.api.bus.snapshot(), &agent_b).is_empty(),
        "B untouched"
    );
    // A is followed from now on.
    a.write_turn("a2");
    history_is(&daemon, &agent_a, &names(&["a1", "a2"])).await;

    // A permission card on B, the deck still on A: B holds a card, so B is
    // read too.
    let ask = b.hook(
        "PermissionRequest",
        json!({"tool_name": "Bash", "tool_input": {"command": "date", "description": "Print the date"}}),
    );
    let held = {
        let (port, auth) = (daemon.port, auth.clone());
        tokio::spawn(async move {
            common::http(
                port,
                "POST",
                "/v1/hooks/claude",
                &[("Authorization", &auth)],
                &ask.to_string(),
            )
            .await
        })
    };
    history_is(&daemon, &agent_b, &names(&["b1"])).await;

    // The deck turns to B. A is still read for the linger: what it writes
    // still arrives.
    let slot_b = daemon.api.bus.snapshot().agents[&agent_b]
        .slot
        .expect("B is on the deck");
    daemon.device_input(DeviceLine::Channel {
        slot: slot_b.0,
        seq: 2,
    });
    a.write_turn("a3");
    history_is(&daemon, &agent_a, &names(&["a1", "a2", "a3"])).await;

    let _ = stop.send(true);
    follower.await.unwrap();
    held.abort();
    daemon.shutdown().await;
}

/// The follower acts on the reader as it finds it, not only on the next
/// change: a daemon's core publishes the reader before the follower
/// starts. A quiet session the deck already has open (no hook to start a
/// watcher) is filled as soon as the follower runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follower_that_starts_late_fills_the_session_already_open() {
    claude_dir();
    let daemon = Daemon::with(Options {
        lazy: true,
        ..Options::default()
    })
    .await;
    daemon
        .until("the deck is linked and the collectors run", |s| {
            s.service.dormancy.is_some_and(|d| d.collecting)
        })
        .await;
    let auth = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let c = Session::new("sess-c", "/w/gamma");
    c.write_turn("c1");
    hook(
        &daemon,
        &auth,
        &c.hook("UserPromptSubmit", json!({"prompt": "Prompt c1"})),
    )
    .await;
    let agent_c = agent_of(&daemon, &c).await;
    let slot = daemon.api.bus.snapshot().agents[&agent_c]
        .slot
        .expect("C is on the deck");
    daemon.device_input(DeviceLine::Channel {
        slot: slot.0,
        seq: 1,
    });
    daemon
        .until("the deck reads C", |_| {
            daemon.api.core.adapters().reader().agents() == Some(&[agent_c.clone()].into())
        })
        .await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(
        ids(&daemon.api.bus.snapshot(), &agent_c).is_empty(),
        "no hook since, and no follower yet"
    );

    let (stop, stopped) = tokio::sync::watch::channel(false);
    let follower = tokio::spawn(deckd::reader::follow(Arc::clone(&daemon.api), stopped));
    history_is(&daemon, &agent_c, &names(&["c1"])).await;
    let _ = stop.send(true);
    follower.await.unwrap();
    daemon.shutdown().await;
}
