//! Built-in stand-in for a deck host, until a real one says hello: the mockup
//! data, a responder that answers every `Event` the way a host would, and a
//! script that only ever advances as a consequence of user input and never
//! wakes the screen.
//!
//! `load` reproduces the mockup strings EXACTLY so previews can be laid over
//! `mock/*.png`. `host` answers ONE `HostMsg` per event; the later
//! consequences (an answered card makes its agent Working, ~20 s later another
//! agent asks something, ~35 s later the first one reports Ready) are queued
//! in `Script` as small descriptors, never as messages. `on_tick` builds one
//! due message at a time on its stack and applies it through `UiState::host`,
//! so the demo exercises exactly the path a real host does. Timers are lazy:
//! `animating` asks for animation-rate ticks only while a transcript is being
//! typed or a follow-up is due within a second; while the screen is off nothing
//! advances and the due times shift by the dark time.
use super::model::{Agent, Deck, Notice, MAX_AGENTS};
use super::msg::{
    AgentKind, AgentState, Binding, CardKind, Event, HostMsg, KeyId, PttCtx, Tone, UsageItem,
    WindowRow,
};
use super::text::Text;
use crate::ui::{UiState, Update};

/// Face id of the default strip's Macropad, which the demo keys belong to.
pub const MACRO_FACE: u8 = 3;
/// 19:42.
pub const CLOCK_MINUTE: u16 = 19 * 60 + 42;
/// What PTT "hears" in demo mode (mockup 7, without its quotes).
pub const TRANSCRIPT: &str = "Refactor the auth module, then run the test suite.";
/// The name a demo PTT in Naming produces.
pub const FACE_NAME: &str = "Deploy";

/// What PTT "hears" when a card is answered by voice.
const ANSWER_TRANSCRIPT: &str = "Run them on staging first, then rerun the tests.";
/// The agent's answer to `TRANSCRIPT`, and what it does next.
const TALK_REPLY: &str = "On it: refactoring auth, then running the suite.";
const TALK_WORKING: &str = "Refactoring the auth module";
const TALK_DONE: &str = "Auth refactor done \u{b7} suite passing";
const TALK_DONE_REPLY: &str = "Auth module refactored; the full test suite passes.";
const TALK_STEPS: [&str; 2] = ["Open a pull request", "Deploy to staging"];
/// For an agent the tables do not know (a host's roster kept after a link loss).
const GENERIC_WORKING: &str = "Working on it";
const GENERIC_DONE: &str = "Done";
const DONE_SUFFIX: &str = " \u{b7} done";
const DONE_PREFIX: &str = "Done: ";

// The script's timing, every delay counted from the user's input.
/// STT latency before the first word of a transcript shows.
const HEAR_MS: u32 = 300;
/// Typing cadence of the transcript, per character.
const CHAR_MS: u32 = 40;
/// A voice answer: the card clears this long after the final transcript.
const CLEAR_MS: u32 = 300;
/// Working follows the card clear after this.
const WORKING_MS: u32 = 400;
/// A dictation is answered after this.
const REPLY_MS: u32 = 700;
/// A fresh question from another agent.
const QUESTION_MS: u32 = 20_000;
/// The agent that got orders reports Ready.
const READY_MS: u32 = 35_000;
/// `animating` horizon: only follow-ups this close tick at the animation rate.
const SOON_MS: u32 = 1000;

/// One `Orders` and one `Ready` per agent plus the one fresh question: with
/// the dedupe below the queue can never lose a consequence.
const QUEUE_LEN: usize = 2 * MAX_AGENTS + 1;
const WORKSPACE_COUNT: u8 = 4;
const MAX_WINDOWS: u8 = 6;
/// Fresh cards count from here; 1 and 2 are the mockup's.
const FIRST_CARD: u16 = 100;
/// Phases of `Pending::Orders`; an entry may start at any of them.
const ORDERS_REPLY: u8 = 0;
const ORDERS_CLEAR: u8 = 1;
const ORDERS_WORKING: u8 = 2;

/// What an agent was told to do; picks the titles of its follow-ups.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Job {
    /// A question was answered, by choice or by voice.
    Question,
    /// A suggested step of a Ready card was chosen.
    Step(u8),
    /// Free dictation.
    Talk,
}

/// One queued consequence. Multi-message ones advance a phase per delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// What an agent does with its orders. Phases: reply (a dictation),
    /// card clear (when it holds one), Working.
    Orders { agent: u8, job: Job, phase: u8 },
    /// A question from an agent other than `not`, picked when due. Phases:
    /// agent state, reply, card.
    Question {
        not: u8,
        agent: u8,
        card: u16,
        phase: u8,
    },
    /// Same phases.
    Ready {
        agent: u8,
        job: Job,
        card: u16,
        phase: u8,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Entry {
    what: Pending,
    due_ms: u32,
}

/// A transcript being typed out while PTT is held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Typing {
    agent: u8,
    ctx: PttCtx,
    started_ms: u32,
    /// Characters sent so far.
    shown: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Workspace {
    selected: u8,
    /// Terminals ENTER added to the canned list.
    terms: u8,
}

/// Private state of the responder and script. Lives in `UiState.demo`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Script {
    queue: [Option<Entry>; QUEUE_LEN],
    typing: Option<Typing>,
    /// First tick with the screen off; due times shift by the dark time.
    off_since: Option<u32>,
    next_card: u16,
    /// Round-robin cursor (agent id) for fresh questions.
    asker: u8,
    /// 1-based, as `load` shows it.
    active_ws: u8,
    workspaces: [Workspace; WORKSPACE_COUNT as usize],
    refreshes: u8,
}

impl Script {
    pub const fn new() -> Self {
        Self {
            queue: [None; QUEUE_LEN],
            typing: None,
            off_since: None,
            next_card: FIRST_CARD,
            asker: 0,
            active_ws: 2,
            workspaces: [Workspace {
                selected: 0,
                terms: 0,
            }; WORKSPACE_COUNT as usize],
            refreshes: 0,
        }
    }

    /// Follow-ups waiting to be delivered.
    pub fn queued(&self) -> usize {
        self.queue.iter().flatten().count()
    }

    /// A transcript is being typed out.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// False when the queue is full: the consequence is lost, never a panic.
    fn push(&mut self, what: Pending, due_ms: u32) -> bool {
        match self.queue.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(Entry { what, due_ms });
                true
            }
            None => false,
        }
    }

    /// The latest orders to an agent replace what it was still about to do.
    fn push_orders(&mut self, agent: u8, job: Job, phase: u8, due_ms: u32) {
        let what = Pending::Orders { agent, job, phase };
        let waiting = self
            .queue
            .iter_mut()
            .flatten()
            .find(|entry| matches!(entry.what, Pending::Orders { agent: a, .. } if a == agent));
        match waiting {
            Some(entry) => *entry = Entry { what, due_ms },
            None => {
                self.push(what, due_ms);
            }
        }
    }

    /// At most one fresh question is in flight at a time.
    fn push_question(&mut self, not: u8, due_ms: u32) {
        let queued = self
            .queue
            .iter()
            .flatten()
            .any(|entry| matches!(entry.what, Pending::Question { .. }));
        if !queued {
            self.push(
                Pending::Question {
                    not,
                    agent: 0,
                    card: 0,
                    phase: 0,
                },
                due_ms,
            );
        }
    }

    /// The latest orders decide when, and as what, an agent reports Ready.
    fn push_ready(&mut self, agent: u8, job: Job, due_ms: u32) {
        let what = Pending::Ready {
            agent,
            job,
            card: 0,
            phase: 0,
        };
        let waiting = self.queue.iter_mut().flatten().find(
            |entry| matches!(entry.what, Pending::Ready { agent: a, phase: 0, .. } if a == agent),
        );
        match waiting {
            Some(entry) => *entry = Entry { what, due_ms },
            None => {
                self.push(what, due_ms);
            }
        }
    }

    /// The entry that has waited longest past its due time.
    fn due(&self, now_ms: u32) -> Option<usize> {
        let mut best: Option<(usize, u32)> = None;
        for (index, entry) in self.queue.iter().enumerate() {
            let Some(entry) = entry else { continue };
            let late = now_ms.wrapping_sub(entry.due_ms);
            if late < u32::MAX / 2 && best.map_or(true, |(_, most)| late > most) {
                best = Some((index, late));
            }
        }
        best.map(|(index, _)| index)
    }

    fn new_card(&mut self) -> u16 {
        let id = self.next_card;
        self.next_card = match self.next_card.checked_add(1) {
            Some(next) => next,
            None => FIRST_CARD,
        };
        id
    }

    /// The next agent round the roster that is present, has no card, has a
    /// canned question and is not `not`.
    fn pick_asker(&mut self, deck: &Deck, not: u8) -> Option<u8> {
        for step in 1..=MAX_AGENTS as u8 {
            let id = (self.asker + step - 1) % MAX_AGENTS as u8 + 1;
            if id != not
                && deck.agent(id).is_some()
                && deck.card(id).is_none()
                && canned(id).is_some()
            {
                self.asker = id;
                return Some(id);
            }
        }
        None
    }

    fn workspace(&self) -> usize {
        (self.active_ws as usize).wrapping_sub(1) % WORKSPACE_COUNT as usize
    }

    /// LEFT/RIGHT cycle the canned workspaces, the wheel moves the selection,
    /// ENTER opens a terminal (which takes the focus). True when handled.
    fn omarchy_key(&mut self, key: KeyId, act: u8) -> bool {
        let index = self.workspace();
        let count = window_count(index, self.workspaces[index].terms);
        let ws = &mut self.workspaces[index];
        match (key, act) {
            (KeyId::Left, 1) => {
                self.active_ws = if self.active_ws <= 1 {
                    WORKSPACE_COUNT
                } else {
                    self.active_ws - 1
                }
            }
            (KeyId::Right, 1) => self.active_ws = self.active_ws % WORKSPACE_COUNT + 1,
            (KeyId::Cw, 2) => ws.selected = (ws.selected + 1).min(count.saturating_sub(1)),
            (KeyId::Ccw, 2) => ws.selected = ws.selected.saturating_sub(1),
            (KeyId::Enter, 1) => {
                if count < MAX_WINDOWS {
                    ws.terms += 1;
                    ws.selected = count;
                }
            }
            _ => return false,
        }
        true
    }

    /// The `ws` push for the active workspace, filled in place.
    fn workspaces_msg(&self, out: &mut HostMsg) {
        let index = self.workspace();
        let ws = self.workspaces[index];
        *out = HostMsg::Workspaces {
            active: self.active_ws,
            count: WORKSPACE_COUNT,
            selected: 0,
            windows: [WindowRow::EMPTY; 6],
            window_count: 0,
        };
        let HostMsg::Workspaces {
            selected,
            windows,
            window_count,
            ..
        } = out
        else {
            return;
        };
        let terms = core::iter::repeat((Tone::Gray, TERM_ROW)).take(ws.terms as usize);
        for (slot, (tone, title)) in windows
            .iter_mut()
            .zip(WORKSPACES[index].iter().copied().chain(terms))
        {
            slot.tone = tone;
            slot.title.set(title);
            *window_count += 1;
        }
        *selected = ws.selected.min(window_count.saturating_sub(1));
    }
}

