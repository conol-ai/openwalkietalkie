//! The Service page (board Service): the background service — whether it
//! runs, its restart and its stop, and whether it starts at login — the two
//! updates, the local API's switch, a check of the whole setup with the
//! support file beside it, and the line that leads to everything the app
//! has put on this Mac. Its sub-pages are their own files:
//! [`crate::footprint`] and [`crate::local_api`]; the update flows are
//! [`crate::updates`].
//!
//! **Every fact on it is read, and from where it lives.** The service's
//! answers (`service.info`, `config.get`, `care.checks`, `care.footprint`,
//! `sources.list`, `integrations.status`) are kept in this page's pane,
//! because nothing folds a call's reply into the mirror; whether the deck is
//! linked and whether Bluetooth is allowed are the mirror's; the launch
//! agent is `SMAppService`'s, read when the window is made and again
//! whenever the page asks, a connection comes or goes, and every few
//! seconds, and never guessed: what the Deck page reads to call a service
//! "not installed" is always what macOS said.
//!
//! A service older than this app answers `-32601` for the care methods; the
//! sections that need them say "This needs a newer background service."
//! instead of drawing anything they could not read.
//!
//! The words are pure functions of those facts ([`check_rows`],
//! [`login_row`], [`api_line`], …), so a test reads every row in every state
//! without a window.
use crate::app;
use crate::deck::no_service_title;
use crate::footprint::{self, UNINSTALL_DIALOG};
use crate::local_api::{close, Shown, TOKEN_DIALOG};
use crate::login_item;
use crate::present::{self, LoginItem};
use crate::shell::{Screen, Sheet, Shell};
use crate::shots::Scene;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Row, Switch, Type as _};
use crate::updater::MacOsUpdater;
use crate::updates::{self, FirmwarePane, FirmwareStep, FIRMWARE_DIALOG};
use deck_client::ClientError;
use deck_gui_core::admin::{
    method_missing, port_clash, ApiConfig, Info, Report, Status, TokenRow, Verb,
};
use deck_gui_core::care::{Check, Checks, Footprint, Source, SourceState, Sources};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{DeviceState, Permission, ServiceState, Tone};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Between the page's blocks, and between its two columns.
const GAP: f32 = 20.;
/// Between the sections of the left column.
const COLUMN_GAP: f32 = 16.;

/// How often, in seconds, the launch agent is read again: the owner can
/// change it in System Settings while the window is open.
const AGENT_EVERY: u64 = 5;
/// How often the footprint is weighed again while its page is open.
const FOOTPRINT_EVERY: u64 = 5;

/// Where a newer Windows preview is downloaded from: the install guide,
/// which links the release's zip.
#[cfg(windows)]
const WINDOWS_DOWNLOAD: &str = "https://openwalkietalkie.org/docs/windows/";

/// The id of the Integrations page's plan dialog, after which a removal may
/// have changed what Footprint lists.
const PLAN_DIALOG: &str = "plan";

/// The harness's scenes that are this page's.
const SERVICE_SCENES: [Scene; 6] = [
    Scene::Service,
    Scene::Footprint,
    Scene::ServiceStop,
    Scene::ServiceFirmware,
    Scene::LocalApi,
    Scene::FootprintUninstall,
];

// ---- the replies the page keeps ----------------------------------------------

/// What a read of one of the care methods has come back with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Answer<T> {
    /// Nothing yet: not asked, or asked and not answered.
    #[default]
    Unread,
    /// The service has no such method (`-32601`): it is older than this app.
    Missing,
    /// What it said.
    Got(T),
}

impl<T> Answer<T> {
    pub fn got(&self) -> Option<&T> {
        match self {
            Answer::Got(answer) => Some(answer),
            Answer::Unread | Answer::Missing => None,
        }
    }

    pub fn missing(&self) -> bool {
        matches!(self, Answer::Missing)
    }

    /// Take a reply: the answer, parsed; a service without the method, as
    /// such; and from a call that failed or got no answer, nothing, so the
    /// last answer stands.
    pub fn take(&mut self, result: Result<Value, ClientError>, parse: impl FnOnce(&Value) -> T) {
        match result {
            Ok(value) => *self = Answer::Got(parse(&value)),
            Err(error) if method_missing(&error) => *self = Answer::Missing,
            Err(_) => {}
        }
    }
}

/// What the page reads of `config.get`: the local API's `[api]` table and
/// the switch of its TCP door, and whether the service logs what is said
/// in sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Api {
    pub config: ApiConfig,
    /// `api.tcp`, the door as the service resolved it: off unless its owner
    /// switched it on, or the pre-plugin Claude hooks still post to it.
    /// `None` from a service that has no such switch, whose door is always
    /// open: the page then offers none.
    pub tcp: Option<bool>,
    /// `api.tcp_rule`: which rule decided [`Api::tcp`]. `None` from a
    /// service that does not say, and for a word this build does not know.
    pub tcp_rule: Option<TcpRule>,
    /// `privacy.log_content`: the service keeps the text of prompts,
    /// replies and transcripts in its logs, and so in the support file
    /// ([`support_note`]). Off unless the owner turned it on; `None` from a
    /// reply that does not say.
    pub log_content: Option<bool>,
}

impl Api {
    pub fn parse(result: &Value) -> Api {
        let api = result.get("api");
        Api {
            config: ApiConfig::parse(result),
            tcp: api.and_then(|api| api.get("tcp")).and_then(Value::as_bool),
            tcp_rule: api
                .and_then(|api| api.get("tcp_rule"))
                .and_then(Value::as_str)
                .and_then(TcpRule::parse),
            log_content: result
                .get("privacy")
                .and_then(|privacy| privacy.get("log_content"))
                .and_then(Value::as_bool),
        }
    }
}

/// Why the local API's door is as it is, as the service says it
/// (`config.get`'s `api.tcp_rule`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpRule {
    /// The owner's own switch (`[api] tcp` in the service's file).
    Explicit,
    /// Nothing switched it: off.
    Default,
    /// Nothing switched it, and the service found Claude Code's hooks from
    /// before the plugin, which post to the door, at its start: open for
    /// them until Repair migrates them.
    LegacyHooks,
}

impl TcpRule {
    /// The service's word, or `None` for one this build does not know.
    pub fn parse(word: &str) -> Option<TcpRule> {
        match word {
            "explicit" => Some(TcpRule::Explicit),
            "default" => Some(TcpRule::Default),
            "legacy_hooks" => Some(TcpRule::LegacyHooks),
            _ => None,
        }
    }
}

// ---- what the page says, as pure functions ------------------------------------

/// How one setup check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Fine: a check mark.
    Ok,
    /// Something to do: the warning mark, and the remedy under it when there
    /// is one to name.
    Problem,
    /// Neither: the page could not tell (an older service, a read that has
    /// not come back, a copy of the app with nothing to compare).
    NotChecked,
}

/// One row of "Check my setup".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRow {
    pub verdict: Verdict,
    pub title: String,
    pub sub: Option<String>,
}

