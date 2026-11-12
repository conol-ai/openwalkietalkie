//! First run (boards Connect and Found): the window, with no sidebar, takes
//! the owner from a deck in the box to a deck that works with this Mac, in
//! four steps. **1 Connect** looks for the deck until it is plugged in or
//! paired. **2 What this adds** says what this app is about to add — the
//! background service and the Claude Code plugin — and adds nothing until
//! the owner presses **Add These**. **3 Voice** and **4 Integrations** show
//! the Voice and Integrations pages' own cards, with **Continue**, **Send a
//! Test Question** and **Finish**.
//!
//! **Nothing is added before Add These, and nothing by Not Now**, which only
//! remembers that first run was seen: the next start opens on the pages,
//! where the Deck page offers **Set Up…** for as long as the service is not
//! set up (at step 1 until this Mac has seen a deck, [`set_up_step`]). And
//! **Add These adds nothing until a deck has been seen** (owner decision 7,
//! [`Flow::may_add`]): found by the window, or vouched for by **Continue**
//! on a copy that cannot look. Before any service exists the deck is found
//! by the `deckd` beside this app's executable (`deckd probe --json`, every
//! 2 s, off the UI thread): it lists HID and opens nothing. Add These
//! registers the
//! service's launch agent, waits up to 20 s for the service to answer (as
//! long as it takes while macOS holds it for the owner's approval), then has
//! the service add the plugin and reads the plugin's status back. An install
//! the service could not finish by itself — a `claude` command it found no
//! CLI to run, or a plugin that reads back as anything but installed — is
//! not reported as done: the step shows the one command that finishes it
//! from a terminal, with **Copy Command** (amendment B4, by the rule the
//! Integrations page follows too, [`present::finished`]).
//!
//! The last step's test question is the Integrations page's: it is sent and
//! read by that page's pane, and its line is that page's
//! ([`present::test_line`]).
//!
//! The steps are a machine with no window ([`Flow`]): what the owner pressed
//! and what came back go in, and what to do next goes out to an
//! [`Installer`], which only starts each thing. The window's own installer
//! does them on this Mac; a test's records them, and a harness scene's steps
//! drop them, so neither registers, starts or writes anything. The words
//! are pure functions of the flow and a language, so a test reads every
//! line without a window.
use crate::app;
use crate::art::{self, Device};
use crate::present::{self, Leftover, LoginItem};
use crate::shell::{Screen, Shell};
use crate::shots::Scene;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, PopupButton, Row, Segmented, Type as _};
use deck_client::ClientError;
use deck_gui_core::admin::{Report, Status, Verb};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Template, Tone, Transport, Ts};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, Subscription, Window};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use std::io::Read as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// How often step 1 looks for the deck, counted from the last look's answer.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);
/// How long **Add These** waits for a registered service to answer.
pub const SERVICE_WAIT: Duration = Duration::from_secs(20);
/// How long one look may take: `deckd probe` only lists HID, and one that
/// hangs must not stop the next.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The installer's name for the Claude Code plugin.
const CLAUDE: &str = "claude";
/// Why a copy of the app with no `deckd` beside it cannot look.
const NO_DECKD: &str = "no deckd beside this app's executable";

/// The links under step 1.
pub const WAITLIST: &str = "https://openwalkietalkie.org/#waitlist";
pub const BUILD_YOUR_OWN: &str = "https://github.com/conol-ai/openwalkietalkie/tree/main/hardware";
pub const DEMO: &str = "https://openwalkietalkie.org/#demo";

/// The bar across the top: its padding (top, left, right), and the room the
/// traffic lights take at its head (three 12 px lights 8 apart; AppKit
/// draws its own there).
const BAR: (f32, f32, f32) = (12., 18., 20.);
const LIGHTS: f32 = 52.;
/// The stage the deck stands on, the deck on it, and the column of words
/// beside it.
const STAGE: (f32, f32) = (330., 470.);
const DECK_WIDTH: f32 = 236.;
const WORDS_WIDTH: f32 = 470.;
/// Steps 3 and 4 show the Voice and Integrations pages' cards, which are
/// drawn for a page's column.
const CARDS_WIDTH: f32 = ui::PAGE_WIDTH;
/// Between the stage and the words, the body's sides and its foot.
const BODY_GAP: f32 = 56.;
const BODY_SIDE: f32 = 56.;
const BODY_FOOT: f32 = 40.;
/// Between the blocks of a column of words.
const GAP: f32 = 16.;

// ---- the steps ---------------------------------------------------------------

/// First run's four steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    /// Find the deck.
    Connect,
    /// What this app adds, and adding it.
    Adds,
    /// The speech engines.
    Voice,
    /// What can put sessions on the deck, and a test question.
    Integrations,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Connect, Step::Adds, Step::Voice, Step::Integrations];

    /// Its number in the stepper, from 1.
    pub const fn number(self) -> u8 {
        match self {
            Step::Connect => 1,
            Step::Adds => 2,
            Step::Voice => 3,
            Step::Integrations => 4,
        }
    }

    /// The step numbered `n`; past either end, the nearest one.
    pub fn numbered(n: u8) -> Step {
        Step::ALL[usize::from(n.clamp(1, 4)) - 1]
    }
}

/// A step's name in the stepper.
pub fn step_name(step: Step, language: Language) -> &'static str {
    match step {
        Step::Connect => language.text("Connect"),
        Step::Adds => language.text("What this adds"),
        Step::Voice => language.text("Voice"),
        Step::Integrations => language.text("Integrations"),
    }
}

/// How the stepper draws a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Behind the open step: a check.
    Done,
    /// The open step: its number on the primary's plate.
    Current,
    /// Still to come: its number in a ring.
    Ahead,
}

/// How `step` is drawn while `at` is open.
pub fn mark(step: Step, at: Step) -> Mark {
    match step.cmp(&at) {
        std::cmp::Ordering::Less => Mark::Done,
        std::cmp::Ordering::Equal => Mark::Current,
        std::cmp::Ordering::Greater => Mark::Ahead,
    }
}

// ---- looking for the deck ------------------------------------------------------

/// A bus `deckd probe` lists the deck on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Usb,
    Bluetooth,
    /// Anything else HID reports.
    Unknown,
}

impl Bus {
    pub fn parse(word: &str) -> Bus {
        match word {
            "usb" => Bus::Usb,
            "bluetooth" => Bus::Bluetooth,
            _ => Bus::Unknown,
        }
    }
}

/// What `deckd probe --json` says: whether a deck is there, and each bus HID
/// lists it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub present: bool,
    pub buses: Vec<Bus>,
}

