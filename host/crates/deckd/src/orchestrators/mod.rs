//! `deckd::orchestrators` — fleet tools on the deck (`docs/orchestrator-adapters.md`).
//!
//! An orchestrator runs many agents at once. Its adapter does two things
//! only: it reads the fleet's state from the product's own CLI
//! ([`Orchestrator::status`], pure, no daemon) and it turns one dictated
//! order into the product's own "send this agent a message" verb
//! ([`Orchestrator::send_order`], argv only). Everything between — which
//! tasks get a place on the deck, what their cards say, how long a title
//! may be and what it may never contain — is decided here, once, so the
//! five adapters cannot drift apart.
//!
//! **Shape.** Each visible task becomes a poll-capability virtual agent
//! (§4.15) owned by the `deckd orchestrators sync` session (`crate::cli`),
//! the same shape `crate::grokbot` uses for its bots, rather than the
//! `crate::adapters` shape (a pure normaliser of a CLI's own hook events
//! into one agent): an orchestrator is a *source of many agents* whose
//! state we poll, not a hook stream from one session.
//!
//! The deck has two card types, `q` and `r` (§4.6), and five agent states.
//! The contract's table maps onto them like this ([`plan`]):
//!
//! | Task | Agent state | Card |
//! |---|---|---|
//! | `Blocked(Approval)` | `question` | `q`, choices [`APPROVAL_CHOICES`] |
//! | `Blocked(Question)` | `question` | `q`, free text (PTT answers) |
//! | `Blocked(Failed)` | `error` | none: the state is the signal, and no `card.clear` can take it away |
//! | `Working` | `working` | none: the roster row carries the activity |
//! | `Done` | `ready` | `r`, pushed once (the deck chimes for a new card only) |
//! | `Idle` | not on the deck | — |
//!
//! **Content discipline (§11).** A title is the most sensitive string in
//! the daemon. Every string an adapter hands over passes [`title`] or
//! [`activity`] here: paths and secret-looking words are masked, and a
//! string that reads as a command line is replaced by the task's own name.
use crate::config::Config;
use deck_model::{sanitize, AgentState};
use serde::Serialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub mod cindy;
pub mod herdr;
pub mod multica;
pub mod orca;
pub mod t3;

/// A task's title on the deck, in characters.
pub const TITLE_MAX: usize = 48;
/// What a task is doing now, in characters.
pub const ACTIVITY_MAX: usize = 40;
/// A card's text, in characters.
pub const CARD_MAX: usize = 64;
/// The most cards one orchestrator may have on the deck.
pub const CARDS_MAX: usize = 8;
/// Slots on one roster page of the deck.
pub const DECK_SLOTS: usize = 8;
/// The choices on an approval card: ENTER takes the first, CANCEL the second.
pub const APPROVAL_CHOICES: [&str; 2] = ["Approve", "Decline"];
/// The task key of an orchestrator's summary entry.
pub const SUMMARY_KEY: &str = "summary";
/// An agent key starts with this, then `<orchestrator>:<task key>`.
pub const KEY_PREFIX: &str = "orch:";

// ---------------------------------------------------------------------------
// Task model
// ---------------------------------------------------------------------------

/// One unit the operator can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Stable within the orchestrator; the address for an order.
    pub key: String,
    /// ≤ [`TITLE_MAX`] chars, already redacted ([`title`]).
    pub title: String,
    pub state: TaskState,
    /// ≤ [`ACTIVITY_MAX`] chars, what it is doing now, never a command line
    /// ([`activity`]).
    pub activity: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Working,
    Blocked(Blocked),
    Done,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    Approval,
    Question,
    Failed,
}

impl TaskState {
    /// The word `status` prints, and the card's reason.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            TaskState::Working => "working",
            TaskState::Blocked(Blocked::Approval) => "waiting for approval",
            TaskState::Blocked(Blocked::Question) => "has a question",
            TaskState::Blocked(Blocked::Failed) => "failed",
            TaskState::Done => "done",
            TaskState::Idle => "idle",
        }
    }

    #[must_use]
    pub fn is_blocked(self) -> bool {
        matches!(self, TaskState::Blocked(_))
    }

    /// Which tasks get the deck first: the blocked ones, then the finished
    /// ones, then the working ones.
    fn rank(self) -> u8 {
        match self {
            TaskState::Blocked(Blocked::Approval) => 0,
            TaskState::Blocked(Blocked::Question) => 1,
            TaskState::Blocked(Blocked::Failed) => 2,
            TaskState::Done => 3,
            TaskState::Working => 4,
            TaskState::Idle => 5,
        }
    }
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

