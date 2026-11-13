//! Contract §5 row 13 — `config::hot_reload_keeps_last_good_on_invalid`:
//! the real `notify` watcher on a temp data dir, an invalid `config.toml`
//! keeping the last good config (plus the `.invalid` copy and the
//! `error{code:"config_invalid"}` event on the bus), and a valid change to
//! `api.allowed_origins` reaching the cell, the core and
//! `config.changed{keys}`.
//!
//! Nothing here binds a port or touches the real data dir: `DECKD_DATA_DIR`
//! is never read, the dir is `$TMPDIR/deckd-config-<pid>-<n>` and the core
//! is a detached `CoreHandle` whose messages the test reads itself.
use deck_model::{Event, EventKind};
use deckd::bus::Bus;
use deckd::config::{self, Config, Paths};
use deckd::core::handle::{CoreHandle, CoreMsg};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The contract's bound on one reload (300 ms debounce + FSEvents).
const WAIT: Duration = Duration::from_secs(2);
/// How often the test looks at the bus while it waits.
const POLL: Duration = Duration::from_millis(10);

static DIRS: AtomicU32 = AtomicU32::new(0);

/// A data dir of our own, short and under `$TMPDIR`.
fn data_dir() -> std::path::PathBuf {
    let n = DIRS.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("deckd-cfg-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temp data dir");
    dir
}

/// Wait for the first event of `kind` on `rx`, or fail with what did arrive.
fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<deckd::bus::Published>,
    kind: EventKind,
) -> Arc<Event> {
    let deadline = Instant::now() + WAIT;
    let mut seen: Vec<&'static str> = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(published) => {
                if published.event.kind == kind {
                    return published.event;
                }
                seen.push(published.event.kind.as_str());
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "no {} within {WAIT:?} (saw {seen:?})",
                    kind.as_str()
                );
                std::thread::sleep(POLL);
            }
            Err(e) => panic!("the bus ended: {e}"),
        }
    }
}

/// Write a whole file the way an editor does: a temp file and a rename, so
/// the watcher never sees a half-written config (and the test never races
/// its own write).
fn put(path: &std::path::Path, text: &str) {
    let tmp = path.with_extension("writing");
    std::fs::write(&tmp, text).expect("write the temp file");
    std::fs::rename(&tmp, path).expect("rename it into place");
}

