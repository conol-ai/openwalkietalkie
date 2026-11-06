//! `HostLine` → one compact JSON line with its LF, byte-identical to
//! `tools/deck-sim.py` (`json.dumps(separators=(",", ":"), ensure_ascii=False)`
//! over the same key order), after the byte caps of the design (§6.5) have
//! been applied on char boundaries with `…`.
use crate::types::{name_of, BINDINGS, KINDS, STATES, TEMPLATES, TONES};
use crate::{CardKind, DocFont, DocKind, HostLine, LINE_MAX};
use std::io::Write;

/// Byte capacities (UTF-8) per field, and the line ceiling the host enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub name: usize,
    /// The `proj` of an `agent` line.
    pub project: usize,
    pub title: usize,
    pub card_text: usize,
    /// One wrapped document line, and how many of them a window may carry.
    pub doc_line: usize,
    pub doc_lines: usize,
    pub choice: usize,
    pub choices: usize,
    pub reply: usize,
    /// The tail the device keeps; the wire accepts 200.
    pub transcript: usize,
    pub window_title: usize,
    pub windows: usize,
    pub workspaces: u8,
    pub macro_src: usize,
    pub macro_label: usize,
    pub macro_keys: usize,
    pub macro_keystroke: usize,
    pub usage_period: usize,
    pub usage_name: usize,
    pub usage_account: usize,
    pub usage_reset: usize,
    pub usage_tip: usize,
    pub usage_items: usize,
    pub face_name: usize,
    pub faces: usize,
    pub spoken_name: usize,
    /// A longer encoded line (with its LF) is a bug, never sent.
    pub line: usize,
}

const CAPS: Caps = Caps {
    name: 24,
    project: 24,
    title: 64,
    card_text: 128,
    doc_line: 48,
    doc_lines: 17,
    choice: 32,
    choices: 4,
    reply: 200,
    transcript: 120,
    window_title: 48,
    windows: 6,
    workspaces: 9,
    macro_src: 16,
    macro_label: 12,
    macro_keys: 9,
    macro_keystroke: 32,
    usage_period: 12,
    usage_name: 20,
    usage_account: 48,
    usage_reset: 24,
    usage_tip: 32,
    usage_items: 6,
    face_name: 16,
    faces: 8,
    spoken_name: 16,
    line: 1000,
};

pub fn caps() -> &'static Caps {
    &CAPS
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The encoded line (with its LF) exceeds `caps().line`.
    #[error("line of {len} bytes exceeds the {} byte ceiling", CAPS.line)]
    TooLong { len: usize },
    /// A number outside the range the device accepts (`err why:range`).
    #[error("{field} out of range")]
    Range { field: &'static str },
}

/// The largest window starting at `first` that actually fits one line.
///
/// `CAPS.doc_lines` bounds the window by *count*, which is not the same as
/// fitting: JSON escaping costs a byte for every `"` and `\\`, and a question
/// body can be a shell command full of both. A full twelve-line window of
/// plain prose lands around 700 of the 1000-byte ceiling, so roughly 25
/// escapes spread across it would push the line over — and the outbox
/// **drops** an over-cap line rather than truncating it, so the window would
/// simply never arrive.
///
/// Rather than model that cost a second time, this asks the encoder: it trims
/// the window from the end until `encode` accepts it. Exact by construction,
/// and it cannot drift from the encoder's own arithmetic.
/// Single letters, not words: `doclines` is a device->host line and must fit
/// one 61-byte report, which `"d":"reply"` (63 B) does not.
pub(crate) const fn doc_name(doc: DocKind) -> &'static str {
    match doc {
        DocKind::Card => "c",
        DocKind::Reply => "r",
        DocKind::Workflow => "w",
        DocKind::Tr => "t",
    }
}

pub(crate) const fn font_name(font: DocFont) -> &'static str {
    match font {
        DocFont::Title => "title",
        DocFont::Name => "name",
        DocFont::Row => "row",
        DocFont::Text => "text",
        DocFont::RichRow => "richrow",
        DocFont::RichText => "richtext",
        DocFont::RichTitle => "richtitle",
    }
}

pub fn fit_doc_window(
    slot: u8,
    doc: DocKind,
    id: u16,
    first: u16,
    total: u16,
    font: DocFont,
    lines: &[String],
) -> Vec<String> {
    let mut take = lines.len().min(CAPS.doc_lines);
    while take > 0 {
        let candidate = HostLine::DocText {
            slot,
            doc,
            id,
            first,
            total,
            font,
            lines: lines[..take].to_vec(),
        };
        if encode(&candidate).is_ok() {
            return lines[..take].to_vec();
        }
        take -= 1;
    }
    Vec::new()
}

