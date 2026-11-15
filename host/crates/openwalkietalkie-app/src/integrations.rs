//! The Integrations page (board Integrations): what puts sessions on the
//! deck — Claude Code, the Claude Channels bridge, Codex, Codex Desktop and
//! the apps that connect by themselves — and whether each one is arriving,
//! as counts and times and never a session.
//!
//! **Three answers.** The installer says whether a plugin or a launcher is
//! on disk and current (`integrations.status`); the service says what each
//! source has on the deck and when it was last heard from (`sources.list`);
//! its settings say which source is switched on, whether everything is
//! paused, and whether a connected app's own cloud hears its chats
//! (`config.get`). None of them folds into the mirror, so the pane keeps the
//! answers ([`Facts`]) and asks again: the sources every five seconds while
//! the page shows (every half minute while it does not, for the old-helper
//! count the Deck page and the menu read), the settings on each connection
//! whatever page shows (the menu's **Pause Integrations** ticks from them),
//! whenever the page opens (a switch another page moved shows here) and
//! after every write, the installer when the page opens, on each connection
//! and after it writes. A switch this service does not manage is not drawn,
//! and a method it does not have says so in place of what it would have
//! shown.
//!
//! **A source's own switch and the pause are told apart.** A source switched
//! off on its own row is **Off**; **Paused** is only ever the one switch
//! that pauses everything (or the older `hooks.enabled = false`). The count
//! is what holds a place on the deck, with every session the source has on
//! hover when that is more.
//!
//! **Nothing is written without the plan.** Set Up and Repair, and
//! Footprint's Remove…, ask for a dry run first and show it in the plan
//! dialog; **Write these changes** applies the plan the owner is looking at.
//! A run is not reported as done when it left a command for the owner to
//! run, or when the installer, read again, still does not say what the run
//! was for (the one rule first run follows too, [`present::finished`]): its
//! command is shown with **Copy Command** in place of the success line.
//!
//! The words are pure functions of those answers and a language ([`tools`],
//! [`apps`], and the test question's [`present::test_line`]), so a test
//! reads every row in every state without a window.
use crate::present::{self, Leftover, TestMark};
use crate::shell::{Screen, Shell};
use crate::shots::{self, Scene};
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Press, Row, Switch, Type as _};
use deck_gui_core::admin::{method_missing, Plan, Report, Status, Verb};
use deck_gui_core::care::{
    self, keys, Handoff, QuestionTest, Source, SourceState, Sources, SourcesConfig, TestState,
    TestStatus, Unit,
};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Tone, Ts};
use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, Hsla, SharedString, Subscription, Window,
};
use gpui_component::{h_flex, v_flex};
use serde_json::{json, Value};
use std::path::Path;

/// The table's columns (the board's, GEN `integrations`): the source's tile,
/// then after the name the count on the deck, the last event, the hand-off
/// and the state, and the row's control at the right end.
const TILE: f32 = 32.;
const COUNT: f32 = 88.;
const AGE: f32 = 80.;
const HANDOFF: f32 = 96.;
const STATE: f32 = 112.;
const CONTROL: f32 = 96.;
/// The hand-off and the state stand this far in from their columns' edge.
const CELL_INSET: f32 = 8.;
/// The head's and the foot's sides, a row's own.
const SIDE: f32 = 16.;
/// A row is at least this tall, its rule included: the board's rows carry
/// two sub-lines and come out a pixel taller with their rule.
const ROW_HEIGHT: f32 = 64.;

/// The page drawn while the service is not answering.
const DIMMED: f32 = 0.55;

/// The page's dialogs: the installer's plan, how a Claude session joins the
/// Channels bridge, and the command a run left to the owner.
const PLAN: &str = "plan";
const PLAN_WIDTH: f32 = 520.;
const CONNECT: &str = "connect-claude";
const CONNECT_WIDTH: f32 = 460.;
const NEXT_STEP: &str = "next-step";
const NEXT_STEP_WIDTH: f32 = 520.;

/// How often `sources.list` is asked while the page shows, and while it
/// does not (the old-helper count the Deck page and the menu read).
const SOURCES_EVERY: u64 = 5;
const BACKGROUND_EVERY: u64 = 30;

/// The harness's scenes that are this page's.
const INTEGRATIONS_SCENES: [Scene; 4] = [
    Scene::Integrations,
    Scene::IntegrationsPaused,
    Scene::IntegrationsRepair,
    Scene::IntegrationsPlan,
];

// ---- what the page says, as pure functions ---------------------------------

/// What the page has been told, each answer as it last came.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// `sources.list`; `None` until it has answered.
    pub sources: Option<Sources>,
    /// `config.get`'s switches; `None` until it has answered.
    pub config: Option<SourcesConfig>,
    /// `integrations.status`'s rows.
    pub reports: Vec<Report>,
    /// `integrations.status`'s count of refused helpers, the one a service
    /// from before `sources.list` gives.
    pub channels_stale: u32,
}

impl Facts {
    /// The source with this id.
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.as_ref()?.get(id)
    }

    /// The installer's row for `target`.
    pub fn report(&self, target: &str) -> Option<&Report> {
        self.reports.iter().find(|report| report.target == target)
    }

    /// What **Pause integrations** shows, the menu's item too: paused by
    /// its own switch or by the older `hooks.enabled = false`. `None` until
    /// a service with the switch has answered.
    pub fn paused(&self) -> Option<bool> {
        self.config.as_ref()?.effective_pause()
    }

    /// Every source paused by the one switch (`sources.paused`), as the
    /// settings say, or the sources while the settings have not.
    pub fn all_paused(&self) -> bool {
        self.config
            .as_ref()
            .and_then(|config| config.paused)
            .or_else(|| self.sources.as_ref().map(|sources| sources.paused))
            .unwrap_or(false)
    }

    /// The older pause, which stops the hooks alone.
    pub fn hooks_off(&self) -> bool {
        self.config.as_ref().and_then(|config| config.hooks_enabled) == Some(false)
    }

    /// Claude sessions on a helper the service refuses.
    pub fn stale_helpers(&self) -> u32 {
        self.source(Source::CLAUDE_CHANNELS)
            .and_then(|source| source.stale)
            .unwrap_or(self.channels_stale)
    }

    /// Whether the Claude Code plugin carries the Channels helper: as the
    /// service resolves its switch, and on where it has none (every plugin
    /// such a service installs carries it).
    pub fn channels_on(&self) -> bool {
        self.config
            .as_ref()
            .and_then(|config| config.claude_channels)
            .unwrap_or(true)
    }

    /// Whether a connected app that transcribes its own chats hears the
    /// presses meant for them (`voice.stt.companion`, the Voice page's
    /// Conol switch): as the settings say, else on, as a service with no
    /// such switch always has it.
    pub fn companion(&self) -> bool {
        self.config
            .as_ref()
            .and_then(|config| config.stt_companion)
            .unwrap_or(true)
    }

    /// A switch's value, when this service manages it; `None` draws no
    /// switch.
    pub fn switch(&self, toggle: &Toggle) -> Option<bool> {
        let config = self.config.as_ref().filter(|config| config.managed())?;
        match toggle {
            Toggle::Claude => config.claude,
            Toggle::ClaudeChannels => config.claude_channels,
            Toggle::Codex => config.codex_daemon,
            Toggle::App(name) => Some(!config.apps_paused.iter().any(|paused| paused == name)),
        }
    }
}

/// A row's state: a word in a tone. Green and gray are a dot and the word,
/// amber a pill (`ui::status`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub word: String,
    pub tone: Tone,
}

impl State {
    fn new(word: impl Into<String>, tone: Tone) -> State {
        State {
            word: word.into(),
            tone,
        }
    }

    /// The source's own switch is off.
    fn off(language: Language) -> State {
        State::new(language.text("Off"), Tone::Gray)
    }

    /// Everything is paused, by **Pause integrations** (or, for Claude
    /// Code's hooks, by the older `hooks.enabled = false`).
    fn paused(language: Language) -> State {
        State::new(language.text("Paused"), Tone::Gray)
    }

    fn connected(language: Language) -> State {
        State::new(language.text("Connected"), Tone::Green)
    }
}

/// A row's switch, by what it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// `sources.claude`.
    Claude,
    /// `sources.claude_channels`; the plugin is then repaired to match.
    ClaudeChannels,
    /// `codex.daemon`.
    Codex,
    /// The app's name in `sources.apps_paused`.
    App(String),
}

/// What a row offers at its right end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    None,
    /// The source's own switch, on or off.
    Switch(Toggle, bool),
    /// **Set Up**: the installer's plan to install `target`.
    SetUp(&'static str),
    /// **Repair**, the primary: the installer's plan to repair `target`,
    /// with what is wrong in its tooltip.
    Repair(&'static str, String),
}

impl Control {
    /// The switch, when the service manages it.
    fn switch(toggle: Toggle, on: Option<bool>) -> Control {
        on.map_or(Control::None, |on| Control::Switch(toggle, on))
    }
}

/// One row of a table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The source's id or the installer's target, which the row's controls
    /// are named by.
    pub id: String,
    pub name: String,
    pub icon: &'static str,
    /// Under the name; a `\n` breaks it where the board does.
    pub sub: String,
    /// A row with no count, time or hand-off cells.
    pub wide: bool,
    /// What holds a place on the deck, in the source's unit.
    pub count: Option<String>,
    /// The count's hint: every session the source has, when that is more
    /// than holds a place (it is paused, or the deck is full).
    pub total: Option<String>,
    pub age: Option<String>,
    pub handoff: Option<String>,
    pub state: Option<State>,
    pub control: Control,
    /// **How to Connect…** beside the name: the Claude Channels row while
    /// the bridge is on.
    pub connect: bool,
}

