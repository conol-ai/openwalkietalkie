//! The Codex adapter end to end (contract §6 rows 7–8): recorded hook
//! payloads in, cards and roster entries out, and the exact JSON Codex
//! reads back on stdout.
//!
//! Every payload is replayed from `tests/fixtures/hooks/codex/*.json`,
//! captured from `codex-cli 0.153.4` and **scrubbed** before it was
//! committed — the scrubbing is asserted here, not assumed (contract §9:
//! hook payloads are the most sensitive data on the box).
//!
//! The daemon is in-process with a temp data dir under `$TMPDIR`; nothing
//! here reads or writes the user's `~/.codex`, and no real `codex` is run.
mod common;

use common::{Daemon, Options};
use deck_model::Scope;
use std::path::PathBuf;

/// The CLI session id every fixture carries, and the roster key it makes.
const SESSION: &str = "01a0bca9-1344-7d20-a793-10903639be67";
const KEY: &str = "codex:01a0bca9-1344-7d20-a793-10903639be67";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks/codex")
}

/// One recorded payload.
fn fixture(name: &str) -> serde_json::Value {
    let path = fixtures().join(format!("{name}.json"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

/// A `hooks.<method>` JSON-RPC body carrying one payload, shaped exactly
/// as `deckd hook` sends it.
fn call(method: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method,
        "params": {
            "payload": payload,
            "env": {"TERM_PROGRAM": "ghostty", "TMUX_PANE": "%3"},
            "tty": "/dev/ttys004",
            "cli_pid": 4242,
            "pid_kind": "cli"
        }
    })
    .to_string()
}

/// The agent id and card of the one Codex session on the roster.
fn one_agent(daemon: &Daemon) -> (String, Option<deck_model::Card>) {
    let snapshot = daemon.api.bus.snapshot();
    let agent = snapshot
        .agents
        .values()
        .find(|a| a.session.key.as_deref() == Some(KEY))
        .unwrap_or_else(|| panic!("the codex session is on the roster"));
    (agent.id.0.clone(), agent.card.clone())
}

async fn started(daemon: &Daemon, auth: &str) {
    let out = daemon
        .rpc(auth, &call("hooks.codex", fixture("session-start")))
        .await;
    assert_eq!(out["result"], serde_json::json!({}), "{out}");
    daemon
        .until("the codex session is on the roster", |s| {
            s.agents
                .values()
                .any(|a| a.session.key.as_deref() == Some(KEY))
        })
        .await;
}

fn untrusted() -> Options {
    Options {
        verifier: deckd::trust::FakeVerifier {
            trusted: false,
            capped: false,
        },
        ..Options::default()
    }
}

/// Even without an app-server connection, the hook must return before any
/// user answer so Codex can draw its native approval dialog.
#[tokio::test]
async fn permission_fallback_returns_without_waiting_for_an_answer() {
    let daemon = Daemon::with(untrusted()).await;
    let hooks = daemon.bearer("hooks", &[Scope::HooksIngest]);
    started(&daemon, &hooks).await;
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        daemon.rpc(&hooks, &call("hooks.codex", fixture("permission-request"))),
    )
    .await
    .expect("permission hook must not wait for an answer");
    assert_eq!(reply["result"], serde_json::json!({}));
    daemon
        .until("attention is reported", |s| {
            s.agents.values().any(|a| {
                a.session.key.as_deref() == Some(KEY) && a.state == deck_model::AgentState::Question
            })
        })
        .await;
    let (_, card) = one_agent(&daemon);
    assert!(
        card.is_none(),
        "no unusable Allow/Deny card without an app-server"
    );
    daemon
        .rpc(&hooks, &call("hooks.codex", fixture("pre-tool-use")))
        .await;
    daemon
        .until("Codex continued after its native answer", |s| {
            s.agents.values().any(|a| {
                a.session.key.as_deref() == Some(KEY) && a.state == deck_model::AgentState::Working
            })
        })
        .await;
    daemon.shutdown().await;
}

