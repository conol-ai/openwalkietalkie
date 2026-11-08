//! The deck model: everything a host pushes, in the shape the faces read it.
//! `apply` validates a message completely, then applies it atomically. A
//! `hello` with a new sid keeps the old roster drawn but STALE until `synced`
//! purges what the host did not confirm; the same sid after a link loss just
//! resumes. Roster order is only appended to / compacted here: the re-sort is
//! the Agents grammar's, which calls `sort` when the user is not looking. A
//! sorted roster is grouped by project (the `proj` of the `agent` line, in
//! name order, agents without one last) and by state class inside a project;
//! the Agents face shows one project at a time (`page_rows`).
//!
//! Never move a `Deck` by value at runtime: `const fn new()` is for statics,
//! everything else fills in place.
use super::msg::{
    AgentKind, AgentState, DocFont, CardKind, ErrWhy, HostMsg, MacroKey, SettingKey, UsageItem,
    WindowRow, DocKind, DOC_HELD, DOC_LINE,
};
use super::text::Text;

/// The id space: agent ids are one byte on the wire, 1..=255. A deck holds
/// [`Agents::len`] of them (its session table, sized by its owner); the
/// per-id arrays below are only bytes each.
pub const MAX_AGENTS: usize = 255;
/// Sessions a deck holds when its owner gives it no larger table: what
/// every deck held before the table moved to PSRAM, and what a host assumes
/// of a deck that does not say (`hello` without `agents`).
pub const DEFAULT_SESSIONS: usize = 8;
pub const MAX_NOTICES: usize = 8;
pub const MAX_MACRO_SETS: usize = 3;
/// The host says hello every 5 s; this much silence means it is gone.
pub const HEARTBEAT_MS: u32 = 15_000;
/// A resync the host never terminates is treated as complete after this.
pub const SYNC_TIMEOUT_MS: u32 = 10_000;
const MAX_WORKSPACES: u8 = 9;

/// A body the **host** laid out and the deck scrolls: the `held` lines from
/// `first`, of `total` in the whole text.
///
/// One type for the card's question, the agent's last reply and the live
/// transcript, because the deck does the same thing with all three — draws a
/// window of somebody else's layout and asks for the next one. `total == 0`
/// means no window has arrived, and every lane falls back to the capped text
/// its own line carries, so an old host still works and an empty window cannot
/// blank a lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Doc {
    pub lines: [Text<DOC_LINE>; DOC_HELD],
    pub held: u8,
    pub first: u16,
    pub total: u16,
    /// What the host measured the lines with; the deck draws with the same.
    pub font: DocFont,
    /// Which body these lines are of: the card id, or the `rev` the `reply` /
    /// `transcript` line carried. A window whose id has moved on is dropped.
    pub id: u16,
}

impl Doc {
    pub const fn new(font: DocFont) -> Self {
        Self {
            lines: [Text::new(); DOC_HELD],
            held: 0,
            first: 0,
            total: 0,
            font,
            id: 0,
        }
    }
    /// The host wrapped this body for us and the deck holds lines of it.
    pub fn windowed(&self) -> bool {
        self.total != 0 && self.held != 0
    }
    /// Line `index` of the whole body, if the buffer covers it.
    pub fn line(&self, index: usize) -> Option<&str> {
        let slot = index.checked_sub(self.first as usize)?;
        (slot < self.held as usize)
            .then(|| self.lines.get(slot))
            .flatten()
            .map(Text::as_str)
    }
    /// One past the last line the buffer holds.
    pub fn end(&self) -> usize {
        self.first as usize + self.held as usize
    }
    /// Forget the buffer: the body changed under it, so its lines are of a
    /// text that is no longer here.
    pub fn forget(&mut self) {
        self.held = 0;
        self.first = 0;
        self.total = 0;
    }

