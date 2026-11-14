//! The UDS door (design §4.1, contract §3.3): a `UnixListener` at
//! `paths.socket` (dir 0700, socket 0600, a stale file removed), one
//! `conn::run` per connection after the peer credentials were read (the
//! uid must be ours; the pid goes to the verifier once, before the hello
//! reply). No HTTP parser here: `curl --unix-socket` is unsupported by
//! design.
//!
//! On Windows the same door is the data directory's named pipe
//! (`deck_client::windows`): every instance is created with a DACL that
//! grants only this user, remote clients are refused, the first instance
//! must be the first of its name (a squatter fails the bind), and each
//! client's token user is checked on accept in place of the uid. From the
//! accepted connection on, the two platforms share [`serve_peer`].
use super::conn::{self, Door, Peer};
use super::Api;
use crate::trust::PeerVerdict;
use deck_model::Principal;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// macOS's `sun_path` holds 104 bytes including the NUL.
pub const SUN_PATH_MAX: usize = 103;

/// Bind the socket, replacing a stale file. A path longer than
/// `SUN_PATH_MAX` is refused with a clear error (the API still serves TCP).
#[cfg(unix)]
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if path.as_os_str().len() > SUN_PATH_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "socket path is {} bytes, more than sun_path allows ({SUN_PATH_MAX}): {}",
                path.as_os_str().len(),
                path.display()
            ),
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// The peer's uid and pid (`LOCAL_PEERPID` on macOS, through tokio).
#[cfg(unix)]
pub fn peer_of(stream: &UnixStream) -> std::io::Result<(u32, Option<u32>)> {
    let cred = stream.peer_cred()?;
    let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
    Ok((cred.uid(), pid))
}

/// Accept until `stop` flips; every connection runs on its own task.
#[cfg(unix)]
pub async fn accept_loop(api: Arc<Api>, listener: UnixListener, mut stop: watch::Receiver<bool>) {
    let own_uid = super::session::own_uid();
    loop {
        let stream = tokio::select! {
            _ = stop.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!(error = %e, "uds accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let (uid, pid) = match peer_of(&stream) {
            Ok(peer) => peer,
            Err(e) => {
                tracing::warn!(error = %e, "uds peer credentials unreadable; closed");
                continue;
            }
        };
        if uid != own_uid {
            tracing::warn!(uid, "uds peer of another uid; closed");
            continue;
        }
        let (reader, writer) = stream.into_split();
        serve_peer(&api, &stop, uid, pid, reader, writer);
    }
}

/// One accepted, same-user connection: verify its peer, then run it.
///
/// The verification is blocking work (Security.framework plus up to
/// `ANCESTRY_MAX_HOPS` `proc_pidinfo` calls, 250–500 ms measured): it runs
/// per connection, on the blocking pool, so a slow peer never holds up the
/// accept loop — and still before this connection's hello reply, as §3.3
/// requires.
fn serve_peer<R, W>(
    api: &Arc<Api>,
    stop: &watch::Receiver<bool>,
    uid: u32,
    pid: Option<u32>,
    reader: R,
    writer: W,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let api = api.clone();
    let stop = stop.clone();
    tokio::spawn(async move {
        let verdict = verdict_of(&api, pid).await;
        tracing::debug!(
            uid,
            ?pid,
            trusted = verdict.trusted,
            capped = verdict.ancestry_capped,
            "uds peer"
        );
        conn::run(
            api,
            Peer {
                door: Door::Uds,
                principal: Some((Principal::Uid(uid), Vec::new())),
                verdict,
            },
            conn::line_stream(reader),
            conn::line_sink(writer),
            stop,
        )
        .await;
    });
}

/// The bound pipe: its name, the instance waiting for the next client, and
/// what every further instance is created with.
#[cfg(windows)]
pub struct Listener {
    name: std::path::PathBuf,
    next: tokio::net::windows::named_pipe::NamedPipeServer,
    security: deck_client::windows::SecurityDescriptor,
    user: deck_client::windows::Sid,
}

#[cfg(windows)]
impl Listener {
    fn instance(
        &self,
        first: bool,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        instance(&self.name, &self.security, first)
    }
}

/// One pipe instance with `security`; `first` refuses a name some other
/// process already serves. Also the Claude channel's door.
#[cfg(windows)]
pub(crate) fn instance(
    name: &Path,
    security: &deck_client::windows::SecurityDescriptor,
    first: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let mut attributes = security.attributes();
    // SAFETY: `attributes` points at `security`'s descriptor, which outlives
    // the call; the kernel copies it into the new instance.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, std::ptr::addr_of_mut!(attributes).cast())
    }
}