/// A source's count, in its own unit: `1 session`, `3 sessions`, `2
/// threads`, `7 chats`. A unit this build does not know counts sessions.
pub fn count_words(count: u32, unit: &Unit, language: Language) -> String {
    let n = count.to_string();
    match (unit, count) {
        (Unit::Threads, 1) => language.text("1 thread").to_string(),
        (Unit::Threads, _) => language.format("{0} threads", &[&n]),
        (Unit::Chats, 1) => language.text("1 chat").to_string(),
        (Unit::Chats, _) => language.format("{0} chats", &[&n]),
        (_, 1) => language.text("1 session").to_string(),
        (_, _) => language.format("{0} sessions", &[&n]),
    }
}

/// Every session a source has, as the count's hint says it: `7 chats in
/// all`.
pub fn total_words(sessions: u32, unit: &Unit, language: Language) -> String {
    language.format("{0} in all", &[&count_words(sessions, unit, language)])
}

/// When a source was last heard from: `4 s ago`, or a dash for never.
pub fn age_words(at: Option<Ts>, now: Ts, language: Language) -> String {
    at.map_or_else(
        || "\u{2014}".to_string(),
        |at| present::ago(at, now, language),
    )
}

/// What a press on the deck does to bring a source's session forward: the
/// service raises a Claude Code session's app (`app`), Codex has nothing yet
/// (`none`) and a connected app does it itself (`self`). `terminal` is what
/// a 0.13 service says of Claude Code, which could focus a tab only in
/// Ghostty, so its words say so. A word this build does not know is shown
/// as it came.
pub fn handoff_words(handoff: &Handoff, language: Language) -> String {
    match handoff {
        Handoff::Terminal => language.text("Focuses its Ghostty tab").to_string(),
        Handoff::App => language.text("Raises its app").to_string(),
        Handoff::NotYet => language.text("Not yet").to_string(),
        Handoff::Itself => language.text("By itself").to_string(),
        Handoff::Unknown(word) => word.clone(),
    }
}

/// What the installer's status makes of a row, while it is not installed:
/// not set up, with **Set Up**, or anything else in need of **Repair**.
/// `None` once it is installed, and the source says the rest.
fn installer(
    target: &'static str,
    status: &Status,
    language: Language,
) -> Option<(State, Control)> {
    match status {
        Status::Installed => None,
        Status::Absent => Some((
            State::new(language.text("Not set up"), Tone::Gray),
            Control::SetUp(target),
        )),
        trouble => Some((
            State::new(language.text("Needs repair"), Tone::Amber),
            Control::Repair(
                target,
                present::status_help_localized(trouble, language).to_string(),
            ),
        )),
    }
}

/// What a source's row says in its count, time and hand-off cells.
#[derive(Default)]
struct Figures {
    count: Option<String>,
    total: Option<String>,
    age: Option<String>,
    handoff: Option<String>,
}

/// A source's figures, as far as it has said: what it has on the deck
/// (the column is "On the deck"), every session it has when that is more,
/// its last event and its hand-off.
fn figures(source: Option<&Source>, now: Ts, language: Language) -> Figures {
    let Some(source) = source else {
        return Figures::default();
    };
    Figures {
        count: Some(count_words(source.on_deck, &source.unit, language)),
        total: (source.sessions > source.on_deck)
            .then(|| total_words(source.sessions, &source.unit, language)),
        age: Some(age_words(source.last_event_at, now, language)),
        handoff: source
            .handoff
            .as_ref()
            .map(|handoff| handoff_words(handoff, language)),
    }
}

/// A source's own state word: connected, waiting (in the row's own words),
/// off, or a word this build does not know.
fn source_state(state: &SourceState, waiting: &str, language: Language) -> State {
    match state {
        SourceState::Connected => State::connected(language),
        SourceState::Waiting => State::new(waiting, Tone::Gray),
        SourceState::Off => State::off(language),
        SourceState::Unknown(word) => State::new(word.as_str(), Tone::Gray),
    }
}

/// What a row whose source is otherwise fine says, before its own state:
/// **Off** when its own switch is, **Paused** while everything is (`paused`).
fn held(on: bool, paused: bool, language: Language) -> Option<State> {
    if !on {
        Some(State::off(language))
    } else if paused {
        Some(State::paused(language))
    } else {
        None
    }
}

/// The sessions no row counts (a Gemini or an MCP adapter's), said under the
/// Agent tools card while there are any.
pub fn others_line(facts: &Facts, language: Language) -> Option<String> {
    match facts.sources.as_ref()?.other {
        0 => None,
        1 => Some(
            language
                .text("1 other session, from MCP or Gemini.")
                .to_string(),
        ),
        other => Some(language.format(
            "{0} other sessions, from MCP or Gemini.",
            &[&other.to_string()],
        )),
    }
}

/// What first run's card says before a row's control: the row's state,
/// except beside the row's own switch while it is off, whose word already
/// says **Off** ("Off Off" otherwise). The page's table keeps both: there
/// the state has a column of its own.
pub fn card_state(line: &Line) -> Option<&State> {
    line.state
        .as_ref()
        .filter(|_| !matches!(line.control, Control::Switch(_, false)))
}

/// The Agent tools card's rows, in the board's order: Claude Code, Claude
/// Channels, Codex CLI, Codex Desktop.
pub fn tools(facts: &Facts, now: Ts, language: Language) -> Vec<Line> {
    let mut lines = vec![
        claude_line(facts, now, language),
        channels_line(facts, language),
        codex_line(facts, now, language),
    ];
    // The Codex Desktop launcher is a macOS app bundle; Windows has none.
    if cfg!(not(windows)) {
        lines.push(desktop_line(facts, language));
    }
    lines
}

/// Claude Code: the plugin's state from the installer, its sessions from
/// the service, and its switch. Off by its own switch; paused by the pause
/// of everything, or by the older `hooks.enabled = false`, which stops its
/// hooks.
fn claude_line(facts: &Facts, now: Ts, language: Language) -> Line {
    let source = facts.source(Source::CLAUDE);
    let switch = facts.switch(&Toggle::Claude);
    let on = switch.unwrap_or(true) && source.is_none_or(|source| source.enabled);
    let figures = figures(source, now, language);
    let control = Control::switch(Toggle::Claude, switch);
    let paused = facts.all_paused() || facts.hooks_off();
    let (state, control) = match facts.report("claude").map(|report| &report.status) {
        None => (None, control),
        Some(status) => match installer("claude", status, language) {
            Some((state, trouble)) => (Some(state), trouble),
            None => (
                Some(held(on, paused, language).unwrap_or_else(|| State::connected(language))),
                control,
            ),
        },
    };
    let sub = source
        .and_then(|source| source.hooks)
        .map(|hooks| {
            let hooks = hooks.to_string();
            if facts.channels_on() {
                language.format("Plugin: {0} hooks and\nthe Channels helper.", &[&hooks])
            } else {
                language.format("Plugin: {0} hooks.", &[&hooks])
            }
        })
        .unwrap_or_default();
    Line {
        id: Source::CLAUDE.into(),
        name: "Claude Code".into(),
        icon: ic::CLAUDE,
        sub,
        wide: false,
        count: figures.count,
        total: figures.total,
        age: figures.age,
        handoff: figures.handoff,
        state,
        control,
        connect: false,
    }
}

/// Claude Channels: no count of its own. Its own switch comes first (off is
/// how a new install leaves it, decision 13, and is not a pause), then
/// sessions still on an old helper (they need a restart whatever else is
/// true), then the pause, then whether a session is connected; **How to
/// Connect…** while it is on.
fn channels_line(facts: &Facts, language: Language) -> Line {
    let source = facts.source(Source::CLAUDE_CHANNELS);
    let switch = facts.switch(&Toggle::ClaudeChannels);
    let on = switch.unwrap_or(true) && source.is_none_or(|source| source.enabled);
    let stale = facts.stale_helpers();
    let mut sub = language
        .text("Voice orders arrive as your own message, not as another session's.")
        .to_string();
    match stale {
        0 => {}
        1 => {
            sub.push('\n');
            sub.push_str(
                language
                    .text("1 session started before the last update still uses the old helper."),
            );
        }
        n => {
            sub.push('\n');
            sub.push_str(&language.format(
                "{0} sessions started before the last update still use the old helper.",
                &[&n.to_string()],
            ));
        }
    }
    let state = if !on {
        Some(State::off(language))
    } else if stale == 1 {
        Some(State::new(language.text("Restart 1 session"), Tone::Amber))
    } else if stale > 1 {
        Some(State::new(
            language.format("Restart {0} sessions", &[&stale.to_string()]),
            Tone::Amber,
        ))
    } else if facts.all_paused() {
        Some(State::paused(language))
    } else {
        source
            .and_then(|source| source.state.as_ref())
            .map(|state| source_state(state, language.text("No session connected"), language))
    };
    Line {
        id: Source::CLAUDE_CHANNELS.into(),
        name: "Claude Channels".into(),
        icon: ic::MICROPHONE,
        sub,
        wide: true,
        count: None,
        total: None,
        age: None,
        handoff: None,
        state,
        control: Control::switch(Toggle::ClaudeChannels, switch),
        connect: on,
    }
}

