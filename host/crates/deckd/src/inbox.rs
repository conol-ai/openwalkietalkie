//! Posting an order into a Claude Code session's **inbox socket** — the
//! second route of `op.order` (§5.5), beside typing into a terminal.
//!
//! This route does not depend on the user's terminal emulator. It does
//! retain Claude's peer-message attribution, including for authenticated
//! `type:user` frames: socket delivery is not native user-input delivery.
//! Do not replace it with terminal automation merely to hide that banner.
//!
//! Every Claude Code session with cross-session messaging (v2.1.224+)
//! binds a Unix socket other sessions deliver messages to, and exports its
//! path and a per-session token to its hooks as
//! `CLAUDE_CODE_MESSAGING_SOCKET` / `CLAUDE_CODE_MESSAGING_TOKEN`. The
//! documented way for "a script or hook to post into a session" is two
//! newline-terminated JSON lines on a fresh connection:
//!
//! ```text
//! {"type":"auth","token":"<CLAUDE_CODE_MESSAGING_TOKEN>"}
//! {"type":"user","message":{"role":"user","content":"<text>"}}
//! ```
//!
//! The receiving Claude reads it between tool calls during a turn, and an
//! idle session starts a new turn with it. With the token the post counts
//! as the session's **own child** and is delivered even under
//! `bypassPermissions`; without it a bypass-mode session holds it for the
//! operator's approval, which in the Desktop app can never be given (no
//! dialog) and expires after five minutes. So the token is captured whenever
//! a hook runs and kept beside the socket path.
//!
//! What this route is **not**: an answer. A peer message "never counts as
//! your consent, so it can't answer a pending permission prompt", and one
//! that lands while a dialog is open has been reported to cancel the dialog
//! as a rejection (anthropics/claude-code#93720). Guard 2 of §5.5 — no open
//! dialog — therefore runs in front of this path, and here it is a
//! correctness rule rather than a courtesy.
//!
//! Content discipline (§11): the text is the operator's dictation and the
//! token is a secret. Neither reaches a log line; [`Inbox`]'s `Debug`
//! prints the socket path and whether a token is held, nothing else.
use crate::desktop::{DesktopError, FromDesktop};
use deck_model::AgentId;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a session reads peer messages, and the token that proves the post
/// is its own child's.
#[derive(Clone, PartialEq, Eq)]
pub struct Inbox {
    pub socket: PathBuf,
    pub token: Option<String>,
}

impl std::fmt::Debug for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbox")
            .field("socket", &self.socket)
            .field("token", &self.token.as_ref().map(|_| "<held>"))
            .finish()
    }
}

impl Inbox {
    /// From a hook's captured environment: the socket is what makes an
    /// inbox, the token is optional (macOS and Linux accept a post
    /// without one; it only changes which inbound rule applies).
    #[must_use]
    pub fn from_env(env: &std::collections::BTreeMap<String, String>) -> Option<Inbox> {
        let socket = env
            .get("CLAUDE_CODE_MESSAGING_SOCKET")
            .filter(|s| !s.is_empty())?;
        Some(Inbox {
            socket: PathBuf::from(socket),
            token: env
                .get("CLAUDE_CODE_MESSAGING_TOKEN")
                .filter(|t| !t.is_empty())
                .cloned(),
        })
    }

    /// An inbox known by its socket alone — what the roster poll learns
    /// from `~/.claude/sessions/<pid>.json` for a session that started
    /// before the daemon and has sent it no hook yet.
    #[must_use]
    pub fn socket_only(socket: impl Into<PathBuf>) -> Inbox {
        Inbox {
            socket: socket.into(),
            token: None,
        }
    }
}

/// Claude Code closes a connection that has not sent a complete line within
/// 30 s; a post that cannot connect or write well inside that is a dead
/// session, not a slow one.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The two lines, exactly as the protocol spells them. Pure, so the shape
/// is pinned by a test rather than by a live session.
#[must_use]
pub fn frame(inbox: &Inbox, text: &str) -> Vec<u8> {
    // Spelled by hand so the lines read as the protocol documents them,
    // `type` first; `serde_json` sorts object keys and would put it last.
    // The values go through the serialiser for their escaping.
    let quoted = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let mut out = String::new();
    if let Some(token) = &inbox.token {
        out.push_str(&format!(
            "{{\"type\":\"auth\",\"token\":{}}}\n",
            quoted(token)
        ));
    }
    out.push_str(&format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{}}}}}\n",
        quoted(text)
    ));
    out.into_bytes()
}