    /// Take a chunk the host sent **into** the buffer.
    ///
    /// A chunk that touches what is already held widens the span rather than
    /// replacing it, so the deck accumulates a run of the body to scroll
    /// through instead of holding exactly one chunk and stalling at its edge.
    /// A chunk of a different body, or one that does not touch the held span,
    /// starts the buffer over — there is no way to draw the gap between them.
    pub fn merge(
        &mut self,
        id: u16,
        first: u16,
        total: u16,
        font: DocFont,
        lines: &[Text<DOC_LINE>],
        count: u8,
    ) {
        let count = (count as usize).min(lines.len()).min(DOC_HELD);
        let incoming = first as usize..first as usize + count;
        let joins = self.windowed()
            && self.id == id
            && (self.total == total || font.rich())
            && self.font == font
            && incoming.start <= self.end()
            && incoming.end >= self.first as usize;
        if !joins {
            self.lines[..count].copy_from_slice(&lines[..count]);
            self.id = id;
            self.first = first;
            self.total = total;
            self.font = font;
            self.held = count as u8;
            return;
        }

        let held_first = self.first as usize;
        let want_first = held_first.min(incoming.start);
        let want_end = self.end().max(incoming.end).min(total as usize);
        // Too much to hold: keep the end the chunk arrived at, which is the
        // one the reader is scrolling towards.
        let (new_first, new_end) = if want_end - want_first > DOC_HELD {
            if incoming.start >= held_first {
                (want_end - DOC_HELD, want_end)
            } else {
                (want_first, want_first + DOC_HELD)
            }
        } else {
            (want_first, want_end)
        };

        // Shift what is kept so it lines up with `new_first`.
        if new_first > held_first {
            let drop = new_first - held_first;
            let keep = (self.held as usize).saturating_sub(drop);
            self.lines.copy_within(drop..drop + keep, 0);
        } else if new_first < held_first {
            let shift = held_first - new_first;
            let keep = (self.held as usize).min(DOC_HELD - shift);
            self.lines.copy_within(0..keep, shift);
        }
        // Write the chunk where it belongs.
        let at = incoming.start - new_first;
        for (slot, line) in lines.iter().take(count).enumerate() {
            if let Some(cell) = self.lines.get_mut(at + slot) {
                *cell = *line;
            }
        }
        self.total = total;
        self.first = new_first as u16;
        self.held = (new_end - new_first).min(DOC_HELD) as u8;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Card {
    pub id: u16,
    pub kind: CardKind,
    /// The one-line summary the `card` line carries: the channel view's card
    /// block, a Ready card's heading, and the question body of a host that
    /// sends no window at all.
    pub text: Text<128>,
    /// The window of the question body the host wrapped for us.
    pub body: Doc,
    pub choices: [Text<32>; 4],
    pub choice_count: u8,
    pub recommended: Option<u8>,
    /// Sent, awaiting the host's clear.
    pub pending_answer: Option<u8>,
    pub sent_at_ms: u32,
    pub snoozed: bool,
    /// A Ready card that has been opened.
    pub acknowledged: bool,
}
impl Card {
    pub const fn new() -> Self {
        Self {
            id: 0,
            kind: CardKind::Question,
            text: Text::new(),
            body: Doc::new(DocFont::Title),
            choices: [Text::new(); 4],
            choice_count: 0,
            recommended: None,
            pending_answer: None,
            sent_at_ms: 0,
            snoozed: false,
            acknowledged: false,
        }
    }
    /// Still wants the user: a Question, or a Ready card not yet opened. An
    /// acknowledged Ready card is only its summary text in the channel view.
    pub fn open(&self) -> bool {
        self.kind == CardKind::Question || !self.acknowledged
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Agent {
    /// The stable host channel number 1..=8; the slot is `id - 1`.
    pub id: u8,
    pub present: bool,
    /// Kept from before a resync or link loss; drawn gray.
    pub stale: bool,
    pub kind: AgentKind,
    pub state: AgentState,
    pub name: Text<24>,
    /// The project the session runs in (the host's `proj`: the working
    /// directory's last component); empty = no project. The roster's pages.
    pub project: Text<24>,
    pub title: Text<64>,
    /// The session's major task (the roster's title); empty falls back to
    /// the name.
    pub task: Text<64>,
    /// Subagents and workflow runs in flight (the roster's count).
    pub subs: u8,
    pub last_reply: Text<200>,
    /// The whole reply, laid out by the host and scrolled here. `last_reply`
    /// stays the capped line the `reply` message carries, so a lane with no
    /// window behaves exactly as it did before.
    pub reply: Doc,
    /// Selected reading mode; chat and workflow share one bounded buffer.
    pub reply_kind: DocKind,
    /// The `rev` of the reply currently held; a window for an older one is
    /// dropped rather than drawn over the new text.
    pub reply_rev: u16,
    pub state_since_ms: u32,
    pub has_card: bool,
    pub card: Card,
}
impl Agent {
    pub const fn new() -> Self {
        Self {
            id: 0,
            present: false,
            stale: false,
            kind: AgentKind::Other,
            state: AgentState::Idle,
            name: Text::new(),
            project: Text::new(),
            title: Text::new(),
            task: Text::new(),
            subs: 0,
            last_reply: Text::new(),
            reply: Doc::new(DocFont::Row),
            reply_kind: DocKind::Reply,
            reply_rev: 0,
            state_since_ms: 0,
            has_card: false,
            card: Card::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub arcade_sound: u8,
    /// 0..=10.
    pub speaker: u8,
    /// 0..=10.
    pub mic: u8,
    pub auto_announce: bool,
    pub chime: bool,
    pub auto_jump: bool,
    /// 1..=5.
    pub brightness: u8,
    /// 0 (never), 30, 60 or 120.
    pub idle_home_s: u16,
    pub demo: bool,
    /// 0 small, 1 medium, 2 large. Chooses the face documents are drawn in;
    /// the host wraps with the same one.
    pub text_size: u8,
}
impl Settings {
    pub const fn new() -> Self {
        Self {
            speaker: 6,
            arcade_sound: 0,
            mic: 6,
            auto_announce: true,
            chime: true,
            auto_jump: false,
            brightness: 5,
            idle_home_s: 120,
            // Off: without a host the deck shows itself offline, not made-up
            // agents. Settings > Demo data turns the demo on for a showcase.
            demo: false,
            // Medium by default: `Row` at 20 px was measured as hard to read.
            text_size: 1,
        }
    }
    /// The value a `set` message or event carries for `key`.
    pub fn get(&self, key: SettingKey) -> u16 {
        match key {
            SettingKey::ArcadeSound => self.arcade_sound as u16,
            SettingKey::Speaker => self.speaker as u16,
            SettingKey::Mic => self.mic as u16,
            SettingKey::Announce => self.auto_announce as u16,
            SettingKey::Chime => self.chime as u16,
            SettingKey::AutoJump => self.auto_jump as u16,
            SettingKey::Brightness => self.brightness as u16,
            SettingKey::IdleHome => self.idle_home_s,
            SettingKey::Demo => self.demo as u16,
            SettingKey::TextSize => self.text_size as u16,
        }
    }
    /// Clamp into the key's range and store. True when the value changed.
    pub fn set(&mut self, key: SettingKey, value: u16) -> bool {
        let before = self.get(key);
        match key {
            SettingKey::ArcadeSound => self.arcade_sound = value.min(2) as u8,
            SettingKey::Speaker => self.speaker = value.min(10) as u8,
            SettingKey::Mic => self.mic = value.min(10) as u8,
            SettingKey::Announce => self.auto_announce = value != 0,
            SettingKey::Chime => self.chime = value != 0,
            SettingKey::AutoJump => self.auto_jump = value != 0,
            SettingKey::Brightness => self.brightness = value.clamp(1, 5) as u8,
            SettingKey::IdleHome => {
                self.idle_home_s = match value {
                    0 => 0,
                    1..=30 => 30,
                    31..=60 => 60,
                    _ => 120,
                }
            }
            SettingKey::Demo => self.demo = value != 0,
            SettingKey::TextSize => self.text_size = value.min(2) as u8,
        }
        before != self.get(key)
    }
}

/// One drawer/toast entry, identified by (agent, card). Built only through
/// `Notice::new` outside this file, so private fields may be added.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Notice {
    pub agent: u8,
    pub card: u16,
    pub kind: CardKind,
    pub in_drawer: bool,
    pub used: bool,
    /// When the card arrived: the ring drops the oldest Ready first
    /// (compare ages with `now.wrapping_sub(raised_ms)`).
    pub raised_ms: u32,
}
impl Notice {
    pub const EMPTY: Self = Self {
        agent: 0,
        card: 0,
        kind: CardKind::Question,
        in_drawer: false,
        used: false,
        raised_ms: 0,
    };

    /// A live notice that is not in the drawer yet.
    pub const fn new(agent: u8, card: u16, kind: CardKind, raised_ms: u32) -> Self {
        Self {
            agent,
            card,
            kind,
            in_drawer: false,
            used: true,
            raised_ms,
        }
    }

    /// The same notice, filed in the drawer.
    pub const fn filed(mut self) -> Self {
        self.in_drawer = true;
        self
    }
}

/// The keys of one Macropad instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacroSet {
    /// Face id this set belongs to; 0 = free slot.
    pub face: u8,
    pub source: Text<16>,
    pub keys: [MacroKey; 9],
}
impl MacroSet {
    pub const EMPTY: Self = Self {
        face: 0,
        source: Text::new(),
        keys: [MacroKey::EMPTY; 9],
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Workspaces {
    /// 1-based active workspace; 0 = unknown.
    pub active: u8,
    pub count: u8,
    /// Host-selected window row.
    pub selected: u8,
    pub windows: [WindowRow; 6],
    pub window_count: u8,
}
impl Workspaces {
    pub const fn new() -> Self {
        Self {
            active: 0,
            count: 0,
            selected: 0,
            windows: [WindowRow::EMPTY; 6],
            window_count: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Usage {
    pub period: Text<12>,
    pub items: [UsageItem; 6],
    pub count: u8,
}
impl Usage {
    pub const fn new() -> Self {
        Self {
            period: Text::new(),
            items: [UsageItem::EMPTY; 6],
            count: 0,
        }
    }
}

/// What applying one host message did, for the UI layers that react to it.
/// The state handlers only ever see it by reference.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Effects {
    pub dirty: bool,
    /// The message was refused as a whole; nothing was applied.
    pub reject: Option<ErrWhy>,
    /// A `hello` arrived: reply with `Event::Hello`.
    pub hello: bool,
    /// First hello on this link, or a new sid: demo data and holds are void.
    pub new_session: bool,
    /// A NEW (agent, card) outside sync: toast / wake / auto-jump candidate.
    pub notice: Option<Notice>,
    /// (agent, card) that the host cleared or replaced.
    pub card_cleared: Option<(u8, u16)>,
    pub agent_gone: Option<u8>,
}
impl Effects {
    pub const fn new() -> Self {
        Self {
            dirty: false,
            reject: None,
            hello: false,
            new_session: false,
            notice: None,
            card_cleared: None,
            agent_gone: None,
        }
    }
}

/// Not `Copy`: its session table is storage the owner provides
/// (`UiState::copy_into` copies the contents).
#[derive(PartialEq, Eq, Debug)]
pub struct Deck {
    /// Slot = id - 1; as many as the owner's table holds.
    pub agents: Agents,
    /// Displayed roster order (agent ids): by project (name order, no project
    /// last), then Question > Ready > Error > Working > Idle, by id inside a
    /// class, once `sort` has run.
    pub order: [u8; MAX_AGENTS],
    pub order_len: u8,
    pub workspaces: Workspaces,
    pub macros: [MacroSet; MAX_MACRO_SETS],
    pub usage: Usage,
    pub system: super::msg::SystemStatus,
    pub system_at: Option<u32>,
    pub system_stale: bool,
    pub settings: Settings,
    pub notices: [Notice; MAX_NOTICES],
    pub clock_valid: bool,
    /// Displayed minute of day, advanced by `tick`.
    pub clock_minute: u16,
    /// A hello was seen on this link and the heartbeat is alive.
    pub host_present: bool,
    pub syncing: bool,
    pub sid: u32,
    pub transcript: Text<200>,
    pub transcript_final: bool,
    pub transcript_channel_tip: bool,
    /// The host's `rec` marker: this press is recorded for a companion's
    /// recogniser, so the overlay shows a waveform instead of words.
    pub transcript_rec: bool,
    /// The whole live transcript, laid out by the host. Its window sits at the
    /// **end** of the body: what the operator wants is the newest speech.
    pub transcript_doc: Doc,
    pub transcript_rev: u16,
    clock_anchor_ms: u32,
    heard_ms: u32,
    sync_at_ms: u32,
    /// A host session existed since boot: `sid` is meaningful.
    had_session: bool,
    /// The sid a COMPLETE state is held for (`held_sid`): set by `synced`,
    /// 0 from a new session or a lost host until the next `synced`.
    synced_sid: u32,
    /// Host data kept from a lost host: shown "offline", never as the demo.
    offline_data: bool,
    /// An agent changed class since `sort` last ran.
    order_stale: bool,
}

/// The deck's session table: `len` agents at `ptr`, storage its owner
/// provides (the firmware: a table in PSRAM; the host tests: the heap).
/// Indexed and iterated like the array it replaced; slot = id - 1.
pub struct Agents {
    ptr: *mut Agent,
    len: usize,
    /// The table was allocated here (host builds) and is freed with it.
    #[cfg(not(target_os = "none"))]
    owned: bool,
}

// SAFETY: the table is reached only through its `Deck`, which the owner
// keeps on one thread; the pointer is an address, not shared state.
unsafe impl Send for Agents {}
unsafe impl Sync for Agents {}

impl Agents {
    /// No table yet: every lookup misses until the owner attaches one.
    pub const fn empty() -> Self {
        Self {
            ptr: core::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            #[cfg(not(target_os = "none"))]
            owned: false,
        }
    }

    /// Use `len` initialised agents at `ptr`.
    ///
    /// # Safety
    /// `ptr` must point to `len` valid `Agent`s that live as long as this
    /// table and are reached through nothing else.
    pub unsafe fn attach(&mut self, ptr: *mut Agent, len: usize) {
        self.release();
        self.ptr = ptr;
        self.len = len.min(MAX_AGENTS);
    }

    /// A table of `len` sessions on the heap (host builds: tests, preview).
    #[cfg(not(target_os = "none"))]
    pub fn allocate(&mut self, len: usize) {
        let table: std::boxed::Box<[Agent]> = (0..len.min(MAX_AGENTS)).map(|_| Agent::new()).collect();
        let len = table.len();
        let ptr = std::boxed::Box::into_raw(table) as *mut Agent;
        // SAFETY: just allocated, `len` agents, owned by this table alone.
        unsafe { self.attach(ptr, len) };
        self.owned = true;
    }

    fn release(&mut self) {
        #[cfg(not(target_os = "none"))]
        if core::mem::take(&mut self.owned) {
            // SAFETY: `allocate` made exactly this boxed slice.
            drop(unsafe { std::boxed::Box::from_raw(core::ptr::slice_from_raw_parts_mut(self.ptr, self.len)) });
        }
        // Field by field: assigning a whole `Agents` would drop this one and
        // call back in here.
        self.ptr = core::ptr::NonNull::dangling().as_ptr();
        self.len = 0;
    }
}

#[cfg(not(target_os = "none"))]
impl Drop for Agents {
    fn drop(&mut self) {
        self.release();
    }
}

impl core::ops::Deref for Agents {
    type Target = [Agent];
    fn deref(&self) -> &[Agent] {
        // SAFETY: `attach`'s contract; an empty table is a dangling, aligned
        // pointer with length 0.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl core::ops::DerefMut for Agents {
    fn deref_mut(&mut self) -> &mut [Agent] {
        // SAFETY: as `deref`, and `&mut self` is the only way in.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl<'a> IntoIterator for &'a Agents {
    type Item = &'a Agent;
    type IntoIter = core::slice::Iter<'a, Agent>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Agents {
    type Item = &'a mut Agent;
    type IntoIter = core::slice::IterMut<'a, Agent>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl PartialEq for Agents {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for Agents {}

impl core::fmt::Debug for Agents {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.iter().filter(|a| a.present)).finish()
    }
}

impl Deck {
    /// A deck with a heap table of `sessions` (host builds: tests).
    #[cfg(not(target_os = "none"))]
    pub fn boxed(sessions: usize) -> std::boxed::Box<Self> {
        let mut deck = std::boxed::Box::new(Self::new());
        deck.agents.allocate(sessions);
        deck
    }

    pub const fn new() -> Self {
        Self {
            agents: Agents::empty(),
            order: [0; MAX_AGENTS],
            order_len: 0,
            workspaces: Workspaces::new(),
            macros: [MacroSet::EMPTY; MAX_MACRO_SETS],
            usage: Usage::new(),
            system: super::msg::SystemStatus::EMPTY,
            system_at: None,
            system_stale: true,
            settings: Settings::new(),
            notices: [Notice::EMPTY; MAX_NOTICES],
            clock_valid: false,
            clock_minute: 0,
            host_present: false,
            syncing: false,
            sid: 0,
            transcript: Text::new(),
            transcript_final: false,
            transcript_channel_tip: false,
            transcript_rec: false,
            transcript_doc: Doc::new(DocFont::Row),
            transcript_rev: 0,
            clock_anchor_ms: 0,
            heard_ms: 0,
            sync_at_ms: 0,
            had_session: false,
            synced_sid: 0,
            offline_data: false,
            order_stale: false,
        }
    }

    /// Power-on defaults, in place. No demo data: `demo::load` adds that.
    pub fn reset(&mut self) {
        self.clear_host_data();
        self.settings = Settings::new();
        self.host_present = false;
        self.syncing = false;
        self.sid = 0;
        self.had_session = false;
        self.synced_sid = 0;
    }

    /// Forget everything a host (or the demo) supplied; settings survive.
    pub fn clear_host_data(&mut self) {
        for agent in &mut self.agents {
            clear_agent(agent);
        }
        self.order_len = 0;
        self.order_stale = false;
        self.workspaces.active = 0;
        self.workspaces.count = 0;
        self.workspaces.selected = 0;
        self.workspaces.window_count = 0;
        for set in &mut self.macros {
            set.face = 0;
            set.source.clear();
        }
        self.system = super::msg::SystemStatus::EMPTY;
        self.system_at = None;
        self.system_stale = true;
        self.usage.count = 0;
        self.usage.period.clear();
        for notice in &mut self.notices {
            notice.used = false;
        }
        self.clock_valid = false;
        self.transcript.clear();
        self.transcript_final = false;
        self.transcript_channel_tip = false;
        self.transcript_rec = false;
        self.transcript_doc.forget();
        self.offline_data = false;
    }

    /// Settings say demo and no real host has taken over. A lost host's data
    /// is shown offline, never as the demo (`clear_host_data` / `demo::load`
    /// end that).
    pub fn demo_active(&self) -> bool {
        self.settings.demo && !self.host_present && !self.offline_data
    }

    /// What `hello` reports as `sid`: the session a complete state is held
    /// for, 0 until the host's `synced` (after boot, a new sid or a lost
    /// host) so the host knows to run a full sync.
    pub fn held_sid(&self) -> u32 {
        if self.host_present && !self.syncing && self.synced_sid == self.sid {
            self.sid
        } else {
            0
        }
    }

    pub fn agent(&self, id: u8) -> Option<&Agent> {
        self.agents
            .get((id as usize).wrapping_sub(1))
            .filter(|agent| agent.present)
    }

    pub fn agent_mut(&mut self, id: u8) -> Option<&mut Agent> {
        self.agents
            .get_mut((id as usize).wrapping_sub(1))
            .filter(|agent| agent.present)
    }

    /// The pending card of `id`, if any.
    pub fn card(&self, id: u8) -> Option<&Card> {
        self.agent(id)
            .filter(|agent| agent.has_card)
            .map(|agent| &agent.card)
    }

    /// Position of `id` in the displayed roster order.
    pub fn row_of(&self, id: u8) -> Option<u8> {
        self.order_ids()
            .iter()
            .position(|&agent| agent == id)
            .map(|row| row as u8)
    }

    fn order_ids(&self) -> &[u8] {
        &self.order[..(self.order_len as usize).min(MAX_AGENTS)]
    }

    /// The project label of `id`: `""` for no project or no such agent.
    pub fn project_of(&self, id: u8) -> &str {
        self.agent(id).map_or("", |agent| agent.project.as_str())
    }

    /// The first agent of each project, in roster order: one id per page of
    /// the Agents roster. Returns how many `out` holds.
    pub fn project_heads(&self, out: &mut [u8; MAX_AGENTS]) -> usize {
        let mut len = 0;
        for &id in self.order_ids() {
            let label = self.project_of(id);
            let seen = out[..len].iter().any(|&head| self.project_of(head) == label);
            if !seen {
                out[len] = id;
                len += 1;
            }
        }
        len
    }

    /// Which page of the Agents roster project `label` is (0 when it has
    /// none), and how many pages there are.
    pub fn project_page(&self, label: &str) -> (u8, u8) {
        let mut heads = [0; MAX_AGENTS];
        let len = self.project_heads(&mut heads);
        let at = heads[..len].iter().position(|&head| self.project_of(head) == label).unwrap_or(0);
        (at as u8, len as u8)
    }

    /// The agents of project `label`, in roster order: one page of the
    /// Agents roster. Returns how many `out` holds.
    pub fn page_rows(&self, label: &str, out: &mut [u8; MAX_AGENTS]) -> usize {
        let mut len = 0;
        for &id in self.order_ids() {
            if self.project_of(id) == label {
                out[len] = id;
                len += 1;
            }
        }
        len
    }

    /// How many agents project `label`'s page holds.
    pub fn page_len(&self, label: &str) -> u8 {
        self.order_ids()
            .iter()
            .filter(|&&id| self.project_of(id) == label)
            .count() as u8
    }

    /// The agent at `row` of project `label`'s page.
    pub fn page_agent(&self, label: &str, row: u8) -> Option<&Agent> {
        self.order_ids()
            .iter()
            .filter(|&&id| self.project_of(id) == label)
            .nth(row as usize)
            .and_then(|&id| self.agent(id))
    }

    /// Position of `id` on its project's page.
    pub fn page_row_of(&self, id: u8) -> Option<u8> {
        if self.agent(id).is_none() {
            return None;
        }
        let label = self.project_of(id);
        self.order_ids()
            .iter()
            .filter(|&&agent| self.project_of(agent) == label)
            .position(|&agent| agent == id)
            .map(|row| row as u8)
    }

    pub fn macro_set(&self, face: u8) -> Option<&MacroSet> {
        self.macros
            .iter()
            .find(|set| set.face == face && face != 0)
    }

    /// The set of `face`, taking a free slot if it has none yet: the deck's
    /// own key editor needs one before any host `macro` has arrived. `None`
    /// when every slot belongs to another face.
    pub fn macro_set_mut(&mut self, face: u8) -> Option<&mut MacroSet> {
        if face == 0 {
            return None;
        }
        let slot = self
            .macros
            .iter()
            .position(|set| set.face == face)
            .or_else(|| self.macros.iter().position(|set| set.face == 0))?;
        let set = &mut self.macros[slot];
        if set.face != face {
            *set = MacroSet { face, ..MacroSet::EMPTY };
        }
        Some(set)
    }

    pub fn notice_count(&self) -> u8 {
        self.notices
            .iter()
            .filter(|notice| notice.used && notice.in_drawer)
            .count() as u8
    }

    /// Colour source of the top-edge indicator: a Question is waiting.
    pub fn drawer_has_question(&self) -> bool {
        self.notices
            .iter()
            .any(|n| n.used && n.in_drawer && n.kind == CardKind::Question)
    }

    /// `order` no longer reflects the agents' classes: `sort` is due once the
    /// user is not looking (the Agents grammar decides when).
    pub fn sort_pending(&self) -> bool {
        self.order_stale
    }

    /// Recompute `order` now. The roster grammar decides when that is polite.
    pub fn sort(&mut self) {
        let mut len = 0;
        for agent in &self.agents {
            if agent.present {
                self.order[len] = agent.id;
                len += 1;
            }
        }
        // Eight entries at most: an insertion sort by (project, class, id).
        for end in 1..len {
            let mut at = end;
            while at > 0 && self.sorts_before(self.order[at], self.order[at - 1]) {
                self.order.swap(at, at - 1);
                at -= 1;
            }
        }
        self.order_len = len as u8;
        self.order_stale = false;
    }

    /// `a` is drawn above `b`: its project first (by name, ASCII case folded,
    /// no project last), then its state class, then its id.
    fn sorts_before(&self, a: u8, b: u8) -> bool {
        let (Some(a), Some(b)) = (self.agent(a), self.agent(b)) else {
            return false;
        };
        match project_order(a.project.as_str(), b.project.as_str()) {
            core::cmp::Ordering::Less => true,
            core::cmp::Ordering::Greater => false,
            core::cmp::Ordering::Equal => (rank(a.state), a.id) < (rank(b.state), b.id),
        }
    }

    /// Apply one validated host message atomically. UI-level messages
    /// (`Focus`, `Faces`, `Name`) are left to the state handlers.
    pub fn apply(&mut self, msg: &HostMsg, now_ms: u32, fx: &mut Effects) {
        match msg {
            HostMsg::Language { .. } | HostMsg::Image { .. } | HostMsg::SessionMenu(_) | HostMsg::None | HostMsg::Focus { .. } | HostMsg::Faces { .. } | HostMsg::Name { .. } => {
                // Nothing for the model, but any line from the host is a heartbeat.
                self.heard_ms = now_ms;
                return;
            }
            HostMsg::Hello {
                sid, minute_of_day, ..
            } => {
                fx.hello = true;
                let same = self.had_session && self.sid == *sid;
                if !self.host_present && same {
                    // The same daemon, back after a link drop or a pause: its
                    // state has not moved, so nothing needs a resync.
                    for agent in &mut self.agents {
                        agent.stale = false;
                    }
                    self.syncing = false;
                } else if !same {
                    fx.new_session = true;
                    if self.had_session && !self.demo_active() {
                        // Keep drawing the old roster, gray, until `synced`
                        // says what survived; no toasts meanwhile.
                        for agent in &mut self.agents {
                            agent.stale = agent.present;
                        }
                        self.syncing = self.agents.iter().any(|agent| agent.present);
                        self.sync_at_ms = now_ms;
                    } else {
                        self.clear_host_data();
                        self.syncing = false;
                    }
                    self.sid = *sid;
                    self.had_session = true;
                    self.synced_sid = 0;
                }
                self.host_present = true;
                self.offline_data = false;
                if let Some(minute) = minute_of_day {
                    self.clock_valid = true;
                    self.clock_minute = *minute % 1440;
                    self.clock_anchor_ms = now_ms;
                }
            }
            HostMsg::Synced => self.end_sync(),
            HostMsg::Agent {
                id,
                gone,
                kind,
                state,
                name,
                project,
                title,
                since_s,
                task,
                subs,
            } => {
                let Some(slot) = (*id as usize).checked_sub(1).filter(|&slot| slot < self.agents.len())
                else {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                };
                if *gone {
                    if self.agents[slot].present {
                        fx.agent_gone = Some(*id);
                        self.remove_from_order(*id);
                    }
                    clear_agent(&mut self.agents[slot]);
                } else {
                    let agent = &mut self.agents[slot];
                    if !agent.present {
                        clear_agent(agent);
                        agent.present = true;
                        agent.id = *id;
                        agent.state_since_ms = now_ms;
                        self.order_stale = true;
                        let len = (self.order_len as usize).min(MAX_AGENTS);
                        if let Some(row) = self.order.get_mut(len) {
                            *row = *id;
                            self.order_len = len as u8 + 1;
                        }
                    }
                    let agent = &mut self.agents[slot];
                    agent.stale = false;
                    if let Some(kind) = kind {
                        agent.kind = *kind;
                    }
                    if let Some(state) = state {
                        if agent.state != *state {
                            agent.state_since_ms = now_ms;
                            self.order_stale |= rank(agent.state) != rank(*state);
                        }
                        agent.state = *state;
                    }
                    if let Some(name) = name {
                        agent.name = *name;
                    }
                    if let Some(project) = project {
                        // A page move: the roster is re-sorted when polite.
                        self.order_stale |= agent.project != *project;
                        agent.project = *project;
                    }
                    if let Some(title) = title {
                        agent.title = *title;
                    }
                    if let Some(task) = task {
                        agent.task = *task;
                    }
                    if let Some(subs) = subs {
                        agent.subs = *subs;
                    }
                    if let Some(age) = since_s {
                        agent.state_since_ms = now_ms.wrapping_sub(age.saturating_mul(1000));
                    }
                }
            }
            HostMsg::Card {
                agent,
                id,
                clear,
                kind,
                text,
                choices,
                choice_count,
                recommended,
            } => {
                if *choice_count > 4 || recommended.is_some_and(|r| r >= *choice_count) {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                }
                let known = self.notice_exists(*agent, *id);
                let Some(owner) = self.agent_mut(*agent) else {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                };
                if *clear {
                    if owner.has_card {
                        fx.card_cleared = Some((*agent, owner.card.id));
                    }
                    owner.has_card = false;
                } else {
                    let new = !owner.has_card || owner.card.id != *id;
                    if new {
                        if owner.has_card {
                            fx.card_cleared = Some((*agent, owner.card.id));
                        }
                        owner.card = Card::new();
                        if !known {
                            fx.notice = Some(Notice::new(*agent, *id, *kind, now_ms));
                        }
                    }
                    // A re-push (a resync, an edit) updates the content only:
                    // sent / snoozed / acknowledged survive it. An edited
                    // body invalidates the window wrapped from the old one;
                    // a resync of the same text keeps it, so the question
                    // does not flicker back to one line and out again.
                    if owner.card.text != *text {
                        owner.card.body.forget();
                    }
                    owner.has_card = true;
                    owner.card.id = *id;
                    owner.card.kind = *kind;
                    owner.card.text = *text;
                    owner.card.choices = *choices;
                    owner.card.choice_count = *choice_count;
                    owner.card.recommended = *recommended;
                }
            }
            HostMsg::DocText {
                agent,
                doc,
                id,
                first,
                total,
                font,
                lines,
                line_count,
            } => {
                // A window whose id is not the body on the device belongs to
                // one the host has since replaced: it is dropped, not an
                // error, because it was in flight when that happened. The
                // line is still a heartbeat.
                if *doc == DocKind::Tr {
                    if self.transcript_rev == *id {
                        self.transcript_doc
                            .merge(*id, *first, *total, *font, lines, *line_count);
                    }
                    return;
                }
                let Some(owner) = self.agent_mut(*agent) else {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                };
                match doc {
                    DocKind::Card if owner.has_card && owner.card.id == *id => owner
                        .card
                        .body
                        .merge(*id, *first, *total, *font, lines, *line_count),
                    DocKind::Reply | DocKind::Workflow if owner.reply_rev == *id && owner.reply_kind == *doc => owner
                        .reply
                        .merge(*id, *first, *total, *font, lines, *line_count),
                    _ => {}
                }
            }
            HostMsg::Reply { agent, text, rev } => {
                let Some(owner) = self.agent_mut(*agent) else {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                };
                // A new body invalidates the window wrapped from the old one;
                // a resync of the same one keeps it, so the reply does not
                // flicker back to four lines and out again.
                if owner.reply_rev != *rev {
                    // Workflow counters refresh continuously. Keep the old
                    // page visible until the matching DocText replaces it in
                    // one merge; clearing on the envelope flashes a loading
                    // page between every pair of USB/BLE messages.
                    if owner.reply_kind != DocKind::Workflow {
                        owner.reply.forget();
                    }
                    owner.reply_rev = *rev;
                }
                owner.last_reply = *text;
            }
            HostMsg::Transcript { text, last, channel_tip, rec, .. } => {
                if self.transcript != *text {
                    self.transcript_doc.forget();
                    self.transcript_rev = self.transcript_rev.wrapping_add(1);
                }
                self.transcript = *text;
                self.transcript_final = *last;
                self.transcript_channel_tip = *channel_tip;
                // Sticky for the press: the final line (no `rec`) still ends
                // a recording, and a new press clears it.
                self.transcript_rec |= *rec;
            }
            HostMsg::Workspaces {
                active,
                count,
                selected,
                windows,
                window_count,
            } => {
                if *count > MAX_WORKSPACES
                    || *window_count > 6
                    || *active > *count
                    || *selected >= (*window_count).max(1)
                {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                }
                self.workspaces = Workspaces {
                    active: *active,
                    count: *count,
                    selected: *selected,
                    windows: *windows,
                    window_count: *window_count,
                };
            }
            HostMsg::Macro { face, source, keys } => {
                let slot = self
                    .macros
                    .iter()
                    .position(|set| set.face == *face)
                    .or_else(|| self.macros.iter().position(|set| set.face == 0));
                // Face ids are the strip's 1..=8 (`ui::state::MAX_FACES`).
                let Some(slot) = slot.filter(|_| (1..=8u8).contains(face)) else {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                };
                // An absent `src` keeps what THIS instance showed before, never
                // what a freed slot's previous face left behind.
                let kept = if self.macros[slot].face == *face {
                    self.macros[slot].source
                } else {
                    Text::new()
                };
                let source = source.unwrap_or(kept);
                self.macros[slot] = MacroSet {
                    face: *face,
                    source,
                    keys: *keys,
                };
            }
            HostMsg::System(status) => {
                if !status.valid() { fx.reject = Some(ErrWhy::Range); return; }
                self.system = *status;
                self.system_at = Some(now_ms);
                self.system_stale = false;
            }
            HostMsg::Usage {
                period,
                items,
                count,
            } => {
                if *count > 6 {
                    fx.reject = Some(ErrWhy::Range);
                    return;
                }
                self.usage = Usage {
                    period: *period,
                    items: *items,
                    count: *count,
                };
                for item in &mut self.usage.items {
                    item.percent = item.percent.min(100);
                }
            }
            HostMsg::Set { key, value } => {
                if self.settings.set(*key, *value) && *key == SettingKey::Demo && !self.host_present {
                    // Only a host that has not said hello can reach this;
                    // keep the data consistent with the switch either way.
                    if self.settings.demo {
                        super::demo::load(self);
                    } else {
                        self.clear_host_data();
                    }
                }
            }
        }
        self.heard_ms = now_ms;
        fx.dirty = true;
    }

    /// Advance the displayed clock, expire the heartbeat and a sync the host
    /// never finished; true when something displayed changed.
    pub fn tick(&mut self, now_ms: u32) -> bool {
        let mut changed = false;
        if self.host_present && now_ms.wrapping_sub(self.heard_ms) >= HEARTBEAT_MS {
            self.host_lost();
            changed = true;
        }
        if self.syncing && now_ms.wrapping_sub(self.sync_at_ms) >= SYNC_TIMEOUT_MS {
            self.end_sync();
            changed = true;
        }
        if self.clock_valid {
            let minutes = now_ms.wrapping_sub(self.clock_anchor_ms) / 60_000;
            if minutes != 0 {
                self.clock_anchor_ms = self.clock_anchor_ms.wrapping_add(minutes * 60_000);
                self.clock_minute = ((self.clock_minute as u32 + minutes) % 1440) as u16;
                changed = true;
            }
        }
        changed
    }

    /// The HID link dropped: no host, roster kept but stale. True when that
    /// changed anything.
    pub fn link_lost(&mut self) -> bool {
        let changed = self.host_present;
        if changed {
            self.host_lost();
        }
        changed
    }

    fn host_lost(&mut self) {
        self.host_present = false;
        self.syncing = false;
        // The host may have moved on meanwhile: it resyncs on its return.
        self.synced_sid = 0;
        self.offline_data = self.had_session;
        for agent in &mut self.agents {
            agent.stale = agent.present;
        }
    }

    /// Purge what the host did not confirm during the resync.
    fn end_sync(&mut self) {
        self.syncing = false;
        self.synced_sid = self.sid;
        for slot in 0..self.agents.len() {
            if self.agents[slot].present && self.agents[slot].stale {
                let id = self.agents[slot].id;
                self.remove_from_order(id);
                clear_agent(&mut self.agents[slot]);
            }
        }
    }

    fn remove_from_order(&mut self, id: u8) {
        let len = (self.order_len as usize).min(MAX_AGENTS);
        if let Some(row) = self.order[..len].iter().position(|&agent| agent == id) {
            self.order.copy_within(row + 1..len, row);
            self.order_len = len as u8 - 1;
        }
    }

    fn notice_exists(&self, agent: u8, card: u16) -> bool {
        self.notices
            .iter()
            .any(|n| n.used && (n.agent, n.card) == (agent, card))
    }
}

fn rank(state: AgentState) -> u8 {
    match state {
        AgentState::Question => 0,
        AgentState::Ready => 1,
        AgentState::Error => 2,
        AgentState::Working => 3,
        AgentState::Idle => 4,
    }
}

/// Project page order: by name with ASCII case folded (then by the bytes, so
/// two names that differ only in case keep a stable order), agents without a
/// project last.
fn project_order(a: &str, b: &str) -> core::cmp::Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return core::cmp::Ordering::Equal,
        (true, false) => return core::cmp::Ordering::Greater,
        (false, true) => return core::cmp::Ordering::Less,
        (false, false) => {}
    }
    let folded = a
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()));
    folded.then_with(|| a.cmp(b))
}

// In place: an `Agent` is 600 bytes, too big to build on the stack and move.
fn clear_agent(agent: &mut Agent) {
    agent.present = false;
    agent.stale = false;
    agent.id = 0;
    agent.kind = AgentKind::Other;
    agent.state = AgentState::Idle;
    agent.name.clear();
    agent.project.clear();
    agent.title.clear();
    agent.task.clear();
    agent.subs = 0;
    agent.last_reply.clear();
    agent.reply.forget();
    agent.reply_rev = 0;
    agent.reply_kind = DocKind::Reply;
    agent.state_since_ms = 0;
    agent.has_card = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::msg::DOC_LINES;

    fn hello(sid: u32) -> HostMsg {
        HostMsg::Hello {
            v: 1,
            sid,
            minute_of_day: None,
        }
    }

    fn agent(id: u8, state: AgentState) -> HostMsg {
        HostMsg::Agent {
            id,
            gone: false,
            kind: None,
            state: Some(state),
            name: None,
            project: None,
            title: None,
            since_s: None, task: None, subs: None,
        }
    }

    fn in_project(id: u8, state: AgentState, project: &str) -> HostMsg {
        let mut msg = agent(id, state);
        if let HostMsg::Agent { project: slot, .. } = &mut msg {
            *slot = Some(Text::from_str(project));
        }
        msg
    }

    fn card(agent: u8, id: u16) -> HostMsg {
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind: CardKind::Question,
            text: Text::new(),
            choices: [Text::new(); 4],
            choice_count: 2,
            recommended: Some(1),
        }
    }

    fn apply(deck: &mut Deck, msg: &HostMsg, now: u32) -> Effects {
        let mut fx = Effects::new();
        deck.apply(msg, now, &mut fx);
        fx
    }

    #[test]
    fn deck_stays_within_its_ram_budget() {
        // Measured 2026-09-23: Deck 16,512 B, Agent 1,804, Card 888, Doc 608.
        //
        // Two thirds of an agent is windows of somebody else's layout: 608 B
        // for the card's question body and 608 for the reply, each 12 lines of
        // 48 bytes plus its bookkeeping. Eight agents is 14.4 KB, and the live
        // transcript's window is one more.
        //
        // The main core reports 318 KB free, so at 5.2% of it a static this
        // size is not the constraint the wire is — but it is still one static,
        // and it grows by 4.9 KB the moment anyone widens a line or holds more
        // of them, which is what this guard is for.
        // Measured 2026-09-23 with DOC_HELD 40: Deck 40,312 B, Card 2,288,
        // Doc 2,008.
        //
        // Nearly all of it is buffered document: a card body and a reply for
        // eight agents plus the live transcript, 40 lines of 48 bytes each.
        // That buffer is why the wheel keeps moving — one 17-line chunk left
        // it dead two thirds of the time — and 40 KB is 12.7% of the 318 KB
        // the main core reports free. It grows by 6.5 KB for every five lines
        // added to `DOC_HELD`, which is what this guard is for.
        assert!(core::mem::size_of::<Deck>() <= 48 * 1024);
        assert!(core::mem::size_of::<Card>() <= 2560);
        assert!(core::mem::size_of::<Doc>() <= 2048);
    }

    #[test]
    fn an_absent_macro_source_never_inherits_a_freed_slots() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        let mut fx = Effects::new();
        let mut source = Text::new();
        source.set("deck.toml");
        let keys = [MacroKey::EMPTY; 9];
        deck.apply(&HostMsg::Macro { face: 3, source: Some(source), keys }, 0, &mut fx);
        deck.apply(&HostMsg::Macro { face: 3, source: None, keys }, 1, &mut fx);
        assert_eq!(deck.macro_set(3).unwrap().source, "deck.toml");
        // The manager removed face 3; its slot is reused by face 7.
        deck.macros[0].face = 0;
        deck.apply(&HostMsg::Macro { face: 7, source: None, keys }, 2, &mut fx);
        assert!(fx.reject.is_none());
        assert!(deck.macro_set(7).unwrap().source.is_empty());
        deck.clear_host_data();
        assert!(deck.macros.iter().all(|set| set.face == 0 && set.source.is_empty()));
    }

    #[test]
    fn settings_round_trip_and_clamp() {
        let mut settings = Settings::new();
        assert!(settings.auto_announce && settings.chime && !settings.auto_jump);
        assert_eq!((settings.brightness, settings.idle_home_s), (5, 120));
        assert!(settings.set(SettingKey::Brightness, 0));
        assert_eq!(settings.brightness, 1);
        assert!(!settings.set(SettingKey::Brightness, 1));
        settings.set(SettingKey::Speaker, 99);
        assert_eq!(settings.get(SettingKey::Speaker), 10);
        settings.set(SettingKey::IdleHome, 45);
        assert_eq!(settings.idle_home_s, 60);
    }

    #[test]
    fn first_hello_clears_the_demo_and_a_new_sid_resyncs_through_stale() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        deck.settings.demo = true; // off at power-on
        super::super::demo::load(&mut deck);
        assert!(deck.demo_active());
        let fx = apply(&mut deck, &hello(7), 1000);
        assert!(fx.hello && fx.new_session && !deck.syncing);
        assert!(deck.host_present && !deck.demo_active());
        assert_eq!(deck.order_len, 0);
        assert!(deck.notices.iter().all(|n| !n.used));
        // A real roster arrives; its questions toast (no sync suppresses them).
        apply(&mut deck, &agent(2, AgentState::Question), 1001);
        apply(&mut deck, &agent(5, AgentState::Idle), 1002);
        let fx = apply(&mut deck, &card(2, 9), 1003);
        assert_eq!(fx.notice.map(|n| (n.agent, n.card)), Some((2, 9)));
        // The daemon restarts: the roster stays, gray, until `synced`.
        let fx = apply(&mut deck, &hello(8), 5000);
        assert!(fx.new_session && deck.syncing);
        assert!(deck.agent(2).unwrap().stale && deck.agent(5).unwrap().stale);
        assert_eq!(deck.order_len, 2);
        // The host confirms agent 2 and its card, not agent 5.
        apply(&mut deck, &agent(2, AgentState::Question), 5001);
        assert!(!deck.agent(2).unwrap().stale);
        let fx = apply(&mut deck, &card(2, 9), 5002);
        assert_eq!(fx.notice, None, "a card that survived the resync is not new");
        apply(&mut deck, &HostMsg::Synced, 5003);
        assert!(!deck.syncing);
        assert!(deck.agent(5).is_none() && deck.agent(2).is_some());
        assert_eq!(&deck.order[..deck.order_len as usize], &[2]);
    }

    #[test]
    fn the_held_sid_is_zero_until_synced_and_again_after_a_lost_host() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        assert_eq!(deck.held_sid(), 0);
        apply(&mut deck, &hello(3), 0);
        assert_eq!(deck.held_sid(), 0, "a fresh session holds nothing yet");
        apply(&mut deck, &agent(1, AgentState::Working), 1);
        apply(&mut deck, &HostMsg::Synced, 2);
        assert_eq!(deck.held_sid(), 3);
        apply(&mut deck, &hello(3), 3);
        assert_eq!(deck.held_sid(), 3, "a heartbeat keeps it");
        assert!(deck.link_lost());
        assert_eq!(deck.held_sid(), 0);
        apply(&mut deck, &hello(3), 4);
        assert_eq!(deck.held_sid(), 0, "the host may have moved on: resync");
        apply(&mut deck, &HostMsg::Synced, 5);
        assert_eq!(deck.held_sid(), 3);
        // A new sid syncs (stale roster) and holds after the timeout too.
        apply(&mut deck, &hello(4), 6);
        assert!(deck.syncing && deck.held_sid() == 0);
        deck.tick(6 + SYNC_TIMEOUT_MS);
        assert_eq!(deck.held_sid(), 4);
        deck.tick(6 + SYNC_TIMEOUT_MS + HEARTBEAT_MS);
        assert_eq!(deck.held_sid(), 0, "silence loses the host");
    }

    #[test]
    fn the_same_sid_resumes_after_a_link_loss_and_heartbeat_silence_marks_the_host_absent() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(3), 0);
        apply(&mut deck, &agent(1, AgentState::Working), 1);
        assert!(deck.link_lost());
        assert!(!deck.host_present && deck.agent(1).unwrap().stale);
        assert!(!deck.demo_active(), "offline data is never shown as the demo");
        assert!(!deck.link_lost());
        let fx = apply(&mut deck, &hello(3), 2000);
        assert!(fx.hello && !fx.new_session && !deck.syncing);
        assert!(deck.host_present && !deck.agent(1).unwrap().stale);
        // Silence: still alive one tick before the deadline, gone at it.
        assert!(!deck.tick(2000 + HEARTBEAT_MS - 1));
        assert!(deck.host_present);
        assert!(deck.tick(2000 + HEARTBEAT_MS));
        assert!(!deck.host_present && deck.agent(1).unwrap().stale && !deck.demo_active());
        // Any line is a heartbeat, a UI-level one included.
        apply(&mut deck, &hello(3), 30_000);
        apply(&mut deck, &HostMsg::Focus { agent: 1, ack: None }, 40_000);
        assert!(!deck.tick(40_000 + HEARTBEAT_MS - 1));
        assert!(deck.host_present);
        // A sync the host never terminates ends by itself.
        apply(&mut deck, &hello(4), 50_000);
        assert!(deck.syncing);
        assert!(deck.tick(50_000 + SYNC_TIMEOUT_MS));
        assert!(!deck.syncing && deck.agent(1).is_none());
        // Across the clock wrap too.
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(3), u32::MAX - 100);
        assert!(!deck.tick(100));
        assert!(deck.tick(HEARTBEAT_MS));
        assert!(!deck.host_present);
    }

    #[test]
    fn card_repush_updates_content_only_and_a_replacement_clears_the_old_one() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(1), 0);
        apply(&mut deck, &agent(1, AgentState::Question), 1);
        let fx = apply(&mut deck, &card(1, 5), 2);
        assert!(fx.notice.is_some() && fx.card_cleared.is_none());
        deck.notices[0] = fx.notice.unwrap();
        deck.agent_mut(1).unwrap().card.pending_answer = Some(1);
        deck.agent_mut(1).unwrap().card.snoozed = true;
        let mut edited = card(1, 5);
        if let HostMsg::Card { text, .. } = &mut edited {
            text.set("edited");
        }
        let fx = apply(&mut deck, &edited, 3);
        assert_eq!((fx.notice, fx.card_cleared), (None, None));
        let card5 = deck.card(1).unwrap();
        assert_eq!((card5.text.as_str(), card5.pending_answer, card5.snoozed), ("edited", Some(1), true));
        let fx = apply(&mut deck, &card(1, 6), 4);
        assert_eq!(fx.card_cleared, Some((1, 5)));
        assert_eq!(fx.notice.map(|n| n.card), Some(6));
        assert_eq!(deck.card(1).unwrap().pending_answer, None);
        let mut clear = card(1, 6);
        if let HostMsg::Card { clear, .. } = &mut clear {
            *clear = true;
        }
        let fx = apply(&mut deck, &clear, 5);
        assert_eq!(fx.card_cleared, Some((1, 6)));
        assert!(deck.card(1).is_none());
        // Bad indices refuse the whole message.
        let mut bad = card(1, 7);
        if let HostMsg::Card { recommended, .. } = &mut bad {
            *recommended = Some(2);
        }
        assert_eq!(apply(&mut deck, &bad, 6).reject, Some(ErrWhy::Range));
        assert!(deck.card(1).is_none());
        assert_eq!(apply(&mut deck, &card(4, 1), 7).reject, Some(ErrWhy::Range));
        assert_eq!(
            apply(&mut deck, &HostMsg::Reply { agent: 4, text: Text::new(), rev: 0 }, 8).reject,
            Some(ErrWhy::Range)
        );
    }

    /// A window of the host's wrapped lines for one card.
    fn window(agent: u8, id: u16, first: u16, total: u16, lines: &[&str]) -> HostMsg {
        let mut held = [Text::new(); DOC_LINES];
        for (slot, line) in lines.iter().enumerate().take(DOC_LINES) {
            held[slot].set(line);
        }
        HostMsg::DocText {
            agent,
            doc: DocKind::Card,
            id,
            first,
            total,
            font: DocFont::Name,
            lines: held,
            line_count: lines.len().min(DOC_LINES) as u8,
        }
    }

    #[test]
    fn a_window_lands_only_on_the_card_it_names_and_an_edited_body_forgets_it() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(1), 0);
        apply(&mut deck, &agent(1, AgentState::Question), 1);
        apply(&mut deck, &card(1, 5), 2);
        assert!(!deck.card(1).unwrap().body.windowed(), "no window until the host sends one");
        apply(&mut deck, &window(1, 5, 0, 9, &["one", "two", "three"]), 3);
        let five = deck.card(1).unwrap();
        assert!(five.body.windowed() && five.body.font == DocFont::Name);
        assert_eq!((five.body.line(0), five.body.line(2), five.body.line(3)), (Some("one"), Some("three"), None));
        // A window further down the text: only the lines it holds answer.
        apply(&mut deck, &window(1, 5, 4, 9, &["five", "six"]), 4);
        let five = deck.card(1).unwrap();
        assert_eq!((five.body.line(3), five.body.line(4), five.body.line(5)), (None, Some("five"), Some("six")));
        // A window for a card the host has since replaced is dropped — and is
        // still a heartbeat, because the line did arrive.
        apply(&mut deck, &card(1, 6), 5);
        let fx = apply(&mut deck, &window(1, 5, 0, 9, &["stale"]), 6);
        assert!(fx.dirty && fx.reject.is_none());
        assert!(!deck.card(1).unwrap().body.windowed(), "a new card starts with no window");
        // A re-push of the same body keeps its lines (a resync must not make
        // the question flicker); one that edits the body drops them.
        apply(&mut deck, &window(1, 6, 0, 2, &["a", "b"]), 7);
        apply(&mut deck, &card(1, 6), 8);
        assert!(deck.card(1).unwrap().body.windowed());
        let mut edited = card(1, 6);
        if let HostMsg::Card { text, .. } = &mut edited {
            text.set("edited");
        }
        apply(&mut deck, &edited, 9);
        assert!(!deck.card(1).unwrap().body.windowed());
        assert_eq!(deck.card(1).unwrap().body.line(0), None);
        // A window for an agent that is not there is a range error.
        assert_eq!(apply(&mut deck, &window(4, 1, 0, 1, &["x"]), 10).reject, Some(ErrWhy::Range));
    }

    #[test]
    fn roster_order_is_appended_and_compacted_and_sorted_only_on_request() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(1), 0);
        apply(&mut deck, &agent(3, AgentState::Idle), 1);
        apply(&mut deck, &agent(1, AgentState::Working), 2);
        apply(&mut deck, &agent(2, AgentState::Question), 3);
        assert_eq!(&deck.order[..3], &[3, 1, 2]);
        assert!(deck.sort_pending());
        deck.sort();
        assert_eq!(&deck.order[..3], &[2, 1, 3]);
        assert!(!deck.sort_pending());
        // A title-only update never asks for a re-sort; a class change does.
        let mut titled = agent(3, AgentState::Idle);
        if let HostMsg::Agent { state, title, .. } = &mut titled {
            *state = None;
            *title = Some(Text::from_str("busy"));
        }
        apply(&mut deck, &titled, 4);
        assert!(!deck.sort_pending());
        apply(&mut deck, &agent(3, AgentState::Ready), 5);
        assert!(deck.sort_pending());
        assert_eq!(&deck.order[..3], &[2, 1, 3], "order is frozen until sort");
        let mut gone = agent(1, AgentState::Idle);
        if let HostMsg::Agent { gone, .. } = &mut gone {
            *gone = true;
        }
        let fx = apply(&mut deck, &gone, 6);
        assert_eq!(fx.agent_gone, Some(1));
        assert_eq!((&deck.order[..2], deck.row_of(3)), (&[2, 3][..], Some(1)));
        assert_eq!(apply(&mut deck, &gone, 7).agent_gone, None);
        assert_eq!(apply(&mut deck, &agent(9, AgentState::Idle), 8).reject, Some(ErrWhy::Range));
        assert_eq!(apply(&mut deck, &agent(0, AgentState::Idle), 8).reject, Some(ErrWhy::Range));
    }

    #[test]
    fn a_sorted_roster_is_grouped_by_project_with_the_unlabelled_last() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        apply(&mut deck, &hello(1), 0);
        apply(&mut deck, &in_project(1, AgentState::Idle, "zeta"), 1);
        apply(&mut deck, &agent(2, AgentState::Question), 2);
        apply(&mut deck, &in_project(3, AgentState::Working, "Alpha"), 3);
        apply(&mut deck, &in_project(4, AgentState::Question, "zeta"), 4);
        apply(&mut deck, &in_project(5, AgentState::Idle, "alpha"), 5);
        assert_eq!(&deck.order[..5], &[1, 2, 3, 4, 5], "appended until sorted");
        deck.sort();
        // Alpha and alpha are one page's worth of names apart only by case:
        // the fold puts them together, the bytes keep them stable.
        assert_eq!(&deck.order[..5], &[3, 5, 4, 1, 2]);
        // Pages are exact labels: the fold only orders them.
        let mut heads = [0; MAX_AGENTS];
        assert_eq!(deck.project_heads(&mut heads), 4);
        assert_eq!(&heads[..4], &[3, 5, 4, 2]);
        let mut rows = [0; MAX_AGENTS];
        assert_eq!(deck.page_rows("zeta", &mut rows), 2);
        assert_eq!(&rows[..2], &[4, 1]);
        assert_eq!(deck.page_rows("alpha", &mut rows), 1);
        assert_eq!(&rows[..1], &[5]);
        assert_eq!(deck.page_rows("", &mut rows), 1);
        assert_eq!(&rows[..1], &[2]);
        assert_eq!(deck.page_rows("nope", &mut rows), 0);
        assert_eq!((deck.page_len("zeta"), deck.page_len(""), deck.page_len("nope")), (2, 1, 0));
        assert_eq!(deck.page_agent("zeta", 1).map(|agent| agent.id), Some(1));
        assert_eq!(deck.page_agent("zeta", 2).map(|agent| agent.id), None);
        assert_eq!((deck.page_row_of(1), deck.page_row_of(4), deck.page_row_of(2)), (Some(1), Some(0), Some(0)));
        assert_eq!(deck.page_row_of(9), None);
        assert_eq!((deck.project_of(3), deck.project_of(2), deck.project_of(9)), ("Alpha", "", ""));
        // A project push moves the agent to another page once re-sorted; the
        // same label again asks for nothing.
        assert!(!deck.sort_pending());
        apply(&mut deck, &in_project(2, AgentState::Question, "alpha"), 6);
        assert!(deck.sort_pending());
        deck.sort();
        assert_eq!(&deck.order[..5], &[3, 2, 5, 4, 1]);
        apply(&mut deck, &in_project(2, AgentState::Question, "alpha"), 7);
        assert!(!deck.sort_pending());
        assert_eq!(project_order("b", "a"), core::cmp::Ordering::Greater);
        assert_eq!(project_order("", ""), core::cmp::Ordering::Equal);
    }

    #[test]
    fn workspaces_usage_and_macros_are_range_checked() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        let ws = |active, count, selected, window_count| HostMsg::Workspaces {
            active,
            count,
            selected,
            windows: [WindowRow::EMPTY; 6],
            window_count,
        };
        assert_eq!(apply(&mut deck, &ws(2, 4, 1, 3), 0).reject, None);
        assert_eq!(deck.workspaces.active, 2);
        assert_eq!(apply(&mut deck, &ws(5, 4, 0, 3), 0).reject, Some(ErrWhy::Range));
        assert_eq!(apply(&mut deck, &ws(1, 4, 3, 3), 0).reject, Some(ErrWhy::Range));
        assert_eq!(apply(&mut deck, &ws(1, 4, 0, 7), 0).reject, Some(ErrWhy::Range));
        assert_eq!(apply(&mut deck, &ws(1, 10, 0, 0), 0).reject, Some(ErrWhy::Range));
        assert_eq!(deck.workspaces.active, 2, "a refused message changes nothing");
        let mut items = [UsageItem::EMPTY; 6];
        items[0].percent = 130;
        let usage = HostMsg::Usage { period: Text::new(), items, count: 1 };
        assert_eq!(apply(&mut deck, &usage, 0).reject, None);
        assert_eq!(deck.usage.items[0].percent, 100);
        let usage = HostMsg::Usage { period: Text::new(), items, count: 7 };
        assert_eq!(apply(&mut deck, &usage, 0).reject, Some(ErrWhy::Range));
        let keys = [MacroKey::EMPTY; 9];
        for face in [0, 9] {
            let bad = HostMsg::Macro { face, source: None, keys };
            assert_eq!(apply(&mut deck, &bad, 0).reject, Some(ErrWhy::Range));
        }
        for face in 1..=3 {
            apply(&mut deck, &HostMsg::Macro { face, source: None, keys }, 0);
        }
        let fourth = HostMsg::Macro { face: 4, source: None, keys };
        assert_eq!(apply(&mut deck, &fourth, 0).reject, Some(ErrWhy::Range));
    }

    #[test]
    fn a_host_set_of_demo_before_hello_loads_or_clears_the_demo_data() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        deck.settings.demo = true; // off at power-on
        super::super::demo::load(&mut deck);
        apply(&mut deck, &HostMsg::Set { key: SettingKey::Demo, value: 0 }, 0);
        assert!(!deck.settings.demo && deck.order_len == 0 && !deck.demo_active());
        apply(&mut deck, &HostMsg::Set { key: SettingKey::Demo, value: 1 }, 0);
        assert!(deck.demo_active() && deck.order_len == 5);
        // With a host present the switch is stored and the data is left alone.
        apply(&mut deck, &hello(1), 0);
        apply(&mut deck, &agent(1, AgentState::Idle), 0);
        apply(&mut deck, &HostMsg::Set { key: SettingKey::Demo, value: 0 }, 0);
        assert!(deck.agent(1).is_some() && !deck.settings.demo);
        apply(&mut deck, &HostMsg::Set { key: SettingKey::Brightness, value: 2 }, 0);
        assert_eq!(deck.settings.brightness, 2);
    }

    #[test]
    fn the_clock_advances_by_whole_minutes_from_the_hello_stamp() {
        let mut deck = Deck::boxed(DEFAULT_SESSIONS);
        assert!(!deck.tick(100_000));
        let noon = HostMsg::Hello { v: 1, sid: 1, minute_of_day: Some(1439) };
        apply(&mut deck, &noon, 1000);
        assert!(deck.clock_valid && deck.clock_minute == 1439);
        // Heartbeats keep the host present while the minutes pass.
        let alive = |deck: &mut Deck, now: u32| apply(deck, &HostMsg::None, now);
        alive(&mut deck, 50_000);
        assert!(!deck.tick(60_999));
        assert!(deck.tick(61_000));
        assert_eq!(deck.clock_minute, 0);
        for at in (70_000..=240_000).step_by(10_000) {
            alive(&mut deck, at);
        }
        assert!(deck.tick(61_000 + 3 * 60_000 + 500));
        assert_eq!(deck.clock_minute, 3);
        alive(&mut deck, 300_000);
        assert!(!deck.tick(241_000 + 59_999));
        assert!(deck.tick(301_000));
        assert_eq!(deck.clock_minute, 4);
    }




    fn chunk(first: u16, count: usize) -> ([Text<DOC_LINE>; DOC_HELD], u8) {
        let mut lines = [Text::new(); DOC_HELD];
        for (slot, line) in lines.iter_mut().enumerate().take(count) {
            let mut text = Text::<DOC_LINE>::new();
            let _ = core::fmt::Write::write_fmt(&mut text, format_args!("L{}", first as usize + slot));
            *line = text;
        }
        (lines, count as u8)
    }

    fn span(doc: &Doc) -> (usize, usize) {
        (doc.first as usize, doc.end())
    }

    /// Chunks that touch widen the buffer instead of replacing it. Holding one
    /// chunk is what left the wheel dead two thirds of the time.
    #[test]
    fn touching_chunks_accumulate_into_one_run() {
        let mut doc = Doc::new(DocFont::Row);
        let (lines, n) = chunk(0, DOC_LINES);
        doc.merge(1, 0, 400, DocFont::Row, &lines, n);
        assert_eq!(span(&doc), (0, DOC_LINES));

        // The next chunk starts exactly where the buffer ends.
        let (lines, n) = chunk(DOC_LINES as u16, DOC_LINES);
        doc.merge(1, DOC_LINES as u16, 400, DocFont::Row, &lines, n);
        assert_eq!(span(&doc), (0, 2 * DOC_LINES), "the run grew");
        for index in 0..2 * DOC_LINES {
            assert_eq!(doc.line(index), Some(std::format!("L{index}").as_str()), "line {index}");
        }
    }

    /// Past the buffer's capacity the far end is dropped, keeping the end the
    /// reader is scrolling towards.
    #[test]
    fn a_full_buffer_drops_the_end_behind_the_reader() {
        let mut doc = Doc::new(DocFont::Row);
        let mut at = 0u16;
        while (doc.end() as u16) < 80 {
            let (lines, n) = chunk(at, DOC_LINES);
            doc.merge(1, at, 400, DocFont::Row, &lines, n);
            at = doc.end() as u16;
        }
        assert_eq!(doc.held as usize, DOC_HELD, "the buffer is full, not overflowing");
        let (first, end) = span(&doc);
        assert_eq!(end - first, DOC_HELD);
        assert!(first > 0, "the oldest lines were dropped");
        assert_eq!(doc.line(end - 1), Some(std::format!("L{}", end - 1).as_str()));
        assert_eq!(doc.line(first - 1), None, "and are gone");
    }

    /// Scrolling back fills the buffer the other way.
    #[test]
    fn a_chunk_before_the_buffer_extends_it_backwards() {
        let mut doc = Doc::new(DocFont::Row);
        let (lines, n) = chunk(20, DOC_LINES);
        doc.merge(1, 20, 400, DocFont::Row, &lines, n);
        let (lines, n) = chunk(20 - DOC_LINES as u16, DOC_LINES);
        doc.merge(1, 20 - DOC_LINES as u16, 400, DocFont::Row, &lines, n);
        let (first, end) = span(&doc);
        assert_eq!((first, end), (20 - DOC_LINES, 20 + DOC_LINES));
        for index in first..end {
            assert_eq!(doc.line(index), Some(std::format!("L{index}").as_str()), "line {index}");
        }
    }

    /// A chunk with a gap before it cannot be drawn with what is held, and a
    /// chunk of another body is not this body: both start the buffer over.
    #[test]
    fn a_disjoint_or_foreign_chunk_starts_over() {
        let mut doc = Doc::new(DocFont::Row);
        let (lines, n) = chunk(0, DOC_LINES);
        doc.merge(1, 0, 400, DocFont::Row, &lines, n);

        let (lines, n) = chunk(200, DOC_LINES);
        doc.merge(1, 200, 400, DocFont::Row, &lines, n);
        assert_eq!(span(&doc), (200, 200 + DOC_LINES), "a gap is not bridged");
        assert_eq!(doc.line(0), None);

        let (lines, n) = chunk(0, DOC_LINES);
        doc.merge(2, 0, 400, DocFont::Row, &lines, n);
        assert_eq!((doc.id, span(&doc)), (2, (0, DOC_LINES)), "another body replaces");
    }

}
