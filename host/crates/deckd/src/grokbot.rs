//! `deckd::grokbot` — Grok Bot (xAI's teammate app) on the deck.
//!
//! Grok Bot has no public API (Cursor's staff said so on 2026-10-26). It
//! documents three ways in, and this module uses two of them:
//!
//! - **A routine's webhook** carries the deck to a bot. The owner gives each
//!   bot one routine with a webhook trigger and pastes its URL and key here
//!   (`deckd grokbot add`, the Integrations page). A press addressed to the
//!   bot on the deck becomes an order, and the order is a `POST` of the
//!   transcript to that webhook ([`webhook`]); a `200` means the bot started
//!   a run. Anything else is reported back as an undelivered order, which
//!   the deck shows.
//! - **The bot's own commands on this Mac** ("Execution on this computer"
//!   in Grok Bot's settings) carry the bot to the deck: `deckd grokbot say`
//!   puts its reply on the deck and reads it out, `deckd grokbot ask` puts
//!   a question card there and prints the operator's answer, and `deckd
//!   grokbot state` says whether it is working. Every order's body says how
//!   (`reply_with`, `ask_with`), and the routine's instruction
//!   ([`routine_text`]) tells the bot to use them.
//!
//! The third, a `grokbot://` link, only fills in the chat box and is not
//! used: the deck is for when the operator is not at the keyboard.
//!
//! **Shape.** Each bot set up in `[grokbot.bots.<id>]` is a **poll**-
//! capability agent owned by this module's internal session (§4.15), keyed
//! `grokbot:<id>`, of kind `grok` (a deck that does not draw that kind is
//! sent `other`, `Kind::for_deck`), on a roster page of its own ("Grok
//! Bot"). Orders and answers reach the module through `agent.wait`, exactly
//! as they reach codexd's threads and a third party's script, so the core
//! learned no new delivery mode. A bot that reports before it was set up
//! (it knows the command, the owner has not pasted its webhook yet) is put
//! on the deck by its name all the same: it can talk to the deck, and an
//! order to it is refused as `no_webhook`, which the deck shows.
//!
//! The module goes quiet with the rest of the collectors (`crate::
//! dormancy`) and with `[grokbot] enabled = false`: its bots leave the deck
//! and their commands are refused, so a bot left running cannot fill a deck
//! nobody is holding.
//!
//! **Content discipline (§11).** A transcript, a bot's reply, its question
//! and a webhook key never reach a log line; the log carries bot ids, the
//! webhook's host and statuses.
pub mod keys;
pub mod webhook;

use crate::api::dispatch::ConnCtx;
use crate::api::Api;
use crate::config::{GrokBot as Settings, GrokBotEntry};
use deck_model::{
    sanitize, AdapterKind, AgentId, AgentRegister, AgentState, AgentUpdate, Answer, AnswerCap,
    ApiError, Caps, CardClear, CardId, CardKind, CardSet, Choice, Deliver, Kind, OrderCap,
    OrderOutcome, Origin, Principal, ReplySet, Request, SessionId, TitleSource,
};
use keys::BotKeys;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

/// The client name of the module's internal session.
pub const CLIENT: &str = "grokbot";
/// A bot's agent key is this and its id.
pub const KEY_PREFIX: &str = "grokbot:";
/// A bot is drawn under this name when it gave none.
pub const DEFAULT_NAME: &str = "Grok Bot";
/// A bot that was sent an order and has said nothing since goes back to
/// idle after this long: nothing else would ever end its `working`.
pub const WORKING_FOR: Duration = Duration::from_secs(15 * 60);
/// `grokbot.ask` waits this long for the operator unless told otherwise,
/// and never longer than the most.
pub const ASK_DEFAULT_S: u64 = 300;
pub const ASK_MAX_S: u64 = 600;
/// What a bot may say in one `say`, in bytes (the deck shows the start; the
/// app's channel the whole reply).
pub const TEXT_MAX: usize = 8 * 1024;
/// A question's text, in bytes.
pub const QUESTION_MAX: usize = 512;
/// The deck shows four choices, each at most 32 bytes (§4.6).
pub const CHOICES_MAX: usize = 4;
pub const CHOICE_MAX: usize = 32;
/// A title's length (the roster's), shared with the other adapters.
const TITLE_MAX: usize = 64;
/// A quiet `agent.wait` is re-armed at this cadence.
const WAIT_S: u64 = 300;
/// The module's own clock: config changes, asks that ran out, bots that
/// stopped working.
const TICK: Duration = Duration::from_secs(1);
/// A roster the core would not complete is made again after this long.
const RETRY: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Names, ids and texts
// ---------------------------------------------------------------------------

// The names, ids and words are shared with the app, which writes a bot's
// setup prompt before the bot is added (`deck_model::grokbot`).
pub use deck_model::grokbot::{
    ask_command, id_problem, invocation, name_problem, reply_command, routine_text, setup_prompt,
    sh_quote, slug, HOW_TO_ANSWER, ID_MAX, NAME_MAX,
};

/// Which bot a command means: the id of a bot that is set up (by its id,
/// or by its name ignoring case), or the id a bot that is not set up is put
/// on the deck under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Configured(String),
    Unlisted { id: String, name: String },
}

impl Resolved {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Resolved::Configured(id) | Resolved::Unlisted { id, .. } => id,
        }
    }
}

/// `bot` as a command names it: an id or a name.
pub fn resolve(bots: &BTreeMap<String, GrokBotEntry>, bot: &str) -> Result<Resolved, ApiError> {
    let bot = bot.trim();
    if let Some(problem) = name_problem(bot) {
        return Err(ApiError::invalid("bot", problem));
    }
    if bots.contains_key(bot) {
        return Ok(Resolved::Configured(bot.to_string()));
    }
    if let Some((id, _)) = bots
        .iter()
        .find(|(_, entry)| entry.name.trim().eq_ignore_ascii_case(bot))
    {
        return Ok(Resolved::Configured(id.clone()));
    }
    let id = slug(bot);
    if bots.contains_key(&id) {
        return Ok(Resolved::Configured(id));
    }
    Ok(Resolved::Unlisted {
        id,
        name: bot.to_string(),
    })
}

/// The body of the webhook call an order becomes. `order` is the deck's
/// order id, `None` for a test; `invocation` starts the bot's commands
/// ([`bot_invocation`]).
#[must_use]
pub fn order_body(
    id: &str,
    name: &str,
    text: &str,
    order: Option<&str>,
    invocation: &str,
) -> String {
    json!({
        "source": "openwalkietalkie",
        "bot": name,
        "bot_id": id,
        "text": text,
        "order": order,
        "test": order.is_none(),
        "sent_at": rfc3339_now(),
        "reply_with": reply_command(invocation, id),
        "ask_with": ask_command(invocation, id),
        "how_to_answer": HOW_TO_ANSWER,
    })
    .to_string()
}

