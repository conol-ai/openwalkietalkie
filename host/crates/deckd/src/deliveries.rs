//! The delivery record: what became of every card, and how the deck's
//! reader was used. The companion design asks for "a content-free record of
//! what happened to each card, and a count of reader opens, to inform
//! decision 2" (phase 1, PR 6); decision 2 feeds the reader lazily, counts
//! its opens for a release and then decides what to trim. This is the
//! counting.
//!
//! It reads what the core already sees and changes nothing: every step's
//! events ([`Ledger::observe`]), every line the deck sends
//! ([`Ledger::device_line`]), and what the settle window did with Claude's
//! permission asks ([`Asks`], counted on the hook door's side by
//! `adapters::settle::Tally`).
//!
//! **Content-free by construction.** A record is ids (the opaque `ag_` id,
//! the card's number), times, flags, counts and words from the closed lists
//! in this file. A card's text, its choices, the label an answer chose, a
//! reply, an order's words, an agent's name, key or working directory, an
//! app's name, and every reason an app writes itself (`agent.card.clear
//! {reason}`, `agent.release{reason}`, `agent.order.result{reason}`) never
//! reach it: a word that is not on a list is `other`. So the record can sit
//! in `state.json`, travel in the support file and answer `metrics.get`
//! beside the link counters.
//!
//! **What is kept.** For each card, from its `card.set` to the event that
//! takes it away: its kind, origin and delivery mode, where it came from
//! (`claude`, `codex`, `other`, `app`, `daemon`), whether it reached the deck
//! and whether the deck was used while it was up, whether it was snoozed,
//! degraded or handed off, who answered it (`deck`, `ptt`, `gui`, `api`),
//! and what became of it ([`Record::outcome`]). The newest [`RECENT_MAX`]
//! such records, and totals kept since the first daemon that kept them
//! (`since`, `version`): every card from a source, by origin and by source,
//! the daemon's own failure cards apart (a failed order's and a hand-off's,
//! `card.set{host:"failure"}`), orders dispatched and failed, the settle
//! window's asks, and the reader counters.
//!
//! **Proxies, on firmware 0.13.0.** The deck reports neither its screen nor
//! what its reader shows, so three things are read off the lines it does
//! send. A firmware line saying what the screen shows would make each of
//! them exact; that is a firmware change, not part of this record.
//!
//! - *Reached*: the deck was live and the card's agent held a slot at some
//!   moment while the card was up. The card could be seen; nothing says it
//!   was.
//! - *Deck used*: an input only a person makes arrived while the card was up
//!   and had reached the deck, on any face. A waking press is never
//!   delivered, so any input that is means the screen was lit. A glance with
//!   no press reads as unused, and the lines the deck sends by itself — its
//!   heartbeat, the face it goes home to when idle, the reply window it
//!   follows, a request it retries — are not inputs.
//! - *Reader opens*: a `channel` line is a channel opened, by ENTER or by a
//!   PTT press (the firmware sends `channel` just before `ptt` when a press
//!   moves the channel, counted apart), and the deck sends none when the
//!   channel it opens is already the active one. Opening a channel shows the
//!   reply the host pushed and asks for nothing, so `history` counts reads
//!   past the window the deck holds: a scroll, the tail it follows, a
//!   refresh. A history, plan, card body or transcript read is one per slot
//!   until [`READ_WINDOW`] passes with no request for it; an image is one per
//!   open (its first request is retried every six seconds, three times at
//!   most); the model picker one per [`PICKER_WINDOW`]; every tool row toggle
//!   and every picker choice counts.
//!
//! **Stored** in `state.json` under `deliveries`: copied there when a record
//! closes, otherwise at most once every [`PERSIST_EVERY`], and at shutdown,
//! when the cards still up close as `shutdown`. A record this build cannot
//! read starts again empty rather than costing the rest of the file
//! ([`lenient`]).
use crate::core::event_agent;
use deck_model::{AgentId, DeviceLine, DocKind, Event, EventKind, SessionState, Snapshot, Ts};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// How many closed records `recent` keeps, oldest first.
pub const RECENT_MAX: usize = 100;
/// Between closed records, a changed record is copied into `state.json` at
/// most this often.
pub const PERSIST_EVERY: Duration = Duration::from_secs(60);
/// A history, plan, card body or transcript read: one per slot until this
/// long passes with no request for it.
pub const READ_WINDOW: Duration = Duration::from_secs(60);
/// An image open. The deck retries its first request every 6 s, three
/// times at most, all inside this.
pub const IMAGE_WINDOW: Duration = Duration::from_secs(30);
/// The model picker. The deck asks for its first page again on the way
/// back from a field.
pub const PICKER_WINDOW: Duration = Duration::from_secs(10);

/// Any word that is not on the list it was checked against.
const OTHER: &str = "other";
/// Where a card came from: the source of its agent's session
/// (`care::sources::Ledger::word`). A connected app is `app`, never its name.
const FROM: &[&str] = &["claude", "codex", "other", "app", "daemon", "unknown"];
const KINDS: &[&str] = &["q", "r"];
const ORIGINS: &[&str] = &["permission", "question", "elicitation", "custom", "ready"];
const DELIVERS: &[&str] = &["hold", "push", "poll", "focus", "none"];
/// `card.answered{source}`: a press on the deck, a dictated answer, the
/// Mac app, an API call.
const ANSWER_SOURCES: &[&str] = &["deck", "ptt", "gui", "api"];
/// `card.failed{reason}`: an answer that was not handed over.
const FAIL_REASONS: &[&str] = &["link", "timeout", "undeliverable", "hold_gone"];
/// The reasons a card ends under that are the daemon's own words: the
/// store's, the Claude adapter's, the Codex client's, and an agent's end
/// (the store's `grace` and `released`, the liveness poll's `gone`, the
/// adapters' `session_end`, the quiet service's `dormant`
/// (`crate::dormancy::DORMANT`), and Claude Code's own `SessionEnd`
/// reasons). Anything else is an app's own text and is `other`.
const END_REASONS: &[&str] = &[
    "replaced",
    "hold_written",
    "late_pickup",
    "group_step",
    "focus",
    "muted",
    "ack",
    "ttl",
    "state_change",
    "tool_ran",
    "turn_ended",
    "prompt",
    "idle",
    "session_start",
    "resolved",
    "thread_unloaded",
    "grace",
    "released",
    "gone",
    "session_end",
    "dormant",
    "clear",
    "resume",
    "logout",
    "prompt_input_exit",
];
/// End reasons that mean the card was decided in the session itself, never
/// on the deck: the call ran, the turn ended or moved on, the session
/// restarted, Codex resolved it.
const ELSEWHERE: &[&str] = &[
    "tool_ran",
    "turn_ended",
    "prompt",
    "idle",
    "session_start",
    "resolved",
];
/// Why an order was not delivered, or why the daemon put a failure card up
/// (`order.failed{reason}`, `card.set{host:"failure", reason}`): the
/// daemon's, its routes' and the hand-off's words. An owner's own
/// `agent.order.result{reason}` that is not one of them is `other`.
const FAILURE_REASONS: &[&str] = &[
    "no_route",
    "dialog_open",
    "owner_gone",
    "channel_full",
    "expired",
    "agent_gone",
    "no_agent",
    "undeliverable",
    "inbox_unreachable",
    "inbox_worker_failed",
    "inbox_write_failed",
    "channel_unavailable",
    "channel_session_changed",
    "channel_worker_failed",
    "channel_delivery_uncertain",
    "turn_refused",
    "no_app",
    "app_not_running",
    "raise_refused",
];
/// `order.dispatched{via}`.
const ROUTES: &[&str] = &["push", "poll", "inject"];
/// `order.failed{shown}`: what the deck was given for a failed order.
const SHOWN: &[&str] = &["card", "toast", "none"];