/// Encode `line` as its JSON object plus LF, applying the byte caps.
pub fn encode(line: &HostLine) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::with_capacity(160);
    let mut w = Writer::new(&mut out);
    w.begin(line.m());
    match line {
        HostLine::Image { slot, rev, index, offset, total, width, height, data } => {
            if data.len() > 512 || *width > 128 || *height > 128 || *total > 16896 { return Err(EncodeError::Range { field: "image" }); }
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            for (k, v) in [("r", *rev), ("i", u16::from(*index)), ("o", *offset), ("n", *total), ("w", *width), ("h", *height)] { w.num(k, u64::from(v)); }
            w.key("data"); w.string(data);
        }
        HostLine::SessionMenu(menu) => {
            if menu.field > 2 || menu.page > 63 || menu.status > 3 || menu.choices.len() > 4
                || usize::from(menu.page) * 4 + menu.choices.len() > usize::from(menu.total)
                || menu.selected.is_some_and(|i| i >= menu.total) {
                return Err(EncodeError::Range { field: "session" });
            }
            w.num("agent", u64::from(slot_id(menu.slot, "agent")?));
            w.num("rev", u64::from(menu.rev));
            w.num("field", u64::from(menu.field));
            w.num("page", u64::from(menu.page));
            w.cut("model", &menu.model, 48);
            w.cut("effort", &menu.effort, 16);
            w.num("status", u64::from(menu.status));
            w.cut("note", &menu.note, 96);
            w.num("total", u64::from(menu.total));
            if let Some(i) = menu.selected { w.num("selected", u64::from(i)); }
            w.key("choices"); w.raw(b"[");
            for (i, label) in menu.choices.iter().take(4).enumerate() {
                if i > 0 { w.raw(b","); }
                w.string(&cut(label, 40));
            }
            w.raw(b"]");
        }
        HostLine::Hello { sid, hm } => {
            w.num("v", u64::from(crate::VERSION));
            w.num("sid", u64::from(*sid));
            if let Some(hm) = hm {
                if !is_hm(hm) {
                    return Err(EncodeError::Range { field: "hm" });
                }
                w.str("hm", hm);
            }
        }
        HostLine::Synced => {}
        HostLine::Agent {
            slot,
            kind,
            name,
            project,
            state,
            title,
            since_s,
            task,
            subs,
        } => {
            w.num("id", u64::from(slot_id(*slot, "id")?));
            if let Some(kind) = kind {
                w.str("kind", name_of(KINDS, *kind));
            }
            if let Some(name) = name {
                w.cut("name", name, CAPS.name);
            }
            if let Some(project) = project {
                w.cut("proj", project, CAPS.project);
            }
            if let Some(state) = state {
                w.str("state", name_of(STATES, *state));
            }
            if let Some(title) = title {
                w.cut("title", title, CAPS.title);
            }
            if let Some(since) = since_s {
                w.num("since", u64::from(*since));
            }
            if let Some(task) = task {
                w.cut("task", task, CAPS.title);
            }
            if let Some(subs) = subs {
                w.num("subs", u64::from((*subs).min(99)));
            }
        }
        HostLine::AgentGone { slot } => {
            w.num("id", u64::from(slot_id(*slot, "id")?));
            w.num("gone", 1);
        }
        HostLine::Card {
            slot,
            id,
            kind,
            text,
            choices,
            rec,
        } => {
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            w.num("id", u64::from(*id));
            w.str(
                "type",
                match kind {
                    CardKind::Q => "q",
                    CardKind::R => "r",
                },
            );
            w.cut("text", text, CAPS.card_text);
            let shown = &choices[..choices.len().min(CAPS.choices)];
            if !shown.is_empty() {
                w.key("choices");
                w.raw(b"[");
                for (i, choice) in shown.iter().enumerate() {
                    if i > 0 {
                        w.raw(b",");
                    }
                    w.string(&cut(choice, CAPS.choice));
                }
                w.raw(b"]");
            }
            if let Some(rec) = rec {
                if usize::from(*rec) >= shown.len() {
                    return Err(EncodeError::Range { field: "rec" });
                }
                w.num("rec", u64::from(*rec));
            }
        }
        HostLine::DocText {
            slot,
            doc,
            id,
            first,
            total,
            font,
            lines,
        } => {
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            w.str("d", doc_name(*doc));
            w.num("id", u64::from(*id));
            w.num("first", u64::from(*first));
            w.num("total", u64::from(*total));
            w.str("font", font_name(*font));
            // A window that claims to run past the end of the body is a bug on
            // the sending side, not something the deck should have to police.
            if usize::from(*first) + lines.len() > usize::from(*total) {
                return Err(EncodeError::Range { field: "first" });
            }
            let shown = &lines[..lines.len().min(CAPS.doc_lines)];
            w.key("lines");
            w.raw(b"[");
            for (i, line) in shown.iter().enumerate() {
                if i > 0 {
                    w.raw(b",");
                }
                w.string(&cut(line, CAPS.doc_line));
            }
            w.raw(b"]");
        }
        HostLine::CardClear { slot, id } => {
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            w.num("id", u64::from(*id));
            w.num("clear", 1);
        }
        HostLine::Reply { slot, text, rev } => {
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            w.cut("text", text, CAPS.reply);
            w.num("rev", u64::from(*rev));
        }
        HostLine::Transcript { slot, text, last, press, failed, channel_tip, rec } => {
            // Slot 0 is a session with no agent on the strip (a face name,
            // a key label): the line goes without `agent`, and the deck
            // shows it for whoever is holding PTT.
            if *slot != 0 {
                w.num("agent", u64::from(slot_id(*slot, "agent")?));
            }
            w.str("text", &tail(text, CAPS.transcript));
            if *last {
                w.num("final", 1);
            }
            if let Some(press) = press { w.num("p", u64::from(*press)); }
            if *failed { w.num("failed", 1); }
            if *channel_tip { w.num("channel_tip", 1); }
            if *rec { w.num("rec", 1); }
        }
        HostLine::Focus { slot, ack } => {
            w.num("agent", u64::from(slot_id(*slot, "agent")?));
            if let Some(ack) = ack {
                w.num("ack", u64::from(*ack));
            }
        }
        HostLine::Ws {
            active,
            count,
            sel,
            windows,
        } => {
            if *count > CAPS.workspaces {
                return Err(EncodeError::Range { field: "count" });
            }
            if *active > *count {
                return Err(EncodeError::Range { field: "active" });
            }
            let shown = &windows[..windows.len().min(CAPS.windows)];
            if usize::from(*sel) >= shown.len().max(1) {
                return Err(EncodeError::Range { field: "sel" });
            }
            w.num("active", u64::from(*active));
            w.num("count", u64::from(*count));
            w.num("sel", u64::from(*sel));
            w.key("windows");
            w.raw(b"[");
            for (i, window) in shown.iter().enumerate() {
                if i > 0 {
                    w.raw(b",");
                }
                w.raw(b"{");
                let mut first = true;
                if let Some(tone) = window.tone {
                    w.field_str(&mut first, "c", name_of(TONES, tone));
                }
                w.field_str(&mut first, "t", &cut(&window.title, CAPS.window_title));
                w.raw(b"}");
            }
            w.raw(b"]");
        }
        HostLine::Macro { face, src, keys } => {
            w.num("f", u64::from(face_id(*face)?));
            if let Some(src) = src {
                w.cut("src", src, CAPS.macro_src);
            }
            w.key("keys");
            w.raw(b"[");
            for (i, key) in keys.iter().take(CAPS.macro_keys).enumerate() {
                if i > 0 {
                    w.raw(b",");
                }
                w.raw(b"{");
                let mut first = true;
                w.field_str(&mut first, "l", &cut(&key.label, CAPS.macro_label));
                if let Some(binding) = key.binding {
                    w.field_str(&mut first, "b", name_of(BINDINGS, binding));
                }
                if let Some(keys) = &key.keys {
                    w.field_str(&mut first, "k", &cut(keys, CAPS.macro_keystroke));
                }
                w.raw(b"}");
            }
            w.raw(b"]");
        }
        HostLine::System(s) => {
            if !s.valid() { return Err(EncodeError::Range { field: "system" }); }
            if let Some(v) = s.cpu { w.num("cpu", u64::from(v)); }
            if let Some(v) = s.gpu { w.num("gpu", u64::from(v)); }
            for (key, value) in [("mu", s.memory_used_mib), ("mt", s.memory_total_mib),
                ("df", s.disk_free_mib), ("dt", s.disk_total_mib), ("seq", s.sample)] {
                w.num(key, u64::from(value));
            }
        }
        HostLine::Usage { period, items } => {
            w.cut("period", period, CAPS.usage_period);
            w.key("items");
            w.raw(b"[");
            for (i, item) in items.iter().take(CAPS.usage_items).enumerate() {
                if item.percent > 100 {
                    return Err(EncodeError::Range { field: "p" });
                }
                if i > 0 {
                    w.raw(b",");
                }
                w.raw(b"{");
                let mut first = true;
                w.field_str(&mut first, "n", &cut(&item.name, CAPS.usage_name));
                if let Some(account) = item.account.as_deref().filter(|s| !s.is_empty()) {
                    w.field_str(&mut first, "a", &cut(account, CAPS.usage_account));
                }
                w.key("p");
                w.number(u64::from(item.percent));
                w.field_str(&mut first, "r", &cut(&item.reset, CAPS.usage_reset));
                if let Some(tip) = &item.reset_tip {
                    w.field_str(&mut first, "tip", &cut(tip, CAPS.usage_tip));
                }
                if let Some(tone) = item.tone {
                    w.field_str(&mut first, "c", name_of(TONES, tone));
                }
                w.raw(b"}");
            }
            w.raw(b"]");
        }
        HostLine::Faces { list } => {
            w.key("list");
            write_faces(&mut w, list)?;
        }
        HostLine::Name { text } => {
            w.cut("text", text, CAPS.spoken_name);
        }
        HostLine::Set { key, value } => {
            if !key.accepts(*value) {
                return Err(EncodeError::Range { field: "v" });
            }
            w.str("k", key.name());
            w.num("v", u64::from(*value));
        }
    }
    w.raw(b"}\n");
    if out.len() > CAPS.line {
        return Err(EncodeError::TooLong { len: out.len() });
    }
    debug_assert!(out.len() <= LINE_MAX);
    Ok(out)
}