/// Contract §6 row 8. `Stop` and `notify agent-turn-complete` are the
/// same turn seen through two doors (C17: `notify` is a single command
/// and is already occupied on this machine, so the `Stop` hook is the
/// primary path and `notify` the redundancy). The second one to arrive is
/// a no-op, keyed on the **pair** `(session_id, turn_id)`.
#[tokio::test]
async fn the_notify_payload_is_deduped_against_the_stop_hook() {
    let daemon = Daemon::with(untrusted()).await;
    let hooks = daemon.bearer("hooks", &[Scope::HooksIngest]);
    started(&daemon, &hooks).await;

    // 1. The `Stop` hook reports the turn.
    let stopped = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("stop")))
        .await;
    assert_eq!(stopped["result"], serde_json::json!({}), "{stopped}");
    daemon
        .until("the turn is reported ready", |s| {
            s.agents.values().any(|a| {
                a.state == deck_model::AgentState::Ready
                    && a.reply.as_ref().and_then(|r| r.text.as_deref())
                        == Some("REDACTED assistant message")
            })
        })
        .await;

    // 2. A new prompt moves the session back to `working`, so a second
    //    report of the same turn would be visible as a state change.
    let prompted = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("user-prompt-submit")))
        .await;
    assert_eq!(prompted["result"], serde_json::json!({}), "{prompted}");
    daemon
        .until("the session is working again", |s| {
            s.agents
                .values()
                .any(|a| a.state == deck_model::AgentState::Working)
        })
        .await;

    // 3. `notify` for the *same* turn: answered, and nothing enqueued.
    let mut duplicate = fixture("notify-agent-turn-complete");
    duplicate["last-assistant-message"] = "the duplicate".into();
    let ignored = daemon
        .rpc(&hooks, &call("hooks.codex_notify", duplicate))
        .await;
    assert_eq!(ignored["result"], serde_json::json!({}), "{ignored}");

    // 4. `notify` for the *next* turn is a real report. The queue is
    //    serial, so once this one lands the duplicate has had its chance.
    let mut next = fixture("notify-agent-turn-complete");
    next["turn-id"] = "01a0bcae-0000-7042-8668-78526755dd62".into();
    next["last-assistant-message"] = "the second turn".into();
    let reported = daemon.rpc(&hooks, &call("hooks.codex_notify", next)).await;
    assert_eq!(reported["result"], serde_json::json!({}), "{reported}");
    daemon
        .until("the second turn is reported", |s| {
            s.agents.values().any(|a| {
                a.reply.as_ref().and_then(|r| r.text.as_deref()) == Some("the second turn")
            })
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    for agent in snapshot.agents.values() {
        assert_ne!(
            agent.reply.as_ref().and_then(|r| r.text.as_deref()),
            Some("the duplicate"),
            "the duplicate turn was reported twice"
        );
    }
    drop(snapshot);
    daemon.shutdown().await;
}

/// The events this build does not act on answer `{}` and touch nothing —
/// C10's discipline, which is what keeps a Codex release that adds a
/// thirteenth event from taking a tool call down with it.
#[tokio::test]
async fn an_event_this_adapter_does_not_know_answers_empty_and_changes_nothing() {
    let daemon = Daemon::with(untrusted()).await;
    let hooks = daemon.bearer("hooks", &[Scope::HooksIngest]);
    started(&daemon, &hooks).await;
    // Put the session somewhere an unknown event could visibly move it
    // from, so "nothing changed" is an assertion and not a tautology.
    let prompted = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("user-prompt-submit")))
        .await;
    assert_eq!(prompted["result"], serde_json::json!({}), "{prompted}");
    daemon
        .until("the session is working", |s| {
            s.agents
                .values()
                .any(|a| a.state == deck_model::AgentState::Working)
        })
        .await;

    // `PostToolUse` is a real Codex event this build does not act on, and
    // the second one is from a Codex that does not exist yet.
    let mut future = fixture("post-tool-use");
    future["hook_event_name"] = "SomethingNewInTheNextRelease".into();
    for payload in [fixture("post-tool-use"), future] {
        let out = daemon.rpc(&hooks, &call("hooks.codex", payload)).await;
        assert_eq!(out["result"], serde_json::json!({}), "{out}");
    }
    let snapshot = daemon.api.bus.snapshot();
    assert!(
        snapshot
            .agents
            .values()
            .all(|a| a.state == deck_model::AgentState::Working),
        "an event we do not know moved the session"
    );
    drop(snapshot);

    // `Interrupt` *is* known: it is the one that must change something.
    let out = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("interrupt")))
        .await;
    assert_eq!(out["result"], serde_json::json!({}), "{out}");
    daemon
        .until("the interrupted session is idle", |s| {
            s.agents
                .values()
                .any(|a| a.state == deck_model::AgentState::Idle)
        })
        .await;
    daemon.shutdown().await;
}

/// `SessionEnd` releases the agent, and a `SessionEnd` for a session the
/// daemon never saw does not put one on the roster on its way out.
#[tokio::test]
async fn session_end_releases_the_agent_and_never_resurrects_an_unknown_one() {
    let daemon = Daemon::with(untrusted()).await;
    let hooks = daemon.bearer("hooks", &[Scope::HooksIngest]);

    // Unknown session first: nothing is registered.
    let out = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("session-end")))
        .await;
    assert_eq!(out["result"], serde_json::json!({}), "{out}");
    assert!(
        daemon.api.bus.snapshot().agents.is_empty(),
        "a SessionEnd never creates an agent"
    );

    started(&daemon, &hooks).await;
    let out = daemon
        .rpc(&hooks, &call("hooks.codex", fixture("session-end")))
        .await;
    assert_eq!(out["result"], serde_json::json!({}), "{out}");
    daemon
        .until("the agent is released", |s| {
            !s.agents
                .values()
                .any(|a| a.session.key.as_deref() == Some(KEY))
        })
        .await;
    daemon.shutdown().await;
}

