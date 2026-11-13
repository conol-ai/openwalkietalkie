//! The design's phase-3 gate (R5), as a test: with no deck linked for ten
//! minutes there are no polls, no Keychain reads and no child processes.
//!
//! A real core decides, driven here on a simulated clock: never linked, then
//! linked, then the link drops and ten minutes pass in no time at all, then
//! the deck is back. The collectors are the daemon's own, on real time and
//! sharing the core's gate (`deckd::dormancy`): the liveness pollers, the
//! Codex app-server client, the Claude usage probe and the System sampler.
//! Each reaches out only through a fake that counts:
//!
//! - `claude agents --json`, the one periodic child process, is a script on
//!   a path of its own that writes a line every time it runs;
//! - Codex's app-server control socket is a listener that counts connects;
//! - the Claude usage probe, the only collector that reads the credentials
//!   file and the Claude login in the Keychain and runs `curl`, is a
//!   counter (`run_with`);
//! - the core the collectors call is a responder that counts what the pid
//!   poll releases and what the System sampler sends;
//! - the service's own look for a release (a USB probe and a `curl` of the
//!   release feed) is an offline updater whose looks the core counts;
//! - the cloud voice key's Keychain item (`deckd::secrets`, on a Mac's own
//!   install) is a store that counts its reads and writes. The service
//!   reads it once, when it starts, deck or not; nothing may ask it again
//!   on its own, the ten quiet minutes included.
//!
//! Nothing here runs the operator's own CLI, reads their Keychain or home,
//! or touches the network.
use deck_model::{
    AdapterKind, AgentRegister, AnswerCap, Caps, DeviceLine, FaceEntry, Kind, LinkEvent, OrderCap,
    PidKind, Principal, Request, SessionId, Store, StoreConfig, Template, Ts,
};
use deckd::api::Api;
use deckd::config::{Config, Paths};
use deckd::core::handle::{CoreHandle, CoreMsg};
use deckd::core::Core;
use deckd::dormancy::{Dormancy, Gate, WINDOW};
use deckd::hid::HidHandle;
use deckd::secrets::{KeyHome, MemoryKeyStore, VoiceKey};
use deckd::state::State;
use deckd::tokens::Tokens;
use deckd::trust::FakeVerifier;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WALL: Ts = Ts(1_789_760_523_000);

/// What the collectors did, as the fakes counted it.
#[derive(Clone, Default)]
struct Counts {
    /// Codex control-socket connects.
    accepts: Arc<AtomicUsize>,
    /// Claude usage probes.
    probes: Arc<AtomicUsize>,
    /// `agent.release` from the pid poll.
    releases: Arc<AtomicUsize>,
    /// System samples.
    samples: Arc<AtomicUsize>,
}

/// One look at every counter: `claude agents --json` runs, Codex connects,
/// usage probes, pid-poll releases, System samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    children: usize,
    accepts: usize,
    probes: usize,
    releases: usize,
    samples: usize,
}

impl Counts {
    fn seen(&self, calls: &Path) -> Seen {
        Seen {
            children: std::fs::read_to_string(calls)
                .unwrap_or_default()
                .lines()
                .count(),
            accepts: self.accepts.load(Ordering::SeqCst),
            probes: self.probes.load(Ordering::SeqCst),
            releases: self.releases.load(Ordering::SeqCst),
            samples: self.samples.load(Ordering::SeqCst),
        }
    }
}

/// A fake `claude` that appends a line to `calls` and lists no session.
fn fake_claude(dir: &Path) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(dir).unwrap();
    let bin = dir.join("claude");
    let calls = dir.join("calls");
    let mut file = std::fs::File::create(&bin).unwrap();
    write!(
        file,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '[]\\n'\n",
        calls.display()
    )
    .unwrap();
    drop(file);
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, calls)
}