/// How the commands this service gives its bots start: its own `deckd`,
/// and `--data-dir` when its socket is not where a bare `deckd` looks
/// (Omarchy's unit runs it on `~/.local/share/openwalkietalkie`; a test
/// service runs on a directory of its own). A bot's shell has none of the
/// service's environment, so the command has to say.
#[must_use]
pub fn bot_invocation(deckd: &Path, paths: &crate::config::Paths) -> String {
    let elsewhere = match crate::config::Paths::platform_default() {
        Ok(bare) => bare.socket != paths.socket,
        Err(_) => true,
    };
    invocation(deckd, elsewhere.then_some(paths.data.as_path()))
}

/// Now, as the body's `sent_at` gives it: RFC 3339, seconds, UTC.
fn rfc3339_now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Now, in milliseconds since the epoch (`grokbot.status`'s `at_ms`).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The text of the test call (`grokbot.test`).
pub const TEST_TEXT: &str = "This is a test from my OpenWalkieTalkie deck. Reply with a short hello \
                             so I can hear it on the deck.";

// ---------------------------------------------------------------------------
// The handle the doors reach the module through
// ---------------------------------------------------------------------------

/// The module's mailbox and what the doors need of it, shared through
/// `rt::Adapters` so that both doors and the core reach the same one.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<Option<Attached>>,
    next_ask: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
struct Attached {
    tx: mpsc::UnboundedSender<Cmd>,
    keys: Arc<dyn BotKeys>,
    config: PathBuf,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").finish_non_exhaustive()
    }
}

impl Hub {
    fn attach(&self, tx: mpsc::UnboundedSender<Cmd>, keys: Arc<dyn BotKeys>, config: PathBuf) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner = Some(Attached { tx, keys, config });
        }
    }

    fn detach(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner = None;
        }
    }

    fn attached(&self) -> Result<Attached, ApiError> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.clone())
            .ok_or_else(|| ApiError::degraded("grokbot_not_running"))
    }

    fn send(&self, cmd: Cmd) -> Result<(), ApiError> {
        self.attached()?
            .tx
            .send(cmd)
            .map_err(|_| ApiError::degraded("grokbot_not_running"))
    }
}

/// What a door asks of the module.
enum Cmd {
    Say {
        bot: String,
        text: String,
        speak: bool,
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
    State {
        bot: String,
        state: AgentState,
        title: Option<String>,
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
    Ask {
        ask: u64,
        bot: String,
        question: String,
        choices: Vec<String>,
        free_text: bool,
        timeout: Duration,
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
    /// The caller of an ask went away before the answer came.
    AskGone { ask: u64 },
    Status {
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
    Test {
        bot: String,
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
    /// Make the roster match the settings now, and answer once it does.
    Sync {
        reply: oneshot::Sender<Result<Value, ApiError>>,
    },
}

/// Ends an ask whose caller went away: the card leaves the deck rather
/// than wait out its time for an answer nobody would read.
struct AskGuard {
    ask: u64,
    tx: Option<mpsc::UnboundedSender<Cmd>>,
}

impl AskGuard {
    fn disarm(mut self) {
        self.tx = None;
    }
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Cmd::AskGone { ask: self.ask });
        }
    }
}

// ---------------------------------------------------------------------------
// The doors' side: `grokbot.*`
// ---------------------------------------------------------------------------

/// `grokbot.<verb>`, answered on the tokio side: an ask waits for the
/// operator for minutes, and `add`/`remove` write files.
pub async fn api_dispatch(ctx: &ConnCtx, verb: &str, params: Value) -> Result<Value, ApiError> {
    let hub = ctx.core.adapters().grokbot.clone();
    match verb {
        "say" => {
            let bot = text_param(&params, "bot")?;
            let text = text_param(&params, "text")?;
            if text.len() > TEXT_MAX {
                return Err(ApiError::too_large(TEXT_MAX));
            }
            let speak = params.get("speak").and_then(Value::as_bool).unwrap_or(true);
            ask_module(&hub, |reply| Cmd::Say { bot, text, speak, reply }).await
        }
        "state" => {
            let bot = text_param(&params, "bot")?;
            let state = match params.get("state").and_then(Value::as_str) {
                Some("working") => AgentState::Working,
                Some("idle") => AgentState::Idle,
                Some("ready") => AgentState::Ready,
                Some("error") => AgentState::Error,
                _ => {
                    return Err(ApiError::invalid(
                        "state",
                        "working, idle, ready or error",
                    ))
                }
            };
            let title = params
                .get("title")
                .and_then(Value::as_str)
                .map(|t| sanitize::cut(&sanitize::collapse(t), TITLE_MAX))
                .filter(|t| !t.is_empty());
            ask_module(&hub, |reply| Cmd::State { bot, state, title, reply }).await
        }
        "ask" => {
            let bot = text_param(&params, "bot")?;
            let question = text_param(&params, "question")?;
            if question.len() > QUESTION_MAX {
                return Err(ApiError::too_large(QUESTION_MAX));
            }
            let choices: Vec<String> = match params.get("choices") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty() && s.len() <= CHOICE_MAX)
                            .ok_or_else(|| {
                                ApiError::invalid("choices", "each choice is 1 to 32 bytes of text")
                            })
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(ApiError::invalid("choices", "a list of strings")),
            };
            if choices.len() > CHOICES_MAX {
                return Err(ApiError::invalid("choices", "the deck shows at most four choices"));
            }
            let free_text = params
                .get("free_text")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if choices.is_empty() && !free_text {
                return Err(ApiError::invalid(
                    "choices",
                    "a question with no choices is answered by voice: leave free_text on",
                ));
            }
            let timeout_s = params
                .get("timeout_s")
                .and_then(Value::as_u64)
                .unwrap_or(ASK_DEFAULT_S)
                .clamp(5, ASK_MAX_S);
            let attached = hub.attached()?;
            let ask = hub
                .next_ask
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let guard = AskGuard {
                ask,
                tx: Some(attached.tx.clone()),
            };
            let result = ask_module(&hub, |reply| Cmd::Ask {
                ask,
                bot,
                question,
                choices,
                free_text,
                timeout: Duration::from_secs(timeout_s),
                reply,
            })
            .await;
            guard.disarm();
            result
        }
        "status" => ask_module(&hub, |reply| Cmd::Status { reply }).await,
        "test" => {
            let bot = text_param(&params, "bot")?;
            ask_module(&hub, |reply| Cmd::Test { bot, reply }).await
        }
        "add" => add(ctx, &hub, &params).await,
        "remove" => remove(ctx, &hub, &params).await,
        other => Err(ApiError::method_not_found(&format!("grokbot.{other}"))),
    }
}

