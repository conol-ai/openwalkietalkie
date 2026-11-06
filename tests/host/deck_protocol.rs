//! Channel-5 end to end, as a daemon sees it: lines framed into report bodies,
//! reassembled, parsed, applied to a `UiState`, and the device's replies
//! encoded back; plus the hostile-input fuzz that must never panic.
use crate::deck::msg::*;
use crate::deck::proto::*;
use crate::ui::{UiState, Update};

/// Power-on state (no host, no demo).
fn ui() -> Box<UiState> {
    let mut ui = Box::new(UiState::new());
    ui.reset();
    ui
}

/// Frame `line` + LF into report bodies of at most `chunk` payload bytes and
/// feed them to `assembler`; returns every completed line.
fn deliver(assembler: &mut LineAssembler, line: &[u8], chunk: usize) -> Vec<Result<Vec<u8>, ErrWhy>> {
    let mut out = Vec::new();
    let mut with_lf = line.to_vec();
    with_lf.push(b'\n');
    for piece in with_lf.chunks(chunk.clamp(1, PAYLOAD_MAX)) {
        let mut body = vec![CHANNEL, piece.len() as u8];
        body.extend_from_slice(piece);
        assembler.push(&body, &mut |l| out.push(l.map(|l| l.to_vec())));
    }
    out
}

/// Parse `line`, apply it, and return the events the device produced.
fn host(ui: &mut UiState, line: &str, at_ms: u32) -> Result<Vec<Event>, ErrWhy> {
    let mut msg = Box::new(HostMsg::None);
    parse(line.as_bytes(), &mut msg)?;
    let mut update = Update::new();
    ui.host(&msg, at_ms, &mut update);
    Ok(update.deck_events().collect())
}

fn encoded(event: Event, ui: &UiState) -> String {
    let mut out = [0; TX_MAX];
    let n = encode(event, ui, &mut out);
    assert!(n > 0, "{event:?} did not fit");
    String::from_utf8(out[..n].to_vec()).unwrap()
}

/// The daemon's full initial sync, one line per message.
const SYNC: &[&str] = &[
    r#"{"m":"hello","v":1,"sid":314159,"hm":"19:42"}"#,
    r#"{"m":"agent","id":1,"kind":"codex","name":"cohdl-api","state":"question","title":"Run migrations on staging?","since":120}"#,
    r#"{"m":"card","agent":1,"id":1,"type":"q","text":"Tests fail on main. Run pending migrations on staging first?","choices":["Run on staging","Skip migrations","Abort task"],"rec":0}"#,
    r#"{"m":"reply","agent":1,"text":"Tests fail on main: 3 pending migrations are not applied on staging.","rev":1}"#,
    r#"{"m":"agent","id":2,"kind":"claude","name":"web-ui","state":"ready","title":"Header refactor done \u00b7 34 files"}"#,
    r#"{"m":"card","agent":2,"id":2,"type":"r","text":"Header refactor done \u00B7 34 files","choices":["Run the test suite","Open a pull request"]}"#,
    r#"{"m":"agent","id":3,"kind":"codex","name":"firmware","state":"working","title":"Compiling openmicro","since":120}"#,
    r#"{"m":"agent","id":4,"kind":"gemini","name":"research","state":"idle","title":"Idle"}"#,
    r#"{"m":"agent","id":5,"kind":"claude","name":"infra","state":"error","title":"Deploy failed · exit 1"}"#,
    r#"{"m":"ws","active":2,"count":4,"sel":0,"windows":[{"c":"blue","t":"term \u00b7 claude \u2014 header refactor"},{"c":"amber","t":"firefox — github · pulls"},{"c":"green","t":"term · codex — compiling..."}]}"#,
    r#"{"m":"macro","f":3,"src":"deck.toml","keys":[{"l":"build","b":"left"},{"l":"test","b":"right"},{"l":"deploy","b":"enter"},{"l":"git push","b":"shift+left"},{"l":"screenshot"},{"l":"lock"},{"l":"mute","b":"shift+right"},{"l":"term","b":"shift+enter"}]}"#,
    r#"{"m":"usage","period":"SEP 2026","items":[{"n":"Claude Pro","p":62,"r":"resets in 4 days"},{"n":"Codex · OpenAI","p":81,"r":"resets tomorrow"},{"n":"Gemini","p":12,"r":"resets in 11 days"}]}"#,
    r#"{"m":"transcript","agent":1,"text":"Refactor the auth module","final":0}"#,
    r#"{"m":"focus","agent":2}"#,
    r#"{"m":"name","text":"Deploy"}"#,
    r#"{"m":"set","k":"brightness","v":3}"#,
    r#"{"m":"synced"}"#,
];