/// Codex CLI: everything from the service, through Codex itself, with
/// nothing installed — unless the hooks of an older setup are still in
/// Codex's own file (the installer's `codex` target). Those still put Codex
/// sessions on the deck whatever this row's switch says (it is
/// `codex.daemon`, the connection through Codex itself), so the row says so
/// and where they are removed.
fn codex_line(facts: &Facts, now: Ts, language: Language) -> Line {
    let source = facts.source(Source::CODEX);
    let switch = facts.switch(&Toggle::Codex);
    let on = switch.unwrap_or(true) && source.is_none_or(|source| source.enabled);
    let figures = figures(source, now, language);
    let state = source
        .and_then(|source| source.state.as_ref())
        .map(|state| {
            held(on, facts.all_paused(), language).unwrap_or_else(|| {
                source_state(state, language.text("Codex is not running"), language)
            })
        });
    let sub = match facts.report("codex").map(|report| &report.status) {
        Some(Status::Installed) => language.text(
            "Through Codex itself.\nLeftover hooks also reach the deck: remove them in Footprint.",
        ),
        // On disk and not as the installer wrote them (outdated, modified,
        // untrusted…): whether Codex still runs them, the row cannot say.
        Some(status) if status.installed() => language.text(
            "Through Codex itself.\nLeftover hooks are still on this Mac: remove them in \
             Footprint.",
        ),
        _ => language.text("Through Codex itself.\nNothing is installed."),
    };
    Line {
        id: Source::CODEX.into(),
        name: "Codex CLI".into(),
        icon: ic::OPENAI,
        sub: sub.to_string(),
        wide: false,
        count: figures.count,
        total: figures.total,
        age: figures.age,
        handoff: figures.handoff,
        state,
        control: Control::switch(Toggle::Codex, switch),
        connect: false,
    }
}

/// Codex Desktop: a launcher the installer writes, so its state is the
/// installer's. Its sessions arrive through Codex and are counted there.
fn desktop_line(facts: &Facts, language: Language) -> Line {
    const TARGET: &str = "codex-desktop";
    let (state, control) = match facts.report(TARGET).map(|report| &report.status) {
        None => (None, Control::None),
        Some(Status::Installed) => (
            Some(State::new(language.text("Installed"), Tone::Green)),
            Control::None,
        ),
        Some(status) => installer(TARGET, status, language)
            .map_or((None, Control::None), |(state, control)| {
                (Some(state), control)
            }),
    };
    Line {
        id: TARGET.into(),
        name: "Codex Desktop".into(),
        icon: ic::MONITOR,
        sub: language
            .text(
                "Open \u{201c}Codex with OpenWalkieTalkie\u{201d} instead of Codex.\nIt is in the \
                 Applications folder of your home folder.",
            )
            .to_string(),
        wide: true,
        count: None,
        total: None,
        age: None,
        handoff: None,
        state,
        control,
        connect: false,
    }
}

/// The Connected apps card's rows, in the service's order. None until it
/// has answered.
pub fn apps(facts: &Facts, now: Ts, language: Language) -> Vec<Line> {
    let Some(sources) = &facts.sources else {
        return Vec::new();
    };
    sources
        .apps()
        .map(|app| app_line(app, facts, now, language))
        .collect()
}

/// A connected app, named by itself: what it does in one sentence, its
/// count, its last event, and its own switch. Whether an app that
/// transcribes its own chats hears them is the Voice page's switch for it
/// (`voice.stt.companion`): off, its chats are heard on this Mac.
fn app_line(app: &Source, facts: &Facts, now: Ts, language: Language) -> Line {
    let client = app.app_name().unwrap_or(&app.name).to_string();
    let toggle = Toggle::App(client);
    let switch = facts.switch(&toggle);
    let on = app.enabled && switch.unwrap_or(true);
    let sub = if app.does("voice") && facts.companion() {
        language.format("Sessions. Voice for its\nchats: {0}'s cloud.", &[&app.name])
    } else if app.does("voice") {
        language
            .text("Sessions. Its chats are\nheard on this Mac.")
            .to_string()
    } else if app.does("face") {
        language
            .text("Sessions, the Omarchy\nface and its keys.")
            .to_string()
    } else {
        language.text("Sessions.").to_string()
    };
    let figures = figures(Some(app), now, language);
    let state = held(on, facts.all_paused(), language).or_else(|| {
        app.state
            .as_ref()
            .map(|state| source_state(state, language.text("Waiting"), language))
    });
    Line {
        id: app.id.clone(),
        name: app.name.clone(),
        icon: app_icon(&app.name),
        sub,
        wide: false,
        count: figures.count,
        total: figures.total,
        age: figures.age,
        handoff: figures.handoff,
        state,
        control: Control::switch(toggle, switch),
        connect: false,
    }
}

/// An app's mark: Conol's own, a terminal for the Omarchy remote, a plug
/// for any other.
pub fn app_icon(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if name == "conol" {
        ic::CONOL
    } else if name.starts_with("omarchy") {
        ic::TERMINAL
    } else {
        ic::PLUG
    }
}

/// What turning a row's switch on or off writes. An app's switch writes the
/// whole list of paused apps, every other name in it kept.
pub fn toggle_action(toggle: &Toggle, on: bool, config: &SourcesConfig) -> UiAction {
    let (key, value) = match toggle {
        Toggle::Claude => (keys::CLAUDE, ConfigValue::Bool(on)),
        Toggle::ClaudeChannels => (keys::CLAUDE_CHANNELS, ConfigValue::Bool(on)),
        Toggle::Codex => (keys::CODEX_DAEMON, ConfigValue::Bool(on)),
        Toggle::App(name) => (
            keys::APPS_PAUSED,
            ConfigValue::List(apps_paused_after(&config.apps_paused, name, on)),
        ),
    };
    UiAction::ConfigSet {
        key: key.to_string(),
        value,
    }
}

/// The paused apps once `name` is switched `on` (out of the list) or off
/// (into it), every other name kept in its place.
pub fn apps_paused_after(paused: &[String], name: &str, on: bool) -> Vec<String> {
    let mut list: Vec<String> = paused
        .iter()
        .filter(|paused| paused.as_str() != name)
        .cloned()
        .collect();
    if !on {
        list.push(name.to_string());
    }
    list
}

/// What a run left, as its dialog shows it: the one command that finishes
/// it, when the service left a command to run, and the service's warnings
/// as it wrote them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NextStep {
    pub command: Option<String>,
    pub warnings: Vec<String>,
}

impl NextStep {
    /// For a `verb` on `target` that left `leftover`, with this app's
    /// `deckd` where there is one beside it ([`present::finish_command`]).
    pub fn of(leftover: Leftover, verb: Verb, target: &str, deckd: Option<&Path>) -> NextStep {
        NextStep {
            command: (!leftover.commands.is_empty())
                .then(|| present::finish_command(deckd, verb, target, &leftover.commands)),
            warnings: leftover.warnings,
        }
    }
}

/// The success line of an applied run, from the receipt it answered.
pub fn receipt_line(what: &str, receipt: &Value, language: Language) -> String {
    let count = |key: &str| {
        receipt
            .get(key)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
            .to_string()
    };
    language.format(
        "{0}: {1} file(s) written, {2} backed up.",
        &[what, &count("changed"), &count("backups")],
    )
}

/// A change of the plan, in a word: create, update, remove, or the
/// installer's own word for one this build does not know.
pub fn change_word(action: &str, language: Language) -> String {
    match action {
        "create" => language.text("create"),
        "update" => language.text("update"),
        "remove" => language.text("remove"),
        "restore" => language.text("restore"),
        other => other,
    }
    .to_string()
}

/// The `claude_channels_stale` count of an `integrations.status` answer.
pub fn channels_stale(value: &Value) -> u32 {
    value
        .get("claude_channels_stale")
        .and_then(Value::as_u64)
        .and_then(|stale| u32::try_from(stale).ok())
        .unwrap_or(0)
}

// ---- the pane ----------------------------------------------------------------

/// A plan the owner has been shown and has not yet accepted.
#[derive(Clone, Debug)]
pub struct Pending {
    pub verb: Verb,
    pub target: String,
    pub trust: bool,
    pub plan: Plan,
}

/// A run applied and waiting for the installer to be read again.
#[derive(Clone, Debug)]
struct Applied {
    verb: Verb,
    target: String,
    /// "Repair — Claude Code".
    what: String,
    /// What it says when it is done.
    line: String,
    leftover: Leftover,
    /// The read it waits for.
    ticket: u64,
}

/// One kind of read, by ticket: an answer that a later request's answer
/// overtook is dropped, and no poll goes while one is out.
#[derive(Clone, Copy, Debug, Default)]
struct Reads {
    asked: u64,
    seen: u64,
}

impl Reads {
    /// A request's ticket.
    fn ask(&mut self) -> u64 {
        self.asked += 1;
        self.asked
    }

    /// Whether a request is still out.
    fn pending(self) -> bool {
        self.seen < self.asked
    }

    /// The answer to `ticket` came: whether it is newer than every answer
    /// before it, and so is kept.
    fn answered(&mut self, ticket: u64) -> bool {
        let newer = ticket > self.seen;
        self.seen = self.seen.max(ticket);
        newer
    }

    /// Drop every answer still on its way: what was written since makes
    /// them stale.
    fn forget(&mut self) {
        self.seen = self.asked;
    }
}

/// The page's own state, which the shell owns: the answers it was given,
/// the plan in its dialog, and what a run left to do.
#[derive(Default)]
pub struct IntegrationsPane {
    facts: Facts,
    /// The last test question.
    test: Option<QuestionTest>,
    /// The service has no `sources.list`, or no `care.test.*`.
    sources_missing: bool,
    test_missing: bool,
    installer_reads: Reads,
    sources_reads: Reads,
    config_reads: Reads,
    test_reads: Reads,
    /// The installer has answered a full read (`detect`) since the window
    /// opened.
    asked: bool,
    /// The connection as the pane last saw it.
    connected: bool,
    /// A plan or a run on its way.
    busy: bool,
    /// The pause being written.
    pausing: bool,
    /// The switch being written.
    writing: Option<Toggle>,
    /// The plan in the dialog, kept so **Write these changes** applies the
    /// plan the owner is looking at.
    pending: Option<Pending>,
    /// The plan dialog is up: a re-plan (the trust switch) keeps it up.
    plan_open: bool,
    applied: Option<Applied>,
    /// What the last run left to the owner, in its dialog.
    next_step: Option<NextStep>,
}