/// `word` as the list spells it, or `other`. The list's own `'static`
/// string is what is kept, so nothing a caller wrote can be.
fn listed(list: &[&'static str], word: &str) -> &'static str {
    list.iter().copied().find(|w| *w == word).unwrap_or(OTHER)
}

/// A reason as the record keeps it. The Codex client's one spelled with a
/// space ("thread unloaded") is kept as one word.
fn end_reason(reason: &str) -> &'static str {
    listed(END_REASONS, &reason.replace(' ', "_"))
}

/// The record, as `state.json` keeps it and `metrics.get` serves it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deliveries {
    /// When the first daemon that kept this record started counting.
    pub since: Option<Ts>,
    /// That daemon's version. The totals are cumulative from there, across
    /// updates.
    pub version: String,
    /// Every card from a source (Claude Code, Codex, another adapter, a
    /// connected app): not the daemon's own, nor its failure cards.
    pub cards: Totals,
    /// Every card but the failure cards, by origin (`permission`,
    /// `question`, `elicitation`, `custom`, `ready`).
    pub by_origin: BTreeMap<String, Totals>,
    /// The same cards by where they came from: `claude`, `codex`, `other`,
    /// `app`, or `daemon` for the daemon's own (the test question, the
    /// install's confirmation, the demo).
    pub by_from: BTreeMap<String, Totals>,
    /// The cards the daemon put up to show something that did not happen:
    /// a failed order, a hand-off that could not raise its app.
    pub failure_cards: Totals,
    pub orders: Orders,
    pub asks: Asks,
    pub reader: Reader,
    /// The newest [`RECENT_MAX`] cards that ended, oldest first.
    pub recent: VecDeque<Record>,
}

/// Counts over the cards that ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Totals {
    pub total: u64,
    /// Reached the deck ([`Record::reached`]).
    pub reached: u64,
    /// The deck was used while they were up ([`Record::deck_used`]).
    pub deck_used: u64,
    pub snoozed: u64,
    pub degraded: u64,
    /// What became of them: an outcome word ([`Record::outcome`]) → count.
    pub outcomes: BTreeMap<String, u64>,
    /// Where their answers came from: `deck`, `ptt`, `gui`, `api` → count.
    pub answered_on: BTreeMap<String, u64>,
}

impl Totals {
    fn add(&mut self, record: &Record) {
        self.total += 1;
        self.reached += u64::from(record.reached);
        self.deck_used += u64::from(record.deck_used);
        self.snoozed += u64::from(record.snoozed);
        self.degraded += u64::from(record.degraded);
        *self.outcomes.entry(record.outcome.clone()).or_default() += 1;
        if let Some(source) = &record.answered {
            *self.answered_on.entry(source.clone()).or_default() += 1;
        }
    }
}

/// Orders, by route and by why they failed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Orders {
    /// `order.dispatched`, by route: `push`, `poll`, `inject` (the daemon's
    /// own routes: Claude Channels, the session's inbox).
    pub dispatched: BTreeMap<String, u64>,
    /// `order.failed`, by reason.
    pub failed: BTreeMap<String, u64>,
    /// What the deck was given for a failed order: `card`, `toast`, `none`.
    pub shown: BTreeMap<String, u64>,
}

/// What the settle window did with Claude's permission asks: how many
/// waited out a window, and how each window ended — decided in the session
/// and never shown, a person being asked (Claude's own `permission_prompt`),
/// or the window passing with no sign either way. The last two reached the
/// deck.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asks {
    pub waited: u64,
    pub settled: u64,
    pub prompted: u64,
    pub passed: u64,
}

/// The deck's reader, as its requests show it (the module's proxies).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reader {
    /// Channels opened by ENTER, a sibling or a jump.
    pub channels: u64,
    /// Channels a PTT press moved to.
    pub channels_by_ptt: u64,
    /// Reads of a reply's history past the window the deck holds.
    pub history: u64,
    /// Reads of the workflow view (the plan and its tool list).
    pub plans: u64,
    /// Reads of a card's body past its first window.
    pub card_bodies: u64,
    /// Reads of the live transcript.
    pub transcripts: u64,
    /// Tool rows opened or closed.
    pub tool_rows: u64,
    /// Images opened.
    pub images: u64,
    /// Model pickers opened.
    pub pickers: u64,
    /// Choices made in one.
    pub picker_choices: u64,
    /// Every reader request, as it came.
    pub lines: u64,
}

/// One card, from its `card.set` to its end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub agent: AgentId,
    /// The card's number on the deck. A card set while its agent was off
    /// the deck takes a slot's number when it gets one (§6.5): the last one
    /// it had.
    pub card: u16,
    /// `q` (a question) or `r` (Ready).
    pub kind: String,
    pub origin: String,
    /// How an answer was to go, as the card was set.
    pub deliver: String,
    pub from: String,
    /// The reason word of a card the daemon put up to show a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub set: Ts,
    pub ended: Ts,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reached: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deck_used: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub snoozed: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
    /// Its `Open app` was pressed (`card.focused`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    /// Where its answer came from, when it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
    /// Why its answer was last not handed over (`card.failed{reason}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// What became of it, the first that holds of: `answered` (an answer
    /// was handed over), `failed` (an answer was not), `handed_off` (its
    /// `Open app` was pressed), `expired` (its time ran out, or the agent's
    /// next state change ended a Ready card), `replaced`, `muted` (its
    /// source was paused), `acked`, `elsewhere` (decided in the session),
    /// `agent_gone`, `shutdown`, `cleared` (by its owner, for its own
    /// reason).
    pub outcome: String,
    /// The reason word of the event that ended it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Default for Record {
    fn default() -> Self {
        Record {
            agent: AgentId(String::new()),
            card: 0,
            kind: String::new(),
            origin: String::new(),
            deliver: String::new(),
            from: String::new(),
            failure: None,
            set: Ts::default(),
            ended: Ts::default(),
            reached: false,
            deck_used: false,
            snoozed: false,
            degraded: false,
            focused: false,
            answered: None,
            failed: None,
            outcome: String::new(),
            reason: None,
        }
    }
}

/// The record in brief, for `status.json` and `deckd status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Brief {
    pub since: Option<Ts>,
    /// Cards from the sources that ended.
    pub cards: u64,
    /// …whose answer was handed over.
    pub answered: u64,
    /// Answers given on the deck, by a press or by voice.
    pub answers_on_deck: u64,
    pub failure_cards: u64,
    pub reader: Reader,
}

/// `state.json`'s `deliveries`, read without ever failing the file: a
/// record this build cannot read (an older daemon after an update, a hand
/// edit) starts again empty, and the slots, settings and card ids beside it
/// load as they are.
pub fn lenient<'de, D>(deserializer: D) -> Result<Deliveries, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|_| {
        tracing::warn!("state.json: the delivery record could not be read; it starts again");
        Deliveries::default()
    }))
}

/// How a card's answer last fared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Delivered,
    Failed,
}

/// A card still up: its record so far.
#[derive(Debug)]
struct Open {
    record: Record,
    delivery: Option<Delivery>,
    /// The card's own `created` stamp, which a slot's new number keeps.
    created: Option<Ts>,
}

/// The event that ended a card.
enum End<'a> {
    /// `card.expired{reason}`.
    Expired(Option<&'a str>),
    /// `card.cleared{delivered, reason}`.
    Cleared {
        delivered: bool,
        reason: Option<&'a str>,
    },
    /// `agent.removed{reason}`: the card went with its agent.
    AgentGone(Option<&'a str>),
    /// The daemon stopped with the card up.
    Shutdown,
}

impl End<'_> {
    fn reason(&self) -> Option<&'static str> {
        match self {
            End::Expired(reason) | End::Cleared { reason, .. } | End::AgentGone(reason) => {
                reason.map(end_reason)
            }
            End::Shutdown => None,
        }
    }
}

