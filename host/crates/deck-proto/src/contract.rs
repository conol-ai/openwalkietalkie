//! Design §14 "Wire contract", checked against the other end of the wire:
//! the firmware's own assembler, parser and event encoder (`fw`, mounted from
//! `src/deck`). Every `HostLine` this crate can emit survives framing and
//! reassembly at every chunk size and parses into the matching `HostMsg`
//! with the same enum spellings and the same text; every event the firmware
//! encodes parses into the matching `DeviceLine`; per-edge lines fit one
//! report; the deck-sim mockup session is byte-identical; and hostile input
//! never panics on either side.
use crate::fw::msg::{self, Event, HostMsg};
use crate::fw::proto as fwp;
use crate::fw::ui::UiState;
use crate::types::{BINDINGS, KINDS, STATES, TEMPLATES, TONES};
use crate::*;
use proptest::prelude::*;

// ---- the firmware side, as a daemon sees it ---------------------------------

fn fw_parse(bytes: &[u8]) -> Result<Box<HostMsg>, msg::ErrWhy> {
    let mut out = Box::new(HostMsg::None);
    fwp::parse(bytes, &mut out)?;
    Ok(out)
}

fn fw_encode(event: Event, ui: &UiState) -> Vec<u8> {
    let mut out = [0; fwp::TX_MAX];
    let n = fwp::encode(event, ui, &mut out);
    assert!(n > 0, "{event:?} did not fit");
    out[..n].to_vec()
}

fn ui() -> Box<UiState> {
    let mut ui = Box::new(UiState::new());
    ui.reset();
    ui
}

/// `line` (its LF included) as report bodies of `chunk` payload bytes, zero
/// padded to 64 like a real HID report.
fn reports(line: &[u8], chunk: usize) -> Vec<Vec<u8>> {
    line.chunks(chunk.clamp(1, PAYLOAD_MAX))
        .map(|piece| {
            let mut r = vec![REPORT_ID, CHANNEL, piece.len() as u8];
            r.extend_from_slice(piece);
            r.resize(REPORT_LEN, 0);
            r
        })
        .collect()
}

/// The lines the firmware's assembler makes of `stream`.
fn fw_assemble(stream: &[Vec<u8>]) -> Vec<Result<Vec<u8>, ()>> {
    let mut assembler = fwp::LineAssembler::new();
    let mut got = Vec::new();
    for report in stream {
        assembler.push(report, &mut |l| {
            got.push(l.map(<[u8]>::to_vec).map_err(|_| ()))
        });
    }
    got
}

/// The lines this crate's assembler makes of `stream`.
fn host_assemble(stream: &[Vec<u8>]) -> Vec<Result<Vec<u8>, ()>> {
    let mut assembler = LineAssembler::new();
    let mut got = Vec::new();
    for report in stream {
        assembler.push(report, &mut |l| {
            got.push(l.map(<[u8]>::to_vec).map_err(|_| ()))
        });
    }
    got
}

fn stripped(line: &[u8]) -> Vec<u8> {
    line.strip_suffix(b"\n").unwrap_or(line).to_vec()
}

// ---- vocabulary mapping: this crate's members ↔ the firmware's -------------

fn fw_kind(k: Kind) -> msg::AgentKind {
    match k {
        Kind::Claude => msg::AgentKind::Claude,
        Kind::Codex => msg::AgentKind::Codex,
        Kind::Gemini => msg::AgentKind::Gemini,
        Kind::Other => msg::AgentKind::Other,
        Kind::Conol => msg::AgentKind::Conol,
    }
}

fn fw_state(s: AgentState) -> msg::AgentState {
    match s {
        AgentState::Working => msg::AgentState::Working,
        AgentState::Question => msg::AgentState::Question,
        AgentState::Ready => msg::AgentState::Ready,
        AgentState::Error => msg::AgentState::Error,
        AgentState::Idle => msg::AgentState::Idle,
    }
}

fn fw_card_kind(k: CardKind) -> msg::CardKind {
    match k {
        CardKind::Q => msg::CardKind::Question,
        CardKind::R => msg::CardKind::Ready,
    }
}

fn fw_tone(t: Tone) -> msg::Tone {
    match t {
        Tone::Blue => msg::Tone::Blue,
        Tone::Amber => msg::Tone::Amber,
        Tone::Green => msg::Tone::Green,
        Tone::Red => msg::Tone::Red,
        Tone::Gray => msg::Tone::Gray,
    }
}

fn fw_template(t: Template) -> msg::Template {
    match t {
        Template::Agents => msg::Template::Agents,
        Template::Omarchy => msg::Template::Omarchy,
        Template::Macropad => msg::Template::Macropad,
        Template::Usage => msg::Template::Usage,
        Template::Settings => msg::Template::Settings,
        Template::Arcade => msg::Template::Arcade,
        Template::System => msg::Template::System,
    }
}

fn fw_binding(b: Option<Binding>) -> msg::Binding {
    match b {
        None => msg::Binding::None,
        Some(Binding::Left) => msg::Binding::Left,
        Some(Binding::Right) => msg::Binding::Right,
        Some(Binding::Enter) => msg::Binding::Enter,
        Some(Binding::Cw) => msg::Binding::Cw,
        Some(Binding::Ccw) => msg::Binding::Ccw,
        Some(Binding::ShiftLeft) => msg::Binding::ShiftLeft,
        Some(Binding::ShiftRight) => msg::Binding::ShiftRight,
        Some(Binding::ShiftEnter) => msg::Binding::ShiftEnter,
    }
}

fn fw_set_key(k: SetKey) -> msg::SettingKey {
    match k {
        SetKey::Speaker => msg::SettingKey::Speaker,
        SetKey::Mic => msg::SettingKey::Mic,
        SetKey::Announce => msg::SettingKey::Announce,
        SetKey::Chime => msg::SettingKey::Chime,
        SetKey::Autojump => msg::SettingKey::AutoJump,
        SetKey::Brightness => msg::SettingKey::Brightness,
        SetKey::Idlehome => msg::SettingKey::IdleHome,
        SetKey::Demo => msg::SettingKey::Demo,
        SetKey::Textsize => msg::SettingKey::TextSize,
        SetKey::Arcadesound => msg::SettingKey::ArcadeSound,
    }
}

fn fw_key_id(k: KeyId) -> msg::KeyId {
    match k {
        KeyId::Enter => msg::KeyId::Enter,
        KeyId::Cancel => msg::KeyId::Cancel,
        KeyId::Left => msg::KeyId::Left,
        KeyId::Right => msg::KeyId::Right,
        KeyId::Cw => msg::KeyId::Cw,
        KeyId::Ccw => msg::KeyId::Ccw,
    }
}

fn fw_ptt_ctx(c: PttCtx) -> msg::PttCtx {
    match c {
        PttCtx::Talk => msg::PttCtx::Talk,
        PttCtx::Answer => msg::PttCtx::Answer,
        PttCtx::Name => msg::PttCtx::Name,
    }
}

fn fw_err_why(w: ErrWhy) -> msg::ErrWhy {
    match w {
        ErrWhy::Size => msg::ErrWhy::Size,
        ErrWhy::Parse => msg::ErrWhy::Parse,
        ErrWhy::Range => msg::ErrWhy::Range,
        ErrWhy::Unknown => msg::ErrWhy::Unknown,
    }
}