impl Probe {
    /// The probe's output, `{"present": bool, "instances": [{"bus": …}]}`.
    /// Its exit code is not read: it says what `present` says (0 there, 1
    /// not), and the JSON is what is trusted. `None` for anything that is not
    /// that answer — a `deckd` too old to have the verb prints a usage error
    /// and no JSON at all.
    pub fn parse(output: &str) -> Option<Probe> {
        let line = output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| line.starts_with('{'))?;
        let reply: Value = serde_json::from_str(line).ok()?;
        let present = reply.get("present")?.as_bool()?;
        let buses = reply
            .get("instances")
            .and_then(Value::as_array)
            .map(|instances| {
                instances
                    .iter()
                    .map(|instance| {
                        Bus::parse(
                            instance
                                .get("bus")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Probe { present, buses })
    }

    /// How the deck it found is linked: by cable when HID lists it on USB at
    /// all (the cable is the link the service takes first), else over
    /// Bluetooth; `None` when there is no deck.
    pub fn found(&self) -> Option<Found> {
        if !self.present {
            return None;
        }
        Some(if self.buses.contains(&Bus::Usb) {
            Found::Cable
        } else if self.buses.contains(&Bus::Bluetooth) {
            Found::Bluetooth
        } else {
            Found::Linked
        })
    }
}

/// How a deck that was found is linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Cable,
    Bluetooth,
    /// On a bus nobody named: the probe's `unknown`, or a service too old
    /// to report its transport.
    Linked,
}

impl Found {
    /// The deck a service has linked, by the transport it reports.
    pub fn of(transport: Option<Transport>) -> Found {
        match transport {
            Some(Transport::Usb) => Found::Cable,
            Some(Transport::Bluetooth) => Found::Bluetooth,
            None => Found::Linked,
        }
    }
}

/// Where the search for the deck stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    /// Looking, every [`PROBE_EVERY`].
    Looking,
    /// This copy has no `deckd` to look with, or its `deckd` gave no answer:
    /// the owner goes on by hand.
    CannotLook,
    Found(Found),
}

/// The search in words, and their tone: the info pill while looking, a
/// plain word when this copy cannot look, a good dot once found.
pub fn search_status(search: Search, language: Language) -> (&'static str, Tone) {
    match search {
        Search::Looking => (language.text("Looking for your deck"), Tone::Blue),
        Search::CannotLook => (
            language.text("This copy cannot look for a deck"),
            Tone::Gray,
        ),
        Search::Found(Found::Cable) => (language.text("Deck found, linked by cable"), Tone::Green),
        Search::Found(Found::Bluetooth) => (
            language.text("Deck found, linked over Bluetooth"),
            Tone::Green,
        ),
        Search::Found(Found::Linked) => (language.text("Deck found"), Tone::Green),
    }
}

// ---- adding the service and the plugin -----------------------------------------

/// What first run does outside its window: look for the deck, register the
/// background service, wait for it, have it add the Claude Code plugin.
///
/// Each method only **starts** its work; the answer comes back to the
/// [`Flow`] later — [`Flow::probed`], [`Flow::registered`],
/// [`Flow::service`], [`Flow::plugin`]. The window's own installer does the
/// work on this Mac; a test's only records what it was asked, and a harness
/// scene's steps drop everything, so neither registers, starts or writes
/// anything.
pub trait Installer {
    /// Look for a deck once: the bundled `deckd probe --json`.
    fn probe(&mut self);
    /// Register the service's launch agent with macOS.
    fn register_agent(&mut self);
    /// Wait for the registered service to answer.
    fn wait_service(&mut self);
    /// Have the service add the Claude Code plugin, and read the plugin's
    /// status back.
    fn install_claude(&mut self);
}

/// How step 2's **Add These** goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adding {
    /// Not pressed: **Add These** and **Not Now**.
    Idle,
    /// Registering the service with macOS.
    Registering,
    /// Waiting for the service to answer, since `since`.
    Waiting { since: Ts },
    /// macOS holds the service until the owner allows it in Login Items;
    /// waiting for as long as that takes.
    Approval,
    /// The service answers: adding the plugin and reading it back.
    Installing,
    /// Both added: the flow is on step 3.
    Added,
    /// The service did not answer in time, or could not be registered.
    ServiceFailed,
    /// The plugin is not finished: one command for the owner to run.
    OneMoreStep { command: String },
    /// The plugin was not added, for the service's reason.
    NotAdded(String),
}

impl Adding {
    /// Under way: **Add These** waits.
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Adding::Registering | Adding::Waiting { .. } | Adding::Approval | Adding::Installing
        )
    }

    /// Over, with the service answering and the plugin not added: the owner
    /// goes on to step 3 with **Continue**.
    pub fn settled(&self) -> bool {
        matches!(self, Adding::OneMoreStep { .. } | Adding::NotAdded(_))
    }
}

/// How adding the plugin ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginOutcome {
    /// Added, read back as installed, and nothing left to do.
    Added,
    /// Not finished: this command, run in a terminal, finishes it.
    OneMoreStep { command: String },
    /// Not added, for this reason.
    NotAdded(String),
}

/// How adding the plugin ended, from the install's reply and the plugin's
/// status read back after it (`None` when that read failed). `deckd` is the
/// one beside this app's executable, which the command left to run names.
///
/// It is the Integrations page's rule ([`present::finished`]): added when
/// the service ran everything itself and the plugin does not read back as
/// anything but installed. A next step that is a command the service could
/// not run (`run: …`, when it found no `claude`) or one that failed
/// (`WARNING: …`) is work not done, and so is any status but `installed`:
/// then one command finishes it from a terminal
/// ([`present::finish_command`]). The service's advice beside a finished
/// install (a session already open reloads its plugins) leaves nothing
/// undone. A refused install is not added, in the service's own words.
pub fn plugin_outcome(
    install: Result<&Value, &ClientError>,
    status: Option<&Status>,
    deckd: Option<&Path>,
) -> PluginOutcome {
    let receipt = match install {
        Ok(receipt) => receipt,
        Err(error) => return PluginOutcome::NotAdded(present::reason(error)),
    };
    let leftover = Leftover::of(receipt);
    if present::finished(Verb::Install, &leftover, status) {
        return PluginOutcome::Added;
    }
    PluginOutcome::OneMoreStep {
        command: present::finish_command(deckd, Verb::Install, CLAUDE, &leftover.commands),
    }
}

/// The Claude Code plugin's status in an `integrations.status` reply.
pub fn claude_status(reply: &Value) -> Option<Status> {
    Report::list(reply)
        .0
        .into_iter()
        .find(|report| report.target == CLAUDE)
        .map(|report| report.status)
}