/// One fleet tool. `status` and `parse` must work with no daemon and no
/// deck; `send_order` is argv only, never a shell string.
pub trait Orchestrator {
    /// The CLI's word (`herdr`): the `<name>` of `deckd orchestrators <name>`.
    fn name(&self) -> &'static str;
    /// How a person names it (`Herdr`).
    fn label(&self) -> &'static str;
    /// The fleet now: run the product's CLI and [`parse`](Self::parse) it.
    fn status(&self) -> Result<Vec<Task>, String>;
    /// The CLI's `--json` output as tasks. What `status --from-file` runs.
    fn parse(&self, raw: &str) -> Result<Vec<Task>, String>;
    /// Send `text` to the task `key`. An `Err` starts with one reason word
    /// (`^[a-z0-9_]{1,32}$`), optionally followed by `: detail` for the
    /// terminal; the word is what `agent.order.result` carries ([`reason_word`]).
    fn send_order(&self, key: &str, text: &str) -> Result<(), String>;
    /// A card's text for `task`, through the shared rules.
    fn card_text(&self, task: &Task) -> String {
        let name = title(&task.title, &task.key);
        clip(&format!("{name}: {}", task.state.word()), CARD_MAX)
    }
}

/// A module's constructor: `None` when it is not available (a stub).
pub type Constructor = fn(&Config) -> Option<Box<dyn Orchestrator>>;

/// Every adapter, with the branch that owns it.
pub const MODULES: [(&str, &str, Constructor); 5] = [
    ("herdr", "feat/orchestrator-herdr", herdr::new),
    ("orca", "feat/orchestrator-orca", orca::new),
    ("multica", "feat/orchestrator-multica", multica::new),
    ("t3", "feat/orchestrator-t3", t3::new),
    ("cindy", "feat/orchestrator-cindy", cindy::new),
];

/// Every adapter that is available in this build.
#[must_use]
pub fn all(cfg: &Config) -> Vec<Box<dyn Orchestrator>> {
    MODULES.iter().filter_map(|(_, _, new)| new(cfg)).collect()
}

/// The adapter called `name`, if it is available.
#[must_use]
pub fn by_name(cfg: &Config, name: &str) -> Option<Box<dyn Orchestrator>> {
    MODULES
        .iter()
        .find(|(n, _, _)| *n == name)
        .and_then(|(_, _, new)| new(cfg))
}

// ---------------------------------------------------------------------------
// Text rules: every adapter's strings pass through these
// ---------------------------------------------------------------------------

