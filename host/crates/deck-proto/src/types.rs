//! The channel-5 vocabulary as host-side owned values. Wire names are the
//! lowercase serde names (`kind:"claude"`, `b:"shift+left"`), so the same
//! enums serve the daemon's JSON API unchanged.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Gemini,
    Other,
    Conol,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Working,
    Question,
    Ready,
    Error,
    Idle,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CardKind {
    Q,
    R,
}

/// Which face the host measured a document with, so the deck draws with the
/// same one. The deck never re-wraps a body the host has laid out — two
/// wrapping implementations that can disagree is the defect this whole
/// mechanism exists to avoid.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum DocFont {
    Title,
    Name,
    Row,
    /// 24 px Regular: the reply lane one step up from `Row`, for the
    /// `text` size setting.
    Text,
    RichRow,
    RichText,
    RichTitle,
}

/// Which of a slot's long texts a window belongs to.
///
/// One mechanism, not three: the card body, the agent's last reply and the live
/// transcript are all bodies the host lays out and the deck scrolls, and the
/// only thing that differs is which lane draws them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum DocKind {
    Card,
    Reply,
    Workflow,
    /// The live STT tail. Its window sits at the **end** of the body, not the
    /// start: what the operator wants to see is the newest speech.
    Tr,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Blue,
    Amber,
    Green,
    Red,
    Gray,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Window {
    /// Absent draws gray.
    pub tone: Option<Tone>,
    pub title: String,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Binding {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "enter")]
    Enter,
    #[serde(rename = "cw")]
    Cw,
    #[serde(rename = "ccw")]
    Ccw,
    #[serde(rename = "shift+left")]
    ShiftLeft,
    #[serde(rename = "shift+right")]
    ShiftRight,
    #[serde(rename = "shift+enter")]
    ShiftEnter,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MacroKey {
    pub label: String,
    /// A hardware control of the deck that presses this key; `None` = touch
    /// (and the keystroke, if any) only.
    pub binding: Option<Binding>,
    /// The keystroke the daemon posts when the key is pressed:
    /// `cmd+shift+4` (`keystroke_valid`). `None` = an event only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
}

/// The keys a keystroke may name, as the deck's `KEYS` table names them
/// (the firmware's `msg::KEYS`, checked equal by a contract test).
pub const KEYSTROKE_KEYS: &[&str] = &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "up", "down", "left", "right", "enter", "esc", "tab", "space", "backspace", "delete", "home", "end", "pageup", "pagedown", "minus", "equal", "lbracket", "rbracket", "backslash", "semicolon", "quote", "comma", "period", "slash", "grave"];

/// `ctrl+opt+shift+cmd+<key>`: modifiers in any order (`alt` = `opt`),
/// exactly one key of `KEYSTROKE_KEYS`.
#[must_use]
pub fn keystroke_valid(combo: &str) -> bool {
    let mut key = None;
    for part in combo.split('+') {
        match part {
            "ctrl" | "opt" | "alt" | "shift" | "cmd" => {}
            name => {
                if key.is_some() || !KEYSTROKE_KEYS.contains(&name) {
                    return false;
                }
                key = Some(name);
            }
        }
    }
    key.is_some()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct UsageItem {
    pub name: String,
    /// The account these windows belong to, when the source identifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// 0..=100.
    pub percent: u8,
    pub reset: String,
    /// An expiring redeemable reset reminder; absent when none is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_tip: Option<String>,
    /// `None` derives the bar tone from `percent`.
    pub tone: Option<Tone>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    Agents,
    Omarchy,
    Macropad,
    Usage,
    Settings,
    Arcade,
    System,
}

/// Live computer metrics; zero capacity and absent percentages mean unavailable.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SystemStatus {
    pub cpu: Option<u8>,
    pub gpu: Option<u8>,
    pub memory_used_mib: u32,
    pub memory_total_mib: u32,
    pub disk_free_mib: u32,
    pub disk_total_mib: u32,
    pub sample: u32,
}
impl SystemStatus {
    pub const EMPTY: Self = Self { cpu: None, gpu: None, memory_used_mib: 0,
        memory_total_mib: 0, disk_free_mib: 0, disk_total_mib: 0, sample: 0 };
    pub fn valid(&self) -> bool {
        self.cpu.is_none_or(|v| v <= 100) && self.gpu.is_none_or(|v| v <= 100)
            && self.memory_used_mib <= self.memory_total_mib && self.disk_free_mib <= self.disk_total_mib
    }
}