const TERM_ROW: &str = "term \u{b7} zsh";
/// Window lists of workspaces 1..=4; 2 is the mockup's.
const WORKSPACES: [&[(Tone, &str)]; WORKSPACE_COUNT as usize] = [
    &[
        (Tone::Blue, "term \u{b7} nvim \u{2014} openwalkietalkie"),
        (Tone::Amber, "firefox \u{2014} hyprland wiki"),
    ],
    &[
        (Tone::Blue, "term \u{b7} claude \u{2014} header refactor"),
        (Tone::Amber, "firefox \u{2014} github \u{b7} pulls"),
        (Tone::Green, "term \u{b7} codex \u{2014} compiling..."),
    ],
    &[
        (Tone::Green, "term \u{b7} gemini \u{2014} research notes"),
        (Tone::Blue, "obsidian \u{2014} deck spec"),
    ],
    &[
        (Tone::Red, "term \u{b7} infra \u{2014} deploy failed"),
        (Tone::Gray, "spotify \u{2014} focus mix"),
    ],
];

fn window_count(index: usize, terms: u8) -> u8 {
    let base = WORKSPACES.get(index).map_or(0, |list| list.len()) as u8;
    base.saturating_add(terms).min(MAX_WINDOWS)
}

const USAGE_PERIOD: &str = "SEP 2026";
const USAGE: [(&str, u8, &str); 3] = [
    ("Claude Pro", 62, "resets in 4 days"),
    ("Codex \u{b7} OpenAI", 81, "resets tomorrow"),
    ("Gemini", 12, "resets in 11 days"),
];

/// The `usage` push: every refresh visibly moves each bar, never past 99.
fn usage_msg(refreshes: u8, out: &mut HostMsg) {
    *out = HostMsg::Usage {
        period: Text::from_str(USAGE_PERIOD),
        items: [UsageItem::EMPTY; 6],
        count: USAGE.len() as u8,
    };
    if let HostMsg::Usage { items, .. } = out {
        for (index, (slot, (name, percent, reset))) in items.iter_mut().zip(USAGE).enumerate() {
            slot.name.set(name);
            slot.reset.set(reset);
            slot.percent = (percent + refreshes.wrapping_add(index as u8) % 3).min(99);
        }
    }
}

/// What each demo agent asks, does and reports; index = id - 1.
struct Canned {
    /// The question card, its title on the roster, and the reply it comes with.
    question: &'static str,
    title: &'static str,
    choices: [&'static str; 3],
    recommended: u8,
    asks: &'static str,
    /// Title while it works on the answer.
    working: &'static str,
    /// Title and Ready-card text when done, the reply it comes with, and the
    /// suggested steps.
    done: &'static str,
    done_reply: &'static str,
    steps: [&'static str; 2],
}

const CANNED: [Canned; 5] = [
    Canned {
        question: "Tests fail on main. Run pending migrations on staging first?",
        title: "Run migrations on staging?",
        choices: ["Run on staging", "Skip migrations", "Abort task"],
        recommended: 0,
        asks: "Tests fail on main: 3 pending migrations are not applied on staging.",
        working: "Applying migrations on staging",
        done: "Migrations applied \u{b7} tests green",
        done_reply: "3 migrations applied on staging; the suite passes again.",
        steps: ["Deploy the API to staging", "Open a pull request"],
    },
    Canned {
        question: "Header refactor changed 4 storybook components. Update the stories?",
        title: "Update the storybook stories?",
        choices: ["Update stories", "Leave them", "Abort"],
        recommended: 0,
        asks: "The header refactor changed 4 components the storybook still renders the old way.",
        working: "Updating storybook stories",
        done: "Header refactor done \u{b7} 34 files",
        done_reply: "Header refactor done, 34 files changed",
        steps: ["Run the test suite", "Open a pull request"],
    },
    Canned {
        question: "Build finished for both boards. Flash the LilyGo now?",
        title: "Flash the LilyGo now?",
        choices: ["Flash now", "Run tests first", "Hold"],
        recommended: 0,
        asks: "Native and Embassy images built; the LilyGo is on USB.",
        working: "Flashing the LilyGo",
        done: "Firmware flashed \u{b7} boots clean",
        done_reply: "Flashed and rebooted; the panel wake sequence passes.",
        steps: ["Run the smoke tests", "Tag the release"],
    },
    Canned {
        question: "Found 3 papers on BLE audio latency. Summarize all of them?",
        title: "Summarize 3 papers?",
        choices: ["Summarize all", "Newest only", "Skip"],
        recommended: 0,
        asks: "Three papers on BLE audio latency look relevant to the HFP path.",
        working: "Summarizing 3 papers",
        done: "Summary ready \u{b7} 3 papers",
        done_reply: "Summary written to research/ble-latency.md.",
        steps: ["Read the summary aloud", "File the notes"],
    },
    Canned {
        question: "terraform plan wants to replace the database. Apply it?",
        title: "Apply the terraform plan?",
        choices: ["Apply", "Plan only", "Abort"],
        recommended: 1,
        asks: "The plan replaces the RDS instance; that means downtime.",
        working: "Retrying the deploy",
        done: "Deploy green \u{b7} 12 resources",
        done_reply: "terraform apply finished; 12 resources in sync.",
        steps: ["Check the health dashboard", "Notify the team"],
    },
];

fn canned(agent: u8) -> Option<&'static Canned> {
    CANNED.get((agent as usize).wrapping_sub(1))
}

fn transcript_of(ctx: PttCtx) -> &'static str {
    match ctx {
        PttCtx::Answer => ANSWER_TRANSCRIPT,
        _ => TRANSCRIPT,
    }
}

fn working_title(agent: u8, job: Job, title: &mut Text<64>) {
    let canned = canned(agent);
    title.set(match job {
        Job::Talk => TALK_WORKING,
        Job::Question => canned.map_or(GENERIC_WORKING, |c| c.working),
        Job::Step(step) => canned
            .and_then(|c| c.steps.get(step as usize).copied())
            .unwrap_or(GENERIC_WORKING),
    });
}

fn done_title(agent: u8, job: Job, title: &mut Text<64>) {
    let canned = canned(agent);
    match job {
        Job::Talk => title.set(TALK_DONE),
        Job::Question => title.set(canned.map_or(GENERIC_DONE, |c| c.done)),
        Job::Step(step) => match canned.and_then(|c| c.steps.get(step as usize)) {
            Some(step) => {
                title.set(step);
                title.push_str(DONE_SUFFIX);
            }
            None => title.set(GENERIC_DONE),
        },
    }
}

fn done_reply(agent: u8, job: Job, reply: &mut Text<200>) {
    let canned = canned(agent);
    match job {
        Job::Talk => reply.set(TALK_DONE_REPLY),
        Job::Question => reply.set(canned.map_or(GENERIC_DONE, |c| c.done_reply)),
        Job::Step(step) => match canned.and_then(|c| c.steps.get(step as usize)) {
            Some(step) => {
                reply.set(DONE_PREFIX);
                reply.push_str(step);
                reply.push_char('.');
            }
            None => reply.set(GENERIC_DONE),
        },
    }
}

fn steps_of(agent: u8, job: Job) -> [&'static str; 2] {
    match (job, canned(agent)) {
        (Job::Talk, _) | (_, None) => TALK_STEPS,
        (_, Some(canned)) => canned.steps,
    }
}

fn agent_msg(id: u8, state: AgentState, title: Text<64>, out: &mut HostMsg) {
    *out = HostMsg::Agent {
        id,
        gone: false,
        kind: None,
        state: Some(state),
        name: None,
        project: None,
        title: Some(title),
        since_s: Some(0),
    };
}