impl Open {
    /// The record, ended by `end` at `at`.
    fn finish(mut self, end: &End<'_>, at: Ts) -> Record {
        let reason = end.reason();
        self.record.outcome = self.outcome(end, reason).into();
        self.record.reason = reason.map(String::from);
        self.record.ended = at;
        self.record
    }

    fn outcome(&self, end: &End<'_>, reason: Option<&'static str>) -> &'static str {
        if self.record.answered.is_some() {
            // The owner's own clear can confirm what the daemon did not see
            // handed over (a `push` answer's owner, inside its 4 s).
            if matches!(
                end,
                End::Cleared {
                    delivered: true,
                    ..
                }
            ) || self.delivery == Some(Delivery::Delivered)
            {
                return "answered";
            }
            if self.delivery == Some(Delivery::Failed) {
                return "failed";
            }
        }
        if self.record.focused || reason == Some("focus") {
            return "handed_off";
        }
        match (end, reason) {
            (End::Expired(_), _) => "expired",
            (End::AgentGone(_), _) => "agent_gone",
            (End::Shutdown, _) => "shutdown",
            (End::Cleared { .. }, Some("replaced")) => "replaced",
            (End::Cleared { .. }, Some("muted")) => "muted",
            (End::Cleared { .. }, Some("ack")) => "acked",
            (End::Cleared { .. }, Some(word)) if ELSEWHERE.contains(&word) => "elsewhere",
            (End::Cleared { .. }, _) => "cleared",
        }
    }
}

/// What a reader request is about, for counting one read per window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Read {
    /// A document's lines: its kind, the slot, and a card body's card.
    Doc(DocKind, u8, u16),
    /// An image: the slot, the message revision, the image.
    Image(u8, u16, u8),
    /// The model picker on a slot.
    Picker(u8),
}

/// Whether a request for `key` starts a read: the first one, or the first
/// after `window` with none. The stamp moves with every request, so a read
/// the deck keeps asking for (a tail it follows, a retry) is one read
/// however long it lasts.
fn starts(seen: &mut HashMap<Read, Instant>, key: Read, window: Duration, now: Instant) -> bool {
    let fresh = seen
        .get(&key)
        .is_none_or(|at| now.saturating_duration_since(*at) >= window);
    seen.insert(key, now);
    fresh
}

/// The record in memory: what is saved, the cards still up, and what the
/// reader counters need between lines.
#[derive(Debug, Default)]
pub struct Ledger {
    saved: Deliveries,
    /// The card each agent has up, by agent: an agent has one card at a
    /// time, and a card keeps its record when a slot gives it a new number.
    open: BTreeMap<AgentId, Open>,
    /// When each read was last asked for.
    seen: HashMap<Read, Instant>,
    /// The slot of the deck's last line when that line was a `channel`: a
    /// PTT press on the same slot claims it.
    channel: Option<u8>,
    /// Something changed since the last copy into `state.json`.
    changed: bool,
    persisted_at: Option<Instant>,
}

impl Ledger {
    /// The record `state.json` had, which goes on counting. One that was
    /// never started starts now, under this daemon's `version`.
    #[must_use]
    pub fn new(mut saved: Deliveries, version: &str, now: Ts) -> Ledger {
        if saved.since.is_none() {
            saved.since = Some(now);
            saved.version = version.to_string();
        }
        Ledger {
            saved,
            ..Ledger::default()
        }
    }

    /// One step's events. `from` names the source of an agent's cards
    /// (`care::sources::Ledger::word`); it is asked once, when the card is
    /// set. True when a card ended, which the core copies into `state.json`
    /// at once.
    pub fn observe<F>(&mut self, events: &[Event], snapshot: &Snapshot, from: F) -> bool
    where
        F: Fn(&AgentId) -> &'static str,
    {
        let mut ended = false;
        for event in events {
            let data = &event.data;
            let reason = data.get("reason").and_then(serde_json::Value::as_str);
            match event.kind {
                EventKind::CardSet => ended |= self.card_set(event, &from),
                EventKind::CardAnswered => {
                    let source =
                        listed(ANSWER_SOURCES, data["source"].as_str().unwrap_or_default());
                    if let Some(open) = self.card_of(event) {
                        open.record.answered = Some(source.into());
                    }
                }
                EventKind::CardDelivered => {
                    if let Some(open) = self.card_of(event) {
                        open.delivery = Some(Delivery::Delivered);
                    }
                }
                EventKind::CardFailed => {
                    if let Some(open) = self.card_of(event) {
                        open.delivery = Some(Delivery::Failed);
                        open.record.failed = reason.map(|r| listed(FAIL_REASONS, r).into());
                    }
                }
                EventKind::CardSnoozed => {
                    if let Some(open) = self.card_of(event) {
                        open.record.snoozed = true;
                    }
                }
                EventKind::CardDegraded => {
                    if let Some(open) = self.card_of(event) {
                        open.record.degraded = true;
                    }
                }
                EventKind::CardFocused => {
                    if let Some(open) = self.card_of(event) {
                        open.record.focused = true;
                    }
                }
                EventKind::CardExpired => ended |= self.end(event, &End::Expired(reason)),
                EventKind::CardCleared => {
                    let delivered = data["delivered"].as_bool().unwrap_or(false);
                    ended |= self.end(event, &End::Cleared { delivered, reason });
                }
                EventKind::AgentRemoved => ended |= self.end(event, &End::AgentGone(reason)),
                EventKind::OrderDispatched => {
                    let via = listed(ROUTES, data["via"].as_str().unwrap_or_default());
                    *self.saved.orders.dispatched.entry(via.into()).or_default() += 1;
                    self.changed = true;
                }
                EventKind::OrderFailed => {
                    let orders = &mut self.saved.orders;
                    let why = listed(FAILURE_REASONS, reason.unwrap_or_default());
                    *orders.failed.entry(why.into()).or_default() += 1;
                    if let Some(shown) = data["shown"].as_str() {
                        *orders.shown.entry(listed(SHOWN, shown).into()).or_default() += 1;
                    }
                    self.changed = true;
                }
                _ => {}
            }
        }
        // A card reaches the deck when the deck is live and its agent has a
        // slot, which can come after the card: the deck links, a slot frees.
        if snapshot.device.session == SessionState::Live {
            for (agent, open) in &mut self.open {
                if !open.record.reached
                    && snapshot.agents.get(agent).is_some_and(|a| a.slot.is_some())
                {
                    open.record.reached = true;
                }
            }
        }
        ended
    }