fn text_param(params: &Value, key: &str) -> Result<String, ApiError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::invalid_params(&format!("{key} is required")))
}

async fn ask_module(
    hub: &Hub,
    cmd: impl FnOnce(oneshot::Sender<Result<Value, ApiError>>) -> Cmd,
) -> Result<Value, ApiError> {
    let (tx, rx) = oneshot::channel();
    hub.send(cmd(tx))?;
    rx.await
        .unwrap_or_else(|_| Err(ApiError::degraded("grokbot_not_running")))
}

/// `grokbot.add {name, webhook, key?, id?}`: the bot's table in
/// `config.toml` and its key in the store, then a reload so the bot is on
/// the deck before the reply. A bot that is there already is updated in
/// place (its name, its webhook, its key when one is given).
async fn add(ctx: &ConnCtx, hub: &Hub, params: &Value) -> Result<Value, ApiError> {
    let attached = hub.attached()?;
    let name = text_param(params, "name")?;
    if let Some(problem) = name_problem(&name) {
        return Err(ApiError::invalid("name", problem));
    }
    let webhook = text_param(params, "webhook")?;
    if let Some(problem) = webhook::url_problem(&webhook) {
        return Err(ApiError::invalid("webhook", problem));
    }
    let id = match params.get("id").and_then(Value::as_str).map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => slug(&name),
    };
    if let Some(problem) = id_problem(&id) {
        return Err(ApiError::invalid("id", problem));
    }
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    if key.as_ref().is_some_and(|k| k.len() > 4096 || k.chars().any(char::is_control)) {
        return Err(ApiError::invalid("key", "the key is one line of at most 4096 characters"));
    }
    // The key first: a bot on the deck whose key did not take would refuse
    // every order, and the owner would not know why.
    if let Some(key) = key.clone() {
        let keys = attached.keys.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || keys.set(&id, &key))
            .await
            .map_err(|e| ApiError::internal(&e.to_string()))?
            .map_err(|e| ApiError::internal(&format!("the key was not kept: {e}")))?;
    }
    let patch = vec![
        (format!("grokbot.bots.{id}.name"), Value::String(name.clone())),
        (format!("grokbot.bots.{id}.webhook"), Value::String(webhook.clone())),
    ];
    let next = write_config(&attached.config, patch).await?;
    reload(ctx, hub, next).await;
    let keys = attached.keys.clone();
    let has_key = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || keys.exists(&id).unwrap_or(false))
            .await
            .unwrap_or(false)
    };
    tracing::info!(bot = %id, host = %webhook::host(&webhook), has_key, "grokbot: a bot was set up");
    Ok(json!({"id": id, "name": name, "has_key": has_key}))
}

/// `grokbot.remove {bot}`: the bot's table and its key; the bot leaves the
/// deck with the reload.
async fn remove(ctx: &ConnCtx, hub: &Hub, params: &Value) -> Result<Value, ApiError> {
    let attached = hub.attached()?;
    let bot = text_param(params, "bot")?;
    let config = crate::config::Config::load(&attached.config)
        .map_err(|e| ApiError::invalid("config.toml", &e.to_string()))?;
    let id = match resolve(&config.grokbot.bots, &bot)? {
        Resolved::Configured(id) => id,
        Resolved::Unlisted { .. } => return Err(ApiError::not_found("bot")),
    };
    let next =
        write_config(&attached.config, vec![(format!("grokbot.bots.{id}"), Value::Null)]).await?;
    let keys = attached.keys.clone();
    let key_removed = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || keys.delete(&id))
            .await
            .map_err(|e| ApiError::internal(&e.to_string()))?
            .map_err(|e| ApiError::internal(&format!("the key was not removed: {e}")))?
    };
    reload(ctx, hub, next).await;
    tracing::info!(bot = %id, "grokbot: a bot was removed");
    Ok(json!({"id": id, "removed": true, "key_removed": key_removed}))
}

/// The edit, validated whole before anything is written (`config::edit`
/// keeps every other byte of the operator's file); the config it wrote.
async fn write_config(
    path: &Path,
    patch: Vec<(String, Value)>,
) -> Result<crate::config::Config, ApiError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(ApiError::internal(&e.to_string())),
        };
        crate::config::Config::parse(&text)
            .map_err(|e| ApiError::invalid("config.toml", &e.to_string()))?;
        let mut edited = text;
        for (key, value) in &patch {
            edited = crate::config::edit(&edited, key, value)
                .map_err(|e| ApiError::invalid("config.toml", &e.to_string()))?;
        }
        let next = crate::config::Config::parse(&edited)
            .map_err(|e| ApiError::invalid("config.toml", &e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ApiError::internal(&e.to_string()))?;
        }
        crate::config::write(&path, &edited).map_err(|e| ApiError::internal(&e.to_string()))?;
        Ok(next)
    })
    .await
    .map_err(|e| ApiError::internal(&e.to_string()))?
}

/// Put the edited file in force now rather than at the watcher's next turn:
/// the doors' copy (the watcher, finding the file it would load already in
/// force, then has nothing to do), the core's, and the module's roster.
async fn reload(ctx: &ConnCtx, hub: &Hub, next: crate::config::Config) {
    ctx.adapters.config.store(Arc::new(next));
    let _ = ask_module(hub, |reply| Cmd::Sync { reply }).await;
    let reply = ctx
        .core
        .call(crate::api::Api::internal_ctx(), "config.reload", json!({}))
        .await;
    if let Err(e) = reply.result {
        tracing::debug!(code = e.code, "grokbot: the reload after a write was refused");
    }
}

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

/// What the waiters and the webhook calls hand the loop.
enum Mail {
    Order {
        id: String,
        agent: AgentId,
        text: String,
        order: Option<String>,
    },
    /// Card ids are the agent's own, so an answer is matched by both.
    Answer {
        agent: AgentId,
        card: CardId,
        answer: Answer,
    },
    Snooze {
        agent: AgentId,
        card: CardId,
    },
    /// A test call's outcome: recorded, and nothing else.
    Tested {
        id: String,
        outcome: webhook::Outcome,
    },
    /// The wait was refused: the agent is gone from the store.
    Gone { id: String, agent: AgentId },
    Posted {
        id: String,
        agent: AgentId,
        title: String,
        order: Option<String>,
        outcome: webhook::Outcome,
    },
}