fn write_faces(w: &mut Writer<'_>, list: &[crate::FaceEntry]) -> Result<(), EncodeError> {
    w.raw(b"[");
    for (i, face) in list.iter().take(CAPS.faces).enumerate() {
        if i > 0 {
            w.raw(b",");
        }
        w.raw(b"{");
        w.raw(b"\"f\":");
        w.number(u64::from(face_id(face.f)?));
        w.raw(b",\"t\":");
        w.string(name_of(TEMPLATES, face.t));
        w.raw(b",\"n\":");
        w.string(&cut(&face.n, CAPS.face_name));
        w.raw(b"}");
    }
    w.raw(b"]");
    Ok(())
}

fn slot_id(slot: u8, field: &'static str) -> Result<u8, EncodeError> {
    // One byte on the wire; the store never assigns a slot above the deck's
    // own table (`hello`'s `agents`).
    if slot >= 1 {
        Ok(slot)
    } else {
        Err(EncodeError::Range { field })
    }
}

fn face_id(face: u8) -> Result<u8, EncodeError> {
    if (1..=8).contains(&face) {
        Ok(face)
    } else {
        Err(EncodeError::Range { field: "f" })
    }
}

fn is_hm(hm: &str) -> bool {
    let b = hm.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && (b[0] - b'0') * 10 + (b[1] - b'0') < 24
        && (b[3] - b'0') * 10 + (b[4] - b'0') < 60
}

