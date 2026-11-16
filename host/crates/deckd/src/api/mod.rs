//! The API doors (design §4, contract §3.3): the UDS newline-delimited
//! JSON-RPC socket and axum on `127.0.0.1:<port>`, one `dispatch`, one
//! session model. `serve` binds both (each may fail on its own: the UDS
//! path too long, the port in use; the TCP one is skipped altogether with
//! `[api] tcp = false`) and returns what it bound; `Served::shutdown`
//! stops the accept loops and every connection.
pub mod conn;
pub mod dispatch;
pub mod hooks;
pub mod http;
pub mod rpc;
pub mod session;
pub mod uds;

use crate::bus::Bus;
use crate::config::{Config, Paths};
use crate::core::handle::{CallCtx, CoreHandle};
use crate::tokens::Tokens;
use crate::trust::PeerVerifier;
use arc_swap::ArcSwap;
use deck_model::{Principal, Scope, SessionId};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::watch;

/// Everything a door needs.
pub struct Api {
    pub core: CoreHandle,
    pub bus: Arc<Bus>,
    /// The effective config (hot reloaded by `config::watch`).
    pub config: Arc<ArcSwap<Config>>,
    /// `tokens.toml` as last loaded (reloaded by the same watcher).
    pub tokens: Arc<ArcSwap<Tokens>>,
    pub verifier: Arc<dyn PeerVerifier>,
    /// The agent adapters (§5), one per CLI, with the memo a hook needs
    /// across requests and doors.
    pub adapters: Arc<crate::adapters::Adapters>,
    pub paths: Paths,
    /// What `[api] tcp` left out of the file resolves to when [`serve`]
    /// decides the TCP door: `true` when `deckd run` found the pre-plugin
    /// Claude hooks, which post to it, in Claude's settings at start
    /// (`config::Api::tcp_door`, the same finding as `Core::legacy_hooks`).
    pub legacy_hooks: bool,
}

impl Api {
    /// The context of the daemon's own calls into the core (`service.bound`).
    pub fn internal_ctx() -> CallCtx {
        CallCtx {
            principal: Principal::Uid(0),
            session: SessionId(0),
            scopes: vec![Scope::Admin],
            trusted: false,
            own: BTreeSet::new(),
        }
    }
}

/// What `serve` bound.
pub struct Served {
    /// The TCP port, `None` while degraded (`port_in_use`) and with no TCP
    /// door at all (off, `config::Api::tcp_door`).
    pub port: Option<u16>,
    /// The UDS door is up.
    pub uds: bool,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    socket: std::path::PathBuf,
    port_file: std::path::PathBuf,
}

impl Served {
    /// Stop the doors without waiting.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// Stop the doors, wait for their tasks, remove the socket, empty the port file.
    pub async fn shutdown(self) {
        self.stop();
        for task in self.tasks {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::write(&self.port_file, "");
    }
}

/// Write the bound port (`0600`), empty while degraded and with no door.
/// The file appears whole: it is written beside itself and renamed into
/// place, because readers (`deckd status`, the installer, the tests) take
/// "it exists" for "the door is decided" and must never read it between
/// its creation and its first byte.
pub(crate) fn write_port_file(path: &std::path::Path, port: Option<u16>) {
    let text = port.map(|p| p.to_string()).unwrap_or_default();
    let partial = path.with_extension("partial");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts
        .open(&partial)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(text.as_bytes())
        })
        .and_then(|()| std::fs::rename(&partial, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        tracing::warn!(error = %e, path = %path.display(), "cannot write the port file");
    }
}

/// Bind the UDS and TCP doors and start serving. Never fails as a whole:
/// each door logs its own failure, and the TCP result reaches the core as
/// `service.bound` (so `service.info`/`/v1/info` tell the truth).
pub async fn serve(api: Arc<Api>) -> Served {
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = Vec::new();
    let uds = match uds::bind(&api.paths.socket) {
        Ok(listener) => {
            tracing::info!(path = %api.paths.socket.display(), "uds door");
            tasks.push(tokio::spawn(uds::accept_loop(
                api.clone(),
                listener,
                stop_rx.clone(),
            )));
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "no uds door: `deckd ctl` unavailable");
            false
        }
    };
    // The TCP door binds, or degrades and keeps retrying on its own task
    // (§3.3 D8): either way `service.bound` reaches the core and the port
    // file tells the truth. Off — the default, unless the file switches it
    // on or the pre-plugin Claude hooks still post to it — there is no
    // door to bind, and `service.bound` says that too.
    let door = api.config.load().api.tcp_door(|| api.legacy_hooks);
    let port = if door.on {
        let (port, tcp) = http::tcp_door(api.clone(), stop_rx.clone()).await;
        tasks.push(tcp);
        port
    } else {
        http::tcp_off(&api).await;
        None
    };
    Served {
        port,
        uds,
        stop: stop_tx,
        tasks,
        socket: api.paths.socket.clone(),
        port_file: api.paths.port.clone(),
    }
}