/// One bot on the deck.
struct Bot {
    name: String,
    /// Set up in `config.toml`, rather than put there by its own command.
    configured: bool,
    agent: AgentId,
    waiter: tokio::task::JoinHandle<()>,
    /// Sent an order and silent since: back to idle at this time.
    working_until: Option<Instant>,
}

/// A question waiting for the operator.
struct Ask {
    bot: String,
    agent: AgentId,
    card: CardId,
    deadline: Instant,
    reply: oneshot::Sender<Result<Value, ApiError>>,
}

/// The last webhook call of a bot, for `grokbot.status`.
#[derive(Clone, Debug)]
struct Last {
    at_ms: u64,
    outcome: webhook::Outcome,
}

struct Module {
    api: Arc<Api>,
    keys: Arc<dyn BotKeys>,
    curl: PathBuf,
    deckd: PathBuf,
    /// How the bot's commands start ([`bot_invocation`]).
    invocation: String,
    principal: Principal,
    session: Option<SessionId>,
    mail: mpsc::UnboundedSender<Mail>,
    bots: BTreeMap<String, Bot>,
    asks: HashMap<u64, Ask>,
    last: BTreeMap<String, Last>,
    /// The settings and the gate the roster was last made to match.
    applied: Option<(Settings, bool)>,
    /// Whether the bots may be on the deck now: switched on, and the
    /// collectors not quiet.
    live: bool,
    /// A registration the core refused: the roster is made again then.
    retry_at: Option<Instant>,
    /// The gate as the loop last read it.
    collect: bool,
}

/// The module's loop: started with the service, ended by `stop`.
pub async fn run(api: Arc<Api>, keys: Arc<dyn BotKeys>, mut stop: watch::Receiver<bool>) {
    let hub = api.core.adapters().grokbot.clone();
    let (tx, mut commands) = mpsc::unbounded_channel();
    hub.attach(tx, keys.clone(), api.paths.config.clone());
    let (mail_tx, mut mail) = mpsc::unbounded_channel();
    let mut gate = api.core.adapters().watch_gate();
    let deckd = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("deckd"));
    let mut module = Module {
        api: api.clone(),
        keys,
        curl: webhook::curl(),
        invocation: bot_invocation(&deckd, &api.paths),
        deckd,
        principal: Principal::Adapter(AdapterKind::Grok),
        session: None,
        mail: mail_tx,
        bots: BTreeMap::new(),
        asks: HashMap::new(),
        last: BTreeMap::new(),
        applied: None,
        live: false,
        retry_at: None,
        collect: true,
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        module.collect = gate.borrow_and_update().collect;
        module.sync(module.collect).await;
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    break;
                }
            }
            Some(cmd) = commands.recv() => module.command(cmd).await,
            Some(item) = mail.recv() => module.mail(item).await,
            _ = gate.changed() => {}
            _ = tick.tick() => {}
        }
    }
    hub.detach();
    module.shutdown().await;
}

impl Module {
    fn settings(&self) -> Settings {
        self.api.config.load().grokbot.clone()
    }

    /// Make the roster match the settings and the gate, and run the clocks.
    async fn sync(&mut self, collect: bool) {
        let settings = self.settings();
        let live = settings.enabled && collect;
        if self.retry_at.is_some_and(|at| at <= Instant::now()) {
            self.retry_at = None;
            self.applied = None;
        }
        if self.applied.as_ref() != Some(&(settings.clone(), collect)) {
            self.live = live;
            if live {
                if !self.reconcile(&settings).await {
                    self.retry_at = Some(Instant::now() + RETRY);
                }
            } else if settings.enabled {
                // The collectors went quiet (`crate::dormancy`).
                self.release_all(crate::dormancy::DORMANT, "no_deck").await;
            } else {
                self.release_all("released", "grokbot_off").await;
            }
            self.applied = Some((settings, collect));
        }
        self.clocks().await;
    }

