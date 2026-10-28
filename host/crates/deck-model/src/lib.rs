//! The daemon's domain model (design §3, §4.13, §6.5): the `Snapshot` every
//! party sees, the `Store` reducer `apply(Command, now) -> (events, effects)`,
//! the device projection and the per-scope redaction. Pure: no I/O and no
//! clock reads — time enters only as the `Instant` passed to `apply` and the
//! wall-clock base given to `StoreConfig`.

mod agent;
mod api;
mod cards;
mod command;
mod devapi;
mod device;
mod effect;
pub mod guard;
mod ids;
mod principal;
#[cfg(test)]
mod props;
mod redact;
mod roster;
pub mod sanitize;
pub mod schema;
pub mod companion;
pub mod session_controls;
mod snapshot;
mod store;
#[cfg(test)]
mod testkit;
mod tick;
mod time;
/// The PTT session machine **and** §5.6's one spoken grammar (B13), which
/// every adapter calls instead of keeping a word list of its own.
pub mod voice;
pub mod workflow;

pub use api::{ApiError, ApiResult};
pub use cards::{AnswerInput, MAX_CHOICES};
pub use redact::{redact, redact_event, redact_for};
pub use roster::{project_label, DISCONNECTED};
pub use store::{DELIVERY_BUDGET, FINALIZE_BUDGET, PTT_CAP, USAGE_REFRESH_GRACE};

pub use agent::{
    ChatImage, ChatMessage, ChatRole, MessageMeta,
    Agent, Answer, AnswerCap, AnswerSource, Caps, Card, CardStatus, Choice, Deliver, Group, Order,
    OrderCap, OrderSource, Origin, OriginKind, Owner, Page, PidKind, Reply, SessionInfo, Source,
    Terminal, TitleSource,
};
pub use command::{
    AgentRegister, AgentUpdate, CardClear, CardSet, Command, DeviceSet, FocusReport, LinkEvent,
    OpAnswer, OpOrder, ReplySet, Request, UtterancePhase, VoiceIn, VoiceListen, VoiceSay,
};
pub use device::{Control, DeviceOp, FaceId, Key, Lane, SyncReason};
pub use effect::{
    AudioCmd, DesktopCmd, Effect, Event, EventKind, Release, StatePatch, TimerKey, VoiceInput,
};
pub use ids::{AgentId, BadSessionId, CardId, HoldId, SessionId, Slot};
pub use principal::{AdapterKind, Principal, Scope};
pub use snapshot::{
    DeviceState, ErrCounts, Focus, FocusSource, Link, Listener, Permission, Permissions, PttInfo,
    Route, ServiceState, SessionState, Settings, Snapshot, Speaking, SttInfo, TtsInfo, Usage,
    VoiceState, VoiceTransport,
};
pub use store::{project, MacroLayout, Persisted, PinConfig, SlotMemory, Store, StoreConfig};
pub use time::Ts;

/// `hello.rx` the device must report.
pub const LINE_MAX_U16: u16 = deck_proto::LINE_MAX as u16;

/// The longest duration a client may ask for (`timeout_s`, `lease_ttl_s`):
/// a year, so an `Instant` deadline can never overflow from API input.
pub const MAX_CLIENT_SECS: u32 = 366 * 86_400;

// The wire vocabulary is the API vocabulary: same names, same JSON.
pub use deck_proto::{
    AgentState, Binding, CardKind, DeviceLine, DocFont, DocKind, ErrWhy, FaceEntry, HostLine,
    KeyId, Kind, MacroKey, PttCtx, SetKey, Template, Tone, UsageItem, Window,
};

#[cfg(test)]
mod chat_tests;