/// The Codex client, as far as the page can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexCheck {
    /// The owner switched it off: the row is not drawn.
    Off,
    Reachable,
    NotRunning,
    NotChecked,
}

/// What "Check my setup" is drawn from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// A deck is linked.
    pub linked: bool,
    /// The service runs from this copy of the app; `None` when it cannot be
    /// told.
    pub this_copy: Option<bool>,
    /// The Claude Code plugin's installer status, once read.
    pub claude: Option<Status>,
    /// Claude sessions still on a helper from before the last update.
    pub stale_helpers: u32,
    pub codex: CodexCheck,
    /// No launch agent is left from before the rename; `None` when not
    /// checked.
    pub legacy_agent: Option<bool>,
    /// Bluetooth, as macOS has it for the service.
    pub bluetooth: Permission,
}

/// The rows of "Check my setup", in the board's order: the deck, the copy
/// of the app the service runs from, the Claude Code plugin, the old helper
/// (only while a session uses one), Codex (only while its switch is on), a
/// leftover launch agent and Bluetooth (macOS only).
pub fn check_rows(setup: &Setup, language: Language) -> Vec<CheckRow> {
    let row = |verdict, title: &str, sub: Option<&str>| CheckRow {
        verdict,
        title: title.to_string(),
        sub: sub.map(str::to_string),
    };
    let mut rows = vec![if setup.linked {
        row(Verdict::Ok, language.text("The deck is linked"), None)
    } else {
        row(
            Verdict::Problem,
            language.text("The deck is not linked"),
            None,
        )
    }];
    rows.push(match setup.this_copy {
        Some(true) => row(
            Verdict::Ok,
            language.text("The service runs from this copy of the app"),
            None,
        ),
        Some(false) => row(
            Verdict::Problem,
            language.text("The service runs from another copy of the app"),
            Some(language.text("Quit the other copy, then restart the service.")),
        ),
        None => row(
            Verdict::NotChecked,
            language.text("The service runs from this copy of the app"),
            None,
        ),
    });
    rows.push(match &setup.claude {
        Some(Status::Installed) => row(
            Verdict::Ok,
            language.text("Claude Code plugin is current"),
            None,
        ),
        Some(Status::Absent) => row(
            Verdict::Problem,
            language.text("Claude Code plugin is not set up"),
            None,
        ),
        Some(_) => row(
            Verdict::Problem,
            language.text("Claude Code plugin needs repair"),
            None,
        ),
        None => row(
            Verdict::NotChecked,
            language.text("Claude Code plugin is current"),
            None,
        ),
    });
    match setup.stale_helpers {
        0 => {}
        1 => rows.push(row(
            Verdict::Problem,
            language.text("1 Claude session uses an old helper"),
            Some(language.text("Restart that session.")),
        )),
        n => rows.push(CheckRow {
            verdict: Verdict::Problem,
            title: language.format("{0} Claude sessions use an old helper", &[&n.to_string()]),
            sub: Some(language.text("Restart those sessions.").to_string()),
        }),
    }
    match setup.codex {
        CodexCheck::Off => {}
        CodexCheck::Reachable => {
            rows.push(row(Verdict::Ok, language.text("Codex is reachable"), None))
        }
        CodexCheck::NotRunning => {
            rows.push(row(
                Verdict::Problem,
                language.text("Codex is not running"),
                None,
            ));
        }
        CodexCheck::NotChecked => {
            rows.push(row(
                Verdict::NotChecked,
                language.text("Codex is reachable"),
                None,
            ));
        }
    }
    // macOS's own: an agent from before the rename, and Bluetooth's TCC
    // grant. Windows has neither to check.
    if cfg!(windows) {
        return rows;
    }
    rows.push(match setup.legacy_agent {
        Some(true) => row(Verdict::Ok, language.text("No leftover launch agent"), None),
        Some(false) => row(
            Verdict::Problem,
            language.text("An old launch agent is still installed"),
            Some(language.text("Remove it with Uninstall, or by hand in ~/Library/LaunchAgents.")),
        ),
        None => row(
            Verdict::NotChecked,
            language.text("No leftover launch agent"),
            None,
        ),
    });
    rows.push(match setup.bluetooth {
        Permission::Granted => row(Verdict::Ok, language.text("Bluetooth is allowed"), None),
        Permission::Denied => row(
            Verdict::Problem,
            language.text("Bluetooth is not allowed"),
            None,
        ),
        // Not asked yet, or not known: no verdict either way.
        Permission::NotDetermined | Permission::Unknown => row(
            Verdict::NotChecked,
            language.text("Bluetooth is allowed"),
            None,
        ),
    });
    rows
}

/// Whether the service runs from this copy of the app: its executable is
/// the `deckd` beside this app's own. `None` when either is not known.
pub fn this_copy(executable: Option<&Path>, own: Option<&Path>) -> Option<bool> {
    Some(executable? == own?)
}

/// Codex, from the service's own check of its client, else from the Codex
/// source's state; the row is left out while the client is switched off.
pub fn codex_check(checks: Option<&Checks>, sources: Option<&Sources>) -> CodexCheck {
    if let Some(check) = checks.and_then(|checks| checks.get(Check::CODEX_DAEMON)) {
        return match check.ok {
            Some(true) => CodexCheck::Reachable,
            Some(false) => CodexCheck::NotRunning,
            // Not checked: the client is switched off.
            None => CodexCheck::Off,
        };
    }
    match sources.and_then(|sources| sources.get(Source::CODEX)) {
        Some(codex) if !codex.enabled => CodexCheck::Off,
        Some(codex) => match codex.state {
            Some(SourceState::Connected) => CodexCheck::Reachable,
            Some(SourceState::Waiting | SourceState::Off) => CodexCheck::NotRunning,
            _ => CodexCheck::NotChecked,
        },
        None => CodexCheck::NotChecked,
    }
}

/// Whether no launch agent is left from before the rename, by the service's
/// check; `None` when it has not said.
pub fn legacy_agent(checks: Option<&Checks>) -> Option<bool> {
    checks?.get(Check::LEGACY_LAUNCH_AGENT)?.ok
}

/// Claude sessions on an old helper: the Claude Channels source's count,
/// else what the Integrations page has read (`known`).
pub fn stale_helpers(sources: Option<&Sources>, known: u32) -> u32 {
    sources
        .and_then(|sources| sources.get(Source::CLAUDE_CHANNELS))
        .and_then(|channels| channels.stale)
        .unwrap_or(known)
}

/// The Start at login row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginRow {
    /// The switch is on: macOS has the service's launch agent registered,
    /// or waiting for the owner's approval.
    pub on: bool,
    /// The switch can be moved: only in the installed app, and turned on
    /// only once a deck has been seen on this Mac.
    pub enabled: bool,
    /// The line under it.
    pub sub: Option<&'static str>,
    /// Offer **Open Settings** (Login Items): macOS waits for the owner.
    pub settings: bool,
}