const ALL_KEY_IDS: [KeyId; 6] = [
    KeyId::Enter,
    KeyId::Cancel,
    KeyId::Left,
    KeyId::Right,
    KeyId::Cw,
    KeyId::Ccw,
];
const ALL_CTXS: [PttCtx; 3] = [PttCtx::Talk, PttCtx::Answer, PttCtx::Name];
const ALL_WHYS: [ErrWhy; 4] = [ErrWhy::Size, ErrWhy::Parse, ErrWhy::Range, ErrWhy::Unknown];

fn minute_of_day(hm: &str) -> u16 {
    let (h, m) = hm.split_once(':').unwrap();
    h.parse::<u16>().unwrap() * 60 + m.parse::<u16>().unwrap()
}

/// What the device keeps of a host string: C0 controls and DEL become a
/// space (P:59-60); the host escapes them, the device launders them.
fn laundered(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect()
}

/// `mine` (already capped: the result of parsing the encoded line back) and
/// the firmware's reading of the same bytes describe the same message.
fn assert_same(mine: &HostLine, fw: &HostMsg) {
    match (mine, fw) {
        (
            HostLine::Hello { sid, hm },
            HostMsg::Hello {
                v,
                sid: fw_sid,
                minute_of_day: fw_hm,
            },
        ) => {
            assert_eq!(*v, VERSION);
            assert_eq!(fw_sid, sid);
            assert_eq!(*fw_hm, hm.as_deref().map(minute_of_day));
        }
        (HostLine::Synced, HostMsg::Synced) => {}
        (HostLine::System(s), HostMsg::System(f)) => {
            assert_eq!((s.cpu, s.gpu, s.memory_used_mib, s.memory_total_mib,
                s.disk_free_mib, s.disk_total_mib, s.sample),
                (f.cpu, f.gpu, f.memory_used_mib, f.memory_total_mib,
                f.disk_free_mib, f.disk_total_mib, f.sample));
        }
        (
            HostLine::Agent {
                slot,
                kind,
                name,
                project,
                state,
                title,
                since_s,
            },
            HostMsg::Agent {
                id,
                gone: false,
                kind: fw_k,
                state: fw_s,
                name: fw_name,
                project: fw_project,
                title: fw_title,
                since_s: fw_since,
            },
        ) => {
            assert_eq!(id, slot);
            assert_eq!(*fw_k, kind.map(fw_kind));
            assert_eq!(*fw_s, state.map(fw_state));
            assert_eq!(
                fw_name.as_ref().map(|t| t.as_str().to_string()),
                name.as_deref().map(laundered)
            );
            assert_eq!(
                fw_project.as_ref().map(|t| t.as_str().to_string()),
                project.as_deref().map(laundered)
            );
            assert_eq!(
                fw_title.as_ref().map(|t| t.as_str().to_string()),
                title.as_deref().map(laundered)
            );
            assert_eq!(fw_since, since_s);
        }
        (HostLine::AgentGone { slot }, HostMsg::Agent { id, gone: true, .. }) => {
            assert_eq!(id, slot);
        }
        (
            HostLine::Card {
                slot,
                id,
                kind,
                text,
                choices,
                rec,
            },
            HostMsg::Card {
                agent,
                id: fw_id,
                clear: false,
                kind: fw_k,
                text: fw_text,
                choices: fw_choices,
                choice_count,
                recommended,
            },
        ) => {
            assert_eq!(agent, slot);
            assert_eq!(fw_id, id);
            assert_eq!(*fw_k, fw_card_kind(*kind));
            assert_eq!(fw_text.as_str(), laundered(text));
            let shown: Vec<String> = fw_choices[..usize::from(*choice_count)]
                .iter()
                .map(|c| c.as_str().to_string())
                .collect();
            assert_eq!(
                shown,
                choices.iter().map(|c| laundered(c)).collect::<Vec<_>>()
            );
            assert_eq!(recommended, rec);
        }
        (
            HostLine::CardClear { slot, id },
            HostMsg::Card {
                agent,
                id: fw_id,
                clear: true,
                ..
            },
        ) => {
            assert_eq!(agent, slot);
            assert_eq!(fw_id, id);
        }
        (
            HostLine::Reply { slot, text, rev: 1 },
            HostMsg::Reply {
                agent,
                text: fw_text,
                ..
            },
        ) => {
            assert_eq!(agent, slot);
            assert_eq!(fw_text.as_str(), laundered(text));
        }
        (
            HostLine::Transcript { slot, text, last, press, failed, channel_tip },
            HostMsg::Transcript {
                agent,
                text: fw_text,
                last: fw_last,
                press: fw_press,
                failed: fw_failed,
                channel_tip: fw_channel_tip,
            },
        ) => {
            assert_eq!(agent, slot);
            assert_eq!(fw_text.as_str(), laundered(text));
            assert_eq!(fw_last, last);
            assert_eq!(fw_press, press);
            assert_eq!(fw_failed, failed);
            assert_eq!(fw_channel_tip, channel_tip);
        }
        (HostLine::Focus { slot, ack }, HostMsg::Focus { agent, ack: fw_ack }) => {
            assert_eq!(agent, slot);
            assert_eq!(fw_ack, ack);
        }
        (
            HostLine::Ws {
                active,
                count,
                sel,
                windows,
            },
            HostMsg::Workspaces {
                active: fw_active,
                count: fw_count,
                selected,
                windows: fw_windows,
                window_count,
            },
        ) => {
            assert_eq!(fw_active, active);
            assert_eq!(fw_count, count);
            assert_eq!(selected, sel);
            assert_eq!(usize::from(*window_count), windows.len());
            for (fw_w, w) in fw_windows.iter().zip(windows) {
                assert_eq!(fw_w.tone, fw_tone(w.tone.unwrap_or(Tone::Gray)));
                assert_eq!(fw_w.title.as_str(), laundered(&w.title));
            }
        }
        (
            HostLine::Macro { face, src, keys },
            HostMsg::Macro {
                face: fw_face,
                source,
                keys: fw_keys,
            },
        ) => {
            assert_eq!(fw_face, face);
            assert_eq!(
                source.as_ref().map(|t| t.as_str().to_string()),
                src.as_deref().map(laundered)
            );
            for (index, fw_key) in fw_keys.iter().enumerate() {
                match keys.get(index) {
                    Some(key) => {
                        assert!(fw_key.present);
                        assert_eq!(fw_key.label.as_str(), laundered(&key.label));
                        assert_eq!(fw_key.binding, fw_binding(key.binding));
                    }
                    None => assert!(!fw_key.present),
                }
            }
        }
        (
            HostLine::Usage { period, items },
            HostMsg::Usage {
                period: fw_period,
                items: fw_items,
                count,
            },
        ) => {
            assert_eq!(fw_period.as_str(), laundered(period));
            assert_eq!(usize::from(*count), items.len());
            for (fw_item, item) in fw_items.iter().zip(items) {
                assert_eq!(fw_item.name.as_str(), laundered(&item.name));
                assert_eq!(fw_item.account.as_str(), laundered(item.account.as_deref().unwrap_or_default()));
                assert_eq!(fw_item.percent, item.percent);
                assert_eq!(fw_item.reset.as_str(), laundered(&item.reset));
                assert_eq!(fw_item.reset_tip.as_str(), laundered(item.reset_tip.as_deref().unwrap_or_default()));
                assert_eq!(fw_item.tone, item.tone.map(fw_tone));
            }
        }
        (
            HostLine::Faces { list },
            HostMsg::Faces {
                list: fw_list,
                count,
            },
        ) => {
            assert_eq!(usize::from(*count), list.len());
            for (fw_face, face) in fw_list.iter().zip(list) {
                assert_eq!(fw_face.id, face.f);
                assert_eq!(fw_face.template, fw_template(face.t));
                assert_eq!(fw_face.name.as_str(), laundered(&face.n));
            }
        }
        (HostLine::Name { text }, HostMsg::Name { text: fw_text }) => {
            assert_eq!(fw_text.as_str(), laundered(text));
        }
        (
            HostLine::Set { key, value },
            HostMsg::Set {
                key: fw_key,
                value: fw_value,
            },
        ) => {
            assert_eq!(*fw_key, fw_set_key(*key));
            assert_eq!(fw_value, value);
        }
        _ => panic!("{mine:?} read by the firmware as {fw:?}"),
    }
}