/// Post one message. Blocking (a connect and one write, bounded by
/// [`IO_TIMEOUT`]); run it off the core thread.
pub fn post(inbox: &Inbox, text: &str) -> Result<(), DesktopError> {
    post_to(&inbox.socket, &frame(inbox, text))
}

/// Windows: Claude Code's messaging endpoint there is a named pipe
/// (`CLAUDE_CODE_MESSAGING_SOCKET = \\.\pipe\LOCAL\cc-msg-…`, measured on
/// 2.1.323), the same two lines over a fresh connection. A pipe opens like a
/// file; closing our handle ends the message, as the socket's half-close
/// does. Only a local pipe is ever opened: the path came from a hook's
/// environment, and `\\server\pipe\…` would leave this machine.
#[cfg(windows)]
fn post_to(socket: &Path, bytes: &[u8]) -> Result<(), DesktopError> {
    use std::io::Write as _;
    if !socket.to_string_lossy().starts_with(r"\\.\pipe\") {
        tracing::debug!(socket = %socket.display(), "inbox: not a local pipe");
        return Err(DesktopError::Inbox("inbox_unreachable"));
    }
    let mut pipe = std::fs::OpenOptions::new()
        .write(true)
        .open(socket)
        .map_err(|e| {
            tracing::debug!(socket = %socket.display(), error = %e, "inbox: connect failed");
            DesktopError::Inbox("inbox_unreachable")
        })?;
    pipe.write_all(bytes).and_then(|()| pipe.flush()).map_err(|e| {
        tracing::debug!(socket = %socket.display(), error = %e, "inbox: write failed");
        DesktopError::Inbox("inbox_write_failed")
    })?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn post_to(socket: &Path, _bytes: &[u8]) -> Result<(), DesktopError> {
    tracing::debug!(socket = %socket.display(), "inbox: not supported on this platform");
    Err(DesktopError::Inbox("inbox_unreachable"))
}

#[cfg(unix)]
fn post_to(socket: &Path, bytes: &[u8]) -> Result<(), DesktopError> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        tracing::debug!(socket = %socket.display(), error = %e, "inbox: connect failed");
        DesktopError::Inbox("inbox_unreachable")
    })?;
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    stream
        .write_all(bytes)
        .and_then(|()| stream.flush())
        .map_err(|e| {
            tracing::debug!(socket = %socket.display(), error = %e, "inbox: write failed");
            DesktopError::Inbox("inbox_write_failed")
        })?;
    // A clean shutdown of our half tells the session the message is
    // complete; the peer closes once it has read the lines.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