/// One strip entry: face id `f` (1..=8), template `t`, spoken name `n`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FaceEntry {
    pub f: u8,
    pub t: Template,
    pub n: String,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SetKey {
    Speaker,
    Mic,
    Announce,
    Chime,
    Autojump,
    Brightness,
    Idlehome,
    Demo,
    /// How big the deck draws a document it was sent: 0 small, 1 medium,
    /// 2 large. The **host** has to know it, because the host is what wraps
    /// the text — a size the two sides disagreed about would break every line
    /// in a different place.
    Textsize,
    Arcadesound,
}

impl SetKey {
    /// Every key, in the order a `boot:1` sync pushes them.
    pub const ALL: [SetKey; 10] = [
        SetKey::Speaker,
        SetKey::Mic,
        SetKey::Announce,
        SetKey::Chime,
        SetKey::Autojump,
        SetKey::Brightness,
        SetKey::Idlehome,
        SetKey::Demo,
        SetKey::Textsize,
        SetKey::Arcadesound,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            SetKey::Speaker => "speaker",
            SetKey::Mic => "mic",
            SetKey::Announce => "announce",
            SetKey::Chime => "chime",
            SetKey::Autojump => "autojump",
            SetKey::Brightness => "brightness",
            SetKey::Idlehome => "idlehome",
            SetKey::Demo => "demo",
            SetKey::Textsize => "text",
            SetKey::Arcadesound => "arcadesound",
        }
    }

    /// True when `value` is inside the range the device accepts (P:124).
    /// `idlehome` accepts anything (the device rounds up).
    pub const fn accepts(self, value: u16) -> bool {
        match self {
            SetKey::Speaker | SetKey::Mic => value <= 10,
            SetKey::Announce | SetKey::Chime | SetKey::Autojump | SetKey::Demo => value <= 1,
            SetKey::Brightness => value >= 1 && value <= 5,
            SetKey::Idlehome => true,
            // 0 small, 1 medium, 2 large.
            SetKey::Textsize | SetKey::Arcadesound => value <= 2,
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum KeyId {
    Enter,
    Cancel,
    Left,
    Right,
    Cw,
    Ccw,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum PttCtx {
    Talk,
    Answer,
    Name,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ErrWhy {
    Size,
    Parse,
    Range,
    Unknown,
}

/// One host → device line (`docs/deck-protocol.md`, "Host → device"). `slot`
/// is the wire's `id` on `agent` and `agent` everywhere else; strings are
/// uncapped here and cut by `encode`.
/// A single requested page of the device's per-session model/effort picker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionMenu {
    pub slot: u8,
    pub rev: u16,
    /// 0 overview, 1 model, 2 thinking effort.
    pub field: u8,
    pub page: u8,
    pub model: String,
    pub effort: String,
    /// 0 read-only, 1 ready, 2 pending, 3 failed.
    pub status: u8,
    pub note: String,
    pub choices: Vec<String>,
    pub total: u8,
    /// Absolute option index, not an index within the page.
    pub selected: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "m", rename_all = "snake_case")]
pub enum HostLine {
    Image { slot: u8, rev: u16, index: u8, offset: u16, total: u16, width: u16, height: u16, data: String },
    SessionMenu(SessionMenu),
    Hello {
        sid: u32,
        /// `"HH:MM"`, the header clock.
        hm: Option<String>,
    },
    Synced,
    /// An absent field keeps the value the device has.
    Agent {
        slot: u8,
        kind: Option<Kind>,
        name: Option<String>,
        /// `proj`: the project the session runs in (its working directory's
        /// last component, `deck_model::project_label`); the device groups
        /// its roster by it. Absent keeps the device's; `Some("")` is "no
        /// project".
        project: Option<String>,
        state: Option<AgentState>,
        title: Option<String>,
        since_s: Option<u32>,
    },
    AgentGone {
        slot: u8,
    },
    Card {
        slot: u8,
        id: u16,
        kind: CardKind,
        text: String,
        /// Up to 4 on the wire; `encode` cuts the rest.
        choices: Vec<String>,
        /// 0-based recommended choice.
        rec: Option<u8>,
    },
    CardClear {
        slot: u8,
        id: u16,
    },
    /// A window of the question, already wrapped by the host.
    ///
    /// `Card.text` stays what it always was — the first, capped slice — so a
    /// deck that never scrolls, and a host that never sends this, behave
    /// exactly as before. This carries the rest: `first` is where `lines[0]`
    /// sits in the whole wrapped body and `total` is how long that body is, so
    /// the deck can size a scrollbar truthfully from the first window on.
    DocText {
        slot: u8,
        doc: DocKind,
        /// The card id for `Card`; for `Reply` and `Tr` the per-slot revision
        /// the matching `reply`/`transcript` line carries, so a window for a
        /// body that has since been replaced is dropped rather than drawn over
        /// the new one.
        id: u16,
        first: u16,
        total: u16,
        font: DocFont,
        lines: Vec<String>,
    },
    Reply {
        slot: u8,
        text: String,
        /// Bumped whenever the body changes; `DocText.id` matches it.
        rev: u16,
    },
    /// Live STT tail; `last` is the wire's `final:1`.
    Transcript {
        slot: u8,
        text: String,
        last: bool,
        press: Option<u16>,
        failed: bool,
        /// Show the Claude Channels setup hint during this device PTT session.
        channel_tip: bool,
    },
    Focus {
        slot: u8,
        /// Echo of the device's `channel.seq`; absent always applies.
        ack: Option<u16>,
    },
    Ws {
        active: u8,
        count: u8,
        sel: u8,
        windows: Vec<Window>,
    },
    Macro {
        face: u8,
        src: Option<String>,
        keys: Vec<MacroKey>,
    },
    System(SystemStatus),
    Usage {
        period: String,
        items: Vec<UsageItem>,
    },
    Faces {
        list: Vec<FaceEntry>,
    },
    Name {
        text: String,
    },
    Set {
        key: SetKey,
        value: u16,
    },
}

impl HostLine {
    /// The wire's `m` value.
    pub const fn m(&self) -> &'static str {
        match self {
            HostLine::Image { .. } => "image",
            HostLine::SessionMenu(_) => "session",
            HostLine::Hello { .. } => "hello",
            HostLine::Synced => "synced",
            HostLine::Agent { .. } | HostLine::AgentGone { .. } => "agent",
            HostLine::Card { .. } | HostLine::CardClear { .. } => "card",
            // Its own `m`, unlike `CardClear`, which the device tells from a
            // `card` by its missing fields: a window carries `lines` and no
            // `type`, and giving it a name of its own keeps that unambiguous.
            HostLine::DocText { .. } => "doc",
            HostLine::Reply { .. } => "reply",
            HostLine::Transcript { .. } => "transcript",
            HostLine::Focus { .. } => "focus",
            HostLine::Ws { .. } => "ws",
            HostLine::Macro { .. } => "macro",
            HostLine::Usage { .. } => "usage",
            HostLine::System(_) => "system",
            HostLine::Faces { .. } => "faces",
            HostLine::Name { .. } => "name",
            HostLine::Set { .. } => "set",
        }
    }
}

/// One device → host line ("Device → host"). `slot` is the wire's `agent`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "m", rename_all = "snake_case")]
pub enum DeviceLine {
    Image { slot: u8, rev: u16, index: u8, offset: u16 },
    /// choice 255 requests a page; other choices require the displayed revision.
    Session { slot: u8, field: u8, page: u8, rev: u16, choice: u8 },
    Hello {
        v: u8,
        fw: String,
        /// The sid the device holds a complete state for; 0 = resync.
        sid: u32,
        boot: bool,
        rx: u16,
        faces: Vec<FaceEntry>,
    },
    Key {
        k: KeyId,
        /// 1 press, 0 release, 2 step.
        act: u8,
        shift: bool,
        f: u8,
    },
    Up,
    Channel {
        slot: u8,
        seq: u16,
    },
    Ptt {
        act: u8,
        /// 0 = none.
        slot: u8,
        ctx: PttCtx,
        card: Option<u16>,
        abort: bool,
        press: Option<u16>,
    },
    Answer {
        slot: u8,
        card: u16,
        /// 0-based.
        choice: u8,
    },
    Snooze {
        slot: u8,
        card: u16,
    },
    Macro {
        f: u8,
        /// 0..=8.
        i: u8,
        act: u8,
    },
    /// The deck's own key editor set slot `i` of Macropad `f` (`label`
    /// present) or cleared it (no `label`). The host stores it and re-sends
    /// the whole `macro`.
    MacroKey {
        f: u8,
        /// 0..=8.
        i: u8,
        label: Option<String>,
        binding: Option<Binding>,
        /// `keystroke_valid`, or the line is refused.
        keys: Option<String>,
    },
    WsGo(u8),
    WsWin(u8),
    /// "Send me the window that starts at `first`." Emitted when the operator
    /// scrolls near the edge of the lines the deck is holding.
    DocLines {
        slot: u8,
        doc: DocKind,
        id: u16,
        first: u16,
    },
    /// Toggle a tool row; stale document revisions are ignored.
    Tool { slot: u8, id: u16, index: u8 },
    Refresh {
        what: String,
    },
    Set {
        key: SetKey,
        value: u16,
    },
    Faces {
        list: Vec<FaceEntry>,
    },
    Face {
        f: u8,
    },
    Err {
        why: ErrWhy,
    },
}

impl DeviceLine {
    /// The wire's `m` value.
    pub const fn m(&self) -> &'static str {
        match self {
            DeviceLine::Image { .. } => "image",
            DeviceLine::Session { .. } => "session",
            DeviceLine::Hello { .. } => "hello",
            DeviceLine::Key { .. } => "key",
            DeviceLine::Up => "up",
            DeviceLine::Channel { .. } => "channel",
            DeviceLine::Ptt { .. } => "ptt",
            DeviceLine::Answer { .. } => "answer",
            DeviceLine::Snooze { .. } => "snooze",
            DeviceLine::Macro { .. } => "macro",
            DeviceLine::MacroKey { .. } => "macrokey",
            DeviceLine::WsGo(_) | DeviceLine::WsWin(_) => "ws",
            DeviceLine::DocLines { .. } => "doclines",
            DeviceLine::Tool { .. } => "tool",
            DeviceLine::Refresh { .. } => "refresh",
            DeviceLine::Set { .. } => "set",
            DeviceLine::Faces { .. } => "faces",
            DeviceLine::Face { .. } => "face",
            DeviceLine::Err { .. } => "err",
        }
    }
}

