//! The channel-5 vocabulary: what a host can tell the deck (`HostMsg`) and what
//! the deck tells the host (`Event`). Plain old data only. `proto` turns these
//! into JSON lines and back; `model` and `ui::state` give them meaning.
use super::text::Text;

/// The firmware version the deck reports: `hello.fw` and Settings/About. The
/// channel-2 (`sys.version`) version strings of the two runtimes start with it.
pub const FW_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Other,
    Conol,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentState {
    Working,
    Question,
    Ready,
    Error,
    Idle,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardKind {
    Question,
    Ready,
}

/// Bytes of one wrapped card line and lines per window (§6.5 `card_line` /
/// `card_lines`): the host breaks the question body into lines the deck can
/// draw, and sends them a window at a time.
pub const DOC_LINE: usize = 48;
/// Lines a window carries.
///
/// **17 is the ceiling**, not a preference: the widest possible `doc` line is
/// 706 B at twelve lines and each further line of `DOC_LINE` bytes costs 51
/// (48 + its quotes + the comma), so 17 comes to 961 B and 18 to 1012, past
/// the 1000 B `CAPS.line`. More lines is less stalling — a window is ~2.5 s
/// away on the real link, and this is the headroom that covers it.
pub const DOC_LINES: usize = 17;

/// Lines of a document the deck **buffers**, as opposed to the `DOC_LINES` a
/// single wire line carries.
///
/// One chunk is not enough to scroll on. Measured 2026-09-23: a chunk takes
/// 2.0-2.5 s to arrive and a brisk scroll is a line every 200 ms, so holding
/// one 17-line chunk left about 1.2 s of scroll against a 2.5 s refill — the
/// wheel was dead **66%** of the time. Buffering 40 lines puts 34 ahead of the
/// reader, 6.8 s, so a refill lands well before the buffer runs out. This is
/// what a browser does and what the deck was missing.
///
/// 40 x 48 B plus bookkeeping is 1,936 B a document; a card body and a reply
/// for eight agents plus the transcript is 32 KB, 12% of the 318 KB free.
pub const DOC_HELD: usize = 40;

/// The font the host measured a card's wrapped lines with. The deck must draw
/// them with the same one, or the lines the host counted are not the lines the
/// deck shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocFont {
    Title,
    Name,
    Row,
    /// 29 px Regular: the reply lane one step up from `Row`.
    Text,
    RichRow,
    RichText,
    RichTitle,
}

impl DocFont {
    pub const fn rich(self) -> bool {
        matches!(self, Self::RichRow | Self::RichText | Self::RichTitle)
    }
    /// The face the `text` size setting selects for a document lane.
    ///
    /// **Host and device both read this.** The host wraps with the face it
    /// names and the deck draws with the same one; a size the two sides
    /// disagreed about would break every line in a different place, which is
    /// the defect the whole host-side-layout design exists to avoid.
    pub const fn for_size(size: u8) -> Self {
        match size {
            0 => DocFont::Row,
            1 => DocFont::Text,
            _ => DocFont::Title,
        }
    }

    /// Lines of a **reply** the channel lane shows at once in this face.
    /// Measured against `CHANNEL_REPLY` (196 px): line heights are Row 31,
    /// Name 34, Text 36, Title 40 (`faces::agents` tests pin this).
    pub const fn reply_lines(self) -> usize {
        match self {
            DocFont::Row | DocFont::RichRow => 6,
            DocFont::Name | DocFont::Text | DocFont::Title | DocFont::RichText | DocFont::RichTitle => 5,
        }
    }

    /// Lines of a **question body** the card lane shows at once in this face.
    pub const fn question_lines(self) -> usize {
        match self {
            DocFont::Title | DocFont::Text | DocFont::RichTitle | DocFont::RichText => 2,
            DocFont::Name | DocFont::Row | DocFont::RichRow => 3,
        }
    }
}

/// Which of a slot's long texts a window belongs to. One mechanism for the card
/// body, the last reply and the live transcript: they differ only in the lane
/// that draws them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocKind {
    Card,
    Reply,
    Workflow,
    Tr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Blue,
    Amber,
    Green,
    Red,
    Gray,
}

