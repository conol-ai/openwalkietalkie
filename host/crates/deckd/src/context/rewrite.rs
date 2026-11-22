//! One dictation, rewritten for where it lands — or left exactly as it was
//! heard. Runs on a thread of its own ([`Worker`]); the core hands it the
//! final transcript and takes back the text to type.
//!
//! **Failure is invisible.** Every way this can go wrong — no focus, no
//! key, DNS, TLS, a 401, a 500, a timeout, a body that is not a chat
//! completion, an answer that fails [`prompt::sanity`] — ends in the raw
//! transcript, with at most one log line that carries a word for what
//! happened, lengths, timings and salted fingerprints, never the words and
//! never a key.
//!
//! **The budget.** The scene call and the edit call share one total budget
//! (`[context] budget_ms`, 3 s by default): the scene call gets at most
//! [`SCENE_MAX`] of it, the edit call what is left. The core holds the
//! final for the budget plus [`GRACE`] at most, then lets the raw
//! transcript go whatever the worker is doing.
use super::focus::{self, Focus};
use super::keys::Role;
use super::prompt::{self, Scene};
use crate::config::{Config, Paths};
use crate::providers::{wire, Target};
use crossbeam_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The scene call's share of the budget, at most.
pub const SCENE_MAX: Duration = Duration::from_millis(1000);
/// How long past its budget the core waits for the worker's answer.
pub const GRACE: Duration = Duration::from_millis(300);
/// Below this much budget left, the edit call is not started.
const EDIT_MIN: Duration = Duration::from_millis(150);
/// Press-time windows kept waiting for their finals.
const CAPTURES_KEPT: usize = 8;
/// How long a rewrite waits for its press-time window still being read.
const CAPTURE_WAIT: Duration = Duration::from_millis(500);
/// Longer dictations are typed as recognised.
const INPUT_MAX: usize = 2000;

/// A lookup by name the pipeline is handed, shareable across threads.
pub type Lookup<T> = Box<dyn Fn(&str) -> Option<T> + Send + Sync>;

/// What the pipeline needs from the world, so that a test can swap it.
pub struct Deps {
    pub curl: PathBuf,
    pub focus: Box<dyn Fn() -> Option<Focus> + Send + Sync>,
    /// The key a [`Target`]'s `key_of` names (a provider id, or
    /// `legacy:<role>`), `None` when there is none.
    pub keys: Lookup<String>,
    /// An Omarchy local model's own endpoint, by its id, from the last
    /// discovery (`providers::omarchy`); `None` keeps the kind's default.
    pub local_endpoint: Lookup<String>,
    /// Whether an Omarchy local model is running now, by its id, from the
    /// last discovery: `Some(false)` is a model to skip rather than wait
    /// for (a cold start takes minutes); `None` is not known.
    pub local_running: Lookup<bool>,
}

impl Deps {
    /// The real world, for a service on `paths`.
    #[must_use]
    pub fn real(paths: &Paths) -> Deps {
        let cache = Arc::new(crate::providers::keys::KeyCache::new(paths.clone()));
        Deps {
            curl: crate::grokbot::webhook::curl(),
            focus: Box::new(focus::read),
            keys: Box::new(move |key_of| cache.key(key_of)),
            local_endpoint: {
                let paths = paths.clone();
                Box::new(move |model| {
                    crate::providers::omarchy::cached(&paths)
                        .and_then(|found| found.endpoint(model).map(String::from))
                })
            },
            local_running: {
                let paths = paths.clone();
                Box::new(move |model| {
                    crate::providers::omarchy::cached(&paths).and_then(|found| {
                        let model = found.models.iter().find(|m| m.id == model)?;
                        model.unit.as_ref().map(|_| model.running())
                    })
                })
            },
        }
    }
}

/// What the two roles resolve to ([`Config::context_target`]): `None` for a
/// role that is not set up (or whose provider is missing or broken, which
/// is logged once, at the dictation, as `no_edit_model`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub scene: Option<Target>,
    pub edit: Option<Target>,
}

impl Settings {
    #[must_use]
    pub fn of(config: &Config) -> Settings {
        Settings {
            scene: config.context_target(Role::Scene).ok(),
            edit: config.context_target(Role::Edit).ok(),
        }
    }
}

/// The dictation budget for `settings`: `base` (`[context] budget_ms`),
/// raised only by a **local** target to its provider's own timeout, and
/// never past [`crate::providers::TIMEOUT_MAX`]. A cloud target never
/// changes it, so cloud dictation is never slower for a local provider
/// existing.
#[must_use]
pub fn budget_for(settings: &Settings, base: Duration) -> Duration {
    [settings.scene.as_ref(), settings.edit.as_ref()]
        .into_iter()
        .flatten()
        .filter(|target| target.local)
        .filter_map(|target| target.timeout)
        .fold(base, Duration::max)
        .min(crate::providers::TIMEOUT_MAX.max(base))
}