/// The core's other end, for the collectors: every reducer call answered,
/// what the pid poll releases and what the sampler sends counted.
fn responder(rx: crossbeam_channel::Receiver<CoreMsg>, counts: Counts) {
    std::thread::spawn(move || {
        let mut sessions = 100;
        while let Ok(msg) = rx.recv() {
            match msg {
                CoreMsg::Api { req, reply, .. } => {
                    if matches!(*req, Request::AgentRelease { .. }) {
                        counts.releases.fetch_add(1, Ordering::SeqCst);
                    }
                    let _ = reply.send(Ok(serde_json::json!({"rev": 1})));
                }
                CoreMsg::OpenSession { reply, .. } => {
                    sessions += 1;
                    let _ = reply.send(Ok(SessionId(sessions)));
                }
                CoreMsg::SystemStatus(_) => {
                    counts.samples.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    });
}

/// Until `ready` holds, for at most `budget`.
fn until(what: &str, budget: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + budget;
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The core's ticker from `from` up to `to`, one tick every 10 s of the
/// simulated clock (the implicit heartbeat keeps the leases, as it would).
fn tick_through(core: &mut Core, from: Instant, to: Instant) {
    let mut at = from;
    while at < to {
        core.tick(at);
        at += Duration::from_secs(10);
    }
    core.tick(to);
    core.publish();
}

#[test]
fn ten_minutes_without_a_deck_and_nothing_is_collected() {
    let dir = std::env::temp_dir().join(format!("deckd-dormancy-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Claude's own session registry with a session in it: the roster poll
    // looks for sessions to seed even with nobody on the roster, so without
    // the gate it would run `claude agents --json` every second. This test
    // is a process of its own, and sets this before any thread starts.
    let claude_dir = dir.join("claude");
    std::fs::create_dir_all(claude_dir.join("sessions")).unwrap();
    std::fs::write(claude_dir.join("sessions/4242.json"), "{}").unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &claude_dir);
    let (bin, calls) = fake_claude(&dir.join("bin"));
    let counts = Counts::default();
    let socket = dir.join("codex.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let accepts = counts.accepts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            accepts.fetch_add(1, Ordering::SeqCst);
            // No WebSocket on the other end: the client backs off, as it
            // does from a daemon that is not answering.
            drop(stream);
        }
    });

    let mut config = Config::default();
    config.liveness.claude_bin = bin.display().to_string();
    config.liveness.roster_s = 1;
    config.liveness.pid_s = 1;
    config.codex.daemon = true;
    config.codex.daemon_socket = Some(socket.display().to_string());
    config.usage.providers = vec!["claude".into(), "codex".into()];
    let config = Arc::new(config);

    // The core, on a simulated clock: no deck has ever been linked here.
    let paths = Paths::from_data_dir(dir.join("data"), dir.join("logs"), dir.join("caches"));
    let t0 = Instant::now();
    let (hid, _ops, _lines, _debug) = HidHandle::detached();
    let mut core = Core::new(
        Store::new(StoreConfig::new(t0, WALL, 7)),
        State::default(),
        hid,
        paths.clone(),
    );
    // The cloud voice key in a store, as `run` sets it up on a Mac's own
    // install before the first config: read once, here, at start.
    let keychain = Arc::new(MemoryKeyStore::with("sk-test-0123456789abcd"));
    core.set_voice_key(VoiceKey::open(KeyHome::Store(keychain.clone())));
    let started = (keychain.reads(), keychain.writes());
    assert_eq!(started, (1, 0), "one read when the service starts");
    // The service's own looks for a release (R8) run on this clock too. Its
    // updater never reaches out: a look would open a deck on the cable and
    // fetch the release manifest. Each look is counted below.
    core.firmware = deckd::firmware::Updater::offline();
    core.set_config(config.clone());
    core.set_dormancy(Dormancy::start(WINDOW, None, WALL, t0), t0);
    // An agent the pid poll would look at, with a pid that is gone, on a
    // session the quiet does not release (a Gemini adapter's): what keeps
    // it from being polled is the gate, not its absence.
    let gemini = SessionId(93);
    let adapter = Principal::Adapter(AdapterKind::Gemini);
    core.sessions
        .open_internal(gemini, adapter.clone(), "gemini-adapter", t0);
    let register = AgentRegister {
        kind: Kind::Other,
        name: "gemini".into(),
        key: Some("gemini:s1".into()),
        cwd: None,
        pid: Some(dead_pid()),
        pid_kind: Some(PidKind::Cli),
        terminal: None,
        lease_ttl_s: 30,
        caps: Caps {
            answer: AnswerCap::Poll,
            order: OrderCap::Poll,
            focus: false,
            transcribe: Default::default(),
        },
        slot_hint: None,
        voice: None,
        meta: None,
    };
    core.api_call(adapter, gemini, false, Request::AgentRegister(register), t0)
        .unwrap();
    core.publish();

    // The collectors, sharing the core's gate.
    let (tx, rx) = crossbeam_channel::bounded(1024);
    responder(rx, counts.clone());
    let handle = CoreHandle::with_adapters(tx, core.bus.clone(), core.adapters().clone());
    let live_config = Arc::new(arc_swap::ArcSwap::from(config));
    let api = Arc::new(Api {
        core: handle,
        bus: core.bus.clone(),
        config: live_config.clone(),
        tokens: Arc::new(arc_swap::ArcSwap::from_pointee(Tokens::default())),
        verifier: Arc::new(FakeVerifier::default()),
        adapters: Arc::new(deckd::adapters::Adapters::new(live_config)),
        paths,
        legacy_hooks: false,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    runtime.spawn(deckd::liveness::run(api.clone(), stopped.clone()));
    runtime.spawn(deckd::codexd::run(api.clone(), stopped.clone()));
    runtime.spawn(deckd::system_status::run(api.core.clone(), stopped.clone()));
    let probes = counts.probes.clone();
    runtime.spawn(deckd::usage::claude::run_with(
        api.clone(),
        stopped.clone(),
        move || {
            probes.fetch_add(1, Ordering::SeqCst);
            async { deckd::usage::claude::Outcome::Fed(0) }
        },
    ));
    let nothing = Seen {
        children: 0,
        accepts: 0,
        probes: 0,
        releases: 0,
        samples: 0,
    };

    // Never linked: quiet from the start.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(counts.seen(&calls), nothing, "no deck has ever been linked");
    assert_eq!((keychain.reads(), keychain.writes()), started);

    // A deck links, with neither the Usage nor the System face on its strip:
    // the roster poll, the pid poll and the Codex client run; usage and the
    // sampler wait for their faces.
    core.on_link(LinkEvent::Linked, t0);
    core.on_link(LinkEvent::Live { sid: 7 }, t0);
    let face = |f, t| FaceEntry {
        f,
        t,
        n: String::new(),
    };
    let strip = |core: &mut Core, list: Vec<FaceEntry>| {
        core.on_device_line(DeviceLine::Faces { list }, t0);
        core.tick(t0);
        core.publish();
    };
    strip(&mut core, vec![face(1, Template::Agents)]);
    assert_eq!(
        core.adapters().gate(),
        Gate {
            collect: true,
            usage: false,
            system: false
        }
    );
    until(
        "the pollers and the Codex client run while a deck is linked",
        Duration::from_secs(5),
        || {
            let seen = counts.seen(&calls);
            seen.children > 0 && seen.accepts > 0 && seen.releases > 0
        },
    );
    let seen = counts.seen(&calls);
    assert_eq!(
        (seen.probes, seen.samples),
        (0, 0),
        "no Usage or System face: {seen:?}"
    );
    strip(
        &mut core,
        vec![
            face(1, Template::Agents),
            face(4, Template::Usage),
            face(7, Template::System),
        ],
    );
    until(
        "usage is read and the Mac sampled once their faces are on the strip",
        Duration::from_secs(3),
        || {
            let seen = counts.seen(&calls);
            seen.probes > 0 && seen.samples > 0
        },
    );

    // The link drops. Nine minutes fifty-nine later the collectors still
    // run; at ten minutes they stop.
    let t1 = t0 + Duration::from_secs(60);
    tick_through(&mut core, t0, t1);
    core.on_link(LinkEvent::Down, t1);
    tick_through(&mut core, t1, t1 + WINDOW - Duration::from_secs(1));
    assert!(core.adapters().collecting(), "9:59 after the drop");
    tick_through(&mut core, t1 + WINDOW - Duration::from_secs(1), t1 + WINDOW);
    assert_eq!(core.adapters().gate(), Gate::shut());
    // The service looked for a release once, five minutes after it started,
    // while the deck was still about.
    assert_eq!(core.update_schedule().looks(), 1);
    let quiet = core.store.snapshot().service.dormancy.unwrap();
    assert!(
        !quiet.collecting && quiet.dormant_since.is_some(),
        "{quiet:?}"
    );
    // What was already under way when the gate shut finishes (a probe on
    // the blocking pool, a connect): the counters hold still for 400 ms, or
    // 3 s pass. Then two seconds, in which a collector that ignored the gate
    // would tick at least once, and nothing.
    let mut before = counts.seen(&calls);
    let settled = Instant::now() + Duration::from_secs(3);
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let now = counts.seen(&calls);
        let still = now == before;
        before = now;
        if still || Instant::now() > settled {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(2_000));
    assert_eq!(
        counts.seen(&calls),
        before,
        "with no deck for ten minutes: no `claude` child, no Codex connect, no usage probe \
         (so no credentials file, no Keychain, no curl), no pid poll, no System sample"
    );
    // A look for a release is a `curl` too: none without a deck, however
    // long it stays away.
    let away = t1 + WINDOW + Duration::from_secs(3 * 60 * 60);
    tick_through(&mut core, t1 + WINDOW, away);
    assert_eq!(
        core.update_schedule().looks(),
        1,
        "no look for a release without a deck"
    );
    // Three hours of the core's ticks with no deck, and the cloud voice
    // key's Keychain item was not asked either.
    assert_eq!(
        (keychain.reads(), keychain.writes()),
        started,
        "with no deck for ten minutes: the cloud voice key's Keychain item is not asked either"
    );

    // The deck is back: every collector runs again at once.
    let t2 = away + Duration::from_secs(30);
    core.on_link(LinkEvent::Live { sid: 7 }, t2);
    core.publish();
    until(
        "the collectors start again the moment the deck links",
        Duration::from_secs(5),
        || {
            let seen = counts.seen(&calls);
            seen.children > before.children
                && seen.accepts > before.accepts
                && seen.releases > before.releases
                && seen.samples > before.samples
        },
    );

    // Nor on a deck's return: the key is in force from the start's read.
    assert_eq!((keychain.reads(), keychain.writes()), started);

    let _ = stop.send(true);
    runtime.shutdown_timeout(Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(dir);
}

/// A pid that certainly belongs to nothing: a child spawned and reaped
/// here, so the number was real and is now free.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!deckd::liveness::alive(pid), "pid {pid} was recycled");
    pid
}