// ---- corpora -----------------------------------------------------------------

const ALL_TONES: [Tone; 5] = [Tone::Blue, Tone::Amber, Tone::Green, Tone::Red, Tone::Gray];
const ALL_TEMPLATES: [Template; 7] = [
    Template::Agents,
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::Settings,
    Template::Arcade,
    Template::System,
];
const ALL_BINDINGS: [Binding; 8] = [
    Binding::Left,
    Binding::Right,
    Binding::Enter,
    Binding::Cw,
    Binding::Ccw,
    Binding::ShiftLeft,
    Binding::ShiftRight,
    Binding::ShiftEnter,
];

fn default_faces() -> Vec<FaceEntry> {
    fw::ui::DEFAULT_STRIP.iter().map(|t| FaceEntry {
        f: t.default_id(), t: ALL_TEMPLATES.into_iter().find(|h| fw_template(*h) == *t).unwrap(), n: t.label().into(),
    }).collect()
}

/// Every `HostLine` variant, the protocol's examples, every enum member,
/// escapes, and strings over every cap in 1-, 2-, 3- and 4-byte characters.
fn corpus() -> Vec<HostLine> {
    let long = "é…😀 ".repeat(60);
    let mut lines = vec![
        HostLine::Hello {
            sid: 314159,
            hm: Some("19:42".into()),
        },
        HostLine::Hello {
            sid: u32::MAX,
            hm: None,
        },
        HostLine::Synced,
        HostLine::System(SystemStatus::EMPTY),
        HostLine::System(SystemStatus { cpu: Some(100), gpu: Some(0),
            memory_used_mib: 32_000, memory_total_mib: 65_536,
            disk_free_mib: u32::MAX, disk_total_mib: u32::MAX, sample: u32::MAX }),
        HostLine::Agent {
            slot: 1,
            kind: Some(Kind::Codex),
            name: Some("cohdl-api".into()),
            project: Some("cohdl".into()),
            state: Some(AgentState::Question),
            title: Some("Run migrations on staging?".into()),
            since_s: Some(120),
        },
        HostLine::Agent {
            slot: 8,
            kind: Some(Kind::Other),
            name: Some(long.clone()),
            project: Some(long.clone()),
            state: Some(AgentState::Error),
            title: Some(long.clone()),
            since_s: Some(u32::MAX),
        },
        HostLine::Agent {
            slot: 3,
            kind: None,
            name: None,
            project: None,
            state: None,
            title: Some("Compiling openmicro ⠋".into()),
            since_s: None,
        },
        HostLine::AgentGone { slot: 2 },
        HostLine::Card {
            slot: 1,
            id: 1,
            kind: CardKind::Q,
            text: "Tests fail on main. Run pending migrations on staging first?".into(),
            choices: vec![
                "Run on staging".into(),
                "Skip migrations".into(),
                "Abort task".into(),
            ],
            rec: Some(0),
        },
        HostLine::Card {
            slot: 8,
            id: u16::MAX,
            kind: CardKind::R,
            text: long.clone(),
            choices: (0..6).map(|i| format!("{i} {long}")).collect(),
            rec: Some(3),
        },
        HostLine::Card {
            slot: 2,
            id: 2,
            kind: CardKind::R,
            text: "say \"hi\"\\ tab\tnl\n bell\u{7} del\u{7f} é".into(),
            choices: vec![],
            rec: None,
        },
        HostLine::CardClear { slot: 1, id: 1 },
        HostLine::Reply {
            slot: 1,
            text: long.clone(),
            rev: 1,
        },
        HostLine::Transcript {
            slot: 4,
            text: long.clone(),
            last: false, press: None, failed: false, channel_tip: false,
        },
        HostLine::Transcript {
            slot: 4,
            text: "Refactor the auth module".into(),
            last: true, press: None, failed: false, channel_tip: false,
        },
        HostLine::Focus {
            slot: 2,
            ack: Some(7),
        },
        HostLine::Focus { slot: 2, ack: None },
        HostLine::Ws {
            active: 2,
            count: 4,
            sel: 0,
            windows: vec![
                Window {
                    tone: Some(Tone::Blue),
                    title: "term · claude — header refactor".into(),
                },
                Window {
                    tone: None,
                    title: long.clone(),
                },
            ],
        },
        HostLine::Ws {
            active: 9,
            count: 9,
            sel: 5,
            windows: (0..8)
                .map(|i| Window {
                    tone: Some(ALL_TONES[i % 5]),
                    title: format!("w{i} {long}"),
                })
                .collect(),
        },
        HostLine::Ws {
            active: 0,
            count: 0,
            sel: 0,
            windows: vec![],
        },
        HostLine::Macro {
            face: 3,
            src: Some("deck.toml".into()),
            keys: vec![
                MacroKey {
                    label: "build".into(),
                    binding: Some(Binding::Left),
                    keys: None,
                },
                MacroKey {
                    label: "test".into(),
                    binding: Some(Binding::Right),
                    keys: None,
                },
                MacroKey {
                    label: "screenshot".into(),
                    binding: None,
                    keys: None,
                },
            ],
        },
        HostLine::Macro {
            face: 8,
            src: Some(long.clone()),
            keys: (0..11)
                .map(|i| MacroKey {
                    label: format!("{i} {long}"),
                    binding: Some(ALL_BINDINGS[i % 8]),
                    keys: None,
                })
                .collect(),
        },
        HostLine::Macro {
            face: 1,
            src: None,
            keys: vec![],
        },
        HostLine::Usage {
            period: "SEP 2026".into(),
            items: vec![
                UsageItem {
                    reset_tip: Some("2 resets expire in 1d 6h".into()),
                    name: "Claude Pro".into(),
                    account: Some("personal@example.com".into()),
                    percent: 62,
                    reset: "resets in 4 days".into(),
                    tone: None,
                },
                UsageItem {
                    reset_tip: None,
                    name: "Codex".into(),
                    account: None,
                    percent: 100,
                    reset: "tomorrow".into(),
                    tone: Some(Tone::Red),
                },
            ],
        },
        HostLine::Usage {
            period: long.clone(),
            items: (0..7)
                .map(|i| UsageItem {
                    reset_tip: None,
                    name: format!("{i} {long}"),
                    account: Some(long.clone()),
                    percent: 100,
                    reset: long.clone(),
                    tone: Some(ALL_TONES[i % 5]),
                })
                .collect(),
        },
        HostLine::Faces {
            list: default_faces(),
        },
        HostLine::Faces {
            // Nine entries: the ninth is cut away before its duplicate id matters.
            list: (0..9)
                .map(|i| FaceEntry {
                    f: (i % 8) as u8 + 1,
                    t: ALL_TEMPLATES[i % ALL_TEMPLATES.len()],
                    n: format!("{i} {long}"),
                })
                .collect(),
        },
        HostLine::Faces { list: vec![] },
        HostLine::Name { text: long.clone() },
        HostLine::Name {
            text: "Deploy".into(),
        },
    ];
    for key in SetKey::ALL {
        let value = if key == SetKey::Brightness { 5 } else { 1 };
        lines.push(HostLine::Set { key, value });
    }
    lines
}

