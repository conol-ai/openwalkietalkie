//! Input reports → lines → `DeviceLine`. `LineAssembler` mirrors the
//! firmware's assembler (`src/deck/proto.rs`): LF-terminated, other channels
//! ignored, an overlong line reported once as `Size` and discarded to its LF.
use crate::types::{member_of, BINDINGS, CTXS, KEYS, KINDS, STATES, TEMPLATES, TONES, WHYS};
use crate::{
    CardKind, DeviceLine, FaceEntry, HostLine, MacroKey, SetKey, UsageItem, Window, CHANNEL,
    LINE_MAX, PAYLOAD_MAX, REPORT_ID,
};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// A line longer than `LINE_MAX`; discarded up to its LF.
    #[error("line exceeds {LINE_MAX} bytes")]
    Size,
    /// Not a valid message: bad JSON, a missing required field, a wrong type.
    #[error("parse error: {0}")]
    Parse(&'static str),
    /// A number outside its range.
    #[error("{0} out of range")]
    Range(&'static str),
    /// An `m` or enum member this host does not know (the value, cut short).
    #[error("unknown {0}")]
    Unknown(String),
}

/// Raw report bodies → complete lines (without their LF).
#[derive(Debug)]
pub struct LineAssembler {
    line: Vec<u8>,
    discarding: bool,
}

impl Default for LineAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl LineAssembler {
    pub fn new() -> Self {
        Self {
            line: Vec::with_capacity(LINE_MAX),
            discarding: false,
        }
    }

    /// Forget a partial line (link drop).
    pub fn clear(&mut self) {
        self.line.clear();
        self.discarding = false;
    }

    /// Feed one report, with or without its leading report id; other
    /// channels are ignored. `f` sees every completed non-empty line, or
    /// `Err(Size)` once per line that did not fit.
    #[allow(clippy::type_complexity)]
    pub fn push(&mut self, report: &[u8], f: &mut dyn FnMut(Result<&[u8], DecodeError>)) {
        let body = match report {
            [REPORT_ID, CHANNEL, ..] => &report[1..],
            _ => report,
        };
        let [CHANNEL, len, payload @ ..] = body else {
            return;
        };
        let payload = &payload[..usize::from(*len).min(PAYLOAD_MAX).min(payload.len())];
        for &byte in payload {
            if byte == b'\n' {
                if self.discarding {
                    f(Err(DecodeError::Size));
                } else if !self.line.is_empty() {
                    f(Ok(&self.line));
                }
                self.clear();
            } else if self.line.len() + 1 >= LINE_MAX {
                self.discarding = true;
            } else if !self.discarding {
                self.line.push(byte);
            }
        }
    }
}

/// Reports → parsed `DeviceLine`s.
#[derive(Debug, Default)]
pub struct Assembler {
    lines: LineAssembler,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Feed one 64-byte report (with or without the report id); `f` is
    /// called once per complete line with its parse result.
    pub fn push_report(
        &mut self,
        report: &[u8],
        f: &mut dyn FnMut(Result<DeviceLine, DecodeError>),
    ) {
        self.lines.push(report, &mut |line| match line {
            Ok(bytes) => f(parse_device_line(bytes)),
            Err(e) => f(Err(e)),
        });
    }
}

struct Obj<'a>(&'a Map<String, Value>);

impl Obj<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    fn str(&self, key: &'static str) -> Result<Option<&str>, DecodeError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(DecodeError::Parse(key)),
        }
    }

    fn need_str(&self, key: &'static str) -> Result<&str, DecodeError> {
        self.str(key)?.ok_or(DecodeError::Parse(key))
    }

    fn uint(&self, key: &'static str, max: u64) -> Result<Option<u64>, DecodeError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) => {
                let n = n.as_u64().ok_or(DecodeError::Range(key))?;
                if n > max {
                    Err(DecodeError::Range(key))
                } else {
                    Ok(Some(n))
                }
            }
            Some(_) => Err(DecodeError::Parse(key)),
        }
    }

    fn need_uint(&self, key: &'static str, max: u64) -> Result<u64, DecodeError> {
        self.uint(key, max)?.ok_or(DecodeError::Parse(key))
    }

    fn u8(&self, key: &'static str, max: u8) -> Result<Option<u8>, DecodeError> {
        Ok(self.uint(key, u64::from(max))?.map(|n| n as u8))
    }

    fn need_u8(&self, key: &'static str, max: u8) -> Result<u8, DecodeError> {
        self.u8(key, max)?.ok_or(DecodeError::Parse(key))
    }

    fn need_u32(&self, key: &'static str) -> Result<u32, DecodeError> {
        Ok(self.need_uint(key, u64::from(u32::MAX))? as u32)
    }

    fn need_u16(&self, key: &'static str) -> Result<u16, DecodeError> {
        Ok(self.need_uint(key, u64::from(u16::MAX))? as u16)
    }

    /// `0`/`1` or `false`/`true`; absent is `false`.
    fn flag(&self, key: &'static str) -> Result<bool, DecodeError> {
        match self.get(key) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(Value::Number(n)) => match n.as_u64() {
                Some(0) => Ok(false),
                Some(1) => Ok(true),
                _ => Err(DecodeError::Range(key)),
            },
            Some(_) => Err(DecodeError::Parse(key)),
        }
    }

    fn member<T: Copy>(
        &self,
        key: &'static str,
        table: &[(&'static str, T)],
    ) -> Result<Option<T>, DecodeError> {
        match self.str(key)? {
            None => Ok(None),
            Some(name) => member_of(table, name)
                .map(Some)
                .ok_or_else(|| DecodeError::Unknown(short(name))),
        }
    }

    fn need_member<T: Copy>(
        &self,
        key: &'static str,
        table: &[(&'static str, T)],
    ) -> Result<T, DecodeError> {
        self.member(key, table)?.ok_or(DecodeError::Parse(key))
    }

    fn array(&self, key: &'static str) -> Result<Option<&Vec<Value>>, DecodeError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Array(a)) => Ok(Some(a)),
            Some(_) => Err(DecodeError::Parse(key)),
        }
    }
}

fn short(name: &str) -> String {
    name.chars().take(32).collect()
}

fn object(bytes: &[u8]) -> Result<Map<String, Value>, DecodeError> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(DecodeError::Parse("json")),
    }
}