/// The Start at login row for the launch agent's status, in the installed
/// app or not, with a deck seen on this Mac or not (`deck_seen`).
///
/// Turning it on registers the background service, which owner decision 7
/// keeps until a deck has been seen: until then a switch that is off cannot
/// be moved, and says to link the deck. One that is on can always be turned
/// off: that adds nothing.
pub fn login_row(
    agent: LoginItem,
    installed: bool,
    deck_seen: bool,
    language: Language,
) -> LoginRow {
    let on = agent.registered();
    let waiting = installed && agent == LoginItem::RequiresApproval;
    let unseen = installed && !on && !deck_seen;
    let sub = if !installed {
        Some(language.text("Only in the installed app."))
    } else if waiting {
        Some(language.text("Waiting for your approval in System Settings > General > Login Items."))
    } else if unseen {
        Some(language.text("Link the deck first."))
    } else {
        None
    };
    LoginRow {
        on,
        enabled: installed && !unseen,
        sub,
        settings: waiting,
    }
}

/// What the Local API row says while another program holds its port
/// (`service.degraded`, one of the two port words
/// [`deck_gui_core::admin::port_clash`] names): what that means, in place
/// of the line that would call the door open. `None` while nothing clashes,
/// and while the door is off (`api.tcp` false, which is what a service of
/// this release says unless its owner switched the door on): nothing is
/// bound, so nothing can clash.
pub fn api_clash(api: Option<&Api>, service: &ServiceState, language: Language) -> Option<String> {
    if api.is_some_and(|api| api.tcp == Some(false)) {
        return None;
    }
    service
        .degraded
        .as_deref()
        .filter(|reason| port_clash(reason))?;
    present::degraded_banner_localized(service, language)
}

/// The Local API row's line: off, or on and on which port (the one the
/// service holds, else the one configured).
pub fn api_line(api: &Api, held: Option<u16>, language: Language) -> String {
    if api.tcp == Some(false) {
        return language
            .text("Off. Nothing shipped with the deck uses them.")
            .to_string();
    }
    let port = held
        .or(api.config.port)
        .map_or_else(|| "\u{2014}".to_string(), |port| port.to_string());
    language.format(
        "On. Port {0}. Local tools can use it with a token.",
        &[&port],
    )
}

/// What the local API's switch asks before it acts: the sheet's title, its
/// sentence and the word on its going-ahead button, whether that button is
/// a destructive one, and whether going ahead repairs Claude Code's
/// integration instead of switching the door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiQuestion {
    pub title: String,
    pub body: String,
    pub confirm: String,
    pub danger: bool,
    pub repair_first: bool,
}

/// [`ApiQuestion`] for switching the door `on` or off. Off, while the
/// service says the door is open for Claude Code's hooks from before the
/// plugin ([`TcpRule::LegacyHooks`]), the sheet says they still use it and
/// offers Repair first: closing the door under them would leave Claude's
/// permission questions in the terminal. Repair migrates them, and the
/// service closes the door at its next start; the rule it published stays
/// until then, so once the Claude row (`claude`, the installer's latest
/// read) is no longer `outdated` the sheet is the plain one again. A row
/// not read yet takes the service at its word.
pub fn api_question(
    on: bool,
    api: Option<&Api>,
    claude: Option<&Status>,
    language: Language,
) -> ApiQuestion {
    let (title, confirm) = if on {
        (language.text("Turn the local API on?"), language.text("Turn On"))
    } else {
        (language.text("Turn the local API off?"), language.text("Turn Off"))
    };
    let mut question = ApiQuestion {
        title: title.to_string(),
        body: language.text("The service restarts.").to_string(),
        confirm: confirm.to_string(),
        danger: !on,
        repair_first: false,
    };
    let legacy = api.is_some_and(|api| api.tcp_rule == Some(TcpRule::LegacyHooks));
    if !on && legacy && claude.is_none_or(|status| *status == Status::Outdated) {
        question.body = language
            .text(
                "Claude Code's older hooks still use the local API. Repair them first, or \
                 Claude Code's questions stay in the terminal.",
            )
            .to_string();
        question.confirm = language.text("Repair").to_string();
        question.danger = false;
        question.repair_first = true;
    }
    question
}

/// The line under **Export Support File**: where the file goes, and whether
/// the text said in sessions is in it. The service's redaction (`deckd`
/// diagnostics) drops the text of prompts, replies and transcripts while
/// content logging is off, its default, and keeps it while it is on
/// (`privacy.log_content`, [`Api::log_content`]); the line says which, and
/// the default until `config.get` has said. Agent names and working
/// directories are the service's state, not session text, and stay.
pub fn support_note(api: Option<&Api>, language: Language) -> &'static str {
    if api.and_then(|api| api.log_content) == Some(true) {
        language.text("The file is saved to the Desktop. Session text is kept in it.")
    } else {
        language.text("The file is saved to the Desktop. Session text is left out.")
    }
}

/// The firmware the deck runs, or ran when it was last linked, as the
/// version alone (`0.13.0`).
pub fn deck_version(device: &DeviceState) -> Option<String> {
    let fw = device
        .fw
        .as_deref()
        .or_else(|| device.last.as_ref()?.fw.as_deref())?;
    Some(present::fw_text(fw, None)).filter(|version| !version.is_empty())
}

/// The Updates rows' titles: this app's version, and the deck's firmware
/// (a dash while no deck has said which it runs).
pub fn update_titles(deck: Option<&str>, language: Language) -> (String, String) {
    (
        language.format("OpenWalkieTalkie {0}", &[env!("CARGO_PKG_VERSION")]),
        language.format("Deck firmware {0}", &[deck.unwrap_or("\u{2014}")]),
    )
}

/// The deck firmware row's line: how an install reaches the deck. The
/// service asks for ENTER on a linked deck before it installs anything.
pub fn firmware_sub(language: Language) -> &'static str {
    language.text("By cable, after a press on the deck.")
}

// ---- the pane ------------------------------------------------------------------

/// The page's own state, which the shell owns: the service's answers, the
/// launch agent's status, the firmware, the token fields, and the two flows
/// that outlive a frame (an export, an uninstall).
pub struct ServicePane {
    /// The service's launch agent, as `SMAppService` last answered.
    agent: LoginItem,
    /// The `deckd` beside this app's own executable, as the file system
    /// resolves it: the service this copy would run.
    own_deckd: Option<PathBuf>,
    /// The app's own updates (Sparkle, in a signed release).
    pub(crate) updater: MacOsUpdater,
    pub(crate) firmware: FirmwarePane,
    /// `service.info`, and the executable it names (resolved).
    pub(crate) info: Option<Info>,
    executable: Option<PathBuf>,
    /// `config.get`'s local API.
    pub(crate) api: Option<Api>,
    checks: Answer<Checks>,
    pub(crate) footprint: Answer<Footprint>,
    pub(crate) footprint_pending: bool,
    /// Footprint is to be weighed again (a removal may have happened).
    footprint_stale: bool,
    sources: Option<Sources>,
    /// The Claude Code plugin's installer status, once read.
    claude: Option<Status>,
    pub(crate) tokens: Vec<TokenRow>,
    pub(crate) tokens_read: bool,
    /// The one-time plaintext of a token just made (see `local_api`).
    pub(crate) shown: Option<Shown>,
    /// The Local API page's port field, and the new token's name.
    pub(crate) port: Entity<InputState>,
    pub(crate) token_name: Entity<InputState>,
    /// The scopes the new token gets.
    pub(crate) scopes: Vec<String>,
    /// The page asked the service for what it shows.
    asked: bool,
    exporting: bool,
    pub(crate) uninstalling: bool,
    /// How the last Uninstall went, for its dialog.
    pub(crate) uninstalled: Option<Result<(), String>>,
    /// The connection as the last event left it.
    was_connected: bool,
    /// A harness scene drew this pane: everything in it is the scene's, the
    /// page reads nothing, and it is drawn as the installed, signed app
    /// shows it.
    staged: bool,
}