impl IntegrationsPane {
    /// The pane, and the subscriptions its text fields need kept alive: it
    /// has no text field.
    pub(crate) fn new(
        _window: &mut Window,
        _cx: &mut Context<Shell>,
    ) -> (IntegrationsPane, Vec<Subscription>) {
        (IntegrationsPane::default(), Vec::new())
    }

    /// Claude sessions still on a helper from before the last update.
    pub(crate) fn stale_helpers(&self) -> u32 {
        self.facts.stale_helpers()
    }

    /// The Claude Code plugin's installer status, once read.
    pub(crate) fn claude_status(&self) -> Option<Status> {
        self.status_of(Source::CLAUDE)
    }

    /// Whether every source is paused, once `config.get` has said.
    pub(crate) fn paused(&self) -> Option<bool> {
        self.facts.paused()
    }

    /// The last test question as the service last reported it, which the
    /// pane keeps reading while one is on the deck: first run's last step
    /// says its line from this same read ([`present::test_line`]).
    pub(crate) fn test_question(&self) -> Option<&QuestionTest> {
        self.test.as_ref()
    }

    /// The service is older than the test question (`-32601`).
    pub(crate) fn test_missing(&self) -> bool {
        self.test_missing
    }

    /// Whether the trust switch is on for this target and verb. It is kept
    /// on the pending plan, so it survives a re-plan and starts off.
    fn pending_trust(&self, target: &str, verb: Verb) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.target == target && p.verb == verb && p.trust)
    }
}

impl Shell {
    /// Ask for what the page draws: the sources, the settings, the last
    /// test and the installer's files always (they move: a switch on another
    /// page, the Voice page's Conol switch among them, changes what a row
    /// says; the files are a quick read), the installer's full read (each
    /// CLI's version check) when the page has none yet, everything when
    /// `force`d.
    pub(crate) fn refresh_integrations(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let full = force || !self.integrations.asked;
        self.read_installer(full, window, cx);
        self.read_sources(force, window, cx);
        self.read_config(force, window, cx);
        self.read_test(force, window, cx);
    }

    /// The page.
    pub(crate) fn render_integrations(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let pane = &self.integrations;
        let mut header = ui::Header::new(
            self.tr(Screen::Integrations.label()),
            self.tr("What puts sessions on the deck, and whether each one is arriving."),
        );
        // The switch is drawn once a service that has it has said where it
        // is: a guess would offer to undo a pause it cannot see.
        if let Some(paused) = pane
            .facts
            .config
            .as_ref()
            .filter(|config| config.managed())
            .and(pane.facts.paused())
        {
            header = header
                .right(
                    div()
                        .ts(13., 18.)
                        .text_color(p.text2)
                        .whitespace_nowrap()
                        .child(self.tr("Pause integrations")),
                )
                .right(
                    Switch::new("integrations-pause", paused)
                        .disabled(dim || pane.pausing)
                        .on_toggle(cx.listener(|this, on: &bool, window, cx| {
                            this.set_paused(*on, window, cx);
                        })),
                );
        }
        let mut test = Btn::new("integrations-test", self.tr("Send a Test Question"))
            .disabled(dim || !self.linked() || pane.test_missing)
            .on_click(cx.listener(|this, _, window, cx| this.send_test_question(window, cx)));
        if !self.linked() {
            test = test.tooltip(self.tr("Link the deck first."));
        } else if pane.test_missing {
            test = test.tooltip(self.tr("This needs a newer background service."));
        }
        // Sessions no row counts are still said, under the card, as a count.
        let tools = v_flex()
            .gap(px(8.))
            .child(self.tools_card(dim, &p, cx))
            .children(others_line(&pane.facts, self.language()).map(|line| ui::hint(line, &p)));
        let body = v_flex()
            .gap(px(20.))
            .child(header.right(test))
            .child(self.result_lines(&p))
            .child(ui::section(self.tr("Agent tools"), tools, &p))
            .child(ui::section(
                self.tr("Connected apps"),
                self.apps_card(dim, &p, cx),
                &p,
            ))
            .when(dim, |body| body.opacity(DIMMED));
        ui::page("integrations", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// The page's dialog `id` and its width: the plan, how to connect a
    /// Claude session, and the command a run left.
    pub(crate) fn integrations_dialog(
        &self,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        match id {
            PLAN => self.render_plan(cx).map(|body| (body, PLAN_WIDTH)),
            CONNECT => Some((self.render_connect(cx), CONNECT_WIDTH)),
            NEXT_STEP => self
                .render_next_step(cx)
                .map(|body| (body, NEXT_STEP_WIDTH)),
            _ => None,
        }
    }

    /// The dialog `id` was closed: an unanswered plan is dropped, and so is
    /// what a run left once it has been read.
    pub(crate) fn integrations_dialog_closed(&mut self, id: &'static str, _cx: &mut Context<Self>) {
        match id {
            PLAN => {
                self.integrations.pending = None;
                self.integrations.plan_open = false;
            }
            NEXT_STEP => self.integrations.next_step = None,
            _ => {}
        }
    }

    /// Nothing lives outside the render: the page has no text field.
    pub(crate) fn localize_integrations(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// A connection that came: the settings (the menu's pause), the sources
    /// (the old-helper count) and the installer are read again whatever
    /// page shows, since nothing folds them into the mirror.
    pub(crate) fn on_integrations_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connected = self.state.connected();
        if connected == self.integrations.connected {
            return;
        }
        self.integrations.connected = connected;
        // Whatever the open page asked for on the same connection is
        // already out, and is not asked twice.
        if connected {
            self.read_config(false, window, cx);
            self.read_sources(false, window, cx);
            self.read_installer(false, window, cx);
        }
    }

    /// Once a second: a test on the deck is asked after, whatever shows (the
    /// page, or first run's last step), and the sources every few seconds
    /// while the page shows and now and then while it does not.
    pub(crate) fn tick_integrations(
        &mut self,
        beat: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.connected() {
            return;
        }
        if self
            .integrations
            .test
            .as_ref()
            .is_some_and(|test| test.state.pending())
        {
            self.read_test(false, window, cx);
        }
        let open = self.screen == Screen::Integrations;
        let every = if open {
            SOURCES_EVERY
        } else {
            BACKGROUND_EVERY
        };
        if beat.is_multiple_of(every) {
            self.read_sources(false, window, cx);
            if !open {
                self.read_installer(false, window, cx);
            }
        }
    }

    /// The page's scenes, staged from the boards' fixtures read by the
    /// parsers the page reads the service with. Every other staged scene
    /// gets the settings with nothing paused (the menu's **Pause
    /// Integrations** is then enabled, as the MenuBar board draws it), and
    /// the pages that read this one (Service's checks, first run's card) the
    /// answers their boards were drawn from. A live scene starts from
    /// nothing and asks the test service.
    pub(crate) fn stage_integrations(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if scene.live() {
            self.integrations = IntegrationsPane {
                connected: self.state.connected(),
                ..IntegrationsPane::default()
            };
            return false;
        }
        let ours = INTEGRATIONS_SCENES.contains(&scene);
        if ours {
            self.go(Screen::Integrations, window, cx);
        }
        // After `go`: what it asked of a detached state was answered at once,
        // and changes nothing here.
        self.integrations = staged(scene, shots::board_now());
        match scene {
            Scene::IntegrationsPlan => {
                self.integrations.plan_open = true;
                self.open_dialog(PLAN, window);
            }
            Scene::IntegrationsRepair => self.open_dialog(NEXT_STEP, window),
            _ => {}
        }
        cx.notify();
        ours
    }

    /// The card of agent tools for first run's last step: the page's rows
    /// as the kit's rows, which fit a narrower column. A row's state stands
    /// before its control, but not beside a switch that is off
    /// ([`card_state`]).
    pub(crate) fn render_agent_tools(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let rows = tools(&self.integrations.facts, present::now(), self.language())
            .iter()
            .map(|line| {
                Row::new()
                    .lead(ui::tile(line.icon, &p))
                    .title_el(self.line_title(line, "first-run", &p, cx))
                    .when(!line.sub.is_empty(), |row| row.sub(line.sub.clone()))
                    .when_some(card_state(line), |row, state| {
                        row.right(ui::status(state.word.clone(), state.tone, &p))
                    })
                    .when_some(
                        self.line_control(line, "first-run", dim, cx),
                        |row, control| row.right(control),
                    )
                    .into_any_element()
            })
            .collect();
        ui::card(rows, &p)
            .when(dim, |card| card.opacity(DIMMED))
            .into_any_element()
    }

    /// Put a test question on the deck, and ask after it until it is
    /// answered or has expired.
    pub(crate) fn send_test_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::TestQuestion,
            window,
            cx,
            |this, result, window, cx| match result {
                Ok(value) => match TestState::of_reply(&value) {
                    Some(state) => {
                        let pending = state.pending();
                        this.integrations.test = Some(QuestionTest {
                            state,
                            at: Some(present::now()),
                            after_ms: None,
                        });
                        if pending {
                            // Its record, which the tick asks after while
                            // the question is on the deck.
                            this.read_test(true, window, cx);
                        } else {
                            // Nothing went (no deck). The service records
                            // that as the last question too (`no_deck` in
                            // `care.test.status`), so the next read says the
                            // same; a read already on its way was asked
                            // before the press and would put the question
                            // before it back over this one.
                            this.integrations.test_reads.forget();
                        }
                    }
                    None => this.read_test(true, window, cx),
                },
                Err(e) if method_missing(&e) => this.integrations.test_missing = true,
                Err(e) => this.toast(
                    this.fmt(
                        "{0} failed: {1}",
                        &[this.tr("Send a test question"), &e.to_string()],
                    ),
                    window,
                    cx,
                ),
            },
        );
    }