/// What a line about adding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Under way: beside the buttons, with a spinner.
    Busy,
    /// The owner's turn: a line of its own.
    Turn,
    /// It went wrong: a line of its own, with a warning.
    Problem,
}

/// What step 2 says about adding, and what kind of line it is; nothing
/// before **Add These** and after both are added.
pub fn adding_line(adding: &Adding, language: Language) -> Option<(String, LineKind)> {
    let (line, kind) = match adding {
        Adding::Idle | Adding::Added => return None,
        Adding::Registering => (
            language.text("Adding the background service…").to_string(),
            LineKind::Busy,
        ),
        Adding::Waiting { .. } => (
            language.text("Waiting for the service…").to_string(),
            LineKind::Busy,
        ),
        Adding::Approval => (
            language
                .text("macOS is waiting for your approval in System Settings > General > Login Items.")
                .to_string(),
            LineKind::Turn,
        ),
        Adding::Installing => (
            language.text("Adding the Claude Code plugin…").to_string(),
            LineKind::Busy,
        ),
        Adding::ServiceFailed => (
            language
                .text("The service did not start. Open Service to see why.")
                .to_string(),
            LineKind::Problem,
        ),
        Adding::OneMoreStep { .. } => (
            language
                .text("One more step: run this command in a terminal.")
                .to_string(),
            LineKind::Turn,
        ),
        Adding::NotAdded(why) => (
            language.format("The plugin was not added: {0}", &[why]),
            LineKind::Problem,
        ),
    };
    Some((line, kind))
}

/// What step 2 offers beside a line about adding, to do what the line
/// says: first run has no sidebar to go anywhere by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// **Open Login Items**: System Settings, where macOS waits for the
    /// owner to allow the service.
    LoginItems,
    /// **Open Service**: first run ends on the Service page, which says why
    /// the service did not start.
    OpenService,
}

/// The button beside `adding`'s line, if it has one.
pub fn adding_remedy(adding: &Adding) -> Option<Remedy> {
    match adding {
        Adding::Approval => Some(Remedy::LoginItems),
        Adding::ServiceFailed => Some(Remedy::OpenService),
        _ => None,
    }
}

// ---- the machine ---------------------------------------------------------------

/// First run's steps, with no window: what the owner pressed and what came
/// back go in, and what to do next goes to an [`Installer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    step: Step,
    search: Search,
    /// A look is out and has not answered.
    probing: bool,
    /// When the next look is due.
    next_probe: Option<Ts>,
    adding: Adding,
    /// The owner pressed **Continue** on step 1 when this copy could not
    /// look for the deck: going on by hand says there is one.
    went_on: bool,
    /// The owner left (**Not Now**) or finished: nothing more is started.
    ended: bool,
}

impl Flow {
    /// The flow at `step`; `can_look` is whether this copy has a `deckd` to
    /// look for the deck with. A copy that cannot look starts step 2 on step
    /// 1 instead, where the owner goes on by hand: nothing is added before a
    /// deck has been seen, or vouched for there ([`Flow::may_add`]).
    pub fn new(step: Step, can_look: bool) -> Flow {
        Flow {
            step: if !can_look && step == Step::Adds {
                Step::Connect
            } else {
                step
            },
            search: if can_look {
                Search::Looking
            } else {
                Search::CannotLook
            },
            probing: false,
            next_probe: None,
            adding: Adding::Idle,
            went_on: false,
            ended: false,
        }
    }

