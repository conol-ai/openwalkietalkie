//! HID channel 5: one compact JSON object per LF-terminated line, at most
//! `LINE_MAX` bytes, carried in `[6?, 5, len<=61, bytes…]` report bodies.
//! The wire contract for daemon authors is `docs/deck-protocol.md`; this file
//! is its implementation: `LineAssembler` (bodies -> lines), `parse` (line ->
//! validated `HostMsg`), `encode` (`Event` + `UiState` -> line) and `frame`
//! (line -> report bodies).
//!
//! `parse` validates the whole line before `out` holds anything but `None`:
//! a malformed line is `Parse`, an id, index or count outside its range is
//! `Range`, a `m` or enum member the firmware does not know is `Unknown`.
//! Unknown fields are ignored; an absent optional field is `None` so the
//! model keeps its value.
use super::json::{Object, Value, Writer};
use super::model::MAX_AGENTS;
use super::msg::{
    AgentKind, AgentState, Binding, DocFont, DocKind, CardKind, ErrWhy, Event, HostMsg, KeyId, MacroKey,
    PttCtx, SettingKey, Template, Tone, UsageItem, WindowRow, DOC_LINES, FW_VERSION, Keystroke,
};
use super::text::Text;
use crate::ui::UiState;

pub const CHANNEL: u8 = 5;
/// `hello.v` in both directions.
pub const VERSION: u8 = 1;
/// Longest host line including its LF.
pub const LINE_MAX: usize = 1024;
/// Longest encoded event (`hello` with eight named faces).
pub const TX_MAX: usize = 640;
pub const REPORT_ID: u8 = 6;
pub const REPORT_LEN: usize = 64;
pub const PAYLOAD_MAX: usize = 61;
/// Longest per-edge event (`key`, `macro`, `answer`, `channel`, `snooze`,
/// `face`, and `ptt` without `abort`): one report with its LF.
pub const EDGE_MAX: usize = PAYLOAD_MAX - 1;
/// Highest face id (`FaceEntry::id`).
pub const MAX_FACE_ID: u8 = 8;
/// Workspace chips the Omarchy face can draw (`layout::OMARCHY_CHIPS`).
pub const MAX_WORKSPACES: u8 = 9;

/// Reassembles channel-5 lines from report bodies. Several messages may share
/// or straddle reports; an overlong line is discarded up to its LF.
pub struct LineAssembler {
    line: [u8; LINE_MAX],
    len: usize,
    discarding: bool,
}

impl LineAssembler {
    pub const fn new() -> Self {
        Self {
            line: [0; LINE_MAX],
            len: 0,
            discarding: false,
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.discarding = false;
    }

    /// Feed one report body, with or without its leading report id; other
    /// channels are ignored. `f` sees every completed line (without the LF),
    /// or `Err(Size)` once per line that did not fit. Empty lines are skipped.
    pub fn push(&mut self, report: &[u8], f: &mut dyn FnMut(Result<&[u8], ErrWhy>)) {
        let body = match report {
            [REPORT_ID, CHANNEL, ..] => &report[1..],
            _ => report,
        };
        let [CHANNEL, len, payload @ ..] = body else {
            return;
        };
        let payload = &payload[..(*len as usize).min(PAYLOAD_MAX).min(payload.len())];
        for &byte in payload {
            if byte == b'\n' {
                if self.discarding {
                    f(Err(ErrWhy::Size));
                } else if self.len != 0 {
                    f(Ok(&self.line[..self.len]));
                }
                self.clear();
            } else if self.len + 1 >= LINE_MAX {
                self.discarding = true;
            } else if !self.discarding {
                self.line[self.len] = byte;
                self.len += 1;
            }
        }
    }
}

// ---- vocabulary --------------------------------------------------------------

const KINDS: &[(&str, AgentKind)] = &[
    ("claude", AgentKind::Claude),
    ("codex", AgentKind::Codex),
    ("gemini", AgentKind::Gemini),
    ("other", AgentKind::Other),
    ("conol", AgentKind::Conol),
];
const STATES: &[(&str, AgentState)] = &[
    ("working", AgentState::Working),
    ("question", AgentState::Question),
    ("ready", AgentState::Ready),
    ("error", AgentState::Error),
    ("idle", AgentState::Idle),
];
const CARD_KINDS: &[(&str, CardKind)] = &[("q", CardKind::Question), ("r", CardKind::Ready)];
const fn doc_name(doc: DocKind) -> &'static str {
    match doc {
        DocKind::Card => "c",
        DocKind::Reply => "r",
        DocKind::Workflow => "w",
        DocKind::Tr => "t",
    }
}

const DOC_FONTS: &[(&str, DocFont)] =
    &[("title", DocFont::Title), ("name", DocFont::Name), ("row", DocFont::Row), ("text", DocFont::Text),
      ("richrow", DocFont::RichRow), ("richtext", DocFont::RichText), ("richtitle", DocFont::RichTitle)];
/// Single letters, not words: `doclines` is a device->host line and must fit
/// one 61-byte report. With `"d":"r"` the longest one encodes to 63 B and
/// would be dropped by the framer; `"d":"r"` makes it 59.
const DOC_KINDS: &[(&str, DocKind)] =
    &[("c", DocKind::Card), ("r", DocKind::Reply), ("w", DocKind::Workflow), ("t", DocKind::Tr)];
const TONES: &[(&str, Tone)] = &[
    ("blue", Tone::Blue),
    ("amber", Tone::Amber),
    ("green", Tone::Green),
    ("red", Tone::Red),
    ("gray", Tone::Gray),
];
const TEMPLATES: &[(&str, Template)] = &[
    ("agents", Template::Agents),
    ("omarchy", Template::Omarchy),
    ("macropad", Template::Macropad),
    ("usage", Template::Usage),
    ("settings", Template::Settings),
    ("arcade", Template::Arcade),
    ("system", Template::System),
];
const SETTINGS: &[(&str, SettingKey)] = &[
    ("arcadesound", SettingKey::ArcadeSound),
    ("speaker", SettingKey::Speaker),
    ("mic", SettingKey::Mic),
    ("announce", SettingKey::Announce),
    ("chime", SettingKey::Chime),
    ("autojump", SettingKey::AutoJump),
    ("brightness", SettingKey::Brightness),
    ("idlehome", SettingKey::IdleHome),
    ("demo", SettingKey::Demo),
    ("text", SettingKey::TextSize),
];
const BINDINGS: &[(&str, Binding)] = &[
    ("left", Binding::Left),
    ("right", Binding::Right),
    ("enter", Binding::Enter),
    ("cw", Binding::Cw),
    ("ccw", Binding::Ccw),
    ("shift+left", Binding::ShiftLeft),
    ("shift+right", Binding::ShiftRight),
    ("shift+enter", Binding::ShiftEnter),
];

fn name_of<T: PartialEq + Copy>(table: &[(&'static str, T)], value: T) -> &'static str {
    // Every table covers its enum; "" would only mean a table fell behind.
    table
        .iter()
        .find(|(_, member)| *member == value)
        .map_or("", |(name, _)| name)
}

const fn key_name(key: KeyId) -> &'static str {
    match key {
        KeyId::Enter => "enter",
        KeyId::Cancel => "cancel",
        KeyId::Left => "left",
        KeyId::Right => "right",
        KeyId::Cw => "cw",
        KeyId::Ccw => "ccw",
    }
}

const fn ctx_name(ctx: PttCtx) -> &'static str {
    match ctx {
        PttCtx::Talk => "talk",
        PttCtx::Answer => "answer",
        PttCtx::Name => "name",
    }
}

const fn why_name(why: ErrWhy) -> &'static str {
    match why {
        ErrWhy::Size => "size",
        ErrWhy::Parse => "parse",
        ErrWhy::Range => "range",
        ErrWhy::Unknown => "unknown",
    }
}

// ---- host -> device ------------------------------------------------------------

/// Parse one line into `out`, validating all of it first. On any error `out`
/// is `HostMsg::None`.
pub fn parse(line: &[u8], out: &mut HostMsg) -> Result<(), ErrWhy> {
    let result = parse_into(line, out);
    if result.is_err() {
        *out = HostMsg::None;
    }
    result
}