#[test]
fn a_full_sync_lands_in_the_model_and_hello_is_answered() {
    let mut ui = ui();
    let mut events = Vec::new();
    for (index, line) in SYNC.iter().enumerate() {
        events.extend(host(&mut ui, line, index as u32 * 10).unwrap_or_else(|why| panic!("{line}: {why:?}")));
    }
    assert!(ui.deck.host_present);
    assert_eq!(ui.deck.sid, 314159);
    assert!(ui.deck.clock_valid);
    assert_eq!(ui.deck.clock_minute, 19 * 60 + 42);
    let codex = ui.deck.agent(1).unwrap();
    assert_eq!((codex.kind, codex.state), (AgentKind::Codex, AgentState::Question));
    assert_eq!(codex.name, "cohdl-api");
    assert_eq!(codex.card.choice_count, 3);
    assert_eq!(codex.card.choices[2], "Abort task");
    assert_eq!(codex.card.recommended, Some(0));
    assert_eq!(ui.deck.agent(2).unwrap().title, "Header refactor done \u{b7} 34 files");
    assert_eq!(ui.deck.agent(2).unwrap().card.kind, CardKind::Ready);
    assert_eq!(ui.deck.workspaces.window_count, 3);
    assert_eq!(ui.deck.workspaces.windows[1].tone, Tone::Amber);
    assert_eq!(ui.deck.workspaces.windows[2].title, "term \u{b7} codex \u{2014} compiling...");
    let keys = ui.deck.macro_set(3).unwrap();
    assert_eq!(keys.source, "deck.toml");
    assert_eq!((keys.keys[3].label.as_str(), keys.keys[3].binding), ("git push", Binding::ShiftLeft));
    assert!(keys.keys[4].present && keys.keys[4].binding == Binding::None);
    assert!(!keys.keys[8].present);
    assert_eq!(ui.deck.usage.count, 3);
    assert_eq!(ui.deck.usage.items[1].name, "Codex \u{b7} OpenAI");
    assert_eq!(ui.deck.usage.items[1].percent, 81);
    assert_eq!(ui.deck.transcript, "Refactor the auth module");
    assert_eq!(ui.deck.settings.brightness, 3);
    assert!(!ui.deck.syncing);
    // The hello was answered, and with what the host needs to decide on a sync.
    assert_eq!(events.iter().filter(|e| **e == Event::Hello).count(), 1);
    let hello = encoded(Event::Hello, &ui);
    assert!(hello.starts_with(r#"{"m":"hello","v":1,"fw":"#));
    assert!(hello.contains(r#""sid":314159"#));
    assert!(hello.contains(r#""rx":1024"#));
    assert!(hello.contains(r#""boot":0"#)); // the `set` above changed a default
    assert!(hello.contains(r#""faces":[{"f":1,"t":"agents","n":"Agents"}"#));
    // Nothing the host applied is echoed back as its mirror event.
    assert!(!events.iter().any(|e| matches!(e, Event::Set { .. } | Event::Faces | Event::Channel { .. })));
}

#[test]
fn every_message_survives_framing_and_reassembly_at_every_chunk_size() {
    for chunk in [1, 2, 7, 16, 60, PAYLOAD_MAX] {
        let mut assembler = LineAssembler::new();
        for line in SYNC {
            let got = deliver(&mut assembler, line.as_bytes(), chunk);
            assert_eq!(got, [Ok(line.as_bytes().to_vec())], "chunk {chunk}: {line}");
            let mut msg = Box::new(HostMsg::None);
            assert_eq!(parse(&got[0].as_ref().unwrap()[..], &mut msg), Ok(()));
            assert_ne!(*msg, HostMsg::None);
        }
    }
}

#[test]
fn several_lines_share_one_report_and_one_line_straddles_several() {
    let mut assembler = LineAssembler::new();
    let mut got = Vec::new();
    let mut f = |line: Result<&[u8], ErrWhy>| got.push(line.map(|l| l.to_vec()));
    // Three short messages plus the head of a long one in a single report.
    let head = b"{\"m\":\"synced\"}\n{\"m\":\"focus\",\"agent\":1}\n\n{\"m\":\"hello\",\"v\":1";
    let mut body = vec![CHANNEL, head.len() as u8];
    body.extend_from_slice(head);
    assembler.push(&body, &mut f);
    // Reports without payload are harmless.
    assembler.push(&[CHANNEL, 0], &mut f);
    let tail = b",\"sid\":9}\n\n";
    let mut body = vec![CHANNEL, tail.len() as u8];
    body.extend_from_slice(tail);
    assembler.push(&body, &mut f);
    // Empty lines are keepalives, not messages.
    assembler.push(&[CHANNEL, 2, b'\n', b'\n'], &mut f);
    assert_eq!(
        got,
        [
            Ok(br#"{"m":"synced"}"#.to_vec()),
            Ok(br#"{"m":"focus","agent":1}"#.to_vec()),
            Ok(br#"{"m":"hello","v":1,"sid":9}"#.to_vec()),
        ]
    );
}

#[test]
fn a_message_may_straddle_reports_with_or_without_the_report_id() {
    let mut assembler = LineAssembler::new();
    let mut got = Vec::new();
    let head = br#"{"m":"hello""#;
    let tail = b",\"v\":1,\"sid\":2}\n";
    let mut first = vec![REPORT_ID, CHANNEL, head.len() as u8];
    first.extend_from_slice(head);
    let mut second = vec![CHANNEL, tail.len() as u8];
    second.extend_from_slice(tail);
    assembler.push(&first, &mut |l| got.push(l.map(|l| l.to_vec())));
    assembler.push(&second, &mut |l| got.push(l.map(|l| l.to_vec())));
    assert_eq!(got, [Ok(br#"{"m":"hello","v":1,"sid":2}"#.to_vec())]);
    let mut msg = Box::new(HostMsg::None);
    assert_eq!(parse(got[0].as_ref().unwrap(), &mut msg), Ok(()));
    assert!(matches!(*msg, HostMsg::Hello { sid: 2, .. }));
}

#[test]
fn other_channels_interleaved_at_report_granularity_are_ignored() {
    let mut assembler = LineAssembler::new();
    let mut got = Vec::new();
    let mut f = |line: Result<&[u8], ErrWhy>| got.push(line.map(|l| l.to_vec()));
    let line = br#"{"m":"agent","id":2,"state":"ready","title":"Header refactor done"}"#;
    let (a, b) = line.split_at(30);
    let mut first = vec![REPORT_ID, CHANNEL, a.len() as u8];
    first.extend_from_slice(a);
    assembler.push(&first, &mut f);
    // System RPC (channel 2, with and without the report id), an old
    // icon-sync chunk (3), a diagnostics body (4, 63 bytes), and junk on
    // channel 0/255.
    let mut rpc = vec![REPORT_ID, 2, 20];
    rpc.extend_from_slice(br#"{"method":"x"}{"m":"#);
    assembler.push(&rpc, &mut f);
    assembler.push(&rpc[1..], &mut f);
    let mut icon = vec![3, 61];
    icon.extend_from_slice(&[b'{'; 61]);
    assembler.push(&icon, &mut f);
    let mut diag = vec![4, 63];
    diag.extend_from_slice(&[b'\n'; 61]);
    assembler.push(&diag, &mut f);
    assembler.push(&[0, 1, b'\n'], &mut f);
    assembler.push(&[255, 1, b'\n'], &mut f);
    assembler.push(&[], &mut f);
    assembler.push(&[CHANNEL], &mut f);
    assembler.push(&[REPORT_ID], &mut f);
    assembler.push(&[REPORT_ID, CHANNEL], &mut f);
    let mut second = vec![CHANNEL, b.len() as u8 + 1];
    second.extend_from_slice(b);
    second.push(b'\n');
    assembler.push(&second, &mut f);
    assert_eq!(got, [Ok(line.to_vec())]);
}

#[test]
fn an_overlong_line_is_reported_once_and_the_next_line_is_intact() {
    let mut assembler = LineAssembler::new();
    let mut long = br#"{"m":"reply","agent":1,"rev":1,"text":""#.to_vec();
    long.extend(std::iter::repeat_n(b'x', LINE_MAX));
    long.extend_from_slice(br#""}"#);
    let mut got = deliver(&mut assembler, &long, PAYLOAD_MAX);
    got.extend(deliver(&mut assembler, br#"{"m":"synced"}"#, 5));
    assert_eq!(got, [Err(ErrWhy::Size), Ok(br#"{"m":"synced"}"#.to_vec())]);
    // A `clear` (link drop) forgets a partial line and a discard alike.
    deliver(&mut assembler, &long[..LINE_MAX + 10], PAYLOAD_MAX);
    assembler.clear();
    let got = deliver(&mut assembler, br#"{"m":"synced"}"#, 3);
    assert_eq!(got, [Ok(br#"{"m":"synced"}"#.to_vec())]);
}

#[test]
fn every_per_edge_event_fits_one_report_with_its_lf() {
    let ui = ui();
    let mut worst: Vec<String> = Vec::new();
    for key in [KeyId::Enter, KeyId::Cancel, KeyId::Left, KeyId::Right, KeyId::Cw, KeyId::Ccw] {
        for (act, shift) in [(1, false), (0, true), (2, true)] {
            worst.push(encoded(Event::Key { key, act, shift, face: 8 }, &ui));
        }
    }
    for ctx in [PttCtx::Talk, PttCtx::Answer, PttCtx::Name] {
        for card in [0, 65535] {
            worst.push(encoded(Event::Ptt { act: 1, agent: 8, ctx, card, abort: false, press: 0, }, &ui));
            worst.push(encoded(Event::Ptt { act: 0, agent: 8, ctx, card, abort: false, press: 0, }, &ui));
        }
    }
    worst.push(encoded(Event::Macro { face: 8, index: 8, act: 2 }, &ui));
    worst.push(encoded(Event::Answer { agent: 8, card: 65535, choice: 3 }, &ui));
    worst.push(encoded(Event::Channel { agent: 8, seq: 65535 }, &ui));
    worst.push(encoded(Event::Snooze { agent: 8, card: 65535 }, &ui));
    worst.push(encoded(Event::Face { id: 8 }, &ui));
    for json in &worst {
        assert!(json.len() + 1 <= PAYLOAD_MAX, "{json} is {} bytes", json.len());
        let mut report = [0; REPORT_LEN];
        assert_eq!(frame(json.as_bytes(), 0, &mut report), json.len() + 1);
        assert_eq!(frame(json.as_bytes(), json.len() + 1, &mut report), 0);
        assert_eq!(report[3 + json.len()], b'\n');
    }
    // The one documented exception: an aborted card answer needs two reports.
    let abort = encoded(Event::Ptt { act: 0, agent: 8, ctx: PttCtx::Answer, card: 65535, abort: true, press: 0, }, &ui);
    assert!(abort.len() + 1 <= 2 * PAYLOAD_MAX);
    assert_eq!(abort, r#"{"m":"ptt","act":0,"agent":8,"ctx":"answer","card":65535,"abort":1}"#);
}

#[test]
fn device_lines_are_valid_json_a_host_library_would_accept() {
    // No host JSON parser is available here; the device's own reader stands in
    // for one and must accept every line the device produces.
    let mut ui = ui();
    ui.global.strip[3].name.set("tab\there \"q\" \\ \u{1F600}");
    let all = [
        Event::Hello,
        Event::Up,
        Event::Faces,
        Event::Refresh,
        Event::Key { key: KeyId::Left, act: 1, shift: false, face: 2 },
        Event::Channel { agent: 3, seq: 1 },
        Event::Ptt { act: 1, agent: 3, ctx: PttCtx::Answer, card: 7, abort: false, press: 0, },
        Event::Ptt { act: 0, agent: 3, ctx: PttCtx::Answer, card: 7, abort: true, press: 0, },
        Event::Answer { agent: 3, card: 7, choice: 1 },
        Event::Snooze { agent: 3, card: 7 },
        Event::Macro { face: 3, index: 4, act: 1 },
        Event::WsGo(3),
        Event::WsWin(1),
        Event::Set { key: SettingKey::Demo, value: 0 },
        Event::Face { id: 4 },
        Event::Err { why: ErrWhy::Size },
    ];
    for event in all {
        let json = encoded(event, &ui);
        assert!(json.starts_with(r#"{"m":""#), "{json}");
        assert!(json.is_ascii() || matches!(event, Event::Hello | Event::Faces), "{json}");
        assert!(!json.bytes().any(|b| b < 0x20), "{json}");
        let object = crate::deck::json::Object::parse(json.as_bytes()).unwrap_or_else(|why| panic!("{json}: {why:?}"));
        assert!(object.get("m").unwrap().is_some());
        for member in object.members() {
            let (_, value) = member.unwrap();
            if let Ok(items) = value.array() {
                for item in items {
                    for member in item.unwrap().object().unwrap().members() {
                        member.unwrap();
                    }
                }
            }
        }
    }
    let faces = encoded(Event::Faces, &ui);
    assert!(faces.contains(r#""n":"tab\u0009here \"q\" \\ "#));
}

#[test]
fn escapes_and_surrogates_decode_into_the_model() {
    let mut ui = ui();
    host(&mut ui, r#"{"m":"hello","v":1,"sid":1}"#, 0).unwrap();
    host(&mut ui, r#"{"m":"agent","id":1,"name":"café 😀","title":"a\"b\\c\/d\n\ttab"}"#, 0).unwrap();
    let agent = ui.deck.agent(1).unwrap();
    assert_eq!(agent.name, "caf\u{e9} \u{1F600}");
    assert_eq!(agent.title, "a\"b\\c/d  tab");
    host(&mut ui, "{\"m\":\"reply\",\"agent\":1,\"text\":\"raw \u{1F600} \u{e9} \\ud83d lone\",\"rev\":1}", 0).unwrap();
    assert_eq!(ui.deck.agent(1).unwrap().last_reply, "raw \u{1F600} \u{e9} \u{FFFD} lone");
    assert_eq!(host(&mut ui, r#"{"m":"reply","agent":1,"text":"bad \u12","rev":1}"#, 0), Err(ErrWhy::Parse));
    assert_eq!(ui.deck.agent(1).unwrap().last_reply, "raw \u{1F600} \u{e9} \u{FFFD} lone");
}

#[test]
fn rejected_lines_change_nothing_and_are_reported_once_per_second() {
    let mut ui = ui();
    host(&mut ui, r#"{"m":"hello","v":1,"sid":1}"#, 0).unwrap();
    host(&mut ui, r#"{"m":"agent","id":1,"name":"one","state":"working"}"#, 0).unwrap();
    let before = ui.deck.agents.to_vec();
    // Unknown agent: the model refuses it and the runtime reports `err`.
    let events = host(&mut ui, r#"{"m":"card","agent":7,"id":1,"type":"q","text":"?"}"#, 100).unwrap();
    assert_eq!(events, [Event::Err { why: ErrWhy::Range }]);
    // Parse-level rejections go through `UiState::reject` with the same limiter.
    let mut update = Update::new();
    ui.reject(ErrWhy::Parse, 200, &mut update);
    assert_eq!(update.deck_events().count(), 0);
    ui.reject(ErrWhy::Size, 1200, &mut update);
    assert_eq!(update.deck_events().collect::<Vec<_>>(), [Event::Err { why: ErrWhy::Size }]);
    assert_eq!(*ui.deck.agents, before[..]);
    assert_eq!(encoded(Event::Err { why: ErrWhy::Size }, &ui), r#"{"m":"err","why":"size"}"#);
}

// ---- fuzz --------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        // xorshift64*: deterministic, good enough to spread bytes around.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

const NAMES: &[&str] = &[
    "m", "v", "sid", "hm", "id", "gone", "kind", "state", "name", "title", "since", "agent", "type", "text",
    "choices", "rec", "clear", "final", "ack", "active", "count", "sel", "windows", "c", "t", "f", "src",
    "keys", "l", "b", "period", "items", "n", "p", "r", "list", "k", "x",
];
const WORDS: &[&str] = &[
    "hello", "synced", "agent", "card", "reply", "transcript", "focus", "ws", "macro", "usage", "faces",
    "name", "set", "claude", "codex", "gemini", "other", "working", "question", "ready", "error", "idle",
    "q", "r", "blue", "amber", "green", "red", "gray", "agents", "omarchy", "macropad", "usage", "arcade", "settings",
    "speaker", "mic", "announce", "chime", "autojump", "brightness", "idlehome", "demo", "left", "right",
    "enter", "cw", "ccw", "shift+left", "shift+right", "shift+enter", "19:42", "bogus", "",
];

fn random_string(rng: &mut Rng, out: &mut Vec<u8>) {
    out.push(b'"');
    for _ in 0..rng.below(40) {
        match rng.below(12) {
            0 => out.extend_from_slice(b"\\\""),
            1 => out.extend_from_slice(b"\\\\"),
            2 => out.extend_from_slice(b"\\n"),
            3 => out.extend_from_slice(b"\\u00e9"),
            4 => out.extend_from_slice(b"\\ud83d\\ude00"),
            5 => out.extend_from_slice(b"\\ud83d"),
            6 if rng.below(4) == 0 => out.extend_from_slice(b"\\u12"),
            7 => out.push(rng.byte()),
            8 => out.extend_from_slice("\u{1F600}".as_bytes()),
            _ => out.push(b'a' + rng.below(26) as u8),
        }
    }
    out.push(b'"');
}

/// A random JSON-ish value of bounded depth: mostly well-formed, sometimes not.
fn random_value(rng: &mut Rng, out: &mut Vec<u8>, depth: u32) {
    match rng.below(if depth == 0 { 6 } else { 9 }) {
        0 => out.extend_from_slice(rng.next().to_string().as_bytes()),
        1 => out.extend_from_slice(b"true"),
        2 => out.extend_from_slice(b"-3.5e2"),
        3 => out.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes()),
        4 | 5 => random_string(rng, out),
        6 => {
            out.push(b'[');
            for i in 0..rng.below(11) {
                if i > 0 {
                    out.push(b',');
                }
                random_value(rng, out, depth - 1);
            }
            out.push(b']');
        }
        _ => {
            out.push(b'{');
            for i in 0..rng.below(8) {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'"');
                out.extend_from_slice(NAMES[rng.below(NAMES.len())].as_bytes());
                out.extend_from_slice(b"\":");
                random_value(rng, out, depth - 1);
            }
            out.push(b'}');
        }
    }
}

/// One message of a known shape with random, mostly in-range values: the
/// model sees plenty of valid variety, the validators see every edge.
fn random_shaped(rng: &mut Rng, out: &mut Vec<u8>) {
    fn num(rng: &mut Rng, out: &mut Vec<u8>, bound: usize) {
        out.extend_from_slice(rng.below(bound).to_string().as_bytes());
    }
    fn word(rng: &mut Rng, out: &mut Vec<u8>, from: &[&str]) {
        out.push(b'"');
        out.extend_from_slice(from[rng.below(from.len() + 1).min(from.len() - 1)].as_bytes());
        out.push(b'"');
    }
    fn field(rng: &mut Rng, out: &mut Vec<u8>, key: &str, optional: bool) -> bool {
        if optional && rng.below(2) == 0 {
            return false;
        }
        out.extend_from_slice(b",\"");
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(b"\":");
        true
    }
    fn list(rng: &mut Rng, out: &mut Vec<u8>, key: &str, max: usize, item: &mut dyn FnMut(&mut Rng, &mut Vec<u8>)) {
        field(rng, out, key, false);
        out.push(b'[');
        for i in 0..rng.below(max + 2) {
            if i > 0 {
                out.push(b',');
            }
            // Members are written with a leading comma; drop the first one.
            let mut scratch = Vec::new();
            item(rng, &mut scratch);
            out.push(b'{');
            out.extend_from_slice(scratch.get(1..).unwrap_or(&[]));
            out.push(b'}');
        }
        out.push(b']');
    }
    const KINDS: &[&str] = &["claude", "codex", "gemini", "other"];
    const STATES: &[&str] = &["working", "question", "ready", "error", "idle"];
    const TONES: &[&str] = &["blue", "amber", "green", "red", "gray"];
    const TEMPLATES: &[&str] = &["agents", "omarchy", "macropad", "usage", "arcade", "settings"];
    const KEYS: &[&str] = &["speaker", "mic", "announce", "chime", "autojump", "brightness", "idlehome", "demo", "text", "arcadesound"];
    const BINDINGS: &[&str] = &["left", "right", "enter", "cw", "ccw", "shift+left", "shift+right", "shift+enter"];
    out.clear();
    out.extend_from_slice(b"{\"m\":");
    let shape = rng.below(13);
    word(rng, out, &["hello", "synced", "agent", "card", "reply", "transcript", "focus", "ws", "macro", "usage", "faces", "name", "set"][shape..shape + 1]);
    match shape {
        0 => {
            field(rng, out, "v", false);
            num(rng, out, 3);
            field(rng, out, "sid", false);
            num(rng, out, 5);
            if field(rng, out, "hm", true) {
                out.push(b'"');
                num(rng, out, 26);
                out.push(b':');
                out.extend_from_slice(std::format!("{:02}", rng.below(62)).as_bytes());
                out.push(b'"');
            }
        }
        2 => {
            field(rng, out, "id", false);
            num(rng, out, 10);
            if field(rng, out, "gone", true) {
                num(rng, out, 2);
            }
            if field(rng, out, "kind", true) {
                word(rng, out, KINDS);
            }
            if field(rng, out, "state", true) {
                word(rng, out, STATES);
            }
            if field(rng, out, "name", true) {
                random_string(rng, out);
            }
            if field(rng, out, "title", true) {
                random_string(rng, out);
            }
            if field(rng, out, "since", true) {
                num(rng, out, 100000);
            }
        }
        3 => {
            field(rng, out, "agent", false);
            num(rng, out, 10);
            field(rng, out, "id", false);
            num(rng, out, 6);
            if field(rng, out, "clear", true) {
                num(rng, out, 2);
            }
            if field(rng, out, "type", true) {
                word(rng, out, &["q", "r"]);
            }
            if field(rng, out, "text", true) {
                random_string(rng, out);
            }
            if field(rng, out, "choices", true) {
                out.push(b'[');
                for i in 0..rng.below(6) {
                    if i > 0 {
                        out.push(b',');
                    }
                    random_string(rng, out);
                }
                out.push(b']');
            }
            if field(rng, out, "rec", true) {
                num(rng, out, 5);
            }
        }
        4 | 5 => {
            field(rng, out, "agent", false);
            num(rng, out, 10);
            field(rng, out, "text", false);
            random_string(rng, out);
            if field(rng, out, "final", true) {
                num(rng, out, 2);
            }
        }
        6 => {
            field(rng, out, "agent", false);
            num(rng, out, 10);
            if field(rng, out, "ack", true) {
                num(rng, out, 70000);
            }
        }
        7 => {
            for key in ["active", "count", "sel"] {
                field(rng, out, key, false);
                num(rng, out, 11);
            }
            list(rng, out, "windows", 6, &mut |rng, out| {
                if field(rng, out, "c", true) {
                    word(rng, out, TONES);
                }
                field(rng, out, "t", false);
                random_string(rng, out);
            });
        }
        8 => {
            field(rng, out, "f", false);
            num(rng, out, 10);
            if field(rng, out, "src", true) {
                random_string(rng, out);
            }
            list(rng, out, "keys", 9, &mut |rng, out| {
                field(rng, out, "l", false);
                random_string(rng, out);
                if field(rng, out, "b", true) {
                    word(rng, out, BINDINGS);
                }
            });
        }
        9 => {
            field(rng, out, "period", false);
            random_string(rng, out);
            list(rng, out, "items", 6, &mut |rng, out| {
                field(rng, out, "n", false);
                random_string(rng, out);
                field(rng, out, "p", false);
                num(rng, out, 110);
                field(rng, out, "r", false);
                random_string(rng, out);
                if field(rng, out, "c", true) {
                    word(rng, out, TONES);
                }
            });
        }
        10 => {
            list(rng, out, "list", 8, &mut |rng, out| {
                field(rng, out, "f", false);
                num(rng, out, 10);
                field(rng, out, "t", false);
                word(rng, out, TEMPLATES);
                field(rng, out, "n", false);
                random_string(rng, out);
            });
        }
        11 => {
            field(rng, out, "text", false);
            random_string(rng, out);
        }
        12 => {
            field(rng, out, "k", false);
            word(rng, out, KEYS);
            field(rng, out, "v", false);
            num(rng, out, 70000);
        }
        _ => {}
    }
    out.push(b'}');
}

fn random_message(rng: &mut Rng, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(b"{\"m\":\"");
    out.extend_from_slice(WORDS[rng.below(14)].as_bytes());
    out.push(b'"');
    for _ in 0..rng.below(9) {
        out.extend_from_slice(b",\"");
        out.extend_from_slice(NAMES[rng.below(NAMES.len())].as_bytes());
        out.extend_from_slice(b"\":");
        random_value(rng, out, 2);
    }
    out.push(b'}');
}

fn mutate(rng: &mut Rng, line: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(line.len() + 1);
        match rng.below(6) {
            0 if !line.is_empty() => {
                let last = line.len() - 1;
                line[at.min(last)] = rng.byte();
            }
            1 if !line.is_empty() => {
                line.remove(at.min(line.len() - 1));
            }
            2 => line.insert(at, b"{}[]\":,\\\n\x00\xff"[rng.below(11)]),
            3 => line.truncate(at),
            4 if !line.is_empty() => {
                let piece = line[at.min(line.len() - 1)..].to_vec();
                line.extend_from_slice(&piece);
            }
            _ => line.insert(at, rng.byte()),
        }
    }
}

/// Everything a hostile daemon can send: through the assembler in random
/// report sizes, then parse, then the model. Must never panic.
#[test]
fn hostile_input_never_panics() {
    let mut rng = Rng(0x5EED_5EED_5EED_5EED);
    let mut ui = ui();
    let mut assembler = LineAssembler::new();
    let mut msg = Box::new(HostMsg::None);
    let mut update = Update::new();
    let mut line = Vec::new();
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut parsed = 0;
    let mut applied = 0;
    let mut by_kind = [0; 4];
    for case in 0..12000u32 {
        match case % 4 {
            0 => {
                // Random bytes, sometimes very long.
                line.clear();
                let len = if rng.below(10) == 0 { rng.below(1300) } else { rng.below(120) };
                line.extend((0..len).map(|_| rng.byte()));
            }
            1 => {
                // A valid message, damaged.
                line.clear();
                line.extend_from_slice(SYNC[rng.below(SYNC.len())].as_bytes());
                mutate(&mut rng, &mut line);
            }
            2 => random_message(&mut rng, &mut line),
            _ => random_shaped(&mut rng, &mut line),
        }
        // Straight into the parser (a line the assembler would have delivered).
        update.clear();
        match parse(&line, &mut msg) {
            Ok(()) => {
                parsed += 1;
                by_kind[(case % 4) as usize] += 1;
                ui.host(&msg, case * 7, &mut update);
                applied += 1;
            }
            Err(why) => {
                assert_eq!(*msg, HostMsg::None);
                ui.reject(why, case * 7, &mut update);
            }
        }
        // And through the assembler in random report shapes, with junk reports mixed in.
        line.push(b'\n');
        let mut at = 0;
        while at < line.len() {
            let n = (1 + rng.below(PAYLOAD_MAX)).min(line.len() - at);
            let mut body = Vec::new();
            match rng.below(7) {
                0 => body.push(REPORT_ID),
                1 => {
                    let mut junk = vec![rng.byte(), rng.byte()];
                    junk.extend((0..rng.below(62)).map(|_| rng.byte()));
                    assembler.push(&junk, &mut |l| lines.push(l.unwrap_or(b"").to_vec()));
                }
                _ => {}
            }
            body.push(CHANNEL);
            // Sometimes lie about the length.
            body.push(if rng.below(20) == 0 { rng.byte() } else { n as u8 });
            body.extend_from_slice(&line[at..at + n]);
            assembler.push(&body, &mut |l| lines.push(l.unwrap_or(b"").to_vec()));
            at += n;
        }
        for delivered in lines.drain(..) {
            if parse(&delivered, &mut msg).is_ok() {
                update.clear();
                ui.host(&msg, case * 7 + 3, &mut update);
            }
        }
        if case % 500 == 0 {
            update.clear();
            ui.tick(case * 7 + 5, 0, true, &mut update);
            for event in update.deck_events() {
                let mut out = [0; TX_MAX];
                encode(event, &ui, &mut out);
            }
            let mut out = [0; TX_MAX];
            assert!(encode(Event::Hello, &ui, &mut out) > 0);
            let _ = ui.animating();
        }
    }
    // The corpus is not all noise: a good share of it parsed and applied.
    assert!(parsed > 1500 && by_kind[1..].iter().all(|n| *n > 100), "only {parsed} lines parsed ({by_kind:?})");
    assert_eq!(parsed, applied);
    assert!(ui.deck.host_present);
}

#[test]
fn random_bytes_through_the_json_reader_never_panic() {
    let mut rng = Rng(0xC0FFEE);
    let mut buf = Vec::new();
    let mut text = crate::deck::text::Text::<24>::new();
    for _ in 0..4000 {
        buf.clear();
        random_value(&mut rng, &mut buf, 3);
        if rng.below(3) == 0 {
            mutate(&mut rng, &mut buf);
        }
        if let Ok(object) = crate::deck::json::Object::parse(&buf) {
            for member in object.members() {
                let Ok((_, value)) = member else { break };
                let _ = value.u32();
                let _ = value.bool();
                let _ = value.text_into(&mut text);
                if let Ok(items) = value.array() {
                    for item in items.flatten() {
                        let _ = item.u8();
                        let _ = item.text_into(&mut text);
                        if let Ok(inner) = item.object() {
                            let _ = inner.get("t");
                        }
                    }
                }
            }
        }
        let _ = crate::deck::json::decode_into(&buf, &mut text);
    }
}