    /// Whether **Add These** may add anything (owner decision 7: nothing is
    /// added until a deck has been seen): the deck has been found, or this
    /// copy cannot look for one and the owner went on by hand from step 1.
    /// While the window is still looking, it may not.
    pub fn may_add(&self) -> bool {
        match self.search {
            Search::Found(_) => true,
            Search::CannotLook => self.went_on,
            Search::Looking => false,
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    pub fn search(&self) -> Search {
        self.search
    }

    pub fn adding(&self) -> &Adding {
        &self.adding
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Whether it looks for the deck: on the first two steps until the deck
    /// is found, and only while nothing is being added.
    pub fn looking(&self) -> bool {
        !self.ended
            && self.search == Search::Looking
            && self.step <= Step::Adds
            && self.adding == Adding::Idle
    }

    /// A moment has passed (`now`): look again when a look is due, and stop
    /// waiting for a service that has had [`SERVICE_WAIT`].
    pub fn tick(&mut self, now: Ts, installer: &mut impl Installer) {
        if self.ended {
            return;
        }
        if let Adding::Waiting { since } = self.adding {
            if now.millis().saturating_sub(since.millis()) >= millis(SERVICE_WAIT) {
                self.adding = Adding::ServiceFailed;
            }
        }
        if self.looking() && !self.probing && self.next_probe.is_none_or(|due| now >= due) {
            self.probing = true;
            installer.probe();
        }
    }

    /// A look answered at `now`: a deck found moves step 1 on to step 2; a
    /// look that could not be made means this copy cannot look, and the
    /// owner goes on by hand from step 1 (a step 2 opened by **Set Up…**
    /// goes back there: it adds nothing before a deck is vouched for).
    pub fn probed(&mut self, answer: Result<Probe, String>, now: Ts) {
        self.probing = false;
        self.next_probe = Some(now.plus(PROBE_EVERY));
        if self.ended || self.search != Search::Looking {
            return;
        }
        match answer {
            Ok(probe) => {
                if let Some(found) = probe.found() {
                    self.seen(found);
                }
            }
            Err(_) => {
                self.search = Search::CannotLook;
                if self.step == Step::Adds {
                    self.step = Step::Connect;
                }
            }
        }
    }

    /// A deck is linked as `found`: the probe found it, or a service that
    /// already runs has linked it. Step 1 is done.
    pub fn seen(&mut self, found: Found) {
        if self.ended {
            return;
        }
        self.search = Search::Found(found);
        if self.step == Step::Connect {
            self.step = Step::Adds;
        }
    }

    /// **Continue**: from step 1 when this copy cannot look (the owner says
    /// there is a deck, which lets **Add These** add), from step 2 once
    /// adding is over with the plugin not added, from step 3.
    pub fn advance(&mut self) {
        if self.ended {
            return;
        }
        self.step = match self.step {
            Step::Connect if self.search == Search::CannotLook => {
                self.went_on = true;
                Step::Adds
            }
            Step::Adds if self.adding.settled() => Step::Voice,
            Step::Voice => Step::Integrations,
            step => step,
        };
    }

    /// **Add These**: register the service. Again after a service that did
    /// not start. Nothing at all before a deck has been seen
    /// ([`Flow::may_add`]).
    pub fn add_these(&mut self, installer: &mut impl Installer) {
        if self.ended
            || self.step != Step::Adds
            || !self.may_add()
            || !matches!(self.adding, Adding::Idle | Adding::ServiceFailed)
        {
            return;
        }
        self.adding = Adding::Registering;
        installer.register_agent();
    }

    /// The registration answered with the status macOS reports after it, at
    /// `now`; `connected` is whether a service answers already. A service
    /// that answers is the one to add the plugin to, whatever macOS said;
    /// otherwise an enabled service is waited for, and one held for the
    /// owner's approval for as long as that takes.
    pub fn registered(
        &mut self,
        answer: Result<LoginItem, String>,
        connected: bool,
        now: Ts,
        installer: &mut impl Installer,
    ) {
        if self.ended || self.adding != Adding::Registering {
            return;
        }
        if connected {
            self.install(installer);
            return;
        }
        self.adding = match answer {
            Ok(LoginItem::Enabled) => Adding::Waiting { since: now },
            Ok(LoginItem::RequiresApproval) => Adding::Approval,
            Ok(_) | Err(_) => Adding::ServiceFailed,
        };
        if self.adding.busy() {
            installer.wait_service();
        }
    }

    /// Whether the service answers now. Waited for — or given up on, and
    /// answering late — it is asked to add the plugin.
    pub fn service(&mut self, connected: bool, installer: &mut impl Installer) {
        if self.ended || !connected {
            return;
        }
        if matches!(
            self.adding,
            Adding::Waiting { .. } | Adding::Approval | Adding::ServiceFailed
        ) {
            self.install(installer);
        }
    }

    fn install(&mut self, installer: &mut impl Installer) {
        self.adding = Adding::Installing;
        installer.install_claude();
    }

    /// Adding the plugin ended. Added, the flow goes on to step 3 by itself;
    /// otherwise it stays on step 2 with what is left, until **Continue**.
    pub fn plugin(&mut self, outcome: PluginOutcome) {
        if self.ended || self.adding != Adding::Installing {
            return;
        }
        match outcome {
            PluginOutcome::Added => {
                self.adding = Adding::Added;
                self.step = Step::Voice;
            }
            PluginOutcome::OneMoreStep { command } => self.adding = Adding::OneMoreStep { command },
            PluginOutcome::NotAdded(why) => self.adding = Adding::NotAdded(why),
        }
    }

    /// **Not Now**: leave, having added nothing that was not already added.
    pub fn not_now(&mut self) {
        self.ended = true;
    }

    /// **Finish**.
    pub fn finish(&mut self) {
        self.ended = true;
    }
}

/// `duration` in milliseconds, as a [`Ts`] counts.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The step **Set Up…** opens first run at (the Deck page's, the frame's
/// strip's, the Service page's): the first, where the window looks for the
/// deck, unless this Mac has seen one already (`deck_seen`), when it is the
/// second, what this app adds. Either way **Add These** waits for the deck
/// ([`Flow::may_add`]).
pub const fn set_up_step(deck_seen: bool) -> u8 {
    if deck_seen {
        Step::Adds.number()
    } else {
        Step::Connect.number()
    }
}

// ---- the pane ----------------------------------------------------------------

/// One thing the flow asked for, for the shell to do ([`Asked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Probe,
    RegisterAgent,
    WaitService,
    InstallClaude,
}

/// The window's own [`Installer`]: what the flow asked for, in order, which
/// the shell then does on this Mac — unless the steps are a harness
/// scene's, when it does none of it.
#[derive(Default)]
struct Asked(Vec<Job>);

impl Installer for Asked {
    fn probe(&mut self) {
        self.0.push(Job::Probe);
    }

    fn register_agent(&mut self) {
        self.0.push(Job::RegisterAgent);
    }

    fn wait_service(&mut self) {
        self.0.push(Job::WaitService);
    }

    fn install_claude(&mut self) {
        self.0.push(Job::InstallClaude);
    }
}

/// First run's own state, which the shell owns.
pub struct FirstRunPane {
    /// First run has the window, in place of the sidebar and the pages.
    active: bool,
    /// The steps are a harness scene's: drawn as staged, and nothing they
    /// ask for is done.
    staged: bool,
    flow: Flow,
}

impl FirstRunPane {
    /// The pane, and the subscriptions its text fields need kept alive: it
    /// has no text field.
    pub(crate) fn new(
        _window: &mut Window,
        _cx: &mut Context<Shell>,
    ) -> (FirstRunPane, Vec<Subscription>) {
        (FirstRunPane::at(Step::Connect, false), Vec::new())
    }

    /// Not active, at `step`.
    fn at(step: Step, can_look: bool) -> FirstRunPane {
        FirstRunPane {
            active: false,
            staged: false,
            flow: Flow::new(step, can_look),
        }
    }

    /// Whether first run has the window, in place of the sidebar and pages.
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    /// First run has the window for real, not as a harness scene.
    fn live(&self) -> bool {
        self.active && !self.staged
    }
}

impl Shell {
    /// Ask for what the open step shows: on the first two, a look for the
    /// deck at once rather than at the next tick; on the last two, what the
    /// Voice and Integrations pages' cards draw, which their pages ask for
    /// (the last test question among the Integrations page's).
    pub(crate) fn refresh_firstrun(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.first_run.live() {
            return;
        }
        match self.first_run.flow.step() {
            Step::Connect | Step::Adds => {
                let now = present::now();
                self.first_run_do(|flow, jobs| flow.tick(now, jobs), window, cx);
            }
            Step::Voice => self.refresh_voice(force, window, cx),
            Step::Integrations => self.refresh_integrations(force, window, cx),
        }
    }

    /// The open step under the bar: the deck on its stage beside the words
    /// on the first two, the borrowed cards on the last two. `dim` is set
    /// while the service is not there, which only those cards depend on.
    pub(crate) fn render_firstrun(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let step = self.first_run.flow.step();
        let content = match step {
            Step::Connect => h_flex()
                .gap(px(BODY_GAP))
                .child(self.first_run_stage(step, &p, cx))
                .child(self.first_run_connect(&p, cx)),
            Step::Adds => h_flex()
                .gap(px(BODY_GAP))
                .child(self.first_run_stage(step, &p, cx))
                .child(self.first_run_adds(&p, cx)),
            Step::Voice => self.first_run_voice(dim, &p, cx),
            Step::Integrations => self.first_run_integrations(dim, &p, cx),
        };
        // Centred both ways in what the bar leaves, and scrolled when a
        // short window cannot hold it. The scroller is a flex column and the
        // row grows to fill it: a block parent hands its children no height,
        // so a row's `min_h_full` would resolve to nothing and the row would
        // only be as tall as the step, with no room to centre it in. The
        // row never shrinks below the step, so a short window scrolls.
        div()
            .id("first-run")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .w_full()
                    .flex_grow()
                    .flex_shrink_0()
                    .justify_center()
                    .px(px(BODY_SIDE))
                    .pb(px(BODY_FOOT))
                    .child(content),
            )
            .into_any_element()
    }

    /// The whole window while first run has it: the bar over the open step,
    /// with no sidebar.
    pub(crate) fn render_first_run(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        v_flex()
            .size_full()
            .bg(p.win)
            .child(self.first_run_bar(&p, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_firstrun(!self.state.connected(), cx)),
            )
            .into_any_element()
    }

    /// Hand the window to first run at `step` (1 to 4): at 1 when the
    /// installed app opens with nothing set up, at [`set_up_step`] from
    /// **Set Up…**. It starts looking for the deck at once.
    pub(crate) fn begin_first_run(
        &mut self,
        step: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let can_look = app::deckd_path().is_some();
        self.first_run = FirstRunPane {
            active: true,
            ..FirstRunPane::at(Step::numbered(step), can_look)
        };
        self.refresh_firstrun(true, window, cx);
        cx.notify();
    }

    /// **Set Up…**, wherever a service that was never set up is offered it:
    /// first run, at step 1 unless this Mac has seen a deck
    /// ([`set_up_step`]).
    pub(crate) fn set_up_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = set_up_step(self.prefs(cx).deck_seen());
        self.begin_first_run(step, window, cx);
    }