    /// Pause every source, or resume them: what the page's switch and the
    /// menu's item both write ([`care::pause_action`]). Resuming turns the
    /// older `hooks.enabled` back on too.
    pub(crate) fn set_paused(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.integrations.pausing = true;
        self.ask(
            care::pause_action(on),
            window,
            cx,
            |this, result, window, cx| {
                this.integrations.pausing = false;
                this.written(result, window, cx);
            },
        );
    }

    /// Plan `verb` for the installer's `target` as a dry run and show the
    /// plan in the page's dialog, where nothing is written until it is
    /// confirmed. Footprint's **Remove…** comes here too.
    pub(crate) fn plan_integration(
        &mut self,
        verb: Verb,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = target.to_string();
        let trust = verb.takes_trust() && self.integrations.pending_trust(&target, verb);
        self.integrations.busy = true;
        let action = UiAction::Integrations {
            verb,
            target: target.clone(),
            trust,
            dry_run: true,
        };
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.integrations.busy = false;
            match result {
                // A service that ignored `dry_run` has written already: there
                // is nothing to preview, and saying so is the only honest
                // answer.
                Ok(value) if !Plan::is_dry_run(&value) => {
                    if this.integrations.plan_open {
                        this.close_dialog(window, cx);
                    }
                    this.toast(
                        this.tr(
                            "This daemon answered that call without honouring dry_run; nothing \
                             is being previewed.",
                        ),
                        window,
                        cx,
                    );
                    this.read_installer(false, window, cx);
                }
                Ok(value) => {
                    let plan = Plan::parse(&value);
                    this.integrations.pending = Some(Pending {
                        verb,
                        target,
                        trust,
                        plan,
                    });
                    if !this.integrations.plan_open {
                        this.integrations.plan_open = true;
                        this.open_dialog(PLAN, window);
                    }
                }
                Err(e) => this.toast(
                    this.fmt("Could not plan that: {0}", &[&present::reason(&e)]),
                    window,
                    cx,
                ),
            }
        });
    }

    // ---- the reads ---------------------------------------------------------

    /// `integrations.status`: `detect` runs each CLI's version check, which
    /// only the page's own full read does.
    fn read_installer(&mut self, detect: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.integrations.installer_reads.pending() {
            return;
        }
        let ticket = self.integrations.installer_reads.ask();
        self.ask(
            UiAction::IntegrationsStatus { detect },
            window,
            cx,
            move |this, result, window, cx| {
                let pane = &mut this.integrations;
                let newer = pane.installer_reads.answered(ticket);
                let read = match result {
                    Ok(value) => {
                        if newer {
                            pane.facts.reports = Report::list(&value).0;
                            pane.facts.channels_stale = channels_stale(&value);
                            pane.asked |= detect;
                        }
                        true
                    }
                    Err(_) => false,
                };
                this.finish_applied(ticket, read, window, cx);
            },
        );
    }

    /// `sources.list`. `again` asks even with a read out, whose answer is
    /// then dropped: something was written since it went.
    fn read_sources(&mut self, again: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reads = &mut self.integrations.sources_reads;
        if again {
            reads.forget();
        } else if reads.pending() {
            return;
        }
        let ticket = reads.ask();
        self.ask(
            UiAction::SourcesList,
            window,
            cx,
            move |this, result, window, cx| {
                let pane = &mut this.integrations;
                if !pane.sources_reads.answered(ticket) {
                    return;
                }
                match result {
                    Ok(value) => {
                        let sources = Sources::parse(&value);
                        // Paused or resumed from elsewhere (the CLI, another
                        // window): the settings say so too, and the menu
                        // reads them.
                        let moved = pane
                            .facts
                            .config
                            .as_ref()
                            .and_then(|config| config.paused)
                            .is_some_and(|paused| paused != sources.paused);
                        pane.facts.sources = Some(sources);
                        pane.sources_missing = false;
                        if moved {
                            this.read_config(true, window, cx);
                        }
                    }
                    Err(e) if method_missing(&e) => pane.sources_missing = true,
                    Err(_) => {}
                }
            },
        );
    }

    /// `config.get`'s switches.
    fn read_config(&mut self, again: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reads = &mut self.integrations.config_reads;
        if again {
            reads.forget();
        } else if reads.pending() {
            return;
        }
        let ticket = reads.ask();
        self.ask(
            UiAction::ConfigGet,
            window,
            cx,
            move |this, result, _, _| {
                let pane = &mut this.integrations;
                if pane.config_reads.answered(ticket) {
                    if let Ok(value) = result {
                        pane.facts.config = Some(SourcesConfig::parse(&value));
                    }
                }
            },
        );
    }

    /// `care.test.status`: the last test question.
    fn read_test(&mut self, again: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reads = &mut self.integrations.test_reads;
        if again {
            reads.forget();
        } else if reads.pending() {
            return;
        }
        let ticket = reads.ask();
        self.ask(
            UiAction::TestStatus,
            window,
            cx,
            move |this, result, _, _| {
                let pane = &mut this.integrations;
                if !pane.test_reads.answered(ticket) {
                    return;
                }
                match result {
                    Ok(value) => {
                        pane.test = TestStatus::parse(&value).question;
                        pane.test_missing = false;
                    }
                    Err(e) if method_missing(&e) => pane.test_missing = true,
                    Err(_) => {}
                }
            },
        );
    }

    // ---- the writes --------------------------------------------------------

    /// A row's switch moved to `on`.
    fn flip(&mut self, toggle: &Toggle, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.integrations.facts.config.as_ref() else {
            return;
        };
        let action = toggle_action(toggle, on, config);
        self.integrations.writing = Some(toggle.clone());
        let channels = *toggle == Toggle::ClaudeChannels;
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.integrations.writing = None;
            let wrote = result.is_ok();
            this.written(result, window, cx);
            // The Channels helper is part of the plugin: the plugin is
            // rewritten to match at once, with no plan to read (the switch
            // was the owner's answer). A plugin that is not there is not
            // installed by it.
            let installed = this
                .integrations
                .claude_status()
                .is_some_and(|status| status.installed());
            if wrote && channels && installed {
                this.run_installer(Verb::Repair, Source::CLAUDE.into(), false, window, cx);
            }
        });
    }

    /// A settings write came back: read the settings and the sources again,
    /// or say why it failed.
    fn written(
        &mut self,
        result: Result<Value, deck_client::ClientError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(_) => {
                self.read_config(true, window, cx);
                self.read_sources(true, window, cx);
            }
            Err(e) => self.toast(
                self.fmt(
                    "{0} failed: {1}",
                    &[self.tr("Change the config"), &e.to_string()],
                ),
                window,
                cx,
            ),
        }
    }

    /// Codex's `/hooks` trust, switched in the plan dialog. It changes what
    /// would be written, so the plan is made again and the dialog shows the
    /// new one.
    fn set_trust(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.integrations.pending.as_mut() else {
            return;
        };
        pending.trust = trust;
        let (verb, target) = (pending.verb, pending.target.clone());
        self.plan_integration(verb, &target, window, cx);
    }

    /// **Write these changes**: apply the plan the owner is looking at.
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.integrations.pending.take() else {
            return;
        };
        self.close_dialog(window, cx);
        self.run_installer(pending.verb, pending.target, pending.trust, window, cx);
    }

    /// Run `verb` on `target` for real, then read the installer again: the
    /// files it describes were just rewritten, and only that read says
    /// whether the run is done ([`present::finished`]).
    fn run_installer(
        &mut self,
        verb: Verb,
        target: String,
        trust: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.integrations.busy = true;
        let what = present::plan_headline_localized(verb, &target, self.language());
        let action = UiAction::Integrations {
            verb,
            target: target.clone(),
            trust,
            dry_run: false,
        };
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.integrations.busy = false;
            match result {
                Ok(receipt) => {
                    let line = receipt_line(&what, &receipt, this.language());
                    let pane = &mut this.integrations;
                    // The read after the write is the one this run waits
                    // for; any read before it is stale.
                    pane.installer_reads.forget();
                    pane.applied = Some(Applied {
                        verb,
                        target,
                        what,
                        line,
                        leftover: Leftover::of(&receipt),
                        ticket: pane.installer_reads.asked + 1,
                    });
                    this.read_installer(false, window, cx);
                }
                Err(e) => this.toast(
                    this.fmt("{0} failed: {1}", &[&what, &present::reason(&e)]),
                    window,
                    cx,
                ),
            }
        });
    }

    /// The read a run waited for came (`read`: with an answer): the success
    /// line when the run is done, the command it left when it left one, and
    /// otherwise what the installer still says is wrong.
    fn finish_applied(
        &mut self,
        ticket: u64,
        read: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(applied) = self
            .integrations
            .applied
            .take_if(|applied| applied.ticket == ticket)
        else {
            return;
        };
        let status = read
            .then(|| self.integrations.status_of(&applied.target))
            .flatten();
        if present::finished(applied.verb, &applied.leftover, status.as_ref()) {
            self.toast(applied.line, window, cx);
        } else if !applied.leftover.is_empty() {
            let deckd = crate::app::deckd_path();
            self.integrations.next_step = Some(NextStep::of(
                applied.leftover,
                applied.verb,
                &applied.target,
                deckd.as_deref(),
            ));
            self.open_dialog(NEXT_STEP, window);
            cx.notify();
        } else {
            let why = status
                .as_ref()
                .map(|status| present::status_help_localized(status, self.language()))
                .unwrap_or_default();
            self.toast(
                self.fmt("{0} failed: {1}", &[&applied.what, why]),
                window,
                cx,
            );
        }
    }

    /// Show how a Claude session joins the Channels bridge.
    fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(CONNECT, window);
        cx.notify();
    }

    // ---- the page's parts ----------------------------------------------------

    /// The line under the head: what the last test question did, and at its
    /// right that the window shows counts and times. A service with no test
    /// says so. While everything is paused, that is said first, on a line
    /// of its own: beside the note it would wrap (PLAN T5.4: one line).
    fn result_lines(&self, p: &Palette) -> Div {
        let pane = &self.integrations;
        let paused = (pane.facts.paused() == Some(true)).then(|| {
            self.tr(
                "Paused. Nothing new reaches the deck; questions stay in each tool's own window.",
            )
            .to_string()
        });
        let test: Option<(&'static str, Hsla, String)> = if pane.test_missing {
            Some((
                ic::INFO,
                p.text2,
                self.tr("This needs a newer background service.")
                    .to_string(),
            ))
        } else {
            present::test_line(pane.test.as_ref(), self.language()).map(|(words, mark)| {
                let (icon, color) = match mark {
                    TestMark::Done => (ic::CHECK_CIRCLE, p.good),
                    TestMark::Waiting => (ic::CLOCK, p.text2),
                    TestMark::Missed => (ic::DANGER, p.warn),
                    TestMark::Note => (ic::INFO, p.text2),
                };
                (icon, color, words)
            })
        };
        let note = self.tr("This window shows counts and times, never a session.");
        let line =
            |mark: Option<(&'static str, Hsla)>, words: Option<String>, note: Option<&str>| {
                h_flex()
                    .w_full()
                    .gap(px(8.))
                    .ts(12., 16.)
                    .text_color(p.text2)
                    .children(mark.map(|(path, color)| ui::icon(path, 15., color)))
                    .child(div().flex_1().min_w_0().children(words.map(ui::para)))
                    .children(note.map(|note| {
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .child(note.to_string())
                    }))
            };
        // The note at the right of the test's line, as the board has it, or
        // alone at the right while there is no test to tell of.
        let noted = match test {
            Some((icon, color, words)) => line(Some((icon, color)), Some(words), Some(note)),
            None => line(None, None, Some(note)),
        };
        match paused {
            None => noted,
            Some(paused) => v_flex()
                .gap(px(6.))
                .child(line(Some((ic::INFO, p.text2)), Some(paused), None))
                .child(noted),
        }
    }

    /// The Agent tools card: its head, then a row per tool.
    fn tools_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let lines = tools(&self.integrations.facts, present::now(), self.language());
        let last = lines.len().saturating_sub(1);
        let mut children = vec![self.table_head(p)];
        children.extend(
            lines
                .iter()
                .enumerate()
                .map(|(i, line)| self.table_row(line, i == last, dim, p, cx)),
        );
        ui::card(children, p)
    }

    /// The Connected apps card: a row per app, then the line that says
    /// others appear when they connect. A service with no `sources.list`
    /// says so instead.
    fn apps_card(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let foot = |words: &str| {
            div()
                .px(px(SIDE))
                .py(px(12.))
                .ts(12., 16.)
                .text_color(p.text2)
                .child(ui::para(words.to_string()))
                .into_any_element()
        };
        if self.integrations.sources_missing {
            return ui::card(
                vec![foot(self.tr("This needs a newer background service."))],
                p,
            );
        }
        let mut children: Vec<AnyElement> =
            apps(&self.integrations.facts, present::now(), self.language())
                .iter()
                .map(|line| self.table_row(line, false, dim, p, cx))
                .collect();
        children.push(foot(self.tr(
            "Other apps appear here when they connect. Each gets its own switch.",
        )));
        ui::card(children, p)
    }

    /// The table's head: the cells of a row, with their names.
    fn table_head(&self, p: &Palette) -> AnyElement {
        h_flex()
            .w_full()
            .gap(px(ui::CELL_GAP))
            .px(px(SIDE))
            .pt(px(8.))
            // One more for the card's rule, drawn over the head's last pixel.
            .pb(px(9.))
            .ts(12., 16.)
            .text_color(p.text3)
            .whitespace_nowrap()
            .child(ui::cell(TILE))
            .child(div().flex_1().min_w_0().child(self.tr("Source")))
            .child(ui::cell_end(COUNT).child(self.tr("On the deck")))
            .child(ui::cell_end(AGE).child(self.tr("Last event")))
            .child(
                ui::cell(HANDOFF)
                    .pl(px(CELL_INSET))
                    .child(self.tr("Hand-off")),
            )
            .child(ui::cell(STATE).pl(px(CELL_INSET)).child(self.tr("State")))
            .child(ui::cell(CONTROL))
            .into_any_element()
    }

    /// One row of a table, in the head's cells; a wide row has none for a
    /// count, a time or a hand-off.
    fn table_row(
        &self,
        line: &Line,
        last: bool,
        dim: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let name = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(self.line_title(line, "integrations", p, cx))
            .when(!line.sub.is_empty(), |name| {
                name.child(
                    div()
                        .ts(12., 16.)
                        .text_color(p.text2)
                        .child(ui::para(line.sub.clone())),
                )
            });
        let row = ui::table_row(ROW_HEIGHT, !last)
            .child(ui::tile(line.icon, p))
            .child(name);
        let row = if line.wide {
            row
        } else {
            // What is on the deck; every session the source has, on hover,
            // when that is more.
            let count = ui::cell_end(COUNT)
                .ts(13., 18.)
                .medium()
                .tnum()
                .text_color(p.text)
                .whitespace_nowrap();
            let count = match &line.total {
                Some(total) => count.child(
                    div()
                        .id(SharedString::from(format!("integrations-{}-count", line.id)))
                        .tooltip(ui::tip(total.clone()))
                        .children(line.count.clone()),
                ),
                None => count.children(line.count.clone()),
            };
            row.child(count)
                .child(
                    ui::cell_end(AGE)
                        .ts(12., 16.)
                        .tnum()
                        .text_color(p.text2)
                        .whitespace_nowrap()
                        .children(line.age.clone()),
                )
                .child(
                    ui::cell(HANDOFF)
                        .pl(px(CELL_INSET))
                        .ts(12., 16.)
                        .text_color(p.text2)
                        .children(line.handoff.clone().map(ui::para)),
                )
        };
        // The state is a flex cell, so a pill is as wide as its word.
        row.child(
            ui::cell(STATE)
                .flex()
                .items_center()
                .pl(px(CELL_INSET))
                .children(
                    line.state
                        .as_ref()
                        .map(|state| ui::status(state.word.clone(), state.tone, p)),
                ),
        )
        .child(ui::cell_end(CONTROL).children(self.line_control(line, "integrations", dim, cx)))
        .into_any_element()
    }

    /// A row's name, and **How to Connect…** beside it on the row that
    /// offers it. `scope` keeps the ids of the page's rows and first run's
    /// apart.
    fn line_title(&self, line: &Line, scope: &str, p: &Palette, cx: &Context<Self>) -> Div {
        let hover = p.text;
        h_flex()
            .gap(px(10.))
            .child(
                div()
                    .ts(13., 18.)
                    .medium()
                    .text_color(p.text)
                    .whitespace_nowrap()
                    .child(line.name.clone()),
            )
            .when(line.connect, |title| {
                title.child(
                    Press::new(SharedString::from(format!("{scope}-connect")))
                        .radius(4.)
                        .flex_none()
                        .ts(12., 16.)
                        .medium()
                        .whitespace_nowrap()
                        .text_color(p.text2)
                        .hover(move |style| style.text_color(hover))
                        .child(self.tr("How to Connect\u{2026}"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_connect(window, cx))),
                )
            })
    }

    /// What a row offers at its right end: its switch (with its word, On or
    /// Off: a source's own switch is never the pause), **Set Up**, or
    /// **Repair**. Disabled while the service is away, and a switch while it
    /// is being written.
    fn line_control(
        &self,
        line: &Line,
        scope: &str,
        dim: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let pane = &self.integrations;
        let id = SharedString::from(format!("{scope}-{}", line.id));
        let plan = |verb: Verb, target: &'static str| {
            cx.listener(move |this: &mut Shell, _: &gpui::ClickEvent, window, cx| {
                this.plan_integration(verb, target, window, cx);
            })
        };
        Some(match &line.control {
            Control::None => return None,
            Control::Switch(toggle, on) => {
                let writing = pane.writing.as_ref() == Some(toggle);
                let toggle = toggle.clone();
                Switch::new(id, *on)
                    .word(self.tr("On"), self.tr("Off"))
                    .disabled(dim || writing)
                    .on_toggle(cx.listener(move |this, on: &bool, window, cx| {
                        this.flip(&toggle, *on, window, cx);
                    }))
                    .into_any_element()
            }
            Control::SetUp(target) => Btn::new(id, self.tr("Set Up"))
                .disabled(dim || pane.busy)
                .on_click(plan(Verb::Install, target))
                .into_any_element(),
            Control::Repair(target, help) => Btn::new(id, self.tr("Repair"))
                .primary()
                .tooltip(help.clone())
                .disabled(dim || pane.busy)
                .on_click(plan(Verb::Repair, target))
                .into_any_element(),
        })
    }

    // ---- the dialogs -------------------------------------------------------

    /// "What this writes": the plan's changes, the installer's notes, what
    /// it leaves alone and what is then the owner's to do, verbatim; Codex's
    /// trust switch; **Cancel** and **Write these changes**.
    fn render_plan(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let p = *palette(cx);
        let language = self.language();
        let pane = &self.integrations;
        let Pending {
            verb,
            target,
            trust,
            plan,
        } = pane.pending.as_ref()?;
        let busy = pane.busy;
        let noop = plan.is_noop();
        // Codex only, and never a removal: the trust records the hooks'
        // hashes, and removing a hook records nothing.
        let trustable = verb.takes_trust() && target == "codex";
        let heading = |words: &str| ui::label(words.to_string(), &p).mt(px(16.)).mb(px(6.));
        let lines = |lines: &[String], mono: bool| {
            v_flex().gap(px(6.)).children(lines.iter().map(|line| {
                div()
                    .ts(12., 16.)
                    .text_color(p.text)
                    .when(mono, |text| text.mono())
                    .child(ui::para(line.clone()))
            }))
        };
        let changes = v_flex()
            .gap(px(6.))
            .p(px(12.))
            .rounded(px(8.))
            .bg(p.well)
            .children(plan.changes.iter().map(|change| {
                h_flex()
                    .items_start()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(56.))
                            .ts(12., 16.)
                            .medium()
                            .text_color(p.text2)
                            .child(change_word(&change.action, language)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ts(12., 16.)
                            .mono()
                            .text_color(p.text)
                            .child(ui::para(change.path.clone())),
                    )
            }))
            .when(noop, |changes| {
                changes.child(ui::hint(self.tr(present::NOTHING_TO_WRITE), &p))
            });
        let mut body = v_flex()
            .id("plan-body")
            .min_h_0()
            .overflow_y_scroll()
            .px(px(20.))
            .pt(px(14.))
            .child(changes);
        if !plan.notes.is_empty() {
            body = body
                .child(heading(self.tr("What that means")))
                .child(lines(&plan.notes, false));
        }
        if !plan.conflicts.is_empty() {
            body = body
                .child(heading(self.tr("Left alone, because it is not ours")))
                .child(lines(&plan.conflicts, true));
        }
        if !plan.next_steps.is_empty() {
            body = body
                .child(heading(self.tr("Then, on your side")))
                .child(lines(&plan.next_steps, false));
        }
        if trustable {
            body = body.child(
                h_flex()
                    .mt(px(16.))
                    .gap(px(12.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .ts(13., 18.)
                                    .medium()
                                    .child(self.tr("Record the hook hashes now")),
                            )
                            .child(ui::hint(
                                self.tr(
                                    "Codex's /hooks trust: the hooks run without Codex asking \
                                     first.",
                                ),
                                &p,
                            )),
                    )
                    .child(Switch::new("plan-trust", *trust).disabled(busy).on_toggle(
                        cx.listener(|this, on: &bool, window, cx| {
                            this.set_trust(*on, window, cx);
                        }),
                    )),
            );
        }
        let cancel = Btn::new("plan-cancel", self.tr("Cancel"))
            .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx)));
        let write = Btn::new("plan-write", self.tr("Write these changes"))
            .disabled(noop || busy)
            .on_click(cx.listener(|this, _, window, cx| this.apply_pending(window, cx)));
        // A removal asks with Cancel as the default, as every question
        // before something that cannot be taken back does.
        let (cancel, write) = if *verb == Verb::Uninstall {
            (cancel.primary(), write.danger())
        } else {
            (cancel, write.primary())
        };
        let head = v_flex()
            .flex_none()
            .px(px(20.))
            .pt(px(20.))
            .gap(px(4.))
            .child(
                div()
                    .ts(13., 18.)
                    .semibold()
                    .child(present::plan_headline_localized(*verb, target, language)),
            )
            .child(div().ts(13., 18.).text_color(p.text2).child(ui::para(
                self.tr("Nothing is written until you press Write these changes."),
            )));
        let foot = h_flex()
            .flex_none()
            .p(px(20.))
            .gap(px(8.))
            .when(busy, |foot| {
                foot.child(
                    h_flex()
                        .gap(px(6.))
                        .ts(12., 16.)
                        .text_color(p.text2)
                        .child(ui::spinner(13., p.text2))
                        .child(self.tr("Planning\u{2026}")),
                )
            })
            .child(div().flex_1())
            .child(cancel)
            .child(write);
        Some(
            v_flex()
                .min_h_0()
                .child(head)
                .child(body)
                .child(foot)
                .into_any_element(),
        )
    }

    /// How a Claude session joins the Channels bridge: the command to start
    /// it with, **Copy Command** and **Done**.
    fn render_connect(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        v_flex()
            .p(px(20.))
            .gap(px(4.))
            .ts(13., 18.)
            .child(div().semibold().child(self.tr("Connect a Claude session")))
            .child(div().text_color(p.text2).child(ui::para(
                self.tr("In your project's terminal, start Claude Code with this command:"),
            )))
            .child(ui::code_block(present::CLAUDE_CHANNEL_COMMAND, &p).mt(px(8.)))
            .child(div().mt(px(8.)).text_color(p.text2).child(ui::para(
                self.tr("If Claude asks for a confirmation, accept it."),
            )))
            .child(
                h_flex()
                    .mt(px(16.))
                    .gap(px(8.))
                    .justify_end()
                    .child(ui::CopyButton::new(
                        "connect-copy",
                        present::CLAUDE_CHANNEL_COMMAND,
                        self.tr("Copy Command"),
                        self.tr("Copied"),
                    ))
                    .child(self.done_button("connect-done", cx)),
            )
            .into_any_element()
    }

    /// What a run left to the owner, in place of the line that would have
    /// said it was done: the command that finishes it, with **Copy
    /// Command**, and the service's warnings as it wrote them.
    fn render_next_step(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let p = *palette(cx);
        let next = self.integrations.next_step.as_ref()?;
        let title = match next.command {
            Some(_) => self.tr("One more step: run this command in a terminal."),
            None => self.tr("Then, on your side"),
        };
        Some(
            v_flex()
                .p(px(20.))
                .gap(px(4.))
                .ts(13., 18.)
                .child(div().semibold().child(ui::para(title)))
                .children(
                    next.command
                        .clone()
                        .map(|command| ui::code_block(command, &p).mt(px(8.))),
                )
                .children(next.warnings.iter().map(|warning| {
                    div()
                        .mt(px(8.))
                        .ts(12., 16.)
                        .text_color(p.text2)
                        .child(ui::para(warning.clone()))
                }))
                .child(
                    h_flex()
                        .mt(px(16.))
                        .gap(px(8.))
                        .justify_end()
                        .when_some(next.command.clone(), |buttons, command| {
                            buttons.child(ui::CopyButton::new(
                                "next-step-copy",
                                command,
                                self.tr("Copy Command"),
                                self.tr("Copied"),
                            ))
                        })
                        .child(self.done_button("next-step-done", cx)),
                )
                .into_any_element(),
        )
    }

    /// **Done**, the dialog's default.
    fn done_button(&self, id: &'static str, cx: &Context<Self>) -> Btn {
        Btn::new(id, self.tr("Done"))
            .primary()
            .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx)))
    }
}