/// Create the pipe's first instance, owned by and open to this user only.
/// A name some other process already serves is refused
/// (`FILE_FLAG_FIRST_PIPE_INSTANCE`): access denied, and the API keeps its
/// TCP door if that is on, as a socket that cannot bind does on Unix.
#[cfg(windows)]
pub fn bind(path: &Path) -> std::io::Result<Listener> {
    let user = deck_client::windows::current_user()?;
    let security = deck_client::windows::SecurityDescriptor::only(&user)?;
    let next = instance(path, &security, true).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot create {} (is another deckd running?): {e}", path.display()),
        )
    })?;
    Ok(Listener {
        name: path.to_path_buf(),
        next,
        security,
        user,
    })
}

/// Accept until `stop` flips; every connection runs on its own task. The
/// next instance is created as soon as one connects, so a client never
/// finds the pipe gone between two accepts (it waits out "busy" instead).
#[cfg(windows)]
pub async fn accept_loop(api: Arc<Api>, mut listener: Listener, mut stop: watch::Receiver<bool>) {
    let uid = listener.user.rid();
    loop {
        let connected = tokio::select! {
            _ = stop.changed() => break,
            connected = listener.next.connect() => connected,
        };
        let replacement = match listener.instance(false) {
            Ok(next) => next,
            Err(e) => {
                tracing::error!(error = %e, "pipe: cannot create the next instance; door closed");
                break;
            }
        };
        let server = std::mem::replace(&mut listener.next, replacement);
        if let Err(e) = connected {
            tracing::warn!(error = %e, "pipe accept failed");
            continue;
        }
        let pid = match deck_client::windows::pipe_peer_pid(&server, true) {
            Ok(pid) => pid,
            Err(e) => {
                tracing::warn!(error = %e, "pipe peer unidentifiable; closed");
                continue;
            }
        };
        match deck_client::windows::process_user(pid) {
            Ok(user) if user == listener.user => {}
            Ok(user) => {
                tracing::warn!(pid, user = %user.string(), "pipe peer of another user; closed");
                continue;
            }
            Err(e) => {
                tracing::warn!(pid, error = %e, "pipe peer's user unreadable; closed");
                continue;
            }
        }
        let (reader, writer) = tokio::io::split(server);
        serve_peer(&api, &stop, uid, Some(pid), reader, writer);
    }
}