// Whole messages are const so `*out = X` copies a template instead of
// building a 300-byte value on the stack; the fields are then filled in place.
const AGENT: HostMsg = HostMsg::Agent {
    id: 0,
    gone: false,
    kind: None,
    state: None,
    name: None,
    project: None,
    title: None,
    since_s: None,
};
const CARD: HostMsg = HostMsg::Card {
    agent: 0,
    id: 0,
    clear: false,
    kind: CardKind::Question,
    text: Text::new(),
    choices: [Text::new(); 4],
    choice_count: 0,
    recommended: None,
};
const DOC_TEXT: HostMsg = HostMsg::DocText {
    agent: 0,
    doc: DocKind::Card,
    id: 0,
    first: 0,
    total: 0,
    font: DocFont::Title,
    lines: [Text::new(); DOC_LINES],
    line_count: 0,
};
const REPLY: HostMsg = HostMsg::Reply {
    agent: 0,
    text: Text::new(),
    rev: 0,
};
const TRANSCRIPT: HostMsg = HostMsg::Transcript {
    agent: 0,
    text: Text::new(),
    last: false, press: None, failed: false, channel_tip: false,
};
const WORKSPACES: HostMsg = HostMsg::Workspaces {
    active: 0,
    count: 0,
    selected: 0,
    windows: [WindowRow::EMPTY; 6],
    window_count: 0,
};
const MACRO: HostMsg = HostMsg::Macro {
    face: 0,
    source: None,
    keys: [MacroKey::EMPTY; 9],
};
const USAGE: HostMsg = HostMsg::Usage {
    period: Text::new(),
    items: [UsageItem::EMPTY; 6],
    count: 0,
};
const FACES: HostMsg = HostMsg::Faces {
    list: [super::msg::FaceEntry::EMPTY; 8],
    count: 0,
};
const NAME: HostMsg = HostMsg::Name { text: Text::new() };

fn agent_id(value: Value) -> Result<u8, ErrWhy> {
    match value.u8()? {
        id @ 1..=8 if id as usize <= MAX_AGENTS => Ok(id),
        _ => Err(ErrWhy::Range),
    }
}

fn face_id(value: Value) -> Result<u8, ErrWhy> {
    match value.u8()? {
        id @ 1..=MAX_FACE_ID => Ok(id),
        _ => Err(ErrWhy::Range),
    }
}

/// An optional flag; absent is false.
fn flag(object: &Object, key: &str) -> Result<bool, ErrWhy> {
    object.get(key)?.map_or(Ok(false), |v| v.bool())
}

fn opt_text<const N: usize>(
    object: &Object,
    key: &str,
    out: &mut Option<Text<N>>,
) -> Result<(), ErrWhy> {
    if let Some(value) = object.get(key)? {
        value.text_into(out.insert(Text::new()))?;
    }
    Ok(())
}

/// `"19:42"` as the minute of the day.
fn clock(value: Value) -> Result<u16, ErrWhy> {
    let [h1, h0, b':', m1, m0] = value.raw_str().ok_or(ErrWhy::Parse)? else {
        return Err(ErrWhy::Parse);
    };
    let digit = |d: &u8| d.checked_sub(b'0').filter(|d| *d < 10).ok_or(ErrWhy::Parse);
    let hour = digit(h1)? * 10 + digit(h0)?;
    let minute = digit(m1)? * 10 + digit(m0)?;
    if hour >= 24 || minute >= 60 {
        return Err(ErrWhy::Range);
    }
    Ok(hour as u16 * 60 + minute as u16)
}