impl IntegrationsPane {
    /// The installer's status for `target`, once read.
    fn status_of(&self, target: &str) -> Option<Status> {
        self.facts
            .report(target)
            .map(|report| report.status.clone())
    }
}

// ---- the harness's scenes ---------------------------------------------------------

/// The moment the fixtures' times were written against: 10:02:00 UTC on the
/// boards' day. A staged scene moves them by the difference to its own
/// minute on this Mac's clock, so "4 s ago" is the board's anywhere.
const FIXTURE_NOW: Ts = Ts::from_millis(1_794_391_320_000);

/// Where the installed app's `deckd` is, as a staged scene names it.
const BUNDLED_DECKD: &str = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";

/// The boards' replies, as the service answers them.
const SOURCES_FIXTURE: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/sources_list.json");
const CONFIG_FIXTURE: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json");
const TEST_FIXTURE: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/care_test_status.json");

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// `integrations.status` as the boards' Mac answers it: the plugin in
/// `claude` status, Codex Desktop not set up, and two refused helpers.
fn installer_reply(claude: &str) -> Value {
    json!({
        "targets": [
            {"target": "claude", "status": claude},
            {"target": "codex", "status": "absent"},
            {"target": "codex-desktop", "status": "absent"},
            {"target": "gemini", "status": "absent"},
            {"target": "claude-desktop", "status": "absent"}
        ],
        "warnings": [],
        "claude_channels": [],
        "claude_channels_stale": 2
    })
}