    /// A `card.set`. A same-id re-push changes nothing here. A `card.set`
    /// for an agent whose card has not ended, with that card's `created`
    /// stamp, is the same card under the number a slot gave it (§6.5); the
    /// store ends a card before it sets the next (`card.cleared{replaced}`),
    /// so any other one would be a card that went unseen, and it ends as
    /// `replaced` rather than lending the new card its record. True when a
    /// card ended.
    fn card_set<F>(&mut self, event: &Event, from: &F) -> bool
    where
        F: Fn(&AgentId) -> &'static str,
    {
        let data = &event.data;
        if data["repush"].as_bool() == Some(true) {
            return false;
        }
        let (Some(agent), Some(card)) = (
            event_agent(event),
            data["card"]["id"]
                .as_u64()
                .and_then(|id| u16::try_from(id).ok()),
        ) else {
            return false;
        };
        let created = serde_json::from_value::<Ts>(data["card"]["created"].clone()).ok();
        let mut ended = false;
        if let Some(open) = self.open.remove(&agent) {
            if created.is_some() && open.created == created {
                let mut open = open;
                open.record.card = card;
                self.open.insert(agent, open);
                return false;
            }
            let replaced = End::Cleared {
                delivered: false,
                reason: Some("replaced"),
            };
            self.file(open.finish(&replaced, event.ts));
            ended = true;
        }
        let word = |list: &[&'static str], value: &serde_json::Value| -> String {
            listed(list, value.as_str().unwrap_or_default()).into()
        };
        let failure = (data["host"].as_str() == Some(deck_model::HOST_FAILURE))
            .then(|| listed(FAILURE_REASONS, data["reason"].as_str().unwrap_or_default()).into());
        let source = from(&agent);
        let record = Record {
            card,
            kind: word(KINDS, &data["card"]["type"]),
            origin: word(ORIGINS, &data["card"]["origin"]["kind"]),
            deliver: word(DELIVERS, &data["card"]["deliver"]),
            from: FROM
                .iter()
                .copied()
                .find(|w| *w == source)
                .unwrap_or("unknown")
                .into(),
            failure,
            set: event.ts,
            agent: agent.clone(),
            ..Record::default()
        };
        self.open.insert(
            agent,
            Open {
                record,
                delivery: None,
                created,
            },
        );
        ended
    }

    /// The card a card event is about, while it is up.
    fn card_of(&mut self, event: &Event) -> Option<&mut Open> {
        let agent = event_agent(event)?;
        let card = event.data["card"].as_u64()?;
        self.open
            .get_mut(&agent)
            .filter(|open| u64::from(open.record.card) == card)
    }

    /// End the card `event` is about (an agent's removal ends whichever it
    /// has up) and file its record.
    fn end(&mut self, event: &Event, end: &End<'_>) -> bool {
        let Some(agent) = event_agent(event) else {
            return false;
        };
        let card = event.data["card"].as_u64();
        let Some(open) = self.open.get(&agent) else {
            return false;
        };
        if card.is_some_and(|card| card != u64::from(open.record.card)) {
            return false;
        }
        let Some(open) = self.open.remove(&agent) else {
            return false;
        };
        self.file(open.finish(end, event.ts));
        true
    }

    /// A finished record into the totals and the ring.
    fn file(&mut self, record: Record) {
        let saved = &mut self.saved;
        if record.failure.is_some() {
            saved.failure_cards.add(&record);
        } else {
            if record.from != "daemon" {
                saved.cards.add(&record);
            }
            saved
                .by_origin
                .entry(record.origin.clone())
                .or_default()
                .add(&record);
            saved
                .by_from
                .entry(record.from.clone())
                .or_default()
                .add(&record);
        }
        saved.recent.push_back(record);
        while saved.recent.len() > RECENT_MAX {
            saved.recent.pop_front();
        }
        self.changed = true;
    }

    /// One line from the deck, before anything else handles it.
    pub fn device_line(&mut self, line: &DeviceLine, now: Instant) {
        self.seen
            .retain(|_, at| now.saturating_duration_since(*at) < READ_WINDOW);
        let before = self.saved.reader;
        let (reader, seen) = (&mut self.saved.reader, &mut self.seen);
        let mut channel = None;
        let person = match *line {
            // The link's own traffic: nobody's hand, and nothing that comes
            // between a `channel` and the press it belongs to.
            DeviceLine::Hello { .. } | DeviceLine::Up | DeviceLine::Err { .. } => return,
            // The deck goes home by itself when idle, with this line.
            DeviceLine::Face { .. } => false,
            DeviceLine::Channel { slot, .. } => {
                reader.channels += 1;
                channel = Some(slot);
                true
            }
            DeviceLine::Ptt { act, slot, .. } => {
                if act == 1 && self.channel == Some(slot) {
                    reader.channels = reader.channels.saturating_sub(1);
                    reader.channels_by_ptt += 1;
                }
                true
            }
            DeviceLine::DocLines { slot, doc, id, .. } => {
                reader.lines += 1;
                let (card, counter) = match doc {
                    DocKind::Reply => (0, &mut reader.history),
                    DocKind::Workflow => (0, &mut reader.plans),
                    DocKind::Card => (id, &mut reader.card_bodies),
                    DocKind::Tr => (0, &mut reader.transcripts),
                };
                if starts(seen, Read::Doc(doc, slot, card), READ_WINDOW, now) {
                    *counter += 1;
                }
                // Also asked for by itself: the tail it follows, a re-ask.
                false
            }
            DeviceLine::Tool { .. } => {
                reader.lines += 1;
                reader.tool_rows += 1;
                true
            }
            DeviceLine::Image {
                slot,
                rev,
                index,
                offset,
            } => {
                reader.lines += 1;
                let opened =
                    offset == 0 && starts(seen, Read::Image(slot, rev, index), IMAGE_WINDOW, now);
                reader.images += u64::from(opened);
                opened
            }
            DeviceLine::Session {
                slot,
                field,
                page,
                choice,
                ..
            } => {
                reader.lines += 1;
                if choice != 255 {
                    reader.picker_choices += 1;
                    true
                } else if field == 0
                    && page == 0
                    && starts(seen, Read::Picker(slot), PICKER_WINDOW, now)
                {
                    reader.pickers += 1;
                    true
                } else {
                    false
                }
            }
            DeviceLine::Key { .. }
            | DeviceLine::Answer { .. }
            | DeviceLine::Snooze { .. }
            | DeviceLine::Macro { .. }
            | DeviceLine::MacroKey { .. }
            | DeviceLine::WsGo(_)
            | DeviceLine::WsWin(_)
            | DeviceLine::Refresh { .. }
            | DeviceLine::Set { .. }
            | DeviceLine::Faces { .. }
            | DeviceLine::Language { .. } => true,
        };
        self.channel = channel;
        self.changed |= self.saved.reader != before;
        if person {
            for open in self.open.values_mut() {
                if open.record.reached {
                    open.record.deck_used = true;
                }
            }
        }
    }

    /// What the settle window did since the last look.
    pub fn add_asks(&mut self, asks: Asks) {
        if asks == Asks::default() {
            return;
        }
        let saved = &mut self.saved.asks;
        saved.waited += asks.waited;
        saved.settled += asks.settled;
        saved.prompted += asks.prompted;
        saved.passed += asks.passed;
        self.changed = true;
    }

    /// The daemon is stopping: every card still up ends as `shutdown`.
    pub fn shutdown(&mut self, at: Ts) {
        for (_, open) in std::mem::take(&mut self.open) {
            self.file(open.finish(&End::Shutdown, at));
        }
    }