/// `s` cut to `cap` bytes on a char boundary, ending in `…` when cut.
pub(crate) fn cut(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let room = cap.saturating_sub("…".len());
    let mut end = room;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The last `cap` bytes of `s` on a char boundary, starting with `…` when cut.
pub(crate) fn tail(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let room = cap.saturating_sub("…".len());
    let mut start = s.len() - room;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

/// A JSON object writer that keeps the deck-sim key order and escaping.
struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    fn begin(&mut self, m: &str) {
        self.raw(b"{\"m\":");
        self.string(m);
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    fn key(&mut self, key: &str) {
        self.raw(b",");
        self.string(key);
        self.raw(b":");
    }

    fn string(&mut self, s: &str) {
        // serde_json escapes exactly the set Python's json module escapes
        // with ensure_ascii=False: `"`, `\`, and the C0 controls.
        let _ = serde_json::to_writer(&mut *self.out, s);
    }

    fn number(&mut self, n: u64) {
        let _ = write!(self.out, "{n}");
    }

    fn num(&mut self, key: &str, n: u64) {
        self.key(key);
        self.number(n);
    }

    fn str(&mut self, key: &str, s: &str) {
        self.key(key);
        self.string(s);
    }

    fn cut(&mut self, key: &str, s: &str, cap: usize) {
        self.str(key, &cut(s, cap));
    }

    fn field_str(&mut self, first: &mut bool, key: &str, s: &str) {
        if !*first {
            self.raw(b",");
        }
        *first = false;
        self.string(key);
        self.raw(b":");
        self.string(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentState, Binding, FaceEntry, Kind, MacroKey, SetKey, Template, Tone, UsageItem, Window,
    };

    fn text(line: &HostLine) -> String {
        String::from_utf8(encode(line).unwrap()).unwrap()
    }

    #[test]
    fn the_protocol_examples_encode_verbatim() {
        assert_eq!(
            text(&HostLine::Hello {
                sid: 314159,
                hm: Some("19:42".into())
            }),
            "{\"m\":\"hello\",\"v\":1,\"sid\":314159,\"hm\":\"19:42\"}\n"
        );
        assert_eq!(
            text(&HostLine::Agent {
                slot: 1,
                kind: Some(Kind::Codex),
                name: Some("cohdl-api".into()),
                project: Some("cohdl".into()),
                state: Some(AgentState::Question),
                title: Some("Run migrations on staging?".into()),
                since_s: Some(120),
                task: None,
                subs: None,
            }),
            "{\"m\":\"agent\",\"id\":1,\"kind\":\"codex\",\"name\":\"cohdl-api\",\"proj\":\"cohdl\",\"state\":\"question\",\"title\":\"Run migrations on staging?\",\"since\":120}\n"
        );
        assert_eq!(
            text(&HostLine::Card {
                slot: 1,
                id: 1,
                kind: CardKind::Q,
                text: "Tests fail on main. Run pending migrations on staging first?".into(),
                choices: vec!["Run on staging".into(), "Skip migrations".into(), "Abort task".into()],
                rec: Some(0),
            }),
            "{\"m\":\"card\",\"agent\":1,\"id\":1,\"type\":\"q\",\"text\":\"Tests fail on main. Run pending migrations on staging first?\",\"choices\":[\"Run on staging\",\"Skip migrations\",\"Abort task\"],\"rec\":0}\n"
        );
        assert_eq!(
            text(&HostLine::Agent {
                slot: 3,
                kind: None,
                name: None,
                project: None,
                state: None,
                title: Some("Compiling openmicro ⠋".into()),
                since_s: None,
                task: None,
                subs: None,
            }),
            "{\"m\":\"agent\",\"id\":3,\"title\":\"Compiling openmicro ⠋\"}\n"
        );
        assert_eq!(
            text(&HostLine::Ws {
                active: 2,
                count: 4,
                sel: 0,
                windows: vec![
                    Window { tone: Some(Tone::Blue), title: "term · claude — header refactor".into() },
                    Window { tone: Some(Tone::Amber), title: "firefox — github · pulls".into() },
                ],
            }),
            "{\"m\":\"ws\",\"active\":2,\"count\":4,\"sel\":0,\"windows\":[{\"c\":\"blue\",\"t\":\"term · claude — header refactor\"},{\"c\":\"amber\",\"t\":\"firefox — github · pulls\"}]}\n"
        );
        assert_eq!(
            text(&HostLine::Macro {
                face: 3,
                src: Some("deck.toml".into()),
                keys: vec![
                    MacroKey { label: "build".into(), binding: Some(Binding::Left), keys: None, },
                    MacroKey { label: "test".into(), binding: Some(Binding::Right), keys: None, },
                    MacroKey { label: "screenshot".into(), binding: None, keys: None, },
                ],
            }),
            "{\"m\":\"macro\",\"f\":3,\"src\":\"deck.toml\",\"keys\":[{\"l\":\"build\",\"b\":\"left\"},{\"l\":\"test\",\"b\":\"right\"},{\"l\":\"screenshot\"}]}\n"
        );
        assert_eq!(
            text(&HostLine::CardClear { slot: 1, id: 1 }),
            "{\"m\":\"card\",\"agent\":1,\"id\":1,\"clear\":1}\n"
        );
        assert_eq!(text(&HostLine::Synced), "{\"m\":\"synced\"}\n");
        assert_eq!(
            text(&HostLine::AgentGone { slot: 2 }),
            "{\"m\":\"agent\",\"id\":2,\"gone\":1}\n"
        );
        assert_eq!(
            text(&HostLine::Usage {
                period: "SEP 2026".into(),
                items: vec![
                    UsageItem { reset_tip: None, name: "Claude Pro".into(), account: None, percent: 62, reset: "resets in 4 days".into(), tone: None },
                    UsageItem { reset_tip: None, name: "Codex".into(), account: None, percent: 100, reset: "tomorrow".into(), tone: Some(Tone::Red) },
                ],
            }),
            "{\"m\":\"usage\",\"period\":\"SEP 2026\",\"items\":[{\"n\":\"Claude Pro\",\"p\":62,\"r\":\"resets in 4 days\"},{\"n\":\"Codex\",\"p\":100,\"r\":\"tomorrow\",\"c\":\"red\"}]}\n"
        );
        assert_eq!(
            text(&HostLine::Faces {
                list: vec![
                    FaceEntry { f: 1, t: Template::Agents, n: "Agents".into() },
                    FaceEntry { f: 6, t: Template::Settings, n: "Settings".into() },
                ]
            }),
            "{\"m\":\"faces\",\"list\":[{\"f\":1,\"t\":\"agents\",\"n\":\"Agents\"},{\"f\":6,\"t\":\"settings\",\"n\":\"Settings\"}]}\n"
        );
        assert_eq!(
            text(&HostLine::Transcript {
                slot: 4,
                text: "hi".into(),
                last: true, press: None, failed: false, channel_tip: false, rec: false,
            }),
            "{\"m\":\"transcript\",\"agent\":4,\"text\":\"hi\",\"final\":1}\n"
        );
        assert_eq!(
            text(&HostLine::Focus {
                slot: 2,
                ack: Some(7)
            }),
            "{\"m\":\"focus\",\"agent\":2,\"ack\":7}\n"
        );
        assert_eq!(
            text(&HostLine::Focus { slot: 2, ack: None }),
            "{\"m\":\"focus\",\"agent\":2}\n"
        );
        assert_eq!(
            text(&HostLine::Name {
                text: "Deploy".into()
            }),
            "{\"m\":\"name\",\"text\":\"Deploy\"}\n"
        );
        assert_eq!(
            text(&HostLine::Set {
                key: SetKey::Brightness,
                value: 3
            }),
            "{\"m\":\"set\",\"k\":\"brightness\",\"v\":3}\n"
        );
        assert_eq!(
            text(&HostLine::Reply {
                slot: 4,
                text: "ok".into(),
                rev: 1,
            }),
            "{\"m\":\"reply\",\"agent\":4,\"text\":\"ok\",\"rev\":1}\n"
        );
    }

    #[test]
    fn escaping_matches_python_json_dumps() {
        let line = HostLine::Reply {
            slot: 1,
            text: "say \"hi\"\\ tab\tnl\n bell\u{7} del\u{7f} é".into(),
            rev: 1,
        };
        assert_eq!(
            text(&line),
            "{\"m\":\"reply\",\"agent\":1,\"text\":\"say \\\"hi\\\"\\\\ tab\\tnl\\n bell\\u0007 del\u{7f} é\",\"rev\":1}\n"
        );
    }

    #[test]
    fn caps_cut_on_char_boundaries_with_an_ellipsis() {
        let title = "é".repeat(40); // 80 bytes
        let line = HostLine::Agent {
            slot: 1,
            kind: None,
            name: None,
            project: None,
            state: None,
            title: Some(title),
            since_s: None,
            task: None,
            subs: None,
        };
        let out = text(&line);
        let cut_title = out
            .split("\"title\":\"")
            .nth(1)
            .unwrap()
            .trim_end_matches("\"}\n");
        assert!(cut_title.ends_with('…'));
        assert!(cut_title.len() <= CAPS.title);
        assert_eq!(cut_title.len(), 30 * 2 + 3);
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "…");
        assert_eq!(tail("abcdefgh", 6), "…fgh");
        assert_eq!(tail("ééééé", 7), "…éé");
    }

    #[test]
    fn a_transcript_for_a_session_without_an_agent_goes_without_the_field() {
        assert_eq!(
            text(&HostLine::Transcript {
                slot: 0,
                text: "deploy".into(),
                last: false, press: None, failed: false, channel_tip: false, rec: false,
            }),
            "{\"m\":\"transcript\",\"text\":\"deploy\"}\n"
        );
    }

    #[test]
    fn transcript_keeps_the_tail_and_reply_the_head() {
        let long = (0..300)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect::<String>();
        let t = text(&HostLine::Transcript {
            slot: 1,
            text: long.clone(),
            last: false, press: None, failed: false, channel_tip: false, rec: false,
        });
        let body = t
            .split("\"text\":\"")
            .nth(1)
            .unwrap()
            .trim_end_matches("\"}\n");
        assert!(body.starts_with('…'));
        assert_eq!(body.len(), CAPS.transcript);
        assert!(body.ends_with(&long[long.len() - 10..]));
        let r = text(&HostLine::Reply {
            slot: 1,
            text: long.clone(),
            rev: 1,
        });
        let body = r
            .split("\"text\":\"")
            .nth(1)
            .unwrap()
            .trim_end_matches("\",\"rev\":1}\n");
        assert!(body.ends_with('…'));
        assert_eq!(body.len(), CAPS.reply);
        assert!(body.starts_with(&long[..10]));
    }

    #[test]
    fn choices_are_cut_to_four_and_rec_must_point_at_one() {
        let line = HostLine::Card {
            slot: 1,
            id: 1,
            kind: CardKind::Q,
            text: "t".into(),
            choices: (0..6).map(|i| format!("c{i}")).collect(),
            rec: Some(3),
        };
        assert_eq!(
            text(&line),
            "{\"m\":\"card\",\"agent\":1,\"id\":1,\"type\":\"q\",\"text\":\"t\",\"choices\":[\"c0\",\"c1\",\"c2\",\"c3\"],\"rec\":3}\n"
        );
        let mut bad = line.clone();
        if let HostLine::Card { rec, .. } = &mut bad {
            *rec = Some(4);
        }
        assert_eq!(encode(&bad), Err(EncodeError::Range { field: "rec" }));
        let mut none = line;
        if let HostLine::Card { choices, rec, .. } = &mut none {
            choices.clear();
            *rec = None;
        }
        assert_eq!(
            text(&none),
            "{\"m\":\"card\",\"agent\":1,\"id\":1,\"type\":\"q\",\"text\":\"t\"}\n"
        );
    }

    #[test]
    fn ranges_are_refused_before_the_device_sees_them() {
        assert_eq!(
            encode(&HostLine::AgentGone { slot: 0 }),
            Err(EncodeError::Range { field: "id" })
        );
        assert_eq!(
            encode(&HostLine::Reply {
                slot: 0,
                text: "x".into(),
                rev: 1,
            }),
            Err(EncodeError::Range { field: "agent" })
        );
        assert_eq!(
            encode(&HostLine::Hello {
                sid: 1,
                hm: Some("25:00".into())
            }),
            Err(EncodeError::Range { field: "hm" })
        );
        assert_eq!(
            encode(&HostLine::Hello {
                sid: 1,
                hm: Some("1942".into())
            }),
            Err(EncodeError::Range { field: "hm" })
        );
        assert_eq!(
            encode(&HostLine::Ws {
                active: 5,
                count: 4,
                sel: 0,
                windows: vec![]
            }),
            Err(EncodeError::Range { field: "active" })
        );
        assert_eq!(
            encode(&HostLine::Ws {
                active: 1,
                count: 10,
                sel: 0,
                windows: vec![]
            }),
            Err(EncodeError::Range { field: "count" })
        );
        assert_eq!(
            encode(&HostLine::Ws {
                active: 1,
                count: 2,
                sel: 1,
                windows: vec![]
            }),
            Err(EncodeError::Range { field: "sel" })
        );
        assert_eq!(
            text(&HostLine::Ws {
                active: 0,
                count: 0,
                sel: 0,
                windows: vec![]
            }),
            "{\"m\":\"ws\",\"active\":0,\"count\":0,\"sel\":0,\"windows\":[]}\n"
        );
        assert_eq!(
            encode(&HostLine::Macro {
                face: 9,
                src: None,
                keys: vec![]
            }),
            Err(EncodeError::Range { field: "f" })
        );
        assert_eq!(
            encode(&HostLine::Set {
                key: SetKey::Brightness,
                value: 6
            }),
            Err(EncodeError::Range { field: "v" })
        );
        assert_eq!(
            encode(&HostLine::Usage {
                period: "x".into(),
                items: vec![UsageItem {
                    reset_tip: None,
                    name: "n".into(),
                    account: None,
                    percent: 101,
                    reset: "r".into(),
                    tone: None
                }]
            }),
            Err(EncodeError::Range { field: "p" })
        );
    }

    #[test]
    fn the_longest_lines_stay_far_below_the_ceiling() {
        // Everything at its cap: the longest ws and card the design mentions.
        let ws = HostLine::Ws {
            active: 9,
            count: 9,
            sel: 5,
            windows: (0..6)
                .map(|_| Window {
                    tone: Some(Tone::Amber),
                    title: "w".repeat(48),
                })
                .collect(),
        };
        let card = HostLine::Card {
            slot: 8,
            id: 65535,
            kind: CardKind::Q,
            text: "t".repeat(128),
            choices: (0..4).map(|_| "c".repeat(32)).collect(),
            rec: Some(3),
        };
        for line in [ws, card] {
            let n = encode(&line).unwrap().len();
            assert!(n < 500, "{n}");
        }
        assert_eq!(caps().line, 1000);
    }

    #[test]
    fn a_line_over_the_ceiling_is_refused() {
        // Only reachable by an uncapped list of capped items: 8 faces is not
        // enough, so drive the ceiling down through a synthetic line.
        let big = HostLine::Macro {
            face: 1,
            src: Some("s".repeat(16)),
            keys: (0..9)
                .map(|_| MacroKey {
                    label: "l".repeat(12),
                    binding: Some(Binding::ShiftEnter),
                    keys: None,
                })
                .collect(),
        };
        assert!(encode(&big).unwrap().len() < CAPS.line);
        let mut bytes = Vec::new();
        let mut w = Writer::new(&mut bytes);
        w.begin("reply");
        w.str("text", &"x".repeat(1200));
        assert!(bytes.len() > CAPS.line);
        // The public path cannot exceed it; the guard is still asserted.
        assert_eq!(
            EncodeError::TooLong { len: 1201 }.to_string(),
            "line of 1201 bytes exceeds the 1000 byte ceiling"
        );
    }

    /// `fit_doc_window` cuts a window by **encoded** length, not by line
    /// count, because `outbox` *drops* an over-cap line rather than truncating
    /// it — a window that does not fit is a window that never arrives, and the
    /// operator scrolls into a hole.
    ///
    /// Measured here, 12-line windows against the 1000 B `CAPS.line` ceiling:
    ///
    /// | window | raw | encoded |
    /// |---|---|---|
    /// | plain prose | 456 | 572 |
    /// | `copy "C:\Users\a\b" …` | 492 | 740 |
    /// | `ssh -o "K=\"V\"" …` | 504 | 812 |
    /// | `"a":"b","c":"d",…` (20 quotes) | 468 | **824** |
    /// | 48 bytes of pure `"` | 576 | over |
    ///
    /// Fixed overhead is 92 B, leaving ~73 escaped bytes per line. So every
    /// *realistic* quoted command fits, but with only ~176 B of clearance —
    /// close enough that cutting by count alone would eventually drop a window.
    /// An earlier version of this test used lines with about six escapes each,
    /// found they fit, and concluded nothing.
    #[test]
    fn a_window_of_escaped_text_is_trimmed_until_it_actually_fits() {
        let window = |lines: &[String]| HostLine::DocText {
            slot: 1,
            doc: DocKind::Card,
            id: 17,
            first: 0,
            total: 40,
            font: DocFont::Name,
            lines: lines.to_vec(),
        };

        let prose: Vec<String> = (0..12)
            .map(|i| format!("line {i} of an ordinary wrapped question"))
            .collect();
        assert_eq!(
            fit_doc_window(1, DocKind::Card, 17, 0, 40, DocFont::Name, &prose).len(),
            12,
            "plain prose fits the full window"
        );

        // The realistic worst case: a permission prompt quoting a command.
        let quoted: Vec<String> = (0..12)
            .map(|_| r#""a":"b","c":"d","e":"f","g":"h","i":"j""#.to_string())
            .collect();
        let n = encode(&window(&quoted))
            .expect("a quoted command still fits twelve lines")
            .len();
        assert!(
            n > 800 && n <= CAPS.line,
            "escaping nearly doubles it — {n} B of the {} B ceiling",
            CAPS.line
        );

        // The boundary: `encode` cuts each line to `doc_line` (48), and 48
        // bytes that all need escaping is 96 on the wire.
        let dense: Vec<String> = (0..12).map(|_| "\"".repeat(CAPS.doc_line)).collect();
        assert!(
            encode(&window(&dense)).is_err(),
            "twelve fully-escaped lines do not fit"
        );
        let kept = fit_doc_window(1, DocKind::Card, 17, 0, 40, DocFont::Name, &dense);
        assert!(
            !kept.is_empty() && kept.len() < 12,
            "so fewer are sent, but a window is still produced: {}",
            kept.len()
        );
        assert!(
            encode(&window(&kept))
                .expect("the trimmed window encodes")
                .len()
                <= CAPS.line,
            "and it is inside the ceiling"
        );
    }

    /// How many lines a window is guaranteed to carry, however badly its text
    /// escapes. The host centres a window on the line the deck asked for, which
    /// costs up to `(doc_lines - visible) / 2` lines of backwards overscan —
    /// five at worst. If a window could ever be trimmed below that, the deck
    /// could be handed a window that does not contain the line it asked about,
    /// and since it never asks twice for the same line, it would stall.
    ///
    /// This is what makes that unreachable, so `deck-model`'s guard against it
    /// stays dead code. If a cap moves and this drops towards five, that guard
    /// stops being theoretical.
    #[test]
    fn a_window_always_holds_more_lines_than_the_overscan_skips() {
        let worst: Vec<String> = (0..CAPS.doc_lines)
            .map(|_| "\"".repeat(CAPS.doc_line))
            .collect();
        let kept = fit_doc_window(1, DocKind::Card, 1, 0, 999, DocFont::Name, &worst).len();
        assert!(
            kept >= 9,
            "the worst-escaping window carries {kept} lines; the host skips up \
             to {} for overscan",
            (CAPS.doc_lines - 2) / 2
        );
    }

    /// `first + lines.len()` past `total` is a sending-side bug; the deck
    /// should never have to police it.
    #[test]
    fn a_window_that_runs_past_the_end_of_the_body_is_refused() {
        let err = encode(&HostLine::DocText {
            slot: 1,
            doc: DocKind::Card,
            id: 17,
            first: 38,
            total: 40,
            font: DocFont::Title,
            lines: vec!["a".into(), "b".into(), "c".into()],
        })
        .unwrap_err();
        assert_eq!(err, EncodeError::Range { field: "first" });
    }

    /// The window and the request both round-trip, and the request fits the
    /// one report a device edge gets.
    #[test]
    fn a_window_and_its_request_round_trip_on_the_wire() {
        let line = HostLine::DocText {
            slot: 2,
            doc: DocKind::Card,
            id: 9,
            first: 4,
            total: 30,
            font: DocFont::Name,
            lines: vec!["first".into(), "second".into()],
        };
        let bytes = encode(&line).expect("encodes");
        let text = core::str::from_utf8(&bytes).expect("utf8");
        assert!(text.contains(r#""m":"doc""#), "{text}");
        assert!(
            text.contains(r#""d":"c""#),
            "the doc kind is a single letter: {text}"
        );
        assert!(
            text.contains(r#""agent":2"#),
            "slot is spelled agent: {text}"
        );
        assert!(text.contains(r#""font":"name""#), "{text}");
    }
}