impl ServicePane {
    /// The pane, and the subscriptions its two text fields need kept alive:
    /// Return in either is its button.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> (ServicePane, Vec<Subscription>) {
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("47201"));
        let token_name = cx.new(|cx| InputState::new(window, cx));
        let change = cx.subscribe_in(&port, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.change_api_port(window, cx);
            }
        });
        let create = cx.subscribe_in(&token_name, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.create_api_token(window, cx);
            }
        });
        let pane = ServicePane {
            agent: login_item::agent_status(),
            own_deckd: app::deckd_path().map(resolved),
            // Sparkle only in the installed app run for real: it reaches the
            // network and writes the app's defaults as it starts.
            updater: MacOsUpdater::for_run(app::launch().installed()),
            firmware: FirmwarePane::default(),
            info: None,
            executable: None,
            api: None,
            checks: Answer::Unread,
            footprint: Answer::Unread,
            footprint_pending: false,
            footprint_stale: false,
            sources: None,
            claude: None,
            tokens: Vec::new(),
            tokens_read: false,
            shown: None,
            port,
            token_name,
            // `observe` is the least a token can usefully have, and the one
            // scope that gives a script nothing it could break.
            scopes: vec!["observe".to_string()],
            asked: false,
            exporting: false,
            uninstalling: false,
            uninstalled: None,
            was_connected: false,
            staged: false,
        };
        (pane, vec![change, create])
    }

    /// A deck firmware newer than the deck's, by its version, once the
    /// service has found one: the deck on the cable's, else the one its
    /// hello named ([`updates::newer_firmware`]).
    pub(crate) fn firmware_available(&self) -> Option<String> {
        updates::newer_firmware(&self.firmware.status)
    }

    /// The background service's launch agent, as `SMAppService` last
    /// reported it: read when the pane was made and again whenever the page
    /// asks, never assumed.
    pub(crate) fn agent(&self) -> LoginItem {
        self.agent
    }

    /// A harness scene drew the pane.
    pub(crate) fn staged(&self) -> bool {
        self.staged
    }

    /// Put away what a scene staged: every answer goes, the agent and this
    /// copy's own service are read again, and the page reads for itself.
    fn unstage(&mut self) {
        if !self.staged {
            return;
        }
        self.staged = false;
        self.agent = login_item::agent_status();
        self.own_deckd = app::deckd_path().map(resolved);
        self.firmware = FirmwarePane::default();
        self.info = None;
        self.executable = None;
        self.api = None;
        self.checks = Answer::Unread;
        self.footprint = Answer::Unread;
        self.sources = None;
        self.claude = None;
        self.tokens.clear();
        self.tokens_read = false;
        close(&mut self.shown);
        self.uninstalled = None;
        self.asked = false;
    }
}

/// `path` as the file system resolves it, or as it is when it cannot.
fn resolved(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// The executable `service.info` names, as the file system resolves it.
pub fn executable(info: &Value) -> Option<PathBuf> {
    info.get("executable")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| resolved(PathBuf::from(path)))
}

// ---- the scenes' fixtures -------------------------------------------------------

/// The replies the scenes are drawn from: `deck-gui-core`'s fixtures, read by
/// the parsers the page reads the service with.
const CARE_CHECKS: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_checks.json");
const CARE_FOOTPRINT: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_footprint.json");
const SOURCES_LIST: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/sources_list.json");
const CONFIG_GET: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json");

/// The home the footprint fixture's paths are under.
const FIXTURE_HOME: &str = "/Users/you";

/// The installed app's own service, as the scenes' `service.info` names it.
const BOARD_DECKD: &str = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";

/// A fixture, parsed; nothing when it is not JSON.
fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_default()
}

/// The footprint the boards draw: the fixture's, under this Mac's home so
/// its paths read as the page prints a real one, with the settings and the
/// logs at the board's sizes (the fixture weighs them in binary units).
pub(crate) fn board_footprint() -> Footprint {
    let mut footprint = Footprint::parse(&fixture(CARE_FOOTPRINT));
    let home = std::env::var("HOME").unwrap_or_default();
    for item in &mut footprint.items {
        if let Some(rest) = item
            .path
            .as_deref()
            .and_then(|path| path.strip_prefix(FIXTURE_HOME))
        {
            item.path = Some(format!("{home}{rest}"));
        }
        match item.id.as_str() {
            "data" => item.bytes = Some(180_000),
            "logs" => item.bytes = Some(77_000_000),
            _ => {}
        }
    }
    footprint
}

/// A release the service found newer than the deck's, with the deck on the
/// cable: the firmware dialog's scene.
fn board_firmware() -> Value {
    serde_json::json!({
        "phase": "ready",
        "progress": 0,
        "message": "Firmware release checked. Installation requires your confirmation.",
        "device": {
            "serial": "5E2A91C04B7D",
            "board": "hw-b",
            "version": "0.13.0",
            "recovery": false,
            "recovery_protocol": 2,
            "maintenance": false
        },
        "release": {
            "firmware": {
                "version": "0.14.0",
                "protocol": 2,
                "variants": {
                    "hw-a": {"name": "OpenWalkieTalkie-0.14.0-hw-a.bin"},
                    "hw-b": {"name": "OpenWalkieTalkie-0.14.0-hw-b.bin"}
                }
            }
        },
        "error": null,
        "available": true,
        "bootloader_warning": false
    })
}

/// A minute on this Mac's clock, written as the service writes a moment
/// (RFC 3339, UTC), so the token lines of the scene, which the page prints
/// on this Mac's clock, read the same in every time zone.
fn board_stamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    present::local_ts(year, month, day, hour, minute)
        .unwrap_or_default()
        .to_string()
}

/// Two tokens, as `token.list` answers: a connected app's and a script's.
fn board_tokens() -> Vec<TokenRow> {
    TokenRow::list(&serde_json::json!([
        {
            "name": "conol",
            "scopes": ["observe", "agent", "voice"],
            "created": board_stamp(2026, 11, 2, 9, 12),
            "expires": null,
            "embedded_in": [],
            "client": "Conol"
        },
        {
            "name": "backup-script",
            "scopes": ["observe"],
            "created": board_stamp(2026, 11, 10, 16, 40),
            "expires": board_stamp(2027, 2, 10, 16, 40),
            "embedded_in": []
        }
    ]))
}