fn clear_card(agent: u8, id: u16, kind: CardKind, out: &mut HostMsg) {
    *out = HostMsg::Card {
        agent,
        id,
        clear: true,
        kind,
        text: Text::new(),
        choices: [Text::new(); 4],
        choice_count: 0,
        recommended: None,
    };
}

fn card_msg(
    agent: u8,
    id: u16,
    kind: CardKind,
    text: &str,
    list: &[&str],
    recommended: Option<u8>,
    out: &mut HostMsg,
) {
    *out = HostMsg::Card {
        agent,
        id,
        clear: false,
        kind,
        text: Text::from_str(text),
        choices: [Text::new(); 4],
        choice_count: list.len().min(4) as u8,
        recommended,
    };
    if let HostMsg::Card { choices, .. } = out {
        for (slot, choice) in choices.iter_mut().zip(list) {
            slot.set(choice);
        }
    }
}

/// Seconds the mockups show as state age ("blocked 2m", "· 2m").
const AGE_S: u32 = 120;
/// The demo's two projects: the roster's first page holds the mockup's rows
/// (agents 1 to 3), the second the other two, so LEFT/RIGHT have somewhere
/// to go.
pub const PROJECTS: [&str; 2] = ["cohdl", "openmicro"];

#[allow(clippy::too_many_arguments)]
fn agent<'a>(
    deck: &'a mut Deck,
    id: u8,
    kind: AgentKind,
    name: &str,
    project: &str,
    state: AgentState,
    title: &str,
    age_s: u32,
) -> &'a mut Agent {
    let slot = &mut deck.agents[(id as usize - 1) % MAX_AGENTS];
    slot.id = id;
    slot.present = true;
    slot.stale = false;
    slot.kind = kind;
    slot.state = state;
    slot.name.set(name);
    slot.project.set(project);
    slot.title.set(title);
    slot.last_reply.clear();
    // Loaded at boot, when the clock reads 0: ages count from "before".
    slot.state_since_ms = 0u32.wrapping_sub(age_s * 1000);
    slot.has_card = false;
    slot
}

/// Fill `deck` with the mockup data, in place. Settings are left alone.
pub fn load(deck: &mut Deck) {
    deck.clear_host_data();

    let codex = agent(
        deck,
        1,
        AgentKind::Codex,
        "cohdl-api",
        PROJECTS[0],
        AgentState::Question,
        "Run migrations on staging?",
        AGE_S,
    );
    codex
        .last_reply
        .set("Tests fail on main: 3 pending migrations are not applied on staging.");
    codex.has_card = true;
    codex.card.id = 1;
    codex.card.kind = CardKind::Question;
    codex
        .card
        .text
        .set("Tests fail on main. Run pending migrations on staging first?");
    for (slot, choice) in ["Run on staging", "Skip migrations", "Abort task"]
        .into_iter()
        .enumerate()
    {
        codex.card.choices[slot].set(choice);
    }
    codex.card.choices[3].clear();
    codex.card.choice_count = 3;
    codex.card.recommended = Some(0);
    codex.card.pending_answer = None;
    codex.card.snoozed = false;
    codex.card.acknowledged = false;

    let claude = agent(
        deck,
        2,
        AgentKind::Claude,
        "web-ui",
        PROJECTS[0],
        AgentState::Ready,
        "Header refactor done \u{b7} 34 files",
        0,
    );
    claude
        .last_reply
        .set("Header refactor done, 34 files changed");
    claude.has_card = true;
    claude.card.id = 2;
    claude.card.kind = CardKind::Ready;
    claude.card.text.set("Header refactor done \u{b7} 34 files");
    for (slot, choice) in ["Run the test suite", "Open a pull request"]
        .into_iter()
        .enumerate()
    {
        claude.card.choices[slot].set(choice);
    }
    claude.card.choices[2].clear();
    claude.card.choices[3].clear();
    claude.card.choice_count = 2;
    claude.card.recommended = None;
    claude.card.pending_answer = None;
    claude.card.snoozed = false;
    claude.card.acknowledged = false;

    agent(
        deck,
        3,
        AgentKind::Codex,
        "firmware",
        PROJECTS[0],
        AgentState::Working,
        "Compiling openmicro",
        AGE_S,
    )
    .last_reply
    .set("Building the native image for both boards.");
    agent(
        deck,
        4,
        AgentKind::Gemini,
        "research",
        PROJECTS[1],
        AgentState::Idle,
        "Idle",
        0,
    );
    agent(
        deck,
        5,
        AgentKind::Claude,
        "infra",
        PROJECTS[1],
        AgentState::Error,
        "Deploy failed \u{b7} exit 1",
        0,
    )
    .last_reply
    .set("terraform apply exited with status 1.");
    // The mockup roster is the first project's page (agents 1 to 3); the
    // other two are the second page. Not sorted yet: the first re-sort puts
    // every class in its place.
    deck.order[..5].copy_from_slice(&[1, 2, 3, 4, 5]);
    deck.order_len = 5;

    // The pill of mockup 6: the question is waiting in the drawer.
    deck.notices[0] = Notice::new(1, 1, CardKind::Question, 0).filed();

    deck.workspaces.active = 2;
    deck.workspaces.count = 4;
    deck.workspaces.selected = 0;
    for (slot, (tone, title)) in [
        (Tone::Blue, "term \u{b7} claude \u{2014} header refactor"),
        (Tone::Amber, "firefox \u{2014} github \u{b7} pulls"),
        (Tone::Green, "term \u{b7} codex \u{2014} compiling..."),
    ]
    .into_iter()
    .enumerate()
    {
        deck.workspaces.windows[slot] = WindowRow::EMPTY;
        deck.workspaces.windows[slot].tone = tone;
        deck.workspaces.windows[slot].title.set(title);
    }
    deck.workspaces.window_count = 3;

    let keys = &mut deck.macros[0];
    keys.face = MACRO_FACE;
    keys.source.set("deck.toml");
    for (slot, (label, binding)) in [
        ("build", Binding::Left),
        ("test", Binding::Right),
        ("deploy", Binding::Enter),
        ("git push", Binding::ShiftLeft),
        ("screenshot", Binding::None),
        ("lock", Binding::None),
        ("mute", Binding::ShiftRight),
        ("term", Binding::ShiftEnter),
    ]
    .into_iter()
    .enumerate()
    {
        keys.keys[slot].label.set(label);
        keys.keys[slot].binding = binding;
        keys.keys[slot].present = true;
    }
    keys.keys[8].present = false;
    keys.keys[8].label.clear();
    keys.keys[8].binding = Binding::None;
    // Two keys with a keystroke for the host to post, as a layout would have.
    keys.keys[4].keys = crate::deck::msg::Keystroke::parse("cmd+shift+4").unwrap_or(crate::deck::msg::Keystroke::NONE);
    keys.keys[5].keys = crate::deck::msg::Keystroke::parse("ctrl+cmd+q").unwrap_or(crate::deck::msg::Keystroke::NONE);

    deck.usage.period.set("SEP 2026");
    for (slot, (name, percent, reset)) in [
        ("Claude Pro", 62, "resets in 4 days"),
        ("Codex \u{b7} OpenAI", 81, "resets tomorrow"),
        ("Gemini", 12, "resets in 11 days"),
    ]
    .into_iter()
    .enumerate()
    {
        deck.usage.items[slot] = UsageItem::EMPTY;
        deck.usage.items[slot].name.set(name);
        deck.usage.items[slot].percent = percent;
        deck.usage.items[slot].reset.set(reset);
    }
    deck.usage.count = 3;

    deck.clock_valid = true;
    deck.clock_minute = CLOCK_MINUTE;
}