fn parse_into(line: &[u8], out: &mut HostMsg) -> Result<(), ErrWhy> {
    *out = HostMsg::None;
    let object = Object::parse(line)?;
    let m = object.need("m")?;
    let m = m.raw_str().ok_or(ErrWhy::Parse)?;
    match m {
        b"hello" => {
            let v = object.need("v")?.u8()?;
            let sid = object.need("sid")?.u32()?;
            let minute_of_day = object.get("hm")?.map(clock).transpose()?;
            *out = HostMsg::Hello {
                v,
                sid,
                minute_of_day,
            };
        }
        b"synced" => *out = HostMsg::Synced,
        b"agent" => {
            *out = AGENT;
            let HostMsg::Agent {
                id,
                gone,
                kind,
                state,
                name,
                project,
                title,
                since_s,
            } = out
            else {
                return Err(ErrWhy::Parse);
            };
            *id = agent_id(object.need("id")?)?;
            *gone = flag(&object, "gone")?;
            if let Some(value) = object.get("kind")? {
                *kind = Some(value.member(KINDS)?);
            }
            if let Some(value) = object.get("state")? {
                *state = Some(value.member(STATES)?);
            }
            opt_text(&object, "name", name)?;
            opt_text(&object, "proj", project)?;
            opt_text(&object, "title", title)?;
            if let Some(value) = object.get("since")? {
                *since_s = Some(value.u32()?);
            }
        }
        b"card" => {
            *out = CARD;
            let HostMsg::Card {
                agent,
                id,
                clear,
                kind,
                text,
                choices,
                choice_count,
                recommended,
            } = out
            else {
                return Err(ErrWhy::Parse);
            };
            *agent = agent_id(object.need("agent")?)?;
            *id = object.need("id")?.u16()?;
            *clear = flag(&object, "clear")?;
            if !*clear {
                *kind = object.need("type")?.member(CARD_KINDS)?;
                object.need("text")?.text_into(text)?;
                if let Some(list) = object.get("choices")? {
                    for item in list.array()? {
                        let slot = choices.get_mut(*choice_count as usize).ok_or(ErrWhy::Range)?;
                        item?.text_into(slot)?;
                        *choice_count += 1;
                    }
                }
                if let Some(value) = object.get("rec")? {
                    let rec = value.u8()?;
                    if rec >= *choice_count {
                        return Err(ErrWhy::Range);
                    }
                    *recommended = Some(rec);
                }
            }
        }
        b"doc" => {
            *out = DOC_TEXT;
            let HostMsg::DocText {
                agent,
                doc,
                id,
                first,
                total,
                font,
                lines,
                line_count,
            } = out
            else {
                return Err(ErrWhy::Parse);
            };
            *agent = agent_id(object.need("agent")?)?;
            *doc = object.need("d")?.member(DOC_KINDS)?;
            *id = object.need("id")?.u16()?;
            *first = object.need("first")?.u16()?;
            *total = object.need("total")?.u16()?;
            *font = object.need("font")?.member(DOC_FONTS)?;
            for item in object.need("lines")?.array()? {
                let slot = lines.get_mut(*line_count as usize).ok_or(ErrWhy::Range)?;
                item?.text_into(slot)?;
                *line_count += 1;
            }
            // A window that runs past the end of the text it belongs to is a
            // host bug: the deck would scroll to a line that does not exist.
            if u32::from(*first) + u32::from(*line_count) > u32::from(*total) {
                return Err(ErrWhy::Range);
            }
        }
        b"reply" => {
            *out = REPLY;
            let HostMsg::Reply { agent, text, rev } = out else {
                return Err(ErrWhy::Parse);
            };
            *agent = agent_id(object.need("agent")?)?;
            object.need("text")?.text_into(text)?;
            *rev = object.need("rev")?.u16()?;
        }
        b"image" => {
            *out = HostMsg::Image { agent: 0, rev: 0, index: 0, offset: 0, total: 0, width: 0, height: 0, len: 0, data: [0;384] };
            let HostMsg::Image { agent, rev, index, offset, total, width, height, len, data } = out else { return Err(ErrWhy::Parse); };
            *agent = agent_id(object.need("agent")?)?;
            *rev = object.need("r")?.u16()?; *index = object.need("i")?.u8()?;
            *offset = object.need("o")?.u16()?; *total = object.need("n")?.u16()?;
            *width = object.need("w")?.u16()?; *height = object.need("h")?.u16()?;
            let mut encoded = Text::<512>::new(); object.need("data")?.text_into(&mut encoded)?;
            *len = decode_image_base64(encoded.as_str().as_bytes(), data)? as u16;
            if *width > 128 || *height > 128 || *total > 16896 || usize::from(*offset) + usize::from(*len) > usize::from(*total) && *total != 0 { return Err(ErrWhy::Range); }
        }
        b"session" => {
            *out = HostMsg::SessionMenu(super::msg::SessionMenu::EMPTY);
            let HostMsg::SessionMenu(menu) = out else { return Err(ErrWhy::Parse); };
            menu.agent = agent_id(object.need("agent")?)?;
            menu.rev = object.need("rev")?.u16()?;
            menu.field = object.need("field")?.u8()?;
            menu.page = object.need("page")?.u8()?;
            menu.status = object.need("status")?.u8()?;
            menu.total = object.need("total")?.u8()?;
            if menu.field > 2 || menu.page > 63 || menu.status > 3 { return Err(ErrWhy::Range); }
            object.need("model")?.text_into(&mut menu.model)?;
            object.need("effort")?.text_into(&mut menu.effort)?;
            object.need("note")?.text_into(&mut menu.note)?;
            if let Some(value) = object.get("selected")? {
                let i = value.u8()?;
                if i >= menu.total { return Err(ErrWhy::Range); }
                menu.selected = Some(i);
            }
            for item in object.need("choices")?.array()? {
                let text = menu.choices.get_mut(menu.count as usize).ok_or(ErrWhy::Range)?;
                item?.text_into(text)?;
                menu.count += 1;
            }
            if u16::from(menu.page) * 4 + u16::from(menu.count) > u16::from(menu.total) {
                return Err(ErrWhy::Range);
            }
        }
        b"transcript" => {
            *out = TRANSCRIPT;
            let HostMsg::Transcript { agent, text, last, press, failed, channel_tip } = out else {
                return Err(ErrWhy::Parse);
            };
            // No `agent`: the words of a session with nobody on the strip
            // (a face name, a key label), for whoever is holding PTT.
            *agent = object.get("agent")?.map(agent_id).transpose()?.unwrap_or(0);
            object.need("text")?.text_into(text)?;
            *last = flag(&object, "final")?;
            *press = object.get("p")?.map(|v| v.u16()).transpose()?;
            *failed = flag(&object, "failed")?;
            *channel_tip = flag(&object, "channel_tip")?;
        }
        b"focus" => {
            let agent = agent_id(object.need("agent")?)?;
            let ack = object.get("ack")?.map(|v| v.u16()).transpose()?;
            *out = HostMsg::Focus { agent, ack };
        }
        b"ws" => {
            *out = WORKSPACES;
            let HostMsg::Workspaces {
                active,
                count,
                selected,
                windows,
                window_count,
            } = out
            else {
                return Err(ErrWhy::Parse);
            };
            *active = object.need("active")?.u8()?;
            *count = object.need("count")?.u8()?;
            *selected = object.need("sel")?.u8()?;
            if let Some(list) = object.get("windows")? {
                for item in list.array()? {
                    let row = windows.get_mut(*window_count as usize).ok_or(ErrWhy::Range)?;
                    let item = item?.object()?;
                    row.tone = item.get("c")?.map_or(Ok(Tone::Gray), |v| v.member(TONES))?;
                    item.need("t")?.text_into(&mut row.title)?;
                    *window_count += 1;
                }
            }
            if *count > MAX_WORKSPACES || *active > *count || *selected >= (*window_count).max(1) {
                return Err(ErrWhy::Range);
            }
        }
        b"macro" => {
            *out = MACRO;
            let HostMsg::Macro { face, source, keys } = out else {
                return Err(ErrWhy::Parse);
            };
            *face = face_id(object.need("f")?)?;
            opt_text(&object, "src", source)?;
            for (index, item) in object.need("keys")?.array()?.enumerate() {
                let key = keys.get_mut(index).ok_or(ErrWhy::Range)?;
                let item = item?.object()?;
                item.need("l")?.text_into(&mut key.label)?;
                key.binding = item.get("b")?.map_or(Ok(Binding::None), |v| v.member(BINDINGS))?;
                key.keys = Keystroke::NONE;
                if let Some(value) = item.get("k")? {
                    let mut combo = super::text::Text::<32>::new();
                    value.text_into(&mut combo)?;
                    key.keys = Keystroke::parse(combo.as_str()).ok_or(ErrWhy::Unknown)?;
                }
                // An empty label is an empty slot: how the host keeps the
                // later keys in place after the deck's editor clears one.
                key.present = !key.label.is_empty();
            }
        }
        b"system" => {
            let status = super::msg::SystemStatus {
                cpu: object.get("cpu")?.map(|v| v.u8()).transpose()?,
                gpu: object.get("gpu")?.map(|v| v.u8()).transpose()?,
                memory_used_mib: object.need("mu")?.u32()?,
                memory_total_mib: object.need("mt")?.u32()?,
                disk_free_mib: object.need("df")?.u32()?,
                disk_total_mib: object.need("dt")?.u32()?,
                sample: object.need("seq")?.u32()?,
            };
            if !status.valid() { return Err(ErrWhy::Range); }
            *out = HostMsg::System(status);
        }
        b"usage" => {
            *out = USAGE;
            let HostMsg::Usage {
                period,
                items,
                count,
            } = out
            else {
                return Err(ErrWhy::Parse);
            };
            object.need("period")?.text_into(period)?;
            for item in object.need("items")?.array()? {
                let slot = items.get_mut(*count as usize).ok_or(ErrWhy::Range)?;
                let item = item?.object()?;
                item.need("n")?.text_into(&mut slot.name)?;
                if let Some(account) = item.get("a")? {
                    account.text_into(&mut slot.account)?;
                }
                slot.percent = item.need("p")?.u8()?;
                if slot.percent > 100 {
                    return Err(ErrWhy::Range);
                }
                item.need("r")?.text_into(&mut slot.reset)?;
                if let Some(tip) = item.get("tip")? {
                    tip.text_into(&mut slot.reset_tip)?;
                }
                slot.tone = item.get("c")?.map(|v| v.member(TONES)).transpose()?;
                *count += 1;
            }
        }
        b"faces" => {
            *out = FACES;
            let HostMsg::Faces { list, count } = out else {
                return Err(ErrWhy::Parse);
            };
            for item in object.need("list")?.array()? {
                let item = item?.object()?;
                let id = face_id(item.need("f")?)?;
                let taken = list[..(*count as usize).min(list.len())]
                    .iter()
                    .any(|entry| entry.id == id);
                let entry = list.get_mut(*count as usize).ok_or(ErrWhy::Range)?;
                if taken {
                    return Err(ErrWhy::Range);
                }
                entry.id = id;
                entry.template = item.need("t")?.member(TEMPLATES)?;
                item.need("n")?.text_into(&mut entry.name)?;
                *count += 1;
            }
        }
        b"name" => {
            *out = NAME;
            let HostMsg::Name { text } = out else {
                return Err(ErrWhy::Parse);
            };
            object.need("text")?.text_into(text)?;
        }
        b"set" => {
            let key = object.need("k")?.member(SETTINGS)?;
            let value = object.need("v")?.u16()?;
            *out = HostMsg::Set { key, value };
        }
        _ => return Err(ErrWhy::Unknown),
    }
    Ok(())
}

// ---- device -> host ------------------------------------------------------------

fn faces(json: &mut Writer, ui: &UiState) {
    json.raw(b"[");
    let len = (ui.global.strip_len as usize).min(ui.global.strip.len());
    for (index, entry) in ui.global.strip[..len].iter().enumerate() {
        if index > 0 {
            json.raw(b",");
        }
        json.raw(b"{");
        json.key("f").number(entry.id as u32);
        json.key("t").string(name_of(TEMPLATES, entry.template));
        json.key("n").string(entry.name.as_str());
        json.raw(b"}");
    }
    json.raw(b"]");
}