    /// Every bot that is set up on the deck under its current name; every bot
    /// that was set up and is not any more, off it. `false` when a bot could
    /// not be registered.
    async fn reconcile(&mut self, settings: &Settings) -> bool {
        let mut complete = true;
        for (id, entry) in &settings.bots {
            if id_problem(id).is_some() {
                tracing::warn!(bot = %id, "grokbot: an id in config.toml is not one; skipped");
                continue;
            }
            let name = display_name(entry);
            match self.bots.get_mut(id) {
                Some(bot) => {
                    bot.configured = true;
                    if bot.name != name {
                        bot.name = name.clone();
                        let agent = bot.agent.clone();
                        self.call(Request::AgentUpdate(update(agent, |u| u.name = Some(name))))
                            .await;
                    }
                }
                None => {
                    complete &= self.register(id, &name, true).await.is_some();
                }
            }
        }
        let gone: Vec<String> = self
            .bots
            .iter()
            .filter(|(id, bot)| bot.configured && !settings.bots.contains_key(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.release(&id, "released", "removed").await;
        }
        complete
    }

    /// The internal session the bots are owned by, opened on first use.
    async fn session(&mut self) -> Option<SessionId> {
        if self.session.is_none() {
            self.session = self
                .api
                .core
                .adapters()
                .session(&self.api.core, AdapterKind::Grok, CLIENT)
                .await;
        }
        self.session
    }

    async fn call(&mut self, req: Request) -> Option<Value> {
        let session = self.session().await?;
        match self
            .api
            .core
            .api(self.principal.clone(), session, false, req)
            .await
        {
            Ok(value) => Some(value),
            Err(e) => {
                tracing::debug!(code = e.code, "grokbot: a request was refused");
                None
            }
        }
    }

    /// Put bot `id` on the deck, with a wait loop that brings its orders
    /// and answers back.
    async fn register(&mut self, id: &str, name: &str, configured: bool) -> Option<AgentId> {
        let session = self.session().await?;
        let register = AgentRegister {
            kind: Kind::Grok,
            name: name.to_string(),
            key: Some(format!("{KEY_PREFIX}{id}")),
            cwd: None,
            pid: None,
            pid_kind: None,
            terminal: None,
            lease_ttl_s: 0,
            caps: Caps {
                answer: AnswerCap::Poll,
                order: OrderCap::Poll,
                focus: false,
                transcribe: Default::default(),
            },
            slot_hint: None,
            voice: None,
            meta: Some(json!({"via": "grokbot"})),
        };
        let result = self.call(Request::AgentRegister(register)).await?;
        let agent = AgentId(result["agent"].as_str()?.to_string());
        let waiter = tokio::spawn(wait_loop(
            self.api.clone(),
            session,
            agent.clone(),
            id.to_string(),
            self.mail.clone(),
        ));
        if let Some(old) = self.bots.insert(
            id.to_string(),
            Bot {
                name: name.to_string(),
                configured,
                agent: agent.clone(),
                waiter,
                working_until: None,
            },
        ) {
            old.waiter.abort();
        }
        tracing::info!(bot = %id, configured, "grokbot: a bot is on the deck");
        Some(agent)
    }

    /// Take bot `id` off the deck: `reason` is the store's word for the
    /// agent's end, `why` what a command still waiting on it is told.
    async fn release(&mut self, id: &str, reason: &str, why: &str) {
        let Some(bot) = self.bots.remove(id) else {
            return;
        };
        bot.waiter.abort();
        self.fail_asks(Some(id), why);
        self.call(Request::AgentRelease {
            agent: bot.agent,
            reason: Some(reason.to_string()),
        })
        .await;
        tracing::info!(bot = %id, why, "grokbot: a bot left the deck");
    }

    async fn release_all(&mut self, reason: &str, why: &str) {
        let ids: Vec<String> = self.bots.keys().cloned().collect();
        for id in ids {
            self.release(&id, reason, why).await;
        }
        self.fail_asks(None, why);
    }

    fn fail_asks(&mut self, bot: Option<&str>, why: &str) {
        let ended: Vec<u64> = self
            .asks
            .iter()
            .filter(|(_, ask)| bot.is_none_or(|bot| ask.bot == bot))
            .map(|(id, _)| *id)
            .collect();
        for id in ended {
            if let Some(ask) = self.asks.remove(&id) {
                let _ = ask.reply.send(Err(ApiError::degraded(why)));
            }
        }
    }

    /// The bot a command means, put on the deck when it is not there yet.
    async fn bot(&mut self, bot: &str) -> Result<(String, AgentId), ApiError> {
        if !self.live {
            let why = if self.settings().enabled { "no_deck" } else { "grokbot_off" };
            return Err(ApiError::degraded(why));
        }
        let settings = self.settings();
        let resolved = resolve(&settings.bots, bot)?;
        let id = resolved.id().to_string();
        if let Some(bot) = self.bots.get(&id) {
            return Ok((id, bot.agent.clone()));
        }
        let (name, configured) = match &resolved {
            Resolved::Configured(id) => (display_name(&settings.bots[id]), true),
            Resolved::Unlisted { name, .. } => (name.clone(), false),
        };
        let agent = self
            .register(&id, &name, configured)
            .await
            .ok_or_else(|| ApiError::internal("the bot could not be put on the deck"))?;
        Ok((id, agent))
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Say { bot, text, speak, reply } => {
                let result = self.say(&bot, &text, speak).await;
                let _ = reply.send(result);
            }
            Cmd::State { bot, state, title, reply } => {
                let result = self.state(&bot, state, title).await;
                let _ = reply.send(result);
            }
            Cmd::Ask {
                ask,
                bot,
                question,
                choices,
                free_text,
                timeout,
                reply,
            } => {
                match self.ask(&bot, &question, choices, free_text).await {
                    Ok((id, agent, card)) => {
                        self.asks.insert(
                            ask,
                            Ask {
                                bot: id,
                                agent,
                                card,
                                deadline: Instant::now() + timeout,
                                reply,
                            },
                        );
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::AskGone { ask } => {
                if let Some(ask) = self.asks.remove(&ask) {
                    self.clear(ask.agent, ask.card, "gone").await;
                }
            }
            Cmd::Status { reply } => {
                let status = self.status().await;
                let _ = reply.send(Ok(status));
            }
            Cmd::Test { bot, reply } => self.test(&bot, reply),
            Cmd::Sync { reply } => {
                self.sync(self.collect).await;
                let _ = reply.send(Ok(Value::Null));
            }
        }
    }

    async fn say(&mut self, bot: &str, text: &str, speak: bool) -> Result<Value, ApiError> {
        let (id, agent) = self.bot(bot).await?;
        let text = sanitize::cut(text.trim(), TEXT_MAX);
        let set = Request::Reply(ReplySet {
            agent: agent.clone(),
            text,
            message: None,
            speak,
            announce: None,
            lane: Some("ready".into()),
        });
        self.call(set)
            .await
            .ok_or_else(|| ApiError::internal("the reply was not taken"))?;
        self.call(Request::AgentUpdate(update(agent.clone(), |u| {
            u.state = Some(AgentState::Ready);
        })))
        .await;
        if let Some(bot) = self.bots.get_mut(&id) {
            bot.working_until = None;
        }
        Ok(json!({"bot": id, "agent": agent.0, "spoken": speak}))
    }

    async fn state(
        &mut self,
        bot: &str,
        state: AgentState,
        title: Option<String>,
    ) -> Result<Value, ApiError> {
        let (id, agent) = self.bot(bot).await?;
        self.call(Request::AgentUpdate(update(agent.clone(), |u| {
            u.state = Some(state);
            if let Some(title) = title {
                u.title = Some(title);
                u.title_source = Some(TitleSource::Api);
            }
        })))
        .await
        .ok_or_else(|| ApiError::internal("the state was not taken"))?;
        if let Some(bot) = self.bots.get_mut(&id) {
            bot.working_until = (state == AgentState::Working).then(|| Instant::now() + WORKING_FOR);
        }
        Ok(json!({"bot": id, "agent": agent.0}))
    }

    async fn ask(
        &mut self,
        bot: &str,
        question: &str,
        choices: Vec<String>,
        free_text: bool,
    ) -> Result<(String, AgentId, CardId), ApiError> {
        let (id, agent) = self.bot(bot).await?;
        if self.asks.values().any(|ask| ask.bot == id) {
            return Err(ApiError::bad_state(
                "pending",
                Some("this bot's last question is still on the deck"),
            ));
        }
        let set = CardSet {
            agent: agent.clone(),
            id: None,
            kind: CardKind::Q,
            text: sanitize::cut(question.trim(), QUESTION_MAX),
            choices: choices
                .into_iter()
                .map(|label| Choice { label, detail: None })
                .collect(),
            rec: None,
            free_text,
            deliver: Deliver::Poll,
            timeout_s: None,
            origin: Origin::custom(),
        };
        let result = self
            .call(Request::CardSet(set))
            .await
            .ok_or_else(|| ApiError::internal("the question was not taken"))?;
        let card = result["card"]["id"]
            .as_u64()
            .and_then(|id| u16::try_from(id).ok())
            .map(CardId)
            .ok_or_else(|| ApiError::internal("the question has no card"))?;
        if let Some(bot) = self.bots.get_mut(&id) {
            bot.working_until = None;
        }
        Ok((id, agent, card))
    }

    async fn clear(&mut self, agent: AgentId, card: CardId, why: &str) {
        self.call(Request::CardClear(CardClear {
            agent,
            card,
            delivered: false,
            reason: Some(why.to_string()),
        }))
        .await;
    }

    /// The asks that ran out, and the bots that went quiet.
    async fn clocks(&mut self) {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .asks
            .iter()
            .filter(|(_, ask)| ask.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(ask) = self.asks.remove(&id) {
                let _ = ask.reply.send(Ok(json!({"timeout": true})));
                self.clear(ask.agent, ask.card, "ask_timeout").await;
            }
        }
        let quiet: Vec<AgentId> = self
            .bots
            .values_mut()
            .filter(|bot| bot.working_until.is_some_and(|until| until <= now))
            .map(|bot| {
                bot.working_until = None;
                bot.agent.clone()
            })
            .collect();
        for agent in quiet {
            self.call(Request::AgentUpdate(update(agent, |u| {
                u.state = Some(AgentState::Idle);
            })))
            .await;
        }
    }

    async fn mail(&mut self, item: Mail) {
        match item {
            Mail::Order { id, agent, text, order } => self.order(id, agent, text, order).await,
            Mail::Posted {
                id,
                agent,
                title,
                order,
                outcome,
            } => {
                tracing::info!(
                    bot = %id,
                    status = ?outcome.status(),
                    accepted = outcome.accepted(),
                    "grokbot: an order went to the bot's webhook"
                );
                self.last.insert(
                    id.clone(),
                    Last {
                        at_ms: now_ms(),
                        outcome: outcome.clone(),
                    },
                );
                if outcome.accepted() {
                    self.call(Request::AgentUpdate(update(agent, |u| {
                        u.state = Some(AgentState::Working);
                        u.title = Some(title);
                        u.title_source = Some(TitleSource::Prompt);
                    })))
                    .await;
                    if let Some(bot) = self.bots.get_mut(&id) {
                        bot.working_until = Some(Instant::now() + WORKING_FOR);
                    }
                } else if let (Some(order), Some(reason)) = (order, outcome.reason()) {
                    self.undelivered(agent, order, &reason).await;
                }
            }
            Mail::Answer {
                agent,
                card,
                answer,
            } => {
                let Some((&ask_id, _)) = self
                    .asks
                    .iter()
                    .find(|(_, ask)| ask.agent == agent && ask.card == card)
                else {
                    tracing::debug!(card = card.0, "grokbot: an answer nobody waits for");
                    return;
                };
                if let Some(ask) = self.asks.remove(&ask_id) {
                    let _ = ask.reply.send(Ok(json!({"answer": {
                        "choice": answer.choice,
                        "label": answer.label,
                        "text": answer.text,
                    }})));
                    let id = ask.bot.clone();
                    self.call(Request::AgentUpdate(update(ask.agent, |u| {
                        u.state = Some(AgentState::Working);
                    })))
                    .await;
                    if let Some(bot) = self.bots.get_mut(&id) {
                        bot.working_until = Some(Instant::now() + WORKING_FOR);
                    }
                }
            }
            Mail::Snooze { agent, card } => {
                let Some((&ask_id, _)) = self
                    .asks
                    .iter()
                    .find(|(_, ask)| ask.agent == agent && ask.card == card)
                else {
                    return;
                };
                if let Some(ask) = self.asks.remove(&ask_id) {
                    let _ = ask.reply.send(Ok(json!({"snoozed": true})));
                    self.clear(ask.agent, ask.card, "snoozed").await;
                }
            }
            Mail::Tested { id, outcome } => {
                self.last.insert(
                    id,
                    Last {
                        at_ms: now_ms(),
                        outcome,
                    },
                );
            }
            Mail::Gone { id, agent } => {
                if self.bots.get(&id).is_some_and(|bot| bot.agent == agent) {
                    if let Some(bot) = self.bots.remove(&id) {
                        bot.waiter.abort();
                    }
                    self.fail_asks(Some(&id), "gone");
                    // Put back at the next sync, if it is still set up.
                    self.applied = None;
                }
            }
        }
    }

    /// An order for a bot: posted to its webhook off the loop.
    async fn order(&mut self, id: String, agent: AgentId, text: String, order: Option<String>) {
        let settings = self.settings();
        let Some(entry) = settings.bots.get(&id).filter(|entry| !entry.webhook.is_empty()) else {
            if let Some(order) = order {
                self.undelivered(agent, order, "no_webhook").await;
            }
            return;
        };
        let name = display_name(entry);
        let url = entry.webhook.clone();
        let keys = self.keys.clone();
        let curl = self.curl.clone();
        let body = order_body(&id, &name, &text, order.as_deref(), &self.invocation);
        let title = sanitize::cut(&sanitize::collapse(&text), TITLE_MAX);
        let mail = self.mail.clone();
        tokio::spawn(async move {
            let outcome = tokio::task::spawn_blocking(move || match keys.get(&id) {
                Ok(Some(key)) => (id, webhook::send(&curl, &webhook::Post { url, key, body })),
                Ok(None) => (
                    id,
                    webhook::Outcome::Failed {
                        exit: None,
                        why: "no key".into(),
                    },
                ),
                Err(e) => (
                    id,
                    webhook::Outcome::Failed {
                        exit: None,
                        why: e.to_string(),
                    },
                ),
            })
            .await;
            if let Ok((id, outcome)) = outcome {
                let _ = mail.send(Mail::Posted {
                    id,
                    agent,
                    title,
                    order,
                    outcome,
                });
            }
        });
    }

    /// An order that did not reach its bot, said once (`agent.order.result`)
    /// so the deck shows it.
    async fn undelivered(&mut self, agent: AgentId, order: String, reason: &str) {
        self.call(Request::OrderOutcome(OrderOutcome {
            agent,
            order,
            ok: false,
            reason: Some(reason.to_string()),
        }))
        .await;
    }

    fn test(&mut self, bot: &str, reply: oneshot::Sender<Result<Value, ApiError>>) {
        let settings = self.settings();
        let id = match resolve(&settings.bots, bot) {
            Ok(Resolved::Configured(id)) => id,
            Ok(Resolved::Unlisted { .. }) => {
                let _ = reply.send(Err(ApiError::not_found("bot")));
                return;
            }
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        let entry = settings.bots[&id].clone();
        let name = display_name(&entry);
        let keys = self.keys.clone();
        let curl = self.curl.clone();
        let body = order_body(&id, &name, TEST_TEXT, None, &self.invocation);
        let mail = self.mail.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let key = keys
                    .get(&id)
                    .map_err(|e| ApiError::internal(&e.to_string()))?
                    .ok_or_else(|| ApiError::invalid("key", "this bot has no key: add it again with its key"))?;
                let outcome = webhook::send(
                    &curl,
                    &webhook::Post {
                        url: entry.webhook.clone(),
                        key,
                        body,
                    },
                );
                Ok::<_, ApiError>((id, outcome))
            })
            .await
            .map_err(|e| ApiError::internal(&e.to_string()))
            .and_then(|r| r);
            let answer = result.map(|(id, outcome)| {
                let value = json!({
                    "bot": id,
                    "ok": outcome.accepted(),
                    "status": outcome.status(),
                    "reason": outcome.reason(),
                    "why": match &outcome {
                        webhook::Outcome::Failed { why, .. } => Some(why.clone()),
                        _ => None,
                    },
                });
                let _ = mail.send(Mail::Tested { id, outcome });
                value
            });
            let _ = reply.send(answer);
        });
    }

