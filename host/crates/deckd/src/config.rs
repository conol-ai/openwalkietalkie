//! `config.toml` (design §9): every key optional, defaults as documented,
//! unknown keys ignored; the data/log paths of the daemon; and the hot
//! reload (`watch`, contract §3.7): a notify recommended watcher on the
//! data dir, a 300 ms debounce, whole-file validation into an `ArcSwap`,
//! `CoreMsg::Config` and `config.changed{keys}` on success, and on failure
//! the last good config kept, a `config.toml.invalid` copy and an
//! `error{code:"config_invalid"}` event. `tokens.toml` rides the same
//! watcher, and `[log] level` is applied through the `reload` layer
//! `service::init_logging` installed.
use crate::bus::{Bus, Published};
use crate::core::handle::{CoreHandle, CoreMsg};
use crate::tokens::Tokens;
use arc_swap::ArcSwap;
use deck_model::{Event, EventKind, Principal};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// File changes are coalesced over this.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the watch thread wakes to check the debounce and the stop flag.
const WATCH_POLL: Duration = Duration::from_millis(25);
/// The keys a reload applies without a restart (`<table>.<key>`, or
/// `<table>.*`); everything else is logged "restart needed".
pub const HOT_KEYS: [&str; 18] = [
    "api.allowed_origins",
    // `deckd::codexd` reads `[codex]` on every connect attempt.
    "codex.*",
    "api.uds_default_scope",
    "api.uds_max_scope",
    "api.mailbox_ttl_s",
    "api.max_held_hooks",
    "trust.*",
    "privacy.log_content",
    "agents.*",
    "cards.ready_ttl_s",
    // Week 6: the tray's "Pause hooks" is `config.set hooks.enabled=false`,
    // and a pause that needs the process restarted is not a pause. It was
    // **already** read live — `Adapters::enabled()` reads the `ArcSwap`
    // cell on every hook request (`api::hooks`) — so this list was simply
    // missing it, the same way it was missing the whole of `[voice]` until
    // B14. A key that is applied and reported "restart needed" is the
    // mirror of B14's failure and just as wrong.
    "hooks.enabled",
    "hooks.claude.hold_s",
    // Read per hook through `Adapters::cfg`, like `hold_s`.
    "hooks.claude.settle_s",
    "hooks.codex.hold_s",
    "hooks.gemini.hold_s",
    "liveness.*",
    // B14: `[voice]` was not hot, so a `config.reload` that changed the
    // STT locale or the device selection reported them as applied and
    // changed nothing until a restart. The whole table is hot because
    // every key in it is read where it is used — the locale at the next
    // `AudioCmd::Listen`, the device selection at the next open — so there
    // is no key in it that needs the process rebuilt. The one exception
    // came later: `voice.transport` picks the devices the engine is built
    // around, and is in `RESTART_KEYS`.
    "voice.*",
    "log.level",
];

/// Keys inside a hot table that still need a restart: they are read once,
/// when the thing they choose is built.
///
/// `voice.transport` picks which devices `deckd-audio` is built around (the
/// BLE link and its threads, or the CoreAudio devices and their property
/// watcher), and that happens in `audio::spawn` and nowhere else. Swapping
/// them under a live engine would mean tearing down a capture and an output
/// mid-session for a setting nobody changes twice, so a reload reports it
/// under "restart needed" instead of claiming it was applied.
pub const RESTART_KEYS: [&str; 1] = ["voice.transport"];

/// The `[voice.tts]` keys the **sidecar** is started with (`audio::sidecar`),
/// read once when it is spawned. They need a restart only while the local
/// model is the synthesiser — before or after the change — which is why
/// they are not in [`RESTART_KEYS`] but in [`is_hot_change`]: the cloud
/// engine reads the same keys live, and a key that is applied and reported
/// "restart needed" is as wrong as the reverse.
pub const SIDECAR_KEYS: [&str; 7] = [
    "voice.tts.engine",
    "voice.tts.model",
    "voice.tts.voices",
    "voice.tts.lang",
    "voice.tts.instruct",
    "voice.tts.python",
    "voice.tts.interval_s",
];

/// A changed key is applied live (`HOT_KEYS`; `<table>.*` covers every key
/// of that table, pins included), unless it is one of [`RESTART_KEYS`].
///
/// This is the key's own answer; [`is_hot_change`] is the one to ask when
/// both configs are at hand, because the sidecar's keys are hot or not
/// depending on which synthesiser is selected.
pub fn is_hot(key: &str) -> bool {
    if RESTART_KEYS.contains(&key) {
        return false;
    }
    HOT_KEYS.iter().any(|hot| match hot.strip_suffix(".*") {
        Some(prefix) => key
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.')),
        None => key == *hot,
    })
}

/// Whether changing `key` from `before` to `after` is applied live.
///
/// [`is_hot`], and on top of it the one engine-dependent rule: a
/// [`SIDECAR_KEYS`] key needs a restart when the local model is the
/// synthesiser on either side of the change (`[voice.tts] engine = "mlx"`),
/// because the sidecar is built with it and nothing re-reads it; with the
/// cloud or Apple engine the same key is read at the next utterance.
pub fn is_hot_change(key: &str, before: &Config, after: &Config) -> bool {
    if !is_hot(key) {
        return false;
    }
    if SIDECAR_KEYS.contains(&key) {
        let sidecar = |c: &Config| c.voice.tts.engine == crate::audio::sidecar::NAME;
        return !(sidecar(before) || sidecar(after));
    }
    true
}

/// The dotted keys whose value differs (`api.port`, `hooks.claude.hold_s`,
/// `agents."claude:cohdl-api".slot`), in a stable order. A config that
/// cannot be serialized (impossible for these types) reports no change
/// rather than a wrong one.
pub fn changed_keys(before: &Config, after: &Config) -> Vec<String> {
    let (Ok(a), Ok(b)) = (serde_json::to_value(before), serde_json::to_value(after)) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    diff("", &a, &b, &mut keys);
    keys
}

fn diff(prefix: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            let names: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for name in names {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                match (x.get(name), y.get(name)) {
                    (Some(a), Some(b)) => diff(&path, a, b, out),
                    _ => out.push(path),
                }
            }
        }
        _ if a != b => out.push(prefix.to_string()),
        _ => {}
    }
}

/// The 1-based line and column of a byte offset in `text`.
fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let mut end = offset.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let upto = &text[..end];
    let line = 1 + upto.matches('\n').count();
    let col = 1 + upto.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line as u32, col as u32)
}

/// The uid the daemon runs as: a `config.toml` edit is the operator's.
#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: `getuid` reads the process's own real uid and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