impl ServicePane {
    /// The boards' service: running from the installed app, at login,
    /// 0.13.0, the local API off (on for the Local API scene), every check
    /// passed but the old helper, and the footprint of nine items.
    fn stage_board(&mut self, api_on: bool) {
        self.staged = true;
        self.asked = true;
        self.agent = LoginItem::Enabled;
        self.own_deckd = Some(PathBuf::from(BOARD_DECKD));
        self.info = Some(Info::parse(
            &serde_json::json!({"daemon": "0.13.0", "port": 47201}),
        ));
        self.executable = Some(PathBuf::from(BOARD_DECKD));
        let mut config = fixture(CONFIG_GET);
        config["api"]["tcp"] = api_on.into();
        self.api = Some(Api::parse(&config));
        self.checks = Answer::Got(Checks::parse(&fixture(CARE_CHECKS)));
        self.footprint = Answer::Got(board_footprint());
        self.sources = Some(Sources::parse(&fixture(SOURCES_LIST)));
        let (reports, _) = Report::list(&serde_json::json!({
            "targets": [{"target": "claude", "status": "installed"}]
        }));
        self.claude = reports.into_iter().next().map(|report| report.status);
        self.tokens = board_tokens();
        self.tokens_read = true;
    }
}

// ---- the page ------------------------------------------------------------------

impl Shell {
    /// Ask the service for what the page draws, unless the page has it
    /// already; `force` asks anyway (a reconnect, **Run Again**). The launch
    /// agent is read every time: it is this Mac's, and it costs nothing.
    pub(crate) fn refresh_service(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.service.staged {
            return;
        }
        self.service.agent = login_item::agent_status();
        if !self.state.connected() {
            return;
        }
        if force || !self.service.asked {
            self.service.asked = true;
            self.read_service_setup(window, cx);
            self.read_service_config(window, cx);
            self.read_footprint(window, cx);
        }
        if self.screen == Screen::LocalApi && (force || !self.service.tokens_read) {
            self.read_api_tokens(window, cx);
        }
    }