/// Every device event the firmware can encode, with the `DeviceLine` a
/// daemon must read from it.
fn device_corpus(ui: &UiState) -> Vec<(Vec<u8>, DeviceLine)> {
    let mut cases: Vec<(Event, DeviceLine)> = vec![
        (Event::Up, DeviceLine::Up),
        (
            Event::Refresh,
            DeviceLine::Refresh {
                what: "usage".into(),
            },
        ),
        (
            Event::Channel {
                agent: 8,
                seq: 65535,
            },
            DeviceLine::Channel {
                slot: 8,
                seq: 65535,
            },
        ),
        (
            Event::Answer {
                agent: 8,
                card: 65535,
                choice: 3,
            },
            DeviceLine::Answer {
                slot: 8,
                card: 65535,
                choice: 3,
            },
        ),
        (
            Event::Snooze {
                agent: 8,
                card: 65535,
            },
            DeviceLine::Snooze {
                slot: 8,
                card: 65535,
            },
        ),
        (
            Event::Macro {
                face: 8,
                index: 8,
                act: 2,
            },
            DeviceLine::Macro { f: 8, i: 8, act: 2 },
        ),
        (Event::WsGo(9), DeviceLine::WsGo(9)),
        (Event::Tool { agent: 8, id: 65535, index: 63 }, DeviceLine::Tool { slot: 8, id: 65535, index: 63 }),
        (Event::WsWin(5), DeviceLine::WsWin(5)),
        (Event::Face { id: 8 }, DeviceLine::Face { f: 8 }),
    ];
    for k in ALL_KEY_IDS {
        for (act, shift) in [(1, false), (0, true), (2, true)] {
            cases.push((
                Event::Key {
                    key: fw_key_id(k),
                    act,
                    shift,
                    face: 8,
                },
                DeviceLine::Key {
                    k,
                    act,
                    shift,
                    f: 8,
                },
            ));
        }
    }
    for ctx in ALL_CTXS {
        for (act, agent, card, abort) in [(1, 8, 0, false), (0, 8, 65535, false), (0, 0, 7, true)] {
            cases.push((
                Event::Ptt {
                    act,
                    agent,
                    ctx: fw_ptt_ctx(ctx),
                    card,
                    abort, press: 0,
                },
                DeviceLine::Ptt {
                    act,
                    slot: agent,
                    ctx,
                    card: (card != 0).then_some(card),
                    abort, press: None,
                },
            ));
        }
    }
    for key in SetKey::ALL {
        cases.push((
            Event::Set {
                key: fw_set_key(key),
                value: 65535,
            },
            DeviceLine::Set { key, value: 65535 },
        ));
    }
    for why in ALL_WHYS {
        cases.push((
            Event::Err {
                why: fw_err_why(why),
            },
            DeviceLine::Err { why },
        ));
    }
    let faces = ui.global.strip[..usize::from(ui.global.strip_len)]
        .iter()
        .map(|e| FaceEntry {
            f: e.id,
            t: ALL_TEMPLATES
                .into_iter()
                .find(|t| fw_template(*t) == e.template)
                .unwrap(),
            n: e.name.as_str().to_string(),
        })
        .collect::<Vec<_>>();
    cases.push((
        Event::Hello,
        DeviceLine::Hello {
            v: VERSION,
            fw: msg::FW_VERSION.into(),
            sid: ui.deck.held_sid(),
            boot: ui.still_default(),
            rx: LINE_MAX as u16,
            faces: faces.clone(),
        },
    ));
    cases.push((Event::Faces, DeviceLine::Faces { list: faces }));
    cases
        .into_iter()
        .map(|(event, line)| (fw_encode(event, ui), line))
        .collect()
}

/// The device with every strip slot named, quotes and all: the longest hello.
fn full_strip_ui() -> Box<UiState> {
    let mut ui = ui();
    for (index, entry) in ui.global.strip.iter_mut().enumerate() {
        entry.id = index as u8 + 1;
        entry.template = fw_template(ALL_TEMPLATES[index % ALL_TEMPLATES.len()]);
        entry
            .name
            .set("\"\\\u{1F600}é\t\"\"\"\"\"\"\"\"\"\"\"\"\"\"");
    }
    ui.global.strip_len = 8;
    ui
}

// ---- host → device -----------------------------------------------------------

#[test]
fn a_tagged_press_round_trips_through_both_sides_and_matches_its_result() {
    let ui = ui();
    for press in [1, u16::MAX] {
        for act in [0, 1] {
            let json = fw_encode(Event::Ptt { act, agent: 8, ctx: msg::PttCtx::Answer, card: u16::MAX, abort: false, press }, &ui);
            assert!(json.len() < 2 * PAYLOAD_MAX);
            assert!(matches!(parse_device_line(&json), Ok(DeviceLine::Ptt { press: Some(p), .. }) if p == press));
        }
        for failed in [false, true] {
            let result = HostLine::Transcript { slot: 8, text: "complete".into(), last: true, press: Some(press), failed, channel_tip: true };
            let bytes = encode(&result).unwrap();
            assert_same(&result, &fw_parse(&stripped(&bytes)).unwrap());
            assert_eq!(parse_host_line(&stripped(&bytes)).unwrap(), result);
        }
    }
}