    /// First run has no dialog of its own.
    pub(crate) fn firstrun_dialog(
        &self,
        _id: &'static str,
        _cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        None
    }

    /// First run has no dialog to let go of.
    pub(crate) fn firstrun_dialog_closed(&mut self, _id: &'static str, _cx: &mut Context<Self>) {}

    /// Nothing lives outside the render: first run has no text field.
    pub(crate) fn localize_firstrun(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// The service answering, and the deck it links, as the flow needs them:
    /// a deck a running service has linked is found, and a service that
    /// answers is the one **Add These** was waiting for.
    pub(crate) fn on_firstrun_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.first_run.live() {
            return;
        }
        let connected = self.state.connected();
        let linked = self
            .linked()
            .then(|| Found::of(self.state.mirror().device.transport));
        self.first_run_do(
            |flow, jobs| {
                if let Some(found) = linked {
                    flow.seen(found);
                }
                flow.service(connected, jobs);
            },
            window,
            cx,
        );
    }

    /// Once a second: look again when a look is due, and stop waiting for a
    /// service that has had its time. A test question on the deck is asked
    /// after by the Integrations page's own tick.
    pub(crate) fn tick_firstrun(
        &mut self,
        _beat: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.first_run.live() {
            return;
        }
        let now = present::now();
        self.first_run_do(|flow, jobs| flow.tick(now, jobs), window, cx);
    }

    /// First run's scenes: step 1 still looking, step 2 with the deck found
    /// by cable, and the two steps drawn from other pages' cards (step 4
    /// with the last test question answered, as the Integrations page stages
    /// it from the service's own reply fixture). Any other scene puts first
    /// run away.
    pub(crate) fn stage_firstrun(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut pane = match scene {
            Scene::Connect => FirstRunPane::at(Step::Connect, true),
            Scene::Found => {
                let mut pane = FirstRunPane::at(Step::Adds, true);
                pane.flow.seen(Found::Cable);
                pane
            }
            Scene::FirstVoice => FirstRunPane::at(Step::Voice, true),
            Scene::FirstIntegrations => FirstRunPane::at(Step::Integrations, true),
            _ => {
                self.first_run.active = false;
                return false;
            }
        };
        pane.active = true;
        pane.staged = true;
        self.first_run = pane;
        self.go(Screen::Deck, window, cx);
        true
    }

    // ---- what the flow asks for ----------------------------------------------

    /// Let the flow take `change`, then do what it asked for — unless its
    /// steps are a harness scene's, which reach nothing outside the window.
    fn first_run_do(
        &mut self,
        change: impl FnOnce(&mut Flow, &mut Asked),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut asked = Asked::default();
        change(&mut self.first_run.flow, &mut asked);
        if self.first_run.live() {
            // A deck found is a deck seen on this Mac, which **Set Up…**
            // and Start at login wait for (owner decision 7).
            let found = matches!(self.first_run.flow.search(), Search::Found(_));
            if found && !self.prefs(cx).deck_seen() {
                self.update_prefs(cx, crate::prefs::Prefs::set_deck_seen);
            }
            for job in asked.0 {
                self.first_run_job(job, window, cx);
            }
        }
        cx.notify();
    }

    fn first_run_job(&mut self, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        match job {
            Job::Probe => self.first_run_probe(window, cx),
            // A moment later, so the line saying so is drawn before macOS is
            // asked.
            Job::RegisterAgent => cx
                .spawn_in(window, async move |this, cx| {
                    this.update_in(cx, |this, window, cx| this.first_run_register(window, cx))
                        .ok();
                })
                .detach(),
            // Nothing to start: the window's connection to the service
            // retries by itself (every 5 s at most), each change of it
            // reaches the flow through `on_firstrun_event`, and the tick
            // keeps the time.
            Job::WaitService => {}
            Job::InstallClaude => self.first_run_install(window, cx),
        }
    }

    /// One look for the deck: the bundled `deckd probe --json`, on the
    /// bridge's blocking pool, its answer back on this thread.
    fn first_run_probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(deckd) = app::deckd_path() else {
            let now = present::now();
            self.first_run_do(|flow, _| flow.probed(Err(NO_DECKD.into()), now), window, cx);
            return;
        };
        let (tx, rx) = async_channel::bounded(1);
        cx.global::<app::Tokio>().0.spawn(async move {
            let answer = tokio::task::spawn_blocking(move || run_probe(&deckd))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(answer).await;
        });
        cx.spawn_in(window, async move |this, cx| {
            let answer = rx.recv().await.unwrap_or_else(|e| Err(e.to_string()));
            this.update_in(cx, |this, window, cx| {
                if this.first_run.live() {
                    let now = present::now();
                    this.first_run_do(|flow, _| flow.probed(answer, now), window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Register the service's launch agent — only the installed app run for
    /// real does; any other run is refused before macOS is asked.
    fn first_run_register(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.first_run.live() {
            return;
        }
        let answer = crate::login_item::register_agent();
        if let Err(why) = &answer {
            eprintln!("openwalkietalkie-app: first run could not register the service: {why}");
        }
        let connected = self.state.connected();
        let now = present::now();
        self.first_run_do(
            |flow, jobs| flow.registered(answer, connected, now, jobs),
            window,
            cx,
        );
    }

    /// Have the service add the Claude Code plugin, then read its status
    /// back: what it says decides whether adding is done.
    fn first_run_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let install = UiAction::Integrations {
            verb: Verb::Install,
            target: CLAUDE.into(),
            trust: false,
            dry_run: false,
        };
        self.ask(install, window, cx, |this, reply, window, cx| {
            let receipt = match reply {
                Ok(receipt) => receipt,
                Err(error) => {
                    let outcome = plugin_outcome(Err(&error), None, None);
                    this.first_run_do(|flow, _| flow.plugin(outcome), window, cx);
                    return;
                }
            };
            let read = UiAction::IntegrationsStatus { detect: false };
            this.ask(read, window, cx, move |this, reply, window, cx| {
                let status = reply.ok().and_then(|reply| claude_status(&reply));
                let deckd = app::deckd_path();
                let outcome = plugin_outcome(Ok(&receipt), status.as_ref(), deckd.as_deref());
                this.first_run_do(|flow, _| flow.plugin(outcome), window, cx);
            });
        });
    }

    // ---- the owner's presses -------------------------------------------------

    fn first_run_add_these(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.first_run_do(|flow, jobs| flow.add_these(jobs), window, cx);
    }

    /// **Not Now**: the main window, with nothing added that was not
    /// already. First run was seen and is not opened by itself again: the
    /// next start opens on the pages too, where the Deck page offers **Set
    /// Up…** for as long as the service is not set up.
    fn first_run_not_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.first_run_leave(Screen::Deck, window, cx);
    }

    /// **Open Service**, beside a service that did not start: first run
    /// ends on the Service page, which says why (the service is registered
    /// by now, so the next start would not open first run either).
    fn first_run_open_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.first_run_leave(Screen::Service, window, cx);
    }

    /// Leave first run for `screen`, starting nothing more, and remember
    /// that it was seen.
    fn first_run_leave(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        self.first_run.flow.not_now();
        self.first_run.active = false;
        self.update_prefs(cx, |prefs| prefs.set_first_run_done(true));
        self.go(screen, window, cx);
    }

    /// **Continue**. Leaving the voice step tells the service its question
    /// about recognition was asked: this step is where it is answered now.
    fn first_run_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.first_run.flow.step() == Step::Voice {
            let prompted = UiAction::ConfigSet {
                key: "voice.stt.prompted".into(),
                value: ConfigValue::Bool(true),
            };
            self.act(prompted, window, cx);
        }
        self.first_run.flow.advance();
        self.refresh_firstrun(false, window, cx);
        cx.notify();
    }

    /// **Finish**: first run is done for good, and the window opens on the
    /// Deck.
    fn finish_first_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.first_run.flow.finish();
        self.first_run.active = false;
        self.update_prefs(cx, |prefs| prefs.set_first_run_done(true));
        self.go(Screen::Deck, window, cx);
    }

    // ---- the bar ---------------------------------------------------------------

    /// The traffic lights' place, the brand, the steps and the language.
    fn first_run_bar(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let (top, left, right) = BAR;
        h_flex()
            .flex_none()
            .w_full()
            .gap(px(20.))
            .pt(px(top))
            .pl(px(left))
            .pr(px(right))
            .child(div().flex_none().w(px(LIGHTS)).h(px(12.)))
            .child(ui::brand())
            .child(div().flex_1())
            .child(self.first_run_stepper(p))
            // It lies on the band a double click zooms the window from, and
            // keeps a double click on it to itself.
            .child(
                div()
                    .flex_none()
                    .occlude()
                    .child(self.first_run_language(cx)),
            )
    }

    /// The four steps: a check on the ones behind, the open one's number on
    /// the primary's plate, the others' in a ring.
    fn first_run_stepper(&self, p: &Palette) -> Div {
        let at = self.first_run.flow.step();
        let language = self.language();
        h_flex()
            .flex_none()
            .gap(px(20.))
            .children(Step::ALL.map(|step| {
                let mark = mark(step, at);
                let current = mark == Mark::Current;
                let (fill, words, rim) = if current {
                    (p.solid, p.on_solid, p.solid)
                } else {
                    (gpui::transparent_black(), p.text2, p.edge)
                };
                let badge = div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded_full()
                    .border_1()
                    .border_color(rim)
                    .bg(fill)
                    .ts(12., 16.)
                    .semibold()
                    .tnum()
                    .text_color(words)
                    .map(|badge| match mark {
                        Mark::Done => badge.child(ui::icon(ic::CHECK, 12., p.text2)),
                        Mark::Current | Mark::Ahead => badge.child(step.number().to_string()),
                    });
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .ts(13., 18.)
                    .medium()
                    .text_color(if current { p.text } else { p.text2 })
                    .child(badge)
                    .child(step_name(step, language))
            }))
    }

    /// The language, by each one's own name. Before any service has
    /// answered it is kept on this Mac and spoken by the window until a
    /// service takes it; with one answering, it is the deck's setting.
    fn first_run_language(&self, cx: &Context<Self>) -> PopupButton {
        let current = self.language();
        let shell = cx.entity();
        PopupButton::new("first-run-language", current.name()).menu(move |mut menu, _, _| {
            for language in Language::ALL {
                let shell = shell.clone();
                let item = PopupMenuItem::new(language.name())
                    .checked(language == current)
                    .on_click(move |_, window, cx| {
                        shell.update(cx, |this, cx| this.choose_language(language, window, cx));
                    });
                menu = menu.item(item);
            }
            menu
        })
    }

    // ---- steps 1 and 2 ---------------------------------------------------------

    /// The deck on its stage: on step 1 with PTT lit and the splash a waking
    /// deck shows, then with its first face up, in the case's colour.
    fn first_run_stage(&self, step: Step, p: &Palette, cx: &Context<Self>) -> Div {
        let case = self.prefs(cx).case();
        let deck = match step {
            Step::Connect => Device::new(case, None, DECK_WIDTH)
                .ptt_lit(true)
                .splash(true),
            _ => Device::new(case, Some(Template::Agents), DECK_WIDTH)
                .title(self.tr(art::plate_title(Template::Agents))),
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(STAGE.0))
            .h(px(STAGE.1))
            .rounded(px(12.))
            .bg(p.stage)
            .border_1()
            .border_color(p.line)
            .child(deck)
    }

    /// Step 1: the search, how to connect, the way to Bluetooth, that
    /// nothing is installed yet, and the links out.
    fn first_run_connect(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let search = self.first_run.flow.search();
        let (said, tone) = search_status(search, self.language());
        let how = v_flex()
            .gap(px(8.))
            .ts(13., 20.)
            .text_color(p.text2)
            .child(div().child(ui::para(
                self.tr("Plug it in with the USB-C cable. This window finds it by itself."),
            )))
            .child(div().child(ui::para(
                self.tr("If macOS asks to allow the accessory, click Allow."),
            )))
            .child(div().child(ui::para(self.tr(
                "If the screen is dark, hold PTT, the long left-edge key, for 2 seconds.",
            ))));
        let bluetooth = h_flex()
            .gap(px(12.))
            .child(
                Btn::new("first-run-bluetooth", self.tr("Open Bluetooth Settings"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(present::BLUETOOTH_SETTINGS)),
            )
            .child(
                div()
                    .ts(12., 16.)
                    .text_color(p.text2)
                    .child(self.tr("It appears as OpenWalkieTalkie.")),
            );
        let nothing_yet = h_flex()
            .items_start()
            .gap(px(10.))
            .px(px(14.))
            .py(px(12.))
            .rounded(px(12.))
            .bg(p.surf)
            .border_1()
            .border_color(p.line)
            .ts(13., 20.)
            .text_color(p.text2)
            .child(ui::bare(ui::icon(ic::INFO, 16., p.text2)))
            .child(div().flex_1().min_w_0().child(ui::para(self.tr(
                "Nothing is installed yet. When your deck is found, the next step\nshows what this app adds and waits for you to agree.",
            ))));
        let links = h_flex()
            .gap(px(20.))
            .child(
                ui::link_out("first-run-waitlist", self.tr("Join the waitlist"), p)
                    .on_click(|_, _, cx| cx.open_url(WAITLIST)),
            )
            .child(
                ui::link_out("first-run-build", self.tr("Build your own"), p)
                    .on_click(|_, _, cx| cx.open_url(BUILD_YOUR_OWN)),
            )
            .child(
                ui::link_out("first-run-demo", self.tr("Try it in the browser"), p)
                    .on_click(|_, _, cx| cx.open_url(DEMO)),
            );
        v_flex()
            .flex_none()
            .w(px(WORDS_WIDTH))
            .gap(px(GAP))
            .child(h_flex().child(ui::status(said, tone, p)))
            .child(heading(self.tr("Connect your deck")))
            .child(how)
            .child(bluetooth)
            .child(nothing_yet)
            .child(links)
            .when(search == Search::CannotLook, |words| {
                words.child(h_flex().child(self.first_run_continue_button("first-run-skip", cx)))
            })
    }

    /// Step 2: the deck found, the two things this app adds, the case's
    /// colour, then **Add These** or **Not Now** and how adding goes.
    fn first_run_adds(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let (said, tone) = search_status(self.first_run.flow.search(), self.language());
        let adds = ui::card(
            vec![
                Row::new()
                    .lead(ui::tile(ic::SERVER, p))
                    .title(self.tr("Background service"))
                    .sub(self.tr("Keeps the deck linked. Starts at login."))
                    .into_any_element(),
                Row::new()
                    .lead(ui::tile(ic::CLAUDE, p))
                    .title(self.tr("Claude Code plugin"))
                    .sub(self.tr("The Claude Code hooks, in this app's own folder."))
                    .into_any_element(),
            ],
            p,
        );
        let case = self.prefs(cx).case();
        let chosen = crate::prefs::Case::ALL
            .iter()
            .position(|c| *c == case)
            .unwrap_or_default();
        let colours = Segmented::new(
            "first-run-case",
            vec![self.tr("White").into(), self.tr("Black").into()],
            chosen,
        )
        .on_select(cx.listener(|this, at: &usize, _, cx| {
            if let Some(case) = crate::prefs::Case::ALL.get(*at).copied() {
                this.update_prefs(cx, |prefs| prefs.set_case(case));
            }
        }));
        let colour = ui::card(
            vec![Row::new()
                .title(self.tr("Case colour"))
                .sub(self.tr("So the picture matches your deck."))
                .right(colours)
                .into_any_element()],
            p,
        );
        v_flex()
            .flex_none()
            .w(px(WORDS_WIDTH))
            .gap(px(GAP))
            .child(h_flex().child(ui::status(said, tone, p)))
            .child(heading(self.tr("What this app adds")))
            .child(div().ts(13., 20.).text_color(p.text2).child(ui::para(self.tr(
                "Nothing has been added yet. These two keep the deck working\nwhen this window is closed.",
            ))))
            .child(adds)
            .child(ui::hint(
                self.tr("Codex needs nothing installed. Speech models come in the next step."),
                p,
            ))
            .child(colour)
            .children(self.first_run_adding_said(p, cx))
            .child(self.first_run_buttons(p, cx))
    }

    /// What adding says when it is the owner's turn or it went wrong, on a
    /// line of its own, with the one button that does what the line says
    /// (**Open Login Items** for macOS's approval, **Open Service** for a
    /// service that did not start); with one more step, the command and
    /// **Copy Command**.
    fn first_run_adding_said(&self, p: &Palette, cx: &Context<Self>) -> Option<Div> {
        let adding = self.first_run.flow.adding();
        let (line, kind) = adding_line(adding, self.language())?;
        let mark = match kind {
            LineKind::Busy => return None,
            LineKind::Turn => ui::icon(ic::INFO, 16., p.text2),
            LineKind::Problem => ui::icon(ic::DANGER, 16., p.warn),
        };
        let button = match adding_remedy(adding) {
            Some(Remedy::LoginItems) => Some(
                Btn::new("first-run-login-items", self.tr("Open Login Items"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(present::LOGIN_ITEMS_SETTINGS)),
            ),
            Some(Remedy::OpenService) => Some(
                Btn::new("first-run-open-service", self.tr("Open Service")).on_click(
                    cx.listener(|this, _, window, cx| this.first_run_open_service(window, cx)),
                ),
            ),
            None => None,
        };
        let said = h_flex()
            .items_start()
            .gap(px(8.))
            .ts(13., 20.)
            .text_color(p.text2)
            .child(ui::bare(mark))
            .child(div().flex_1().min_w_0().child(ui::para(line)))
            .children(button.map(|button| div().flex_none().child(button)));
        let command = match adding {
            Adding::OneMoreStep { command } => Some(self.first_run_command(command, p)),
            _ => None,
        };
        Some(v_flex().gap(px(8.)).child(said).children(command))
    }

    /// The command left to run, and **Copy Command** beside it.
    fn first_run_command(&self, command: &str, p: &Palette) -> Div {
        h_flex()
            .items_start()
            .gap(px(10.))
            .child(ui::code_block(command.to_string(), p).flex_1().min_w_0())
            .child(ui::CopyButton::new(
                "first-run-copy",
                command.to_string(),
                self.tr("Copy Command"),
                self.tr("Copied"),
            ))
    }

    /// **Add These** and **Not Now** — or **Continue** once adding is over
    /// with the plugin not added — and, at the end of the row, where each
    /// can be removed, or what is under way.
    fn first_run_buttons(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let flow = &self.first_run.flow;
        let adding = flow.adding();
        let row = h_flex().gap(px(10.));
        let row = if adding.settled() {
            row.child(self.first_run_continue_button("first-run-continue", cx))
        } else {
            // Nothing is added before the deck is found (owner decision 7).
            let unseen = !flow.may_add();
            row.child(
                Btn::new("first-run-add", self.tr("Add These"))
                    .primary()
                    .disabled(adding.busy() || unseen)
                    .when(unseen, |add| add.tooltip(self.tr("Link the deck first.")))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.first_run_add_these(window, cx)),
                    ),
            )
            .child(Btn::new("first-run-not-now", self.tr("Not Now")).on_click(
                cx.listener(|this, _, window, cx| this.first_run_not_now(window, cx)),
            ))
        };
        let end = match adding_line(adding, self.language()) {
            Some((line, LineKind::Busy)) => h_flex()
                .gap(px(6.))
                .child(ui::spinner(13., p.text2))
                .child(line)
                .into_any_element(),
            _ => div()
                .child(self.tr("Each can be removed later in Service."))
                .into_any_element(),
        };
        row.child(div().flex_1()).child(
            div()
                .flex_none()
                .ts(12., 16.)
                .text_color(p.text2)
                .child(end),
        )
    }

    // ---- steps 3 and 4 ---------------------------------------------------------

    /// Step 3: the Voice page's two engine cards. The sentence over them
    /// says where speech goes as the cards leave it: on this Mac, but for
    /// what is said to a Conol chat while the Conol switch in the cards is
    /// on, which it is unless the owner turns it off.
    fn first_run_voice(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        v_flex()
            .flex_none()
            .w(px(CARDS_WIDTH))
            .gap(px(GAP))
            .child(heading(self.tr("Voice")))
            .child(lead(
                self.tr(
                    "Speech is recognised and spoken on this Mac unless you choose otherwise.\nWhile the Conol switch below is on, what you say to Conol chats is heard by Conol's cloud.",
                ),
                p,
            ))
            .child(self.render_voice_engines(dim, cx))
            .child(h_flex().child(self.first_run_continue_button("first-run-voice-continue", cx)))
    }

    /// Step 4: the Integrations page's agent tools, its test question and
    /// how the last one went (or that the service is too old to send one),
    /// and **Finish**.
    fn first_run_integrations(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let linked = self.linked();
        let missing = self.integrations.test_missing();
        let send = Btn::new("first-run-test", self.tr("Send a Test Question"))
            .disabled(!linked || missing)
            .when(!linked, |send| {
                send.tooltip(self.tr("Link the deck first."))
            })
            .on_click(cx.listener(|this, _, window, cx| this.send_test_question(window, cx)));
        let line = if missing {
            Some(
                self.tr("This needs a newer background service.")
                    .to_string(),
            )
        } else {
            present::test_line(self.integrations.test_question(), self.language())
                .map(|(line, _)| line)
        };
        let finish = Btn::new("first-run-finish", self.tr("Finish"))
            .primary()
            .on_click(cx.listener(|this, _, window, cx| this.finish_first_run(window, cx)));
        v_flex()
            .flex_none()
            .w(px(CARDS_WIDTH))
            .gap(px(GAP))
            .child(heading(self.tr("Integrations")))
            .child(lead(self.tr("What can put sessions on your deck."), p))
            .child(self.render_agent_tools(dim, cx))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(send)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ts(12., 16.)
                            .text_color(p.text2)
                            .children(line.map(ui::para)),
                    )
                    .child(finish),
            )
    }