/// Whitespace collapsed, control characters and ANSI gone, at most `max`
/// characters, ending in `…` when anything was dropped.
#[must_use]
pub fn clip(raw: &str, max: usize) -> String {
    let clean: String = sanitize::strip_ansi(raw)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = sanitize::collapse(&clean);
    if clean.chars().count() <= max {
        return clean;
    }
    let mut out = sanitize::clip_chars(&clean, max.saturating_sub(1));
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// A word that is a path: `/x`, `~/x`, `./x`, `C:\x`, or anything with a
/// separator in it.
fn is_path(word: &str) -> bool {
    word.contains('/') || word.contains('\\') || word.starts_with('~')
}

/// A word that may be a secret: a known token prefix, a `key=value` whose
/// key says secret, or a long run of letters and digits mixed.
fn is_secret(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    const PREFIXES: [&str; 8] = [
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xox",
        "akia",
        "glpat-",
        "eyj",
    ];
    if PREFIXES.iter().any(|p| lower.starts_with(p)) && word.len() >= 12 {
        return true;
    }
    if let Some((key, _)) = lower.split_once('=') {
        if [
            "token", "key", "secret", "password", "passwd", "auth", "bearer",
        ]
        .iter()
        .any(|k| key.contains(k))
        {
            return true;
        }
    }
    let body = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    body.len() >= 24
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+=.".contains(c))
        && body.chars().any(|c| c.is_ascii_digit())
        && body.chars().any(|c| c.is_ascii_alphabetic())
}

/// `raw` with every path word masked as `[path]` and every secret-looking
/// word as `[redacted]`. No length limit (see [`clip`]).
#[must_use]
pub fn redact(raw: &str) -> String {
    let clean = clip(raw, usize::MAX);
    clean
        .split(' ')
        .map(|word| {
            if is_secret(word) {
                "[redacted]"
            } else if is_path(word) {
                "[path]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Does `raw` read as a command line rather than a name? Shell operators,
/// a prompt sign, or a first word that is a common program.
#[must_use]
pub fn looks_like_command(raw: &str) -> bool {
    let text = raw.trim();
    if text.starts_with('$') || text.starts_with('#') || text.starts_with('>') {
        return true;
    }
    if ["&&", "||", " | ", ";", "`", "$(", " > ", " >> ", " < "]
        .iter()
        .any(|op| text.contains(op))
    {
        return true;
    }
    let first = text.split_whitespace().next().unwrap_or_default();
    let first = first.rsplit(['/', '\\']).next().unwrap_or(first);
    const PROGRAMS: [&str; 32] = [
        "sudo", "rm", "mv", "cp", "chmod", "chown", "curl", "wget", "ssh", "scp", "git", "gh",
        "npm", "npx", "pnpm", "yarn", "bun", "node", "python", "python3", "pip", "cargo", "make",
        "docker", "kubectl", "bash", "sh", "zsh", "env", "export", "cd", "cat",
    ];
    // A program word followed by more words; a pane called just "git" is a name.
    PROGRAMS.contains(&first) && text.split_whitespace().nth(1).is_some()
}

/// A task's title: `raw` redacted and clipped to [`TITLE_MAX`], or the
/// task's own `name` when `raw` is empty or reads as a command line.
#[must_use]
pub fn title(raw: &str, name: &str) -> String {
    let redacted = redact(raw);
    if redacted.is_empty() || looks_like_command(raw) {
        let name = redact(name);
        if looks_like_command(name.as_str()) || name.is_empty() {
            return "task".to_string();
        }
        return clip(&name, TITLE_MAX);
    }
    clip(&redacted, TITLE_MAX)
}

/// What a task is doing: `None` for nothing, or for a command line.
#[must_use]
pub fn activity(raw: &str) -> Option<String> {
    if looks_like_command(raw) {
        return None;
    }
    let text = clip(&redact(raw), ACTIVITY_MAX);
    (!text.is_empty()).then_some(text)
}

/// A key an order may be addressed to: 1–64 characters, no path, no
/// control characters, not starting with `-` (it goes into argv).
#[must_use]
pub fn safe_key(key: &str) -> bool {
    let n = key.chars().count();
    (1..=64).contains(&n)
        && !key.starts_with('-')
        && !is_path(key)
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || " ._:@#+-".contains(c))
}

/// The word `agent.order.result` carries for an adapter's `Err`: its first
/// word when that is one (`not_installed: …`), `backend_refused` otherwise.
#[must_use]
pub fn reason_word(err: &str) -> String {
    let word = err.split(':').next().unwrap_or_default().trim();
    let ok = (1..=32).contains(&word.len())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok { word } else { "backend_refused" }.to_string()
}

/// The JSON a CLI printed, from its first `{` or `[`: some CLIs print a
/// banner line first.
pub fn json_payload(raw: &str) -> Result<Value, String> {
    let start = raw
        .find(['{', '['])
        .ok_or_else(|| "malformed: no JSON in the output".to_string())?;
    serde_json::from_str(raw[start..].trim()).map_err(|e| format!("malformed: {e}"))
}

// ---------------------------------------------------------------------------
// Running a product's CLI
// ---------------------------------------------------------------------------

/// Run `program argv…` with no stdin and no shell, and give its stdout.
/// Killed after `timeout`. An `Err` is a reason word and a short detail
/// that never quotes the program's own output (it may hold paths).
pub fn run(program: &str, argv: &[OsString], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("not_installed: {program} is not on PATH"),
            _ => format!("spawn_failed: {program}: {}", e.kind()),
        })?;
    // Read on a thread so a full pipe cannot stall the child.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut out);
        }
        out
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timeout: {program} took longer than {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("spawn_failed: {program}: {}", e.kind())),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        let code = status
            .code()
            .map_or("a signal".to_string(), |c| c.to_string());
        return Err(format!("backend_refused: {program} exited with {code}"));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

// ---------------------------------------------------------------------------
// Card mapping
// ---------------------------------------------------------------------------

/// A card on the deck.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Card {
    /// `q`: choices, or none for a free-text question.
    Q { text: String, choices: Vec<String> },
    /// `r`: a finished task, once.
    R { text: String },
}

/// One virtual agent the deck shows for an orchestrator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The task's key, or [`SUMMARY_KEY`].
    pub key: String,
    /// The agent's name on the deck.
    pub name: String,
    #[serde(serialize_with = "state_word")]
    pub state: AgentState,
    /// The roster row's title.
    pub title: String,
    pub card: Option<Card>,
    /// A summary's hidden tasks (their titles), read out on PTT.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<String>,
}