/// Answer one device event like a real host would (answer -> card clear +
/// Working; key on Omarchy cycles canned workspaces; macro flash; refresh
/// nudges percentages; ptt talk -> transcript then reply; ptt name -> a canned
/// name; set -> nothing). True when `out` holds a message to apply NOW; later
/// consequences are queued in `ui.demo` and delivered by `on_tick`.
pub fn host(event: Event, ui: &mut UiState, now_ms: u32, out: &mut HostMsg) -> bool {
    *out = HostMsg::None;
    let UiState { deck, demo, .. } = ui;
    match event {
        Event::Answer {
            agent,
            card,
            choice,
        } => {
            let Some(pending) = deck.card(agent).filter(|pending| pending.id == card) else {
                return false;
            };
            let job = match pending.kind {
                CardKind::Question => Job::Question,
                CardKind::Ready => Job::Step(choice),
            };
            clear_card(agent, card, pending.kind, out);
            demo.push_orders(agent, job, ORDERS_WORKING, now_ms.wrapping_add(WORKING_MS));
            demo.push_question(agent, now_ms.wrapping_add(QUESTION_MS));
            demo.push_ready(agent, job, now_ms.wrapping_add(READY_MS));
            true
        }
        // SHIFT chords have no canned effect; releases are the host's business.
        Event::Key { shift: true, .. } => false,
        Event::Key { key, act, .. } => {
            if !demo.omarchy_key(key, act) {
                return false;
            }
            demo.workspaces_msg(out);
            true
        }
        Event::WsGo(ws) => {
            if !(1..=WORKSPACE_COUNT).contains(&ws) {
                return false;
            }
            demo.active_ws = ws;
            demo.workspaces_msg(out);
            true
        }
        Event::WsWin(row) => {
            let index = demo.workspace();
            if row >= window_count(index, demo.workspaces[index].terms) {
                return false;
            }
            demo.workspaces[index].selected = row;
            demo.workspaces_msg(out);
            true
        }
        Event::Refresh => {
            demo.refreshes = demo.refreshes.wrapping_add(1);
            usage_msg(demo.refreshes, out);
            true
        }
        Event::Ptt {
            act,
            agent,
            ctx,
            card,
            abort, ..
        } => ptt(deck, demo, act, agent, ctx, card, abort, now_ms, out),
        // The flash is local; the rest never reaches a demo host or needs no
        // answer. `DocLines` never even arrives: the demo's cards carry no
        // wrapped window, so nothing on them scrolls.
        Event::Image { .. }
        | Event::Macro { .. }
        | Event::MacroKey { .. }
        | Event::DocLines { .. }
        | Event::Tool { .. }
        | Event::Snooze { .. }
        | Event::Channel { .. }
        | Event::Face { .. }
        | Event::Faces
        | Event::Set { .. }
        | Event::Hello
        | Event::Up
        | Event::Session { .. }
        | Event::Err { .. } => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn ptt(
    deck: &Deck,
    demo: &mut Script,
    act: u8,
    agent: u8,
    ctx: PttCtx,
    card: u16,
    abort: bool,
    now_ms: u32,
    out: &mut HostMsg,
) -> bool {
    match (ctx, act) {
        (PttCtx::Name, 1) => false,
        (PttCtx::Name, 0) => {
            if abort {
                return false;
            }
            *out = HostMsg::Name {
                text: Text::from_str(FACE_NAME),
            };
            true
        }
        (_, 1) => {
            // Nothing heard yet: the transcript is typed by `on_tick`.
            if deck.agent(agent).is_some() {
                demo.typing = Some(Typing {
                    agent,
                    ctx,
                    started_ms: now_ms,
                    shown: 0,
                });
            }
            false
        }
        (_, 0) => {
            demo.typing = None;
            if abort || deck.agent(agent).is_none() {
                return false;
            }
            *out = HostMsg::Transcript {
                agent,
                text: Text::from_str(transcript_of(ctx)),
                last: true, press: None, failed: false, channel_tip: false, rec: false,
            };
            match ctx {
                PttCtx::Talk => {
                    demo.push_orders(
                        agent,
                        Job::Talk,
                        ORDERS_REPLY,
                        now_ms.wrapping_add(REPLY_MS),
                    );
                    demo.push_ready(agent, Job::Talk, now_ms.wrapping_add(READY_MS));
                }
                _ => {
                    // A voice answer resolves the card it was latched on.
                    let phase = if deck.card(agent).is_some_and(|pending| pending.id == card) {
                        ORDERS_CLEAR
                    } else {
                        ORDERS_WORKING
                    };
                    demo.push_orders(agent, Job::Question, phase, now_ms.wrapping_add(CLEAR_MS));
                    demo.push_question(agent, now_ms.wrapping_add(QUESTION_MS));
                    demo.push_ready(agent, Job::Question, now_ms.wrapping_add(READY_MS));
                }
            }
            true
        }
        _ => false,
    }
}

/// Deliver queued consequences that are due (through `ui.host`). Called by
/// `UiState::tick` while `deck.demo_active()`. Nothing advances while
/// `!screen_on`: the dark time is added to every due time on the next lit
/// tick. `out.wake` needs no care: `UiState` drops it while the demo is
/// active.
///
/// `host` returns ONE message per event. An event with several consequences
/// (answer -> `card clear`, then `agent state:working`) returns the first and
/// queues the rest in `Script`; this delivers them one per tick, which comes
/// at the animation rate because `animating` reports the queue. A `HostMsg`
/// built here lives on the stack: one at a time (<= 512 bytes), never an
/// array of them.
pub fn on_tick(ui: &mut UiState, now_ms: u32, screen_on: bool, out: &mut Update) {
    if !screen_on {
        if ui.demo.off_since.is_none() {
            ui.demo.off_since = Some(now_ms);
        }
        return;
    }
    if let Some(off) = ui.demo.off_since.take() {
        let dark = now_ms.wrapping_sub(off);
        for entry in ui.demo.queue.iter_mut().flatten() {
            entry.due_ms = entry.due_ms.wrapping_add(dark);
        }
        if let Some(typing) = &mut ui.demo.typing {
            typing.started_ms = typing.started_ms.wrapping_add(dark);
        }
    }
    if type_transcript(ui, now_ms, out) {
        return;
    }
    if let Some(index) = ui.demo.due(now_ms) {
        deliver(ui, index, now_ms, out);
    }
}

/// The next characters of the transcript, if their time has come.
fn type_transcript(ui: &mut UiState, now_ms: u32, out: &mut Update) -> bool {
    let Some(typing) = ui.demo.typing else {
        return false;
    };
    // The session ended without a release we heard (link drop): stop.
    if ui.global.ptt.is_none() {
        ui.demo.typing = None;
        return false;
    }
    let text = transcript_of(typing.ctx);
    let total = text.chars().count().min(u8::MAX as usize) as u8;
    let elapsed = now_ms.wrapping_sub(typing.started_ms);
    if elapsed < HEAR_MS || elapsed > u32::MAX / 2 {
        return false;
    }
    let target = ((elapsed - HEAR_MS) / CHAR_MS + 1).min(total as u32) as u8;
    if target <= typing.shown {
        return false;
    }
    let end = text
        .char_indices()
        .nth(target as usize)
        .map_or(text.len(), |(index, _)| index);
    let msg = HostMsg::Transcript {
        agent: typing.agent,
        text: Text::from_str(&text[..end]),
        last: false,
        press: None,
        failed: false, channel_tip: false, rec: false,
    };
    ui.demo.typing = Some(Typing {
        shown: target,
        ..typing
    });
    ui.host(&msg, now_ms, out);
    true
}

/// Deliver the entry at `index`: one message, then its next phase (if any)
/// takes its place in the queue.
fn deliver(ui: &mut UiState, index: usize, now_ms: u32, out: &mut Update) {
    let Some(entry) = ui.demo.queue.get(index).copied().flatten() else {
        return;
    };
    let mut msg = HostMsg::None;
    let rest = {
        let UiState { deck, demo, .. } = &mut *ui;
        build(demo, deck, entry.what, &mut msg)
    };
    if let Some(slot) = ui.demo.queue.get_mut(index) {
        // A phase due at once keeps the entry's place in the order: what was
        // due first finishes first.
        *slot = rest.map(|(what, delay_ms)| Entry {
            what,
            due_ms: if delay_ms == 0 {
                entry.due_ms
            } else {
                now_ms.wrapping_add(delay_ms)
            },
        });
    }
    if !matches!(msg, HostMsg::None) {
        ui.host(&msg, now_ms, out);
    }
}

/// Build the message of one due follow-up; returns what remains of it and
/// how long that waits (0 = at once).
fn build(
    demo: &mut Script,
    deck: &Deck,
    what: Pending,
    out: &mut HostMsg,
) -> Option<(Pending, u32)> {
    match what {
        Pending::Orders { agent, job, phase } => {
            deck.agent(agent)?;
            let next = |phase, delay_ms| Some((Pending::Orders { agent, job, phase }, delay_ms));
            match phase {
                ORDERS_REPLY => {
                    *out = HostMsg::Reply {
                        agent,
                        text: Text::from_str(TALK_REPLY),
                        rev: 0,
                    };
                    if deck.card(agent).is_some() {
                        next(ORDERS_CLEAR, CLEAR_MS)
                    } else {
                        next(ORDERS_WORKING, WORKING_MS)
                    }
                }
                ORDERS_CLEAR => {
                    if let Some(card) = deck.card(agent) {
                        clear_card(agent, card.id, card.kind, out);
                    }
                    next(ORDERS_WORKING, WORKING_MS)
                }
                _ => {
                    let mut title = Text::new();
                    working_title(agent, job, &mut title);
                    agent_msg(agent, AgentState::Working, title, out);
                    None
                }
            }
        }
        Pending::Question {
            not,
            agent,
            card,
            phase,
        } => {
            let next = |agent, card, phase| {
                Some((
                    Pending::Question {
                        not,
                        agent,
                        card,
                        phase,
                    },
                    0,
                ))
            };
            match phase {
                0 => {
                    let agent = demo.pick_asker(deck, not)?;
                    let canned = canned(agent)?;
                    agent_msg(
                        agent,
                        AgentState::Question,
                        Text::from_str(canned.title),
                        out,
                    );
                    next(agent, demo.new_card(), 1)
                }
                1 => {
                    let canned = canned(agent)?;
                    *out = HostMsg::Reply {
                        agent,
                        text: Text::from_str(canned.asks),
                        rev: 0,
                    };
                    next(agent, card, 2)
                }
                _ => {
                    let canned = canned(agent)?;
                    card_msg(
                        agent,
                        card,
                        CardKind::Question,
                        canned.question,
                        &canned.choices,
                        Some(canned.recommended),
                        out,
                    );
                    None
                }
            }
        }
        Pending::Ready {
            agent,
            job,
            card,
            phase,
        } => {
            deck.agent(agent)?;
            let next = |card, phase| {
                Some((
                    Pending::Ready {
                        agent,
                        job,
                        card,
                        phase,
                    },
                    0,
                ))
            };
            match phase {
                0 => {
                    let mut title = Text::new();
                    done_title(agent, job, &mut title);
                    agent_msg(agent, AgentState::Ready, title, out);
                    next(demo.new_card(), 1)
                }
                1 => {
                    let mut text = Text::new();
                    done_reply(agent, job, &mut text);
                    *out = HostMsg::Reply { agent, text, rev: 0 };
                    next(card, 2)
                }
                _ => {
                    let mut title = Text::new();
                    done_title(agent, job, &mut title);
                    card_msg(
                        agent,
                        card,
                        CardKind::Ready,
                        title.as_str(),
                        &steps_of(agent, job),
                        None,
                        out,
                    );
                    None
                }
            }
        }
    }
}

/// The script has something to deliver soon (a queued follow-up message, a
/// transcript or reply being typed out): tick at the animation rate. Consulted
/// by `UiState::animating` only while `deck.demo_active()`. Long waits (the
/// next question 20 s after an answer) do NOT report true.
pub fn animating(ui: &UiState) -> bool {
    let script = &ui.demo;
    if script.off_since.is_some() {
        return false;
    }
    let typing = script
        .typing
        .is_some_and(|typing| (typing.shown as usize) < transcript_of(typing.ctx).chars().count());
    typing
        || script.queue.iter().flatten().any(|entry| {
            ui.now_ms.wrapping_sub(entry.due_ms) < u32::MAX / 2
                || entry.due_ms.wrapping_sub(ui.now_ms) < SOON_MS
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    #[test]
    fn demo_data_reproduces_the_mockup_strings() {
        let mut deck = std::boxed::Box::new(Deck::new());
        load(&mut deck);
        let rows: std::vec::Vec<_> = deck.order[..4]
            .iter()
            .map(|&id| deck.agent(id).unwrap())
            .map(|agent| {
                (
                    agent.kind,
                    agent.name.as_str(),
                    agent.state,
                    agent.title.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    AgentKind::Codex,
                    "cohdl-api",
                    AgentState::Question,
                    "Run migrations on staging?"
                ),
                (
                    AgentKind::Claude,
                    "web-ui",
                    AgentState::Ready,
                    "Header refactor done · 34 files"
                ),
                (
                    AgentKind::Codex,
                    "firmware",
                    AgentState::Working,
                    "Compiling openmicro"
                ),
                (AgentKind::Gemini, "research", AgentState::Idle, "Idle"),
            ]
        );
        assert_eq!(deck.agent(5).unwrap().state, AgentState::Error);
        let card = deck.card(1).unwrap();
        assert_eq!(
            card.text,
            "Tests fail on main. Run pending migrations on staging first?"
        );
        assert_eq!(card.choices[0], "Run on staging");
        assert_eq!(card.choices[1], "Skip migrations");
        assert_eq!(card.choices[2], "Abort task");
        assert_eq!((card.choice_count, card.recommended), (3, Some(0)));
        assert_eq!(deck.card(2).unwrap().kind, CardKind::Ready);
        assert_eq!(
            deck.agent(2).unwrap().last_reply,
            "Header refactor done, 34 files changed"
        );
        assert_eq!(
            deck.workspaces.windows[0].title,
            "term · claude — header refactor"
        );
        assert_eq!((deck.workspaces.active, deck.workspaces.count), (2, 4));
        let keys = deck.macro_set(MACRO_FACE).unwrap();
        assert_eq!(keys.source, "deck.toml");
        assert_eq!(keys.keys[3].label, "git push");
        assert!(keys.keys[4].present && keys.keys[4].binding == Binding::None);
        assert!(!keys.keys[8].present);
        assert_eq!(deck.usage.period, "SEP 2026");
        assert_eq!(deck.usage.items[1].name, "Codex · OpenAI");
        assert_eq!(deck.usage.items[1].percent, 81);
        assert_eq!((deck.clock_valid, deck.clock_minute), (true, 1182));
        assert!(deck.drawer_has_question());
        // Ages read as the mockups' "2m" at boot.
        assert_eq!(
            0u32.wrapping_sub(deck.agent(1).unwrap().state_since_ms) / 60_000,
            2
        );
    }

    #[test]
    fn loading_twice_is_idempotent_and_keeps_settings() {
        let mut deck = std::boxed::Box::new(Deck::new());
        deck.settings.brightness = 2;
        load(&mut deck);
        let first = *deck;
        load(&mut deck);
        assert!(first == *deck);
        assert_eq!(deck.settings.brightness, 2);
    }

    // ---- the responder ---------------------------------------------------------

    fn fresh() -> Box<UiState> {
        let mut ui = Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        assert!(ui.deck.demo_active());
        // The displayed clock is not the script's doing: keep it still.
        ui.deck.clock_valid = false;
        ui
    }

    /// One event in, the immediate answer (if any) applied; the answer is
    /// handed back for inspection.
    fn event(ui: &mut UiState, event: Event, now_ms: u32) -> Option<Box<HostMsg>> {
        let mut msg = Box::new(HostMsg::None);
        let answered = host(event, ui, now_ms, &mut msg);
        assert_eq!(answered, !matches!(*msg, HostMsg::None));
        if !answered {
            return None;
        }
        let mut out = Update::new();
        ui.host(&msg, now_ms, &mut out);
        assert!(!out.wake);
        Some(msg)
    }

    /// What the script can change (the notice ring is the toast's business).
    fn snapshot(
        ui: &UiState,
    ) -> Box<(
        [Agent; MAX_AGENTS],
        crate::deck::Workspaces,
        crate::deck::Usage,
        Text<200>,
        bool,
    )> {
        Box::new((
            ui.deck.agents,
            ui.deck.workspaces,
            ui.deck.usage,
            ui.deck.transcript,
            ui.deck.transcript_final,
        ))
    }

    /// One lit tick; true when the script changed the deck.
    fn tick(ui: &mut UiState, now_ms: u32) -> bool {
        let before = snapshot(ui);
        let mut out = Update::new();
        ui.tick(now_ms, 0, true, &mut out);
        assert!(!out.wake);
        let changed = snapshot(ui) != before;
        assert!(out.dirty || !changed);
        changed
    }

    fn answer(agent: u8, card: u16, choice: u8) -> Event {
        Event::Answer {
            agent,
            card,
            choice,
        }
    }

    fn ptt_event(act: u8, agent: u8, ctx: PttCtx, card: u16, abort: bool) -> Event {
        Event::Ptt {
            act,
            agent,
            ctx,
            card,
            abort, press: 0,
        }
    }

    fn key(key: KeyId, act: u8, shift: bool) -> Event {
        Event::Key {
            key,
            act,
            shift,
            face: 2,
        }
    }

    fn titles(ui: &UiState) -> Vec<&str> {
        ui.deck.workspaces.windows[..ui.deck.workspaces.window_count as usize]
            .iter()
            .map(|row| row.title.as_str())
            .collect()
    }

    #[test]
    fn an_answer_clears_the_card_then_puts_the_agent_to_work() {
        let mut ui = fresh();
        let msg = event(&mut ui, answer(1, 1, 0), 1000).unwrap();
        assert!(matches!(
            *msg,
            HostMsg::Card {
                agent: 1,
                id: 1,
                clear: true,
                ..
            }
        ));
        assert!(ui.deck.card(1).is_none());
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Question);
        // The immediate follow-up asks for animation ticks; the 20 s one does not.
        assert!(animating(&ui));
        assert!(!tick(&mut ui, 1000 + WORKING_MS - 1));
        assert!(tick(&mut ui, 1000 + WORKING_MS));
        let agent = ui.deck.agent(1).unwrap();
        assert_eq!(agent.state, AgentState::Working);
        assert_eq!(agent.title, "Applying migrations on staging");
        assert_eq!(agent.state_since_ms, 1000 + WORKING_MS);
        assert!(!animating(&ui));
        assert_eq!(ui.demo.queued(), 2);
    }

    #[test]
    fn twenty_seconds_after_an_answer_another_agent_asks_a_fresh_question() {
        let mut ui = fresh();
        event(&mut ui, answer(1, 1, 0), 1000);
        tick(&mut ui, 1000 + WORKING_MS);
        assert!(!tick(&mut ui, 1000 + QUESTION_MS - 5000));
        assert!(!animating(&ui), "a 5 s wait is polled, not animated");
        assert!(!tick(&mut ui, 1000 + QUESTION_MS - 1));
        assert!(
            animating(&ui),
            "within a second the ticks come at the animation rate"
        );
        // Agent 2 holds the Ready card, so the firmware agent is next in line.
        assert!(tick(&mut ui, 1000 + QUESTION_MS));
        assert_eq!(ui.deck.agent(3).unwrap().state, AgentState::Question);
        assert_eq!(ui.deck.agent(3).unwrap().title, "Flash the LilyGo now?");
        assert!(ui.deck.card(3).is_none());
        assert!(animating(&ui));
        assert!(tick(&mut ui, 1000 + QUESTION_MS + 1));
        assert_eq!(ui.deck.agent(3).unwrap().last_reply, CANNED[2].asks);
        assert!(tick(&mut ui, 1000 + QUESTION_MS + 2));
        let card = ui.deck.card(3).unwrap();
        assert_eq!(
            (card.id, card.kind, card.choice_count),
            (FIRST_CARD, CardKind::Question, 3)
        );
        assert_eq!(card.text, CANNED[2].question);
        assert_eq!(card.choices[1], "Run tests first");
        assert_eq!(card.recommended, Some(0));
        assert!(ui
            .deck
            .notices
            .iter()
            .any(|notice| notice.used && (notice.agent, notice.card) == (3, FIRST_CARD)));
        assert!(!animating(&ui));
        for now in [1000 + READY_MS, 1001 + READY_MS, 1002 + READY_MS] {
            assert!(tick(&mut ui, now));
        }
        // Answering that one moves the round robin on: research asks next.
        event(&mut ui, answer(3, FIRST_CARD, 1), 40_000);
        for now in [
            40_000 + WORKING_MS,
            40_000 + QUESTION_MS,
            40_001 + QUESTION_MS,
            40_002 + QUESTION_MS,
        ] {
            assert!(tick(&mut ui, now), "{now}");
        }
        let card = ui.deck.card(4).unwrap();
        assert_eq!(
            (card.id, ui.deck.agent(4).unwrap().state),
            (FIRST_CARD + 2, AgentState::Question)
        );
    }

    #[test]
    fn the_agent_that_got_orders_reports_ready_with_suggested_steps() {
        let mut ui = fresh();
        event(&mut ui, answer(1, 1, 0), 1000);
        tick(&mut ui, 1000 + WORKING_MS);
        for now in [1000 + QUESTION_MS, 1001 + QUESTION_MS, 1002 + QUESTION_MS] {
            tick(&mut ui, now);
        }
        assert!(!tick(&mut ui, 1000 + READY_MS - 1));
        assert!(tick(&mut ui, 1000 + READY_MS));
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Ready);
        assert!(tick(&mut ui, 1001 + READY_MS));
        assert_eq!(ui.deck.agent(1).unwrap().last_reply, CANNED[0].done_reply);
        assert!(tick(&mut ui, 1002 + READY_MS));
        let card = ui.deck.card(1).unwrap();
        assert_eq!(
            (card.kind, card.choice_count, card.recommended),
            (CardKind::Ready, 2, None)
        );
        assert_eq!(card.text, "Migrations applied · tests green");
        assert_eq!(card.choices[0], "Deploy the API to staging");
        assert_eq!(card.id, FIRST_CARD + 1);
        assert_eq!(ui.demo.queued(), 0);
        // A step chosen on that card becomes the agent's title.
        event(&mut ui, answer(1, FIRST_CARD + 1, 1), 50_000);
        assert!(tick(&mut ui, 50_000 + WORKING_MS));
        assert_eq!(ui.deck.agent(1).unwrap().title, "Open a pull request");
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Working);
    }

    #[test]
    fn a_ready_card_step_is_answered_like_a_question() {
        let mut ui = fresh();
        let msg = event(&mut ui, answer(2, 2, 0), 500).unwrap();
        assert!(matches!(
            *msg,
            HostMsg::Card {
                agent: 2,
                id: 2,
                clear: true,
                kind: CardKind::Ready,
                ..
            }
        ));
        assert!(tick(&mut ui, 500 + WORKING_MS));
        let agent = ui.deck.agent(2).unwrap();
        assert_eq!(
            (agent.state, agent.title.as_str()),
            (AgentState::Working, "Run the test suite")
        );
        // 35 s later it is Ready again, reporting that step.
        for now in [500 + QUESTION_MS, 501 + QUESTION_MS, 502 + QUESTION_MS] {
            tick(&mut ui, now);
        }
        for now in [500 + READY_MS, 501 + READY_MS, 502 + READY_MS] {
            assert!(tick(&mut ui, now));
        }
        let agent = ui.deck.agent(2).unwrap();
        assert_eq!(agent.title, "Run the test suite · done");
        assert_eq!(agent.last_reply, "Done: Run the test suite.");
        assert_eq!(agent.card.text, "Run the test suite · done");
    }

    #[test]
    fn an_answer_for_a_card_the_host_does_not_have_is_ignored() {
        let mut ui = fresh();
        assert!(event(&mut ui, answer(1, 7, 0), 0).is_none());
        assert!(event(&mut ui, answer(3, 1, 0), 0).is_none());
        assert!(event(&mut ui, answer(0, 1, 0), 0).is_none());
        assert!(event(&mut ui, answer(9, 1, 0), 0).is_none());
        assert_eq!(ui.demo.queued(), 0);
        assert!(!animating(&ui));
    }

    #[test]
    fn events_a_demo_host_has_nothing_to_say_to() {
        let mut ui = fresh();
        for event_ in [
            Event::Snooze { agent: 1, card: 1 },
            Event::Macro {
                face: MACRO_FACE,
                index: 2,
                act: 1,
            },
            Event::Macro {
                face: MACRO_FACE,
                index: 2,
                act: 0,
            },
            Event::Channel { agent: 3, seq: 4 },
            Event::Face { id: 2 },
            Event::Faces,
            Event::Set {
                key: crate::deck::msg::SettingKey::Brightness,
                value: 3,
            },
            Event::Hello,
            Event::Up,
            Event::Err {
                why: crate::deck::msg::ErrWhy::Parse,
            },
            ptt_event(1, 0, PttCtx::Name, 0, false),
            ptt_event(0, 0, PttCtx::Name, 0, true),
            key(KeyId::Right, 1, true),
            key(KeyId::Enter, 1, true),
            key(KeyId::Cancel, 1, false),
            key(KeyId::Right, 0, false),
            key(KeyId::Cw, 1, false),
            Event::WsGo(0),
            Event::WsGo(5),
            Event::WsWin(3),
        ] {
            assert!(event(&mut ui, event_, 10).is_none(), "{event_:?}");
        }
        assert_eq!(ui.demo.queued(), 0);
        assert!(!ui.demo.typing() && !animating(&ui));
        assert!(!tick(&mut ui, 5000));
    }

    #[test]
    fn omarchy_keys_cycle_the_canned_workspaces() {
        let mut ui = fresh();
        let msg = event(&mut ui, key(KeyId::Right, 1, false), 0).unwrap();
        assert!(matches!(
            *msg,
            HostMsg::Workspaces {
                active: 3,
                count: 4,
                selected: 0,
                window_count: 2,
                ..
            }
        ));
        assert_eq!(
            titles(&ui),
            ["term · gemini — research notes", "obsidian — deck spec"]
        );
        assert_eq!(ui.deck.workspaces.windows[0].tone, Tone::Green);
        event(&mut ui, key(KeyId::Right, 1, false), 1);
        event(&mut ui, key(KeyId::Right, 1, false), 2);
        assert_eq!(ui.deck.workspaces.active, 1);
        assert_eq!(titles(&ui)[0], "term · nvim — openwalkietalkie");
        event(&mut ui, key(KeyId::Left, 1, false), 3);
        assert_eq!(ui.deck.workspaces.active, 4);
        assert_eq!(ui.deck.workspaces.windows[0].tone, Tone::Red);
        event(&mut ui, key(KeyId::Left, 1, false), 4);
        event(&mut ui, key(KeyId::Left, 1, false), 5);
        assert_eq!(ui.deck.workspaces.active, 2);
        assert_eq!(
            titles(&ui),
            [
                "term · claude — header refactor",
                "firefox — github · pulls",
                "term · codex — compiling..."
            ]
        );
        // Touch on a chip goes straight there.
        event(&mut ui, Event::WsGo(4), 6);
        assert_eq!(ui.deck.workspaces.active, 4);
        assert_eq!(ui.demo.queued(), 0);
    }

    #[test]
    fn the_wheel_moves_the_selection_and_enter_adds_bounded_terminals() {
        let mut ui = fresh();
        event(&mut ui, key(KeyId::Cw, 2, false), 0);
        assert_eq!(ui.deck.workspaces.selected, 1);
        event(&mut ui, key(KeyId::Cw, 2, false), 1);
        event(&mut ui, key(KeyId::Cw, 2, false), 2);
        assert_eq!(ui.deck.workspaces.selected, 2, "hard end");
        event(&mut ui, key(KeyId::Ccw, 2, false), 3);
        event(&mut ui, key(KeyId::Ccw, 2, false), 4);
        event(&mut ui, key(KeyId::Ccw, 2, false), 5);
        assert_eq!(ui.deck.workspaces.selected, 0, "hard end");
        for n in 0..10 {
            event(&mut ui, key(KeyId::Enter, 1, false), 10 + n);
        }
        assert_eq!(ui.deck.workspaces.window_count, MAX_WINDOWS);
        assert_eq!(titles(&ui)[3..], ["term · zsh", "term · zsh", "term · zsh"]);
        assert_eq!(
            ui.deck.workspaces.selected, 5,
            "the newest terminal has the focus"
        );
        // Each workspace keeps its own list and selection.
        event(&mut ui, Event::WsWin(1), 20);
        assert_eq!(ui.deck.workspaces.selected, 1);
        event(&mut ui, key(KeyId::Right, 1, false), 21);
        assert_eq!(
            (
                ui.deck.workspaces.active,
                ui.deck.workspaces.window_count,
                ui.deck.workspaces.selected
            ),
            (3, 2, 0)
        );
        event(&mut ui, key(KeyId::Left, 1, false), 22);
        assert_eq!(
            (ui.deck.workspaces.window_count, ui.deck.workspaces.selected),
            (6, 1)
        );
    }

    #[test]
    fn refresh_nudges_the_usage_bars_within_bounds() {
        let mut ui = fresh();
        let before: Vec<u8> = ui.deck.usage.items[..3]
            .iter()
            .map(|item| item.percent)
            .collect();
        let mut seen = Vec::new();
        for n in 0..300u32 {
            let msg = event(&mut ui, Event::Refresh, n).unwrap();
            assert!(matches!(*msg, HostMsg::Usage { count: 3, .. }));
            let now: Vec<u8> = ui.deck.usage.items[..3]
                .iter()
                .map(|item| item.percent)
                .collect();
            assert!(now.iter().all(|&percent| percent < 100));
            assert!(now
                .iter()
                .zip(&before)
                .all(|(now, before)| now.abs_diff(*before) <= 2));
            if !seen.contains(&now) {
                seen.push(now);
            }
        }
        assert!(seen.len() >= 3, "every refresh visibly moves the bars");
        assert_eq!(ui.deck.usage.period, "SEP 2026");
        assert_eq!(ui.deck.usage.items[1].name, "Codex · OpenAI");
        assert_eq!(ui.deck.usage.items[2].reset, "resets in 11 days");
        assert_eq!(ui.demo.queued(), 0);
    }

    fn hold_ptt(ui: &mut UiState, agent: u8, ctx: PttCtx, card: u16, now_ms: u32) {
        ui.global.ptt = Some(crate::ui::state::PttSession::new(agent, ctx, card, now_ms));
        ui.deck.transcript.clear();
        assert!(event(ui, ptt_event(1, agent, ctx, card, false), now_ms).is_none());
        assert!(ui.demo.typing() && animating(ui));
    }

    #[test]
    fn ptt_talk_types_the_transcript_out_then_the_agent_replies() {
        let mut ui = fresh();
        hold_ptt(&mut ui, 3, PttCtx::Talk, 0, 1000);
        assert!(!tick(&mut ui, 1000 + HEAR_MS - 1));
        assert!(ui.deck.transcript.is_empty());
        let mut shown = 0;
        let mut now = 1000 + HEAR_MS;
        let mut messages = 0;
        while shown < TRANSCRIPT.len() {
            if tick(&mut ui, now) {
                messages += 1;
                let text = ui.deck.transcript.as_str();
                assert!(
                    text.len() > shown && TRANSCRIPT.starts_with(text),
                    "{text:?}"
                );
                assert!(!ui.deck.transcript_final);
                shown = text.len();
            }
            now += 16;
            assert!(now < 1000 + HEAR_MS + CHAR_MS * 60, "typing never finished");
        }
        assert!(messages >= 20, "typed progressively, not in one go");
        assert!(
            !animating(&ui),
            "fully typed: nothing to animate until the release"
        );
        assert!(!tick(&mut ui, now + 5000));
        // Release: the final transcript now, the reply a beat later.
        ui.global.ptt = None;
        let msg = event(&mut ui, ptt_event(0, 3, PttCtx::Talk, 0, false), now + 5000).unwrap();
        assert!(matches!(
            *msg,
            HostMsg::Transcript {
                agent: 3,
                last: true,
                ..
            }
        ));
        assert_eq!(ui.deck.transcript, TRANSCRIPT);
        assert!(ui.deck.transcript_final && !ui.demo.typing());
        assert!(animating(&ui));
        assert!(!tick(&mut ui, now + 5000 + REPLY_MS - 1));
        assert!(tick(&mut ui, now + 5000 + REPLY_MS));
        assert_eq!(ui.deck.agent(3).unwrap().last_reply, TALK_REPLY);
        assert!(tick(&mut ui, now + 5000 + REPLY_MS + WORKING_MS));
        let agent = ui.deck.agent(3).unwrap();
        assert_eq!(
            (agent.state, agent.title.as_str()),
            (AgentState::Working, TALK_WORKING)
        );
        assert!(!animating(&ui));
        // Its report, 35 s after the orders: Ready with the dictation's steps.
        for at in [READY_MS, READY_MS + 1, READY_MS + 2] {
            assert!(tick(&mut ui, now + 5000 + at));
        }
        let agent = ui.deck.agent(3).unwrap();
        assert_eq!(
            (agent.state, agent.title.as_str()),
            (AgentState::Ready, TALK_DONE)
        );
        assert_eq!(agent.card.choices[0], TALK_STEPS[0]);
        assert_eq!(ui.demo.queued(), 0);
    }

    #[test]
    fn a_dictation_to_an_agent_with_a_card_clears_it_and_the_error_agent_recovers() {
        let mut ui = fresh();
        assert_eq!(ui.deck.agent(5).unwrap().state, AgentState::Error);
        hold_ptt(&mut ui, 5, PttCtx::Talk, 0, 100);
        ui.global.ptt = None;
        event(&mut ui, ptt_event(0, 5, PttCtx::Talk, 0, false), 200);
        assert!(tick(&mut ui, 200 + REPLY_MS));
        assert!(tick(&mut ui, 200 + REPLY_MS + WORKING_MS));
        assert_eq!(ui.deck.agent(5).unwrap().state, AgentState::Working);
        // The Ready agent, told what to do next, drops its Ready card first.
        hold_ptt(&mut ui, 2, PttCtx::Talk, 0, 2000);
        ui.global.ptt = None;
        event(&mut ui, ptt_event(0, 2, PttCtx::Talk, 0, false), 2100);
        assert!(tick(&mut ui, 2100 + REPLY_MS));
        assert!(ui.deck.card(2).is_some());
        assert!(tick(&mut ui, 2100 + REPLY_MS + CLEAR_MS));
        assert!(ui.deck.card(2).is_none());
        assert!(tick(&mut ui, 2100 + REPLY_MS + CLEAR_MS + WORKING_MS));
        assert_eq!(ui.deck.agent(2).unwrap().state, AgentState::Working);
    }

    #[test]
    fn an_aborted_ptt_leaves_no_trace() {
        let mut ui = fresh();
        hold_ptt(&mut ui, 3, PttCtx::Talk, 0, 1000);
        assert!(tick(&mut ui, 1000 + HEAR_MS + CHAR_MS * 5));
        ui.global.ptt = None;
        assert!(event(&mut ui, ptt_event(0, 3, PttCtx::Talk, 0, true), 2000).is_none());
        assert!(!ui.demo.typing() && !animating(&ui));
        assert_eq!(ui.demo.queued(), 0);
        assert!(!tick(&mut ui, 60_000));
        assert_eq!(ui.deck.agent(3).unwrap().state, AgentState::Working);
    }

    #[test]
    fn a_session_that_vanished_stops_the_typing() {
        let mut ui = fresh();
        hold_ptt(&mut ui, 3, PttCtx::Talk, 0, 1000);
        assert!(tick(&mut ui, 1000 + HEAR_MS));
        // The link dropped: global forgot the session without a release.
        ui.global.ptt = None;
        assert!(!tick(&mut ui, 1000 + HEAR_MS + CHAR_MS * 3));
        assert!(!ui.demo.typing() && !animating(&ui));
    }

    #[test]
    fn a_voice_answer_resolves_the_card_on_release() {
        let mut ui = fresh();
        hold_ptt(&mut ui, 1, PttCtx::Answer, 1, 1000);
        assert!(tick(&mut ui, 1000 + HEAR_MS));
        assert!(ANSWER_TRANSCRIPT.starts_with(ui.deck.transcript.as_str()));
        ui.global.ptt = None;
        let msg = event(&mut ui, ptt_event(0, 1, PttCtx::Answer, 1, false), 3000).unwrap();
        assert!(matches!(
            *msg,
            HostMsg::Transcript {
                agent: 1,
                last: true,
                ..
            }
        ));
        assert_eq!(ui.deck.transcript, ANSWER_TRANSCRIPT);
        assert!(ui.deck.card(1).is_some());
        assert!(!tick(&mut ui, 3000 + CLEAR_MS - 1));
        assert!(tick(&mut ui, 3000 + CLEAR_MS));
        assert!(ui.deck.card(1).is_none());
        assert!(tick(&mut ui, 3000 + CLEAR_MS + WORKING_MS));
        let agent = ui.deck.agent(1).unwrap();
        assert_eq!(
            (agent.state, agent.title.as_str()),
            (AgentState::Working, CANNED[0].working)
        );
        assert_eq!(
            agent.last_reply, CANNED[0].asks,
            "a voice answer gets no reply"
        );
        // The same follow-ups as a button answer.
        assert_eq!(ui.demo.queued(), 2);
        assert!(tick(&mut ui, 3000 + QUESTION_MS));
        assert_eq!(ui.deck.agent(3).unwrap().state, AgentState::Question);
    }

    #[test]
    fn ptt_in_naming_yields_the_canned_name_on_release() {
        let mut ui = fresh();
        assert!(event(&mut ui, ptt_event(1, 0, PttCtx::Name, 0, false), 0).is_none());
        assert!(!ui.demo.typing() && !animating(&ui));
        let msg = event(&mut ui, ptt_event(0, 0, PttCtx::Name, 0, false), 900).unwrap();
        let HostMsg::Name { text } = *msg else {
            panic!("{msg:?}");
        };
        assert_eq!(text, FACE_NAME);
        assert_eq!(ui.demo.queued(), 0);
    }

    #[test]
    fn follow_ups_arrive_one_per_tick_earliest_first() {
        let mut ui = fresh();
        event(&mut ui, answer(1, 1, 0), 1000);
        // Long after everything is due: still one message per tick, in order.
        assert!(tick(&mut ui, 100_000));
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Working);
        assert_eq!(ui.deck.agent(3).unwrap().state, AgentState::Working);
        assert!(animating(&ui), "overdue work ticks at the animation rate");
        assert!(tick(&mut ui, 100_001));
        assert_eq!(ui.deck.agent(3).unwrap().state, AgentState::Question);
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Working);
        assert!(tick(&mut ui, 100_002));
        assert!(tick(&mut ui, 100_003));
        assert!(ui.deck.card(3).is_some());
        assert!(tick(&mut ui, 100_004));
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Ready);
        assert!(tick(&mut ui, 100_005));
        assert!(tick(&mut ui, 100_006));
        assert!(ui.deck.card(1).is_some());
        assert!(!tick(&mut ui, 100_007));
        assert_eq!(ui.demo.queued(), 0);
    }

    #[test]
    fn nothing_advances_while_the_screen_is_off_and_the_dark_time_is_not_counted() {
        let mut ui = fresh();
        event(&mut ui, answer(1, 1, 0), 1000);
        // Off with 200 ms of the 400 ms wait left.
        let mut out = Update::new();
        ui.tick(1200, 0, false, &mut out);
        assert!(!out.dirty && !out.wake && !animating(&ui));
        ui.tick(1200 + 30_000, 0, false, &mut out);
        assert!(!out.dirty && !out.wake && !animating(&ui));
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Question);
        // Lit again a minute later: those 200 ms are still to run.
        let lit = 61_200;
        let mut out = Update::new();
        ui.tick(lit, 0, true, &mut out);
        assert!(!out.dirty && animating(&ui));
        assert!(!tick(&mut ui, lit + 199));
        assert!(tick(&mut ui, lit + 200));
        assert_eq!(ui.deck.agent(1).unwrap().state, AgentState::Working);
        // The typing pauses the same way.
        let mut ui = fresh();
        hold_ptt(&mut ui, 3, PttCtx::Talk, 0, 200_000);
        let mut out = Update::new();
        ui.tick(200_100, 0, false, &mut out);
        assert!(ui.deck.transcript.is_empty() && !animating(&ui));
        // Lit 30 s later (within the 60 s PTT cap): 200 ms of latency left.
        ui.tick(230_000, 0, true, &mut out);
        assert!(ui.deck.transcript.is_empty() && animating(&ui));
        assert!(!tick(&mut ui, 230_199));
        assert!(tick(&mut ui, 230_200));
        assert_eq!(ui.deck.transcript.as_str().len(), 1);
    }

    #[test]
    fn the_queue_is_bounded_and_dedupes_per_agent_so_nothing_is_lost() {
        let mut ui = fresh();
        // Rapid-fire orders to every agent, no tick in between: at most one
        // pending job and one Ready report per agent, at the latest time.
        for round in 0..20u32 {
            for agent in 1..=5u8 {
                let at = round * 100 + agent as u32 * 10;
                hold_ptt(&mut ui, agent, PttCtx::Talk, 0, at);
                ui.global.ptt = None;
                event(&mut ui, ptt_event(0, agent, PttCtx::Talk, 0, false), at + 1);
                assert!(ui.demo.queued() <= QUEUE_LEN);
            }
        }
        let count = |ui: &UiState, f: &dyn Fn(&Pending) -> bool| {
            ui.demo
                .queue
                .iter()
                .flatten()
                .filter(|entry| f(&entry.what))
                .count()
        };
        assert_eq!(count(&ui, &|what| matches!(what, Pending::Ready { .. })), 5);
        assert_eq!(
            count(&ui, &|what| matches!(what, Pending::Orders { .. })),
            5
        );
        assert_eq!(ui.demo.queued(), 10);
        for entry in ui.demo.queue.iter().flatten() {
            let (Pending::Ready { agent, .. }
            | Pending::Orders { agent, .. }
            | Pending::Question { agent, .. }) = entry.what;
            let latest = 1900 + agent as u32 * 10 + 1;
            let expected = match entry.what {
                Pending::Ready { .. } => latest + READY_MS,
                _ => latest + REPLY_MS,
            };
            assert_eq!(entry.due_ms, expected);
        }
        // Answers dedupe the fresh question; a full drain loses nothing.
        let mut ui = fresh();
        event(&mut ui, answer(1, 1, 0), 0);
        event(&mut ui, answer(2, 2, 0), 1);
        assert_eq!(
            count(&ui, &|what| matches!(what, Pending::Question { .. })),
            1
        );
        assert_eq!(ui.demo.queued(), 5);
        let mut now = 100;
        let mut delivered = 0;
        for _ in 0..40 {
            delivered += tick(&mut ui, now) as usize;
            now += 20_000;
        }
        assert_eq!(ui.demo.queued(), 0);
        assert_eq!(delivered, 1 + 1 + 3 + 3 + 3);
        // A queue that is somehow full drops instead of panicking.
        let mut script = Script::new();
        for n in 0..QUEUE_LEN as u8 + 5 {
            script.push(
                Pending::Ready {
                    agent: n,
                    job: Job::Talk,
                    card: 0,
                    phase: 0,
                },
                0,
            );
        }
        assert_eq!(script.queued(), QUEUE_LEN);
        assert!(!script.push(
            Pending::Question {
                not: 0,
                agent: 0,
                card: 0,
                phase: 0
            },
            0
        ));
    }

    #[test]
    fn a_question_needs_a_free_agent_and_never_goes_to_the_one_that_answered() {
        let mut ui = fresh();
        // Only the answering agent and card holders are left: nobody asks.
        for id in [3, 4, 5] {
            let mut out = Update::new();
            let gone = HostMsg::Agent {
                id,
                gone: true,
                kind: None,
                state: None,
                name: None,
                project: None,
                title: None,
                since_s: None,
            };
            ui.host(&gone, 0, &mut out);
        }
        event(&mut ui, answer(1, 1, 0), 1000);
        tick(&mut ui, 1000 + WORKING_MS);
        assert!(!tick(&mut ui, 1000 + QUESTION_MS));
        assert!(ui.deck.card(1).is_none() && ui.deck.card(2).is_some());
        assert_eq!(ui.demo.queued(), 1, "only the Ready report is left");
    }

    #[test]
    fn card_ids_never_collide_with_the_mockups_and_never_read_zero() {
        let mut script = Script::new();
        script.next_card = u16::MAX - 1;
        let ids: Vec<u16> = (0..4).map(|_| script.new_card()).collect();
        assert_eq!(ids, [u16::MAX - 1, u16::MAX, FIRST_CARD, FIRST_CARD + 1]);
        assert!(FIRST_CARD > 2);
    }

    #[test]
    fn canned_strings_fit_their_fields_untruncated() {
        for canned in &CANNED {
            assert_eq!(Text::<128>::from_str(canned.question), canned.question);
            assert_eq!(Text::<64>::from_str(canned.title), canned.title);
            assert_eq!(Text::<64>::from_str(canned.working), canned.working);
            assert_eq!(Text::<64>::from_str(canned.done), canned.done);
            assert_eq!(Text::<128>::from_str(canned.done), canned.done);
            assert_eq!(Text::<200>::from_str(canned.asks), canned.asks);
            assert_eq!(Text::<200>::from_str(canned.done_reply), canned.done_reply);
            assert!(canned.recommended < 3);
            for choice in canned.choices.iter().chain(&canned.steps) {
                assert_eq!(Text::<32>::from_str(choice), *choice);
                let mut done: Text<64> = Text::new();
                done.set(choice);
                done.push_str(DONE_SUFFIX);
                assert!(done.as_str().ends_with(DONE_SUFFIX));
            }
        }
        // The loaded mockup cards are what the tables say their agents ask.
        let mut deck = Box::new(Deck::new());
        load(&mut deck);
        let question = deck.card(1).unwrap();
        assert_eq!(question.text, CANNED[0].question);
        assert_eq!(question.choices[2], CANNED[0].choices[2]);
        assert_eq!(deck.agent(1).unwrap().last_reply, CANNED[0].asks);
        let ready = deck.card(2).unwrap();
        assert_eq!(ready.text, CANNED[1].done);
        assert_eq!(ready.choices[1], CANNED[1].steps[1]);
        assert_eq!(deck.agent(2).unwrap().last_reply, CANNED[1].done_reply);
        for (list, base) in WORKSPACES.iter().zip(1..) {
            assert!(!list.is_empty() && list.len() as u8 <= MAX_WINDOWS);
            for (_, title) in list.iter() {
                assert_eq!(Text::<48>::from_str(title), *title);
            }
            if base == 2 {
                let titles: Vec<&str> = list.iter().map(|(_, title)| *title).collect();
                assert_eq!(
                    titles,
                    deck.workspaces.windows[..3]
                        .iter()
                        .map(|row| row.title.as_str())
                        .collect::<Vec<_>>()
                );
            }
        }
        for (name, percent, reset) in USAGE {
            assert_eq!(Text::<20>::from_str(name), name);
            assert_eq!(Text::<24>::from_str(reset), reset);
            assert!(percent + 2 < 100);
        }
        for text in [TRANSCRIPT, ANSWER_TRANSCRIPT, TALK_REPLY, TALK_DONE_REPLY] {
            assert_eq!(Text::<200>::from_str(text), text);
            assert!(text.chars().count() < u8::MAX as usize);
        }
        assert_eq!(Text::<16>::from_str(FACE_NAME), FACE_NAME);
        assert_eq!(Text::<64>::from_str(TALK_WORKING), TALK_WORKING);
        assert_eq!(Text::<128>::from_str(TALK_DONE), TALK_DONE);
    }

    #[test]
    fn the_script_is_small_plain_data() {
        assert!(core::mem::size_of::<Script>() <= 256);
        let script = Script::new();
        let copy = script;
        assert_eq!(script, copy);
        assert_eq!(Script::new().queued(), 0);
    }
}