    /// **Continue**, under `id`.
    fn first_run_continue_button(&self, id: &'static str, cx: &Context<Self>) -> Btn {
        Btn::new(id, self.tr("Continue"))
            .primary()
            .on_click(cx.listener(|this, _, window, cx| this.first_run_continue(window, cx)))
    }
}

/// A step's heading: 30/36, semibold.
fn heading(text: &str) -> Div {
    div().ts(30., 36.).semibold().child(text.to_string())
}

/// The sentence under a step's heading, wrapping.
fn lead(text: &str, p: &Palette) -> Div {
    div()
        .ts(13., 20.)
        .text_color(p.text2)
        .child(ui::para(text.to_string()))
}

/// `deckd probe --json` beside this app, and what it said. Blocking: run
/// off the UI thread. Its exit code is not read (the JSON says the same);
/// one that does not finish in [`PROBE_TIMEOUT`] is stopped.
fn run_probe(deckd: &Path) -> Result<Probe, String> {
    let mut child = std::process::Command::new(deckd)
        .args(["probe", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{} could not be run: {e}", deckd.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < PROBE_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not answer within {} s",
                    deckd.display(),
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_string(&mut output)
            .map_err(|e| e.to_string())?;
    }
    Probe::parse(&output).ok_or_else(|| format!("{} gave no probe answer", deckd.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::{Prefs, Store};

    #[test]
    fn a_language_picked_before_any_service_is_written_to_the_preference_and_spoken() {
        // What the popup's pick does with no service answering
        // (`Shell::choose_language`): the preference holds it, written
        // through to its store, and the window speaks it over the deck's
        // own setting until a service takes it.
        let file = std::env::temp_dir().join(format!(
            "openwalkietalkie-app-{}-first-run-prefs.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        let mut prefs = Prefs::load(Store::File(file.clone()));
        assert_eq!(prefs.language(), None);
        prefs.set_language(Some(Language::Japanese));
        let kept = Prefs::load(Store::File(file.clone()));
        assert_eq!(kept.language(), Some(Language::Japanese));
        assert_eq!(
            crate::shell::spoken(kept.language(), Language::English as u16),
            Language::Japanese
        );
        // A service took it: the preference lets go, and the window speaks
        // the deck's setting, which is the pick now.
        prefs.set_language(None);
        let kept = Prefs::load(Store::File(file.clone()));
        assert_eq!(kept.language(), None);
        assert_eq!(
            crate::shell::spoken(kept.language(), Language::Japanese as u16),
            Language::Japanese
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn the_window_asks_for_each_thing_the_flow_asks_for_in_order() {
        let mut asked = Asked::default();
        let mut flow = Flow::new(Step::Adds, true);
        flow.seen(Found::Cable);
        flow.add_these(&mut asked);
        flow.registered(
            Ok(LoginItem::Enabled),
            false,
            Ts::from_millis(0),
            &mut asked,
        );
        flow.service(true, &mut asked);
        assert_eq!(
            asked.0,
            [Job::RegisterAgent, Job::WaitService, Job::InstallClaude]
        );
    }
}