#[test]
fn every_host_line_is_read_back_by_the_firmware_as_sent() {
    for line in corpus() {
        let bytes = encode(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        assert!(bytes.len() <= caps().line, "{line:?}");
        assert_eq!(bytes.last(), Some(&b'\n'));
        let capped = parse_host_line(&stripped(&bytes)).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        assert_eq!(capped.m(), line.m());
        let fw = fw_parse(&stripped(&bytes)).unwrap_or_else(|why| panic!("{line:?}: {why:?}"));
        assert_same(&capped, &fw);
        // A capped string is a prefix of the original (transcript: a suffix)
        // ending in the ellipsis, or the original itself.
        if let (HostLine::Reply { text, .. }, HostLine::Reply { text: cut, .. }) = (&line, &capped)
        {
            assert!(cut == text || (cut.ends_with('…') && text.starts_with(&cut[..cut.len() - 3])));
        }
        if let (HostLine::Transcript { text, .. }, HostLine::Transcript { text: cut, .. }) =
            (&line, &capped)
        {
            assert!(cut == text || (cut.starts_with('…') && text.ends_with(&cut[3..])));
        }
    }
}

#[test]
fn every_host_line_survives_the_firmware_assembler_at_every_chunk_size() {
    let corpus = corpus();
    for chunk in [1, 2, 7, 16, 60, PAYLOAD_MAX] {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for line in &corpus {
            let bytes = encode(line).unwrap();
            stream.extend(reports(&bytes, chunk));
            expected.push(Ok(stripped(&bytes)));
        }
        let got = fw_assemble(&stream);
        assert_eq!(got, expected, "chunk {chunk}");
        assert_eq!(host_assemble(&stream), expected, "chunk {chunk}");
        for line in got {
            assert_ne!(*fw_parse(&line.unwrap()).unwrap(), HostMsg::None);
        }
    }
}

#[test]
fn the_chunker_output_reassembles_and_is_drained_line_by_line() {
    let mut chunker = Chunker::new();
    let mut expected = Vec::new();
    let mut reports_expected = 0;
    for line in corpus() {
        let bytes = encode(&line).unwrap();
        reports_expected += bytes.len().div_ceil(PAYLOAD_MAX);
        chunker.push_line(&bytes);
        expected.push(Ok(stripped(&bytes)));
    }
    assert_eq!(chunker.pending_reports(), reports_expected);
    let mut stream = Vec::new();
    let mut lines_seen = 0;
    while let Some(report) = chunker.next_report() {
        // Fragment atomicity: a report either ends a line or is followed by
        // more of the same line, never by another line's start.
        let ends_line = report[3..3 + usize::from(report[2])].contains(&b'\n');
        assert_eq!(chunker.in_flight(), !ends_line);
        lines_seen += usize::from(ends_line);
        assert_eq!(chunker.pending_lines(), expected.len() - lines_seen);
        stream.push(report.to_vec());
    }
    assert_eq!(stream.len(), reports_expected);
    assert_eq!(fw_assemble(&stream), expected);
    assert_eq!(host_assemble(&stream), expected);
}

#[test]
fn every_enum_spelling_matches_the_firmware() {
    fn agent(kind: Option<Kind>, state: Option<AgentState>) -> HostLine {
        HostLine::Agent {
            slot: 1,
            kind,
            name: None,
            project: None,
            state,
            title: None,
            since_s: None,
        }
    }
    let read = |line: &HostLine| *fw_parse(&stripped(&encode(line).unwrap())).unwrap();
    for (_, kind) in KINDS {
        assert!(
            matches!(read(&agent(Some(*kind), None)), HostMsg::Agent { kind: Some(k), .. } if k == fw_kind(*kind))
        );
    }
    for (_, state) in STATES {
        assert!(
            matches!(read(&agent(None, Some(*state))), HostMsg::Agent { state: Some(s), .. } if s == fw_state(*state))
        );
    }
    for kind in [CardKind::Q, CardKind::R] {
        let line = HostLine::Card {
            slot: 1,
            id: 1,
            kind,
            text: "t".into(),
            choices: vec![],
            rec: None,
        };
        assert!(matches!(read(&line), HostMsg::Card { kind: k, .. } if k == fw_card_kind(kind)));
    }
    for (_, tone) in TONES {
        let ws = HostLine::Ws {
            active: 1,
            count: 1,
            sel: 0,
            windows: vec![Window {
                tone: Some(*tone),
                title: "w".into(),
            }],
        };
        assert!(
            matches!(read(&ws), HostMsg::Workspaces { windows, .. } if windows[0].tone == fw_tone(*tone))
        );
        let usage = HostLine::Usage {
            period: "p".into(),
            items: vec![UsageItem {
                reset_tip: None,
                name: "n".into(),
                account: None,
                percent: 1,
                reset: "r".into(),
                tone: Some(*tone),
            }],
        };
        assert!(
            matches!(read(&usage), HostMsg::Usage { items, .. } if items[0].tone == Some(fw_tone(*tone)))
        );
    }
    for (_, template) in TEMPLATES {
        let faces = HostLine::Faces {
            list: vec![FaceEntry {
                f: 1,
                t: *template,
                n: "n".into(),
            }],
        };
        assert!(
            matches!(read(&faces), HostMsg::Faces { list, .. } if list[0].template == fw_template(*template))
        );
    }
    for (_, binding) in BINDINGS {
        let line = HostLine::Macro {
            face: 1,
            src: None,
            keys: vec![MacroKey {
                label: "l".into(),
                binding: Some(*binding),
                keys: None,
            }],
        };
        assert!(
            matches!(read(&line), HostMsg::Macro { keys, .. } if keys[0].binding == fw_binding(Some(*binding)))
        );
    }
    for key in SetKey::ALL {
        let line = HostLine::Set { key, value: 1 };
        assert_eq!(
            read(&line),
            HostMsg::Set {
                key: fw_set_key(key),
                value: 1
            }
        );
    }
    // Device → host: what the firmware writes for each member is what this
    // crate reads as the same member.
    let ui = ui();
    for k in ALL_KEY_IDS {
        let json = fw_encode(
            Event::Key {
                key: fw_key_id(k),
                act: 1,
                shift: false,
                face: 1,
            },
            &ui,
        );
        assert!(matches!(parse_device_line(&json), Ok(DeviceLine::Key { k: got, .. }) if got == k));
    }
    for ctx in ALL_CTXS {
        let json = fw_encode(
            Event::Ptt {
                act: 1,
                agent: 1,
                ctx: fw_ptt_ctx(ctx),
                card: 0,
                abort: false, press: 0,
            },
            &ui,
        );
        assert!(
            matches!(parse_device_line(&json), Ok(DeviceLine::Ptt { ctx: got, .. }) if got == ctx)
        );
    }
    for why in ALL_WHYS {
        let json = fw_encode(
            Event::Err {
                why: fw_err_why(why),
            },
            &ui,
        );
        assert_eq!(parse_device_line(&json), Ok(DeviceLine::Err { why }));
    }
    for key in SetKey::ALL {
        let json = fw_encode(
            Event::Set {
                key: fw_set_key(key),
                value: 3,
            },
            &ui,
        );
        assert_eq!(
            parse_device_line(&json),
            Ok(DeviceLine::Set { key, value: 3 })
        );
    }
    let ui = full_strip_ui();
    let json = fw_encode(Event::Faces, &ui);
    let Ok(DeviceLine::Faces { list }) = parse_device_line(&json) else {
        panic!("{json:?}")
    };
    assert_eq!(list.len(), 8);
    for (entry, template) in list.iter().zip(ALL_TEMPLATES.iter().cycle()) {
        assert_eq!(entry.t, *template);
    }
}

#[test]
fn the_deck_sim_mockup_session_is_byte_identical() {
    // `tools/deck-sim.py --dry-run --sid 314159 --hm 19:42 --flood 3 --send …`,
    // its `-> ` lines verbatim (json.dumps compact, raw UTF-8).
    const MOCKUP: &[&str] = &[
        r#"{"m":"hello","v":1,"sid":314159,"hm":"19:42"}"#,
        r#"{"m":"agent","id":1,"kind":"codex","name":"cohdl-api","proj":"cohdl","state":"question","title":"Run migrations on staging?","since":120}"#,
        r#"{"m":"card","agent":1,"id":1,"type":"q","text":"Tests fail on main. Run pending migrations on staging first?","choices":["Run on staging","Skip migrations","Abort task"],"rec":0}"#,
        r#"{"m":"reply","agent":1,"text":"Tests fail on main: 3 pending migrations are not applied on staging.","rev":1}"#,
        r#"{"m":"agent","id":2,"kind":"claude","name":"web-ui","state":"ready","title":"Header refactor done · 34 files"}"#,
        r#"{"m":"card","agent":2,"id":2,"type":"r","text":"Header refactor done · 34 files","choices":["Run the test suite","Open a pull request"]}"#,
        r#"{"m":"reply","agent":2,"text":"Header refactor done, 34 files changed","rev":1}"#,
        r#"{"m":"agent","id":3,"kind":"codex","name":"firmware","state":"working","title":"Compiling openmicro","since":120}"#,
        r#"{"m":"reply","agent":3,"text":"Building the native image for both boards.","rev":1}"#,
        r#"{"m":"agent","id":4,"kind":"gemini","name":"research","state":"idle","title":"Idle"}"#,
        r#"{"m":"agent","id":5,"kind":"claude","name":"infra","state":"error","title":"Deploy failed · exit 1"}"#,
        r#"{"m":"reply","agent":5,"text":"terraform apply exited with status 1.","rev":1}"#,
        r#"{"m":"ws","active":2,"count":4,"sel":0,"windows":[{"c":"blue","t":"term · claude — header refactor"},{"c":"amber","t":"firefox — github · pulls"},{"c":"green","t":"term · codex — compiling..."}]}"#,
        r#"{"m":"macro","f":3,"src":"deck.toml","keys":[{"l":"build","b":"left"},{"l":"test","b":"right"},{"l":"deploy","b":"enter"},{"l":"git push","b":"shift+left"},{"l":"screenshot"},{"l":"lock"},{"l":"mute","b":"shift+right"},{"l":"term","b":"shift+enter"}]}"#,
        r#"{"m":"usage","period":"SEP 2026","items":[{"n":"Claude Pro","p":62,"r":"resets in 4 days"},{"n":"Codex · OpenAI","p":81,"r":"resets tomorrow"},{"n":"Gemini","p":12,"r":"resets in 11 days"}]}"#,
        r#"{"m":"faces","list":[{"f":1,"t":"agents","n":"Agents"},{"f":2,"t":"omarchy","n":"Omarchy"},{"f":3,"t":"macropad","n":"Macropad"},{"f":4,"t":"usage","n":"Usage"},{"f":7,"t":"system","n":"System"},{"f":6,"t":"arcade","n":"Arcade"},{"f":5,"t":"settings","n":"Settings"}]}"#,
        r#"{"m":"synced"}"#,
        r#"{"m":"agent","id":3,"title":"Compiling openmicro ⠋ 0"}"#,
        r#"{"m":"agent","id":3,"title":"Compiling openmicro ⠙ 1"}"#,
        r#"{"m":"agent","id":3,"title":"Compiling openmicro ⠹ 2"}"#,
        r#"{"m":"set","k":"brightness","v":2}"#,
        r#"{"m":"card","agent":1,"id":1,"clear":1}"#,
        r#"{"m":"agent","id":5,"gone":1}"#,
    ];
    /// What deck-sim's framing makes of exactly these lines (its full
    /// session's `dry run: N reports` counts the whole roster).
    const MOCKUP_REPORTS: usize = 51;
    let mut chunker = Chunker::new();
    let mut ui = ui();
    let mut update = crate::fw::ui::Update::new();
    for expected in MOCKUP {
        let line =
            parse_host_line(expected.as_bytes()).unwrap_or_else(|e| panic!("{expected}: {e}"));
        let bytes = encode(&line).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            format!("{expected}\n")
        );
        chunker.push_line(&bytes);
        // The firmware takes the whole session without a single `err`.
        let msg = fw_parse(expected.as_bytes()).unwrap_or_else(|why| panic!("{expected}: {why:?}"));
        ui.host(&msg, 0, &mut update);
    }
    assert_eq!(chunker.pending_reports(), MOCKUP_REPORTS);
    assert_eq!(ui.deck.held_sid(), 314159);
    assert!(!ui.still_default(), "the `set` ended `boot`");
    let stream: Vec<Vec<u8>> =
        std::iter::from_fn(|| chunker.next_report().map(|r| r.to_vec())).collect();
    assert_eq!(stream.len(), MOCKUP_REPORTS);
    let lines: Vec<Vec<u8>> = MOCKUP.iter().map(|l| l.as_bytes().to_vec()).collect();
    assert_eq!(
        fw_assemble(&stream),
        lines.iter().cloned().map(Ok).collect::<Vec<_>>()
    );
}