fn state_word<S: serde::Serializer>(state: &AgentState, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match state {
        AgentState::Working => "working",
        AgentState::Question => "question",
        AgentState::Ready => "ready",
        AgentState::Error => "error",
        AgentState::Idle => "idle",
    })
}

/// How many entries each orchestrator may have when `configured` are on
/// the deck: `slots / configured`, at most [`CARDS_MAX`], at least one, and
/// never every slot (the deck's own CLIs need one).
#[must_use]
pub fn budget(configured: usize) -> usize {
    (DECK_SLOTS / configured.max(1))
        .min(CARDS_MAX)
        .min(DECK_SLOTS - 1)
        .max(1)
}

/// The agent key of entry `key` of orchestrator `name`.
#[must_use]
pub fn agent_key(name: &str, key: &str) -> String {
    format!("{KEY_PREFIX}{name}:{key}")
}

/// One task's entry.
fn entry(orch: &dyn Orchestrator, task: &Task) -> Entry {
    let name = title(&task.title, &task.key);
    let text = clip(&redact(&orch.card_text(task)), CARD_MAX);
    let (state, card) = match task.state {
        TaskState::Blocked(Blocked::Approval) => (
            AgentState::Question,
            Some(Card::Q {
                text,
                choices: APPROVAL_CHOICES.iter().map(|c| c.to_string()).collect(),
            }),
        ),
        TaskState::Blocked(Blocked::Question) => (
            AgentState::Question,
            Some(Card::Q {
                text,
                choices: Vec::new(),
            }),
        ),
        TaskState::Blocked(Blocked::Failed) => (AgentState::Error, None),
        TaskState::Done => (AgentState::Ready, Some(Card::R { text })),
        TaskState::Working => (AgentState::Working, None),
        TaskState::Idle => (AgentState::Idle, None),
    };
    let row = match (task.state, &task.activity) {
        (TaskState::Working, Some(a)) => activity(a).unwrap_or_else(|| "working".into()),
        (state, _) => state.word().to_string(),
    };
    Entry {
        key: task.key.clone(),
        name,
        state,
        title: row,
        card,
        list: Vec::new(),
    }
}

/// What the deck shows for `tasks`: at most `budget` entries (see
/// [`budget`]), blocked tasks first; idle tasks never. When the fleet does
/// not fit, the last place is a summary ("12 waiting: Herdr") whose PTT
/// reads out the tasks it stands for.
#[must_use]
pub fn plan(orch: &dyn Orchestrator, tasks: &[Task], budget: usize) -> Vec<Entry> {
    let budget = budget.clamp(1, CARDS_MAX);
    let mut visible: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.state != TaskState::Idle && safe_key(&t.key))
        .collect();
    visible.sort_by_key(|t| t.state.rank());
    if visible.len() <= budget {
        return visible.into_iter().map(|t| entry(orch, t)).collect();
    }
    let (shown, hidden) = visible.split_at(budget - 1);
    let mut out: Vec<Entry> = shown.iter().map(|t| entry(orch, t)).collect();
    let waiting = hidden.iter().filter(|t| t.state.is_blocked()).count();
    let label = orch.label();
    let (state, text, card) = if waiting > 0 {
        let text = clip(&format!("{waiting} waiting: {label}"), CARD_MAX);
        let card = Card::Q {
            text: text.clone(),
            choices: Vec::new(),
        };
        (AgentState::Question, text, Some(card))
    } else {
        let text = clip(&format!("{} more: {label}", hidden.len()), CARD_MAX);
        (AgentState::Working, text, None)
    };
    out.push(Entry {
        key: SUMMARY_KEY.to_string(),
        name: clip(label, TITLE_MAX),
        state,
        title: text,
        card,
        list: hidden
            .iter()
            .filter(|t| waiting == 0 || t.state.is_blocked())
            .map(|t| format!("{} ({})", title(&t.title, &t.key), t.state.word()))
            .collect(),
    });
    out
}