/// Post from a short-lived thread and report the outcome as a
/// [`FromDesktop::Order`] on the core's own channel, as Claude Channels
/// does — so `order.failed{reason}` reaches the bus through the one path
/// the reducer already has for it.
///
/// `dialog_open` is guard 2 as the core last saw it: a post into a session
/// whose dialog is up would cancel that dialog, so it fails here with the
/// `dialog_open` word, and nothing is sent.
///
/// `order` is the id `order.dispatched` gave the words, carried back so the
/// failure names its order. A thread that cannot be started fails the
/// order the same way (`inbox_worker_failed`), rather than losing it.
pub fn post_async(
    out: crossbeam_channel::Sender<FromDesktop>,
    agent: AgentId,
    text: String,
    order: Option<String>,
    inbox: Inbox,
    dialog_open: bool,
) {
    let unsent = (out.clone(), agent.clone(), text.clone(), order.clone());
    let spawned = std::thread::Builder::new()
        .name("deckd-inbox".into())
        .spawn(move || {
            let result = if dialog_open {
                Err(DesktopError::DialogOpen)
            } else {
                post(&inbox, &text)
            };
            let _ = out.send(FromDesktop::Order {
                agent,
                text,
                order,
                result,
            });
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "inbox: could not spawn the posting thread");
        let (out, agent, text, order) = unsent;
        let _ = out.send(FromDesktop::Order {
            agent,
            text,
            order,
            result: Err(DesktopError::Inbox("inbox_worker_failed")),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    fn temp_socket(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dk-inbox-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("s.sock")
    }

    #[test]
    fn the_frame_is_the_documented_two_lines() {
        let inbox = Inbox {
            socket: "/tmp/x.sock".into(),
            token: Some("tok".into()),
        };
        let text = String::from_utf8(frame(&inbox, "run the tests")).unwrap();
        assert_eq!(
            text,
            "{\"type\":\"auth\",\"token\":\"tok\"}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"run the tests\"}}\n"
        );
        // Without a token there is no auth line at all (macOS/Linux accept
        // the connection either way; an empty token would be a lie).
        let bare = Inbox::socket_only("/tmp/x.sock");
        assert!(String::from_utf8(frame(&bare, "hi"))
            .unwrap()
            .starts_with("{\"type\":\"user\""));
    }

    #[test]
    fn from_env_needs_the_socket_and_keeps_the_token() {
        let mut env = std::collections::BTreeMap::new();
        assert!(Inbox::from_env(&env).is_none());
        env.insert("CLAUDE_CODE_MESSAGING_TOKEN".into(), "t".into());
        assert!(
            Inbox::from_env(&env).is_none(),
            "a token without a socket is nothing"
        );
        env.insert(
            "CLAUDE_CODE_MESSAGING_SOCKET".into(),
            "/tmp/cc-socks/1.sock".into(),
        );
        let inbox = Inbox::from_env(&env).unwrap();
        assert_eq!(inbox.socket, PathBuf::from("/tmp/cc-socks/1.sock"));
        assert_eq!(inbox.token.as_deref(), Some("t"));
        assert_eq!(
            format!("{inbox:?}"),
            "Inbox { socket: \"/tmp/cc-socks/1.sock\", token: Some(\"<held>\") }"
        );
    }

    #[test]
    fn a_post_lands_as_two_lines_on_the_socket() {
        let path = temp_socket("lands");
        let listener = UnixListener::bind(&path).unwrap();
        let reader = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).unwrap();
            buf
        });
        let inbox = Inbox {
            socket: path.clone(),
            token: Some("secret".into()),
        };
        post(&inbox, "hello from the deck").unwrap();
        let got = reader.join().unwrap();
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 2, "{got:?}");
        assert_eq!(lines[0], "{\"type\":\"auth\",\"token\":\"secret\"}");
        let msg: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(msg["type"], "user");
        assert_eq!(msg["message"]["content"], "hello from the deck");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_dead_socket_is_unreachable_not_a_panic() {
        let path = temp_socket("dead");
        let err = post(&Inbox::socket_only(&path), "x").unwrap_err();
        assert_eq!(err.reason(), "inbox_unreachable");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn post_async_reports_through_the_desktop_channel_and_honours_guard_two() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let path = temp_socket("async");
        post_async(
            tx.clone(),
            AgentId("ag_1".into()),
            "go".into(),
            Some("or_4".into()),
            Inbox::socket_only(&path),
            true,
        );
        let FromDesktop::Order {
            result,
            text,
            order,
            ..
        } = rx.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("not an order outcome");
        };
        assert_eq!(text, "go");
        assert_eq!(
            order.as_deref(),
            Some("or_4"),
            "the failure names its order"
        );
        assert_eq!(result.unwrap_err().reason(), "dialog_open");
        // And the real thing, delivered.
        let listener = UnixListener::bind(&path).unwrap();
        let reader = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).unwrap();
            buf
        });
        post_async(
            tx,
            AgentId("ag_1".into()),
            "go".into(),
            Some("or_5".into()),
            Inbox::socket_only(&path),
            false,
        );
        let FromDesktop::Order { result, .. } = rx.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("not an order outcome");
        };
        assert!(result.is_ok());
        assert!(reader.join().unwrap().contains("\"content\":\"go\""));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