// ---- device → host -----------------------------------------------------------

#[test]
fn every_device_event_parses_and_the_per_edge_ones_fit_one_report() {
    let ui = ui();
    let mut assembler = Assembler::new();
    for (json, expected) in device_corpus(&ui) {
        assert_eq!(parse_device_line(&json), Ok(expected.clone()), "{json:?}");
        let per_edge = matches!(
            expected,
            DeviceLine::Key { .. }
                | DeviceLine::Macro { .. }
                | DeviceLine::Answer { .. }
                | DeviceLine::Channel { .. }
                | DeviceLine::Snooze { .. }
                | DeviceLine::Face { .. }
                | DeviceLine::Ptt { abort: false, .. }
                | DeviceLine::DocLines { .. }
                | DeviceLine::Tool { .. }
        );
        // Framed by the firmware's own `frame`, fed to this crate's assembler.
        let mut got = Vec::new();
        let mut offset = 0;
        let mut count = 0;
        let mut report = [0; REPORT_LEN];
        loop {
            let n = fwp::frame(&json, offset, &mut report);
            if n == 0 {
                break;
            }
            offset += n;
            count += 1;
            assembler.push_report(&report, &mut |l| got.push(l));
        }
        assert_eq!(got, [Ok(expected.clone())], "{json:?}");
        if per_edge {
            assert!(json.len() < PAYLOAD_MAX, "{json:?} is {} bytes", json.len());
            assert_eq!(count, 1);
        }
        if let DeviceLine::Ptt {
            abort: true,
            card: Some(_),
            ..
        } = expected
        {
            assert!(json.len() < 2 * PAYLOAD_MAX);
            assert_eq!(count, 2);
        }
    }
    // The documented worst case: an aborted answer release, 67 bytes at most.
    let abort = fw_encode(
        Event::Ptt {
            act: 0,
            agent: 8,
            ctx: msg::PttCtx::Answer,
            card: 65535,
            abort: true, press: 0,
        },
        &ui,
    );
    assert_eq!(
        abort,
        br#"{"m":"ptt","act":0,"agent":8,"ctx":"answer","card":65535,"abort":1}"#
    );
    // "<= 67 bytes" in the protocol is the JSON; with its LF it takes two reports.
    assert!(abort.len() <= 67);
    let stream = reports(&[abort.as_slice(), b"\n"].concat(), PAYLOAD_MAX);
    assert_eq!(stream.len(), 2);
    let mut got = Vec::new();
    stream
        .iter()
        .for_each(|r| assembler.push_report(r, &mut |l| got.push(l)));
    assert_eq!(
        got,
        [Ok(DeviceLine::Ptt {
            act: 0,
            slot: 8,
            ctx: PttCtx::Answer,
            card: Some(65535),
            abort: true, press: None,
        })]
    );
}