/// `sources` with every last event moved from [`FIXTURE_NOW`] to `now`.
fn rebased(mut sources: Sources, now: Ts) -> Sources {
    let shift = i128::from(now.millis()) - i128::from(FIXTURE_NOW.millis());
    for source in &mut sources.sources {
        source.last_event_at = source.last_event_at.map(|at| {
            let moved = (i128::from(at.millis()) + shift).max(0);
            Ts::from_millis(u64::try_from(moved).unwrap_or(u64::MAX))
        });
    }
    sources
}

/// The pane as a staged scene draws it at `now`.
fn staged(scene: Scene, now: Ts) -> IntegrationsPane {
    let mut pane = IntegrationsPane {
        connected: true,
        asked: true,
        ..IntegrationsPane::default()
    };
    let board = |pane: &mut IntegrationsPane, claude: &str| {
        let installer = installer_reply(claude);
        pane.facts.sources = Some(rebased(Sources::parse(&fixture(SOURCES_FIXTURE)), now));
        pane.facts.reports = Report::list(&installer).0;
        pane.facts.channels_stale = channels_stale(&installer);
        pane.test = TestStatus::parse(&fixture(TEST_FIXTURE)).question;
    };
    pane.facts.config = Some(SourcesConfig::parse(&fixture(CONFIG_FIXTURE)));
    match scene {
        Scene::Integrations
        | Scene::IntegrationsPlan
        | Scene::Service
        | Scene::ServiceStop
        | Scene::ServiceFirmware
        | Scene::Footprint
        | Scene::FootprintUninstall
        | Scene::LocalApi => board(&mut pane, "installed"),
        // Everything paused by the one switch: as the service has it, every
        // session keeps its source and loses its place on the deck.
        Scene::IntegrationsPaused => {
            board(&mut pane, "installed");
            if let Some(config) = &mut pane.facts.config {
                config.paused = Some(true);
            }
            if let Some(sources) = &mut pane.facts.sources {
                sources.paused = true;
                for source in &mut sources.sources {
                    source.on_deck = 0;
                }
            }
            pane.test = None;
        }
        // What goes wrong: the plugin out of date and repaired by a service
        // that could not find `claude` (its commands are left to run), no
        // session on the bridge, Codex not running, and a test question
        // nobody answered.
        Scene::IntegrationsRepair => {
            board(&mut pane, "outdated");
            if let Some(sources) = &mut pane.facts.sources {
                for source in &mut sources.sources {
                    match source.id.as_str() {
                        Source::CLAUDE_CHANNELS => {
                            source.state = Some(SourceState::Waiting);
                            source.stale = Some(0);
                        }
                        Source::CODEX => {
                            source.state = Some(SourceState::Waiting);
                            source.sessions = 0;
                            source.on_deck = 0;
                        }
                        _ => {}
                    }
                }
            }
            pane.facts.channels_stale = 0;
            pane.test = TestStatus::parse(&json!({
                "question": {"state": "expired", "at": null, "after_ms": null},
                "voice": null
            }))
            .question;
            // As the installed app says it: its own `deckd` repairs again.
            pane.next_step = Some(NextStep::of(
                Leftover::of(&repair_receipt()),
                Verb::Repair,
                Source::CLAUDE,
                Some(Path::new(BUNDLED_DECKD)),
            ));
        }
        // A Mac that has just added the plugin: nothing on the deck yet, the
        // bridge off as a new install has it, Codex not started, and the
        // test question first run's last step sent answered on the deck.
        Scene::FirstIntegrations => {
            pane.facts.sources = Some(Sources::parse(&fresh_sources()));
            pane.facts.config = Some(SourcesConfig::parse(&json!({
                "sources": {"paused": false, "claude": true, "claude_channels": false,
                            "apps_paused": []},
                "codex": {"daemon": true},
                "hooks": {"enabled": true}
            })));
            let installer = installer_reply("installed");
            pane.facts.reports = Report::list(&installer).0;
            pane.test = TestStatus::parse(&fixture(TEST_FIXTURE)).question;
        }
        _ => {}
    }
    if scene == Scene::IntegrationsPlan {
        let plan = desktop_plan();
        pane.pending = Some(Pending {
            verb: Verb::Install,
            target: "codex-desktop".into(),
            trust: false,
            plan: Plan::parse(&plan),
        });
    }
    pane
}

