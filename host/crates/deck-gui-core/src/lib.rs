//! The GUI's testable half (design §8.1, contract §4): the `async_channel`
//! event bus, the tokio bridge the gpui thread spawns onto, the `ApiClient`
//! that keeps one UDS session to `deckd`, and the `HostState` mirror that
//! folds the daemon's events into a `Snapshot` and answers with `HostEffect`s.
//!
//! [`admin`] and [`voice`] are the week-6 additions beside them: the reply
//! shapes of `integrations.*`, `token.*`, `log.tail`, `service.info`,
//! `config.get`, `voice.engines`, `voice.test` and `perm.request`, which are
//! **calls and not events** — nothing the daemon emits folds any of them into
//! the mirror, so the screens ask, keep the answer themselves, and ask again.
//!
//! The companion app's additions follow that pattern and one more. [`care`]
//! parses the replies of `sources.list`, `care.*` and the `[sources]` half
//! of `config.get`, which are calls. [`attention`] and [`menu`] are pure
//! functions of the mirror — what needs the owner's attention, and the
//! status item — so the whole of what the menu bar *says* is testable here
//! and only the status item itself is platform code.
//!
//! The window is the deck's companion and never a session's: the mirror
//! holds no agent and folds no event about one, and the hello asks for
//! `voice`, `device` and `admin` only ([`host_state`], [`api_client`]).
//!
//! There is no `gpui` here, deliberately. Everything in this crate is a plain
//! type a `#[test]` can drive, so the reducer suite (`tests/reducer.rs`) is
//! hermetic — scripted `ApiEvent`s in, a mirror and effects out, no daemon and
//! no window — and `deck-gui` above it is thin enough to read.
//!
//! The one rule the shape exists to keep: **nothing calls `block_on` on the
//! UI thread.** The bridge hands out a spawn and never a runtime handle, and
//! `ApiClient::call` is a synchronous queue push that returns a
//! `oneshot::Receiver` for a view to await on gpui's own executor.
pub mod admin;
pub mod api_client;
pub mod attention;
pub mod care;
pub mod events;
pub mod host_state;
pub mod i18n;
pub mod menu;
pub mod tokio_bridge;
pub mod voice;

pub use admin::{
    method_missing, ApiConfig, Change, Detected, Hijack, Info, Plan, Report, Secret, Status, Tail,
    TokenRow, Verb,
};
pub use api_client::ApiClient;
pub use attention::{Attention, Extras};
pub use care::{Checks, Footprint, FootprintItem, Source, Sources, SourcesConfig, TestStatus};
pub use events::{bus, ApiEvent, AppEvent, EventReceiver, EventSender};
pub use host_state::{
    Backoff, ConfigValue, HostEffect, HostState, Level, SheetKind, UiAction, BACKOFF_MAX,
    BACKOFF_MIN,
};
pub use menu::{IconState, MenuCommand, MenuModel, NoService};
pub use tokio_bridge::TokioBridge;
pub use voice::{Engine, Engines, Prompt, TestReport};