/// Encode one event as a JSON object without the LF; 0 when it did not fit.
/// Lists (`hello`, `faces`) are read from `ui`.
pub fn encode(event: Event, ui: &UiState, out: &mut [u8; TX_MAX]) -> usize {
    let mut json = Writer::new(out);
    json.raw(b"{");
    match event {
        Event::Hello => {
            json.key("m").string("hello");
            json.key("v").number(VERSION as u32);
            json.key("fw").string(FW_VERSION);
            // The session the device holds a COMPLETE state for: 0 until the
            // host's `synced`, so a host always knows whether to resync.
            let held = ui.deck.held_sid();
            json.key("sid").number(held);
            json.key("boot").number(ui.still_default() as u32);
            json.key("rx").number(LINE_MAX as u32);
            json.key("faces");
            faces(&mut json, ui);
        }
        Event::Up => {
            json.key("m").string("up");
        }
        Event::Faces => {
            json.key("m").string("faces");
            json.key("list");
            faces(&mut json, ui);
        }
        Event::Refresh => {
            json.key("m").string("refresh");
            json.key("what").string("usage");
        }
        Event::Key {
            key,
            act,
            shift,
            face,
        } => {
            json.key("m").string("key");
            json.key("k").string(key_name(key));
            json.key("act").number(act as u32);
            json.key("shift").number(shift as u32);
            json.key("f").number(face as u32);
        }
        Event::Channel { agent, seq } => {
            json.key("m").string("channel");
            json.key("agent").number(agent as u32);
            json.key("seq").number(seq as u32);
        }
        Event::Ptt {
            act,
            agent,
            ctx,
            card,
            abort,
            press,
        } => {
            json.key("m").string("ptt");
            json.key("act").number(act as u32);
            json.key("agent").number(agent as u32);
            json.key("ctx").string(ctx_name(ctx));
            if card != 0 {
                json.key("card").number(card as u32);
            }
            if abort {
                json.key("abort").number(1);
            }
            if press != 0 {
                json.key("p").number(press as u32);
            }
        }
        Event::Answer {
            agent,
            card,
            choice,
        } => {
            json.key("m").string("answer");
            json.key("agent").number(agent as u32);
            json.key("card").number(card as u32);
            json.key("choice").number(choice as u32);
        }
        Event::Snooze { agent, card } => {
            json.key("m").string("snooze");
            json.key("agent").number(agent as u32);
            json.key("card").number(card as u32);
        }
        Event::DocLines {
            agent,
            doc,
            id,
            first,
        } => {
            json.key("m").string("doclines");
            json.key("agent").number(agent as u32);
            json.key("d").string(doc_name(doc));
            json.key("id").number(id as u32);
            json.key("first").number(first as u32);
        }
        Event::Image { agent, rev, index, offset } => {
            json.key("m").string("image");
            json.key("agent").number(agent as u32);
            json.key("r").number(rev as u32);
            json.key("i").number(index as u32);
            json.key("o").number(offset as u32);
        }
        Event::Session { agent, field, page, rev, choice } => {
            json.key("m").string("session");
            json.key("agent").number(agent as u32);
            json.key("f").number(field as u32);
            json.key("p").number(page as u32);
            json.key("r").number(rev as u32);
            json.key("i").number(choice as u32);
        }
        Event::Tool { agent, id, index } => {
            json.key("m").string("tool");
            json.key("agent").number(agent as u32);
            json.key("id").number(id as u32);
            json.key("i").number(index as u32);
        }
        Event::Macro { face, index, act } => {
            json.key("m").string("macro");
            json.key("f").number(face as u32);
            json.key("i").number(index as u32);
            json.key("act").number(act as u32);
        }
        Event::MacroKey { face, index } => {
            json.key("m").string("macrokey");
            json.key("f").number(face as u32);
            json.key("i").number(index as u32);
            let key = ui
                .deck
                .macro_set(face)
                .and_then(|set| set.keys.get(index as usize))
                .filter(|key| key.present);
            if let Some(key) = key {
                json.key("l").string(key.label.as_str());
                if key.binding != Binding::None {
                    json.key("b").string(name_of(BINDINGS, key.binding));
                }
                if key.keys.is_some() {
                    let mut combo = super::text::Text::<32>::new();
                    key.keys.wire(&mut combo);
                    json.key("k").string(combo.as_str());
                }
            }
        }
        Event::WsGo(workspace) => {
            json.key("m").string("ws");
            json.key("go").number(workspace as u32);
        }
        Event::WsWin(row) => {
            json.key("m").string("ws");
            json.key("win").number(row as u32);
        }
        Event::Set { key, value } => {
            json.key("m").string("set");
            json.key("k").string(name_of(SETTINGS, key));
            json.key("v").number(value as u32);
        }
        Event::Face { id } => {
            json.key("m").string("face");
            json.key("f").number(id as u32);
        }
        Event::Err { why } => {
            json.key("m").string("err");
            json.key("why").string(why_name(why));
        }
    }
    json.raw(b"}");
    json.finish()
}