    /// Whether the record changed and its last copy into `state.json` is
    /// [`PERSIST_EVERY`] old.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        self.changed
            && self
                .persisted_at
                .is_none_or(|at| now.saturating_duration_since(at) >= PERSIST_EVERY)
    }

    /// The record to copy into `state.json` now.
    pub fn persisted(&mut self, now: Instant) -> Deliveries {
        self.changed = false;
        self.persisted_at = Some(now);
        self.saved.clone()
    }

    /// `metrics.get`'s `deliveries`: the record, and how many cards are up.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(&self.saved).unwrap_or_default();
        value["open"] = self.open.len().into();
        value
    }

    /// The record in brief, for `status.json`.
    #[must_use]
    pub fn brief(&self) -> Brief {
        let cards = &self.saved.cards;
        let answers = |source: &str| cards.answered_on.get(source).copied().unwrap_or(0);
        Brief {
            since: self.saved.since,
            cards: cards.total,
            answered: cards.outcomes.get("answered").copied().unwrap_or(0),
            answers_on_deck: answers("deck") + answers("ptt"),
            failure_cards: self.saved.failure_cards.total,
            reader: self.saved.reader,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testkit::{card, register};
    use deck_model::{
        AdapterKind, AgentRegister, AgentState, AgentUpdate, AnswerCap, CardClear, CardId, CardSet,
        Choice, Command, Deliver, HoldId, KeyId, Kind, LinkEvent, OpOrder, OrderCap, OrderOutcome,
        OrderSource, Origin, OriginKind, Principal, PttCtx, Request, SessionId, Store, StoreConfig,
    };
    use std::collections::BTreeSet;

    const SID: u32 = 7;
    /// The session of the connected app that owns the test agents.
    const APP: SessionId = SessionId(1);
    /// The Claude adapter's internal session.
    const CLAUDE: SessionId = SessionId(50);
    const T0: Ts = Ts(1_789_760_523_000);

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn claude() -> Principal {
        Principal::Adapter(AdapterKind::Claude)
    }

    /// A store driven the way the core drives it: every step's events go
    /// to the ledger, and every deck line reaches the ledger first
    /// (`Core::on_device_line`).
    struct Rig {
        store: Store,
        ledger: Ledger,
        now: Instant,
        /// What the core would call every card's source.
        from: &'static str,
        /// The last step ended a card.
        ended: bool,
    }

    impl Rig {
        fn new() -> Rig {
            let now = Instant::now();
            Rig {
                store: Store::new(StoreConfig::new(now, T0, SID)),
                ledger: Ledger::new(Deliveries::default(), "0.13.0", T0),
                now,
                from: "app",
                ended: false,
            }
        }

        fn observe(&mut self, events: &[Event]) {
            let from = self.from;
            self.ended = self
                .ledger
                .observe(events, self.store.snapshot(), move |_| from);
        }

        fn apply(&mut self, cmd: Command) {
            let (events, _) = self.store.apply(cmd, self.now);
            self.observe(&events);
        }

        fn call_as(
            &mut self,
            principal: Principal,
            session: SessionId,
            req: Request,
        ) -> serde_json::Value {
            let (result, events, _) = self.store.call(principal, session, req, self.now);
            self.observe(&events);
            result.expect("the store took it")
        }

        /// A call by the app that owns the test agents.
        fn call(&mut self, req: Request) -> serde_json::Value {
            self.call_as(Principal::Uid(501), APP, req)
        }

        fn line(&mut self, line: DeviceLine) {
            self.ledger.device_line(&line, self.now);
            self.apply(Command::Device(line));
        }

        fn hello() -> DeviceLine {
            DeviceLine::Hello {
                v: 1,
                fw: "0.13.0".into(),
                sid: SID,
                boot: false,
                rx: deck_model::LINE_MAX_U16,
                agents: None,
                lang: Some(0),
                lang_rev: Some(1),
                faces: Vec::new(),
            }
        }

        /// A deck that said hello and went live.
        fn link(&mut self) {
            self.apply(Command::Link(LinkEvent::Linked));
            self.line(Rig::hello());
            self.apply(Command::Link(LinkEvent::Live { sid: SID }));
        }

        fn register_as(
            &mut self,
            principal: Principal,
            session: SessionId,
            r: AgentRegister,
        ) -> AgentId {
            let res = self.call_as(principal.clone(), session, Request::AgentRegister(r));
            let agent = AgentId(res["agent"].as_str().expect("an agent").into());
            self.call_as(principal, session, working(&agent));
            agent
        }

        /// A poll agent of the app's, at work, on the deck when a slot is free.
        fn agent(&mut self, name: &str) -> AgentId {
            self.register_as(
                Principal::Uid(501),
                APP,
                register(name, Some(&format!("k:{name}"))),
            )
        }

        /// A Claude Code session, as the hooks register one.
        fn claude_agent(&mut self) -> AgentId {
            let mut r = register("api", Some("claude:api"));
            r.kind = Kind::Claude;
            r.caps.answer = AnswerCap::Hold;
            r.caps.order = OrderCap::Inject;
            self.register_as(claude(), CLAUDE, r)
        }

        fn set(&mut self, card: CardSet) -> u16 {
            let res = self.call(Request::CardSet(card));
            u16::try_from(res["card"]["id"].as_u64().expect("a card id")).unwrap()
        }

        /// A two-choice poll question.
        fn ask(&mut self, agent: &AgentId, text: &str) -> u16 {
            self.set(card(agent, text))
        }

        /// A permission ask held by the Claude adapter, as a hook parks one.
        fn hold(&mut self, agent: &AgentId) -> u16 {
            let mut ask = card(agent, "Bash: psql");
            ask.deliver = Deliver::Hold;
            ask.origin = Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            };
            let (result, events, _) = self.store.hold(&claude(), CLAUDE, ask, HoldId(1), self.now);
            self.observe(&events);
            result.expect("held").0
        }

        fn clear_as(
            &mut self,
            principal: Principal,
            session: SessionId,
            agent: &AgentId,
            card: u16,
            delivered: bool,
            reason: Option<&str>,
        ) {
            let clear = CardClear {
                agent: agent.clone(),
                card: CardId(card),
                delivered,
                reason: reason.map(String::from),
            };
            self.call_as(principal, session, Request::CardClear(clear));
        }

        fn later(&mut self, by: Duration) {
            self.now += by;
            self.apply(Command::Tick(self.now));
        }

        fn open(&self, agent: &AgentId) -> &Record {
            &self.ledger.open[agent].record
        }

        fn last(&self) -> &Record {
            self.ledger.saved.recent.back().expect("a card ended")
        }
    }

    fn working(agent: &AgentId) -> Request {
        state(agent, AgentState::Working)
    }

    fn state(agent: &AgentId, state: AgentState) -> Request {
        Request::AgentUpdate(AgentUpdate {
            agent: agent.clone(),
            state: Some(state),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None,
            task: None,
            subagents: None,
        })
    }

    fn channel(slot: u8) -> DeviceLine {
        DeviceLine::Channel { slot, seq: 1 }
    }

    fn ptt(act: u8, slot: u8) -> DeviceLine {
        DeviceLine::Ptt {
            act,
            slot,
            ctx: PttCtx::Talk,
            card: None,
            abort: false,
            press: Some(1),
        }
    }

    fn key() -> DeviceLine {
        DeviceLine::Key {
            k: KeyId::Right,
            act: 1,
            shift: false,
            f: 1,
        }
    }

    /// Everything the record lets out: what `state.json` keeps, what
    /// `metrics.get` serves and what `status.json` carries.
    fn everything(ledger: &mut Ledger) -> String {
        let saved = serde_json::to_string(&ledger.persisted(Instant::now())).unwrap();
        let brief = serde_json::to_string(&ledger.brief()).unwrap();
        format!("{saved}{}{brief}", ledger.json())
    }

    /// The whole life of a card answered on the deck, and not one word of
    /// it: not the card's text, the label chosen, the agent's name, key or
    /// working directory, nor the reply beside it.
    #[test]
    fn a_card_answered_on_the_deck_is_recorded_without_its_words() {
        let mut rig = Rig::new();
        rig.link();
        let mut reg = register("SECRET-NAME", Some("SECRET-KEY"));
        reg.cwd = Some("/Users/SECRET-CWD".into());
        let agent = rig.register_as(Principal::Uid(501), APP, reg);
        rig.call(Request::Reply(deck_model::ReplySet {
            agent: agent.clone(),
            text: "SECRET-REPLY".into(),
            message: None,
            speak: false,
            announce: None,
            lane: None,
        }));
        let mut ask = card(&agent, "SECRET-CARD");
        ask.choices[0] = Choice {
            label: "SECRET-LABEL".into(),
            detail: Some("SECRET-DETAIL".into()),
        };
        let id = rig.set(ask);
        rig.line(DeviceLine::Answer {
            slot: 1,
            card: id,
            choice: 0,
        });
        assert!(!rig.ended, "answered, not yet handed over");
        rig.apply(Command::Delivered {
            agent: agent.clone(),
            card: CardId(id),
            ok: true,
        });
        assert!(rig.ended, "the card ended");
        let record = rig.last().clone();
        assert_eq!(
            (
                record.outcome.as_str(),
                record.answered.as_deref(),
                record.reason.as_deref()
            ),
            ("answered", Some("deck"), None)
        );
        assert_eq!(
            (record.card, record.kind.as_str(), record.origin.as_str()),
            (id, "q", "custom")
        );
        assert_eq!(
            (record.deliver.as_str(), record.from.as_str()),
            ("poll", "app")
        );
        assert!(record.reached && record.deck_used, "{record:?}");
        assert!(record.ended >= record.set);
        let saved = &rig.ledger.saved;
        assert_eq!(saved.cards.total, 1);
        assert_eq!(saved.cards.outcomes["answered"], 1);
        assert_eq!(saved.cards.answered_on["deck"], 1);
        assert_eq!((saved.cards.reached, saved.cards.deck_used), (1, 1));
        assert_eq!(saved.by_origin["custom"].total, 1);
        assert_eq!(saved.by_from["app"].total, 1);
        assert_eq!((saved.since, saved.version.as_str()), (Some(T0), "0.13.0"));
        let out = everything(&mut rig.ledger);
        assert!(!out.contains("SECRET"), "{out}");
    }

    /// A card the session decided itself is `elsewhere`; the reason an app
    /// writes on its own clear is its own text, and is kept as `other`.
    #[test]
    fn decided_in_the_terminal_is_elsewhere_and_an_apps_reason_is_other() {
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        let first = rig.ask(&agent, "first");
        rig.clear_as(
            Principal::Uid(501),
            APP,
            &agent,
            first,
            false,
            Some("tool_ran"),
        );
        assert!(rig.ended);
        assert_eq!(
            (rig.last().outcome.as_str(), rig.last().reason.as_deref()),
            ("elsewhere", Some("tool_ran"))
        );
        let second = rig.ask(&agent, "second");
        rig.clear_as(
            Principal::Uid(501),
            APP,
            &agent,
            second,
            true,
            Some("SECRET-REASON"),
        );
        assert_eq!(
            (rig.last().outcome.as_str(), rig.last().reason.as_deref()),
            ("cleared", Some("other")),
            "an owner's word that says delivered, with nothing answered, is its own"
        );
        assert_eq!(
            rig.ledger.saved.cards.outcomes,
            BTreeMap::from([("cleared".into(), 1), ("elsewhere".into(), 1)])
        );
        let out = everything(&mut rig.ledger);
        assert!(!out.contains("SECRET"), "{out}");
    }

    /// A card reaches the deck when the deck is live and its agent has a
    /// slot, whichever comes last; a card set off the deck keeps its record
    /// under the number a slot gives it.
    #[test]
    fn reached_needs_a_live_deck_and_a_slot_and_comes_true_when_the_deck_links() {
        let mut rig = Rig::new();
        let agent = rig.agent("bot");
        rig.ask(&agent, "before the deck");
        assert!(!rig.open(&agent).reached, "no deck yet");
        rig.link();
        assert!(rig.open(&agent).reached, "the deck linked with the card up");

        // A paused source's agent holds no slot: its card waits off the
        // deck, and reaches it with the slot it is given back.
        let session = SessionId(2);
        let muted = rig.register_as(
            Principal::Uid(501),
            session,
            register("muted", Some("k:muted")),
        );
        rig.apply(Command::Mute {
            sessions: BTreeSet::from([session]),
        });
        let offdeck = card(&muted, "off the deck");
        rig.call_as(Principal::Uid(501), session, Request::CardSet(offdeck));
        assert!(rig.store.snapshot().agents[&muted].slot.is_none());
        assert!(!rig.open(&muted).reached, "no slot");
        rig.apply(Command::Mute {
            sessions: BTreeSet::new(),
        });
        let numbered = rig.store.snapshot().agents[&muted]
            .card
            .as_ref()
            .map(|c| c.id.0)
            .expect("its card");
        assert!(rig.open(&muted).reached, "a slot, on a live deck");
        assert_eq!(rig.open(&muted).card, numbered, "the slot's number");
        assert!(
            rig.ledger.saved.recent.is_empty(),
            "the same card, not a new one"
        );
        let slot = rig.store.snapshot().agents[&muted].slot.expect("a slot").0;
        rig.line(DeviceLine::Answer {
            slot,
            card: numbered,
            choice: 1,
        });
        rig.apply(Command::Delivered {
            agent: muted.clone(),
            card: CardId(numbered),
            ok: true,
        });
        assert_eq!(rig.last().outcome, "answered", "{:?}", rig.last());
    }

    /// The store ends a card before it sets the next. A `card.set` over a
    /// card the record still has up, which is not that card renumbered,
    /// ends it as replaced rather than taking its record over.
    #[test]
    fn a_card_set_over_one_still_up_ends_it_as_replaced() {
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        let (_, events, _) = rig.store.call(
            Principal::Uid(501),
            APP,
            Request::CardSet(card(&agent, "first")),
            rig.now,
        );
        rig.observe(&events);
        let mut unseen = events
            .iter()
            .find(|e| e.kind == EventKind::CardSet)
            .expect("card.set")
            .clone();
        unseen.data["card"]["id"] = 9.into();
        unseen.data["card"]["created"] = serde_json::to_value(T0.plus(secs(5))).unwrap();
        rig.observe(&[unseen]);
        assert!(rig.ended);
        assert_eq!(
            (rig.last().card, rig.last().outcome.as_str()),
            (1, "replaced")
        );
        assert_eq!(rig.open(&agent).card, 9);
    }

    /// Only an input a person makes marks a card that reached the deck as
    /// used: the deck's heartbeat, the face it goes home to and the lines it
    /// asks for by itself do not, and a card that never reached the deck is
    /// not used by any of it.
    #[test]
    fn deck_input_marks_a_reached_card_used_and_a_heartbeat_does_not() {
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        rig.ask(&agent, "on the deck");
        let session = SessionId(2);
        let muted = rig.register_as(
            Principal::Uid(501),
            session,
            register("muted", Some("k:muted")),
        );
        rig.apply(Command::Mute {
            sessions: BTreeSet::from([session]),
        });
        rig.call_as(
            Principal::Uid(501),
            session,
            Request::CardSet(card(&muted, "off")),
        );
        assert!(rig.open(&agent).reached && !rig.open(&muted).reached);
        for line in [
            Rig::hello(),
            DeviceLine::Face { f: 1 },
            DeviceLine::DocLines {
                slot: 1,
                doc: DocKind::Reply,
                id: 1,
                first: 0,
            },
            DeviceLine::Image {
                slot: 1,
                rev: 1,
                index: 0,
                offset: 512,
            },
        ] {
            rig.line(line.clone());
            assert!(!rig.open(&agent).deck_used, "{line:?}");
        }
        rig.line(key());
        assert!(rig.open(&agent).deck_used, "a press");
        assert!(!rig.open(&muted).deck_used, "never on the deck");
    }

    /// The firmware sends `channel` just before `ptt` when a press moves
    /// the channel: that one is the press's, counted apart from a channel
    /// opened on purpose.
    #[test]
    fn a_channel_a_ptt_press_caused_is_counted_apart() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let mut feed = |lines: &[DeviceLine]| {
            for line in lines {
                ledger.device_line(line, t0);
            }
            (
                ledger.saved.reader.channels,
                ledger.saved.reader.channels_by_ptt,
            )
        };
        assert_eq!(feed(&[channel(1), ptt(1, 1), ptt(0, 1)]), (0, 1));
        assert_eq!(feed(&[channel(2), key()]), (1, 1), "opened, then a press");
        assert_eq!(
            feed(&[ptt(1, 2), ptt(0, 2)]),
            (1, 1),
            "the channel was already open"
        );
        assert_eq!(
            feed(&[channel(3), Rig::hello(), ptt(1, 3)]),
            (1, 2),
            "the deck's heartbeat between them is not an input"
        );
        assert_eq!(
            feed(&[channel(4), ptt(1, 5)]),
            (2, 2),
            "a press on another slot"
        );
    }

    /// A history, plan, card body or transcript read is one per slot until
    /// the window passes with no request for it, however long the deck
    /// keeps asking; every request is a line.
    #[test]
    fn reads_are_counted_once_per_window_per_slot() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let doc = |slot, doc, id| DeviceLine::DocLines {
            slot,
            doc,
            id,
            first: 0,
        };
        for s in 0..5 {
            ledger.device_line(&doc(1, DocKind::Reply, 4), t0 + secs(s));
        }
        assert_eq!(
            (ledger.saved.reader.history, ledger.saved.reader.lines),
            (1, 5)
        );
        for line in [
            doc(2, DocKind::Reply, 9),
            doc(1, DocKind::Card, 7),
            doc(1, DocKind::Card, 7),
            doc(1, DocKind::Card, 8),
            doc(1, DocKind::Workflow, 4),
            doc(1, DocKind::Tr, 1),
        ] {
            ledger.device_line(&line, t0 + secs(5));
        }
        let reader = ledger.saved.reader;
        assert_eq!(
            (
                reader.history,
                reader.card_bodies,
                reader.plans,
                reader.transcripts
            ),
            (2, 2, 1, 1)
        );
        // The tail the deck follows is one read for as long as it lasts…
        for s in [50, 100, 150] {
            ledger.device_line(&doc(1, DocKind::Reply, 4), t0 + secs(s));
        }
        assert_eq!(ledger.saved.reader.history, 2);
        // …and the first request after a quiet window is the next one.
        ledger.device_line(&doc(1, DocKind::Reply, 4), t0 + secs(150) + READ_WINDOW);
        assert_eq!(
            (ledger.saved.reader.history, ledger.saved.reader.lines),
            (3, 15)
        );
    }

    /// The deck asks again for an image's first chunk every six seconds,
    /// three times at most: one open. The rest of the image is not an open
    /// either; the next image is, and so is this one once it was let go.
    #[test]
    fn an_image_retry_is_not_a_second_open() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let image = |index, offset| DeviceLine::Image {
            slot: 1,
            rev: 3,
            index,
            offset,
        };
        for s in [0, 6, 12] {
            ledger.device_line(&image(0, 0), t0 + secs(s));
        }
        ledger.device_line(&image(0, 512), t0 + secs(13));
        assert_eq!(
            (ledger.saved.reader.images, ledger.saved.reader.lines),
            (1, 4)
        );
        ledger.device_line(&image(1, 0), t0 + secs(14));
        assert_eq!(ledger.saved.reader.images, 2);
        ledger.device_line(&image(0, 0), t0 + secs(12) + IMAGE_WINDOW);
        assert_eq!(ledger.saved.reader.images, 3);
    }

    /// The model picker: its first page is an open, asked for again on the
    /// way back from a field; a field's page is neither; a choice counts.
    #[test]
    fn the_picker_open_and_a_choice_are_counted_apart() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let session = |field, choice| DeviceLine::Session {
            slot: 1,
            field,
            page: 0,
            rev: 2,
            choice,
        };
        ledger.device_line(&session(0, 255), t0);
        ledger.device_line(&session(1, 255), t0 + secs(1));
        ledger.device_line(&session(0, 255), t0 + secs(2));
        ledger.device_line(&session(1, 3), t0 + secs(3));
        let reader = ledger.saved.reader;
        assert_eq!(
            (reader.pickers, reader.picker_choices, reader.lines),
            (1, 1, 4)
        );
        ledger.device_line(&session(0, 255), t0 + secs(2) + PICKER_WINDOW);
        assert_eq!(ledger.saved.reader.pickers, 2);
    }

    /// `recent` keeps the newest hundred cards; the totals count them all.
    #[test]
    fn the_ring_keeps_the_newest_hundred_and_totals_keep_counting() {
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        for n in 0..=RECENT_MAX + 5 {
            rig.ask(&agent, &format!("card {n}"));
        }
        let saved = &rig.ledger.saved;
        assert_eq!(saved.recent.len(), RECENT_MAX);
        assert_eq!(
            (
                saved.recent.front().map(|r| r.card),
                saved.recent.back().map(|r| r.card)
            ),
            (Some(6), Some(105)),
            "each card replaced the one before it"
        );
        assert_eq!(saved.cards.total, 105);
        assert_eq!(saved.cards.outcomes["replaced"], 105);
        assert_eq!(rig.ledger.json()["open"], 1);
    }

    /// A card the daemon put up to show a failure is a failure card, kept
    /// apart from the cards the sources asked: with the reason word, when
    /// it is the daemon's, and `other` for an owner's own.
    #[test]
    fn host_made_cards_count_as_failure_cards_only() {
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        let dispatched = rig.call_as(
            Principal::Uid(501),
            SessionId(9),
            Request::OpOrder(OpOrder {
                agent: agent.clone(),
                text: "SECRET-ORDER".into(),
                ctx: "talk".into(),
                source: OrderSource::Ptt,
                utterance: None,
            }),
        );
        let order = dispatched["order"]
            .as_str()
            .expect("an order id")
            .to_string();
        rig.call(Request::OrderOutcome(OrderOutcome {
            agent: agent.clone(),
            order,
            ok: false,
            reason: Some("secret_app_word".into()),
        }));
        assert!(
            rig.ledger.open[&agent].record.failure.is_some(),
            "a failure card"
        );
        // It goes like any Ready card: at the agent's next state change.
        rig.call(state(&agent, AgentState::Idle));
        assert!(rig.ended);
        let record = rig.last();
        assert_eq!(
            (
                record.failure.as_deref(),
                record.outcome.as_str(),
                record.kind.as_str()
            ),
            (Some("other"), "expired", "r")
        );
        // The hand-off's own word, and a Ready card's time running out.
        let (_, events, _) = rig.store.fail_on_deck(
            Some(&agent),
            deck_model::HandoffFailure::NoApp.reason(),
            deck_model::HandoffFailure::NoApp.line(),
            rig.now,
        );
        rig.observe(&events);
        rig.later(secs(901));
        assert_eq!(
            (rig.last().failure.as_deref(), rig.last().reason.as_deref()),
            (Some("no_app"), Some("ttl"))
        );
        let saved = &rig.ledger.saved;
        assert_eq!(saved.failure_cards.total, 2);
        assert_eq!(saved.failure_cards.outcomes["expired"], 2);
        assert_eq!(saved.cards, Totals::default(), "no source asked anything");
        assert!(saved.by_origin.is_empty() && saved.by_from.is_empty());
        assert_eq!(saved.orders.dispatched["poll"], 1);
        assert_eq!(saved.orders.failed["other"], 1);
        assert_eq!(saved.orders.shown["card"], 1);
        let out = everything(&mut rig.ledger);
        assert!(
            !out.contains("SECRET") && !out.contains("secret_app_word"),
            "{out}"
        );
    }

    /// Orders by route, and why they failed in the daemon's own words.
    #[test]
    fn orders_are_counted_by_route_and_by_reason_word() {
        let mut rig = Rig::new();
        rig.link();
        rig.from = "claude";
        let agent = rig.claude_agent();
        rig.call_as(
            Principal::Device,
            SessionId(9),
            Request::OpOrder(OpOrder {
                agent: agent.clone(),
                text: "deploy".into(),
                ctx: "talk".into(),
                source: OrderSource::Ptt,
                utterance: None,
            }),
        );
        rig.apply(Command::OrderResult {
            principal: Principal::Device,
            agent: agent.clone(),
            text: "deploy".into(),
            ok: false,
            reason: Some("no_route".into()),
            order: None,
        });
        let orders = &rig.ledger.saved.orders;
        assert_eq!(orders.dispatched, BTreeMap::from([("inject".into(), 1)]));
        assert_eq!(orders.failed, BTreeMap::from([("no_route".into(), 1)]));
        assert_eq!(orders.shown, BTreeMap::from([("card".into(), 1)]));
        assert!(
            rig.ledger.due(rig.now),
            "counted, so copied at the next minute"
        );
    }

    /// A held ask that ran out is degraded to `Open app`, and then decided
    /// in the session; one snoozed on the deck and then handed off is
    /// `handed_off`, with what happened on the way.
    #[test]
    fn held_cards_that_ran_out_are_degraded_then_elsewhere() {
        let mut rig = Rig::new();
        rig.link();
        rig.from = "claude";
        let agent = rig.claude_agent();
        let first = rig.hold(&agent);
        rig.later(secs(121));
        assert!(rig.open(&agent).degraded, "the hold budget ran out");
        rig.clear_as(claude(), CLAUDE, &agent, first, false, Some("tool_ran"));
        let record = rig.last().clone();
        assert_eq!(
            (
                record.outcome.as_str(),
                record.origin.as_str(),
                record.deliver.as_str()
            ),
            ("elsewhere", "permission", "hold")
        );
        assert!(record.degraded && !record.snoozed && !record.deck_used);

        let second = rig.hold(&agent);
        rig.line(DeviceLine::Snooze {
            slot: 1,
            card: second,
        });
        rig.line(DeviceLine::Answer {
            slot: 1,
            card: second,
            choice: 0,
        });
        assert!(rig.open(&agent).focused, "Open app was pressed");
        rig.clear_as(claude(), CLAUDE, &agent, second, false, Some("turn_ended"));
        let record = rig.last().clone();
        assert_eq!(
            (record.outcome.as_str(), record.answered),
            ("handed_off", None)
        );
        assert!(record.snoozed && record.degraded && record.deck_used);
        let permission = &rig.ledger.saved.by_origin["permission"];
        assert_eq!(
            (permission.total, permission.degraded, permission.snoozed),
            (2, 2, 1)
        );
        assert_eq!(rig.ledger.saved.by_from["claude"].total, 2);
    }

    /// An agent that goes takes its card with it; an app's own reason for
    /// letting it go is its own text, kept as `other`.
    #[test]
    fn agent_removed_closes_its_records() {
        let mut rig = Rig::new();
        rig.link();
        for (name, reason, word) in [
            ("one", None, "released"),
            ("two", Some("SECRET-WHY"), "other"),
            // The quiet service's own release (`crate::dormancy`).
            ("three", Some(crate::dormancy::DORMANT), "dormant"),
        ] {
            let agent = rig.agent(name);
            rig.ask(&agent, "up");
            rig.call(Request::AgentRelease {
                agent,
                reason: reason.map(String::from),
            });
            assert!(rig.ended);
            assert_eq!(
                (rig.last().outcome.as_str(), rig.last().reason.as_deref()),
                ("agent_gone", Some(word))
            );
        }
        assert!(rig.ledger.open.is_empty());
        let out = everything(&mut rig.ledger);
        assert!(!out.contains("SECRET"), "{out}");
    }

    /// At shutdown every card still up ends as `shutdown`, an answer still
    /// on its way among them.
    #[test]
    fn shutdown_closes_open_records() {
        let mut rig = Rig::new();
        rig.link();
        let one = rig.agent("one");
        let two = rig.agent("two");
        rig.ask(&one, "up");
        let answering = rig.ask(&two, "answered");
        let slot = rig.store.snapshot().agents[&two].slot.expect("a slot").0;
        rig.line(DeviceLine::Answer {
            slot,
            card: answering,
            choice: 0,
        });
        rig.apply(Command::Shutdown);
        let at = rig.store.ts(rig.now);
        rig.ledger.shutdown(at);
        let saved = &rig.ledger.saved;
        assert_eq!(saved.cards.outcomes["shutdown"], 2);
        assert!(saved.recent.iter().all(|r| r.ended == at));
        assert_eq!(saved.cards.answered_on["deck"], 1);
        assert_eq!(rig.ledger.json()["open"], 0);
        assert!(rig.ledger.due(rig.now));
    }

    /// A record is copied into `state.json` at once when a card ends (the
    /// core does it on `observe`'s word), and otherwise at most once a
    /// minute.
    #[test]
    fn reader_counts_are_copied_at_most_once_a_minute() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        assert!(!ledger.due(t0), "nothing to copy");
        ledger.device_line(&channel(1), t0);
        assert!(ledger.due(t0), "the first change is copied at once");
        assert_eq!(ledger.persisted(t0).reader.channels, 1);
        ledger.device_line(&channel(2), t0 + secs(1));
        assert!(!ledger.due(t0 + secs(1)));
        assert!(ledger.due(t0 + PERSIST_EVERY));
        // The deck's own traffic changes nothing.
        let _ = ledger.persisted(t0 + PERSIST_EVERY);
        ledger.device_line(&Rig::hello(), t0 + secs(200));
        ledger.device_line(&DeviceLine::Up, t0 + secs(200));
        assert!(!ledger.due(t0 + secs(200)));
    }

    /// `state.json` keeps the record whole and loads it back; a file from
    /// before the record loads with none, and one this build cannot read
    /// starts the record again and keeps the rest of the file.
    #[test]
    fn the_record_round_trips_and_an_older_state_json_loads() {
        use crate::state::{Loaded, State};
        let dir = std::env::temp_dir().join(format!("deckd-deliveries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let mut rig = Rig::new();
        rig.link();
        let agent = rig.agent("bot");
        let id = rig.ask(&agent, "up");
        rig.clear_as(Principal::Uid(501), APP, &agent, id, false, Some("idle"));
        rig.line(channel(1));
        let mut state = State::default();
        state.card_ids.insert(1, 5);
        state.deliveries = rig.ledger.persisted(rig.now);
        state.save(&path).unwrap();
        let (back, loaded) = State::load(&path);
        assert_eq!(loaded, Loaded::Read);
        assert_eq!(back.deliveries, state.deliveries);
        assert_eq!(back.deliveries.recent.len(), 1);
        // The next daemon goes on counting from there, under the first
        // daemon's `since` and `version`.
        let next = Ledger::new(back.deliveries, "0.14.0", T0.plus(secs(3600)));
        assert_eq!(
            (
                next.saved.since,
                next.saved.version.as_str(),
                next.saved.cards.total
            ),
            (Some(T0), "0.13.0", 1)
        );
        for text in [
            r#"{"card_ids":{"1":5}}"#,
            r#"{"card_ids":{"1":5},"deliveries":{"cards":"many","recent":7}}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            let (back, loaded) = State::load(&path);
            assert_eq!(loaded, Loaded::Read, "{text}");
            assert_eq!(
                (back.card_ids[&1], back.deliveries),
                (5, Deliveries::default())
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