#[test]
fn the_longest_device_hello_parses_at_every_chunk_size() {
    let mut ui = full_strip_ui();
    let mut update = crate::fw::ui::Update::new();
    ui.host(
        &HostMsg::Hello {
            v: 1,
            sid: u32::MAX,
            minute_of_day: None,
        },
        0,
        &mut update,
    );
    ui.host(&HostMsg::Synced, 1, &mut update);
    let json = fw_encode(Event::Hello, &ui);
    assert!(json.len() > 5 * PAYLOAD_MAX, "{}", json.len());
    for chunk in [1, 2, 7, 16, 60, PAYLOAD_MAX] {
        let stream = reports(&[json.as_slice(), b"\n"].concat(), chunk);
        let mut assembler = Assembler::new();
        let mut got = Vec::new();
        stream
            .iter()
            .for_each(|r| assembler.push_report(r, &mut |l| got.push(l)));
        let [Ok(DeviceLine::Hello {
            v,
            fw,
            sid,
            boot,
            rx,
            faces,
        })] = got.as_slice()
        else {
            panic!("chunk {chunk}: {got:?}")
        };
        assert_eq!(
            (*v, fw.as_str(), *sid, *boot, *rx),
            (VERSION, msg::FW_VERSION, u32::MAX, false, 1024)
        );
        assert_eq!(faces.len(), 8);
        // 16 bytes of the name survived the device's cap; the tab was set
        // locally (not laundered) and travels escaped.
        assert!(
            faces
                .iter()
                .all(|f| f.n == "\"\\\u{1F600}é\t\"\"\"\"\"\"\""),
            "{faces:?}"
        );
    }
    // The device's own `faces` is a valid host `faces` restore (`boot:0`).
    let restore = HostLine::Faces {
        list: match parse_device_line(&fw_encode(Event::Faces, &ui)) {
            Ok(DeviceLine::Faces { list }) => list,
            other => panic!("{other:?}"),
        },
    };
    let bytes = encode(&restore).unwrap();
    assert!(matches!(
        *fw_parse(&stripped(&bytes)).unwrap(),
        HostMsg::Faces { count: 8, .. }
    ));
}

// ---- properties and fuzz -----------------------------------------------------

/// A report of another channel (never 5, and never a bare 6 that could pass
/// for a report id in front of channel 5), with or without the report id.
fn foreign_report() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![0u8, 1, 2, 3, 4, 7, 255]),
        any::<bool>(),
        prop::collection::vec(any::<u8>(), 0..=61),
    )
        .prop_map(|(channel, with_id, payload)| {
            let mut r = Vec::new();
            if with_id {
                r.push(REPORT_ID);
            }
            r.push(channel);
            r.push(payload.len() as u8);
            r.extend(payload);
            r
        })
}

fn mutations() -> impl Strategy<Value = Vec<(usize, u8, u8)>> {
    prop::collection::vec((0..1400usize, 0..6u8, any::<u8>()), 1..6)
}

/// The deck_protocol.rs mutator: damage a valid line a few ways.
fn mutate(line: &mut Vec<u8>, ops: &[(usize, u8, u8)]) {
    for &(at, op, byte) in ops {
        let at = at % (line.len() + 1);
        let last = line.len().saturating_sub(1);
        match op {
            0 if !line.is_empty() => line[at.min(last)] = byte,
            1 if !line.is_empty() => {
                line.remove(at.min(last));
            }
            2 => line.insert(at, b"{}[]\":,\\\n\x00\xff"[usize::from(byte) % 11]),
            3 => line.truncate(at),
            4 if !line.is_empty() => {
                let piece = line[at.min(last)..].to_vec();
                line.extend_from_slice(&piece);
            }
            _ => line.insert(at, byte),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Any chunk size, any mix of lines, foreign channels between the reports:
    /// both assemblers deliver exactly the lines, and the firmware parses each.
    #[test]
    fn any_chunk_size_with_foreign_channels_interleaved_reassembles(
        chunk in 1usize..=PAYLOAD_MAX,
        picks in prop::collection::vec(0usize..36, 1..6),
        foreign in prop::collection::vec(foreign_report(), 0..6),
        every in 1usize..5,
    ) {
        let corpus = corpus();
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for pick in picks {
            let bytes = encode(&corpus[pick % corpus.len()]).unwrap();
            stream.extend(reports(&bytes, chunk));
            expected.push(Ok(stripped(&bytes)));
        }
        let mut mixed = Vec::new();
        let mut junk = foreign.iter().cycle();
        for (i, report) in stream.into_iter().enumerate() {
            if i % every == 0 {
                mixed.extend(junk.next().cloned());
            }
            mixed.push(report);
        }
        let fw = fw_assemble(&mixed);
        prop_assert_eq!(&fw, &expected);
        prop_assert_eq!(host_assemble(&mixed), expected);
        for line in fw {
            prop_assert!(fw_parse(&line.unwrap()).is_ok());
        }
    }

    /// Device events at every chunk size, with foreign traffic between reports.
    #[test]
    fn device_events_reassemble_at_any_chunk_size(
        chunk in 1usize..=PAYLOAD_MAX,
        picks in prop::collection::vec(0usize..80, 1..5),
        foreign in prop::collection::vec(foreign_report(), 0..4),
    ) {
        let ui = ui();
        let corpus = device_corpus(&ui);
        let mut assembler = Assembler::new();
        let mut got = Vec::new();
        let mut expected = Vec::new();
        for (i, pick) in picks.into_iter().enumerate() {
            let (json, line) = &corpus[pick % corpus.len()];
            for report in reports(&[json.as_slice(), b"\n"].concat(), chunk) {
                if let Some(junk) = foreign.get(i) {
                    assembler.push_report(junk, &mut |l| got.push(l));
                }
                assembler.push_report(&report, &mut |l| got.push(l));
            }
            expected.push(Ok(line.clone()));
        }
        prop_assert_eq!(got, expected);
    }

    /// Random bytes into every entry point that device or hook input reaches.
    #[test]
    fn random_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
        let _ = parse_device_line(&bytes);
        let _ = parse_host_line(&bytes);
        let _ = fw_parse(&bytes);
        let mut assembler = Assembler::new();
        assembler.push_report(&bytes, &mut |_| {});
        let mut chunker = Chunker::new();
        chunker.push_line(&bytes);
        while chunker.next_report().is_some() {}
    }

    /// Damaged corpus lines (host and device), straight into the parsers and
    /// through the assembler in random report shapes.
    #[test]
    fn mutated_corpus_lines_never_panic(
        pick in 0usize..120,
        ops in mutations(),
        chunk in 1usize..=PAYLOAD_MAX,
        lie in prop::option::of(any::<u8>()),
    ) {
        let ui = ui();
        let hosts = corpus();
        let devices = device_corpus(&ui);
        let mut line = if pick < hosts.len() {
            stripped(&encode(&hosts[pick]).unwrap())
        } else {
            devices[(pick - hosts.len()) % devices.len()].0.clone()
        };
        mutate(&mut line, &ops);
        let _ = parse_host_line(&line);
        let _ = parse_device_line(&line);
        let _ = fw_parse(&line);
        line.push(b'\n');
        let mut stream = reports(&line, chunk);
        if let (Some(len), Some(first)) = (lie, stream.first_mut()) {
            first[2] = len;
        }
        let mut assembler = Assembler::new();
        for report in &stream {
            assembler.push_report(report, &mut |_| {});
        }
        // Whatever the stream became, both assemblers read it the same way.
        prop_assert_eq!(host_assemble(&stream), fw_assemble(&stream));
    }

    /// Arbitrary report streams: this crate's assembler and the firmware's
    /// make the same lines and the same `Size` rejections of them.
    #[test]
    fn the_assemblers_agree_on_hostile_report_streams(
        stream in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..70), 0..40),
    ) {
        prop_assert_eq!(host_assemble(&stream), fw_assemble(&stream));
    }

    /// Encoding never panics for any field values, and what it accepts the
    /// firmware accepts.
    #[test]
    fn any_agent_line_encodes_or_is_refused_and_the_firmware_agrees(
        slot in any::<u8>(),
        name in prop::option::of("\\PC{0,40}"),
        project in prop::option::of("\\PC{0,40}"),
        title in prop::option::of("\\PC{0,90}"),
        since in prop::option::of(any::<u32>()),
    ) {
        let line = HostLine::Agent { slot, kind: Some(Kind::Claude), name, project, state: None, title, since_s: since };
        match encode(&line) {
            Ok(bytes) => {
                prop_assert!(bytes.len() <= caps().line);
                let capped = parse_host_line(&stripped(&bytes)).unwrap();
                assert_same(&capped, &fw_parse(&stripped(&bytes)).unwrap());
            }
            Err(EncodeError::Range { field }) => prop_assert!(field == "id" && !(1..=8).contains(&slot)),
            Err(e) => prop_assert!(false, "{e}"),
        }
    }
}

