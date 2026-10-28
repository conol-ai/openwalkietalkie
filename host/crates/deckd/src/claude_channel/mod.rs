//! Terminal-independent Claude Channels bridge. Only the core can send orders;
//! MCP children receive them over an authenticated, local, receive-only door.
//! A nonce acknowledged through MCP proves Claude accepted channel notifications.
//! An uncertain write is never retried through the inbox (that would duplicate it).
mod mcp;

use crate::desktop::{DesktopError, FromDesktop};
use crate::inbox::Inbox;
use deck_model::Agent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

const BUDGET: Duration = Duration::from_secs(5);
const MAX_LINE: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Identity {
    pid: u32,
    session: String,
}

/// Read current identity, not the MCP child's startup environment: /clear and
/// interactive resume can change the conversation without restarting the child.
fn identity(dir: &Path, pid: u32) -> Option<Identity> {
    let bytes = std::fs::read(dir.join("sessions").join(format!("{pid}.json"))).ok()?;
    if bytes.len() > MAX_LINE {
        return None;
    }
    let row: Value = serde_json::from_slice(&bytes).ok()?;
    if row["pid"].as_u64()? != u64::from(pid) {
        return None;
    }
    let session = row["sessionId"].as_str()?;
    if session.is_empty() || session.len() > 256 {
        return None;
    }
    Some(Identity {
        pid,
        session: session.into(),
    })
}

#[derive(Debug)]
struct Job {
    text: String,
    deadline: Instant,
    reply: crossbeam_channel::Sender<Delivery>,
    started: bool,
}

// A queued job dropped on disconnect has definitely not been written. Once
// writing starts, a dropped job is uncertain and must not be replayed.
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.reply.try_send(if self.started {
            Delivery::Uncertain
        } else {
            Delivery::Unavailable
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Sent,
    Unavailable,
    Uncertain,
    SessionChanged,
}

#[derive(Debug)]
struct Entry {
    connection: u64,
    ready: bool,
    sender: mpsc::Sender<Job>,
}

#[derive(Debug, Default)]
pub struct Hub {
    entries: Mutex<BTreeMap<Identity, Entry>>,
    /// Helpers from another install of deckd, last refused at. They retry
    /// every 2 s, so a short window is a live count.
    stale: Mutex<BTreeMap<u32, Instant>>,
}

const STALE_WINDOW: Duration = Duration::from_secs(10);

impl Hub {
    #[cfg(test)]
    pub(crate) fn test_channel(
        &self,
        pid: u32,
        session: &str,
    ) -> crossbeam_channel::Receiver<String> {
        let (sender, mut jobs) = mpsc::channel::<Job>(8);
        self.entries.lock().unwrap().insert(
            Identity {
                pid,
                session: session.into(),
            },
            Entry {
                connection: 1,
                ready: true,
                sender,
            },
        );
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            while let Some(job) = jobs.blocking_recv() {
                let _ = tx.send(job.text.clone());
                let _ = job.reply.send(Delivery::Sent);
            }
        });
        rx
    }

    fn sender(&self, agent: &Agent) -> Option<mpsc::Sender<Job>> {
        // Virtual agents cannot claim another conversation's channel.
        if agent.owner.principal != deck_model::Principal::Adapter(deck_model::AdapterKind::Claude)
        {
            return None;
        }
        let id = Identity {
            pid: agent.session.pid?,
            session: agent
                .session
                .key
                .as_deref()?
                .strip_prefix("claude:")?
                .into(),
        };
        self.entries
            .lock()
            .ok()?
            .get(&id)
            .filter(|e| e.ready && !e.sender.is_closed())
            .map(|e| e.sender.clone())
    }

    fn note_stale(&self, pid: u32) {
        self.stale.lock().expect("channel hub").insert(pid, Instant::now());
    }

    /// Channel helpers refused because they run a different deckd executable,
    /// e.g. left over from before an update. Restarting their sessions fixes it.
    pub fn stale_count(&self) -> usize {
        let mut stale = self.stale.lock().expect("channel hub");
        stale.retain(|_, seen| seen.elapsed() < STALE_WINDOW);
        stale.len()
    }

    pub fn ready(&self, agent: &Agent) -> bool { self.sender(agent).is_some() }

    pub fn status(&self) -> Value {
        let entries = self.entries.lock().expect("channel hub");
        json!(entries
            .iter()
            .filter(|(_, e)| !e.sender.is_closed())
            .map(|(id, e)| {
                json!({"session": id.session, "state": if e.ready { "ready" } else { "checking" }})
            })
            .collect::<Vec<_>>())
    }

    /// Returns false before taking the order when there is no verified channel.
    /// Once taken, this owns both the outcome and any safe inbox fallback.
    pub fn post(
        &self,
        agent: &Agent,
        text: &str,
        inbox: Option<Inbox>,
        dialog_open: bool,
        out: crossbeam_channel::Sender<FromDesktop>,
    ) -> bool {
        let Some(sender) = self.sender(agent) else {
            return false;
        };
        let id = agent.id.clone();
        let text = text.to_owned();
        let failed_id = id.clone();
        let failed_text = text.clone();
        let failed_out = out.clone();
        if std::thread::Builder::new()
            .name("deckd-channel-order".into())
            .spawn(move || {
                let result = if dialog_open {
                    Err(DesktopError::DialogOpen)
                } else {
                    let (reply, rx) = crossbeam_channel::bounded(1);
                    let job = Job {
                        text: text.clone(),
                        deadline: Instant::now() + BUDGET,
                        reply,
                        started: false,
                    };
                    let delivery = if sender.try_send(job).is_err() {
                        Delivery::Unavailable
                    } else {
                        rx.recv_timeout(BUDGET + Duration::from_secs(1))
                            .unwrap_or(Delivery::Uncertain)
                    };
                    finish_delivery(delivery, inbox.as_ref(), &text)
                };
                let _ = out.send(FromDesktop::Order {
                    agent: id,
                    text,
                    result,
                });
            })
            .is_err()
        {
            let _ = failed_out.send(FromDesktop::Order {
                agent: failed_id,
                text: failed_text,
                result: Err(DesktopError::Inbox("channel_worker_failed")),
            });
        }
        true
    }
}