/// Live computer metrics; zero capacity and absent percentages mean unavailable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// What a face is; several strip entries may share one template.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Template {
    Agents,
    Omarchy,
    Macropad,
    Usage,
    Settings,
    Arcade,
    System,
}
impl Template {
    pub const ALL: [Template; 7] = [
        Template::Agents,
        Template::Omarchy,
        Template::Macropad,
        Template::Usage,
        Template::Settings,
        Template::Arcade,
        Template::System,
    ];
    /// Append enum variants to keep existing face IDs stable as the strip grows.
    pub const fn default_id(self) -> u8 { self as u8 + 1 }
    /// Header label and the default instance name.
    pub const fn label(self) -> &'static str {
        match self {
            Template::Agents => "Agents",
            Template::Omarchy => "Omarchy",
            Template::Macropad => "Macropad",
            Template::Usage => "Usage",
            Template::Settings => "Settings",
            Template::Arcade => "Arcade",
            Template::System => "System",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingKey {
    Speaker,
    Mic,
    Announce,
    Chime,
    AutoJump,
    Brightness,
    IdleHome,
    Demo,
    /// How big documents are drawn: 0 small, 1 medium, 2 large. The host is
    /// told, because the host is what wraps them.
    TextSize,
    /// 0 off, 1 effects only, 2 music and effects.
    ArcadeSound,
}

/// Keys a host-rendered face forwards. SHIFT and PTT never travel as keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyId {
    Enter,
    Cancel,
    Left,
    Right,
    Cw,
    Ccw,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttCtx {
    Talk,
    Answer,
    Name,
}

/// Hardware control bound to a macro key; `None` is touch-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Binding {
    None,
    Left,
    Right,
    Enter,
    Cw,
    Ccw,
    ShiftLeft,
    ShiftRight,
    ShiftEnter,
}

/// Modifier bits of a `Keystroke`, in the Mac's display order.
pub const MOD_CTRL: u8 = 1;
pub const MOD_OPT: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_CMD: u8 = 8;

/// The keys a keystroke may name: (wire name, what the deck shows), in the
/// key editor's order. The host maps the names to its own key codes.
pub const KEYS: [(&str, &str); 73] = [
    ("a", "A"),
    ("b", "B"),
    ("c", "C"),
    ("d", "D"),
    ("e", "E"),
    ("f", "F"),
    ("g", "G"),
    ("h", "H"),
    ("i", "I"),
    ("j", "J"),
    ("k", "K"),
    ("l", "L"),
    ("m", "M"),
    ("n", "N"),
    ("o", "O"),
    ("p", "P"),
    ("q", "Q"),
    ("r", "R"),
    ("s", "S"),
    ("t", "T"),
    ("u", "U"),
    ("v", "V"),
    ("w", "W"),
    ("x", "X"),
    ("y", "Y"),
    ("z", "Z"),
    ("0", "0"),
    ("1", "1"),
    ("2", "2"),
    ("3", "3"),
    ("4", "4"),
    ("5", "5"),
    ("6", "6"),
    ("7", "7"),
    ("8", "8"),
    ("9", "9"),
    ("f1", "F1"),
    ("f2", "F2"),
    ("f3", "F3"),
    ("f4", "F4"),
    ("f5", "F5"),
    ("f6", "F6"),
    ("f7", "F7"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("f10", "F10"),
    ("f11", "F11"),
    ("f12", "F12"),
    ("up", "\u{2191}"),
    ("down", "\u{2193}"),
    ("left", "\u{2190}"),
    ("right", "\u{2192}"),
    ("enter", "\u{21b5}"),
    ("esc", "esc"),
    ("tab", "tab"),
    ("space", "space"),
    ("backspace", "bksp"),
    ("delete", "del"),
    ("home", "home"),
    ("end", "end"),
    ("pageup", "pgup"),
    ("pagedown", "pgdn"),
    ("minus", "-"),
    ("equal", "="),
    ("lbracket", "["),
    ("rbracket", "]"),
    ("backslash", "\\"),
    ("semicolon", ";"),
    ("quote", "'"),
    ("comma", ","),
    ("period", "."),
    ("slash", "/"),
    ("grave", "`")
];

/// What a macro key sends through the host when it is pressed: modifier
/// bits and one of `KEYS`. On the wire it is `ctrl+opt+shift+cmd+<name>`
/// with only the set modifiers (`wire`); on the deck it shows as the Mac
/// glyphs (`shown`). The host types it (deckd: CoreGraphics, with the
/// Accessibility grant); the deck itself is never a keyboard, because its
/// USB block has a single IN endpoint and a keyboard collection beside the
/// vendor one would make macOS gate the deck behind Input Monitoring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Keystroke {
    pub mods: u8,
    /// 1-based index into `KEYS`; 0 = no keystroke.
    pub key: u8,
}

impl Keystroke {
    pub const NONE: Self = Self { mods: 0, key: 0 };