fn faces(list: &[Value]) -> Result<Vec<FaceEntry>, DecodeError> {
    if list.len() > 8 {
        return Err(DecodeError::Range("list"));
    }
    list.iter()
        .map(|entry| {
            let Value::Object(map) = entry else {
                return Err(DecodeError::Parse("list"));
            };
            let o = Obj(map);
            let f = o.need_u8("f", 8)?;
            if f == 0 {
                return Err(DecodeError::Range("f"));
            }
            Ok(FaceEntry {
                f,
                t: o.need_member("t", TEMPLATES)?,
                n: o.need_str("n")?.to_string(),
            })
        })
        .collect()
}

/// Parse one device line (without its LF) into a `DeviceLine`.
pub fn parse_device_line(bytes: &[u8]) -> Result<DeviceLine, DecodeError> {
    let map = object(bytes)?;
    let o = Obj(&map);
    let m = o.need_str("m")?;
    Ok(match m {
        "hello" => DeviceLine::Hello {
            v: o.need_u8("v", u8::MAX)?,
            fw: o.need_str("fw")?.to_string(),
            sid: o.need_uint("sid", u64::from(u32::MAX))? as u32,
            boot: o.flag("boot")?,
            rx: o.need_u16("rx")?,
            faces: faces(o.array("faces")?.map_or(&[][..], |a| a.as_slice()))?,
        },
        "key" => DeviceLine::Key {
            k: o.need_member("k", KEYS)?,
            act: o.need_u8("act", 2)?,
            shift: o.flag("shift")?,
            f: o.need_u8("f", 8)?,
        },
        "up" => DeviceLine::Up,
        "channel" => DeviceLine::Channel {
            slot: slot(o.need_u8("agent", 8)?)?,
            seq: o.need_u16("seq")?,
        },
        "ptt" => DeviceLine::Ptt {
            act: o.need_u8("act", 1)?,
            slot: o.need_u8("agent", 8)?,
            ctx: o.need_member("ctx", CTXS)?,
            card: o
                .uint("card", u64::from(u16::MAX))?
                .map(|n| n as u16)
                .filter(|&n| n != 0),
            abort: o.flag("abort")?,
            press: o.uint("p", u64::from(u16::MAX))?.map(|n| n as u16),
        },
        "answer" => DeviceLine::Answer {
            slot: slot(o.need_u8("agent", 8)?)?,
            card: o.need_u16("card")?,
            choice: o.need_u8("choice", 3)?,
        },
        "snooze" => DeviceLine::Snooze {
            slot: slot(o.need_u8("agent", 8)?)?,
            card: o.need_u16("card")?,
        },
        "doclines" => DeviceLine::DocLines {
            slot: slot(o.need_u8("agent", 8)?)?,
            doc: doc_kind(o.need_str("d")?)?,
            id: o.need_u16("id")?,
            first: o.need_u16("first")?,
        },
        "image" => DeviceLine::Image { slot: slot(o.need_u8("agent", 8)?)?, rev: o.need_u16("r")?, index: o.need_u8("i", 255)?, offset: o.need_u16("o")? },
        "session" => DeviceLine::Session {
            slot: slot(o.need_u8("agent", 8)?)?,
            field: o.need_u8("f", 2)?, page: o.need_u8("p", 63)?,
            rev: o.need_u16("r")?, choice: o.need_u8("i", 255)?,
        },
        "tool" => DeviceLine::Tool {
            slot: slot(o.need_u8("agent", 8)?)?,
            id: o.need_u16("id")?,
            index: o.need_u8("i", 65)?,
        },
        "macro" => DeviceLine::Macro {
            f: slot_field(o.need_u8("f", 8)?, "f")?,
            i: o.need_u8("i", 8)?,
            act: o.need_u8("act", 2)?,
        },
        "macrokey" => {
            let keys = o.str("k")?.map(str::to_string);
            if keys.as_deref().is_some_and(|k| !crate::keystroke_valid(k)) {
                return Err(DecodeError::Range("k"));
            }
            DeviceLine::MacroKey {
                f: slot_field(o.need_u8("f", 8)?, "f")?,
                i: o.need_u8("i", 8)?,
                label: o.str("l")?.map(str::to_string),
                binding: o.member("b", BINDINGS)?,
                keys,
            }
        }
        "ws" => match (o.u8("go", 9)?, o.u8("win", 5)?) {
            (Some(go), None) => DeviceLine::WsGo(go),
            (None, Some(win)) => DeviceLine::WsWin(win),
            _ => return Err(DecodeError::Parse("ws")),
        },
        "refresh" => DeviceLine::Refresh {
            what: o.need_str("what")?.to_string(),
        },
        "set" => DeviceLine::Set {
            key: o.need_member("k", SET_KEYS)?,
            value: o.need_u16("v")?,
        },
        "faces" => DeviceLine::Faces {
            list: faces(o.array("list")?.ok_or(DecodeError::Parse("list"))?)?,
        },
        "face" => DeviceLine::Face {
            f: slot_field(o.need_u8("f", 8)?, "f")?,
        },
        "err" => DeviceLine::Err {
            why: o.need_member("why", WHYS)?,
        },
        other => return Err(DecodeError::Unknown(short(other))),
    })
}