/// Shared fixtures for the door tests: an `Api` over a live core thread
/// on a temp data dir, `port = 0`, a `FakeVerifier`.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::core::testkit::LiveCore;
    use crate::trust::FakeVerifier;

    pub struct TestDaemon {
        pub api: Arc<Api>,
        pub stop: watch::Sender<bool>,
        pub core: LiveCore,
    }

    impl TestDaemon {
        /// Mint a bearer token into the daemon's table; returns the plaintext.
        pub fn token(&self, name: &str, scopes: &[Scope]) -> String {
            let mut tokens = (**self.api.tokens.load()).clone();
            let plain = tokens
                .create(name, scopes.to_vec(), None, None, crate::core::wall_now())
                .unwrap();
            self.api.tokens.store(Arc::new(tokens));
            plain
        }
    }

    impl Drop for TestDaemon {
        fn drop(&mut self) {
            let _ = self.stop.send(true);
        }
    }

    pub fn test_api(name: &str, verifier: FakeVerifier) -> TestDaemon {
        test_api_on(name, verifier, 0)
    }

    /// The same on a named port (the port-in-use degrade).
    pub fn test_api_on(name: &str, verifier: FakeVerifier, port: u16) -> TestDaemon {
        test_api_with(name, Arc::new(verifier), port)
    }

    /// The same with any verifier (the accept-loop tests need a slow one).
    #[cfg_attr(not(unix), allow(dead_code))] // its tests dial the Unix socket
    pub fn test_api_verifier(name: &str, verifier: Arc<dyn PeerVerifier>) -> TestDaemon {
        test_api_with(name, verifier, 0)
    }

    /// The same, its core keeping the cloud voice's key in `store` (a
    /// `MemoryKeyStore`, never the Keychain).
    pub fn test_api_keyed(
        name: &str,
        verifier: FakeVerifier,
        store: Arc<dyn crate::secrets::KeyStore>,
    ) -> TestDaemon {
        let core = LiveCore::start_with(&format!("api-{name}"), |core| {
            core.set_voice_key(crate::secrets::VoiceKey::open(
                crate::secrets::KeyHome::Store(store),
            ));
        });
        test_api_around(core, Arc::new(verifier), 0)
    }

    fn test_api_with(name: &str, verifier: Arc<dyn PeerVerifier>, port: u16) -> TestDaemon {
        test_api_around(LiveCore::start(&format!("api-{name}")), verifier, port)
    }

    fn test_api_around(core: LiveCore, verifier: Arc<dyn PeerVerifier>, port: u16) -> TestDaemon {
        let paths = core.paths();
        let mut config = Config::default();
        config.api.port = port;
        // Both doors: these are the doors' own tests, and the TCP door is
        // off unless the file switches it on.
        config.api.tcp = Some(true);
        let config = Arc::new(ArcSwap::from_pointee(config));
        let api = Arc::new(Api {
            core: core.handle.clone(),
            bus: core.handle.bus().clone(),
            config: config.clone(),
            tokens: Arc::new(ArcSwap::from_pointee(Tokens::default())),
            verifier,
            adapters: Arc::new(crate::adapters::Adapters::new(config)),
            paths,
            legacy_hooks: false,
        });
        let (stop, _) = watch::channel(false);
        TestDaemon { api, stop, core }
    }
}

#[cfg(test)]
mod tests {
    use super::write_port_file;

    /// The port file is written whole and renamed into place: it holds the
    /// port at `0600`, is emptied the same way when there is no door, and
    /// leaves nothing beside it.
    #[test]
    fn the_port_file_appears_whole_at_0600_and_leaves_nothing_beside_it() {
        let dir = std::env::temp_dir().join(format!("deckd-port-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("port");
        write_port_file(&path, Some(50031));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "50031");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        write_port_file(&path, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from("port")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