/// `deckd orchestrators <name> status --json`: the tasks and the deck's
/// plan for them. Keys are [`safe_key`]s and titles went through [`title`],
/// so nothing here is a path, a command line or a token.
#[must_use]
pub fn status_json(orch: &dyn Orchestrator, tasks: &[Task], budget: usize) -> Value {
    let tasks_json: Vec<Value> = tasks
        .iter()
        .filter(|t| safe_key(&t.key))
        .map(|t| {
            json!({
                "key": t.key,
                "title": title(&t.title, &t.key),
                "state": match t.state {
                    TaskState::Blocked(_) => "blocked",
                    s => s.word(),
                },
                "blocked": match t.state {
                    TaskState::Blocked(Blocked::Approval) => Some("approval"),
                    TaskState::Blocked(Blocked::Question) => Some("question"),
                    TaskState::Blocked(Blocked::Failed) => Some("failed"),
                    _ => None,
                },
                "activity": t.activity.as_deref().and_then(activity),
            })
        })
        .collect();
    json!({
        "orchestrator": orch.name(),
        "label": orch.label(),
        "tasks": tasks_json,
        "deck": plan(orch, tasks, budget),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An orchestrator with no CLI behind it, for the mapping tests.
    pub(crate) struct Fake;

    impl Orchestrator for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn label(&self) -> &'static str {
            "Fake"
        }
        fn status(&self) -> Result<Vec<Task>, String> {
            Ok(Vec::new())
        }
        fn parse(&self, _raw: &str) -> Result<Vec<Task>, String> {
            Ok(Vec::new())
        }
        fn send_order(&self, _key: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn task(key: &str, state: TaskState) -> Task {
        Task {
            key: key.into(),
            title: key.into(),
            state,
            activity: None,
        }
    }

    #[test]
    fn an_empty_fleet_puts_nothing_on_the_deck() {
        assert!(plan(&Fake, &[], budget(1)).is_empty());
        let only_idle = [task("a", TaskState::Idle), task("b", TaskState::Idle)];
        assert!(plan(&Fake, &only_idle, budget(1)).is_empty());
    }

    #[test]
    fn one_blocked_task_is_one_question_card() {
        let tasks = [task("docs", TaskState::Blocked(Blocked::Approval))];
        let deck = plan(&Fake, &tasks, budget(1));
        assert_eq!(deck.len(), 1);
        assert_eq!(deck[0].key, "docs");
        assert_eq!(deck[0].state, AgentState::Question);
        assert_eq!(
            deck[0].card,
            Some(Card::Q {
                text: "docs: waiting for approval".into(),
                choices: vec!["Approve".into(), "Decline".into()],
            })
        );
    }

    #[test]
    fn every_state_maps_as_the_table_says() {
        let tasks = [
            task("q", TaskState::Blocked(Blocked::Question)),
            task("f", TaskState::Blocked(Blocked::Failed)),
            task("d", TaskState::Done),
            Task {
                activity: Some("writing tests".into()),
                ..task("w", TaskState::Working)
            },
            task("i", TaskState::Idle),
        ];
        let deck = plan(&Fake, &tasks, budget(1));
        let by = |k: &str| deck.iter().find(|e| e.key == k).cloned();
        let q = by("q").expect("question");
        assert!(matches!(q.card, Some(Card::Q { ref choices, .. }) if choices.is_empty()));
        let f = by("f").expect("failed");
        assert_eq!((f.state, f.card), (AgentState::Error, None));
        let d = by("d").expect("done");
        assert_eq!(d.state, AgentState::Ready);
        assert!(matches!(d.card, Some(Card::R { .. })));
        let w = by("w").expect("working");
        assert_eq!(
            (w.state, w.card, w.title.as_str()),
            (AgentState::Working, None, "writing tests")
        );
        assert!(by("i").is_none(), "idle is absence");
        // Blocked first.
        assert_eq!(deck[0].key, "q");
    }

    #[test]
    fn a_forty_task_fleet_is_a_summary_not_a_wall() {
        let tasks: Vec<Task> = (0..40)
            .map(|i| {
                task(
                    &format!("worker-{i}"),
                    TaskState::Blocked(Blocked::Approval),
                )
            })
            .collect();
        for configured in 1..=5 {
            let budget = budget(configured);
            let deck = plan(&Fake, &tasks, budget);
            assert!(deck.len() <= CARDS_MAX, "≤8 cards");
            assert!(
                deck.len() < DECK_SLOTS,
                "one orchestrator never takes every slot"
            );
            assert!(
                deck.len() * configured <= DECK_SLOTS,
                "fair share with {configured}"
            );
            let summary = deck.last().expect("a summary");
            assert_eq!(summary.key, SUMMARY_KEY);
            let hidden = 40 - (deck.len() - 1);
            assert_eq!(summary.title, format!("{hidden} waiting: Fake"));
            assert_eq!(summary.list.len(), hidden);
            assert!(matches!(summary.card, Some(Card::Q { .. })));
        }
    }

    #[test]
    fn a_fleet_that_only_works_gets_a_quiet_summary() {
        let tasks: Vec<Task> = (0..20)
            .map(|i| task(&format!("w{i}"), TaskState::Working))
            .collect();
        let deck = plan(&Fake, &tasks, 4);
        assert_eq!(deck.len(), 4);
        assert_eq!(deck[3].title, "17 more: Fake");
        assert_eq!(deck[3].card, None);
    }

    #[test]
    fn clip_counts_characters_and_marks_the_cut() {
        assert_eq!(clip("  a\tb\n c ", 10), "a b c");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("日本語のタイトルです", 4).chars().count(), 4);
        assert!(clip(&"x".repeat(200), TITLE_MAX).chars().count() <= TITLE_MAX);
        assert_eq!(clip("\u{1b}[31mred\u{1b}[0m", 10), "red");
    }

    #[test]
    fn titles_never_carry_paths_tokens_or_command_lines() {
        assert_eq!(title("edit /etc/hosts now", "x"), "edit [path] now");
        assert_eq!(title("fix ~/src/app", "x"), "fix [path]");
        assert_eq!(title("use sk-abcdefghijklmnop1234", "x"), "use [redacted]");
        assert_eq!(title("GITHUB_TOKEN=abc", "x"), "[redacted]");
        assert_eq!(title("rm -rf build", "cleanup"), "cleanup");
        assert_eq!(title("cargo test && git push", "ci"), "ci");
        assert_eq!(title("$ make", "build"), "build");
        assert_eq!(title("", "docs"), "docs");
        assert_eq!(title("git", "x"), "git", "a one-word name is a name");
        assert_eq!(title("Refactor the API", "x"), "Refactor the API");
        assert_eq!(activity("npm run build"), None);
        assert_eq!(
            activity("reading the spec").as_deref(),
            Some("reading the spec")
        );
    }

    #[test]
    fn keys_and_reasons_are_checked() {
        assert!(safe_key("w1:p2"));
        assert!(safe_key("api refactor"));
        assert!(!safe_key("-rf"));
        assert!(!safe_key("../x"));
        assert!(!safe_key(""));
        assert_eq!(
            reason_word("not_installed: herdr is not on PATH"),
            "not_installed"
        );
        assert_eq!(reason_word("Something broke"), "backend_refused");
    }

    #[test]
    fn json_is_found_after_a_banner() {
        let v = json_payload("orca v1.4.180 — update available\n{\"a\":1}").expect("json");
        assert_eq!(v["a"], 1);
        assert!(json_payload("no json here").is_err());
        assert!(json_payload("{not json").is_err());
    }

    #[test]
    fn the_registry_lists_five_and_four_are_stubs() {
        let cfg = Config::default();
        assert_eq!(MODULES.len(), 5);
        let names: Vec<&str> = all(&cfg).iter().map(|o| o.name()).collect();
        assert_eq!(names, ["herdr"]);
        assert!(by_name(&cfg, "orca").is_none());
    }
}