/// `verifier.verify(pid)` off the async worker: the trait stays
/// synchronous (contract §3.4), so the call goes to `spawn_blocking`. No
/// pid (or a blocking pool that could not run it) is the default verdict:
/// untrusted, uncapped.
async fn verdict_of(api: &Arc<Api>, pid: Option<u32>) -> PeerVerdict {
    let Some(pid) = pid else {
        return PeerVerdict::default();
    };
    let verifier = api.verifier.clone();
    match tokio::task::spawn_blocking(move || verifier.verify(pid)).await {
        Ok(verdict) => verdict,
        Err(e) => {
            tracing::warn!(pid, error = %e, "uds peer verification failed; untrusted");
            PeerVerdict::default()
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::api::testkit::test_api;
    use crate::trust::FakeVerifier;
    use deck_model::ApiError;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn line(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> serde_json::Value {
        let mut text = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            reader.read_line(&mut text),
        )
        .await
        .expect("a line within 3 s")
        .unwrap();
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not json: {text:?}"))
    }

    #[tokio::test]
    async fn a_uds_client_must_hello_first_and_then_may_call() {
        let daemon = test_api(
            "uds-hello",
            FakeVerifier {
                trusted: true,
                capped: false,
            },
        );
        let served = crate::api::serve(daemon.api.clone()).await;
        assert!(served.uds, "the socket bound");
        let socket = daemon.api.paths.socket.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // A non-hello first line: -32001 hello_first and the connection closes.
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"{\"id\":1,\"method\":\"session.ping\"}\n")
            .await
            .unwrap();
        let resp = line(&mut r).await;
        assert_eq!(resp["error"]["code"], ApiError::UNAUTHORIZED);
        assert_eq!(resp["error"]["data"]["why"], "hello_first");
        let mut rest = String::new();
        assert_eq!(r.read_line(&mut rest).await.unwrap(), 0, "closed");
        // A hello: the verifier's verdict is the session's trust.
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"{\"id\":1,\"method\":\"session.hello\",\"params\":{\"client\":{\"name\":\"t\",\"version\":\"0\"},\"scopes\":[\"admin\"]}}\n")
            .await
            .unwrap();
        let hello = line(&mut r).await;
        assert_eq!(hello["result"]["principal"]["trusted"], true, "{hello}");
        assert_eq!(
            hello["result"]["principal"]["id"],
            format!("uid:{}", crate::api::session::own_uid())
        );
        assert_eq!(
            hello["result"]["principal"]["scopes"],
            serde_json::json!(["admin"])
        );
        w.write_all(b"{\"id\":2,\"method\":\"service.info\"}\n")
            .await
            .unwrap();
        let info = line(&mut r).await;
        assert_eq!(info["result"]["api"], "1");
        assert!(info["result"]["uds"]
            .as_str()
            .unwrap()
            .ends_with("deckd.sock"));
        assert_eq!(info["result"]["port"], served.port.unwrap());
        drop(w);
        served.shutdown().await;
        assert!(!socket.exists(), "the socket file is removed on shutdown");
    }

    /// The verification is per connection, not per accept: a peer whose
    /// `verify` takes half a second must not stop every other peer from
    /// being accepted and answered (`MacVerifier` measures 250–500 ms).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_verification_does_not_block_the_accept_loop() {
        /// The first verification parks until the test releases it; every
        /// later one answers at once.
        struct GateVerifier {
            entered: std::sync::mpsc::SyncSender<()>,
            release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        }
        impl crate::trust::PeerVerifier for GateVerifier {
            fn verify(&self, _pid: u32) -> PeerVerdict {
                // The guard is dropped before the park: a second peer's
                // verification must not queue behind the first one's lock.
                let parked = self.release.lock().unwrap().take();
                if let Some(release) = parked {
                    let _ = self.entered.send(());
                    let _ = release.recv();
                }
                PeerVerdict::default()
            }
        }

        const HELLO: &[u8] =
            b"{\"id\":1,\"method\":\"session.hello\",\"params\":{\"client\":{\"name\":\"t\"}}}\n";
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let daemon = crate::api::testkit::test_api_verifier(
            "uds-slow-verify",
            Arc::new(GateVerifier {
                entered: entered_tx,
                release: std::sync::Mutex::new(Some(release_rx)),
            }),
        );
        let served = crate::api::serve(daemon.api.clone()).await;
        assert!(served.uds, "the socket bound");
        let socket = daemon.api.paths.socket.clone();
        // The first peer: its verification parks inside the verifier.
        let (slow_r, mut slow_w) = UnixStream::connect(&socket).await.unwrap().into_split();
        let mut slow_r = BufReader::new(slow_r);
        slow_w.write_all(HELLO).await.unwrap();
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(std::time::Duration::from_secs(5))
        })
        .await
        .unwrap()
        .expect("the first verification started");
        // The second peer is accepted, verified and answered meanwhile.
        let (fast_r, mut fast_w) = UnixStream::connect(&socket).await.unwrap().into_split();
        let mut fast_r = BufReader::new(fast_r);
        fast_w.write_all(HELLO).await.unwrap();
        assert_eq!(line(&mut fast_r).await["result"]["api"], "1");
        // Releasing the first one answers it too: nothing was dropped.
        release_tx.send(()).unwrap();
        assert_eq!(line(&mut slow_r).await["result"]["api"], "1");
        served.shutdown().await;
    }

    #[test]
    fn overlong_socket_paths_are_refused_with_a_clear_error() {
        let long = std::path::PathBuf::from(format!("/tmp/{}/deckd.sock", "x".repeat(120)));
        let err = bind(&long).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("sun_path"));
    }
}