    /// What "Check my setup" is read from: the service, its checks, the
    /// sources and the installer.
    fn read_service_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::ServiceInfo,
            window,
            cx,
            |this, result, window, cx| {
                if let Ok(value) = result {
                    this.service.info = Some(Info::parse(&value));
                    this.service.executable = executable(&value);
                    // The port field's hint is the port the service holds.
                    this.localize_service(window, cx);
                }
            },
        );
        self.ask(UiAction::CareChecks, window, cx, |this, result, _, _| {
            this.service.checks.take(result, Checks::parse);
        });
        self.ask(UiAction::SourcesList, window, cx, |this, result, _, _| {
            if let Ok(value) = result {
                this.service.sources = Some(Sources::parse(&value));
            }
        });
        // `detect: false`: the installer reads its files and runs no CLI.
        self.ask(
            UiAction::IntegrationsStatus { detect: false },
            window,
            cx,
            |this, result, _, _| {
                if let Ok(value) = result {
                    let (reports, _) = Report::list(&value);
                    this.service.claude = reports
                        .into_iter()
                        .find(|report| report.target == "claude")
                        .map(|report| report.status);
                }
            },
        );
    }

    /// `config.get`, for the local API's switch and port.
    fn read_service_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::ConfigGet,
            window,
            cx,
            |this, result, window, cx| {
                if let Ok(value) = result {
                    this.service.api = Some(Api::parse(&value));
                    this.localize_service(window, cx);
                }
            },
        );
    }

    /// Service, Footprint or Local API, whichever is open. `dim` is set
    /// while the service is not there: what needs it cannot be pressed.
    pub(crate) fn render_service(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        match self.screen {
            Screen::Footprint => self.render_footprint(dim, cx),
            Screen::LocalApi => self.render_local_api(dim, cx),
            _ => self.render_service_page(dim, cx),
        }
    }

    /// The page's dialog `id` and its width: the firmware's review, the
    /// token shown once, how Uninstall went.
    pub(crate) fn service_dialog(
        &self,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        match id {
            FIRMWARE_DIALOG => Some(self.render_firmware_dialog(cx)),
            TOKEN_DIALOG => self.render_shown_token(cx),
            UNINSTALL_DIALOG => self.render_uninstalled(cx),
            _ => None,
        }
    }

    /// A dialog went. The token's takes its plaintext with it; the
    /// installer's plan dialog (the Integrations page's) may have removed
    /// something Footprint lists, so it is weighed again.
    pub(crate) fn service_dialog_closed(&mut self, id: &'static str, _cx: &mut Context<Self>) {
        match id {
            TOKEN_DIALOG => {
                close(&mut self.service.shown);
            }
            UNINSTALL_DIALOG => self.service.uninstalled = None,
            PLAN_DIALOG => self.service.footprint_stale = true,
            _ => {}
        }
    }

    /// The text fields' hints: the port the service holds, and the token's.
    pub(crate) fn localize_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let port = self
            .service
            .info
            .as_ref()
            .and_then(|info| info.port)
            .or_else(|| self.service.api.as_ref().and_then(|api| api.config.port))
            .map_or_else(|| "47201".to_string(), |port| port.to_string());
        self.service
            .port
            .update(cx, |input, cx| input.set_placeholder(port, window, cx));
        let name = self.tr("A name for the new token…");
        self.service
            .token_name
            .update(cx, |input, cx| input.set_placeholder(name, window, cx));
    }

    /// A connection that came or went: what was read stays until the page
    /// asks again, and the launch agent is read again (the Deck page says
    /// "not installed" by it).
    pub(crate) fn on_service_event(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        let connected = self.state.connected();
        if connected == self.service.was_connected {
            return;
        }
        self.service.was_connected = connected;
        if !connected {
            self.service.asked = false;
            self.service.tokens_read = false;
        }
        if !self.service.staged {
            self.service.agent = login_item::agent_status();
        }
    }

    /// Once a second: the launch agent every few seconds, and the footprint
    /// while its page is open or after a removal.
    pub(crate) fn tick_service(&mut self, beat: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.staged {
            return;
        }
        if beat.is_multiple_of(AGENT_EVERY) {
            self.service.agent = login_item::agent_status();
        }
        let weigh = self.screen == Screen::Footprint && beat.is_multiple_of(FOOTPRINT_EVERY);
        if self.service.footprint_stale || weigh {
            self.service.footprint_stale = false;
            self.read_footprint(window, cx);
        }
    }

    /// The page's scenes, drawn from fixtures as the installed app would
    /// show them, with the question or the dialog each state scene is about
    /// up. Any other scene finds the pane as this run would have it.
    pub(crate) fn stage_service(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.service.unstage();
        if !SERVICE_SCENES.contains(&scene) {
            return false;
        }
        self.service.stage_board(scene == Scene::LocalApi);
        if scene == Scene::ServiceFirmware {
            self.service.firmware.status = board_firmware();
        }
        self.go(scene.screen(), window, cx);
        match scene {
            Scene::ServiceStop => self.ask_stop_service(window, cx),
            Scene::ServiceFirmware => self.review_firmware(window, cx),
            Scene::FootprintUninstall => self.ask_uninstall_app(window, cx),
            _ => {}
        }
        true
    }

    /// Whether this run is the installed app, as the page draws it: a
    /// harness scene draws the installed app.
    pub(crate) fn service_installed(&self) -> bool {
        self.service.staged || app::launch().installed()
    }

    // ---- the Service page ----------------------------------------------------

    /// Two columns — the service, its updates and the local API; the check
    /// of the setup — then the footprint's line.
    fn render_service_page(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let header = ui::Header::new(
            self.tr(Screen::Service.label()),
            self.tr("The background service, updates, and what this app has added to your Mac."),
        );
        let left = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(COLUMN_GAP))
            .child(ui::section(
                self.tr("Background service"),
                self.service_background_card(&p, cx),
                &p,
            ))
            .child(ui::section(
                self.tr("Updates"),
                self.service_updates_card(dim, &p, cx),
                &p,
            ))
            .child(ui::section(
                self.tr("Local API"),
                self.service_api_card(dim, &p, cx),
                &p,
            ));
        // The check's card takes the column's height when the left one is
        // the taller.
        let right = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(8.))
            .child(ui::label(self.tr("Check my setup"), &p))
            .child(self.service_setup_card(dim, &p, cx).flex_1());
        let columns = div()
            .flex()
            .items_stretch()
            .w_full()
            .gap(px(GAP))
            .child(left)
            .child(right);
        let body = v_flex()
            .gap(px(GAP))
            .child(header)
            .child(columns)
            .child(ui::section(
                self.tr("Footprint"),
                self.service_footprint_card(&p, cx),
                &p,
            ));
        ui::page("service", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// Whether the service runs, with **Restart** and **Stop…**; or why it
    /// does not, in the Deck page's words and with its buttons (not
    /// running: **Start**, **Open Log**; never set up: **Set Up…**; held by
    /// macOS: **Open Settings**); and Start at login.
    fn service_background_card(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let state = if self.state.connected() {
            Row::new()
                .title(self.tr("The service is running"))
                .sub(self.tr("It keeps the deck linked."))
                .right(
                    Btn::new("service-restart", self.tr("Restart")).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.service_lifecycle(UiAction::ServiceRestart, window, cx)
                        },
                    )),
                )
                .right(Btn::new("service-stop", self.tr("Stop…")).quiet().on_click(
                    cx.listener(|this, _, window, cx| this.ask_stop_service(window, cx)),
                ))
        } else {
            let why = self.service_absence();
            self.no_service_buttons(why, "service-absent", true, cx)
                .into_iter()
                .fold(
                    Row::new().title(no_service_title(why, self.language())),
                    Row::right,
                )
        };
        let login = login_row(
            self.service.agent,
            self.service_installed(),
            self.prefs(cx).deck_seen(),
            self.language(),
        );
        let switch = Switch::new("service-login", login.on)
            .disabled(!login.enabled)
            .on_toggle(cx.listener(|this, on: &bool, window, cx| {
                this.set_start_at_login(*on, window, cx);
            }));
        let login = Row::new()
            .title(self.tr("Start at login"))
            .when_some(login.sub, |row, sub| row.sub(sub))
            .when(login.settings, |row| {
                row.right(
                    Btn::new("service-login-settings", self.tr("Open Settings"))
                        .on_click(|_, _, cx: &mut App| cx.open_url(present::LOGIN_ITEMS_SETTINGS)),
                )
            })
            .right(switch);
        ui::card(vec![state.into_any_element(), login.into_any_element()], p)
    }

    /// This app's version with **Check**, and the deck's firmware with
    /// **Check**, or **Review…** once there is something to review. The
    /// app's line names a newer app once the service found one.
    fn service_updates_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let (app, deck) = update_titles(
            deck_version(&self.state.mirror().device).as_deref(),
            self.language(),
        );
        let app_line = updates::app_update_line(&self.service.firmware.status, self.language())
            .unwrap_or_else(|| {
                self.tr("The app and the service update together.")
                    .to_string()
            });
        // Sparkle updates a signed release; any other build says why not.
        let signed = self.service.staged || self.service.updater.uses_signed_updates();
        #[cfg(not(windows))]
        let check_app = Btn::new("service-app-check", self.tr("Check"))
            .disabled(!signed)
            .when(!signed, |button| {
                button.tooltip(self.tr("automatic updates are unavailable in this build"))
            })
            .on_click(cx.listener(|this, _, window, cx| this.check_app_update(window, cx)));
        // Windows has no Sparkle: **Check** asks the service to look (the
        // release manifest names the app's version too), and once it found
        // a newer app **Update** fetches and installs it (`win_update`).
        // A copy that is not the installed one (a build, a walk) offers the
        // page its zip is downloaded from instead.
        #[cfg(windows)]
        let check_app = {
            let _ = signed;
            let newer = updates::app_update_line(&self.service.firmware.status, self.language())
                .is_some()
                .then(|| updates::app_version(&self.service.firmware.status));
            if let Some(version) = newer.clone().filter(|_| crate::app::launch().installed()) {
                let updating = crate::win_update::busy();
                Btn::new(
                    "service-app-check",
                    if updating { self.tr("Updating\u{2026}") } else { self.tr("Update") },
                )
                .disabled(updating)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_app(version.clone(), window, cx);
                }))
            } else if newer.is_some() {
                Btn::new("service-app-check", self.tr("Download\u{2026}"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(WINDOWS_DOWNLOAD))
            } else {
                Btn::new("service-app-check", self.tr("Check"))
                    .disabled(dim || updates::busy(&self.service.firmware.status))
                    .on_click(cx.listener(|this, _, window, cx| this.refresh_firmware(true, window, cx)))
            }
        };
        let firmware = &self.service.firmware;
        let review = match updates::firmware_step(&firmware.status) {
            FirmwareStep::Check => Btn::new("service-firmware", self.tr("Check"))
                .disabled(dim || updates::busy(&firmware.status))
                .on_click(
                    cx.listener(|this, _, window, cx| this.refresh_firmware(true, window, cx)),
                ),
            FirmwareStep::Review => Btn::new("service-firmware", self.tr("Review…"))
                .on_click(cx.listener(|this, _, window, cx| this.review_firmware(window, cx))),
        };
        ui::card(
            vec![
                Row::new()
                    .title(app)
                    .sub(app_line)
                    .right(check_app)
                    .into_any_element(),
                Row::new()
                    .title(deck)
                    .sub(firmware_sub(self.language()))
                    .right(review)
                    .into_any_element(),
            ],
            p,
        )
    }

    /// The local API: on or off, and its switch where the service has one;
    /// **Details** while it is on. While another program holds its port, a
    /// second row says so in place of the line that would call it open: the
    /// local API is the one thing that clash closes ([`api_clash`]).
    fn service_api_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let clash = api_clash(
            self.service.api.as_ref(),
            &self.state.mirror().service,
            self.language(),
        );
        let mut row = Row::new().title(self.tr("HTTP port and tokens"));
        if let Some(api) = &self.service.api {
            let held = self.service.info.as_ref().and_then(|info| info.port);
            if clash.is_none() {
                row = row.sub(api_line(api, held, self.language()));
            }
            if api.tcp != Some(false) {
                row = row.right(
                    Btn::new("service-api-details", self.tr("Details")).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.go(Screen::LocalApi, window, cx);
                        },
                    )),
                );
            }
            if let Some(on) = api.tcp {
                row = row.right(Switch::new("service-api", on).disabled(dim).on_toggle(
                    cx.listener(|this, on: &bool, window, cx| this.set_local_api(*on, window, cx)),
                ));
            }
        }
        let mut rows = vec![row.into_any_element()];
        if let Some(clash) = clash {
            rows.push(
                Row::new()
                    .lead(ui::bare(ui::icon(ic::DANGER, 17., p.warn)))
                    .title(clash)
                    .into_any_element(),
            );
        }
        ui::card(rows, p)
    }

    /// What "Check my setup" is drawn from, as the page has it now.
    fn service_setup(&self) -> Setup {
        let checks = self.service.checks.got();
        let sources = self.service.sources.as_ref();
        Setup {
            linked: self.linked(),
            this_copy: this_copy(
                self.service.executable.as_deref(),
                self.service.own_deckd.as_deref(),
            ),
            claude: self
                .service
                .claude
                .clone()
                .or_else(|| self.integrations.claude_status()),
            stale_helpers: stale_helpers(sources, self.integrations.stale_helpers()),
            codex: codex_check(checks, sources),
            legacy_agent: legacy_agent(checks),
            bluetooth: self.state.mirror().voice.permissions.bluetooth,
        }
    }

    /// The checks, **Run Again** and **Export Support File**, and where the
    /// file goes. A service without `care.checks` gets the one line instead.
    fn service_setup_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        if self.service.checks.missing() {
            return ui::card(
                vec![newer_service(self.language(), p).into_any_element()],
                p,
            );
        }
        let mut rows: Vec<AnyElement> = check_rows(&self.service_setup(), self.language())
            .into_iter()
            .map(|row| self.service_check_row(row, p).into_any_element())
            .collect();
        let buttons = h_flex()
            .gap(px(8.))
            .px(px(16.))
            .py(px(8.))
            .child(
                Btn::new("service-run-again", self.tr("Run Again"))
                    .icon(ic::REFRESH)
                    .disabled(dim)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.run_service_checks(window, cx)),
                    ),
            )
            .child(
                Btn::new("service-export", self.tr("Export Support File"))
                    .quiet()
                    .disabled(dim || self.service.exporting)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.export_support_file(window, cx)),
                    ),
            );
        // Whether the service's own redaction leaves the text said in
        // sessions out of the file, as `config.get` says it is set.
        let note = div().px(px(16.)).pb(px(12.)).child(ui::hint(
            support_note(self.service.api.as_ref(), self.language()),
            p,
        ));
        rows.push(v_flex().child(buttons).child(note).into_any_element());
        ui::card(rows, p)
    }

    /// One check: its mark and its words, the remedy under a problem, and
    /// "Not checked" beside what could not be told.
    fn service_check_row(&self, row: CheckRow, p: &Palette) -> Row {
        let mark = |path: &'static str, color| ui::bare(ui::icon(path, 17., color));
        match row.verdict {
            Verdict::Ok => Row::new()
                .lead(mark(ic::CHECK_CIRCLE, p.good))
                .title(row.title),
            Verdict::Problem => Row::new()
                .lead(mark(ic::DANGER, p.warn))
                .title(row.title)
                .when_some(row.sub, |row, sub| row.sub(sub)),
            Verdict::NotChecked => Row::new()
                .lead(ui::bare(div()))
                .title(row.title)
                .right(ui::status(self.tr("Not checked"), Tone::Gray, p)),
        }
    }

    /// What the app has added: how many things and what they weigh, and
    /// **Show All**.
    fn service_footprint_card(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let sub = match &self.service.footprint {
            Answer::Got(footprint) => Some(footprint::summary(footprint, self.language())),
            Answer::Missing => Some(
                self.tr("This needs a newer background service.")
                    .to_string(),
            ),
            Answer::Unread => None,
        };
        let row = Row::new()
            .title(self.tr("What this app has added to your Mac"))
            .when_some(sub, |row, sub| row.sub(sub))
            .right(
                Btn::new("service-footprint", self.tr("Show All"))
                    .on_click(cx.listener(|this, _, window, cx| this.show_footprint(window, cx))),
            );
        ui::card(vec![row.into_any_element()], p)
    }

    // ---- what the controls do --------------------------------------------------

    /// **Stop…**: the question first, then `service.stop`.
    pub(crate) fn ask_stop_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sheet = Sheet {
            title: self.tr("Stop the background service?").into(),
            body: self
                .tr("The deck stops working until you start it again.")
                .into(),
            confirm: self.tr("Stop").into(),
            danger: true,
        };
        self.ask_first(sheet, window, cx, |this, window, cx| {
            this.service_lifecycle(UiAction::ServiceStop, window, cx);
        });
    }

    /// `service.restart` or `service.stop`, honestly: the service answers
    /// first and goes afterwards, so a reply is the request taken, and the
    /// socket closing is what says it went (the frame shows it).
    pub(crate) fn service_lifecycle(
        &mut self,
        action: UiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stop = matches!(action, UiAction::ServiceStop);
        self.ask(action, window, cx, move |this, result, window, cx| {
            let what = if stop {
                this.tr("Stop")
            } else {
                this.tr("Restart")
            };
            match result {
                Ok(_) => this.toast(this.fmt("{0} accepted.", &[what]), window, cx),
                Err(e) if method_missing(&e) => {
                    this.toast(
                        this.tr("This needs a newer background service."),
                        window,
                        cx,
                    );
                }
                Err(e) => this.toast(
                    this.fmt("{0} failed: {1}", &[what, &e.to_string()]),
                    window,
                    cx,
                ),
            }
        });
    }

    /// Start at login: on registers the launch agent; off asks first, then
    /// unregisters it (the service stops with it). Either way the switch
    /// shows the status macOS reports afterwards, never the call's return.
    fn set_start_at_login(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if on {
            let answer = login_item::register_agent();
            self.service_agent_changed(answer, window, cx);
            return;
        }
        let sheet = Sheet {
            title: self.tr("Turn off Start at login?").into(),
            body: self
                .tr("The service stops now and does not start when you log in.")
                .into(),
            confirm: self.tr("Turn Off").into(),
            danger: true,
        };
        self.ask_first(sheet, window, cx, |this, window, cx| {
            let answer = login_item::unregister_agent();
            this.service_agent_changed(answer, window, cx);
        });
    }

    /// The launch agent's status after a change, or the reason it did not
    /// change and the status it kept.
    fn service_agent_changed(
        &mut self,
        answer: Result<LoginItem, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Ok(status) => self.service.agent = status,
            Err(why) => {
                self.service.agent = login_item::agent_status();
                self.toast(why, window, cx);
            }
        }
        cx.notify();
    }

    /// The local API's switch: the question (the service restarts to open or
    /// close its door), then `api.tcp` and the restart. While Claude Code's
    /// hooks from before the plugin still use the door, the question to
    /// turn it off offers Repair first ([`api_question`]), which is the
    /// Integrations page's own Repair of Claude Code, its plan reviewed
    /// before anything is written; the door is left as it is.
    fn set_local_api(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The installer's latest read of the Claude row: the Integrations
        // page's, which a Repair reads again, else this page's own.
        let claude = self
            .integrations
            .claude_status()
            .or_else(|| self.service.claude.clone());
        let question = api_question(
            on,
            self.service.api.as_ref(),
            claude.as_ref(),
            self.language(),
        );
        let sheet = Sheet {
            title: question.title,
            body: question.body,
            confirm: question.confirm,
            danger: question.danger,
        };
        if question.repair_first {
            self.ask_first(sheet, window, cx, |this, window, cx| {
                this.plan_integration(Verb::Repair, "claude", window, cx);
            });
            return;
        }
        self.ask_first(sheet, window, cx, move |this, window, cx| {
            this.ask(
                UiAction::ConfigSet {
                    key: "api.tcp".into(),
                    value: ConfigValue::Bool(on),
                },
                window,
                cx,
                |this, result, window, cx| match result {
                    Ok(_) => this.service_lifecycle(UiAction::ServiceRestart, window, cx),
                    Err(e) if method_missing(&e) => {
                        this.toast(
                            this.tr("This needs a newer background service."),
                            window,
                            cx,
                        );
                    }
                    Err(e) => this.toast(
                        this.fmt("{0} failed: {1}", &[this.tr("Local API"), &e.to_string()]),
                        window,
                        cx,
                    ),
                },
            );
        });
    }

    /// **Run Again**: every check read afresh.
    fn run_service_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.staged {
            return;
        }
        self.service.agent = login_item::agent_status();
        self.read_service_setup(window, cx);
    }

    /// **Export Support File**: the service writes it to the Desktop,
    /// redacted as its diagnostics redact (the page's note says whether the
    /// text said in sessions is in it, [`support_note`]), and says where.
    fn export_support_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.exporting {
            return;
        }
        self.service.exporting = true;
        let action = UiAction::CareSupportFile {
            to: "desktop".into(),
        };
        self.ask(action, window, cx, |this, result, window, cx| {
            this.service.exporting = false;
            match result {
                Ok(reply) => {
                    let path = reply
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = Path::new(path).file_name().map_or_else(
                        || path.to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    this.toast(this.fmt("Saved to the Desktop: {0}", &[&name]), window, cx);
                }
                Err(e) if method_missing(&e) => {
                    this.toast(
                        this.tr("This needs a newer background service."),
                        window,
                        cx,
                    );
                }
                Err(e) => this.toast(
                    this.fmt(
                        "{0} failed: {1}",
                        &[this.tr("Export Support File"), &e.to_string()],
                    ),
                    window,
                    cx,
                ),
            }
        });
    }
}