const SET_KEYS: &[(&str, SetKey)] = &[
    ("speaker", SetKey::Speaker),
    ("mic", SetKey::Mic),
    ("announce", SetKey::Announce),
    ("chime", SetKey::Chime),
    ("autojump", SetKey::Autojump),
    ("brightness", SetKey::Brightness),
    ("idlehome", SetKey::Idlehome),
    ("demo", SetKey::Demo),
    ("text", SetKey::Textsize),
    ("arcadesound", SetKey::Arcadesound),
];

fn doc_kind(name: &str) -> Result<crate::DocKind, DecodeError> {
    match name {
        "c" => Ok(crate::DocKind::Card),
        "r" => Ok(crate::DocKind::Reply),
        "w" => Ok(crate::DocKind::Workflow),
        "t" => Ok(crate::DocKind::Tr),
        _ => Err(DecodeError::Unknown("d".to_string())),
    }
}

fn slot(n: u8) -> Result<u8, DecodeError> {
    slot_field(n, "agent")
}

fn slot_field(n: u8, field: &'static str) -> Result<u8, DecodeError> {
    if n == 0 {
        Err(DecodeError::Range(field))
    } else {
        Ok(n)
    }
}

/// Parse one host line (as `encode` or `deck-sim.py` writes it) back into a
/// `HostLine`: the `device raw`/`--send` path and the round-trip tests.
/// Strings are taken as they are (no caps); the validation mirrors the
/// firmware's parser for shapes and ranges.
pub fn parse_host_line(bytes: &[u8]) -> Result<HostLine, DecodeError> {
    let map = object(bytes)?;
    let o = Obj(&map);
    let m = o.need_str("m")?;
    Ok(match m {
        "hello" => {
            let v = o.need_u8("v", u8::MAX)?;
            if v != crate::VERSION {
                return Err(DecodeError::Range("v"));
            }
            HostLine::Hello {
                sid: o.need_uint("sid", u64::from(u32::MAX))? as u32,
                hm: o.str("hm")?.map(str::to_string),
            }
        }
        "synced" => HostLine::Synced,
        "agent" => {
            let slot = slot_field(o.need_u8("id", 8)?, "id")?;
            if o.flag("gone")? {
                HostLine::AgentGone { slot }
            } else {
                HostLine::Agent {
                    slot,
                    kind: o.member("kind", KINDS)?,
                    name: o.str("name")?.map(str::to_string),
                    project: o.str("proj")?.map(str::to_string),
                    state: o.member("state", STATES)?,
                    title: o.str("title")?.map(str::to_string),
                    since_s: o.uint("since", u64::from(u32::MAX))?.map(|n| n as u32),
                }
            }
        }
        "card" => {
            let slot = slot(o.need_u8("agent", 8)?)?;
            let id = o.need_u16("id")?;
            if o.flag("clear")? {
                HostLine::CardClear { slot, id }
            } else {
                let kind = match o.need_str("type")? {
                    "q" => CardKind::Q,
                    "r" => CardKind::R,
                    other => return Err(DecodeError::Unknown(short(other))),
                };
                let choices = match o.array("choices")? {
                    None => Vec::new(),
                    Some(list) => list
                        .iter()
                        .map(|c| {
                            c.as_str()
                                .map(str::to_string)
                                .ok_or(DecodeError::Parse("choices"))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                };
                if choices.len() > 4 {
                    return Err(DecodeError::Range("choices"));
                }
                let rec = o.u8("rec", u8::MAX)?;
                if rec.is_some_and(|r| usize::from(r) >= choices.len()) {
                    return Err(DecodeError::Range("rec"));
                }
                HostLine::Card {
                    slot,
                    id,
                    kind,
                    text: o.need_str("text")?.to_string(),
                    choices,
                    rec,
                }
            }
        }
        "doc" => {
            let lines = match o.array("lines")? {
                None => Vec::new(),
                Some(list) => list
                    .iter()
                    .map(|l| {
                        l.as_str()
                            .map(str::to_string)
                            .ok_or(DecodeError::Parse("lines"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            if lines.len() > crate::encode::caps().doc_lines {
                return Err(DecodeError::Range("lines"));
            }
            let first = o.need_u16("first")?;
            let total = o.need_u16("total")?;
            if usize::from(first) + lines.len() > usize::from(total) {
                return Err(DecodeError::Range("first"));
            }
            HostLine::DocText {
                slot: slot(o.need_u8("agent", 8)?)?,
                doc: doc_kind(o.need_str("d")?)?,
                id: o.need_u16("id")?,
                first,
                total,
                font: match o.need_str("font")? {
                    "title" => crate::DocFont::Title,
                    "name" => crate::DocFont::Name,
                    "row" => crate::DocFont::Row,
                    "text" => crate::DocFont::Text,
                    "richrow" => crate::DocFont::RichRow,
                    "richtext" => crate::DocFont::RichText,
                    "richtitle" => crate::DocFont::RichTitle,
                    _ => return Err(DecodeError::Unknown("font".to_string())),
                },
                lines,
            }
        }
        "reply" => HostLine::Reply {
            slot: slot(o.need_u8("agent", 8)?)?,
            text: o.need_str("text")?.to_string(),
            rev: o.need_u16("rev")?,
        },
        "image" => HostLine::Image { slot: slot(o.need_u8("agent", 8)?)?, rev: o.need_u16("r")?, index: o.need_u8("i", 255)?, offset: o.need_u16("o")?, total: o.need_u16("n")?, width: o.need_u16("w")?, height: o.need_u16("h")?, data: o.need_str("data")?.to_string() },
        "session" => {
            if o.array("choices")?.is_some_and(|a| a.len() > 4) { return Err(DecodeError::Range("choices")); }
            let menu = crate::SessionMenu {
            slot: slot(o.need_u8("agent", 8)?)?,
            rev: o.need_u16("rev")?, field: o.need_u8("field", 2)?,
            page: o.need_u8("page", 63)?, status: o.need_u8("status", 3)?,
            model: o.need_str("model")?.into(),
            effort: o.need_str("effort")?.into(),
            note: o.need_str("note")?.into(),
            choices: o.array("choices")?.ok_or(DecodeError::Parse("choices"))?.iter().map(|v| v.as_str().map(str::to_string).ok_or(DecodeError::Parse("choices"))).collect::<Result<Vec<_>, _>>()?, total: o.need_u8("total", 255)?,
            selected: o.u8("selected", 254)?,
            };
            if usize::from(menu.page) * 4 + menu.choices.len() > usize::from(menu.total)
                || menu.selected.is_some_and(|i| i >= menu.total) { return Err(DecodeError::Range("session")); }
            HostLine::SessionMenu(menu)
        },
        "transcript" => HostLine::Transcript {
            slot: slot(o.need_u8("agent", 8)?)?,
            text: o.need_str("text")?.to_string(),
            last: o.flag("final")?,
            press: o.uint("p", u64::from(u16::MAX))?.map(|n| n as u16),
            failed: o.flag("failed")?,
            channel_tip: o.flag("channel_tip")?,
            rec: o.flag("rec")?,
        },
        "focus" => HostLine::Focus {
            slot: slot(o.need_u8("agent", 8)?)?,
            ack: o.uint("ack", u64::from(u16::MAX))?.map(|n| n as u16),
        },
        "ws" => {
            let active = o.need_u8("active", 9)?;
            let count = o.need_u8("count", 9)?;
            let sel = o.need_u8("sel", u8::MAX)?;
            let windows = match o.array("windows")? {
                None => Vec::new(),
                Some(list) => list
                    .iter()
                    .map(|w| {
                        let Value::Object(map) = w else {
                            return Err(DecodeError::Parse("windows"));
                        };
                        let w = Obj(map);
                        Ok(Window {
                            tone: w.member("c", TONES)?,
                            title: w.need_str("t")?.to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            if windows.len() > 6 {
                return Err(DecodeError::Range("windows"));
            }
            if active > count {
                return Err(DecodeError::Range("active"));
            }
            if usize::from(sel) >= windows.len().max(1) {
                return Err(DecodeError::Range("sel"));
            }
            HostLine::Ws {
                active,
                count,
                sel,
                windows,
            }
        }
        "macro" => {
            let keys = o
                .array("keys")?
                .ok_or(DecodeError::Parse("keys"))?
                .iter()
                .map(|k| {
                    let Value::Object(map) = k else {
                        return Err(DecodeError::Parse("keys"));
                    };
                    let k = Obj(map);
                    Ok(MacroKey {
                        label: k.need_str("l")?.to_string(),
                        binding: k.member("b", BINDINGS)?,
                        keys: k.str("k")?.map(str::to_string),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if keys.len() > 9 {
                return Err(DecodeError::Range("keys"));
            }
            HostLine::Macro {
                face: slot_field(o.need_u8("f", 8)?, "f")?,
                src: o.str("src")?.map(str::to_string),
                keys,
            }
        }
        "system" => {
            let s = crate::SystemStatus {
                cpu: o.u8("cpu", 100)?, gpu: o.u8("gpu", 100)?,
                memory_used_mib: o.need_u32("mu")?, memory_total_mib: o.need_u32("mt")?,
                disk_free_mib: o.need_u32("df")?, disk_total_mib: o.need_u32("dt")?, sample: o.need_u32("seq")?,
            };
            if !s.valid() { return Err(DecodeError::Range("system")); }
            HostLine::System(s)
        }
        "usage" => {
            let items = o
                .array("items")?
                .ok_or(DecodeError::Parse("items"))?
                .iter()
                .map(|item| {
                    let Value::Object(map) = item else {
                        return Err(DecodeError::Parse("items"));
                    };
                    let i = Obj(map);
                    Ok(UsageItem {
                        reset_tip: i.str("tip")?.map(str::to_string),
                        name: i.need_str("n")?.to_string(),
                        account: i.str("a")?.map(str::to_string),
                        percent: i.need_u8("p", 100)?,
                        reset: i.need_str("r")?.to_string(),
                        tone: i.member("c", TONES)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if items.len() > 6 {
                return Err(DecodeError::Range("items"));
            }
            HostLine::Usage {
                period: o.need_str("period")?.to_string(),
                items,
            }
        }
        "faces" => HostLine::Faces {
            list: faces(o.array("list")?.ok_or(DecodeError::Parse("list"))?)?,
        },
        "name" => HostLine::Name {
            text: o.need_str("text")?.to_string(),
        },
        "set" => {
            let key = o.need_member("k", SET_KEYS)?;
            let value = o.need_u16("v")?;
            if !key.accepts(value) {
                return Err(DecodeError::Range("v"));
            }
            HostLine::Set { key, value }
        }
        other => return Err(DecodeError::Unknown(short(other))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode, ErrWhy, KeyId, PttCtx, Template};

    fn report(bytes: &[u8]) -> Vec<u8> {
        let mut r = vec![6, 5, bytes.len() as u8];
        r.extend_from_slice(bytes);
        r.resize(64, 0);
        r
    }

    fn parsed(line: &str) -> Result<DeviceLine, DecodeError> {
        parse_device_line(line.as_bytes())
    }

    #[test]
    fn every_device_line_of_the_protocol_parses() {
        assert_eq!(
            parsed(r#"{"m":"hello","v":1,"fw":"0.1.0","sid":0,"boot":1,"rx":1024,"faces":[{"f":1,"t":"agents","n":"Agents"},{"f":6,"t":"settings","n":"Settings"}]}"#).unwrap(),
            DeviceLine::Hello {
                v: 1,
                fw: "0.1.0".into(),
                sid: 0,
                boot: true,
                rx: 1024,
                faces: vec![
                    FaceEntry { f: 1, t: Template::Agents, n: "Agents".into() },
                    FaceEntry { f: 6, t: Template::Settings, n: "Settings".into() },
                ],
            }
        );
        assert_eq!(
            parsed(
                r#"{"m":"hello","v":1,"fw":"0.1.0","sid":4294967295,"boot":0,"rx":1024,"faces":[]}"#
            )
            .unwrap(),
            DeviceLine::Hello {
                v: 1,
                fw: "0.1.0".into(),
                sid: u32::MAX,
                boot: false,
                rx: 1024,
                faces: vec![]
            }
        );
        assert_eq!(
            parsed(r#"{"m":"key","k":"cw","act":2,"shift":0,"f":2}"#).unwrap(),
            DeviceLine::Key {
                k: KeyId::Cw,
                act: 2,
                shift: false,
                f: 2
            }
        );
        assert_eq!(
            parsed(r#"{"m":"key","k":"enter","act":1,"shift":true,"f":2}"#).unwrap(),
            DeviceLine::Key {
                k: KeyId::Enter,
                act: 1,
                shift: true,
                f: 2
            }
        );
        assert_eq!(parsed(r#"{"m":"up"}"#).unwrap(), DeviceLine::Up);
        assert_eq!(
            parsed(r#"{"m":"channel","agent":1,"seq":1}"#).unwrap(),
            DeviceLine::Channel { slot: 1, seq: 1 }
        );
        assert_eq!(
            parsed(r#"{"m":"ptt","act":1,"agent":1,"ctx":"answer","card":1}"#).unwrap(),
            DeviceLine::Ptt {
                act: 1,
                slot: 1,
                ctx: PttCtx::Answer,
                card: Some(1),
                abort: false, press: None,
            }
        );
        assert_eq!(
            parsed(r#"{"m":"ptt","act":0,"agent":0,"ctx":"name","abort":1}"#).unwrap(),
            DeviceLine::Ptt {
                act: 0,
                slot: 0,
                ctx: PttCtx::Name,
                card: None,
                abort: true, press: None,
            }
        );
        assert_eq!(
            parsed(r#"{"m":"ptt","act":0,"agent":2,"ctx":"talk","card":0}"#).unwrap(),
            DeviceLine::Ptt {
                act: 0,
                slot: 2,
                ctx: PttCtx::Talk,
                card: None,
                abort: false, press: None,
            }
        );
        assert_eq!(
            parsed(r#"{"m":"answer","agent":1,"card":1,"choice":0}"#).unwrap(),
            DeviceLine::Answer {
                slot: 1,
                card: 1,
                choice: 0
            }
        );
        assert_eq!(
            parsed(r#"{"m":"snooze","agent":3,"card":9}"#).unwrap(),
            DeviceLine::Snooze { slot: 3, card: 9 }
        );
        assert_eq!(
            parsed(r#"{"m":"macro","f":3,"i":0,"act":1}"#).unwrap(),
            DeviceLine::Macro { f: 3, i: 0, act: 1 }
        );
        assert_eq!(
            parsed(r#"{"m":"macrokey","f":3,"i":4,"l":"shot","b":"cw","k":"shift+cmd+4"}"#).unwrap(),
            DeviceLine::MacroKey {
                f: 3,
                i: 4,
                label: Some("shot".into()),
                binding: Some(crate::Binding::Cw),
                keys: Some("shift+cmd+4".into())
            }
        );
        assert_eq!(
            parsed(r#"{"m":"macrokey","f":3,"i":8}"#).unwrap(),
            DeviceLine::MacroKey { f: 3, i: 8, label: None, binding: None, keys: None }
        );
        assert!(parsed(r#"{"m":"macrokey","f":3,"i":9}"#).is_err());
        assert!(parsed(r#"{"m":"macrokey","f":3,"i":1,"l":"x","k":"cmd+ptt"}"#).is_err());
        assert!(crate::keystroke_valid("ctrl+alt+delete") && !crate::keystroke_valid("cmd") && !crate::keystroke_valid("cmd+c+v"));
        assert_eq!(parsed(r#"{"m":"ws","go":3}"#).unwrap(), DeviceLine::WsGo(3));
        assert_eq!(
            parsed(r#"{"m":"ws","win":2}"#).unwrap(),
            DeviceLine::WsWin(2)
        );
        assert_eq!(
            parsed(r#"{"m":"refresh","what":"usage"}"#).unwrap(),
            DeviceLine::Refresh {
                what: "usage".into()
            }
        );
        assert_eq!(
            parsed(r#"{"m":"set","k":"brightness","v":3}"#).unwrap(),
            DeviceLine::Set {
                key: SetKey::Brightness,
                value: 3
            }
        );
        assert_eq!(
            parsed(r#"{"m":"faces","list":[{"f":2,"t":"omarchy","n":"Omarchy"}]}"#).unwrap(),
            DeviceLine::Faces {
                list: vec![FaceEntry {
                    f: 2,
                    t: Template::Omarchy,
                    n: "Omarchy".into()
                }]
            }
        );
        assert_eq!(
            parsed(r#"{"m":"face","f":2}"#).unwrap(),
            DeviceLine::Face { f: 2 }
        );
        assert_eq!(
            parsed(r#"{"m":"err","why":"range"}"#).unwrap(),
            DeviceLine::Err { why: ErrWhy::Range }
        );
    }

    #[test]
    fn unknown_fields_are_ignored_and_bad_lines_classified() {
        assert_eq!(
            parsed(r#"{"m":"up","extra":[1,2]}"#).unwrap(),
            DeviceLine::Up
        );
        assert_eq!(parsed("not json"), Err(DecodeError::Parse("json")));
        assert_eq!(parsed("[1]"), Err(DecodeError::Parse("json")));
        assert_eq!(parsed(r#"{"m":1}"#), Err(DecodeError::Parse("m")));
        assert_eq!(
            parsed(r#"{"m":"bogus"}"#),
            Err(DecodeError::Unknown("bogus".into()))
        );
        assert_eq!(
            parsed(r#"{"m":"key","k":"home","act":1,"f":1}"#),
            Err(DecodeError::Unknown("home".into()))
        );
        assert_eq!(
            parsed(r#"{"m":"key","k":"cw","act":3,"f":1}"#),
            Err(DecodeError::Range("act"))
        );
        assert_eq!(
            parsed(r#"{"m":"channel","agent":9,"seq":1}"#),
            Err(DecodeError::Range("agent"))
        );
        assert_eq!(
            parsed(r#"{"m":"channel","agent":0,"seq":1}"#),
            Err(DecodeError::Range("agent"))
        );
        assert_eq!(
            parsed(r#"{"m":"channel","agent":1,"seq":70000}"#),
            Err(DecodeError::Range("seq"))
        );
        assert_eq!(
            parsed(r#"{"m":"channel","agent":"1","seq":1}"#),
            Err(DecodeError::Parse("agent"))
        );
        assert_eq!(
            parsed(r#"{"m":"channel","agent":-1,"seq":1}"#),
            Err(DecodeError::Range("agent"))
        );
        assert_eq!(
            parsed(r#"{"m":"answer","agent":1,"card":1,"choice":4}"#),
            Err(DecodeError::Range("choice"))
        );
        assert_eq!(
            parsed(r#"{"m":"answer","agent":1,"card":1}"#),
            Err(DecodeError::Parse("choice"))
        );
        assert_eq!(
            parsed(r#"{"m":"ptt","act":1,"agent":1,"ctx":"talk","abort":2}"#),
            Err(DecodeError::Range("abort"))
        );
        assert_eq!(parsed(r#"{"m":"ws"}"#), Err(DecodeError::Parse("ws")));
        assert_eq!(
            parsed(r#"{"m":"ws","go":1,"win":1}"#),
            Err(DecodeError::Parse("ws"))
        );
        assert_eq!(
            parsed(
                r#"{"m":"hello","v":1,"fw":"0.1.0","sid":0,"boot":1,"rx":1024,"faces":[{"f":0,"t":"agents","n":"A"}]}"#
            ),
            Err(DecodeError::Range("f"))
        );
        assert_eq!(
            parsed(r#"{"m":"faces","list":[{"f":1,"t":"lava","n":"A"}]}"#),
            Err(DecodeError::Unknown("lava".into()))
        );
        assert_eq!(parsed(r#"{"m":"faces"}"#), Err(DecodeError::Parse("list")));
        let long = format!("{{\"m\":\"{}\"}}", "z".repeat(100));
        assert_eq!(parsed(&long), Err(DecodeError::Unknown("z".repeat(32))));
        assert_eq!(
            parsed(r#"{"m":"hello","v":1,"fw":"0.1.0","sid":null,"rx":1024}"#),
            Err(DecodeError::Parse("sid"))
        );
    }

    #[test]
    fn the_assembler_joins_split_lines_and_ignores_other_channels() {
        let mut a = Assembler::new();
        let mut got = Vec::new();
        let line = b"{\"m\":\"channel\",\"agent\":1,\"seq\":1}\n{\"m\":\"up\"}\n{\"m\":\"fa";
        a.push_report(&report(line), &mut |l| got.push(l));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Ok(DeviceLine::Channel { slot: 1, seq: 1 }));
        assert_eq!(got[1], Ok(DeviceLine::Up));
        // Channel 2 and 4 traffic in the middle of a line is ignored.
        let mut other = report(b"garbage");
        other[1] = 2;
        a.push_report(&other, &mut |l| got.push(l));
        other[1] = 4;
        a.push_report(&other, &mut |l| got.push(l));
        assert_eq!(got.len(), 2);
        a.push_report(&report(b"ce\",\"f\":3}\n"), &mut |l| got.push(l));
        assert_eq!(got.len(), 3);
        assert_eq!(got[2], Ok(DeviceLine::Face { f: 3 }));
        // Without the report id (hidapi strips it on some platforms).
        let mut short = report(b"{\"m\":\"up\"}\n");
        short.remove(0);
        a.push_report(&short, &mut |l| got.push(l));
        assert_eq!(got[3], Ok(DeviceLine::Up));
        // A `len` beyond the buffer or beyond 61 cannot read past the end.
        let mut bad = report(b"{\"m\":\"up\"}\n");
        bad[2] = 200;
        a.push_report(&bad, &mut |l| got.push(l));
        assert_eq!(got.len(), 5);
        a.push_report(&[6, 5, 61], &mut |l| got.push(l));
        a.push_report(&[6], &mut |l| got.push(l));
        a.push_report(&[], &mut |l| got.push(l));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn an_overlong_line_is_reported_once_and_the_next_line_is_intact() {
        let mut a = Assembler::new();
        let mut got = Vec::new();
        let mut junk = vec![b'x'; 1100];
        junk.push(b'\n');
        junk.extend_from_slice(b"{\"m\":\"up\"}\n");
        for chunk in junk.chunks(61) {
            a.push_report(&report(chunk), &mut |l| got.push(l));
        }
        assert_eq!(got, vec![Err(DecodeError::Size), Ok(DeviceLine::Up)]);
        // Exactly LINE_MAX - 1 bytes plus LF still fits; one more does not.
        let mut got = Vec::new();
        let mut fits = vec![b'y'; LINE_MAX - 1];
        fits.push(b'\n');
        for chunk in fits.chunks(61) {
            a.push_report(&report(chunk), &mut |l| got.push(l));
        }
        assert!(matches!(got.as_slice(), [Err(DecodeError::Parse("json"))]));
        let mut got = Vec::new();
        let mut over = vec![b'y'; LINE_MAX];
        over.push(b'\n');
        for chunk in over.chunks(61) {
            a.push_report(&report(chunk), &mut |l| got.push(l));
        }
        assert_eq!(got, vec![Err(DecodeError::Size)]);
    }

    #[test]
    fn empty_lines_are_skipped_and_clear_forgets_a_partial() {
        let mut a = Assembler::new();
        let mut got = Vec::new();
        a.push_report(&report(b"\n\n{\"m\":\"up"), &mut |l| got.push(l));
        assert!(got.is_empty());
        a.clear();
        a.push_report(&report(b"\"}\n"), &mut |l| got.push(l));
        assert_eq!(got, vec![Err(DecodeError::Parse("json"))]);
    }

    #[test]
    fn host_lines_round_trip_through_parse_host_line() {
        let lines = vec![
            HostLine::Hello {
                sid: 7,
                hm: Some("00:00".into()),
            },
            HostLine::Hello { sid: 0, hm: None },
            HostLine::Synced,
            HostLine::Agent {
                slot: 8,
                kind: Some(crate::Kind::Gemini),
                name: Some("n".into()),
                project: Some("p".into()),
                state: Some(crate::AgentState::Idle),
                title: Some("t".into()),
                since_s: Some(1),
            },
            HostLine::Agent {
                slot: 1,
                kind: None,
                name: None,
                project: None,
                state: None,
                title: None,
                since_s: None,
            },
            HostLine::AgentGone { slot: 2 },
            HostLine::Card {
                slot: 1,
                id: 65535,
                kind: CardKind::R,
                text: "done".into(),
                choices: vec!["a".into(), "b".into()],
                rec: Some(1),
            },
            HostLine::CardClear { slot: 1, id: 1 },
            HostLine::Reply {
                slot: 1,
                text: "r".into(),
                rev: 1,
            },
            HostLine::Transcript {
                slot: 1,
                text: "t".into(),
                last: true, press: None, failed: false, channel_tip: false, rec: false,
            },
            HostLine::Transcript {
                slot: 1,
                text: "t".into(),
                last: false, press: None, failed: false, channel_tip: false, rec: false,
            },
            HostLine::Focus {
                slot: 1,
                ack: Some(65535),
            },
            HostLine::Focus { slot: 1, ack: None },
            HostLine::Ws {
                active: 1,
                count: 1,
                sel: 0,
                windows: vec![
                    Window {
                        tone: None,
                        title: "w".into(),
                    },
                    Window {
                        tone: Some(crate::Tone::Gray),
                        title: "x".into(),
                    },
                ],
            },
            HostLine::Macro {
                face: 3,
                src: None,
                keys: vec![MacroKey {
                    label: "l".into(),
                    binding: Some(crate::Binding::Cw),
                    keys: None,
                }],
            },
            HostLine::Usage {
                period: "p".into(),
                items: vec![UsageItem {
                    reset_tip: None,
                    name: "n".into(),
                    account: None,
                    percent: 0,
                    reset: "r".into(),
                    tone: Some(crate::Tone::Green),
                }],
            },
            HostLine::Faces {
                list: vec![FaceEntry {
                    f: 8,
                    t: Template::Usage,
                    n: "U".into(),
                }],
            },
            HostLine::Name {
                text: "Deploy".into(),
            },
            HostLine::Set {
                key: SetKey::Idlehome,
                value: 120,
            },
        ];
        for line in lines {
            let bytes = encode(&line).unwrap();
            let back = parse_host_line(&bytes[..bytes.len() - 1]).unwrap();
            assert_eq!(back, line);
        }
        assert_eq!(
            parse_host_line(
                br#"{"m":"card","agent":1,"id":1,"type":"q","text":"t","choices":["a"],"rec":1}"#
            ),
            Err(DecodeError::Range("rec"))
        );
        assert_eq!(
            parse_host_line(br#"{"m":"hello","v":2,"sid":1}"#),
            Err(DecodeError::Range("v"))
        );
        assert_eq!(
            parse_host_line(br#"{"m":"set","k":"speaker","v":11}"#),
            Err(DecodeError::Range("v"))
        );
        assert_eq!(
            parse_host_line(br#"{"m":"ws","active":1,"count":2,"sel":1}"#),
            Err(DecodeError::Range("sel"))
        );
        assert_eq!(
            parse_host_line(br#"{"m":"agent","id":9}"#),
            Err(DecodeError::Range("id"))
        );
    }
}