    pub const fn is_some(self) -> bool {
        self.key != 0
    }

    /// The key's wire name ("" for none).
    pub fn name(self) -> &'static str {
        KEYS.get(self.key.wrapping_sub(1) as usize).map_or("", |key| key.0)
    }

    /// The key as the deck shows it ("" for none).
    pub fn shown_key(self) -> &'static str {
        KEYS.get(self.key.wrapping_sub(1) as usize).map_or("", |key| key.1)
    }

    /// `cmd+shift+4`, modifiers in any order, exactly one key; `alt` is
    /// accepted for `opt`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut out = Self::NONE;
        for part in text.split('+') {
            match part {
                "ctrl" => out.mods |= MOD_CTRL,
                "opt" | "alt" => out.mods |= MOD_OPT,
                "shift" => out.mods |= MOD_SHIFT,
                "cmd" => out.mods |= MOD_CMD,
                name => {
                    if out.key != 0 {
                        return None;
                    }
                    out.key = KEYS.iter().position(|key| key.0 == name)? as u8 + 1;
                }
            }
        }
        out.is_some().then_some(out)
    }

    /// The canonical wire form.
    pub fn wire<const N: usize>(self, out: &mut Text<N>) {
        out.clear();
        for (bit, name) in [(MOD_CTRL, "ctrl"), (MOD_OPT, "opt"), (MOD_SHIFT, "shift"), (MOD_CMD, "cmd")] {
            if self.mods & bit != 0 {
                out.push_str(name);
                out.push_char('+');
            }
        }
        out.push_str(self.name());
    }

    /// The glyphs the deck shows: the modifiers, then the key.
    pub fn shown<const N: usize>(self, out: &mut Text<N>) {
        out.clear();
        for (bit, glyph) in [
            (MOD_CTRL, '\u{2303}'),
            (MOD_OPT, '\u{2325}'),
            (MOD_SHIFT, '\u{21e7}'),
            (MOD_CMD, '\u{2318}'),
        ] {
            if self.mods & bit != 0 {
                out.push_char(glyph);
            }
        }
        out.push_str(self.shown_key());
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacroKey {
    pub label: Text<12>,
    pub binding: Binding,
    /// What the host sends when the key is pressed; `NONE` = an event only.
    pub keys: Keystroke,
    /// False draws the inert "+ assign" placeholder.
    pub present: bool,
}
impl MacroKey {
    pub const EMPTY: Self = Self {
        label: Text::new(),
        binding: Binding::None,
        keys: Keystroke::NONE,
        present: false,
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UsageItem {
    pub name: Text<20>,
    pub account: Text<48>,
    pub percent: u8,
    pub reset: Text<24>,
    pub reset_tip: Text<32>,
    /// `None` derives the bar tone from `percent`.
    pub tone: Option<Tone>,
}
impl UsageItem {
    pub const EMPTY: Self = Self {
        name: Text::new(),
        account: Text::new(),
        percent: 0,
        reset: Text::new(),
        reset_tip: Text::new(),
        tone: None,
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowRow {
    pub tone: Tone,
    pub title: Text<48>,
}
impl WindowRow {
    pub const EMPTY: Self = Self {
        tone: Tone::Gray,
        title: Text::new(),
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaceEntry {
    /// 1..=8, stable for the life of the instance; 0 marks an empty slot.
    pub id: u8,
    pub template: Template,
    pub name: Text<16>,
}
impl FaceEntry {
    pub const EMPTY: Self = Self {
        id: 0,
        template: Template::Agents,
        name: Text::new(),
    };
    pub const fn new(id: u8, template: Template, name: &str) -> Self {
        Self {
            id,
            template,
            name: Text::from_str(name),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrWhy {
    Size,
    Parse,
    Range,
    Unknown,
}

/// One bounded picker page, shared by the session overview and its selectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionMenu {
    pub agent: u8, pub rev: u16, pub field: u8, pub page: u8,
    pub model: Text<48>, pub effort: Text<16>, pub status: u8,
    pub note: Text<96>, pub choices: [Text<40>; 4],
    pub count: u8, pub total: u8, pub selected: Option<u8>,
}
impl SessionMenu {
    pub const EMPTY: Self = Self {
        agent: 0, rev: 0, field: 0, page: 0, model: Text::new(),
        effort: Text::new(), status: 0, note: Text::new(),
        choices: [Text::new(); 4], count: 0, total: 0, selected: None,
    };
}

/// One parsed host message. The parser validates all of it before anything is
/// applied. Over half a kilobyte, most of it one `cardtext` window: parse into
/// an out-parameter and pass by reference, never by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostMsg {
    Image { agent: u8, rev: u16, index: u8, offset: u16, total: u16, width: u16, height: u16, len: u16, data: [u8; 384] },
    SessionMenu(SessionMenu),
    /// Nothing to apply: an unknown `m`, or the out-parameter before parsing.
    None,
    Hello {
        v: u8,
        sid: u32,
        minute_of_day: Option<u16>,
    },
    Synced,
    /// An absent field keeps its value.
    Agent {
        id: u8,
        gone: bool,
        kind: Option<AgentKind>,
        state: Option<AgentState>,
        name: Option<Text<24>>,
        /// The project the session runs in (`proj`, the working directory's
        /// last component): the roster groups its rows by it. An empty one is
        /// "no project".
        project: Option<Text<24>>,
        title: Option<Text<64>>,
        since_s: Option<u32>,
    },
    Card {
        agent: u8,
        id: u16,
        clear: bool,
        kind: CardKind,
        text: Text<128>,
        choices: [Text<32>; 4],
        choice_count: u8,
        /// 0-based.
        recommended: Option<u8>,
    },
    /// A window of the card's question body, wrapped by the host: `lines` are
    /// the `line_count` lines from `first` of the `total` the whole text
    /// wraps to. The deck draws them as they are — it never wraps a body the
    /// host has already measured, because two wraps can disagree.
    DocText {
        agent: u8,
        doc: DocKind,
        id: u16,
        first: u16,
        total: u16,
        font: DocFont,
        lines: [Text<DOC_LINE>; DOC_LINES],
        line_count: u8,
    },
    Reply {
        agent: u8,
        text: Text<200>,
        rev: u16,
    },
    /// Cumulative; the host sends the tail.
    Transcript {
        agent: u8,
        text: Text<200>,
        last: bool,
        press: Option<u16>,
        failed: bool,
        /// Show the Claude Channels setup hint during this device PTT session.
        channel_tip: bool,
    },
    Focus {
        agent: u8,
        ack: Option<u16>,
    },
    Workspaces {
        active: u8,
        count: u8,
        selected: u8,
        windows: [WindowRow; 6],
        window_count: u8,
    },
    Macro {
        face: u8,
        /// `src`; absent keeps the instance's previous source.
        source: Option<Text<16>>,
        keys: [MacroKey; 9],
    },
    System(SystemStatus),
    Usage {
        period: Text<12>,
        items: [UsageItem; 6],
        count: u8,
    },
    Faces {
        list: [FaceEntry; 8],
        count: u8,
    },
    Name {
        text: Text<16>,
    },
    Set {
        key: SettingKey,
        value: u16,
    },
}

/// One device-to-host message: ids only, at most 8 bytes. Lists (`Hello`,
/// `Faces`) are read from `UiState` when the event is encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Image { agent: u8, rev: u16, index: u8, offset: u16 },
    Session { agent: u8, field: u8, page: u8, rev: u16, choice: u8 },
    Hello,
    /// Everything is released.
    Up,
    Faces,
    Refresh,
    Key {
        key: KeyId,
        /// 1 press, 0 release, 2 step.
        act: u8,
        shift: bool,
        face: u8,
    },
    Channel {
        agent: u8,
        seq: u16,
    },
    Ptt {
        act: u8,
        agent: u8,
        ctx: PttCtx,
        /// 0 = none.
        card: u16,
        abort: bool,
        press: u16,
    },
    Answer {
        agent: u8,
        card: u16,
        choice: u8,
    },
    Snooze {
        agent: u8,
        card: u16,
    },
    /// Ask for the window of a card's body that holds line `first`; the host
    /// answers with a `cardtext` around it. Sent while scrolling, so the next
    /// lines are on the device before the wheel asks for them.
    DocLines {
        agent: u8,
        doc: DocKind,
        id: u16,
        first: u16,
    },
    /// Toggle one tool entry in this conversation document revision.
    Tool { agent: u8, id: u16, index: u8 },
    Macro {
        face: u8,
        index: u8,
        act: u8,
    },
    /// The deck's own key editor set or cleared one slot of a Macropad; the
    /// key itself is read from the deck's set when the event is encoded.
    /// The host persists it and re-sends the whole `macro`.
    MacroKey {
        face: u8,
        index: u8,
    },
    WsGo(u8),
    WsWin(u8),
    Set {
        key: SettingKey,
        value: u16,
    },
    Face {
        id: u8,
    },
    Err {
        why: ErrWhy,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reported_version_is_the_package_version() {
        assert_eq!(FW_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn events_stay_small_and_host_messages_bounded() {
        assert!(core::mem::size_of::<Event>() <= 8);
        assert!(core::mem::size_of::<Option<Event>>() <= 8);
        // A `doc` window is the largest message: DOC_LINES (17) lines of
        // DOC_LINE (48) bytes plus their lengths, and the
        // slot/doc/id/first/total/font around them. Measured 860 B.
        assert!(core::mem::size_of::<HostMsg>() <= 896);
        assert!(core::mem::size_of::<FaceEntry>() <= 20);
    }

    #[test]
    fn everything_is_const_constructible_for_statics() {
        static MSG: HostMsg = HostMsg::None;
        static KEYS: [MacroKey; 9] = [MacroKey::EMPTY; 9];
        static ITEMS: [UsageItem; 6] = [UsageItem::EMPTY; 6];
        static WINDOWS: [WindowRow; 6] = [WindowRow::EMPTY; 6];
        static STRIP: [FaceEntry; 2] = [
            FaceEntry::new(1, Template::Agents, "Agents"),
            FaceEntry::EMPTY,
        ];
        assert_eq!(MSG, HostMsg::None);
        assert!(KEYS.iter().all(|key| !key.present));
        assert!(ITEMS.iter().all(|item| item.name.is_empty()));
        assert!(WINDOWS.iter().all(|row| row.title.is_empty()));
        assert_eq!(STRIP[0].name, "Agents");
        assert_eq!(STRIP[1].id, 0);
    }

    #[test]
    fn template_labels_fit_an_instance_name() {
        for template in Template::ALL {
            let entry = FaceEntry::new(1, template, template.label());
            assert_eq!(entry.name, template.label());
        }
    }

    #[test]
    fn keystrokes_round_trip_the_wire_and_show_as_mac_glyphs() {
        let mut wire = Text::<32>::new();
        let mut shown = Text::<16>::new();
        for (index, (name, _)) in KEYS.iter().enumerate() {
            let stroke = Keystroke { mods: MOD_CMD | MOD_SHIFT, key: index as u8 + 1 };
            stroke.wire(&mut wire);
            assert_eq!(Keystroke::parse(wire.as_str()), Some(stroke), "{name}");
            assert!(wire.as_str().ends_with(name));
            stroke.shown(&mut shown);
            assert!(shown.as_str().starts_with("\u{21e7}\u{2318}"), "{name}: {}", shown.as_str());
            assert!(!shown.is_empty() && wire.len() <= 32 && shown.len() <= 16);
        }
        let stroke = Keystroke::parse("shift+cmd+4").unwrap();
        stroke.wire(&mut wire);
        assert_eq!(wire.as_str(), "shift+cmd+4", "canonical order");
        stroke.shown(&mut shown);
        assert_eq!(shown.as_str(), "\u{21e7}\u{2318}4");
        assert_eq!(Keystroke::parse("ctrl+alt+delete").map(|k| k.mods), Some(MOD_CTRL | MOD_OPT));
        for bad in ["", "cmd", "cmd+", "cmd+c+v", "super+c", "cmd+ptt"] {
            assert_eq!(Keystroke::parse(bad), None, "{bad}");
        }
        assert_eq!(Keystroke::NONE.name(), "");
        assert!(!Keystroke::NONE.is_some());
    }
}