    /// `grokbot.status`: the switch, where the keys live, the command a bot
    /// runs, and each bot — never a key, a URL's path or a transcript.
    async fn status(&mut self) -> Value {
        let settings = self.settings();
        let ids: Vec<String> = settings.bots.keys().cloned().collect();
        let keys = self.keys.clone();
        let has_key: BTreeMap<String, bool> = tokio::task::spawn_blocking(move || {
            ids.into_iter()
                .map(|id| {
                    let has = keys.exists(&id).unwrap_or(false);
                    (id, has)
                })
                .collect()
        })
        .await
        .unwrap_or_default();
        let mut bots: Vec<Value> = settings
            .bots
            .iter()
            .map(|(id, entry)| {
                let last = self.last.get(id).map(|last| {
                    json!({
                        "at_ms": last.at_ms,
                        "ok": last.outcome.accepted(),
                        "status": last.outcome.status(),
                        "reason": last.outcome.reason(),
                    })
                });
                json!({
                    "id": id,
                    "name": display_name(entry),
                    "set_up": true,
                    "webhook_host": webhook::host(&entry.webhook),
                    "has_key": has_key.get(id).copied().unwrap_or(false),
                    "on_deck": self.bots.contains_key(id),
                    "last": last,
                    "routine": routine_text(&display_name(entry), id, &self.invocation),
                })
            })
            .collect();
        for (id, bot) in self.bots.iter().filter(|(_, bot)| !bot.configured) {
            bots.push(json!({
                "id": id,
                "name": bot.name,
                "set_up": false,
                "webhook_host": null,
                "has_key": false,
                "on_deck": true,
                "last": null,
                "routine": null,
            }));
        }
        json!({
            "enabled": settings.enabled,
            "live": self.live,
            "key_store": self.keys.word(),
            "deckd": self.deckd.to_string_lossy(),
            "invocation": self.invocation,
            "app_installed": grok_bot_app().is_some(),
            "bots": bots,
        })
    }