/// The keystroke key names are the device's, name for name and in order.
#[test]
fn the_keystroke_key_names_are_the_devices() {
    let device: Vec<&str> = crate::fw::msg::KEYS.iter().map(|key| key.0).collect();
    assert_eq!(device, crate::KEYSTROKE_KEYS);
    for name in crate::KEYSTROKE_KEYS {
        assert!(crate::fw::msg::Keystroke::parse(&format!("cmd+{name}")).is_some(), "{name}");
    }
}

#[test]
fn styled_lines_survive_the_real_firmware_parser() {
    for (font, expected) in [(DocFont::RichRow, msg::DocFont::RichRow), (DocFont::RichText, msg::DocFont::RichText), (DocFont::RichTitle, msg::DocFont::RichTitle)] {
        let line = "p0Hello \u{e000}Bbold\u{e000}A!".to_string();
        let wire = encode(&HostLine::DocText { slot: 1, doc: DocKind::Reply, id: 1, first: 0, total: 1, font, lines: vec![line.clone()] }).unwrap();
        let parsed = fw_parse(&wire).unwrap();
        assert!(matches!(*parsed, HostMsg::DocText { font: f, lines, .. } if f == expected && lines[0].as_str() == line));
    }
}

#[test]
fn workflow_windows_and_disclosures_round_trip_through_real_firmware_codec() {
    let line = HostLine::DocText { slot: 1, doc: DocKind::Workflow, id: 42, first: 0, total: 2,
        font: DocFont::RichText, lines: vec!["w00205".into(), "t040Full plan · 5 steps".into()] };
    let bytes = encode(&line).unwrap();
    assert!(matches!(*fw_parse(&stripped(&bytes)).unwrap(), HostMsg::DocText {
        doc: msg::DocKind::Workflow, id: 42, line_count: 2, .. }));
    let bytes = fw_encode(Event::DocLines { agent: 1, doc: msg::DocKind::Workflow, id: 42, first: 0 }, &ui());
    assert_eq!(parse_device_line(&bytes).unwrap(), DeviceLine::DocLines { slot: 1, doc: DocKind::Workflow, id: 42, first: 0 });
    for index in [64, 65] {
        let bytes = fw_encode(Event::Tool { agent: 1, id: 42, index }, &ui());
        assert_eq!(parse_device_line(&bytes).unwrap(), DeviceLine::Tool { slot: 1, id: 42, index });
        assert!(bytes.len() <= PAYLOAD_MAX);
    }
}

#[test]
fn session_picker_round_trips_and_device_choices_fit_one_report() {
    let menu = SessionMenu { slot: 8, rev: 65535, field: 1, page: 1,
        model: "Model A".into(), effort: "high".into(), status: 2,
        note: "Applying for the next turn...".into(), total: 6, selected: Some(0),
        choices: vec!["Model E".into(), "Model F".into()] };
    let line = HostLine::SessionMenu(menu.clone());
    let bytes = encode(&line).unwrap();
    assert_eq!(parse_host_line(&stripped(&bytes)).unwrap(), line);
    let parsed = fw_parse(&stripped(&bytes)).unwrap();
    let HostMsg::SessionMenu(fw) = *parsed else { panic!("session menu") };
    assert_eq!((fw.agent, fw.rev, fw.field, fw.page, fw.status, fw.total, fw.selected),
        (8, 65535, 1, 1, 2, 6, Some(0)));
    assert_eq!(fw.model.as_str(), "Model A");
    assert_eq!(fw.effort.as_str(), "high");
    assert_eq!(fw.note.as_str(), menu.note);
    assert_eq!(fw.choices[1].as_str(), "Model F");
    for choice in [0, 3, 255] {
        let event = Event::Session { agent: 8, field: 2, page: 63, rev: 65535, choice };
        let wire = fw_encode(event, &ui());
        assert!(wire.len() <= PAYLOAD_MAX);
        assert_eq!(parse_device_line(&stripped(&wire)).unwrap(),
            DeviceLine::Session { slot: 8, field: 2, page: 63, rev: 65535, choice });
    }
}

#[test]
fn image_chunks_round_trip_and_reject_bad_base64() {
    let line = HostLine::Image { slot:8, rev:65535, index:255, offset:0, total:16896, width:128, height:128, data:"/wAA".repeat(128) };
    let bytes = encode(&line).unwrap();
    assert!(bytes.len() < 700);
    assert_eq!(parse_host_line(&stripped(&bytes)).unwrap(), line);
    let parsed = fw_parse(&stripped(&bytes)).unwrap();
    let HostMsg::Image { len, data, .. } = *parsed else { panic!("image chunk") };
    assert_eq!(len,384);
    assert_eq!(&data[..6], &[255,0,0,255,0,0]);
    for data in ["!AAA", "A=AA", "AA=A", "AAAA=", "AA==AAAA"] {
        let bad = HostLine::Image { slot:1, rev:1, index:0, offset:0, total:513, width:1, height:1, data:data.into() };
        assert!(fw_parse(&stripped(&encode(&bad).unwrap())).is_err());
    }
    let event = Event::Image { agent:8, rev:65535, index:255, offset:16512 };
    let wire = fw_encode(event, &ui());
    assert!(wire.len() <= PAYLOAD_MAX);
    assert_eq!(parse_device_line(&stripped(&wire)).unwrap(), DeviceLine::Image { slot:8, rev:65535, index:255, offset:16512 });
}

#[test]
fn invalid_system_readings_are_rejected_at_both_ends() {
    for fields in [
        r#""cpu":101,"mu":0,"mt":0,"df":0,"dt":0,"seq":1"#,
        r#""gpu":-1,"mu":0,"mt":0,"df":0,"dt":0,"seq":1"#,
        r#""mu":2,"mt":1,"df":0,"dt":0,"seq":1"#,
        r#""mu":0,"mt":0,"df":2,"dt":1,"seq":1"#,
        r#""mu":0,"mt":0,"df":0,"dt":0,"seq":4294967296"#,
        r#""mu":0,"mt":0,"df":0,"dt":0"#,
    ] {
        let line = format!(r#"{{"m":"system",{fields}}}"#);
        assert!(parse_host_line(line.as_bytes()).is_err(), "{line}");
        assert!(fw_parse(line.as_bytes()).is_err(), "{line}");
    }
    assert!(encode(&HostLine::System(SystemStatus { cpu: Some(101), ..SystemStatus::EMPTY })).is_err());
}