/// `target` with an Omarchy local model's own endpoint, when discovery
/// knows it.
fn located(target: &Target, deps: &Deps) -> Target {
    let mut target = target.clone();
    if target.kind == crate::providers::Kind::Omarchy {
        if let Some(endpoint) = (deps.local_endpoint)(&target.model) {
            target.base_url = endpoint;
        }
    }
    target
}

/// A prompt's messages as the adapters take them: the system prompt, and
/// the turns after it.
fn split(messages: &[(&'static str, String)]) -> (String, Vec<(&'static str, String)>) {
    match messages.split_first() {
        Some(((role, system), turns)) if *role == "system" => (system.clone(), turns.to_vec()),
        _ => (String::new(), messages.to_vec()),
    }
}

/// What became of one dictation.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// What to type: the rewrite, or the transcript as it came.
    pub text: String,
    pub rewritten: bool,
    /// Why the transcript was kept (`timeout`, `http_401`, `reply`…), or
    /// `rewritten`.
    pub why: String,
    pub scene: Scene,
    /// `model` or `rules`.
    pub scene_by: &'static str,
    /// The rewriting model is on this computer or a private network.
    pub local: bool,
    /// The transcript's own words moved the scene to `shell`
    /// ([`prompt::with_words`]).
    pub words: bool,
    /// When the window was read: `press` (the PTT session's start, carried
    /// here by the core) or `final` (read now: no capture was made).
    pub focus_at: &'static str,
    /// The focused window's application class (`foot`, `Alacritty`), one
    /// short token ([`class_token`]); `""` when no window was read. Never
    /// the title. Logged only at debug level for `deckd::context`.
    pub app: String,
    pub focus_ms: u64,
    pub scene_ms: u64,
    pub edit_ms: u64,
    pub total_ms: u64,
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The focused window as the core captured it when the PTT session
/// started, or that it made no capture.
#[derive(Clone, Debug)]
pub enum Captured {
    /// Read at the press (`None`: nothing could be read then).
    Press(Option<Focus>),
    /// No capture: read when the rewrite starts.
    NotCaptured,
}

/// [`run_with`], reading the window when it starts.
#[must_use]
pub fn run(transcript: &str, settings: &Settings, budget: Duration, deps: &Deps) -> Outcome {
    run_with(transcript, settings, budget, deps, Captured::NotCaptured)
}

/// The whole pipeline for one transcript, within `budget`, with the window
/// the core captured at the press.
#[must_use]
pub fn run_with(
    transcript: &str,
    settings: &Settings,
    budget: Duration,
    deps: &Deps,
    captured: Captured,
) -> Outcome {
    let start = Instant::now();
    let mut out = Outcome {
        text: transcript.to_string(),
        rewritten: false,
        why: String::new(),
        scene: Scene::Editor,
        scene_by: "rules",
        local: false,
        words: false,
        focus_at: "press",
        app: String::new(),
        focus_ms: 0,
        scene_ms: 0,
        edit_ms: 0,
        total_ms: 0,
    };
    let finish = |mut out: Outcome, why: &str| {
        out.why = why.to_string();
        out.total_ms = ms(start.elapsed());
        out
    };
    let Some(edit) = settings.edit.as_ref().map(|t| located(t, deps)) else {
        return finish(out, "no_edit_model");
    };
    let edit = &edit;
    // A local model known not to be running is skipped, not waited for: a
    // cold start takes minutes.
    let not_running = |target: &Target| {
        target.kind == crate::providers::Kind::Omarchy && (deps.local_running)(&target.model) == Some(false)
    };
    out.local = edit.local;
    if not_running(edit) {
        return finish(out, "local_not_running");
    }
    if transcript.trim().is_empty() || transcript.chars().count() > INPUT_MAX {
        return finish(out, "input");
    }
    // The scene: two strings from the focused window, then rules, then the
    // scene model when one is configured.
    // The window as it was when the press started; read now only when the
    // core made no capture.
    let focus = match captured {
        Captured::Press(focus) => focus,
        Captured::NotCaptured => {
            out.focus_at = "final";
            (deps.focus)()
        }
    };
    out.focus_ms = ms(start.elapsed());
    out.app = focus
        .as_ref()
        .map(|f| class_token(&f.app))
        .unwrap_or_default();
    out.scene = prompt::rules(focus.as_ref());
    let scene_target = settings.scene.as_ref().map(|t| located(t, deps));
    let scene_target = scene_target.filter(|scene| !not_running(scene));
    if let (Some(focus), Some(scene)) = (focus.as_ref(), scene_target.as_ref()) {
        let t = Instant::now();
        let left = budget.saturating_sub(start.elapsed());
        // A cloud scene model gets a second at most; a local one its own
        // timeout, within what the budget leaves for the edit.
        let share = if scene.local { scene.timeout.unwrap_or(crate::providers::LOCAL_TIMEOUT) / 2 } else { SCENE_MAX };
        let timeout = share.min(left.saturating_sub(EDIT_MIN * 2));
        if !timeout.is_zero() {
            let key = (deps.keys)(&scene.key_of);
            let (system, turns) = split(&prompt::scene_messages(focus));
            let ask = wire::Ask {
                target: scene,
                key: key.as_deref(),
                system: &system,
                turns: &turns,
                max_tokens: 8,
            };
            match wire::complete(&deps.curl, &ask, timeout)
                .ok()
                .and_then(|a| Scene::parse_label(&a))
            {
                Some(label) => {
                    out.scene = label;
                    out.scene_by = "model";
                }
                None => out.scene_by = "rules(model_failed)",
            }
        }
        out.scene_ms = ms(t.elapsed());
    }
    // The words: a transcript that is plainly a command gets the shell's
    // rules in a window that does not say otherwise.
    let recognised = prompt::recognise(focus.as_ref()).is_some();
    (out.scene, out.words) = prompt::with_words(out.scene, recognised, transcript);
    // The edit.
    let left = budget.saturating_sub(start.elapsed());
    if left < EDIT_MIN {
        return finish(out, "late");
    }
    let t = Instant::now();
    let key = (deps.keys)(&edit.key_of);
    let (system, turns) = split(&prompt::edit_messages(out.scene, transcript));
    let ask = wire::Ask {
        target: edit,
        key: key.as_deref(),
        system: &system,
        turns: &turns,
        // About as long as the transcript; room for a model that thinks.
        max_tokens: u32::try_from((3 * transcript.len() + 64).clamp(128, 1024)).unwrap_or(1024),
    };
    let answer = wire::complete(&deps.curl, &ask, left.saturating_sub(Duration::from_millis(20)));
    out.edit_ms = ms(t.elapsed());
    match answer {
        // A local model that ran out of time says so: it is slow, not broken.
        Err(crate::context::http::Failure::Timeout) if edit.local => finish(out, "slow_local"),
        Err(failure) => finish(out, &failure.word()),
        Ok(answer) => match prompt::sanity(transcript, &answer, out.scene) {
            Err(why) => finish(out, why),
            Ok(text) => {
                out.rewritten = text != transcript;
                let why = if out.rewritten {
                    "rewritten"
                } else {
                    "unchanged"
                };
                out.text = text;
                finish(out, why)
            }
        },
    }
}

/// A salted fingerprint of `text`: enough to tell two log lines apart, never
/// enough to read them. The salt is new every run.
#[must_use]
pub fn fingerprint(text: &str) -> String {
    static SALT: OnceLock<[u8; 16]> = OnceLock::new();
    let salt = SALT.get_or_init(|| {
        let mut salt = [0u8; 16];
        let _ = getrandom::fill(&mut salt);
        salt
    });
    let mut hasher = blake3::Hasher::new_keyed(&{
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(salt);
        key
    });
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex()[..8].to_string()
}

/// The application class as the log may carry it: one token of ASCII
/// letters, digits, `.`, `_` and `-`, at most 48 of them (anything else is
/// dropped), `-` when nothing is left. A class names an app, never what is
/// in it; the window title is never logged.
#[must_use]
pub fn class_token(class: &str) -> String {
    let token: String = class
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(48)
        .collect();
    if token.is_empty() {
        "-".into()
    } else {
        token
    }
}

/// The one log line a dictation gets. With debug on for `deckd::context`
/// (`[log] level = "info,deckd::context=debug"`), it also names the focused
/// window's class (`app=foot`), so a scene the rules got wrong can be told
/// from the log; the title never.
pub fn log(transcript: &str, out: &Outcome) {
    let app = if tracing::enabled!(tracing::Level::DEBUG) {
        format!(" app={}", if out.app.is_empty() { "-" } else { &out.app })
    } else {
        String::new()
    };
    tracing::info!(
        "context: {} scene={}({}{}) focus_at={}{app} where={} in={} out={} text={}→{} focus_ms={} scene_ms={} edit_ms={} total_ms={}",
        if out.rewritten { "rewritten".to_string() } else { format!("kept:{}", out.why) },
        out.scene.name(),
        out.scene_by,
        if out.words { "+words" } else { "" },
        out.focus_at,
        if out.local { "local" } else { "cloud" },
        transcript.chars().count(),
        out.text.chars().count(),
        fingerprint(transcript),
        fingerprint(&out.text),
        out.focus_ms,
        out.scene_ms,
        out.edit_ms,
        out.total_ms,
    );
}

/// A rewrite that finished: which, and its outcome.
pub struct Done {
    pub id: u64,
    pub outcome: Outcome,
}

/// The core's side: start a rewrite on a thread of its own, and take the
/// answers off [`Worker::events`] beside the core's other workers.
pub struct Worker {
    deps: Option<Arc<Deps>>,
    paths: Paths,
    next: u64,
    /// Windows read at a press, by the PTT session's utterance id, waiting
    /// for that session's final. The oldest go past [`CAPTURES_KEPT`].
    captures: std::collections::VecDeque<(String, Receiver<Option<Focus>>)>,
    done: Sender<Done>,
    pub events: Receiver<Done>,
}

impl Worker {
    /// The real world, built on the first dictation that needs it.
    #[must_use]
    pub fn new(paths: Paths) -> Worker {
        let (done, events) = crossbeam_channel::unbounded();
        Worker {
            deps: None,
            paths,
            next: 0,
            captures: std::collections::VecDeque::new(),
            done,
            events,
        }
    }

    /// With `deps` in place of the real world (tests).
    #[must_use]
    pub fn with(paths: Paths, deps: Deps) -> Worker {
        let mut worker = Worker::new(paths);
        worker.deps = Some(Arc::new(deps));
        worker
    }

    fn deps(&mut self) -> Arc<Deps> {
        let paths = self.paths.clone();
        self.deps
            .get_or_insert_with(|| Arc::new(Deps::real(&paths)))
            .clone()
    }

    /// Read the focused window now, for the PTT session `utterance` that has
    /// just started, on a thread of its own: [`Worker::start`] for that
    /// session's final uses it, so the scene is the window the owner was in
    /// when they pressed, whatever has the focus once the words are back.
    pub fn capture(&mut self, utterance: &str) {
        let deps = self.deps();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("deckd-context-focus".into())
            .spawn(move || {
                let _ = tx.send((deps.focus)());
            });
        if spawned.is_ok() {
            self.captures.retain(|(u, _)| u != utterance);
            self.captures.push_back((utterance.to_string(), rx));
            while self.captures.len() > CAPTURES_KEPT {
                self.captures.pop_front();
            }
        }
    }

    /// Rewrite `transcript` on a new thread; its [`Done`] comes on
    /// `events` under the id returned. `utterance` is the PTT session it
    /// came from: its press-time window ([`Worker::capture`]) is used when
    /// there is one. `None` when no thread could start (the transcript then
    /// goes as it is).
    pub fn start(
        &mut self,
        transcript: &str,
        settings: Settings,
        budget: Duration,
        utterance: Option<&str>,
    ) -> Option<u64> {
        let deps = self.deps();
        let capture = utterance.and_then(|u| {
            let at = self.captures.iter().position(|(seen, _)| seen == u)?;
            self.captures.remove(at).map(|(_, rx)| rx)
        });
        self.next += 1;
        let id = self.next;
        let done = self.done.clone();
        let transcript = transcript.to_string();
        std::thread::Builder::new()
            .name("deckd-context".into())
            .spawn(move || {
                let captured = match capture {
                    // Read ~10 ms after the press; the final is a second
                    // or more later, so this is all but always there.
                    Some(rx) => Captured::Press(rx.recv_timeout(CAPTURE_WAIT).ok().flatten()),
                    None => Captured::NotCaptured,
                };
                let outcome = run_with(&transcript, &settings, budget, &deps, captured);
                log(&transcript, &outcome);
                let _ = done.send(Done { id, outcome });
            })
            .ok()
            .map(|_| id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;

    /// How the fake server answers one request.
    #[derive(Clone)]
    pub enum Reply {
        /// 200 with a chat completion whose content is this.
        Says(&'static str),
        /// This status, with a small JSON error body.
        Status(u16),
        /// 200 with this raw body.
        Raw(&'static str),
        /// Wait this long first, then answer as the inner reply.
        After(Duration, Box<Reply>),
        /// The reply of the first entry whose needle the request's body
        /// holds, whatever order requests arrive in.
        ByBody(Vec<(&'static str, Reply)>),
    }

    /// What the fake server saw of one request.
    #[derive(Clone, Debug, Default)]
    pub struct Seen {
        pub path: String,
        pub authorization: Option<String>,
        pub body: String,
    }

    /// A loopback HTTP server that answers each request with the next of
    /// `replies` (the last one again when they run out), and records what
    /// it was sent.
    pub fn server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut n = 0;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let reply = replies[n.min(replies.len() - 1)].clone();
                n += 1;
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (mut length, mut authorization) = (0usize, None);
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                        let lower = header.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        if lower.starts_with("authorization:") {
                            authorization = Some(header[14..].trim().to_string());
                        }
                    }
                    let mut body = vec![0u8; length];
                    let _ = reader.read_exact(&mut body);
                    log.lock().unwrap().push(Seen {
                        path,
                        authorization,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    });
                    let body_text = String::from_utf8_lossy(&body).into_owned();
                    let mut next = reply;
                    let reply = loop {
                        next = match next {
                            Reply::After(wait, inner) => {
                                std::thread::sleep(wait);
                                *inner
                            }
                            Reply::ByBody(table) => table
                                .into_iter()
                                .find(|(needle, _)| body_text.contains(needle))
                                .map_or(Reply::Status(404), |(_, r)| r),
                            done => break done,
                        };
                    };
                    let (status, body) = match reply {
                        Reply::Says(text) => (
                            200,
                            serde_json::json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string(),
                        ),
                        Reply::Status(code) => (code, "{\"error\":{\"message\":\"nope\"}}".to_string()),
                        Reply::Raw(raw) => (200, raw.to_string()),
                        Reply::After(..) | Reply::ByBody(_) => unreachable!(),
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        (url, seen)
    }

    /// The two roles on stub servers: a custom provider each (a local
    /// server, so no key is needed).
    pub fn settings(scene: Option<&str>, edit: &str) -> Settings {
        let target = |base: &str, model: &str| Target {
            key_of: "stub".into(),
            kind: crate::providers::Kind::Custom,
            base_url: base.trim_end_matches('/').to_string(),
            model: model.into(),
            // The stubs are on loopback, but the pipeline's tests are about
            // the cloud rule; the local rule has tests of its own.
            local: false,
            timeout: None,
        };
        Settings {
            scene: scene.map(|base| target(base, "scene-model")),
            edit: Some(target(edit, "edit-model")),
        }
    }

    pub fn deps(focus: Option<Focus>, key: Option<&'static str>) -> Deps {
        Deps {
            curl: PathBuf::from("curl"),
            focus: Box::new(move || focus.clone()),
            keys: Box::new(move |_| key.map(String::from)),
            local_endpoint: Box::new(|_| None),
            local_running: Box::new(|_| None),
        }
    }

    fn terminal() -> Option<Focus> {
        Some(Focus {
            app: "foot".into(),
            title: "~/src".into(),
        })
    }

    const BUDGET: Duration = Duration::from_secs(3);

    #[test]
    fn a_rewrite_is_typed_and_the_key_goes_as_a_bearer_header() {
        let (url, seen) = server(vec![Reply::Says("git status")]);
        let out = run(
            "get status",
            &settings(None, &url),
            BUDGET,
            &deps(terminal(), Some("sk-edit")),
        );
        assert_eq!(
            (out.text.as_str(), out.rewritten, out.why.as_str()),
            ("git status", true, "rewritten")
        );
        assert_eq!((out.scene, out.scene_by), (Scene::Shell, "rules"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "no scene model: one call");
        assert_eq!(seen[0].path, "/v1/chat/completions");
        assert_eq!(seen[0].authorization.as_deref(), Some("Bearer sk-edit"));
        let body: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["model"], "edit-model");
        assert_eq!(
            body["messages"].as_array().unwrap().last().unwrap()["content"],
            "Transcript:\nget status"
        );
        // The edit model never sees the window: neither string is sent.
        assert!(!seen[0].body.contains("~/src") && !seen[0].body.contains("foot"));
    }

    #[test]
    fn a_local_endpoint_needs_no_key() {
        let (url, seen) = server(vec![Reply::Says("ls -la")]);
        let out = run(
            "L. S. dash a l",
            &settings(None, &url),
            BUDGET,
            &deps(terminal(), None),
        );
        assert_eq!(out.text, "ls -la");
        assert_eq!(seen.lock().unwrap()[0].authorization, None);
    }

    #[test]
    fn a_slow_model_hits_the_budget_and_the_transcript_goes_as_it_was() {
        let (url, _) = server(vec![Reply::After(
            Duration::from_secs(10),
            Box::new(Reply::Says("git status")),
        )]);
        let start = Instant::now();
        let out = run(
            "get status",
            &settings(None, &url),
            Duration::from_millis(800),
            &deps(terminal(), None),
        );
        let took = start.elapsed();
        assert_eq!(
            (out.text.as_str(), out.rewritten, out.why.as_str()),
            ("get status", false, "timeout")
        );
        assert!(
            took < Duration::from_millis(800) + Duration::from_millis(400),
            "{took:?}"
        );
    }

    #[test]
    fn a_server_error_a_bad_body_and_prose_all_type_the_transcript() {
        for (reply, why) in [
            (Reply::Status(500), "http_500"),
            (Reply::Status(401), "http_401"),
            (Reply::Raw("this is not json"), "malformed"),
            (Reply::Raw("{\"choices\":[]}"), "malformed"),
            (Reply::Says("Sure: git status"), "reply"),
            (Reply::Says("I would recommend checking the documentation for your version control software first."), "length"),
        ] {
            let (url, _) = server(vec![reply]);
            let out = run("get status", &settings(None, &url), BUDGET, &deps(terminal(), None));
            assert_eq!((out.text.as_str(), out.rewritten, out.why.as_str()), ("get status", false, why));
        }
    }

    #[test]
    fn a_dead_endpoint_types_the_transcript_at_once() {
        // A port nothing listens on.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/v1");
        let start = Instant::now();
        let out = run(
            "get status",
            &settings(Some(&url), &url),
            BUDGET,
            &deps(terminal(), None),
        );
        assert_eq!((out.text.as_str(), out.rewritten), ("get status", false));
        assert!(out.why.starts_with("network_"), "{}", out.why);
        assert_eq!(out.scene_by, "rules(model_failed)");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn the_scene_model_gets_the_two_strings_and_a_label_outside_the_set_falls_back_to_rules() {
        let (scene_url, scene_seen) = server(vec![Reply::Says("agent")]);
        let (edit_url, edit_seen) = server(vec![Reply::Says("Can you run the tests?")]);
        let out = run(
            "um can you run the tests",
            &settings(Some(&scene_url), &edit_url),
            BUDGET,
            &deps(terminal(), Some("k")),
        );
        assert_eq!((out.scene, out.scene_by), (Scene::Agent, "model"));
        assert_eq!(out.text, "Can you run the tests?");
        let scene_body = scene_seen.lock().unwrap()[0].body.clone();
        assert!(
            scene_body.contains("App: foot\\nWindow title: ~/src"),
            "{scene_body}"
        );
        assert!(
            !scene_body.contains("run the tests"),
            "the scene model never sees the transcript"
        );
        assert!(edit_seen.lock().unwrap()[0].body.contains("coding agent"));

        let (scene_url, _) = server(vec![Reply::Says("a terminal window")]);
        let (edit_url, _) = server(vec![Reply::Says("git status")]);
        let out = run(
            "get status",
            &settings(Some(&scene_url), &edit_url),
            BUDGET,
            &deps(terminal(), None),
        );
        assert_eq!(
            (out.scene, out.scene_by),
            (Scene::Shell, "rules(model_failed)")
        );
        assert_eq!(out.text, "git status");
    }

    #[test]
    fn no_edit_model_makes_no_call_and_no_focus_is_the_least_change_scene() {
        let (url, seen) = server(vec![Reply::Says("x")]);
        let mut s = settings(Some(&url), &url);
        s.edit = None;
        let out = run("get status", &s, BUDGET, &deps(terminal(), None));
        assert_eq!(
            (out.text.as_str(), out.why.as_str()),
            ("get status", "no_edit_model")
        );
        assert!(seen.lock().unwrap().is_empty());
        let (url, _) = server(vec![Reply::Says("return total plus one")]);
        let out = run(
            "um return total plus one",
            &settings(None, &url),
            BUDGET,
            &deps(None, None),
        );
        assert_eq!(out.scene, Scene::Editor);
        assert_eq!(out.text, "return total plus one");
    }

    #[test]
    fn the_class_is_one_short_token_and_the_title_is_never_kept() {
        assert_eq!(class_token("foot"), "foot");
        assert_eq!(
            class_token("com.mitchellh.ghostty"),
            "com.mitchellh.ghostty"
        );
        assert_eq!(
            class_token("chrome-web.whatsapp.com__-Default"),
            "chrome-web.whatsapp.com__-Default"
        );
        assert_eq!(class_token("my app; rm -rf /"), "myapprm-rf");
        assert_eq!(class_token("终端"), "-");
        assert_eq!(class_token(&"x".repeat(100)).len(), 48);
        let (url, _) = server(vec![Reply::Says("git status")]);
        let focus = Some(Focus {
            app: "foot".into(),
            title: "secret project ~/clients/acme".into(),
        });
        let out = run(
            "get status",
            &settings(None, &url),
            BUDGET,
            &deps(focus, None),
        );
        assert_eq!(out.app, "foot");
        assert!(
            !format!("{out:?}").contains("acme"),
            "the title is nowhere in the outcome"
        );
    }

    #[test]
    fn a_fingerprint_is_short_salted_and_stable_within_a_run() {
        assert_eq!(fingerprint("git status"), fingerprint("git status"));
        assert_ne!(fingerprint("git status"), fingerprint("get status"));
        assert_eq!(fingerprint("x").len(), 8);
    }

    /// A Chinese dictation end to end through the fake server: rewritten,
    /// and too long to send (cut by characters, never bytes) left as it was.
    #[test]
    fn a_chinese_dictation_is_rewritten_and_an_overlong_one_left_alone() {
        let (url, seen) = server(vec![Reply::Says("明天的会改到下午三点可以吗？")]);
        let chat = Some(Focus {
            app: "Telegram".into(),
            title: "群聊".into(),
        });
        let out = run(
            "嗯 那个 明天的会 改到 下午三点 可以吗",
            &settings(None, &url),
            BUDGET,
            &deps(chat.clone(), None),
        );
        assert_eq!(
            (out.text.as_str(), out.scene),
            ("明天的会改到下午三点可以吗？", Scene::Chat)
        );
        assert!(seen.lock().unwrap()[0].body.contains("明天的会"));
        let long = "测".repeat(INPUT_MAX + 1);
        let out = run(&long, &settings(None, &url), BUDGET, &deps(chat, None));
        assert_eq!((out.text == long, out.why.as_str()), (true, "input"));
        assert_eq!(seen.lock().unwrap().len(), 1, "nothing sent for it");
    }

    /// A stub that answers by the rules the request was sent with: the
    /// shell's row gets `shell`, every other row gets `other`.
    fn by_scene(shell: &'static str, other: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
        server(vec![Reply::ByBody(vec![
            ("a shell command line", Reply::Says(shell)),
            ("Transcript", Reply::Says(other)),
        ])])
    }

    fn window(app: &str, title: &str) -> Option<Focus> {
        Some(Focus {
            app: app.into(),
            title: title.into(),
        })
    }

    /// The 12:56 case: a command dictated into a window the rules do not
    /// know (the companion's own) is rewritten with the shell's rules.
    #[test]
    fn a_command_into_an_unknown_window_gets_the_shells_rules() {
        let (url, seen) = by_scene("systemctl status", "System CTL status");
        let out = run(
            "System CTL status",
            &settings(None, &url),
            BUDGET,
            &deps(
                window("ai.cohdl.openwalkietalkie", "OpenWalkieTalkie"),
                None,
            ),
        );
        assert_eq!(
            (out.text.as_str(), out.scene, out.words),
            ("systemctl status", Scene::Shell, true)
        );
        assert!(seen.lock().unwrap()[0]
            .body
            .contains("a shell command line"));
    }

    /// A browser terminal (firefox is unknown to the rules) with a command.
    #[test]
    fn a_command_into_a_browser_terminal_gets_the_shells_rules() {
        let (url, _) = by_scene("kubectl get pods -n kube-system", "x");
        let out = run(
            "kubectl get pods dash n kube dash system",
            &settings(None, &url),
            BUDGET,
            &deps(window("firefox", "Cloud Shell"), None),
        );
        assert_eq!(
            (out.text.as_str(), out.scene, out.words),
            ("kubectl get pods -n kube-system", Scene::Shell, true)
        );
    }

    /// Prose into the companion's window stays prose: least change.
    #[test]
    fn prose_into_the_companion_window_is_left_as_prose() {
        let (url, seen) = by_scene("WRONG", "remind me to call the bank tomorrow");
        let out = run(
            "remind me to call the bank tomorrow",
            &settings(None, &url),
            BUDGET,
            &deps(
                window("ai.cohdl.openwalkietalkie", "OpenWalkieTalkie"),
                None,
            ),
        );
        assert_eq!((out.scene, out.words), (Scene::Editor, false));
        assert_eq!(out.text, "remind me to call the bank tomorrow");
        assert!(seen.lock().unwrap()[0]
            .body
            .contains("a code or text editor"));
    }

    /// A sentence that starts with a command word goes with the shell's
    /// rules, whose own instruction keeps a sentence a sentence; one that
    /// does not start with one is not moved at all.
    #[test]
    fn a_sentence_that_starts_with_a_command_word_stays_a_sentence() {
        let (url, seen) = by_scene("find the thing from the other folder", "x");
        let out = run(
            "find the thing from the other folder",
            &settings(None, &url),
            BUDGET,
            &deps(
                window("ai.cohdl.openwalkietalkie", "OpenWalkieTalkie"),
                None,
            ),
        );
        assert_eq!((out.scene, out.words), (Scene::Shell, true));
        assert_eq!(
            (out.text.as_str(), out.why.as_str()),
            ("find the thing from the other folder", "unchanged")
        );
        assert!(seen.lock().unwrap()[0]
            .body
            .contains("If they said a sentence rather than a command, keep the sentence"));
        let (url, _) = by_scene("WRONG", "get the thing from the other folder");
        let out = run(
            "get the thing from the other folder",
            &settings(None, &url),
            BUDGET,
            &deps(
                window("ai.cohdl.openwalkietalkie", "OpenWalkieTalkie"),
                None,
            ),
        );
        assert_eq!(
            (out.scene, out.words, out.text.as_str()),
            (Scene::Editor, false, "get the thing from the other folder")
        );
    }

    /// A window the rules know keeps its scene, words or not.
    #[test]
    fn a_known_chat_window_keeps_its_scene_for_a_command() {
        let (url, _) = by_scene("WRONG", "git status");
        let out = run(
            "git status",
            &settings(None, &url),
            BUDGET,
            &deps(window("Slack", "general"), None),
        );
        assert_eq!((out.scene, out.words), (Scene::Chat, false));
    }

    /// The window captured at the press is the one used, not the one
    /// focused when the rewrite runs.
    #[test]
    fn the_window_captured_at_the_press_is_the_one_used() {
        let (url, _) = by_scene("git status", "get status");
        let out = run_with(
            "get status",
            &settings(None, &url),
            BUDGET,
            &deps(
                window("ai.cohdl.openwalkietalkie", "OpenWalkieTalkie"),
                None,
            ),
            Captured::Press(window("foot", "tony@host:~")),
        );
        assert_eq!(
            (out.text.as_str(), out.scene, out.focus_at),
            ("git status", Scene::Shell, "press")
        );
        let out = run(
            "get status",
            &settings(None, &url),
            BUDGET,
            &deps(window("ai.cohdl.openwalkietalkie", "x"), None),
        );
        assert_eq!(
            (out.text.as_str(), out.scene, out.focus_at),
            ("get status", Scene::Editor, "final")
        );
    }

    /// An Omarchy local model is called at its own discovered endpoint, not
    /// the kind's default port, with Omarchy's key.
    #[test]
    fn an_omarchy_model_is_called_at_its_discovered_endpoint() {
        let (url, seen) = server(vec![Reply::Says("git status")]);
        let settings = Settings {
            scene: None,
            edit: Some(Target {
                key_of: crate::providers::omarchy::ID.into(),
                kind: crate::providers::Kind::Omarchy,
                base_url: crate::providers::Kind::Omarchy.base_url().unwrap().into(),
                model: "qwen3.8-27b-gufo".into(),
                local: true,
                timeout: Some(crate::providers::LOCAL_TIMEOUT),
            }),
        };
        let endpoint = url.clone();
        let deps = Deps {
            curl: PathBuf::from("curl"),
            focus: Box::new(|| Some(Focus { app: "foot".into(), title: "~".into() })),
            keys: Box::new(|key_of| (key_of == "omarchy").then(|| "local-key".to_string())),
            local_endpoint: Box::new(move |model| (model == "qwen3.8-27b-gufo").then(|| endpoint.clone())),
            local_running: Box::new(|_| Some(true)),
        };
        let out = run("get status", &settings, BUDGET, &deps);
        assert_eq!((out.text.as_str(), out.why.as_str()), ("git status", "rewritten"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].authorization.as_deref(), Some("Bearer local-key"));
        assert!(seen[0].body.contains("\"model\":\"qwen3.8-27b-gufo\""));
    }

    fn local(base: &str, timeout: Duration) -> Target {
        Target {
            key_of: "box".into(),
            kind: crate::providers::Kind::Custom,
            base_url: base.into(),
            model: "edit-model".into(),
            local: true,
            timeout: Some(timeout),
        }
    }

    /// Only a local target raises the budget; a cloud one never does.
    #[test]
    fn the_budget_is_raised_only_by_a_local_provider_and_never_for_cloud() {
        let base = Duration::from_secs(3);
        let cloud = settings(Some("https://scene.example/v1"), "https://edit.example/v1");
        assert_eq!(budget_for(&cloud, base), base, "cloud: the budget as configured");
        let mut slow_cloud = cloud.clone();
        slow_cloud.edit.as_mut().unwrap().timeout = Some(Duration::from_secs(30));
        assert_eq!(budget_for(&slow_cloud, base), base, "a cloud provider's own timeout never raises it");
        let mut with_local = cloud.clone();
        with_local.edit = Some(local("http://127.0.0.1:1/v1", crate::providers::LOCAL_TIMEOUT));
        assert_eq!(budget_for(&with_local, base), crate::providers::LOCAL_TIMEOUT);
        with_local.edit.as_mut().unwrap().timeout = Some(Duration::from_secs(600));
        assert_eq!(budget_for(&with_local, base), crate::providers::TIMEOUT_MAX, "never past the cap: a press ends");
    }

    /// The same 4 s model: as a local provider (6 s of its own) it is
    /// waited for; as a cloud one the 3 s budget cuts it and the words go as
    /// heard, at about 3 s.
    #[test]
    fn a_slow_local_model_is_waited_for_and_a_slow_cloud_one_is_not() {
        let (url, _) = server(vec![Reply::After(Duration::from_millis(4000), Box::new(Reply::Says("git status")))]);
        let base = Duration::from_secs(3);
        let as_local = Settings { scene: None, edit: Some(local(&url, Duration::from_secs(6))) };
        let start = Instant::now();
        let out = run("get status", &as_local, budget_for(&as_local, base), &deps(terminal(), None));
        assert_eq!((out.text.as_str(), out.why.as_str(), out.local), ("git status", "rewritten", true));
        assert!(start.elapsed() >= Duration::from_millis(3900));
        let as_cloud = settings(None, &url);
        let start = Instant::now();
        let out = run("get status", &as_cloud, budget_for(&as_cloud, base), &deps(terminal(), None));
        let took = start.elapsed();
        assert_eq!((out.text.as_str(), out.why.as_str(), out.local), ("get status", "timeout", false));
        assert!(took < base + Duration::from_millis(400), "cloud stays within its budget: {took:?}");
    }

    /// A local model past even its own time says so; one known not to be
    /// running is not called at all.
    #[test]
    fn a_local_model_that_is_too_slow_or_not_running_says_so() {
        let (url, seen) = server(vec![Reply::After(Duration::from_millis(2500), Box::new(Reply::Says("git status")))]);
        let slow = Settings { scene: None, edit: Some(local(&url, Duration::from_millis(1200))) };
        let out = run("get status", &slow, budget_for(&slow, Duration::from_millis(1000)), &deps(terminal(), None));
        assert_eq!((out.text.as_str(), out.why.as_str()), ("get status", "slow_local"));
        let calls = seen.lock().unwrap().len();
        let mut omarchy = local(&url, Duration::from_secs(5));
        omarchy.kind = crate::providers::Kind::Omarchy;
        let stopped = Settings { scene: None, edit: Some(omarchy) };
        let mut d = deps(terminal(), None);
        d.local_running = Box::new(|_| Some(false));
        let start = Instant::now();
        let out = run("get status", &stopped, Duration::from_secs(5), &d);
        assert_eq!((out.text.as_str(), out.why.as_str()), ("get status", "local_not_running"));
        assert!(start.elapsed() < Duration::from_millis(200), "not waited for");
        assert_eq!(seen.lock().unwrap().len(), calls, "no call made");
    }
}