/// The hand-off's input: the hook that puts a session on the roster carries
/// the app its helper found, and it is registered on the session's
/// terminal, which is what a press on the session's `Open app` card raises.
/// A value that does not look like a bundle identifier is dropped at the
/// door, and the event still lands.
#[tokio::test]
async fn a_codex_hook_session_records_its_app() {
    let daemon = Daemon::with(untrusted()).await;
    let hooks = daemon.bearer("hooks", &[Scope::HooksIngest]);
    let start = |session: &str, app: &str| {
        let mut payload = fixture("session-start");
        payload["session_id"] = session.into();
        let mut body: serde_json::Value =
            serde_json::from_str(&call("hooks.codex", payload)).unwrap();
        body["params"]["app"] = app.into();
        body.to_string()
    };
    let terminal_of = |s: &deck_model::Snapshot, session: &str| {
        let key = format!("codex:{session}");
        s.agents
            .values()
            .find(|a| a.session.key.as_deref() == Some(key.as_str()))
            .map(|a| a.session.terminal.clone().unwrap_or_default())
    };
    for (session, app) in [
        ("s-forged", "-b com.evil"),
        ("s-ghostty", "com.mitchellh.ghostty"),
    ] {
        let out = daemon.rpc(&hooks, &start(session, app)).await;
        assert_eq!(out["result"], serde_json::json!({}), "{out}");
    }
    daemon
        .until("both sessions land", |s| {
            terminal_of(s, "s-forged").is_some() && terminal_of(s, "s-ghostty").is_some()
        })
        .await;
    let snapshot = daemon.api.bus.snapshot();
    let forged = terminal_of(&snapshot, "s-forged").unwrap();
    assert_eq!(forged.app, None, "dropped at the door");
    assert_eq!(
        forged.tty.as_deref(),
        Some("/dev/ttys004"),
        "the rest is kept"
    );
    assert_eq!(
        terminal_of(&snapshot, "s-ghostty").unwrap().app.as_deref(),
        Some("com.mitchellh.ghostty")
    );
    daemon.shutdown().await;
}

/// B8: `integrations.status` reads `capabilities` to tell "the hooks are
/// installed and the daemon answers them" from "the hooks are installed
/// and this daemon has no adapter for them". The Codex adapter landing
/// puts its name there, on both surfaces.
#[tokio::test]
async fn the_daemon_advertises_hooks_codex() {
    let daemon = Daemon::start().await;
    let (status, info) = daemon.http("GET", "/v1/info", &[], "").await;
    assert_eq!(status, 200);
    let capabilities = info["capabilities"]
        .as_array()
        .unwrap_or_else(|| panic!("{info}"));
    assert!(
        capabilities.iter().any(|c| c == "hooks.codex"),
        "{capabilities:?}"
    );
    // The Claude adapter is in this build too, on the other plan layer,
    // and says so on the same surface. What must stay absent is a CLI
    // nothing answers for.
    assert!(
        !capabilities.iter().any(|c| c == "hooks.gemini"),
        "nothing answers hooks.gemini in this build: {capabilities:?}"
    );
    daemon.shutdown().await;
}

/// Contract §9. Fixtures are recorded from real sessions, so the scrubbing
/// is not a convention: it is checked every time they are read. A payload
/// that slipped a home directory, an API key or a real prompt into the
/// repository would be committed forever.
#[test]
fn every_committed_fixture_is_scrubbed() {
    let dir = fixtures();
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("the fixture directory") {
        let path = entry.expect("a fixture").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        seen += 1;
        let text = std::fs::read_to_string(&path).expect("a readable fixture");
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
        let name = path.display();
        for secret in [
            "/Users/",
            "/home/",
            "sk-ant-",
            "sk-proj-",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "Bearer ",
            "@gmail",
            "ssh-rsa",
            "BEGIN PRIVATE KEY",
        ] {
            assert!(
                !text.contains(secret),
                "{name} carries {secret:?}: scrub it before committing"
            );
        }
        // Prompts, commands and assistant messages are the payload's own
        // content; each is replaced by a marker rather than shortened, so
        // "it looked harmless" is never the reason something stayed in.
        for (key, value) in [
            ("\"prompt\"", "REDACTED prompt"),
            ("\"command\"", "REDACTED command"),
            ("\"last_assistant_message\"", "REDACTED assistant message"),
            ("\"last-assistant-message\"", "REDACTED assistant message"),
        ] {
            if text.contains(key) {
                assert!(
                    text.contains(value),
                    "{name} has {key} but not the {value:?} marker"
                );
            }
        }
    }
    assert!(seen >= 8, "only {seen} fixtures found in {}", dir.display());
    // The session id in the fixtures is a UUID, not the `thr_…` the vendor
    // docs show (C19): a fixture in the old shape would silently test the
    // wrong thing.
    assert_eq!(fixture("session-start")["session_id"], SESSION);
    assert!(!SESSION.starts_with("thr_"));
}