fn finish_delivery(
    delivery: Delivery,
    inbox: Option<&Inbox>,
    text: &str,
) -> Result<(), DesktopError> {
    match delivery {
        Delivery::Sent => Ok(()),
        Delivery::Unavailable => inbox
            .ok_or(DesktopError::Inbox("channel_unavailable"))
            .and_then(|inbox| crate::inbox::post(inbox, text)),
        Delivery::SessionChanged => Err(DesktopError::Inbox("channel_session_changed")),
        Delivery::Uncertain => Err(DesktopError::Inbox("channel_delivery_uncertain")),
    }
}

fn socket(data: &Path) -> PathBuf {
    data.join("claude-channel.sock")
}

/// The pid of a same-user process running a different copy of `deckd`.
fn stale_helper(stream: &UnixStream) -> Option<u32> {
    let (uid, pid) = crate::api::uds::peer_of(stream).ok()?;
    let pid = pid?;
    let own = std::env::current_exe().ok()?;
    let theirs = crate::trust::exe_path(pid)?;
    (uid == unsafe { libc::geteuid() } && theirs != own && theirs.file_name() == own.file_name())
        .then_some(pid)
}

fn peer(stream: &UnixStream) -> io::Result<u32> {
    let (uid, pid) = crate::api::uds::peer_of(stream)?;
    let pid = pid.ok_or_else(|| io::Error::other("missing peer pid"))?;
    // No script or general API client can impersonate either end of this door.
    // The same deckd executable implements both the daemon and MCP helper.
    let own_executable = std::env::current_exe()?;
    if uid != unsafe { libc::geteuid() }
        || crate::trust::exe_path(pid) != Some(own_executable)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "channel peer",
        ));
    }
    Ok(pid)
}

struct Reader<R> {
    input: BufReader<R>,
    bytes: Vec<u8>,
}

impl<R: tokio::io::AsyncRead + Unpin> Reader<R> {
    fn new(input: R) -> Self {
        Self {
            input: BufReader::new(input),
            bytes: Vec::new(),
        }
    }