/// What this test binary has logged at `WARN` and above, from any thread
/// (the watcher logs from its own). The subscriber is the process's
/// default, installed by the first caller.
fn warnings() -> String {
    #[derive(Clone)]
    struct Text(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Text {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let writer = Text(log.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the only global subscriber");
        log
    });
    let bytes = log.lock().unwrap().clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Decision 11: a file that still names `[voice] fallback_input` reloads as
/// the same config (so nothing reaches the core and no `config.changed` is
/// raised), and the watcher names the line in the log, as the start and
/// `config.reload` do.
#[test]
fn a_retired_line_changes_nothing_and_the_watcher_names_it() {
    const NAMED: &str = "[voice] fallback_input is no longer read";
    assert!(!warnings().contains(NAMED), "nothing named before the file");
    let dir = data_dir();
    let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
    put(&paths.config, "[api]\nport = 0\n");
    let initial = Config::load(&paths.config).expect("the starting config");
    let bus = Arc::new(Bus::default());
    let (core, core_rx) = CoreHandle::detached_with(bus.clone());
    let watch = config::watch(&paths, initial.clone(), core, bus.clone());
    assert!(watch.watching(), "the recommended watcher is up");

    put(
        &paths.config,
        "[api]\nport = 0\n\n[voice]\nfallback_input = \"system_default\"\n",
    );
    let deadline = Instant::now() + WAIT;
    while !warnings().contains(NAMED) {
        assert!(
            Instant::now() < deadline,
            "not named within {WAIT:?}: {}",
            warnings()
        );
        std::thread::sleep(POLL);
    }
    assert!(
        warnings().contains("Remove the line from config.toml."),
        "{}",
        warnings()
    );
    assert_eq!(*watch.current(), initial, "the line changes nothing");
    assert!(core_rx.try_recv().is_err(), "so the core is not even told");

    drop(watch);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hot_reload_keeps_last_good_on_invalid() {
    let dir = data_dir();
    let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
    put(
        &paths.config,
        "[api]\nport = 0\nallowed_origins = [\"http://localhost:5173\"]\n",
    );
    let initial = Config::load(&paths.config).expect("the starting config");
    assert_eq!(initial.api.allowed_origins, ["http://localhost:5173"]);

    let bus = Arc::new(Bus::default());
    let (core, core_rx) = CoreHandle::detached_with(bus.clone());
    let mut events = bus.subscribe();
    let watch = config::watch(&paths, initial.clone(), core, bus.clone());
    assert!(watch.watching(), "the recommended watcher is up");

    // 1. An invalid file: the last good config stays, a copy is kept and
    //    the error is on the bus with its position.
    put(&paths.config, "[api]\nport = \"not a port\"\n");
    let event = wait_for(&mut events, EventKind::Error);
    assert_eq!(event.data["code"], "config_invalid");
    assert_eq!(event.data["context"], "config.toml");
    assert_eq!(event.data["position"]["line"], 2);
    assert!(
        event.data["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{:?}",
        event.data
    );
    assert_eq!(
        *watch.current(),
        initial,
        "the effective config is unchanged"
    );
    let invalid = dir.join("config.toml.invalid");
    assert_eq!(
        std::fs::read_to_string(&invalid).expect("the .invalid copy"),
        "[api]\nport = \"not a port\"\n"
    );
    assert!(
        core_rx.try_recv().is_err(),
        "an invalid config never reaches the core"
    );

    // 2. A valid change to a hot key: the cell, the core and the event.
    put(
        &paths.config,
        "[api]\nport = 0\nallowed_origins = [\"http://localhost:5173\", \"http://localhost:1234\"]\n",
    );
    let event = wait_for(&mut events, EventKind::ConfigChanged);
    let keys: Vec<String> =
        serde_json::from_value(event.data["keys"].clone()).expect("config.changed{keys}");
    assert_eq!(keys, ["api.allowed_origins".to_string()]);
    let now = watch.current();
    assert_eq!(
        now.api.allowed_origins,
        ["http://localhost:5173", "http://localhost:1234"],
        "a request with that Origin now passes"
    );
    let applied = core_rx
        .recv_timeout(WAIT)
        .expect("CoreMsg::Config reaches the core");
    match applied {
        CoreMsg::Config(config) => assert_eq!(config.api.allowed_origins.len(), 2),
        other => panic!("{other:?}"),
    }

    // 3. A key that is not hot is applied to the cell but logged "restart
    //    needed"; `[log] level` goes through the reload handle.
    put(
        &paths.config,
        "[api]\nport = 47299\n\n[log]\nlevel = \"debug\"\n",
    );
    let event = wait_for(&mut events, EventKind::ConfigChanged);
    let keys: Vec<String> = serde_json::from_value(event.data["keys"].clone()).unwrap();
    assert_eq!(keys, ["api.allowed_origins", "api.port", "log.level"]);
    assert_eq!(watch.current().log.level, "debug");
    assert!(core_rx.recv_timeout(WAIT).is_ok());

    drop(watch);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The same watcher reloads `tokens.toml`, so `deckd token create` works
/// while the daemon runs; an unparsable file keeps the last good table.
#[test]
fn tokens_are_reloaded_through_the_same_watcher() {
    let dir = data_dir();
    let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
    let bus = Arc::new(Bus::default());
    let (core, _core_rx) = CoreHandle::detached_with(bus.clone());
    let mut events = bus.subscribe();
    let watch = config::watch(&paths, Config::default(), core, bus.clone());
    assert!(watch.tokens().load().tokens.is_empty());

    let mut tokens = deckd::tokens::Tokens::default();
    let created = tokens
        .create(
            "gate",
            vec![deck_model::Scope::Observe],
            None,
            None,
            deck_model::Ts(1_789_760_523_000),
        )
        .expect("a token");
    assert!(created.starts_with("dk_"));
    tokens.save(&paths.tokens).expect("tokens.toml");

    let deadline = Instant::now() + WAIT;
    loop {
        if watch.tokens().load().tokens.len() == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "tokens.toml was not reloaded");
        std::thread::sleep(POLL);
    }
    assert_eq!(watch.tokens().load().tokens[0].name, "gate");

    // Garbage keeps the last good table and says so on the bus.
    put(&paths.tokens, "this is not toml [");
    let event = wait_for(&mut events, EventKind::Error);
    assert_eq!(event.data["code"], "tokens_invalid");
    assert_eq!(watch.tokens().load().tokens.len(), 1);

    drop(watch);
    std::fs::remove_dir_all(dir).unwrap();
}