/// The line a section shows in place of what an older service cannot
/// answer.
fn newer_service(language: Language, p: &Palette) -> Row {
    Row::new()
        .lead(ui::bare(ui::icon(ic::INFO, 17., p.text2)))
        .title(language.text("This needs a newer background service."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scenes' footprint is the fixture's, re-homed: every path the page
    /// prints starts at `~`, and the board's sizes come out as the board
    /// writes them. The Keychain item is no path and has no size, and the
    /// count is the board's ten.
    #[cfg(not(windows))] // the boards are a Mac's, re-homed under its HOME
    #[test]
    fn the_boards_footprint_is_the_fixtures_under_this_macs_home() {
        let footprint = board_footprint();
        let rows = footprint::items(&footprint, Language::English);
        assert_eq!(rows.len(), 10);
        for row in &rows {
            if let footprint::Place::Path(path) = &row.place {
                assert!(path.starts_with('~'), "{path}");
            }
        }
        let sizes: Vec<Option<&str>> = rows.iter().map(|row| row.size.as_deref()).collect();
        assert_eq!(
            sizes,
            [
                None,
                Some("13 hooks"),
                Some("7 entries"),
                Some("1.6 GB"),
                Some("428 MB"),
                Some("9 MB"),
                Some("180 KB"),
                Some("4 KB"),
                None,
                Some("77 MB"),
            ]
        );
        assert_eq!(
            footprint::summary(&footprint, Language::English),
            "10 items, 2.1 GB. Most can be removed one at a time."
        );
    }

    /// Every fixture a scene is drawn from parses into what the scene shows.
    #[test]
    fn the_scenes_fixtures_read_as_the_board() {
        let checks = Checks::parse(&fixture(CARE_CHECKS));
        let sources = Sources::parse(&fixture(SOURCES_LIST));
        assert_eq!(
            codex_check(Some(&checks), Some(&sources)),
            CodexCheck::Reachable
        );
        assert_eq!(legacy_agent(Some(&checks)), Some(true));
        assert_eq!(stale_helpers(Some(&sources), 0), 2);
        let mut config = fixture(CONFIG_GET);
        assert_eq!(Api::parse(&config).tcp, Some(true));
        config["api"]["tcp"] = false.into();
        assert_eq!(Api::parse(&config).tcp, Some(false));
        assert!(updates::available(&board_firmware()).is_some());
        let tokens = board_tokens();
        let lines: Vec<String> = tokens
            .iter()
            .map(|token| {
                crate::local_api::token_life_line(
                    token,
                    crate::shots::board_now(),
                    Language::English,
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                "created 2026-11-02, 09:12 · no expiry",
                "created yesterday, 16:40 · expires 2027-02-10, 16:40",
            ]
        );
    }

    /// The note under the support file says what the service's redaction
    /// does as it is set: the text said in sessions is left out unless
    /// content logging is on, and the default while `config.get` has not
    /// said.
    #[test]
    fn the_support_file_note_follows_content_logging() {
        crate::i18n::install();
        let left_out = "The file is saved to the Desktop. Session text is left out.";
        let mut config = fixture(CONFIG_GET);
        assert_eq!(Api::parse(&config).log_content, None);
        assert_eq!(support_note(Some(&Api::parse(&config)), Language::English), left_out);
        assert_eq!(support_note(None, Language::English), left_out);
        config["privacy"] = serde_json::json!({"log_content": false});
        assert_eq!(support_note(Some(&Api::parse(&config)), Language::English), left_out);
        config["privacy"]["log_content"] = true.into();
        assert_eq!(Api::parse(&config).log_content, Some(true));
        assert_eq!(
            support_note(Some(&Api::parse(&config)), Language::English),
            "The file is saved to the Desktop. Session text is kept in it."
        );
        assert_eq!(
            support_note(Some(&Api::parse(&config)), Language::Japanese),
            "デスクトップに保存され、セッションの文章も含まれます。"
        );
    }
}