/// The watcher's handle: the effective config and token cells, and the
/// watcher itself — dropping the handle stops it and joins its thread.
pub struct WatchHandle {
    config: Arc<ArcSwap<Config>>,
    tokens: Arc<ArcSwap<Tokens>>,
    core: CoreHandle,
    bus: Arc<Bus>,
    stop: Arc<AtomicBool>,
    watcher: Option<RecommendedWatcher>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WatchHandle {
    /// The cell the doors read.
    pub fn config(&self) -> Arc<ArcSwap<Config>> {
        self.config.clone()
    }

    /// The cell the TCP door verifies bearer tokens against.
    pub fn tokens(&self) -> Arc<ArcSwap<Tokens>> {
        self.tokens.clone()
    }

    /// The effective config now.
    pub fn current(&self) -> Arc<Config> {
        self.config.load_full()
    }

    /// False when no file watcher could be started: edits need a restart.
    pub fn watching(&self) -> bool {
        self.watcher.is_some()
    }

    pub fn core(&self) -> &CoreHandle {
        &self.core
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Dropping the watcher closes the event channel, so the thread's
        // `recv_timeout` returns at once instead of after `WATCH_POLL`.
        drop(self.watcher.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Watch `paths.data` for `config.toml` and `tokens.toml` changes and
/// apply them (§3.7). Never fails: without a watcher the handle simply
/// carries `initial` and says so.
pub fn watch(paths: &Paths, initial: Config, core: CoreHandle, bus: Arc<Bus>) -> WatchHandle {
    let config = Arc::new(ArcSwap::from_pointee(initial));
    let tokens = Arc::new(ArcSwap::from_pointee(match Tokens::load(&paths.tokens) {
        Ok(tokens) => tokens,
        Err(e) => {
            tracing::warn!(error = %e, "tokens.toml unreadable: starting with no tokens");
            Tokens::default()
        }
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    // FSEvents reports canonical paths, so watch the dir by its real one.
    let dir = std::fs::canonicalize(&paths.data).unwrap_or_else(|_| paths.data.clone());
    let watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(mut watcher) => match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => Some(watcher),
            Err(e) => {
                tracing::warn!(error = %e, path = %dir.display(), "no config hot reload: cannot watch the data dir");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "no config hot reload: no file watcher");
            None
        }
    };
    let join = watcher.as_ref().and_then(|_| {
        let reloader = Reloader {
            paths: paths.clone(),
            config: config.clone(),
            tokens: tokens.clone(),
            core: core.clone(),
            bus: bus.clone(),
            stop: stop.clone(),
        };
        std::thread::Builder::new()
            .name("deckd-config".into())
            .spawn(move || reloader.run(rx))
            .ok()
    });
    if join.is_some() {
        tracing::info!(path = %paths.config.display(), "config hot reload armed");
    }
    WatchHandle {
        config,
        tokens,
        core,
        bus,
        stop,
        watcher,
        join,
    }
}

/// The `deckd-config` thread: debounce the watcher's events, then re-read
/// whichever file changed.
struct Reloader {
    paths: Paths,
    config: Arc<ArcSwap<Config>>,
    tokens: Arc<ArcSwap<Tokens>>,
    core: CoreHandle,
    bus: Arc<Bus>,
    stop: Arc<AtomicBool>,
}

impl Reloader {
    fn run(self, rx: Receiver<notify::Result<notify::Event>>) {
        let mut due: Option<Instant> = None;
        let (mut config_dirty, mut tokens_dirty) = (false, false);
        while !self.stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(WATCH_POLL) {
                Ok(Ok(event)) => {
                    for path in &event.paths {
                        match path.file_name().and_then(|n| n.to_str()) {
                            Some("config.toml") => config_dirty = true,
                            Some("tokens.toml") => tokens_dirty = true,
                            _ => {}
                        }
                    }
                    if config_dirty || tokens_dirty {
                        due = Some(Instant::now() + WATCH_DEBOUNCE);
                    }
                }
                Ok(Err(e)) => tracing::warn!(error = %e, "file watcher error"),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if due.is_some_and(|at| Instant::now() >= at) {
                due = None;
                if std::mem::take(&mut config_dirty) {
                    self.reload_config();
                }
                if std::mem::take(&mut tokens_dirty) {
                    self.reload_tokens();
                }
            }
        }
    }

    /// Whole-file validation: anything wrong keeps the last good config.
    fn reload_config(&self) {
        let text = match std::fs::read_to_string(&self.paths.config) {
            Ok(text) => text,
            // A deleted file is the documented defaults, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                self.config_invalid(format!("cannot read config.toml: {e}"), None);
                return;
            }
        };
        let next = match Config::parse_at(&text) {
            Ok(next) => next,
            Err((message, position)) => {
                let copy = self.paths.config.with_extension("toml.invalid");
                if let Err(e) = std::fs::copy(&self.paths.config, &copy) {
                    tracing::warn!(error = %e, "cannot keep a copy of the invalid config.toml");
                }
                tracing::warn!(
                    %message,
                    ?position,
                    "config.toml invalid: the last good config stays in force"
                );
                self.config_invalid(message, position);
                return;
            }
        };
        let current = self.config.load_full();
        if *current == next {
            return;
        }
        let keys = changed_keys(&current, &next);
        if keys.iter().any(|key| key == "log.level") {
            match crate::service::set_log_level(&next.log.level) {
                Ok(true) => tracing::info!(level = %next.log.level, "[log] level applied"),
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "[log] level not applied"),
            }
        }
        for key in keys.iter().filter(|key| !is_hot_change(key, &current, &next)) {
            tracing::warn!(%key, "changed in config.toml: restart needed to apply it");
        }
        let next = Arc::new(next);
        self.config.store(next.clone());
        if !self.core.send_blocking(CoreMsg::Config(next)) {
            tracing::warn!("the core is gone: the reloaded config reached the doors only");
        }
        tracing::info!(keys = %keys.join(","), "config.toml reloaded");
        self.publish(
            EventKind::ConfigChanged,
            serde_json::json!({ "keys": keys }),
        );
    }

    /// `deckd token create` while the daemon runs: the table is re-read.
    fn reload_tokens(&self) {
        match Tokens::load(&self.paths.tokens) {
            Ok(next) => {
                if *self.tokens.load_full() == next {
                    return;
                }
                let n = next.tokens.len();
                self.tokens.store(Arc::new(next));
                tracing::info!(tokens = n, "tokens.toml reloaded");
            }
            Err(e) => {
                tracing::warn!(error = %e, "tokens.toml invalid: the last good table stays");
                self.publish(
                    EventKind::Error,
                    serde_json::json!({
                        "code": "tokens_invalid",
                        "context": "tokens.toml",
                        "message": e.to_string(),
                    }),
                );
            }
        }
    }

    fn config_invalid(&self, message: String, position: Option<(u32, u32)>) {
        let mut data = serde_json::json!({
            "code": "config_invalid",
            "context": "config.toml",
            "message": message,
        });
        if let Some((line, col)) = position {
            data["position"] = serde_json::json!({ "line": line, "col": col });
        }
        self.publish(EventKind::Error, data);
    }

    /// The watcher owns no `rev`: the event carries the store's current one
    /// (like the core's own `error` events, §3.1).
    fn publish(&self, kind: EventKind, data: serde_json::Value) {
        self.bus.publish(Published {
            event: Arc::new(Event {
                rev: self.bus.rev(),
                ts: crate::core::wall_now(),
                kind,
                principal: Principal::Uid(uid()),
                data,
            }),
            owner: None,
        });
    }
}

fn t() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Api {
    pub port: u16,
    pub allowed_origins: Vec<String>,
    pub uds_default_scope: String,
    pub uds_max_scope: String,
    pub mailbox_ttl_s: u32,
    /// §4.1: how many hooks may be parked at once, across every CLI and
    /// both doors. Each one is a card on the deck, a tokio task and a CLI
    /// waiting, so the ceiling is the operator's attention, not memory.
    /// Past it a hold is refused `-32008` and the hook fails open — the
    /// CLI shows its own dialog, which is the right answer when the deck
    /// already has more than anyone can work through.
    pub max_held_hooks: u32,
}

impl Default for Api {
    fn default() -> Self {
        Api {
            port: 47201,
            allowed_origins: Vec::new(),
            uds_default_scope: "observe".into(),
            uds_max_scope: "admin".into(),
            mailbox_ttl_s: 600,
            max_held_hooks: 64,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Trust {
    pub require_trusted_for_allow: bool,
    pub confirm: String,
    pub gui_requirement: String,
    pub dev_requirement: Option<String>,
}

impl Default for Trust {
    fn default() -> Self {
        Trust {
            require_trusted_for_allow: true,
            confirm: "sheet".into(),
            gui_requirement: r#"anchor apple generic and identifier "ai.cohdl.openwalkietalkie" and certificate leaf[subject.OU] = "TEAMID""#.into(),
            dev_requirement: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Privacy {
    pub keep_tool_input: String,
    pub keep_prompts: bool,
    pub keep_replies: String,
    pub transcript_log: bool,
    pub log_content: bool,
    pub event_ring: usize,
}

impl Default for Privacy {
    fn default() -> Self {
        Privacy {
            keep_tool_input: "pending".into(),
            keep_prompts: false,
            keep_replies: "last".into(),
            transcript_log: false,
            log_content: false,
            event_ring: 4096,
        }
    }
}

/// One `[agents]` pin: `{ slot = 1, voice = "…", name = "…" }`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Pin {
    pub slot: Option<u8>,
    pub voice: Option<String>,
    pub name: Option<String>,
}

/// `[agents]`: the scalar keys plus the pins (any other key is a pin).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Agents {
    pub grace_s: u32,
    pub evict: String,
    #[serde(flatten)]
    pub pins: BTreeMap<String, Pin>,
}

impl Default for Agents {
    fn default() -> Self {
        Agents {
            grace_s: 300,
            evict: "lru_idle".into(),
            pins: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct HooksClaude {
    /// The Claude adapter's hold budget, in seconds. It is the **adapter's**
    /// input, not a store setting: the adapter puts it on every held card as
    /// `CardSet.timeout_s`, which the store honours over its own default and
    /// shares across a whole question group. `StoreConfig::hold_s` stays the
    /// daemon-wide fallback for a held card that names no budget of its own.
    pub hold_s: u32,
    pub tool_titles: bool,
    /// How long a permission ask waits, unseen, for an auto-approver (auto
    /// mode's classifier, the host app's approver) or the terminal to
    /// decide it before it reaches the deck. Claude's own "needs your
    /// permission" notification shows it sooner. 0 shows every ask at once.
    pub settle_s: u32,
    pub report_via: String,
    pub desk_presence: bool,
}

impl Default for HooksClaude {
    fn default() -> Self {
        HooksClaude {
            hold_s: 120,
            tool_titles: false,
            settle_s: crate::adapters::SETTLE_S,
            report_via: "http".into(),
            desk_presence: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct HooksCodex {
    /// The Codex adapter's hold budget, in seconds; see `HooksClaude::hold_s`
    /// — it reaches the store only as the `CardSet.timeout_s` the adapter
    /// writes, so the two CLIs really do get different budgets (B2).
    pub hold_s: u32,
}

impl Default for HooksCodex {
    fn default() -> Self {
        HooksCodex { hold_s: 120 }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct HooksGemini {
    pub permission: String,
}

impl Default for HooksGemini {
    fn default() -> Self {
        HooksGemini {
            permission: "notify".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Hooks {
    pub enabled: bool,
    pub claude: HooksClaude,
    pub codex: HooksCodex,
    pub gemini: HooksGemini,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            enabled: true,
            claude: HooksClaude::default(),
            codex: HooksCodex::default(),
            gemini: HooksGemini::default(),
        }
    }
}

/// `[liveness]` — the two pollers of design §5.1 (week-3 contract §1.5).
///
/// Every key is hot: the loop reads this table on each tick, so a
/// `config.reload` that turns the pollers off stops the next one.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Liveness {
    /// Both pollers. Off leaves an agent on the deck until its CLI says
    /// `SessionEnd` — which a killed CLI never does.
    pub enabled: bool,
    /// How often `claude agents --json` is read. The call is a process
    /// spawn (0.12–0.17 s measured with six sessions) run on
    /// `spawn_blocking`, and it is skipped entirely while no Claude
    /// session is on the roster and `seed` is off.
    pub roster_s: u32,
    /// How often a **resolved CLI pid** is probed with `kill(pid, 0)`.
    /// An agent whose `pid_kind` is not `cli` is never probed (B1).
    pub pid_s: u32,
    /// The CLI the roster poll runs, resolved on `PATH`.
    pub claude_bin: String,
    /// Put a Claude session the deck has never seen a hook from on the
    /// roster — one that started before the daemon, or survived a daemon
    /// restart. On by default since 2026-10-10: a seeded agent is reachable
    /// (its inbox socket comes from `sessions/<pid>.json`, its last reply
    /// from the transcript), its next hook completes it, and only a dialog
    /// already open when it was seeded is beyond answering. Off leaves such
    /// a session invisible until its next hook.
    pub seed: bool,
}

impl Default for Liveness {
    fn default() -> Self {
        Liveness {
            enabled: true,
            roster_s: crate::liveness::ROSTER_S as u32,
            pid_s: crate::liveness::PID_S as u32,
            claude_bin: "claude".into(),
            seed: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Cards {
    pub paging: bool,
    pub paging_back: bool,
    pub ready_ttl_s: u32,
}

impl Default for Cards {
    fn default() -> Self {
        Cards {
            paging: false,
            paging_back: true,
            ready_ttl_s: 900,
        }
    }
}

/// `[voice.stt]`: the recogniser. `"apple"` (the default) is Apple's
/// on-device recogniser; `"whisper"` is a Whisper model on this Mac
/// (`audio::whisper`), whose weights are downloaded only when the operator
/// opts in (`deckd stt install`, the app's first-start prompt) and which
/// hands every press to Apple's until they are. Every key here is hot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Stt {
    pub engine: String,
    pub locale: String,
    pub allow_server: bool,
    /// `"whisper"`: which model (`crate::stt::MODELS`); empty is
    /// `large-v3-turbo`.
    pub model: String,
    /// `"whisper"`: `"auto"` detects the language per press (Chinese and
    /// English in one utterance), an ISO 639-1 code pins it, empty follows
    /// `locale`.
    pub language: String,
    /// `"whisper"`: decoder threads; `0` is the machine's own count.
    pub threads: u16,
    /// Whether the operator has been asked once, at the app's first
    /// start, about downloading a local model. Set by the app with the
    /// answer, so the question is asked once per Mac and not once per
    /// launch.
    pub prompted: bool,
    /// Let a native companion that registered `caps.transcribe: "push"`
    /// (Conol) transcribe the presses addressed to its own agents with its
    /// own recogniser. The audio of such a press goes to that app — and
    /// from there to its service — instead of staying on this Mac; the
    /// local recogniser still runs and answers whenever the companion
    /// cannot. Read at every press.
    pub companion: bool,
}

impl Default for Stt {
    fn default() -> Self {
        Stt {
            engine: "apple".into(),
            locale: "en-US".into(),
            allow_server: false,
            model: String::new(),
            language: "auto".into(),
            threads: 0,
            prompted: false,
            companion: true,
        }
    }
}

/// `[voice.tts]`: the synthesiser.
///
/// Bring your own key: with `engine = "cloud"` (the default) and an
/// `api_key`, the announcements are synthesised by `provider`'s hosted
/// model; without a key Apple's synthesiser speaks, on the machine. The
/// cloud keys are read at every utterance, so a key pasted into this file
/// takes effect at the next announcement. The same `model`, `voices`,
/// `lang` and `instruct` keys configure the local model when `engine =
/// "mlx"`, and are then read once, when its sidecar is started
/// ([`SIDECAR_KEYS`]); `announce_max_chars` is hot everywhere.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Tts {
    /// `"cloud"` (the default): `provider`'s model behind `api_key`, Apple's
    /// synthesiser when there is no key or the provider refuses
    /// (`audio::cloud`); `"kokoro"`: a Kokoro voice on this Mac
    /// (`audio::kokoro`), whose package is downloaded only on opt-in
    /// (`deckd tts install`, the app) and which hands every utterance to
    /// Apple's until it is; `"mlx"`: a neural model on this Mac's GPU in
    /// the sidecar `deckd tts install --mlx` sets up (`audio::sidecar`),
    /// Apple's behind it; `"apple"`: `AVSpeechSynthesizer` alone.
    pub engine: String,
    pub announce_max_chars: usize,
    /// `"openai"` or `"elevenlabs"`: whose model and key.
    pub provider: String,
    /// The provider's API key. **Empty means Apple speaks.** The key never
    /// appears on a command line or in the log; `deckd tts key set` writes
    /// it here.
    pub api_key: String,
    /// The model: the provider's (`gpt-4o-mini-tts`, `eleven_flash_v2_5`)
    /// or, for `"mlx"`, the Hugging Face id the sidecar loads. Empty is the
    /// engine's default.
    pub model: String,
    /// The voices agents are assigned from, in the engine's own names
    /// (OpenAI's `coral`…, an ElevenLabs voice name or id, Qwen3-TTS's
    /// `Ryan`…). Empty is the engine's default pool. A `[voice.voices]
    /// default_pool` name the engine does not have is mapped onto one of
    /// these, stably.
    pub voices: Vec<String>,
    /// The language code; `"auto"` lets the model detect one per utterance.
    pub lang: String,
    /// A delivery instruction sent with every utterance, for the models
    /// that take one (OpenAI `gpt-4o-mini-tts`, Qwen3-TTS CustomVoice:
    /// "calm and clear").
    pub instruct: Option<String>,
    /// `"mlx"` only: the Python to run the sidecar with; unset is the
    /// environment `deckd tts install` makes under the caches directory.
    pub python: Option<String>,
    /// `"mlx"` only: seconds of audio per streamed chunk; smaller starts
    /// playback sooner, at some cost per chunk.
    pub interval_s: f32,
    /// `"kokoro"`: decoder threads; `0` is a sensible default.
    pub threads: u16,
}

impl Default for Tts {
    fn default() -> Self {
        Tts {
            engine: "cloud".into(),
            announce_max_chars: 120,
            provider: "openai".into(),
            api_key: String::new(),
            model: String::new(),
            voices: Vec::new(),
            lang: "auto".into(),
            instruct: None,
            python: None,
            interval_s: 0.32,
            threads: 0,
        }
    }
}

/// The key stays out of every `{:?}`: `Config` is logged on reload and in
/// the diagnostics bundle, and a key is the one value in it that must not
/// be.
impl std::fmt::Debug for Tts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tts")
            .field("engine", &self.engine)
            .field("announce_max_chars", &self.announce_max_chars)
            .field("provider", &self.provider)
            .field("api_key", &self.api_key_hint())
            .field("model", &self.model)
            .field("voices", &self.voices)
            .field("lang", &self.lang)
            .field("instruct", &self.instruct)
            .field("python", &self.python)
            .field("interval_s", &self.interval_s)
            .field("threads", &self.threads)
            .finish()
    }
}

impl Tts {
    /// Whether an API key is configured (whitespace is not a key).
    #[must_use]
    pub fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    /// The key as it may be shown: its length and last four characters,
    /// enough to tell two keys apart and nothing more. `"(none)"` without
    /// one.
    #[must_use]
    pub fn api_key_hint(&self) -> String {
        let key = self.api_key.trim();
        if key.is_empty() {
            return "(none)".into();
        }
        let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        if key.chars().count() <= 8 {
            format!("{} chars", key.chars().count())
        } else {
            format!("…{tail} ({} chars)", key.chars().count())
        }
    }

    /// Which synthesiser the agents' pool should come from and the status
    /// should name: `"openai"`/`"elevenlabs"` when the cloud engine has a
    /// key, `"kokoro"` or `"mlx"` for a local model (whether or not it is
    /// downloaded yet — `voice.engines` checks the disk), else `"apple"`.
    #[must_use]
    pub fn effective_engine(&self) -> &str {
        match self.engine.as_str() {
            crate::audio::sidecar::NAME => crate::audio::sidecar::NAME,
            crate::kokoro::ENGINE => crate::kokoro::ENGINE,
            crate::audio::cloud::NAME if self.has_key() => self.provider.as_str(),
            _ => "apple",
        }
    }
}

impl Config {
    /// The voices agents are assigned from (§7.7): the selected engine's
    /// own, so an agent gets a voice the engine has — `[voice.tts] voices`
    /// when set, else the engine's default pool — and `[voice.voices]
    /// default_pool` (Apple's names) when Apple speaks.
    #[must_use]
    pub fn voice_pool(&self) -> Vec<String> {
        let tts = &self.voice.tts;
        let own: Option<&[&str]> = match tts.effective_engine() {
            "apple" => None,
            crate::audio::sidecar::NAME => Some(&crate::audio::sidecar::DEFAULT_VOICES),
            crate::kokoro::ENGINE => Some(&crate::kokoro::DEFAULT_VOICES),
            provider => crate::audio::cloud::Provider::parse(provider).map(crate::audio::cloud::Provider::default_voices),
        };
        match own {
            None => self.voice.voices.default_pool.clone(),
            Some(_) if !tts.voices.is_empty() => tts.voices.clone(),
            Some(pool) => pool.iter().map(|v| (*v).to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Voices {
    pub default_pool: Vec<String>,
}

impl Default for Voices {
    fn default() -> Self {
        Voices {
            default_pool: ["Samantha", "Daniel", "Karen", "Rishi"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// `[voice] transport`: how the deck's microphone and speaker reach the Mac.
///
/// Spelled as an enum rather than a free string like the other `[voice]`
/// keys so that a typo fails the whole-file validation (§3.7: the last good
/// config stays in force, `error{code:"config_invalid"}` names the line)
/// instead of quietly selecting whichever branch a string compare falls
/// into.
pub use deck_model::VoiceTransport;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Voice {
    /// `"ble"` (default) or `"coreaudio"`: see [`VoiceTransport`]. Read
    /// once, when the engine is built ([`RESTART_KEYS`]). With `"ble"`,
    /// `mic_warm` is treated as `"never"` — the deck starts streaming on the
    /// press itself, so there is no bring-up to hide — and `speak_over_sco`
    /// has no effect, because there is no SCO tail to wait out.
    pub transport: VoiceTransport,
    pub device_uid: Option<String>,
    pub fallback_output: String,
    pub fallback_input: String,
    /// `"never" | "card_pending" | "always"` — when the input stream may
    /// exist (§7.1).
    ///
    /// **`card_pending` by default, and the design's `never` was wrong**
    /// (V1, measured 2026-09-21): SCO bring-up from start-IO to the first
    /// **non-zero** mic sample is 1031 ms median (815–1246, n=16) and the
    /// design's own rule was "switch to `card_pending` if bring-up is over
    /// 300 ms". A second of the operator's first sentence is lost on a cold
    /// press, and a pending Question card is exactly when the next press is
    /// coming.
    pub mic_warm: String,
    /// Play the post-release announcement at the SCO rate instead of
    /// waiting for A2DP (§16 Q2's `speak_over_sco`).
    ///
    /// **The key exists and is on** (V1): A2DP resume measured 2307 ms
    /// median (2281–2447, n=16) against a 1.5 s rule, so waiting means the
    /// answer to every PTT arrives two and a half seconds after the
    /// operator let go.
    ///
    /// It never plays while capture is live: V5 measured the deck's own
    /// speaker→mic loopback at ≈ +17 dB, so our own TTS would go straight
    /// back into the recogniser.
    pub speak_over_sco: bool,
    /// §7.5's "the bring-up was slow" threshold, in ms, reported as
    /// `voice.route{slow_sco_ms}` and kept in `metrics.get.sco`.
    ///
    /// The design said 800 ms, **below the measured median** — it would
    /// fire on every press and mean nothing (V2). Keyed above the measured
    /// worst case (1246 ms) instead, and measured from the press to the
    /// first **non-zero sample**, never to the first callback: callbacks
    /// start at ~28 ms with `noErr` and every sample in them is digital
    /// zero for about a second.
    pub slow_sco_ms: u32,
    pub commands: bool,
    pub announce_questions: bool,
    pub stt: Stt,
    pub tts: Tts,
    pub voices: Voices,
}

impl Default for Voice {
    fn default() -> Self {
        Voice {
            transport: VoiceTransport::Ble,
            device_uid: None,
            fallback_output: "none".into(),
            fallback_input: "system_default".into(),
            // V1/V2, measured 2026-09-21; see the fields above.
            mic_warm: "card_pending".into(),
            speak_over_sco: true,
            slow_sco_ms: 1500,
            commands: true,
            announce_questions: false,
            stt: Stt::default(),
            tts: Tts::default(),
            voices: Voices::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Desktop {
    pub inject: Vec<String>,
    #[serde(default = "t")]
    pub focus: bool,
}

impl Default for Desktop {
    fn default() -> Self {
        Desktop {
            inject: vec!["ghostty".into()],
            focus: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Device {
    pub pace_ms: u16,
    pub hello_s: u32,
    pub clock: String,
    pub usage_period: String,
}

impl Default for Device {
    fn default() -> Self {
        Device {
            pace_ms: 8,
            hello_s: 5,
            clock: "local".into(),
            usage_period: "5h / 7d".into(),
        }
    }
}

impl Device {
    /// The pace floor is 8 ms whatever the file says (§6.1).
    pub fn pace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(u64::from(self.pace_ms.max(8)))
    }
}

/// `[macropad."<name>"] keys = [{ l, b?, action? }]`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct MacroKeyConfig {
    pub l: String,
    pub b: Option<String>,
    pub action: Option<toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct MacropadLayout {
    pub keys: Vec<MacroKeyConfig>,
}

/// `[usage]`: the Usage face's feeders (`crate::usage`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct UsageCfg {
    /// The providers to probe and draw, in the face's own order: `claude`
    /// (the `/api/oauth/usage` probe with the Claude Code login, and the
    /// status line's `rate_limits` when one is chained; the older
    /// `claude-statusline` still names it) and `codex` (the app-server's
    /// rate limits). A provider left out is never probed.
    pub providers: Vec<String>,
    /// How often the Claude probe runs, in seconds; 60 at least. Codex
    /// pushes its own updates after every turn.
    pub poll_s: u32,
}

impl Default for UsageCfg {
    fn default() -> Self {
        UsageCfg {
            providers: vec!["claude".into(), "codex".into()],
            poll_s: 300,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Log {
    pub level: String,
}

impl Default for Log {
    fn default() -> Self {
        Log {
            level: "info".into(),
        }
    }
}

/// `[codex]`: the app-server daemon client (`deckd::codexd`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Codex {
    /// Attach to the shared local app-server daemon the Codex TUI runs
    /// through (codex-cli 0.161+): every loaded thread becomes an agent,
    /// its approvals and questions cards, and a spoken order a
    /// `turn/steer`. Off leaves Codex to the hooks alone.
    pub daemon: bool,
    /// The daemon's control socket. Unset: `$CODEX_HOME/app-server-control/
    /// app-server-control.sock`, else `~/.codex/…`.
    pub daemon_socket: Option<String>,
}

impl Default for Codex {
    fn default() -> Self {
        Codex {
            daemon: true,
            daemon_socket: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Config {
    pub api: Api,
    pub codex: Codex,
    pub trust: Trust,
    pub privacy: Privacy,
    pub agents: Agents,
    pub hooks: Hooks,
    pub liveness: Liveness,
    pub cards: Cards,
    pub voice: Voice,
    pub desktop: Desktop,
    pub device: Device,
    pub macropad: BTreeMap<String, MacropadLayout>,
    pub usage: UsageCfg,
    pub log: Log,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config.toml: {0}")]
    Parse(String),
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// `parse`, with the TOML error's own one-line message and the 1-based
    /// line and column it points at when it has a span. The rendered
    /// `Display` is not used on purpose: it quotes the offending source
    /// line, and this message goes out on the bus as an event.
    pub fn parse_at(text: &str) -> Result<Config, (String, Option<(u32, u32)>)> {
        toml::from_str::<Config>(text).map_err(|e| {
            let at = e.span().map(|span| line_col(text, span.start));
            (e.message().to_string(), at)
        })
    }

    /// Defaults when the file is absent; an error when it is unreadable or invalid.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Pins in the model's shape.
    pub fn pins(&self) -> BTreeMap<String, deck_model::PinConfig> {
        self.agents
            .pins
            .iter()
            .map(|(key, pin)| {
                (
                    key.clone(),
                    deck_model::PinConfig {
                        slot: pin.slot.map(deck_model::Slot),
                        voice: pin.voice.clone(),
                        name: pin.name.clone(),
                    },
                )
            })
            .collect()
    }
}

/// What `config.set` could not do to `config.toml` (§4.11).
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("{0} is not a key of config.toml")]
    Key(String),
    #[error("{key} cannot be {why}")]
    Value { key: String, why: String },
    #[error("config.toml is not valid TOML: {0}")]
    Unparsable(String),
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// One dotted key written into `text`, with **everything else returned
/// exactly as it was** — every other key, the order, the blank lines and
/// the operator's comments (§4.11).
///
/// That is the whole reason this goes through `toml_edit` rather than a
/// serde round trip: `toml::to_string(&Config)` would come back with all
/// ~60 documented defaults written out as explicit keys and not one comment,
/// so the first `config.set` from the GUI would silently rewrite a file the
/// operator maintains by hand.
///
/// `key` is the dotted name `config.get` publishes, which is also the TOML
/// path (`api.port`, `hooks.claude.hold_s`, `voice.stt.locale`). A JSON
/// `null` **removes** the key, so the documented default applies again —
/// there is no TOML null to write, and "unset it" is the thing a GUI
/// control with a "Default" position needs to say.
///
/// # Errors
/// [`EditError::Unparsable`] when `text` is not TOML at all (a key is never
/// written into a file the daemon could not have loaded), [`EditError::Key`]
/// for an empty or non-table path, [`EditError::Value`] for a value TOML
/// cannot hold.
pub fn edit(text: &str, key: &str, value: &serde_json::Value) -> Result<String, EditError> {
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| EditError::Unparsable(e.message().to_string()))?;
    let path: Vec<&str> = key.split('.').collect();
    if path.iter().any(|part| part.is_empty()) {
        return Err(EditError::Key(key.into()));
    }
    let (&leaf, tables) = path
        .split_last()
        .ok_or_else(|| EditError::Key(key.into()))?;
    // Walk to the leaf's table, creating the missing ones as **implicit**
    // tables: an operator who only has `[api]` and sets `voice.stt.locale`
    // gets `[voice.stt]` and not a stray empty `[voice]` header above it.
    let mut item = doc.as_item_mut();
    for name in tables {
        let table = item
            .as_table_like_mut()
            .ok_or_else(|| EditError::Key(key.into()))?;
        if table.get(name).is_none() {
            let mut fresh = toml_edit::Table::new();
            fresh.set_implicit(true);
            table.insert(name, toml_edit::Item::Table(fresh));
        }
        item = table
            .get_mut(name)
            .expect("just inserted or already present");
    }
    let table = item
        .as_table_like_mut()
        .ok_or_else(|| EditError::Key(key.into()))?;
    match value {
        serde_json::Value::Null => {
            table.remove(leaf);
        }
        value => {
            let next = toml_value(value).ok_or_else(|| EditError::Value {
                key: key.into(),
                why: "written as TOML (no nulls, and no nulls inside a list)".into(),
            })?;
            // Keep the operator's own spacing and trailing comment when the
            // key was already there; a fresh key gets toml_edit's default.
            match table.get_mut(leaf).and_then(toml_edit::Item::as_value_mut) {
                Some(existing) => {
                    let decor = existing.decor().clone();
                    *existing = next;
                    *existing.decor_mut() = decor;
                }
                None => {
                    table.insert(leaf, toml_edit::Item::Value(next));
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// A JSON value as the TOML one it is. `None` for anything TOML has no way
/// to write, which is a null anywhere in the tree.
fn toml_value(value: &serde_json::Value) -> Option<toml_edit::Value> {
    Some(match value {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.into(),
            (None, Some(f)) => f.into(),
            (None, None) => return None,
        },
        serde_json::Value::String(s) => s.as_str().into(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(toml_value)
            .collect::<Option<toml_edit::Array>>()?
            .into(),
        serde_json::Value::Object(fields) => fields
            .iter()
            .map(|(k, v)| toml_value(v).map(|v| (k.as_str(), v)))
            .collect::<Option<toml_edit::InlineTable>>()?
            .into(),
    })
}

/// Write `config.toml` the way `tokens.toml` is written: a temp file in the
/// same directory, `sync_all`, then a rename — so the notify watcher never
/// sees half a file and a crash mid-write cannot leave the daemon without a
/// config. An existing file's mode is kept: this one is the operator's.
///
/// # Errors
/// [`EditError::Write`] for anything the filesystem refused.
pub fn write(path: &Path, text: &str) -> Result<(), EditError> {
    let failed = |source| EditError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp).map_err(failed)?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(failed)?;
    }
    if let Ok(existing) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, existing.permissions());
    }
    std::fs::rename(&tmp, path).map_err(failed)
}

/// The macOS directory name the data, logs and caches had before the project
/// was renamed from OpenMicroDial.
pub const LEGACY_MACOS_DIR: &str = "OpenMicroDial";

/// Where under `~/Library` the data, logs and caches live on macOS.
const MACOS_LIBRARY_DIRS: [&str; 3] = ["Application Support", "Logs", "Caches"];

/// `deckd run` on the platform defaults (no `--data-dir`, no
/// `DECKD_DATA_DIR`): move an install's directories from before the rename
/// into place. Only the daemon's own start calls this; resolving paths
/// (`Paths::detect`, which the API and its tests reach too) never moves
/// anything.
pub fn adopt_legacy_macos_dirs() {
    if !cfg!(target_os = "macos") || std::env::var_os("DECKD_DATA_DIR").is_some() {
        return;
    }
    let Some(base) = directories::BaseDirs::new() else {
        return;
    };
    for library in MACOS_LIBRARY_DIRS.map(|dir| base.home_dir().join("Library").join(dir)) {
        if adopt_legacy_dir(
            &library.join(LEGACY_MACOS_DIR),
            &library.join("OpenWalkieTalkie"),
        ) {
            tracing::info!(
                "moved {} to OpenWalkieTalkie",
                library.join(LEGACY_MACOS_DIR).display()
            );
        }
    }
}

/// Move `legacy` to `dir` when only `legacy` exists: an install from before
/// the rename keeps its config, tokens and state. True when it moved.
pub fn adopt_legacy_dir(legacy: &Path, dir: &Path) -> bool {
    if dir.exists() || !legacy.is_dir() {
        return false;
    }
    std::fs::rename(legacy, dir).is_ok()
}

/// Where everything lives (§9). `DECKD_DATA_DIR` overrides the data dir
/// (spikes and tests must never touch the user's files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub tokens: PathBuf,
    pub port: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    /// `status.json`: the daemon's live state for `deckd status` (§9).
    pub status: PathBuf,
    pub logs: PathBuf,
    pub caches: PathBuf,
    /// `caches/tts`: the local synthesiser's Python environment, its
    /// script and nothing the daemon cannot rebuild (`deckd tts install`).
    pub tts: PathBuf,
    /// `caches/stt`: the local recogniser's model weights, downloaded on
    /// opt-in (`deckd stt install`) and nothing else.
    pub stt: PathBuf,
    /// `caches/tts/kokoro`: the local voice's model packages, downloaded on
    /// opt-in (`deckd tts install`) and nothing else.
    pub kokoro: PathBuf,
}

impl Paths {
    pub fn from_data_dir(data: PathBuf, logs: PathBuf, caches: PathBuf) -> Paths {
        Paths {
            config: data.join("config.toml"),
            state: data.join("state.json"),
            tokens: data.join("tokens.toml"),
            port: data.join("port"),
            socket: data.join("deckd.sock"),
            lock: data.join("deckd.lock"),
            status: data.join("status.json"),
            logs,
            tts: caches.join("tts"),
            stt: caches.join("stt"),
            kokoro: caches.join("tts").join("kokoro"),
            caches,
            data,
        }
    }

    /// The platform defaults, or `DECKD_DATA_DIR` (logs and caches inside it).
    pub fn detect() -> Result<Paths, ConfigError> {
        if let Some(dir) = std::env::var_os("DECKD_DATA_DIR") {
            let data = PathBuf::from(dir);
            return Ok(Paths::from_data_dir(
                data.clone(),
                data.join("logs"),
                data.join("caches"),
            ));
        }
        let base = directories::BaseDirs::new()
            .ok_or_else(|| ConfigError::Parse("no home directory".into()))?;
        let home = base.home_dir();
        if cfg!(target_os = "macos") {
            let [data, logs, caches] = MACOS_LIBRARY_DIRS
                .map(|base| home.join("Library").join(base).join("OpenWalkieTalkie"));
            Ok(Paths::from_data_dir(data, logs, caches))
        } else {
            let runtime = base
                .runtime_dir()
                .map(|d| d.join("openwalkietalkie"))
                .unwrap_or_else(|| base.cache_dir().join("openwalkietalkie"));
            let mut paths = Paths::from_data_dir(
                base.config_dir().join("openwalkietalkie"),
                base.state_dir()
                    .unwrap_or(base.data_dir())
                    .join("openwalkietalkie/logs"),
                base.cache_dir().join("openwalkietalkie"),
            );
            paths.state = base
                .state_dir()
                .unwrap_or(base.data_dir())
                .join("openwalkietalkie/state.json");
            paths.socket = runtime.join("deckd.sock");
            paths.lock = runtime.join("deckd.lock");
            paths.port = runtime.join("port");
            Ok(paths)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watch_handle_carries_the_initial_config_and_the_hot_key_list() {
        let dir = std::env::temp_dir().join(format!("deckd-config-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        let (core, _rx) = CoreHandle::detached();
        let mut initial = Config::default();
        initial.api.port = 0;
        let handle = watch(&paths, initial, core.clone(), core.bus().clone());
        assert!(handle.watching(), "the data dir exists: the watcher is up");
        assert_eq!(handle.current().api.port, 0);
        assert!(handle.tokens().load().tokens.is_empty(), "no tokens.toml");
        let cell = handle.config();
        cell.store(Arc::new(Config::default()));
        assert_eq!(handle.current().api.port, 47201, "one shared cell");
        assert_eq!(handle.core().queued(), 0, "nothing is applied at startup");
        assert_eq!(handle.bus().rev(), 0);
        assert!(HOT_KEYS.contains(&"log.level") && !HOT_KEYS.contains(&"api.port"));
        assert_eq!(WATCH_DEBOUNCE, Duration::from_millis(300));
        drop(handle);
        // A data dir that does not exist is not fatal, only not watched.
        let missing = dir.join("gone");
        let paths = Paths::from_data_dir(missing.clone(), missing.clone(), missing);
        let handle = watch(&paths, Config::default(), core.clone(), core.bus().clone());
        assert!(!handle.watching());
        drop(handle);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hot_keys_cover_their_whole_table_and_nothing_else() {
        assert!(is_hot("log.level"));
        assert!(is_hot("api.allowed_origins"));
        assert!(is_hot("trust.require_trusted_for_allow"));
        assert!(is_hot("trust.gui_requirement"));
        assert!(is_hot("agents.grace_s"));
        assert!(is_hot("agents.claude:cohdl-api.slot"), "pins are hot");
        assert!(is_hot("hooks.claude.hold_s"));
        assert!(is_hot("hooks.claude.settle_s"));
        assert!(
            is_hot("hooks.enabled"),
            "the tray's Pause hooks is `config.set hooks.enabled=false`, and \
             `Adapters::enabled()` already reads the cell per hook request"
        );
        assert!(!is_hot("api.port"), "restart needed");
        assert!(!is_hot("device.pace_ms"));
        assert!(!is_hot("device"));
        assert!(!is_hot("trusted.x"), "a prefix is not a table");
        assert!(!is_hot("hooks.claude.tool_titles"));
    }

    #[test]
    fn the_changed_keys_are_the_dotted_paths_that_differ() {
        let before = Config::default();
        let after = Config::parse(
            r#"
[api]
port = 47202
allowed_origins = ["http://localhost:5173"]

[hooks.claude]
hold_s = 30

[agents]
"claude:x" = { slot = 4 }
"#,
        )
        .unwrap();
        assert_eq!(
            changed_keys(&before, &after),
            vec![
                "agents.claude:x".to_string(),
                "api.allowed_origins".into(),
                "api.port".into(),
                "hooks.claude.hold_s".into(),
            ]
        );
        assert!(changed_keys(&before, &before).is_empty());
        // Exactly one of those needs a restart.
        let cold: Vec<String> = changed_keys(&before, &after)
            .into_iter()
            .filter(|key| !is_hot(key))
            .collect();
        assert_eq!(cold, ["api.port".to_string()]);
    }

    #[test]
    fn an_invalid_file_reports_its_position_without_quoting_the_line() {
        let text = "[api]\nport = \"nope\"\n";
        let (message, position) = Config::parse_at(text).unwrap_err();
        assert!(!message.contains('\n'), "{message:?}");
        let (line, col) = position.expect("the span of the bad value");
        assert_eq!(line, 2, "{message:?}");
        assert!(col >= 8, "{col} in {message:?}");
        // A syntax error has a position too, and a valid file has none.
        assert!(Config::parse_at("[api\nport = 1").is_err());
        assert_eq!(Config::parse_at("").unwrap(), Config::default());
        assert_eq!(line_col("a\nbc", 0), (1, 1));
        assert_eq!(line_col("a\nbc", 2), (2, 1));
        assert_eq!(line_col("a\nbc", 4), (2, 3));
        assert_eq!(line_col("é", 1), (1, 1), "never split a char");
        assert_eq!(line_col("", 99), (1, 1));
    }

    #[test]
    fn the_defaults_are_the_design_table() {
        let c = Config::default();
        assert_eq!(c.api.port, 47201);
        assert_eq!(c.api.uds_default_scope, "observe");
        assert_eq!(c.api.mailbox_ttl_s, 600);
        assert!(c.trust.require_trusted_for_allow);
        assert_eq!(c.privacy.keep_tool_input, "pending");
        assert_eq!(c.privacy.event_ring, 4096);
        assert!(!c.privacy.log_content);
        assert_eq!(c.agents.grace_s, 300);
        assert_eq!(c.hooks.claude.hold_s, 120);
        assert_eq!(c.hooks.gemini.permission, "notify");
        assert!(!c.cards.paging);
        assert_eq!(c.cards.ready_ttl_s, 900);
        assert_eq!(c.voice.mic_warm, "card_pending");
        assert!(c.voice.speak_over_sco);
        assert_eq!(c.voice.slow_sco_ms, 1500);
        assert_eq!(c.voice.stt.locale, "en-US");
        assert_eq!(c.voice.stt.engine, "apple");
        assert_eq!(c.voice.tts.engine, "cloud");
        assert_eq!(c.voice.tts.provider, "openai");
        assert!(!c.voice.tts.has_key(), "no key by default: Apple speaks");
        assert_eq!(c.voice.tts.effective_engine(), "apple");
        assert!(c.voice.tts.model.is_empty(), "the engine's own default");
        assert!(c.voice.tts.voices.is_empty());
        assert_eq!(c.voice.tts.lang, "auto");
        assert_eq!(c.voice.voices.default_pool.len(), 4);
        assert_eq!(c.desktop.inject, vec!["ghostty"]);
        assert_eq!(c.device.pace_ms, 8);
        assert_eq!(c.device.hello_s, 5);
        assert_eq!(c.device.usage_period, "5h / 7d");
        assert_eq!(c.usage.providers, vec!["claude", "codex"]);
        assert_eq!(c.usage.poll_s, 300);
        assert_eq!(c.log.level, "info");
        assert_eq!(Config::parse("").unwrap(), c);
    }

    /// V1 and V2 (HOST-CONTRACT-W4 §2, measured 2026-09-21, n=16): the two
    /// §16 Q2 thresholds the design wrote its defaults against were both
    /// blown, and the shipped defaults were the wrong side of them.
    ///
    /// * SCO bring-up (start IO → **first non-zero** mic sample) is 1031 ms
    ///   median (815–1246). The design's rule was "switch `mic_warm` to
    ///   `card_pending` if > 300 ms", so `card_pending` is the default.
    /// * A2DP resume (close input → nominal back to 44100) is 2307 ms
    ///   median (2281–2447). The rule was "add `speak_over_sco` if > 1.5 s",
    ///   so the key exists and is **on**.
    /// * `slow_sco_ms` is §7.5's "the bring-up was slow" report. The design
    ///   said 800 ms, which is **below the median** — it would fire on
    ///   every single press and say nothing. It is keyed above the measured
    ///   worst case instead.
    ///
    /// This row is the evidence, not a restatement of the struct: each
    /// assertion carries the number that forces it, so a later reader who
    /// "tidies" a default back has to argue with the measurement.
    #[test]
    fn the_sco_defaults_are_the_measured_ones_and_not_the_designs_guesses() {
        // The measurement, as `$S/w4/facts-audio.md` records it: median,
        // min, max over 16 trials, in ms.
        let (bringup_median, bringup_max) = (1031u32, 1246u32);
        let resume_median = 2307u32;
        let c = Config::default();

        // V1's first rule: bring-up > 300 ms → `card_pending`.
        assert!(bringup_median > 300, "the rule's threshold");
        assert_eq!(c.voice.mic_warm, "card_pending");
        // V1's second: A2DP resume > 1.5 s → the key exists and is on.
        assert!(resume_median > 1500, "the rule's threshold");
        assert!(c.voice.speak_over_sco, "the key exists and is on");
        // V2: a threshold under the measured bring-up fires on every press.
        assert!(
            c.voice.slow_sco_ms > bringup_max,
            "slow_sco_ms {} would fire on a bring-up measured at up to {bringup_max} ms",
            c.voice.slow_sco_ms
        );
        assert!(
            800 < bringup_median,
            "the design's 800 ms sat below the median, which is why it moved"
        );

        // …and the file still overrides all three, because the operator's
        // deck is not the one this was measured on.
        let c = Config::parse(
            "[voice]\nmic_warm = \"never\"\nspeak_over_sco = false\nslow_sco_ms = 900\n",
        )
        .unwrap();
        assert_eq!(c.voice.mic_warm, "never");
        assert!(!c.voice.speak_over_sco);
        assert_eq!(c.voice.slow_sco_ms, 900);
    }

    /// `[voice] transport`: the Rev A deck is not a Classic audio device any
    /// more, so the BLE audio service is the default; the CoreAudio path is
    /// one key away; anything else is refused by the whole-file validation
    /// rather than silently picking a branch; and the key needs a restart
    /// even though the rest of `[voice]` is hot.
    #[test]
    fn the_voice_transport_defaults_to_ble_refuses_a_typo_and_needs_a_restart() {
        assert_eq!(Config::default().voice.transport, VoiceTransport::Ble);
        assert_eq!(
            Config::parse("[voice]\nmic_warm = \"never\"\n")
                .unwrap()
                .voice
                .transport,
            VoiceTransport::Ble,
            "a [voice] table without the key is still BLE"
        );
        let c = Config::parse("[voice]\ntransport = \"coreaudio\"\n").unwrap();
        assert_eq!(c.voice.transport, VoiceTransport::Coreaudio);
        assert_eq!(c.voice.transport.as_str(), "coreaudio");
        assert_eq!(
            Config::parse("[voice]\ntransport = \"ble\"\n")
                .unwrap()
                .voice
                .transport
                .as_str(),
            "ble"
        );

        let (message, position) = Config::parse_at("[voice]\ntransport = \"bluetooth\"\n")
            .expect_err("a transport nobody implements");
        assert!(
            message.contains("ble") && message.contains("coreaudio"),
            "the error names what is allowed: {message}"
        );
        assert_eq!(position.map(|(line, _)| line), Some(2));
        assert!(Config::parse("[voice]\ntransport = \"BLE\"\n").is_err());

        // `config.get` publishes it the way the file spells it.
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["voice"]["transport"], "coreaudio");

        // Every other `[voice]` key stays hot; this one is read once.
        assert!(!is_hot("voice.transport"), "read once, by audio::spawn");
        assert!(is_hot("voice.mic_warm") && is_hot("voice.stt.locale"));
        let cold: Vec<String> = changed_keys(&Config::default(), &c)
            .into_iter()
            .filter(|key| !is_hot(key))
            .collect();
        assert_eq!(cold, ["voice.transport".to_string()]);
    }

    #[test]
    fn the_design_example_parses_and_unknown_keys_are_ignored() {
        let text = r#"
[api]
port = 47202
allowed_origins = ["http://localhost:5173"]
future_key = 1

[trust]
require_trusted_for_allow = false

[agents]
"claude:cohdl-api" = { slot = 1, voice = "com.apple.voice.enhanced.en-US.Samantha" }
"/Users/tony/projects/openmicro" = { slot = 3, name = "openmicro" }
grace_s = 60

[hooks.claude]
hold_s = 30

[device]
pace_ms = 4

[macropad."Build"]
keys = [ { l = "build", b = "left", action = { shell = "cargo build" } },
         { l = "deploy", action = { say = "deploying" } } ]

[unknown_section]
x = 1
"#;
        let c = Config::parse(text).unwrap();
        assert_eq!(c.api.port, 47202);
        assert_eq!(c.api.allowed_origins, vec!["http://localhost:5173"]);
        assert!(!c.trust.require_trusted_for_allow);
        assert_eq!(c.agents.grace_s, 60);
        assert_eq!(c.agents.pins.len(), 2);
        assert_eq!(c.agents.pins["claude:cohdl-api"].slot, Some(1));
        assert_eq!(
            c.agents.pins["/Users/tony/projects/openmicro"]
                .name
                .as_deref(),
            Some("openmicro")
        );
        assert_eq!(c.hooks.claude.hold_s, 30);
        assert_eq!(c.hooks.codex.hold_s, 120);
        assert_eq!(c.device.pace_ms, 4);
        assert_eq!(
            c.device.pace(),
            std::time::Duration::from_millis(8),
            "floor"
        );
        assert_eq!(c.macropad["Build"].keys.len(), 2);
        assert_eq!(c.macropad["Build"].keys[0].b.as_deref(), Some("left"));
        assert!(c.macropad["Build"].keys[1].b.is_none());
        let pins = c.pins();
        assert_eq!(pins["claude:cohdl-api"].slot, Some(deck_model::Slot(1)));
    }

    #[test]
    fn invalid_toml_is_an_error_and_a_missing_file_is_the_default() {
        assert!(matches!(
            Config::parse("[api\nport = 1"),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            Config::parse("[api]\nport = \"x\""),
            Err(ConfigError::Parse(_))
        ));
        let dir = std::env::temp_dir().join(format!("deckd-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            Config::load(&dir.join("absent.toml")).unwrap(),
            Config::default()
        );
        std::fs::write(dir.join("c.toml"), "[log]\nlevel = \"debug\"").unwrap();
        assert_eq!(
            Config::load(&dir.join("c.toml")).unwrap().log.level,
            "debug"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What `config.set` does to the file, and what it leaves alone.
    ///
    /// The operator maintains `config.toml` by hand — §9 is written as a
    /// commented example — so the one thing a GUI toggle may not do is hand
    /// it back a serde round trip: every documented default written out as
    /// an explicit key, in serde's order, with every comment gone.
    #[test]
    fn one_key_is_edited_in_place_and_the_rest_of_the_file_comes_back_unchanged() {
        let before = "\
# the deck's own port; 47201 is the documented default
[api]
port   = 47201   # changed by hand once
allowed_origins = []

# hooks off while I debug
[hooks]
enabled = false
";
        let after = edit(before, "api.port", &serde_json::json!(47_299)).unwrap();
        assert_eq!(
            after,
            before.replace("port   = 47201 ", "port   = 47299 "),
            "the value moved and nothing else did — comment, spacing and order included"
        );
        assert_eq!(Config::parse(&after).unwrap().api.port, 47_299);

        // A key the file does not have yet is appended to its table; a
        // table it does not have yet is created **implicit**, so the file
        // gains `[voice.stt]` and no stray empty `[voice]` above it.
        let after = edit(before, "voice.stt.locale", &serde_json::json!("en-GB")).unwrap();
        assert!(after.starts_with(before), "{after}");
        assert!(after.contains("[voice.stt]"), "{after}");
        assert!(!after.contains("\n[voice]\n"), "no empty parent: {after}");
        assert_eq!(Config::parse(&after).unwrap().voice.stt.locale, "en-GB");
        // …and into an empty file, which is what "no config.toml yet" is.
        let fresh = edit("", "hooks.claude.hold_s", &serde_json::json!(30)).unwrap();
        assert_eq!(Config::parse(&fresh).unwrap().hooks.claude.hold_s, 30);

        // `null` removes the key, which is how a control with a "Default"
        // position says "unset it": there is no TOML null to write.
        let set = edit("", "voice.device_uid", &serde_json::json!("C2-2D")).unwrap();
        assert_eq!(
            Config::parse(&set).unwrap().voice.device_uid.as_deref(),
            Some("C2-2D")
        );
        let unset = edit(&set, "voice.device_uid", &serde_json::Value::Null).unwrap();
        assert_eq!(Config::parse(&unset).unwrap().voice.device_uid, None);

        // Lists and the values TOML cannot hold.
        let origins = edit(
            before,
            "api.allowed_origins",
            &serde_json::json!(["http://localhost:5173"]),
        )
        .unwrap();
        assert_eq!(
            Config::parse(&origins).unwrap().api.allowed_origins,
            ["http://localhost:5173"]
        );
        assert!(matches!(
            edit(before, "api.allowed_origins", &serde_json::json!([null])),
            Err(EditError::Value { .. })
        ));
        assert!(matches!(
            edit(before, "api..port", &serde_json::json!(1)),
            Err(EditError::Key(_))
        ));
        // A leaf is not a table: `api.port.x` has nowhere to go.
        assert!(matches!(
            edit(before, "api.port.x", &serde_json::json!(1)),
            Err(EditError::Key(_))
        ));
        // And a file the daemon could not have loaded is never written
        // into: the operator's broken edit stays theirs to fix.
        assert!(matches!(
            edit("[api\nport = 1", "api.port", &serde_json::json!(1)),
            Err(EditError::Unparsable(_))
        ));
    }

    /// The write is the `tokens.toml` one: a temp file in the same
    /// directory, then a rename — so the notify watcher of §3.7 never reads
    /// half a config and decides the last good one has to stay.
    #[test]
    fn config_toml_is_written_atomically_and_keeps_the_mode_it_had() {
        let dir = std::env::temp_dir().join(format!("deckd-config-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/config.toml");
        write(&path, "[api]\nport = 1\n").expect("the directory is created with it");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[api]\nport = 1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            write(&path, "[api]\nport = 2\n").unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "the operator's own mode survives the rename");
        }
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .all(|e| e.file_name() == "config.toml"),
            "no temp file is left behind"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paths_follow_the_data_dir() {
        let p = Paths::from_data_dir(
            PathBuf::from("/d"),
            PathBuf::from("/l"),
            PathBuf::from("/c"),
        );
        assert_eq!(p.config, PathBuf::from("/d/config.toml"));
        assert_eq!(p.state, PathBuf::from("/d/state.json"));
        assert_eq!(p.socket, PathBuf::from("/d/deckd.sock"));
        assert_eq!(p.lock, PathBuf::from("/d/deckd.lock"));
        assert_eq!(p.status, PathBuf::from("/d/status.json"));
        assert_eq!(p.logs, PathBuf::from("/l"));
    }

    /// A pre-rename install's directory becomes the new one, once: an
    /// existing new directory is never overwritten.
    #[test]
    fn the_directory_under_the_old_project_name_is_adopted_once() {
        let dir = std::env::temp_dir().join(format!("deckd-config-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join(LEGACY_MACOS_DIR);
        let new = dir.join("OpenWalkieTalkie");
        assert!(!adopt_legacy_dir(&legacy, &new), "nothing to adopt");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("tokens.toml"), "kept").unwrap();
        assert!(adopt_legacy_dir(&legacy, &new));
        assert_eq!(
            std::fs::read_to_string(new.join("tokens.toml")).unwrap(),
            "kept"
        );
        assert!(!legacy.exists());
        std::fs::create_dir_all(&legacy).unwrap();
        assert!(!adopt_legacy_dir(&legacy, &new), "the new directory wins");
        assert!(legacy.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