pub(crate) const KINDS: &[(&str, Kind)] = &[
    ("claude", Kind::Claude),
    ("codex", Kind::Codex),
    ("gemini", Kind::Gemini),
    ("other", Kind::Other),
    ("conol", Kind::Conol),
];
pub(crate) const STATES: &[(&str, AgentState)] = &[
    ("working", AgentState::Working),
    ("question", AgentState::Question),
    ("ready", AgentState::Ready),
    ("error", AgentState::Error),
    ("idle", AgentState::Idle),
];
pub(crate) const TONES: &[(&str, Tone)] = &[
    ("blue", Tone::Blue),
    ("amber", Tone::Amber),
    ("green", Tone::Green),
    ("red", Tone::Red),
    ("gray", Tone::Gray),
];
pub(crate) const TEMPLATES: &[(&str, Template)] = &[
    ("agents", Template::Agents),
    ("omarchy", Template::Omarchy),
    ("macropad", Template::Macropad),
    ("usage", Template::Usage),
    ("settings", Template::Settings),
    ("arcade", Template::Arcade),
    ("system", Template::System),
];
pub(crate) const BINDINGS: &[(&str, Binding)] = &[
    ("left", Binding::Left),
    ("right", Binding::Right),
    ("enter", Binding::Enter),
    ("cw", Binding::Cw),
    ("ccw", Binding::Ccw),
    ("shift+left", Binding::ShiftLeft),
    ("shift+right", Binding::ShiftRight),
    ("shift+enter", Binding::ShiftEnter),
];
pub(crate) const KEYS: &[(&str, KeyId)] = &[
    ("enter", KeyId::Enter),
    ("cancel", KeyId::Cancel),
    ("left", KeyId::Left),
    ("right", KeyId::Right),
    ("cw", KeyId::Cw),
    ("ccw", KeyId::Ccw),
];
pub(crate) const CTXS: &[(&str, PttCtx)] = &[
    ("talk", PttCtx::Talk),
    ("answer", PttCtx::Answer),
    ("name", PttCtx::Name),
];
pub(crate) const WHYS: &[(&str, ErrWhy)] = &[
    ("size", ErrWhy::Size),
    ("parse", ErrWhy::Parse),
    ("range", ErrWhy::Range),
    ("unknown", ErrWhy::Unknown),
];