    // The buffer survives cancellation by select!, including partial frames.
    async fn next(&mut self) -> io::Result<Value> {
        let remaining = MAX_LINE + 1 - self.bytes.len();
        (&mut self.input)
            .take(remaining as u64)
            .read_until(b'\n', &mut self.bytes)
            .await?;
        if self.bytes.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if self.bytes.len() > MAX_LINE || self.bytes.last() != Some(&b'\n') {
            return Err(io::Error::other("channel frame too large or incomplete"));
        }
        let bytes = std::mem::take(&mut self.bytes);
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

async fn write<W: AsyncWrite + Unpin>(output: &mut W, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    tokio::time::timeout(BUDGET, async {
        output.write_all(&bytes).await?;
        output.flush().await
    })
    .await?
}

/// The daemon already owns its single-instance lock before opening this socket.
pub async fn serve(data: PathBuf, hub: Arc<Hub>, mut stop: watch::Receiver<bool>) {
    let path = socket(&data);
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "Claude channel door unavailable; using inbox");
            return;
        }
    };
    if std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_err() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let mut connections = tokio::task::JoinSet::new();
    let mut next = 0u64;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break };
                if connections.len() >= 64 { continue; }
                let Ok(pid) = peer(&stream) else {
                    if let Some(pid) = stale_helper(&stream) { hub.note_stale(pid); }
                    continue;
                };
                let cli_pid = crate::hook::walk::resolve(pid, &crate::hook::walk::MacProcs,
                    crate::hook::walk::MAX_HOPS).cli_pid;
                let Some(cli_pid) = cli_pid else { continue };
                next += 1;
                let hub = hub.clone();
                connections.spawn(async move {
                    let _ = connection(stream, cli_pid, next, &hub).await;
                    hub.entries.lock().expect("channel hub").retain(|_, e| e.connection != next);
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    hub.entries.lock().expect("channel hub").clear();
    let _ = std::fs::remove_file(&path);
}

async fn connection(stream: UnixStream, pid: u32, number: u64, hub: &Hub) -> io::Result<()> {
    let (input, mut output) = stream.into_split();
    let mut input = Reader::new(input);
    let hello = tokio::time::timeout(BUDGET, input.next()).await??;
    let dir = PathBuf::from(
        hello["config_dir"]
            .as_str()
            .ok_or_else(|| io::Error::other("config dir"))?,
    );
    let (sender, mut jobs) = mpsc::channel::<Job>(8);
    let mut current: Option<Identity> = None;
    loop {
        tokio::select! {
            frame = input.next() => {
                let frame = frame?;
                let claimed: Option<Identity> = serde_json::from_value(frame["identity"].clone()).ok();
                let actual = identity(&dir, pid);
                let mut entries = hub.entries.lock().expect("channel hub");
                entries.retain(|_, e| e.connection != number);
                current = claimed.filter(|id| Some(id) == actual.as_ref());
                if let Some(id) = &current {
                    entries.insert(id.clone(), Entry { connection: number,
                        ready: frame["ready"] == true, sender: sender.clone() });
                }
            }
            Some(mut job) = jobs.recv() => {
                if current.is_none() || identity(&dir, pid) != current {
                    let _ = job.reply.send(Delivery::SessionChanged);
                    continue;
                }
                if job.deadline <= Instant::now() {
                    let _ = job.reply.send(Delivery::Unavailable);
                    continue;
                }
                job.started = true;
                let request = json!({"identity": current, "text": job.text});
                // After any write attempt, only an explicit pre-write rejection
                // from the helper permits fallback. EOF/timeouts are uncertain.
                let result = tokio::time::timeout_at(job.deadline.into(), async {
                    write(&mut output, &request).await?;
                    loop {
                        let response = input.next().await?;
                        if let Some(result) = response["delivery"].as_str() {
                            return Ok::<_, io::Error>(match result {
                                "sent" => Delivery::Sent,
                                "unavailable" => Delivery::Unavailable,
                                "session_changed" => Delivery::SessionChanged,
                                _ => Delivery::Uncertain,
                            });
                        }
                        // A heartbeat racing the request is not its receipt.
                    }
                }).await;
                let delivery = result.ok().and_then(Result::ok).unwrap_or(Delivery::Uncertain);
                let _ = job.reply.send(delivery);
                if delivery == Delivery::Uncertain { return Err(io::Error::other("channel receipt lost")); }
            }
        }
    }
}

pub fn main(data: &Path) -> Result<i32, String> {
    mcp::run(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_delivery_never_retries_inbox() {
        let missing = Inbox::socket_only("/no/such/socket");
        assert_eq!(
            finish_delivery(Delivery::Uncertain, Some(&missing), "hello")
                .unwrap_err()
                .reason(),
            "channel_delivery_uncertain"
        );
        assert!(finish_delivery(Delivery::Sent, None, "hello").is_ok());
    }

    #[tokio::test]
    async fn framing_handles_escaped_newlines_and_rejects_oversize() {
        let mut input = Reader::new(&b"{\"text\":\"one\\ntwo\"}\n"[..]);
        assert_eq!(input.next().await.unwrap()["text"], "one\ntwo");
        let bytes = vec![b'x'; MAX_LINE + 1];
        assert!(Reader::new(bytes.as_slice()).next().await.is_err());
    }

    #[tokio::test]
    async fn partial_frames_survive_select_cancellation() {
        let (mut output, input) = tokio::io::duplex(256);
        let mut input = Reader::new(input);
        output.write_all(b"{\"text\":").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), input.next())
                .await
                .is_err()
        );
        output.write_all(b"\"hello\"}\n").await.unwrap();
        assert_eq!(input.next().await.unwrap()["text"], "hello");
    }

    #[tokio::test]
    async fn bridge_receipts_disconnects_and_session_changes() {
        let dir = crate::core::testkit::temp("channel-connection");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let registry = dir.join("sessions/42.json");
        std::fs::write(&registry, br#"{"pid":42,"sessionId":"one"}"#).unwrap();
        let id = identity(&dir, 42).unwrap();
        let hub = Arc::new(Hub::default());
        let (server, client) = UnixStream::pair().unwrap();
        let server_hub = hub.clone();
        let server = tokio::spawn(async move { connection(server, 42, 1, &server_hub).await });
        let (input, mut output) = client.into_split();
        let mut input = Reader::new(input);
        write(&mut output, &json!({"config_dir":dir}))
            .await
            .unwrap();
        write(&mut output, &json!({"identity":id, "ready":false}))
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, async {
            while hub.status().as_array().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(hub.status()[0]["state"], "checking");
        write(&mut output, &json!({"identity":id, "ready":true}))
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, async {
            while hub.status()[0]["state"] != "ready" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let sender = hub.entries.lock().unwrap()[&id].sender.clone();

        for expected in [
            Delivery::Sent,
            Delivery::Unavailable,
            Delivery::SessionChanged,
        ] {
            let (reply, received) = crossbeam_channel::bounded(1);
            sender
                .send(Job {
                    text: "Please continue. 请继续。".into(),
                    deadline: Instant::now() + BUDGET,
                    reply,
                    started: false,
                })
                .await
                .unwrap();
            let request = input.next().await.unwrap();
            assert_eq!(request["identity"]["session"], "one");
            assert_eq!(request["text"], "Please continue. 请继续。");
            // A heartbeat racing a receipt must not count as delivery.
            write(&mut output, &json!({"identity":id,"ready":true}))
                .await
                .unwrap();
            write(&mut output, &json!({"delivery":match expected {
                Delivery::Sent => "sent", Delivery::Unavailable => "unavailable", _ => "session_changed"
            }})).await.unwrap();
            let actual =
                tokio::task::spawn_blocking(move || received.recv_timeout(BUDGET).unwrap())
                    .await
                    .unwrap();
            assert_eq!(actual, expected);
        }

        // A /clear before the write cannot redirect dictation into the new session.
        std::fs::write(&registry, br#"{"pid":42,"sessionId":"two"}"#).unwrap();
        let (reply, received) = crossbeam_channel::bounded(1);
        sender
            .send(Job {
                text: "old conversation".into(),
                deadline: Instant::now() + BUDGET,
                reply,
                started: false,
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::task::spawn_blocking(move || received.recv_timeout(BUDGET).unwrap())
                .await
                .unwrap(),
            Delivery::SessionChanged
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), input.next())
                .await
                .is_err()
        );

        // Losing the receipt after writing is uncertain, never a fallback signal.
        std::fs::write(&registry, br#"{"pid":42,"sessionId":"one"}"#).unwrap();
        let (reply, received) = crossbeam_channel::bounded(1);
        sender
            .send(Job {
                text: "only once".into(),
                deadline: Instant::now() + BUDGET,
                reply,
                started: false,
            })
            .await
            .unwrap();
        input.next().await.unwrap();
        drop(input);
        drop(output);
        assert_eq!(
            tokio::task::spawn_blocking(move || received.recv_timeout(BUDGET).unwrap())
                .await
                .unwrap(),
            Delivery::Uncertain
        );
        assert!(server.await.unwrap().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_a_pre_write_refusal_uses_the_inbox() {
        use std::io::Read;
        let dir = crate::core::testkit::temp("channel-fallback");
        let socket = dir.join("inbox.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let inbox = Inbox::socket_only(&socket);
        finish_delivery(Delivery::Unavailable, Some(&inbox), "fallback").unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["message"]["content"],
            "fallback"
        );
        for result in [Delivery::Uncertain, Delivery::SessionChanged] {
            assert!(finish_delivery(result, Some(&inbox), "do not retry").is_err());
            assert!(listener.accept().is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