    async fn shutdown(&mut self) {
        for bot in self.bots.values() {
            bot.waiter.abort();
        }
        self.fail_asks(None, "stopping");
    }
}

/// An `agent.update` of `agent` that changes what `f` sets.
fn update(agent: AgentId, f: impl FnOnce(&mut AgentUpdate)) -> AgentUpdate {
    let mut u = AgentUpdate {
        agent,
        state: None,
        title: None,
        title_source: None,
        since: None,
        name: None,
        meta: None,
        task: None,
        subagents: None,
    };
    f(&mut u);
    u
}

fn display_name(entry: &GrokBotEntry) -> String {
    let name = entry.name.trim();
    if name.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        name.chars().take(NAME_MAX).collect()
    }
}

/// Grok Bot's app, where it is installed ([`grok_bot_places`]).
#[must_use]
pub fn grok_bot_app() -> Option<PathBuf> {
    // The platform's own home folder: Windows sets no HOME (a service
    // started at logon has only USERPROFILE), which left Grok Bot's
    // per-user install unlooked-for there.
    let home = directories::BaseDirs::new()
        .map(|base| base.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    grok_bot_places(home.as_deref(), std::env::var_os("PATH").as_deref())
        .into_iter()
        .find(|place| place.exists())
}

/// Where Grok Bot's app may be, in order. On a Mac: `/Applications`, then
/// the home folder's Applications. On Linux, where Grok Bot ships `.deb`
/// and `.rpm` packages named `grok-bot`: that program on `PATH`, the
/// package's folder under `/opt`, and its desktop entry, system-wide or the
/// user's own.
#[must_use]
pub fn grok_bot_places(home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut places = Vec::new();
    if cfg!(target_os = "macos") {
        places.push(PathBuf::from("/Applications/Grok Bot.app"));
        places.extend(home.map(|home| home.join("Applications/Grok Bot.app")));
    } else if cfg!(target_os = "linux") {
        places.extend(path.into_iter().flat_map(std::env::split_paths).map(|dir| dir.join("grok-bot")));
        places.push(PathBuf::from("/opt/Grok Bot"));
        places.push(PathBuf::from("/usr/share/applications/grok-bot.desktop"));
        places.extend(home.map(|home| home.join(".local/share/applications/grok-bot.desktop")));
    } else if cfg!(windows) {
        places.extend(path.into_iter().flat_map(std::env::split_paths).flat_map(|dir| {
            [dir.join("Grok Bot.exe"), dir.join("grok-bot.exe")]
        }));
        places.extend(home.map(|home| home.join("AppData/Local/Programs/Grok Bot/Grok Bot.exe")));
    }
    places
}

/// One bot's `agent.wait` loop: its orders and the answers to its questions.
async fn wait_loop(
    api: Arc<Api>,
    session: SessionId,
    agent: AgentId,
    id: String,
    mail: mpsc::UnboundedSender<Mail>,
) {
    use crate::core::handle::WaitResult;
    loop {
        let asked = Instant::now();
        match api
            .core
            .wait(session, agent.clone(), Duration::from_secs(WAIT_S))
            .await
        {
            WaitResult::Answer { card, answer, .. } => {
                let _ = mail.send(Mail::Answer {
                    agent: agent.clone(),
                    card,
                    answer,
                });
                // A polled answer is delivered once the waiter has it.
                api.core.delivered(agent.clone(), card, true).await;
            }
            WaitResult::Order {
                text,
                card,
                order,
                delivery,
                ..
            } => {
                let _ = mail.send(Mail::Order {
                    id: id.clone(),
                    agent: agent.clone(),
                    text,
                    order,
                });
                if let (Some(card), Some(_)) = (card, delivery) {
                    api.core.delivered(agent.clone(), card, true).await;
                }
            }
            WaitResult::Snooze { card } => {
                let _ = mail.send(Mail::Snooze {
                    agent: agent.clone(),
                    card,
                });
            }
            // A wait the core answered at once is a core that is gone or
            // whose channel is full (`CoreHandle::wait`): wait a moment
            // rather than ask again in a loop that never yields.
            WaitResult::Timeout => {
                if asked.elapsed() < Duration::from_secs(1) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            WaitResult::Refused(e) => {
                tracing::debug!(code = e.code, bot = %id, "grokbot: the wait was refused");
                let _ = mail.send(Mail::Gone { id, agent });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bots(list: &[(&str, &str)]) -> BTreeMap<String, GrokBotEntry> {
        list.iter()
            .map(|(id, name)| {
                (
                    (*id).to_string(),
                    GrokBotEntry {
                        name: (*name).to_string(),
                        webhook: "https://example.com/hook".into(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_bot_id_comes_from_its_name() {
        assert_eq!(slug("Research"), "research");
        assert_eq!(slug("  Inbox Zero!  "), "inbox-zero");
        assert_eq!(slug("ops__bot--2"), "ops-bot-2");
        assert_eq!(slug("A".repeat(40).as_str()).len(), ID_MAX);
        // No ASCII at all: a stable hash, still a valid id.
        let chinese = slug("研究助手");
        assert!(chinese.starts_with("bot-") && chinese.len() == 12, "{chinese}");
        assert_eq!(chinese, slug("研究助手"));
        assert_ne!(chinese, slug("日程助手"));
        for id in ["research", "inbox-zero", chinese.as_str()] {
            assert_eq!(id_problem(id), None, "{id}");
        }
        for id in ["", "Research", "-a", "a-", "a b", "a_b", &"a".repeat(33)] {
            assert!(id_problem(id).is_some(), "{id:?}");
        }
        assert!(name_problem(" ").is_some());
        assert!(name_problem(&"x".repeat(33)).is_some());
        assert!(name_problem("tab\there").is_some());
        assert_eq!(name_problem("Research"), None);
    }

    #[test]
    fn a_command_names_a_bot_by_id_or_by_name() {
        let set_up = bots(&[("research", "Research"), ("inbox", "Inbox Zero")]);
        assert_eq!(resolve(&set_up, "research").unwrap(), Resolved::Configured("research".into()));
        assert_eq!(resolve(&set_up, "inbox zero").unwrap(), Resolved::Configured("inbox".into()));
        assert_eq!(resolve(&set_up, "INBOX").unwrap(), Resolved::Configured("inbox".into()));
        assert_eq!(
            resolve(&set_up, "Travel Desk").unwrap(),
            Resolved::Unlisted { id: "travel-desk".into(), name: "Travel Desk".into() }
        );
        assert!(resolve(&set_up, "").is_err());
    }

    #[test]
    fn a_service_on_its_own_directory_names_it_in_the_bot_s_commands() {
        let deckd = Path::new("/usr/local/bin/deckd");
        let dir = std::env::temp_dir().join("owt-grokbot-invocation");
        let own = crate::config::Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        assert_eq!(
            bot_invocation(deckd, &own),
            invocation(deckd, Some(&dir))
        );
        // Where a bare `deckd` looks anyway: no flag.
        let bare = crate::config::Paths::platform_default().unwrap();
        assert_eq!(bot_invocation(deckd, &bare), invocation(deckd, None));
    }

    #[test]
    #[cfg(windows)]
    fn windows_looks_for_the_executable_on_path_and_in_the_user_install() {
        let path = std::env::join_paths([r"C:\Tools", r"D:\Bots"]).unwrap();
        let places = grok_bot_places(Some(Path::new(r"C:\Users\Owner")), Some(&path));
        assert!(places.contains(&PathBuf::from(r"C:\Tools\Grok Bot.exe")));
        assert!(places.contains(&PathBuf::from(r"D:\Bots\grok-bot.exe")));
        assert!(places.contains(&PathBuf::from(r"C:\Users\Owner\AppData\Local\Programs\Grok Bot\Grok Bot.exe")));
    }

    #[test]
    fn grok_bot_is_looked_for_where_its_installers_put_it() {
        let places = grok_bot_places(
            Some(Path::new("/home/owner")),
            Some(std::ffi::OsStr::new("/usr/local/bin:/usr/bin")),
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                places,
                [
                    PathBuf::from("/Applications/Grok Bot.app"),
                    PathBuf::from("/home/owner/Applications/Grok Bot.app")
                ]
            );
        } else if cfg!(target_os = "linux") {
            for place in [
                "/usr/bin/grok-bot",
                "/opt/Grok Bot",
                "/usr/share/applications/grok-bot.desktop",
                "/home/owner/.local/share/applications/grok-bot.desktop",
            ] {
                assert!(places.contains(&PathBuf::from(place)), "{place}: {places:?}");
            }
        }
    }

    #[test]
    fn an_order_body_tells_the_bot_how_to_answer() {
        let deckd = &invocation(Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"), None);
        let body: Value =
            serde_json::from_str(&order_body("research", "Research", "Find it's flights", Some("or_1"), deckd))
                .unwrap();
        assert_eq!(body["source"], "openwalkietalkie");
        assert_eq!(body["text"], "Find it's flights");
        assert_eq!(body["order"], "or_1");
        assert_eq!(body["test"], false);
        assert_eq!(
            body["reply_with"],
            format!("{deckd} grokbot say --bot 'research' '<your reply>'")
        );
        assert!(body["ask_with"].as_str().unwrap().contains("grokbot ask --bot 'research' --choice 'Yes'"));
        assert!(body["sent_at"].as_str().unwrap().ends_with('Z'));
        // An older routine's instruction still has the answer kept in the chat.
        assert_eq!(body["how_to_answer"], HOW_TO_ANSWER);
        assert!(HOW_TO_ANSWER.contains("answer me in this chat"));
        let test: Value =
            serde_json::from_str(&order_body("research", "Research", TEST_TEXT, None, deckd)).unwrap();
        assert_eq!(test["test"], true);
        assert!(test["order"].is_null());
        // A quote in a path or an id cannot end the shell word early.
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        let routine = routine_text("Research", "research", deckd);
        assert!(routine.contains("(Research)") && routine.contains("grokbot say --bot 'research'"));
        assert!(routine.contains("Then also answer me here in this chat"));
    }
}