pub(crate) fn name_of<T: PartialEq + Copy>(table: &[(&'static str, T)], value: T) -> &'static str {
    // Every table covers its enum; "" would only mean a table fell behind.
    table
        .iter()
        .find(|(_, member)| *member == value)
        .map_or("", |(name, _)| name)
}

pub(crate) fn member_of<T: Copy>(table: &[(&'static str, T)], name: &str) -> Option<T> {
    table
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, member)| *member)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names_match_the_wire_vocabulary() {
        assert_eq!(serde_json::to_string(&Kind::Claude).unwrap(), "\"claude\"");
        assert_eq!(serde_json::to_string(&CardKind::Q).unwrap(), "\"q\"");
        assert_eq!(
            serde_json::to_string(&Binding::ShiftEnter).unwrap(),
            "\"shift+enter\""
        );
        assert_eq!(
            serde_json::to_string(&SetKey::Idlehome).unwrap(),
            "\"idlehome\""
        );
        for (name, key) in KEYS {
            assert_eq!(serde_json::to_string(key).unwrap(), format!("\"{name}\""));
        }
        for (name, binding) in BINDINGS {
            assert_eq!(member_of(BINDINGS, name), Some(*binding));
            assert_eq!(name_of(BINDINGS, *binding), *name);
        }
    }

    #[test]
    fn set_key_ranges_follow_the_contract() {
        assert!(SetKey::Speaker.accepts(10));
        assert!(!SetKey::Speaker.accepts(11));
        assert!(!SetKey::Brightness.accepts(0));
        assert!(SetKey::Brightness.accepts(5));
        assert!(!SetKey::Chime.accepts(2));
        assert!(SetKey::Idlehome.accepts(45));
        assert!(SetKey::Textsize.accepts(2));
        assert!(!SetKey::Textsize.accepts(3));
        assert_eq!(SetKey::ALL.len(), 10);
    }

    #[test]
    fn line_m_names_match_the_protocol_table() {
        assert_eq!(HostLine::Synced.m(), "synced");
        assert_eq!(HostLine::AgentGone { slot: 1 }.m(), "agent");
        assert_eq!(HostLine::CardClear { slot: 1, id: 1 }.m(), "card");
        assert_eq!(DeviceLine::WsGo(1).m(), "ws");
        assert_eq!(DeviceLine::Up.m(), "up");
    }
}