/// Fill `report` with the next fragment of `json` + LF starting at `offset`;
/// returns the bytes consumed, 0 when the message is complete.
pub fn frame(json: &[u8], offset: usize, report: &mut [u8; REPORT_LEN]) -> usize {
    let total = json.len() + 1;
    if offset >= total {
        return 0;
    }
    let chunk = (total - offset).min(PAYLOAD_MAX);
    report.fill(0);
    report[0] = REPORT_ID;
    report[1] = CHANNEL;
    report[2] = chunk as u8;
    for k in 0..chunk {
        report[3 + k] = json.get(offset + k).copied().unwrap_or(b'\n');
    }
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    fn parsed(line: &str) -> Result<Box<HostMsg>, ErrWhy> {
        let mut out = Box::new(HostMsg::None);
        parse(line.as_bytes(), &mut out)?;
        Ok(out)
    }

    fn ui() -> Box<UiState> {
        let mut ui = Box::new(UiState::new());
        ui.reset();
        ui
    }

    fn encoded(event: Event, ui: &UiState) -> std::string::String {
        let mut out = [0; TX_MAX];
        let n = encode(event, ui, &mut out);
        assert!(n > 0, "{event:?} did not fit");
        std::string::String::from_utf8(out[..n].to_vec()).unwrap()
    }

    #[test]
    fn framed_lines_reassemble_across_reports_and_overlong_lines_are_dropped() {
        let mut assembler = LineAssembler::new();
        let mut lines = Vec::new();
        let json = [b'x'; 150];
        let mut offset = 0;
        let mut report = [0; REPORT_LEN];
        loop {
            let n = frame(&json, offset, &mut report);
            if n == 0 {
                break;
            }
            offset += n;
            // Bodies arrive with or without the report id.
            let body = if offset % 2 == 0 { &report[..] } else { &report[1..] };
            assembler.push(body, &mut |line| lines.push(line.map(|l| l.to_vec())));
        }
        assert_eq!(lines, [Ok(json.to_vec())]);
        lines.clear();
        let mut body = [b'y'; 63];
        body[0] = CHANNEL;
        body[1] = 61;
        for _ in 0..20 {
            assembler.push(&body, &mut |line| lines.push(line.map(|l| l.to_vec())));
        }
        assembler.push(&[CHANNEL, 3, b'\n', b'z', b'\n'], &mut |line| {
            lines.push(line.map(|l| l.to_vec()))
        });
        assembler.push(&[2, 2, b'q', b'\n'], &mut |_| panic!("other channel"));
        assembler.push(&[REPORT_ID, 2, 2, b'q', b'\n'], &mut |_| panic!("other channel"));
        assert_eq!(lines, [Err(ErrWhy::Size), Ok(b"z".to_vec())]);
    }

    #[test]
    fn the_longest_line_is_1024_bytes_with_its_lf() {
        for (len, expect) in [(1023, true), (1024, false)] {
            let mut assembler = LineAssembler::new();
            let mut got = Vec::new();
            let line = std::vec![b'a'; len];
            let mut offset = 0;
            let mut report = [0; REPORT_LEN];
            while {
                let n = frame(&line, offset, &mut report);
                offset += n;
                n > 0
            } {
                assembler.push(&report, &mut |l| got.push(l.map(|l| l.len())));
            }
            assert_eq!(got, [if expect { Ok(len) } else { Err(ErrWhy::Size) }]);
        }
    }

    /// The `doc` window is the widest line the host sends, and the caps behind
    /// it (`doc_line` 48, `doc_lines` 12) were chosen against the 1000 B
    /// ceiling. The arithmetic, with every field at its widest:
    /// 10 `{"m":"doc"` + 10 `,"agent":8` + 8 `,"d":"c"` + 11 `,"id":65535`
    /// + 14 `,"first":65535` + 14 `,"total":65535` + 15 `,"font":"title"`
    /// + 10 `,"lines":[` + 17 x 50 quoted lines + 16 commas + 1 `]` + 1 `}`
    /// + 1 LF = 961 B. **17 lines is the ceiling**: an 18th costs 51 more and
    /// would be 1012, past `CAPS.line`. The 39 B left over is what JSON
    /// escaping may add:
    /// `"` and `\` cost one byte each (the host's sanitiser has already taken
    /// the control characters out), so a window is safe up to 294 of them.
    #[test]
    fn the_widest_window_of_wrapped_lines_is_well_inside_the_line_ceiling() {
        let line = std::format!(
            r#"{{"m":"doc","agent":8,"d":"c","id":65535,"first":65535,"total":65535,"font":"title","lines":[{}]}}"#,
            (0..DOC_LINES)
                .map(|_| std::format!("\"{}\"", "w".repeat(48)))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(line.len() + 1, 961);
        assert!(line.len() + 1 <= LINE_MAX);
        // `first` + the lines may not run past `total`, so the widest line
        // that parses says so; the length is what this test is about.
        assert_eq!(parse(line.as_bytes(), &mut Box::new(HostMsg::None)), Err(ErrWhy::Range));
        let ok = line.replace(r#""first":65535"#, r#""first":0"#);
        let mut out = Box::new(HostMsg::None);
        parse(ok.as_bytes(), &mut out).unwrap();
        let HostMsg::DocText { lines, line_count, .. } = *out else { panic!("{out:?}") };
        assert_eq!(line_count as usize, DOC_LINES);
        assert_eq!(lines[DOC_LINES - 1].len(), 48);
    }

    #[test]
    fn every_message_parses_into_its_variant() {
        assert_eq!(
            *parsed(r#"{"m":"hello","v":1,"sid":77,"hm":"19:42"}"#).unwrap(),
            HostMsg::Hello {
                v: 1,
                sid: 77,
                minute_of_day: Some(19 * 60 + 42)
            }
        );
        assert_eq!(
            *parsed(r#"{"m":"hello","v":1,"sid":0}"#).unwrap(),
            HostMsg::Hello {
                v: 1,
                sid: 0,
                minute_of_day: None
            }
        );
        assert_eq!(*parsed(r#"{"m":"synced"}"#).unwrap(), HostMsg::Synced);
        match *parsed(
            r#"{"m":"agent","id":3,"kind":"codex","state":"question","name":"cohdl-api","proj":"cohdl","title":"Run migrations?","since":120}"#,
        )
        .unwrap()
        {
            HostMsg::Agent {
                id: 3,
                gone: false,
                kind: Some(AgentKind::Codex),
                state: Some(AgentState::Question),
                name: Some(name),
                project: Some(project),
                title: Some(title),
                since_s: Some(120),
            } => {
                assert_eq!(name, "cohdl-api");
                assert_eq!(project, "cohdl");
                assert_eq!(title, "Run migrations?");
            }
            other => panic!("{other:?}"),
        }
        // Absent fields stay `None` (the model keeps its values).
        assert_eq!(
            *parsed(r#"{"m":"agent","id":8,"title":"x"}"#).unwrap(),
            HostMsg::Agent {
                id: 8,
                gone: false,
                kind: None,
                state: None,
                name: None,
                project: None,
                title: Some(Text::from_str("x")),
                since_s: None
            }
        );
        assert!(matches!(
            *parsed(r#"{"m":"agent","id":2,"gone":1}"#).unwrap(),
            HostMsg::Agent { id: 2, gone: true, .. }
        ));
        match *parsed(
            r#"{"m":"card","agent":1,"id":9,"type":"q","text":"Run?","choices":["Yes","No"],"rec":1}"#,
        )
        .unwrap()
        {
            HostMsg::Card {
                agent: 1,
                id: 9,
                clear: false,
                kind: CardKind::Question,
                text,
                choices,
                choice_count: 2,
                recommended: Some(1),
            } => {
                assert_eq!(text, "Run?");
                assert_eq!(choices[0], "Yes");
                assert_eq!(choices[1], "No");
                assert!(choices[2].is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            *parsed(r#"{"m":"card","agent":1,"id":9,"type":"r","text":"Done"}"#).unwrap(),
            HostMsg::Card { kind: CardKind::Ready, choice_count: 0, recommended: None, .. }
        ));
        assert!(matches!(
            *parsed(r#"{"m":"card","agent":2,"id":5,"clear":1}"#).unwrap(),
            HostMsg::Card { agent: 2, id: 5, clear: true, .. }
        ));
        match *parsed(
            r#"{"m":"doc","agent":1,"d":"c","id":9,"first":3,"total":40,"font":"name","lines":["Tests fail on main because the","staging database is three"]}"#,
        )
        .unwrap()
        {
            HostMsg::DocText {
                agent: 1,
                doc: DocKind::Card,
                id: 9,
                first: 3,
                total: 40,
                font: DocFont::Name,
                lines,
                line_count: 2,
            } => {
                assert_eq!(lines[0], "Tests fail on main because the");
                assert_eq!(lines[1], "staging database is three");
                assert!(lines[2].is_empty());
            }
            other => panic!("{other:?}"),
        }
        // An empty body is a window of no lines, not an absent one.
        assert!(matches!(
            *parsed(r#"{"m":"doc","agent":8,"d":"c","id":0,"first":0,"total":0,"font":"title","lines":[]}"#).unwrap(),
            HostMsg::DocText { agent: 8, total: 0, line_count: 0, font: DocFont::Title, .. }
        ));
        assert!(matches!(
            *parsed(r#"{"m":"reply","agent":4,"text":"ok","rev":1}"#).unwrap(),
            HostMsg::Reply { agent: 4, text, .. } if text == "ok"
        ));
        assert!(matches!(
            *parsed(r#"{"m":"transcript","agent":4,"text":"hi","final":true}"#).unwrap(),
            HostMsg::Transcript { agent: 4, text, last: true, press: None, failed: false, channel_tip: false, } if text == "hi"
        ));
        assert!(matches!(
            *parsed(r#"{"m":"transcript","agent":4,"text":"hi"}"#).unwrap(),
            HostMsg::Transcript { last: false, .. }
        ));
        // Nobody's slot: a name or a key label being dictated.
        assert!(matches!(
            *parsed(r#"{"m":"transcript","text":"deploy"}"#).unwrap(),
            HostMsg::Transcript { agent: 0, text, last: false, press: None, failed: false, channel_tip: false, } if text == "deploy"
        ));
        assert_eq!(parsed(r#"{"m":"transcript","agent":0,"text":"x"}"#).unwrap_err(), ErrWhy::Range);
        assert_eq!(
            *parsed(r#"{"m":"focus","agent":2,"ack":7}"#).unwrap(),
            HostMsg::Focus { agent: 2, ack: Some(7) }
        );
        assert_eq!(
            *parsed(r#"{"m":"focus","agent":2}"#).unwrap(),
            HostMsg::Focus { agent: 2, ack: None }
        );
        match *parsed(
            r#"{"m":"ws","active":2,"count":4,"sel":1,"windows":[{"c":"blue","t":"term"},{"t":"firefox"}]}"#,
        )
        .unwrap()
        {
            HostMsg::Workspaces {
                active: 2,
                count: 4,
                selected: 1,
                windows,
                window_count: 2,
            } => {
                assert_eq!((windows[0].tone, windows[0].title.as_str()), (Tone::Blue, "term"));
                assert_eq!((windows[1].tone, windows[1].title.as_str()), (Tone::Gray, "firefox"));
            }
            other => panic!("{other:?}"),
        }
        match *parsed(
            r#"{"m":"macro","f":3,"src":"deck.toml","keys":[{"l":"build","b":"left"},{"l":"shot"}]}"#,
        )
        .unwrap()
        {
            HostMsg::Macro {
                face: 3,
                source: Some(source),
                keys,
            } => {
                assert_eq!(source, "deck.toml");
                assert_eq!((keys[0].label.as_str(), keys[0].binding, keys[0].present), ("build", Binding::Left, true));
                assert_eq!((keys[1].label.as_str(), keys[1].binding, keys[1].present), ("shot", Binding::None, true));
                assert!(!keys[2].present);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            *parsed(r#"{"m":"macro","f":3,"keys":[]}"#).unwrap(),
            HostMsg::Macro { face: 3, source: None, keys } if keys.iter().all(|k| !k.present)
        ));
        match *parsed(
            r#"{"m":"usage","period":"SEP 2026","items":[{"n":"Claude Pro","p":62,"r":"resets in 4 days"},{"n":"Codex","p":100,"r":"tomorrow","c":"red"}]}"#,
        )
        .unwrap()
        {
            HostMsg::Usage {
                period,
                items,
                count: 2,
            } => {
                assert_eq!(period, "SEP 2026");
                assert_eq!((items[0].name.as_str(), items[0].percent, items[0].reset.as_str(), items[0].tone), ("Claude Pro", 62, "resets in 4 days", None));
                assert_eq!((items[1].percent, items[1].tone), (100, Some(Tone::Red)));
            }
            other => panic!("{other:?}"),
        }
        match *parsed(r#"{"m":"faces","list":[{"f":1,"t":"agents","n":"Agents"},{"f":6,"t":"settings","n":"Settings"}]}"#)
            .unwrap()
        {
            HostMsg::Faces { list, count: 2 } => {
                assert_eq!((list[0].id, list[0].template, list[0].name.as_str()), (1, Template::Agents, "Agents"));
                assert_eq!((list[1].id, list[1].template), (6, Template::Settings));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            *parsed(r#"{"m":"name","text":"Deploy"}"#).unwrap(),
            HostMsg::Name { text } if text == "Deploy"
        ));
        assert_eq!(
            *parsed(r#"{"m":"set","k":"brightness","v":3}"#).unwrap(),
            HostMsg::Set {
                key: SettingKey::Brightness,
                value: 3
            }
        );
        // Whitespace, member order and unknown fields do not matter.
        assert_eq!(
            *parsed(" { \"sid\" : 5 , \"extra\" : [1,{\"m\":\"x\"}] , \"m\" : \"hello\" , \"v\" : 1 } ").unwrap(),
            HostMsg::Hello {
                v: 1,
                sid: 5,
                minute_of_day: None
            }
        );
    }

    #[test]
    fn errors_name_their_cause_and_leave_nothing_behind() {
        let mut out = Box::new(HostMsg::Synced);
        for (line, why) in [
            ("", ErrWhy::Parse),
            ("{", ErrWhy::Parse),
            ("hello", ErrWhy::Parse),
            (r#"{"v":1}"#, ErrWhy::Parse),
            (r#"{"m":1}"#, ErrWhy::Parse),
            (r#"{"m":"hello","v":"1","sid":1}"#, ErrWhy::Parse),
            (r#"{"m":"hello","v":1}"#, ErrWhy::Parse),
            (r#"{"m":"hello","v":1,"sid":1,"hm":"1942"}"#, ErrWhy::Parse),
            (r#"{"m":"hello","v":1,"sid":1,"hm":"25:00"}"#, ErrWhy::Range),
            (r#"{"m":"hello","v":1,"sid":1,"hm":"19:60"}"#, ErrWhy::Range),
            (r#"{"m":"hello","v":1,"sid":-1}"#, ErrWhy::Range),
            (r#"{"m":"hello","v":256,"sid":1}"#, ErrWhy::Range),
            (r#"{"m":"bogus"}"#, ErrWhy::Unknown),
            (r#"{"m":"agent","id":0}"#, ErrWhy::Range),
            (r#"{"m":"agent","id":9}"#, ErrWhy::Range),
            (r#"{"m":"agent","id":1,"kind":"cursor"}"#, ErrWhy::Unknown),
            (r#"{"m":"agent","id":1,"state":"busy"}"#, ErrWhy::Unknown),
            (r#"{"m":"agent","id":1,"name":5}"#, ErrWhy::Parse),
            (r#"{"m":"agent","id":1,"name":"a\x"}"#, ErrWhy::Parse),
            (r#"{"m":"agent","id":1,"gone":"yes"}"#, ErrWhy::Parse),
            (r#"{"m":"card","agent":1,"id":1,"type":"x","text":""}"#, ErrWhy::Unknown),
            (r#"{"m":"card","agent":1,"id":1,"type":"q"}"#, ErrWhy::Parse),
            (r#"{"m":"card","agent":1,"id":70000,"type":"q","text":""}"#, ErrWhy::Range),
            (r#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","choices":["a","b","c","d","e"]}"#, ErrWhy::Range),
            (r#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","choices":["a"],"rec":1}"#, ErrWhy::Range),
            (r#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","rec":0}"#, ErrWhy::Range),
            (r#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","choices":"a"}"#, ErrWhy::Parse),
            (r#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","choices":[1]}"#, ErrWhy::Parse),
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":0,"total":1,"font":"bold","lines":["a"]}"#, ErrWhy::Unknown),
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":0,"total":1,"lines":["a"]}"#, ErrWhy::Parse),
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":0,"total":1,"font":"title"}"#, ErrWhy::Parse),
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":0,"total":1,"font":"title","lines":"a"}"#, ErrWhy::Parse),
            // A window that runs past the end of its own text, and one of
            // more lines than the deck holds.
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":2,"total":3,"font":"title","lines":["a","b"]}"#, ErrWhy::Range),
            (r#"{"m":"doc","agent":1,"d":"c","id":1,"first":0,"total":99,"font":"title","lines":["a","b","c","d","e","f","g","h","i","j","k","l","m","n","o","p","q","r"]}"#, ErrWhy::Range),
            (r#"{"m":"reply","agent":9,"text":"x","rev":1}"#, ErrWhy::Range),
            (r#"{"m":"reply","agent":1}"#, ErrWhy::Parse),
            (r#"{"m":"focus","agent":1,"ack":65536}"#, ErrWhy::Range),
            (r#"{"m":"ws","active":5,"count":4,"sel":0}"#, ErrWhy::Range),
            (r#"{"m":"ws","active":1,"count":10,"sel":0}"#, ErrWhy::Range),
            (r#"{"m":"ws","active":1,"count":2,"sel":1}"#, ErrWhy::Range),
            (r#"{"m":"ws","active":1,"count":2,"sel":1,"windows":[{"t":"a"}]}"#, ErrWhy::Range),
            (r#"{"m":"ws","active":1,"count":2,"sel":0,"windows":[{"t":"a","c":"pink"}]}"#, ErrWhy::Unknown),
            (r#"{"m":"ws","active":1,"count":2,"sel":0,"windows":[{"c":"red"}]}"#, ErrWhy::Parse),
            (r#"{"m":"ws","active":1,"count":2,"sel":0,"windows":[{"t":"a"},{"t":"a"},{"t":"a"},{"t":"a"},{"t":"a"},{"t":"a"},{"t":"a"}]}"#, ErrWhy::Range),
            (r#"{"m":"macro","f":0,"keys":[]}"#, ErrWhy::Range),
            (r#"{"m":"macro","f":9,"keys":[]}"#, ErrWhy::Range),
            (r#"{"m":"macro","f":1}"#, ErrWhy::Parse),
            (r#"{"m":"macro","f":1,"keys":[{"l":"a","b":"ptt"}]}"#, ErrWhy::Unknown),
            (r#"{"m":"macro","f":1,"keys":[{"b":"left"}]}"#, ErrWhy::Parse),
            (r#"{"m":"macro","f":1,"keys":[{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"},{"l":"a"}]}"#, ErrWhy::Range),
            (r#"{"m":"usage","period":"p","items":[{"n":"a","p":101,"r":"r"}]}"#, ErrWhy::Range),
            (r#"{"m":"usage","period":"p","items":[{"n":"a","r":"r"}]}"#, ErrWhy::Parse),
            (r#"{"m":"usage","items":[]}"#, ErrWhy::Parse),
            (r#"{"m":"faces","list":[{"f":1,"t":"agents","n":"A"},{"f":1,"t":"usage","n":"U"}]}"#, ErrWhy::Range),
            (r#"{"m":"faces","list":[{"f":9,"t":"agents","n":"A"}]}"#, ErrWhy::Range),
            (r#"{"m":"faces","list":[{"f":1,"t":"clock","n":"A"}]}"#, ErrWhy::Unknown),
            (r#"{"m":"faces","list":[{"f":1,"t":"agents"}]}"#, ErrWhy::Parse),
            (r#"{"m":"faces","list":[{"f":1,"t":"agents","n":"1"},{"f":2,"t":"agents","n":"2"},{"f":3,"t":"agents","n":"3"},{"f":4,"t":"agents","n":"4"},{"f":5,"t":"agents","n":"5"},{"f":6,"t":"agents","n":"6"},{"f":7,"t":"agents","n":"7"},{"f":8,"t":"agents","n":"8"},{"f":8,"t":"agents","n":"9"}]}"#, ErrWhy::Range),
            (r#"{"m":"name"}"#, ErrWhy::Parse),
            (r#"{"m":"set","k":"volume","v":1}"#, ErrWhy::Unknown),
            (r#"{"m":"set","k":"speaker","v":65536}"#, ErrWhy::Range),
            (r#"{"m":"set","k":"speaker"}"#, ErrWhy::Parse),
            (r#"{"m":"synced"} {"m":"synced"}"#, ErrWhy::Parse),
        ] {
            assert_eq!(parse(line.as_bytes(), &mut out), Err(why), "{line}");
            assert_eq!(*out, HostMsg::None, "{line}");
        }
    }

    #[test]
    fn text_fields_are_cut_to_capacity_on_a_character_boundary() {
        let long = "\u{e9}".repeat(100);
        let mut out = Box::new(HostMsg::None);
        parse(std::format!(r#"{{"m":"name","text":"{long}"}}"#).as_bytes(), &mut out).unwrap();
        let HostMsg::Name { text } = *out else { panic!() };
        assert_eq!(text.len(), 16);
        assert_eq!(text.as_str(), "\u{e9}".repeat(8));
        parse(std::format!(r#"{{"m":"reply","agent":1,"text":"{long}{long}{long}","rev":1}}"#).as_bytes(), &mut out).unwrap();
        let HostMsg::Reply { text, .. } = *out else { panic!() };
        assert_eq!(text.len(), 200);
        // Raw control characters and invalid UTF-8 are laundered, not refused.
        parse(b"{\"m\":\"name\",\"text\":\"a\tb\x80c\"}", &mut out).unwrap();
        let HostMsg::Name { text } = *out else { panic!() };
        assert_eq!(text.as_str(), "a b\u{FFFD}c");
    }

    #[test]
    fn every_event_encodes_and_the_per_edge_ones_fit_one_report() {
        let ui = ui();
        let cases: [(Event, &str); 18] = [
            (Event::Up, r#"{"m":"up"}"#),
            (Event::Refresh, r#"{"m":"refresh","what":"usage"}"#),
            (
                Event::Key { key: KeyId::Cancel, act: 1, shift: true, face: 8 },
                r#"{"m":"key","k":"cancel","act":1,"shift":1,"f":8}"#,
            ),
            (
                Event::Key { key: KeyId::Cw, act: 2, shift: false, face: 2 },
                r#"{"m":"key","k":"cw","act":2,"shift":0,"f":2}"#,
            ),
            (Event::Channel { agent: 8, seq: 65535 }, r#"{"m":"channel","agent":8,"seq":65535}"#),
            (
                Event::Ptt { act: 1, agent: 3, ctx: PttCtx::Talk, card: 0, abort: false, press: 0, },
                r#"{"m":"ptt","act":1,"agent":3,"ctx":"talk"}"#,
            ),
            (
                Event::Ptt { act: 0, agent: 8, ctx: PttCtx::Answer, card: 65535, abort: false, press: 0, },
                r#"{"m":"ptt","act":0,"agent":8,"ctx":"answer","card":65535}"#,
            ),
            (
                Event::Ptt { act: 0, agent: 8, ctx: PttCtx::Answer, card: 65535, abort: true, press: 0, },
                r#"{"m":"ptt","act":0,"agent":8,"ctx":"answer","card":65535,"abort":1}"#,
            ),
            (
                Event::Answer { agent: 8, card: 65535, choice: 3 },
                r#"{"m":"answer","agent":8,"card":65535,"choice":3}"#,
            ),
            (Event::Snooze { agent: 8, card: 65535 }, r#"{"m":"snooze","agent":8,"card":65535}"#),
            (Event::Tool { agent: 8, id: 65535, index: 63 }, r#"{"m":"tool","agent":8,"id":65535,"i":63}"#),
            (
                Event::DocLines { agent: 8, doc: DocKind::Card, id: 65535, first: 65535 },
                r#"{"m":"doclines","agent":8,"d":"c","id":65535,"first":65535}"#,
            ),
            (Event::Macro { face: 8, index: 8, act: 2 }, r#"{"m":"macro","f":8,"i":8,"act":2}"#),
            (Event::WsGo(9), r#"{"m":"ws","go":9}"#),
            (Event::WsWin(5), r#"{"m":"ws","win":5}"#),
            (
                Event::Set { key: SettingKey::Brightness, value: 65535 },
                r#"{"m":"set","k":"brightness","v":65535}"#,
            ),
            (Event::Face { id: 8 }, r#"{"m":"face","f":8}"#),
            (Event::Err { why: ErrWhy::Unknown }, r#"{"m":"err","why":"unknown"}"#),
        ];
        for (event, expected) in cases {
            let json = encoded(event, &ui);
            assert_eq!(json, expected);
            let per_edge = matches!(
                event,
                Event::Key { .. }
                    | Event::Macro { .. }
                    | Event::Answer { .. }
                    | Event::Channel { .. }
                    | Event::Snooze { .. }
                    | Event::DocLines { .. }
                    | Event::Tool { .. }
                    | Event::Face { .. }
                    | Event::Ptt { abort: false, .. }
            );
            if per_edge {
                assert!(json.len() <= EDGE_MAX, "{json} is {} bytes", json.len());
            }
            // The abort release is the one edge that may need a second report.
            assert!(json.len() <= 2 * EDGE_MAX, "{json}");
        }
        for key in [SettingKey::Speaker, SettingKey::Mic, SettingKey::Announce, SettingKey::Chime, SettingKey::AutoJump, SettingKey::IdleHome, SettingKey::Demo] {
            let json = encoded(Event::Set { key, value: 1 }, &ui);
            assert_eq!(json, std::format!(r#"{{"m":"set","k":"{}","v":1}}"#, name_of(SETTINGS, key)));
            let mut out = Box::new(HostMsg::None);
            parse(json.as_bytes(), &mut out).unwrap();
            assert_eq!(*out, HostMsg::Set { key, value: 1 });
        }
        for why in [ErrWhy::Size, ErrWhy::Parse, ErrWhy::Range] {
            assert!(encoded(Event::Err { why }, &ui).contains(why_name(why)));
        }
    }

    #[test]
    fn hello_and_faces_carry_the_strip() {
        let mut ui = ui();
        let strip = r#"[{"f":1,"t":"agents","n":"Agents"},{"f":2,"t":"omarchy","n":"Omarchy"},{"f":3,"t":"macropad","n":"Macropad"},{"f":4,"t":"usage","n":"Usage"},{"f":7,"t":"system","n":"System"},{"f":6,"t":"arcade","n":"Arcade"},{"f":5,"t":"settings","n":"Settings"}]"#;
        assert_eq!(
            encoded(Event::Hello, &ui),
            std::format!(r#"{{"m":"hello","v":1,"fw":"{FW_VERSION}","sid":0,"boot":1,"rx":1024,"faces":{strip}}}"#)
        );
        assert_eq!(encoded(Event::Faces, &ui), std::format!(r#"{{"m":"faces","list":{strip}}}"#));
        // A held session is reported once the sync is complete; a renamed face ends `boot`.
        ui.host(&HostMsg::Hello { v: 1, sid: 4242, minute_of_day: None }, 0, &mut crate::ui::Update::new());
        assert!(encoded(Event::Hello, &ui).contains(r#""sid":0,"boot":1"#));
        ui.host(&HostMsg::Synced, 1, &mut crate::ui::Update::new());
        ui.global.strip[1].name.set("Desk");
        assert!(encoded(Event::Hello, &ui).contains(r#""sid":4242,"boot":0"#));
        assert!(encoded(Event::Faces, &ui).contains(r#"{"f":2,"t":"omarchy","n":"Desk"}"#));
        // The device's own list parses back as a host `faces` restore.
        let json = encoded(Event::Faces, &ui);
        let mut out = Box::new(HostMsg::None);
        parse(json.as_bytes(), &mut out).unwrap();
        let HostMsg::Faces { list, count: 7 } = *out else { panic!("{out:?}") };
        assert_eq!(list[1].name, "Desk");
        assert_eq!(list[6].template, Template::Settings);
        ui.global.strip_len = 0;
        assert_eq!(encoded(Event::Faces, &ui), r#"{"m":"faces","list":[]}"#);
    }

    #[test]
    fn the_longest_hello_fits_the_tx_buffer() {
        let mut ui = ui();
        ui.host(&HostMsg::Hello { v: 1, sid: u32::MAX, minute_of_day: None }, 0, &mut crate::ui::Update::new());
        ui.host(&HostMsg::Synced, 1, &mut crate::ui::Update::new());
        for (index, entry) in ui.global.strip.iter_mut().enumerate() {
            entry.id = index as u8 + 1;
            entry.template = Template::Macropad;
            entry.name.set("\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\""); // 16 quotes: 32 bytes escaped
        }
        ui.global.strip_len = 8;
        let json = encoded(Event::Hello, &ui);
        assert!(json.len() <= TX_MAX);
        assert_eq!(json.matches("\\\"").count(), 128);
        // The reply is a superset of the host's hello: a host parser can share code.
        let mut out = Box::new(HostMsg::None);
        parse(json.as_bytes(), &mut out).unwrap();
        assert!(matches!(*out, HostMsg::Hello { v: VERSION, sid: u32::MAX, minute_of_day: None }));
    }

    #[test]
    fn a_face_name_with_quotes_backslash_newline_and_emoji_round_trips() {
        let mut ui = ui();
        let name = "a\"b\\c\nd\u{1F600}";
        ui.global.strip[2].name.set(name);
        let json = encoded(Event::Faces, &ui);
        assert!(json.contains(r#""n":"a\"b\\c\u000ad"#));
        assert!(!json.contains('\n'));
        let mut out = Box::new(HostMsg::None);
        parse(json.as_bytes(), &mut out).unwrap();
        let HostMsg::Faces { list, .. } = *out else { panic!() };
        // The newline came back as a space: control characters never enter a Text.
        assert_eq!(list[2].name, "a\"b\\c d\u{1F600}");
        let mut out2 = Box::new(HostMsg::None);
        parse(br#"{"m":"name","text":"\ud83d\ude00 \"q\" \\ \n"}"#, &mut out2).unwrap();
        let HostMsg::Name { text } = *out2 else { panic!() };
        assert_eq!(text, "\u{1F600} \"q\" \\  ");
    }

    #[test]
    fn frames_split_a_line_into_channel_5_reports_ending_in_lf() {
        let json = [b'j'; PAYLOAD_MAX];
        let mut report = [0xAA; REPORT_LEN];
        assert_eq!(frame(&json, 0, &mut report), PAYLOAD_MAX);
        assert_eq!(&report[..3], &[REPORT_ID, CHANNEL, PAYLOAD_MAX as u8]);
        assert_eq!(&report[3..], &json[..]);
        assert_eq!(frame(&json, PAYLOAD_MAX, &mut report), 1);
        assert_eq!(&report[..4], &[REPORT_ID, CHANNEL, 1, b'\n']);
        assert!(report[4..].iter().all(|&b| b == 0));
        assert_eq!(frame(&json, PAYLOAD_MAX + 1, &mut report), 0);
        assert_eq!(frame(b"", 0, &mut report), 1);
        assert_eq!(&report[..4], &[REPORT_ID, CHANNEL, 1, b'\n']);
    }

    #[test]
    fn a_macrokey_event_carries_the_slot_as_the_set_holds_it_and_an_empty_label_is_a_hole() {
        const MACRO_FACE: u8 = 3;
        let mut ui = Box::new(UiState::new());
        ui.reset();
        let set = ui.deck.macro_set_mut(MACRO_FACE).unwrap();
        set.keys[0] = MacroKey { label: Text::from_str("build"), binding: Binding::Left, keys: Keystroke::NONE, present: true };
        set.keys[1] = MacroKey {
            label: Text::from_str("shot"),
            binding: Binding::None,
            keys: Keystroke::parse("cmd+shift+4").unwrap(),
            present: true,
        };
        set.keys[8] = MacroKey::EMPTY;
        assert_eq!(
            encoded(Event::MacroKey { face: MACRO_FACE, index: 0 }, &ui),
            r#"{"m":"macrokey","f":3,"i":0,"l":"build","b":"left"}"#
        );
        assert_eq!(
            encoded(Event::MacroKey { face: MACRO_FACE, index: 1 }, &ui),
            r#"{"m":"macrokey","f":3,"i":1,"l":"shot","k":"shift+cmd+4"}"#
        );
        assert_eq!(encoded(Event::MacroKey { face: MACRO_FACE, index: 8 }, &ui), r#"{"m":"macrokey","f":3,"i":8}"#);
        // The longest one (a 12-byte label, SHIFT+ENTER) may need two reports.
        let set = ui.deck.macro_set_mut(MACRO_FACE).unwrap();
        set.keys[8] = MacroKey {
            label: Text::from_str("abcdefghijkl"),
            binding: Binding::ShiftEnter,
            keys: Keystroke::parse("ctrl+opt+shift+cmd+pagedown").unwrap(),
            present: true,
        };
        assert!(encoded(Event::MacroKey { face: MACRO_FACE, index: 8 }, &ui).len() <= 2 * EDGE_MAX);
        // A host `macro` with an empty label leaves that slot empty and the
        // later keys where they are; `k` is the keystroke, refused when unknown.
        let msg = parsed(r#"{"m":"macro","f":3,"keys":[{"l":""},{"l":"b","b":"left","k":"cmd+c"}]}"#).unwrap();
        let HostMsg::Macro { keys, .. } = *msg else { panic!() };
        assert!(!keys[0].present && keys[1].present && keys[1].binding == Binding::Left);
        assert_eq!(keys[1].keys, Keystroke::parse("cmd+c").unwrap());
        assert_eq!(parsed(r#"{"m":"macro","f":3,"keys":[{"l":"b","k":"cmd+ptt"}]}"#).err(), Some(ErrWhy::Unknown));
    }
}

fn decode_image_base64(input: &[u8], out: &mut [u8;384]) -> Result<usize, ErrWhy> {
    if input.len() % 4 != 0 { return Err(ErrWhy::Parse); }
    let digit = |c: u8| match c { b'A'..=b'Z' => Ok(c-b'A'), b'a'..=b'z' => Ok(c-b'a'+26), b'0'..=b'9' => Ok(c-b'0'+52), b'+' => Ok(62), b'/' => Ok(63), _ => Err(ErrWhy::Parse) };
    let mut n = 0;
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let pad = if chunk[2] == b'=' { if chunk[3] != b'=' { return Err(ErrWhy::Parse); } 2 } else if chunk[3] == b'=' { 1 } else { 0 };
        if pad > 0 && (i+1)*4 != input.len() { return Err(ErrWhy::Parse); }
        let a = digit(chunk[0])?; let b = digit(chunk[1])?;
        let c = if pad == 2 {0} else {digit(chunk[2])?};
        let d = if pad > 0 {0} else {digit(chunk[3])?};
        for byte in [a<<2 | b>>4, b<<4 | c>>2, c<<6 | d].into_iter().take(3-pad) {
            *out.get_mut(n).ok_or(ErrWhy::Range)? = byte; n += 1;
        }
    }
    Ok(n)
}