/// A repair of the plugin by a service whose `PATH` has no `claude`: the
/// files are written and the two registrations are left to the owner.
fn repair_receipt() -> Value {
    let marketplace = "/Users/you/Library/Application Support/OpenWalkieTalkie/claude-marketplace";
    json!({
        "target": "claude",
        "changed": [
            format!("{marketplace}/.claude-plugin/marketplace.json"),
            format!("{marketplace}/plugins/openwalkietalkie/hooks/hooks.json")
        ],
        "backups": [],
        "notes": [],
        "conflicts": [],
        "next_steps": [
            "a session already open loads the plugin after `/reload-plugins`; new sessions load \
             it at start",
            format!("run: claude plugin marketplace add {marketplace} --scope user --json"),
            "run: claude plugin install openwalkietalkie@openwalkietalkie --scope user --json"
        ]
    })
}

/// The dry run behind Codex Desktop's **Set Up**.
fn desktop_plan() -> Value {
    let launcher = "/Users/you/Applications/Codex with OpenWalkieTalkie.app";
    json!({
        "target": "codex-desktop",
        "dry_run": true,
        "changes": [
            {"path": format!("{launcher}/Contents/Info.plist"), "action": "create"},
            {"path": format!("{launcher}/Contents/MacOS/launch-codex"), "action": "create"}
        ],
        "notes": [
            format!("Creates {launcher}. Opening it starts or reuses Codex's local daemon and \
                     connects Desktop directly to its Unix socket using CODEX_APP_SERVER_WS_URL. \
                     No TCP listener is opened."),
            "The launcher uses CODEX_HOME=/Users/you/.codex; Codex CLI hooks and trust are \
             managed separately."
        ],
        "conflicts": [],
        "next_steps": [
            format!("Quit Codex Desktop, then open {launcher} for desktop sessions on the deck.")
        ]
    })
}

/// `sources.list` on a Mac where nothing has reached the deck yet.
fn fresh_sources() -> Value {
    json!({
        "paused": false,
        "sources": [
            {"id": "claude", "kind": "tool", "name": "Claude Code", "enabled": true,
             "state": null, "on_deck": 0, "sessions": 0, "unit": "sessions",
             "last_event_at": null, "handoff": "app", "hooks": 13, "stale": null,
             "does": null},
            {"id": "claude_channels", "kind": "channel", "name": "Claude Channels",
             "enabled": false, "state": "off", "on_deck": 0, "sessions": 0, "unit": "sessions",
             "last_event_at": null, "handoff": null, "hooks": null, "stale": 0, "does": null},
            {"id": "codex", "kind": "tool", "name": "Codex CLI", "enabled": true,
             "state": "waiting", "on_deck": 0, "sessions": 0, "unit": "threads",
             "last_event_at": null, "handoff": "none", "hooks": null, "stale": null,
             "does": null}
        ],
        "other": 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_keeps_only_the_newest_answer_and_a_poll_waits_for_the_last() {
        let mut reads = Reads::default();
        assert!(!reads.pending());
        let first = reads.ask();
        assert!(reads.pending(), "no second poll while one is out");
        let second = reads.ask();
        assert!(reads.answered(second));
        assert!(!reads.answered(first), "an overtaken answer is dropped");
        assert!(!reads.pending());
        // A write makes every answer still on its way stale.
        let stale = reads.ask();
        reads.forget();
        assert!(!reads.pending());
        let fresh = reads.ask();
        assert!(!reads.answered(stale));
        assert!(reads.answered(fresh));
    }

    #[test]
    fn the_board_scene_says_the_boards_ages_on_any_clock() {
        crate::i18n::install();
        // An hour west of the fixture's clock, and eight east of it.
        for now in [
            FIXTURE_NOW,
            Ts::from_millis(FIXTURE_NOW.millis() - 3_600_000),
            Ts::from_millis(FIXTURE_NOW.millis() + 8 * 3_600_000),
        ] {
            let pane = staged(Scene::Integrations, now);
            let rows = tools(&pane.facts, now, Language::English);
            assert_eq!(rows[0].age.as_deref(), Some("4 s ago"));
            assert_eq!(rows[2].age.as_deref(), Some("1 min ago"));
            let apps = apps(&pane.facts, now, Language::English);
            assert_eq!(apps[0].age.as_deref(), Some("12 s ago"));
        }
    }

    #[test]
    fn every_staged_scene_has_a_read_pause_and_only_the_paused_one_is_paused() {
        for (scene, _) in crate::shots::SCENES {
            if scene.live() {
                continue;
            }
            let pane = staged(scene, crate::shots::board_now());
            let paused = scene == Scene::IntegrationsPaused;
            assert_eq!(pane.paused(), Some(paused), "{scene:?}");
        }
        // Paused, nothing is on the deck, and every session is still
        // counted on hover.
        crate::i18n::install();
        let now = crate::shots::board_now();
        let paused = staged(Scene::IntegrationsPaused, now);
        let rows = tools(&paused.facts, now, Language::English);
        assert_eq!(rows[0].count.as_deref(), Some("0 sessions"));
        assert_eq!(rows[0].total.as_deref(), Some("3 sessions in all"));
        assert!(apps(&paused.facts, now, Language::English)
            .iter()
            .all(|app| app.total.is_some()));
        // The deck's boards show no old helper; the Service board does.
        assert_eq!(staged(Scene::Main, now).stale_helpers(), 0);
        assert_eq!(staged(Scene::Service, now).stale_helpers(), 2);
        assert_eq!(
            staged(Scene::Service, now).claude_status(),
            Some(Status::Installed)
        );
    }

    #[test]
    fn first_runs_card_says_off_once_beside_a_switch_that_is_off() {
        crate::i18n::install();
        let now = crate::shots::board_now();
        let mut pane = staged(Scene::FirstIntegrations, now);
        let rows = tools(&pane.facts, now, Language::English);
        let word = |line: &Line| card_state(line).map(|state| state.word.clone());
        // The bridge is off, as a new install leaves it: the state says Off
        // in the page's table, and first run's card leaves it to the switch.
        let channels = &rows[1];
        assert_eq!(
            channels.control,
            Control::Switch(Toggle::ClaudeChannels, false)
        );
        assert_eq!(channels.state, Some(State::off(Language::English)));
        assert_eq!(word(channels), None);
        // Beside a switch that is on, or a button, the state is said.
        assert_eq!(word(&rows[0]).as_deref(), Some("Connected"));
        assert_eq!(word(&rows[2]).as_deref(), Some("Codex is not running"));
        // Codex Desktop's launcher, a Mac's only.
        if cfg!(windows) {
            assert_eq!(rows.len(), 3);
        } else {
            assert_eq!(word(&rows[3]).as_deref(), Some("Not set up"));
        }
        // Codex switched off reads Off once too.
        if let Some(config) = &mut pane.facts.config {
            config.codex_daemon = Some(false);
        }
        let codex = &tools(&pane.facts, now, Language::English)[2];
        assert_eq!(codex.control, Control::Switch(Toggle::Codex, false));
        assert_eq!(codex.state, Some(State::off(Language::English)));
        assert_eq!(card_state(codex), None);
    }

    #[test]
    fn the_repair_scene_leaves_the_two_registrations_to_the_owner() {
        let pane = staged(Scene::IntegrationsRepair, crate::shots::board_now());
        let next = pane.next_step.expect("what the run left");
        assert_eq!(
            next.command.as_deref(),
            Some("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd integrations repair claude")
        );
        assert!(next.warnings.is_empty());
        assert!(pane.pending.is_none());
        let plan = staged(Scene::IntegrationsPlan, crate::shots::board_now())
            .pending
            .expect("the plan in the dialog");
        assert_eq!(plan.target, "codex-desktop");
        assert_eq!(plan.plan.changes.len(), 2);
    }
}
