//! Agents, cards, replies, answers and orders (§4.13 shapes). Every field is
//! *state* or *content*; `redacted` marks an object whose content fields were
//! nulled for the reader's scope (§4.3).
use crate::{AgentId, AgentState, CardId, CardKind, Kind, Principal, SessionId, Slot, Ts};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// `session_name` / `name`: state.
    SessionName,
    /// Derived from the cwd: state.
    Cwd,
    /// The first 64 B of the operator's prompt: content.
    Prompt,
    /// A tool input head: content.
    Tool,
    /// Set through `agent.update`: state.
    Api,
}

impl TitleSource {
    pub const fn is_content(self) -> bool {
        matches!(self, TitleSource::Prompt | TitleSource::Tool)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Hook,
    Api,
    Mcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PidKind {
    Cli,
    Shell,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnswerCap {
    /// Adapter-only.
    Hold,
    Push,
    Poll,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OrderCap {
    /// Adapter-only.
    Inject,
    Push,
    Poll,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Caps {
    pub answer: AnswerCap,
    pub order: OrderCap,
    /// §4.15 registers with `caps:{answer,order}` alone: an agent that does
    /// not say it can be focused cannot be.
    #[serde(default)]
    pub focus: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            answer: AnswerCap::None,
            order: OrderCap::None,
            focus: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Owner {
    pub principal: Principal,
    pub session: SessionId,
    pub lease_expires: Option<Ts>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Terminal {
    pub program: Option<String>,
    pub tty: Option<String>,
    pub tmux_pane: Option<String>,
    pub iterm_session: Option<String>,
    pub term_session: Option<String>,
    pub title_stamp: Option<String>,
    /// The session's own message inbox: the Unix socket Claude Code binds
    /// for cross-session messaging (`CLAUDE_CODE_MESSAGING_SOCKET`),
    /// captured from the hook helper's environment. An agent with one is
    /// ordered by posting to it rather than by typing into its terminal
    /// (§5.5), which needs no focus, no AppleScript and no TCC grant. The
    /// per-session token that rides beside it is **not** here: it stays
    /// inside the daemon (`adapters::rt::Adapters`), never in a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<String>,
}

/// Where the agent runs (content, except `key`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    /// `"claude:<session_id>"`, `"myapp:<id>"`: the idempotency key.
    pub key: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    pub pid_kind: Option<PidKind>,
    pub terminal: Option<Terminal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Deliver {
    /// Adapter-only: a parked hook response.
    Hold,
    Push,
    Poll,
    /// ENTER focuses the terminal; degraded holds and informational cards.
    Focus,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OriginKind {
    /// Built-in adapters only.
    Permission,
    Question,
    Elicitation,
    /// Any token or script; prefixed `» ` on the deck.
    Custom,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    pub index: u8,
    pub count: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Origin {
    pub kind: OriginKind,
    pub tool: Option<String>,
    pub request_id: Option<String>,
    pub group: Option<Group>,
}

impl Origin {
    pub const fn custom() -> Self {
        Origin {
            kind: OriginKind::Custom,
            tool: None,
            request_id: None,
            group: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Choice {
    pub label: String,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub index: u8,
    pub count: u8,
}

/// The card status machine of §4.8.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CardStatus {
    Pending,
    Answering {
        choice: Option<u8>,
        text: Option<String>,
    },
    Delivered,
    Cleared,
    Failed {
        reason: String,
    },
    Snoozed,
    Degraded {
        focused_at: Option<Ts>,
    },
}

impl CardStatus {
    /// The `status` string of a `-32005 bad_state` (§4.4), i.e. the name the
    /// status carries on the wire.
    pub const fn name(&self) -> &'static str {
        match self {
            CardStatus::Pending => "pending",
            CardStatus::Answering { .. } => "answering",
            CardStatus::Delivered => "delivered",
            CardStatus::Cleared => "cleared",
            CardStatus::Failed { .. } => "failed",
            CardStatus::Snoozed => "snoozed",
            CardStatus::Degraded { .. } => "degraded",
        }
    }

    /// Cards the device still shows (in the barrier): everything but a
    /// cleared card and a retired `focus` card. A `failed` card is shown
    /// again by the device itself after its 5 s revert (§4.8).
    pub const fn on_device(&self) -> bool {
        !matches!(
            self,
            CardStatus::Cleared
                | CardStatus::Degraded {
                    focused_at: Some(_)
                }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Card {
    pub id: CardId,
    #[serde(rename = "type")]
    pub kind: CardKind,
    #[serde(flatten)]
    pub status: CardStatus,
    pub deliver: Deliver,
    pub origin: Origin,
    /// Content.
    pub text: Option<String>,
    /// Content; empty when redacted (`choices_n` keeps the count).
    pub choices: Vec<Choice>,
    pub choices_n: u8,
    /// §5.6 (B10): shown index → the index in the choices the owner sent,
    /// when more than `MAX_CHOICES` were offered and the deck had to
    /// truncate (the recommended option is moved to slot 0 first, so the
    /// mapping is not the identity). Empty when nothing was cut. The
    /// answer carries `{choice, label}` with the index of the **shown**
    /// list, so an adapter with more than four options reads this to get
    /// back to the option it sent; without it, only the label is left to
    /// match on, and two long options can cut to the same one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choice_map: Vec<u8>,
    pub rec: Option<u8>,
    pub free_text: bool,
    pub page: Page,
    pub created: Ts,
    pub expires: Option<Ts>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

impl Card {
    /// **Guard 2's card half** (§5.5): is a dialog this card describes (or
    /// described) still on screen in the agent's terminal?
    ///
    /// One function, called from both the reducer (`op.order` → `-32006
    /// undeliverable{reason:"dialog_open"}`) and the core (the
    /// `DesktopJob::Inject` the worker re-checks), because a security
    /// predicate written twice is a predicate that will one day disagree
    /// with itself.
    ///
    /// The origins are the three the CLI itself raises: a `custom` or
    /// `ready` card is a script's own and blocks nothing.
    ///
    /// The statuses are every one that does **not** mean "the answer
    /// reached the CLI":
    ///
    /// * `pending` / `answering` / `snoozed` — the obvious ones: nobody
    ///   has answered yet, or the answer is still in flight.
    /// * `degraded` — the hold budget expired; §5.5's own word for it is
    ///   "answer it in the terminal", which is only possible because the
    ///   dialog is there.
    /// * **`failed`** — and this is the one §5.5's list forgets. `failed`
    ///   is written by exactly four paths (`hold_gone`, `undeliverable`,
    ///   `timeout`, `link_down`) and every one of them means the response
    ///   was **not** written into the CLI. Claude then falls back to asking
    ///   the operator in the terminal, with Allow highlighted — so a
    ///   `failed` permission card is the *most* certain evidence there is
    ///   that a dialog is up, and an injected `text + Return` would confirm
    ///   it. A held `q` card has no TTL (`expire_at` is `None` for
    ///   `Deliver::Hold`), so the state persists until the owner clears or
    ///   replaces the card.
    ///
    /// `delivered` and `cleared` are the two that are not here: both mean
    /// the CLI has its answer and the dialog is gone.
    #[must_use]
    pub const fn is_open_dialog(&self) -> bool {
        matches!(
            self.origin.kind,
            OriginKind::Permission | OriginKind::Question | OriginKind::Elicitation
        ) && matches!(
            self.status,
            CardStatus::Pending
                | CardStatus::Answering { .. }
                | CardStatus::Snoozed
                | CardStatus::Degraded { .. }
                | CardStatus::Failed { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reply {
    /// Content.
    pub text: Option<String>,
    pub at: Ts,
    pub announce: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole { User, Assistant, Tool }

/// Small references only. Image bytes never enter snapshots or the event ring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatImage {
    pub id: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MessageMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ChatImage>,
    pub id: String,
    pub role: ChatRole,
    #[serde(default)]
    pub streaming: bool,
    /// Explicit public plan updates; absent on ordinary chat/tool messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<crate::workflow::Plan>,
    /// Public background workflow counters, independent of the parent turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<crate::workflow::Run>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::workflow::ActivityStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatMessage {
    #[serde(flatten)]
    pub message: MessageMeta,
    pub text: String,
    pub at: Ts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Agent {
    pub id: AgentId,
    /// The device slot, a projection; `None` = "not on deck".
    pub slot: Option<Slot>,
    pub kind: Kind,
    pub name: String,
    pub state: AgentState,
    /// Content when `title_source` is `prompt`/`tool`.
    pub title: Option<String>,
    pub title_source: TitleSource,
    /// When `state` was entered.
    pub since: Ts,
    pub source: Source,
    pub owner: Owner,
    pub session: SessionInfo,
    pub caps: Caps,
    /// Optional native companion grouping and launcher/session identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<crate::companion::CompanionProfile>,
    pub card: Option<Card>,
    pub reply: Option<Reply>,
    /// Recent conversation, bounded to 64 messages / 512 KiB per agent.
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<crate::workflow::Plan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_runs: Vec<crate::workflow::Run>,
    /// Stable while messages grow; changes when retained history is pruned.
    #[serde(default)]
    pub chat_rev: u16,
    /// Bumped whenever `reply` changes. The `reply` line carries it and so does
    /// the window the host lays out, so the deck can tell a window of the text
    /// it is showing from one of the text before it.
    #[serde(default)]
    pub reply_rev: u16,
    pub voice: Option<String>,
    pub pinned: bool,
    /// Content.
    pub meta: serde_json::Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

impl Agent {
    pub fn native_companion(&self) -> bool { self.companion.is_some() || self.kind == Kind::Conol }

    /// Native clients use the app/session grammar on the deck. Brand-specific
    /// artwork can remain, while provider kinds stay intact in the API.
    pub fn device_kind(&self) -> Kind {
        if self.companion.is_some() && self.kind != Kind::Conol { Kind::Other } else { self.kind }
    }

    /// The project the session runs in: `project_label` of its cwd. State,
    /// not content — it is what the roster is grouped by on every surface.
    #[must_use]
    pub fn project(&self) -> Option<String> {
        if let Some(profile) = &self.companion { return Some(profile.group_label.clone()); }
        // Legacy registrations retain the Conol brand grouping.
        if self.kind == Kind::Conol { return Some("Conol".into()); }
        self.session
            .cwd
            .as_deref()
            .and_then(crate::roster::project_label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnswerSource {
    Deck,
    Ptt,
    Gui,
    Api,
}

/// A decision on a card: a choice, or free text (PTT `ctx:answer`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub choice: Option<u8>,
    /// Exactly the text the operator saw on the deck.
    pub label: Option<String>,
    pub text: Option<String>,
    pub source: AnswerSource,
    pub principal: Principal,
    pub session: Option<SessionId>,
    /// The PTT session this decision was dictated from (§4.12's
    /// `utterance?`), `None` for a typed or chosen one.
    #[serde(default)]
    pub utterance: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OrderSource {
    Ptt,
    Gui,
    Api,
}

/// Dictated or typed text for an agent (`op.order`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Order {
    pub text: String,
    pub ctx: String,
    pub source: OrderSource,
    pub utterance: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_status_flattens_into_the_api_shape() {
        let card = Card {
            id: CardId(17),
            kind: CardKind::Q,
            status: CardStatus::Answering {
                choice: Some(0),
                text: None,
            },
            deliver: Deliver::Hold,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            },
            text: Some("Bash: psql".into()),
            choices: vec![Choice {
                label: "Allow".into(),
                detail: None,
            }],
            choices_n: 1,
            choice_map: Vec::new(),
            rec: Some(0),
            free_text: true,
            page: Page { index: 0, count: 1 },
            created: Ts(0),
            expires: None,
            redacted: false,
        };
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["status"], "answering");
        assert_eq!(json["choice"], 0);
        assert_eq!(json["type"], "q");
        assert_eq!(json["origin"]["kind"], "permission");
        assert!(json.get("redacted").is_none());
        let back: Card = serde_json::from_value(json).unwrap();
        assert_eq!(back, card);
        assert!(CardStatus::Pending.on_device());
        assert!(CardStatus::Snoozed.on_device());
        assert!(!CardStatus::Cleared.on_device());
        assert!(!CardStatus::Degraded {
            focused_at: Some(Ts(1))
        }
        .on_device());
        assert!(CardStatus::Failed { reason: "x".into() }.on_device());
        assert!(CardStatus::Delivered.on_device());
    }

    /// **Guard 2's card half, as the whole cross product** (§5.5): five
    /// origins by seven statuses, every one stated.
    ///
    /// Written out rather than derived, because the interesting half of
    /// this table is the rows that must **not** block — a predicate that
    /// answered `true` for everything would pass a test that only checked
    /// the refusals, and would silently kill dictation for every agent
    /// that ever showed a card.
    #[test]
    fn the_dialog_predicate_states_every_origin_and_every_status() {
        fn card(kind: OriginKind, status: CardStatus) -> Card {
            Card {
                id: CardId(1),
                kind: CardKind::Q,
                status,
                deliver: Deliver::Hold,
                origin: Origin {
                    kind,
                    tool: None,
                    request_id: None,
                    group: None,
                },
                text: None,
                choices: Vec::new(),
                choices_n: 0,
                choice_map: Vec::new(),
                rec: None,
                free_text: false,
                page: Page { index: 0, count: 1 },
                created: Ts(0),
                expires: None,
                redacted: false,
            }
        }
        let statuses = [
            // (status, does a CLI dialog origin block on it?)
            (CardStatus::Pending, true),
            (
                CardStatus::Answering {
                    choice: Some(0),
                    text: None,
                },
                true,
            ),
            (CardStatus::Snoozed, true),
            (CardStatus::Degraded { focused_at: None }, true),
            (
                CardStatus::Degraded {
                    focused_at: Some(Ts(1)),
                },
                true,
            ),
            // The answer never reached the CLI, so the dialog is still up
            // with Allow highlighted. All four reasons mean that.
            (
                CardStatus::Failed {
                    reason: "hold_gone".into(),
                },
                true,
            ),
            (
                CardStatus::Failed {
                    reason: "undeliverable".into(),
                },
                true,
            ),
            (
                CardStatus::Failed {
                    reason: "timeout".into(),
                },
                true,
            ),
            (
                CardStatus::Failed {
                    reason: "link_down".into(),
                },
                true,
            ),
            // …and the two that mean the CLI has its answer.
            (CardStatus::Delivered, false),
            (CardStatus::Cleared, false),
        ];
        for (status, dialog) in statuses {
            for kind in [
                OriginKind::Permission,
                OriginKind::Question,
                OriginKind::Elicitation,
            ] {
                assert_eq!(
                    card(kind, status.clone()).is_open_dialog(),
                    dialog,
                    "{kind:?} × {}",
                    status.name()
                );
            }
            // A script's own card is not a terminal dialog and must not be
            // able to block dictation by existing.
            for kind in [OriginKind::Custom, OriginKind::Ready] {
                assert!(
                    !card(kind, status.clone()).is_open_dialog(),
                    "{kind:?} × {} must not block",
                    status.name()
                );
            }
        }
    }

    #[test]
    fn title_sources_classify_content() {
        assert!(TitleSource::Prompt.is_content());
        assert!(TitleSource::Tool.is_content());
        assert!(!TitleSource::Cwd.is_content());
        assert_eq!(
            serde_json::to_string(&TitleSource::SessionName).unwrap(),
            "\"session_name\""
        );
        assert_eq!(
            serde_json::to_string(&Caps::default()).unwrap(),
            r#"{"answer":"none","order":"none","focus":false}"#
        );
    }
}
