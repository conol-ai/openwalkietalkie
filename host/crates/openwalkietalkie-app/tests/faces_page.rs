//! The Faces & Keys page's rules, read without a window: the strip's pins,
//! moves, removals and additions by the deck's own rules, the ids and names
//! a new face gets, the strip the page reads and the face it selects, the
//! wait for the deck; a Macropad's shortcuts in the deck's grammar (every
//! key and every set of modifiers, checked by the service's own rule), a
//! key pressed in the window as the deck's shortcut, the label's byte rule,
//! a control moving from key to key, the slots; the Usage face's sources;
//! Accessibility; the wire params of both writes; and that every word the
//! page says has a row in all four languages.
use deck_gui_core::i18n::Language;
use deck_model::{
    Binding, FaceEntry, MacroKey, Principal, Request, SessionId, Snapshot, Store, StoreConfig,
    Template, Transport,
};
use gpui::{Keystroke, Modifiers};
use openwalkietalkie_app::faces::{
    addable, added, can_move_left, can_move_right, can_remove, default_id, face_label, face_name,
    free_id, moved, pinned, removed, same_strip, selection, strip, strip_write, usage_write,
    Forget, Panel, Pending, Side, StripNote, UsageConfig, CLAUDE, CODEX, GALLERY, PENDING_HOLD,
};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::keys::{
    binding_mark, binding_word, cleared, empty_key, from_gpui, key_cap, key_name, keys_shown,
    keys_write, label_fits, modifier_only, placed, slot, trimmed, Access, KeyDraft, Sent, Shortcut,
    BINDINGS, KEYS, KEYS_HOLD, LABEL_BYTES, SLOTS,
};
use openwalkietalkie_app::shots::{board_now, board_snapshot, Scene, BOARD_MACROPAD};
use openwalkietalkie_app::ui::Mod;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use Template::{Agents, Arcade, Macropad, Omarchy, Settings, System, Timer, Usage};

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;
const JA: Language = Language::Japanese;

/// The boards' strip: Agents 1, Macropad 3, Usage 4, System 7, Timer 8,
/// Arcade 6, Settings 5.
fn board() -> Vec<FaceEntry> {
    board_snapshot().faces
}

/// The boards' eight keys.
fn board_keys() -> Vec<MacroKey> {
    board_snapshot().macros[&BOARD_MACROPAD].keys.clone()
}

fn face(f: u8, t: Template) -> FaceEntry {
    FaceEntry {
        f,
        t,
        n: face_label(t).into(),
    }
}

fn ids(list: &[FaceEntry]) -> Vec<u8> {
    list.iter().map(|face| face.f).collect()
}

fn key(label: &str, binding: Option<Binding>, keys: Option<&str>) -> MacroKey {
    MacroKey {
        label: label.into(),
        binding,
        keys: keys.map(str::to_string),
    }
}

/// A key pressed in the window, as gpui reports it: `held` names the
/// modifiers (`ctrl opt shift cmd`).
fn stroke(held: &str, key: &str) -> Keystroke {
    Keystroke {
        modifiers: Modifiers {
            control: held.contains("ctrl"),
            alt: held.contains("opt"),
            shift: held.contains("shift"),
            platform: held.contains("cmd"),
            function: false,
        },
        key: key.into(),
        key_char: None,
    }
}

// ---- the strip -----------------------------------------------------------------

#[test]
fn agents_and_settings_stay_at_the_ends_and_the_faces_between_them_move() {
    for face in [Agents, Settings] {
        assert!(pinned(face), "{face:?}");
    }
    for face in [Macropad, Usage, System, Timer, Arcade, Omarchy] {
        assert!(!pinned(face), "{face:?}");
    }
    let list = board();
    // Agents and Settings never move, and nothing moves past them.
    for at in [0, 6] {
        assert!(
            !can_move_left(&list, at) && !can_move_right(&list, at),
            "{at}"
        );
        assert!(!can_remove(&list, at), "{at}");
    }
    // The Macropad sits after Agents: right, not left (the board's
    // disabled Move Left).
    assert!(!can_move_left(&list, 1));
    assert!(can_move_right(&list, 1));
    // Arcade sits before Settings: left, not right.
    assert!(can_move_left(&list, 5));
    assert!(!can_move_right(&list, 5));
    for at in 2..5 {
        assert!(
            can_move_left(&list, at) && can_move_right(&list, at),
            "{at}"
        );
    }
    assert!((1..6).all(|at| can_remove(&list, at)));
    // Past the strip's end there is nothing to move or remove.
    assert!(!can_move_left(&list, 7) && !can_move_right(&list, 7) && !can_remove(&list, 7));
    assert!(!can_move_right(&[], 0) && !can_move_left(&[], 0) && !can_remove(&[], 0));
}

#[test]
fn a_move_swaps_two_neighbours_and_one_the_rules_refuse_changes_nothing() {
    let list = board();
    assert_eq!(ids(&moved(&list, 1, Side::Right)), [1, 4, 3, 7, 8, 6, 5]);
    assert_eq!(ids(&moved(&list, 5, Side::Left)), [1, 3, 4, 7, 6, 8, 5]);
    assert_eq!(moved(&list, 1, Side::Left), list);
    assert_eq!(moved(&list, 5, Side::Right), list);
    assert_eq!(moved(&list, 0, Side::Right), list);
    assert_eq!(ids(&removed(&list, 2)), [1, 3, 7, 8, 6, 5]);
    assert_eq!(removed(&list, 0), list);
    assert_eq!(removed(&list, 6), list);
    assert_eq!(removed(&list, 9), list);
}

#[test]
fn the_strip_takes_what_the_decks_gallery_offers_and_no_more() {
    let list = board();
    // One Macropad and one Timer already: the Timer is not offered again,
    // and Omarchy only on Linux.
    assert_eq!(addable(&list, false), [Macropad, Usage, Arcade, System]);
    assert_eq!(
        addable(&list, true),
        [Omarchy, Macropad, Usage, Arcade, System]
    );
    let no_timer: Vec<FaceEntry> = list.iter().filter(|f| f.t != Timer).cloned().collect();
    assert!(addable(&no_timer, false).contains(&Timer));
    // Three Macropads is the most the deck keeps keys for.
    let mut three = vec![
        face(1, Agents),
        face(3, Macropad),
        face(2, Macropad),
        face(4, Macropad),
        face(5, Settings),
    ];
    assert!(!addable(&three, false).contains(&Macropad));
    three.remove(3);
    assert!(addable(&three, false).contains(&Macropad));
    // Eight faces: nothing more.
    let full = Scene::FacesFull.fixture().unwrap().mirror.faces;
    assert_eq!(full.len(), 8);
    assert!(addable(&full, true).is_empty());
    // Never Agents or Settings, which are always there.
    for linux in [false, true] {
        let offered = addable(&[face(1, Agents), face(5, Settings)], linux);
        assert!(!offered.contains(&Agents) && !offered.contains(&Settings));
        assert_eq!(offered.len(), if linux { 6 } else { 5 });
    }
    assert!(!GALLERY.contains(&Agents) && !GALLERY.contains(&Settings));
}

#[test]
fn a_new_face_takes_its_own_id_while_it_is_free_else_the_lowest_free_one() {
    // The deck's own ids, in its faces' order.
    assert_eq!(
        [Agents, Omarchy, Macropad, Usage, Settings, Arcade, System, Timer].map(default_id),
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
    let list = board();
    assert_eq!(free_id(&list, Omarchy), Some(2));
    // A second Macropad: 3 is taken, 2 is the lowest free.
    assert_eq!(free_id(&list, Macropad), Some(2));
    assert_eq!(
        free_id(&[face(1, Agents), face(5, Settings)], Timer),
        Some(8)
    );
    assert_eq!(free_id(&[], Usage), Some(4));
    // All eight taken.
    let mut full = list.clone();
    full.push(face(2, Omarchy));
    assert_eq!(free_id(&full, Usage), None);
}

#[test]
fn a_face_is_added_before_settings_under_its_own_name_and_the_deck_takes_the_strip() {
    let list = board();
    let new = added(&list, Usage);
    assert_eq!(new.len(), 8);
    assert_eq!(
        new[6],
        FaceEntry {
            f: 2,
            t: Usage,
            n: "Usage".into()
        }
    );
    assert_eq!(new[7].t, Settings);
    assert_eq!(new[..6], list[..6]);
    // With no Settings at the end, at the end.
    assert_eq!(
        added(&[face(1, Agents)], Timer).last(),
        Some(&face(8, Timer))
    );
    // What the rules refuse changes nothing: a second Timer, a full strip.
    assert_eq!(added(&list, Timer), list);
    let full = Scene::FacesFull.fixture().unwrap().mirror.faces;
    assert_eq!(added(&full, Usage), full);
    // Every strip the page builds is one the deck takes: Agents first,
    // Settings last, ids unique in 1..=8, at most three Macropads.
    let mut built = vec![
        list.clone(),
        new,
        moved(&list, 2, Side::Right),
        removed(&list, 3),
    ];
    built.push(added(&added(&list, Macropad), Macropad));
    for strip in &built {
        assert_eq!(strip.first().map(|f| f.t), Some(Agents));
        assert_eq!(strip.last().map(|f| f.t), Some(Settings));
        let unique: BTreeSet<u8> = strip.iter().map(|f| f.f).collect();
        assert_eq!(unique.len(), strip.len());
        assert!(unique.iter().all(|f| (1..=8).contains(f)));
        assert!(strip.iter().filter(|f| f.t == Macropad).count() <= 3);
        assert!(strip.iter().all(|f| !f.n.trim().is_empty()));
    }
}

#[test]
fn the_page_reads_the_linked_decks_strip_else_the_one_the_service_keeps() {
    let mut mirror = Snapshot {
        faces: vec![face(1, Agents), face(5, Settings)],
        ..Snapshot::default()
    };
    // Linked, and the deck has not reported yet.
    assert_eq!(strip(&mirror, true), &mirror.faces[..]);
    mirror.device.faces = board();
    assert_eq!(strip(&mirror, true), &mirror.device.faces[..]);
    // Away: what the deck last reported may be stale; the service's is not.
    assert_eq!(strip(&mirror, false), &mirror.faces[..]);
}

#[test]
fn the_selection_is_the_face_picked_else_the_first_macropad_else_the_first_face() {
    let list = board();
    assert_eq!(selection(&list, Some(6)), Some(5));
    // Nothing picked, or a face no longer on the strip.
    assert_eq!(selection(&list, None), Some(1));
    assert_eq!(selection(&list, Some(2)), Some(1));
    assert_eq!(
        selection(&[face(1, Agents), face(5, Settings)], None),
        Some(0)
    );
    assert_eq!(selection(&[], Some(1)), None);
}

#[test]
fn a_strip_sent_is_waited_for_until_the_deck_reports_it_or_eight_seconds_pass() {
    let at = board_now();
    let sent = moved(&board(), 1, Side::Right);
    let pending = Pending {
        list: sent.clone(),
        at,
    };
    assert!(!pending.taken(&board()));
    assert!(pending.taken(&sent));
    // The names are the deck's to shorten.
    let renamed: Vec<FaceEntry> = sent
        .iter()
        .map(|f| FaceEntry {
            n: "x".into(),
            ..f.clone()
        })
        .collect();
    assert!(same_strip(&renamed, &sent));
    assert!(pending.taken(&renamed));
    assert!(!pending.expired(at.plus(Duration::from_millis(7_999))));
    assert!(pending.expired(at.plus(PENDING_HOLD)));
    assert_eq!(PENDING_HOLD, Duration::from_secs(8));
}

#[test]
fn a_macropad_removed_from_the_deck_takes_its_keys_once_the_deck_has_taken_it_off() {
    let mirror = board_snapshot();
    let list = board();
    let at = list
        .iter()
        .position(|face| face.f == BOARD_MACROPAD)
        .expect("the boards' Macropad");
    let forget = Forget::of(&list[at], &mirror).expect("its keys go with it");
    assert_eq!(forget.f, BOARD_MACROPAD);
    // Written empty, with no tag, under this app's layout.
    assert_eq!(
        forget.write.request(),
        (
            "device.macro.set",
            json!({"f": BOARD_MACROPAD, "layout": "app", "src": null, "keys": []})
        )
    );
    // Only once the deck reports a strip without it: the strip it still
    // reports, or a later one that has it again, keeps its keys.
    assert!(!forget.due(&list));
    assert!(forget.due(&removed(&list, at)));
    // Every other face has no keys to take, and nor does a Macropad the
    // service keeps none for, or only empty slots for.
    for face in list.iter().filter(|face| face.t != Macropad) {
        assert_eq!(Forget::of(face, &mirror), None, "{:?}", face.t);
    }
    let mut keyless = mirror.clone();
    keyless.macros.clear();
    assert_eq!(Forget::of(&list[at], &keyless), None);
    let mut blank = mirror.clone();
    if let Some(layout) = blank.macros.get_mut(&BOARD_MACROPAD) {
        layout.keys = vec![empty_key(), empty_key()];
    }
    assert_eq!(Forget::of(&list[at], &blank), None);
    // The service takes the write and keeps no keys for the face, so a
    // Macropad added in its place (the same id, while it is free) starts
    // blank.
    let started = Instant::now();
    let mut store = Store::new(StoreConfig::new(started, board_now(), 1));
    for action in [keys_write(BOARD_MACROPAD, None, board_keys()), forget.write] {
        let (method, params) = action.request();
        let request: Request =
            serde_json::from_value(json!({"method": method, "params": params})).unwrap();
        store
            .call(Principal::Uid(501), SessionId(1), request, started)
            .0
            .unwrap_or_else(|e| panic!("{method}: {e}"));
    }
    let kept = store.snapshot().macros.get(&BOARD_MACROPAD);
    assert!(
        kept.is_none_or(|layout| layout.keys.is_empty()),
        "{kept:?}"
    );
    let without = removed(&list, at);
    assert_eq!(free_id(&without, Macropad), Some(BOARD_MACROPAD));
}

#[test]
fn the_line_beside_the_buttons_says_what_the_strip_can_do() {
    i18n::install();
    assert_eq!(StripNote::of(true, false, false), StripNote::Limits);
    assert_eq!(StripNote::of(false, false, false), StripNote::NotLinked);
    assert_eq!(StripNote::of(false, false, true), StripNote::NotLinked);
    assert_eq!(StripNote::of(true, true, false), StripNote::Waiting);
    assert_eq!(StripNote::of(true, false, true), StripNote::Refused);
    assert_eq!(
        StripNote::Limits.words(EN),
        "Up to eight faces. Agents and Settings stay at the ends."
    );
    assert_eq!(StripNote::Waiting.words(ZH), "正在等待设备…");
    assert_eq!(
        StripNote::Refused.words(EN),
        "The deck did not take the change. Close its face manager and try again."
    );
    assert_eq!(
        StripNote::NotLinked.words(JA),
        "ページを変更するには、デバイスを接続してください。"
    );
}

#[test]
fn a_face_is_called_by_its_name_and_only_a_templates_own_name_is_translated() {
    i18n::install();
    assert_eq!(face_name(&face(1, Agents), ZH), "助手");
    assert_eq!(face_name(&face(3, Macropad), JA), "マクロパッド");
    assert_eq!(face_name(&face(6, Arcade), EN), "Arcade");
    // A name the owner gave is the owner's in every language.
    let named = FaceEntry {
        f: 3,
        t: Macropad,
        n: "build".into(),
    };
    assert_eq!(face_name(&named, ZH), "build");
    // Omarchy is a name; an empty name is the template's.
    assert_eq!(face_name(&face(2, Omarchy), ZH), "Omarchy");
    let blank = FaceEntry {
        f: 4,
        t: Usage,
        n: String::new(),
    };
    assert_eq!(face_name(&blank, ZH), "用量");
}

#[test]
fn under_the_strip_each_face_shows_what_it_does_on_the_mac() {
    assert_eq!(Panel::of(Macropad), Panel::Keys);
    assert_eq!(Panel::of(Usage), Panel::Usage);
    assert_eq!(Panel::of(Agents), Panel::Demo);
    assert_eq!(Panel::of(Arcade), Panel::ArcadeSound);
    for face in [System, Timer, Settings, Omarchy] {
        assert_eq!(Panel::of(face), Panel::Nothing, "{face:?}");
    }
}

// ---- the Usage face's sources ----------------------------------------------------

#[test]
fn the_usage_sources_are_read_from_config_get_and_written_whole() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json"
    ))
    .unwrap();
    let config = UsageConfig::parse(&fixture);
    assert_eq!(config.providers, ["codex"]);
    assert!(config.writable);
    assert!(config.on(CODEX) && !config.on(CLAUDE));
    assert_eq!(config.switched(CLAUDE, true), ["codex", "claude"]);
    assert!(config.switched(CODEX, false).is_empty());
    // A service whose TCP door is off — every one of this release, unless
    // its owner switched the door on — still publishes the switch, as
    // `false`, so the list stays writable.
    let mut door_off = fixture.clone();
    door_off["api"]["tcp"] = false.into();
    assert!(UsageConfig::parse(&door_off).writable);
    // An older service: the list is published, `api.tcp` is not, and the
    // write would be refused. The service matches names loosely, and so
    // does the page: a variant goes with its provider.
    let older = UsageConfig::parse(&json!({
        "api": {"port": 47201},
        "usage": {"providers": ["claude-statusline", "Codex"]}
    }));
    assert!(!older.writable);
    assert!(older.on(CLAUDE) && older.on(CODEX));
    assert_eq!(older.switched(CLAUDE, false), ["Codex"]);
    assert_eq!(older.switched(CODEX, true), ["claude-statusline", "codex"]);
    assert_eq!(UsageConfig::parse(&json!({})), UsageConfig::default());
    let (method, params) = usage_write(vec!["codex".into(), "claude".into()]).request();
    assert_eq!(method, "config.set");
    assert_eq!(
        params,
        json!({"patch": {"usage.providers": ["codex", "claude"]}})
    );
    let (_, none) = usage_write(Vec::new()).request();
    assert_eq!(none, json!({"patch": {"usage.providers": []}}));
}

// ---- shortcuts ----------------------------------------------------------------------

#[test]
fn the_pages_keys_are_the_decks_own_keys() {
    let source = include_str!("../../deck-proto/src/types.rs");
    let line = source
        .lines()
        .find(|line| line.starts_with("pub const KEYSTROKE_KEYS"))
        .expect("deck-proto names its keys");
    // `pub const KEYSTROKE_KEYS: &[&str] = &["a", "b", …];`
    let list = line
        .split_once("= &[")
        .unwrap()
        .1
        .split_once(']')
        .unwrap()
        .0;
    let names: Vec<&str> = list
        .split(',')
        .map(|name| name.trim().trim_matches('"'))
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(names, KEYS);
}

#[test]
fn every_shortcut_of_the_73_keys_and_16_modifier_sets_round_trips_and_the_service_takes_it() {
    let sets: Vec<Vec<Mod>> = (0..16u8)
        .map(|bits| {
            Mod::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, m)| m)
                .collect()
        })
        .collect();
    let mut all = Vec::new();
    for key in KEYS {
        for mods in &sets {
            let shortcut = Shortcut {
                mods: mods.clone(),
                key,
            };
            let text = shortcut.print();
            assert_eq!(Shortcut::parse(&text), Some(shortcut), "{text}");
            all.push(text);
        }
    }
    assert_eq!(all.len(), 73 * 16);
    // Written in the Mac's order whatever order it was given in.
    assert_eq!(
        Shortcut::parse("cmd+shift+alt+ctrl+a").unwrap().print(),
        "ctrl+opt+shift+cmd+a"
    );
    assert_eq!(Shortcut::parse(" opt+f5 ").unwrap().print(), "opt+f5");
    for bad in [
        "", "cmd", "cmd+", "+a", "a+b", "cmd+f13", "cmd+A", "hyper+a", "cmd++",
    ] {
        assert_eq!(Shortcut::parse(bad), None, "{bad:?}");
    }
    // The service checks every one with the deck's own rule, nine at a
    // time as a face holds them, and refuses a key the deck has not.
    let started = Instant::now();
    let mut store = Store::new(StoreConfig::new(started, board_now(), 1));
    let mut call = |keys: Vec<MacroKey>| {
        let (method, params) = keys_write(BOARD_MACROPAD, None, keys).request();
        let request: Request =
            serde_json::from_value(json!({"method": method, "params": params})).unwrap();
        store
            .call(Principal::Uid(501), SessionId(1), request, started)
            .0
            .is_ok()
    };
    for chunk in all.chunks(SLOTS) {
        let keys = chunk
            .iter()
            .map(|text| key("k", None, Some(text)))
            .collect();
        assert!(call(keys), "{chunk:?}");
    }
    assert!(!call(vec![key("k", None, Some("cmd+insert"))]));
}

#[test]
fn a_key_pressed_in_the_window_is_the_decks_shortcut_for_it() {
    let cases = [
        // Letters and digits; a capital is Shift and the letter.
        (stroke("cmd", "b"), Some("cmd+b")),
        (stroke("shift cmd", "d"), Some("shift+cmd+d")),
        (stroke("cmd", "A"), Some("shift+cmd+a")),
        (stroke("ctrl", "4"), Some("ctrl+4")),
        // macOS reports Shift and 4 as `$`, with no Shift.
        (stroke("cmd", "$"), Some("shift+cmd+4")),
        (stroke("", "!"), Some("shift+1")),
        (stroke("opt", ")"), Some("opt+shift+0")),
        // Function keys, as far as the deck goes.
        (stroke("", "f5"), Some("f5")),
        (stroke("opt", "f12"), Some("opt+f12")),
        (stroke("", "f13"), None),
        // Named keys.
        (stroke("cmd", "escape"), Some("cmd+esc")),
        (stroke("", "enter"), Some("enter")),
        (stroke("ctrl", "tab"), Some("ctrl+tab")),
        (stroke("opt", "space"), Some("opt+space")),
        (stroke("cmd", "backspace"), Some("cmd+backspace")),
        (stroke("", "delete"), Some("delete")),
        (stroke("shift", "up"), Some("shift+up")),
        (stroke("", "pageup"), Some("pageup")),
        (stroke("", "home"), Some("home")),
        // Punctuation, and its shifted symbols.
        (stroke("cmd", "-"), Some("cmd+minus")),
        (stroke("cmd", "_"), Some("shift+cmd+minus")),
        (stroke("", "="), Some("equal")),
        (stroke("", "+"), Some("shift+equal")),
        (stroke("", "["), Some("lbracket")),
        (stroke("", "{"), Some("shift+lbracket")),
        (stroke("", "]"), Some("rbracket")),
        (stroke("", "\\"), Some("backslash")),
        (stroke("", "|"), Some("shift+backslash")),
        (stroke("", ";"), Some("semicolon")),
        (stroke("", ":"), Some("shift+semicolon")),
        (stroke("", "'"), Some("quote")),
        (stroke("", "\""), Some("shift+quote")),
        (stroke("", ","), Some("comma")),
        (stroke("", "<"), Some("shift+comma")),
        (stroke("", "."), Some("period")),
        (stroke("", ">"), Some("shift+period")),
        (stroke("", "/"), Some("slash")),
        (stroke("", "?"), Some("shift+slash")),
        (stroke("ctrl", "`"), Some("ctrl+grave")),
        (stroke("", "~"), Some("shift+grave")),
        // Keys the deck has no name for.
        (stroke("", "insert"), None),
        (stroke("", "é"), None),
        (stroke("cmd", "f20"), None),
    ];
    for (stroke, want) in cases {
        assert_eq!(from_gpui(&stroke).as_deref(), want, "{stroke:?}");
        if let Some(text) = want {
            assert!(Shortcut::parse(text).is_some(), "{text}");
        }
    }
    // A modifier pressed and let go alone is not a key.
    for name in ["shift", "control", "alt", "platform", "function"] {
        assert!(modifier_only(&stroke("", name)), "{name}");
        assert_eq!(from_gpui(&stroke("", name)), None);
    }
    assert!(!modifier_only(&stroke("shift", "a")));
}

#[test]
fn a_label_fits_in_twelve_bytes_twelve_letters_or_four_chinese_characters() {
    assert_eq!(LABEL_BYTES, 12);
    assert!(label_fits("screenshot12"));
    assert!(!label_fits("screenshot123"));
    assert!(label_fits("截图工具"));
    assert!(!label_fits("截图工具箱"));
    assert!(label_fits("ビルド"));
    assert!(!label_fits("スクリーンショット"));
    // Mixed: three bytes a character, one a letter.
    assert!(label_fits("git 推送"));
    assert!(label_fits("ab 推送中"));
    assert!(!label_fits("abc 推送中"));
    assert!(label_fits(""));
}

#[test]
fn a_control_given_to_one_key_is_taken_from_the_key_that_had_it() {
    let keys = board_keys();
    assert_eq!(keys[0].binding, Some(Binding::Left));
    assert_eq!(keys[4].binding, None);
    // The screenshot key takes ‹ from build.
    let mut draft = KeyDraft::of(&keys[4]);
    draft.binding = Some(Binding::Left);
    let new = placed(&keys, 4, draft.key());
    assert_eq!(new[4].binding, Some(Binding::Left));
    assert_eq!(new[0].binding, None);
    assert_eq!(new[0].label, "build");
    for at in [1, 2, 3, 5, 6, 7] {
        assert_eq!(new[at], keys[at], "{at}");
    }
    // Touch only takes nothing from anyone.
    let mut draft = KeyDraft::of(&keys[1]);
    draft.binding = None;
    let new = placed(&keys, 1, draft.key());
    assert_eq!(new[1].binding, None);
    assert_eq!(new[0].binding, Some(Binding::Left));
    // A key keeps its own control when given it again.
    assert_eq!(placed(&keys, 0, keys[0].clone()), keys);
}

#[test]
fn clearing_a_key_keeps_the_keys_after_it_and_the_list_ends_at_its_last_key() {
    let keys = board_keys();
    assert_eq!(keys.len(), 8);
    // A key in the middle: the slot is empty and the rest stay put.
    let new = cleared(&keys, 2);
    assert_eq!(new.len(), 8);
    assert_eq!(new[2], empty_key());
    assert_eq!(slot(&new, 2), None);
    assert_eq!(new[3..], keys[3..]);
    // The last keys: the empty slots after the last key are dropped.
    let new = cleared(&cleared(&keys, 6), 7);
    assert_eq!(new.len(), 6);
    assert_eq!(new[..], keys[..6]);
    assert_eq!(cleared(&keys, 8), keys);
    assert_eq!(
        trimmed(vec![key("a", None, None), empty_key(), empty_key()]),
        [key("a", None, None)]
    );
    // A key given to an empty slot past the end pads the slots before it.
    let new = placed(&keys[..2], 5, key("deploy", None, None));
    assert_eq!(new.len(), 6);
    assert!((2..5).all(|at| slot(&new, at).is_none()));
    assert_eq!(slot(&new, 5), Some(&key("deploy", None, None)));
    // There are nine slots and no tenth.
    assert_eq!(SLOTS, 9);
    assert_eq!(placed(&keys, 9, key("x", None, None)), keys);
}

#[test]
fn a_key_reads_back_as_it_was_and_its_modifiers_need_a_key() {
    for key in board_keys() {
        assert_eq!(KeyDraft::of(&key).key(), key);
    }
    let mut draft = KeyDraft::of(&key("mute", None, None));
    // No key, no modifiers.
    draft.set_mod(Mod::Shift, true);
    assert_eq!(draft.shortcut, None);
    draft.set_key(Some("4"));
    draft.set_mod(Mod::Command, true);
    draft.set_mod(Mod::Shift, true);
    assert_eq!(draft.key().keys.as_deref(), Some("shift+cmd+4"));
    // Another key keeps the modifiers; taking one off leaves the rest.
    draft.set_key(Some("d"));
    draft.set_mod(Mod::Shift, false);
    assert_eq!(draft.key().keys.as_deref(), Some("cmd+d"));
    // None types nothing, and keeps no modifiers either.
    draft.set_key(None);
    assert_eq!(draft.key().keys, None);
    draft.set_key(Some("q"));
    assert_eq!(draft.key().keys.as_deref(), Some("q"));
}

#[test]
fn keys_are_named_as_their_caps_print_them_and_only_six_names_are_translated() {
    i18n::install();
    assert_eq!(key_name("4", ZH), "4");
    assert_eq!(key_name("b", EN), "B");
    assert_eq!(key_name("f5", JA), "F5");
    assert_eq!(key_name("enter", ZH), "Return");
    assert_eq!(key_name("esc", EN), "Escape");
    // The Mac's Delete deletes backwards; the deck's `delete` is forward.
    assert_eq!(key_name("backspace", ZH), "Delete");
    assert_eq!(key_name("delete", EN), "Forward Delete");
    assert_eq!(key_name("delete", ZH), "向前删除");
    assert_eq!(key_name("space", ZH), "空格键");
    assert_eq!(key_name("up", JA), "上矢印");
    assert_eq!(key_name("pagedown", ZH), "Page Down");
    assert_eq!(key_name("grave", EN), "`");
    assert_eq!(key_name("lbracket", EN), "[");
    let names: BTreeSet<String> = KEYS.iter().map(|key| key_name(key, EN)).collect();
    assert_eq!(names.len(), KEYS.len());
    let translated: Vec<&str> = KEYS
        .iter()
        .copied()
        .filter(|key| key_name(key, ZH) != key_name(key, EN))
        .collect();
    assert_eq!(
        translated,
        ["up", "down", "left", "right", "space", "delete"]
    );
    // The corner of a key: the board's caps, and a mark Geist Mono draws.
    let caps: Vec<String> = ["b", "4", "grave", "enter", "up", "backspace", "esc", "f11"]
        .iter()
        .map(|key| key_cap(key))
        .collect();
    assert_eq!(
        caps,
        ["B", "4", "`", "\u{21B5}", "\u{2191}", "\u{232B}", "esc", "F11"]
    );
    let caps: BTreeSet<String> = KEYS.iter().map(|key| key_cap(key)).collect();
    assert_eq!(caps.len(), KEYS.len());
}

#[test]
fn each_control_has_the_mark_the_deck_draws_on_its_keys() {
    i18n::install();
    assert_eq!(
        BINDINGS.map(binding_mark),
        ["‹", "›", "↵", "CW", "CCW", "SHIFT ‹", "SHIFT ›", "SHIFT ↵"]
    );
    assert_eq!(binding_word(None, EN), "Touch only");
    assert_eq!(binding_word(None, ZH), "仅触摸");
    assert_eq!(binding_word(Some(Binding::ShiftLeft), ZH), "SHIFT ‹");
}

#[test]
fn accessibility_is_what_perm_status_says_and_nothing_when_it_says_nothing() {
    assert_eq!(
        Access::parse(&json!({"accessibility": "granted"})),
        Some(Access::Granted)
    );
    assert_eq!(
        Access::parse(&json!({"accessibility": "denied"})),
        Some(Access::Denied)
    );
    assert_eq!(
        Access::parse(&json!({"accessibility": "unknown"})),
        Some(Access::NotChecked)
    );
    assert_eq!(Access::parse(&json!({"microphone": "granted"})), None);
    assert_eq!(Access::parse(&json!({"accessibility": true})), None);
}

#[test]
fn keys_sent_are_shown_until_the_mirror_agrees_or_two_seconds_pass() {
    let mirror = board_keys();
    let at = board_now();
    let sent = Sent {
        f: BOARD_MACROPAD,
        keys: cleared(&mirror, 4),
        at,
    };
    assert_eq!(
        keys_shown(&mirror, BOARD_MACROPAD, Some(&sent), at),
        &sent.keys[..]
    );
    // Another face's keys, nothing sent, or the hold over: the mirror's.
    assert_eq!(keys_shown(&mirror, 9, Some(&sent), at), &mirror[..]);
    assert_eq!(keys_shown(&mirror, BOARD_MACROPAD, None, at), &mirror[..]);
    let later = at.plus(KEYS_HOLD);
    assert_eq!(
        keys_shown(&mirror, BOARD_MACROPAD, Some(&sent), later),
        &mirror[..]
    );
    assert!(sent.holds(at.plus(Duration::from_millis(1_999))));
    assert_eq!(KEYS_HOLD, Duration::from_secs(2));
}

// ---- the writes -----------------------------------------------------------------------

#[test]
fn a_strip_and_a_faces_keys_each_go_out_whole() {
    let (method, params) = strip_write(board()).request();
    assert_eq!(method, "device.faces.set");
    let list = params["list"].as_array().unwrap();
    assert_eq!(list.len(), 7);
    assert_eq!(list[1], json!({"f": 3, "t": "macropad", "n": "Macropad"}));
    let keys = vec![
        key("build", Some(Binding::Left), Some("cmd+b")),
        empty_key(),
        key("lock", None, Some("ctrl+cmd+q")),
        empty_key(),
        empty_key(),
    ];
    let (method, params) = keys_write(3, Some("deck.toml".into()), keys).request();
    assert_eq!(method, "device.macro.set");
    assert_eq!(
        params,
        json!({
            "f": 3,
            "layout": "app",
            "src": "deck.toml",
            "keys": [
                {"label": "build", "binding": "left", "keys": "cmd+b"},
                {"label": "", "binding": null},
                {"label": "lock", "binding": null, "keys": "ctrl+cmd+q"},
            ],
        })
    );
    // The boards' strips and keys are ones the service takes as they are.
    let started = Instant::now();
    let mut store = Store::new(StoreConfig::new(started, board_now(), 1));
    let mut call = |action: deck_gui_core::UiAction| {
        let (method, params) = action.request();
        let request: Request =
            serde_json::from_value(json!({"method": method, "params": params})).unwrap();
        store
            .call(Principal::Uid(501), SessionId(1), request, started)
            .0
            .unwrap_or_else(|e| panic!("{method}: {e}"));
    };
    call(keys_write(BOARD_MACROPAD, None, board_keys()));
    call(strip_write(added(&board(), System)));
    call(strip_write(moved(&board(), 3, Side::Left)));
}

#[test]
fn the_boards_deck_is_linked_over_bluetooth_with_its_macropad_second() {
    let board = board_snapshot();
    assert_eq!(board.device.transport, Some(Transport::Bluetooth));
    assert_eq!(board.faces[1].t, Macropad);
    assert_eq!(board.faces[1].f, BOARD_MACROPAD);
    assert_eq!(board_keys()[4].keys.as_deref(), Some("shift+cmd+4"));
}

// ---- the words ------------------------------------------------------------------------

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::faces::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 52 rows, copied as written.
    assert_eq!(i18n::faces::STRINGS.len(), 52);
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = concat!(
        include_str!("../src/faces.rs"),
        include_str!("../src/keys.rs")
    );
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate.
    assert_eq!(i18n::literals(source).len(), 71);
}

/// A fresh test service's own replies (`deck-gui-core`'s recorded fixtures,
/// `fixtures/daemon/recorded/`): no deck ever linked, `[usage] providers =
/// []`, and no Macropad key pressed yet.
const RECORDED_CONFIG: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/config.get.json");
const RECORDED_PERM: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/perm.status.json");
const RECORDED_HELLO: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/recorded/session.hello.json");

fn recorded(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("a recorded reply is JSON")
}

#[test]
fn a_fresh_test_services_own_replies_leave_the_strip_empty_and_both_usage_switches_off() {
    // The Usage face's sources: none, and the list can be written.
    let usage = UsageConfig::parse(&recorded(RECORDED_CONFIG));
    assert_eq!(
        usage,
        UsageConfig {
            providers: Vec::new(),
            writable: true
        }
    );
    assert!(!usage.on(CLAUDE) && !usage.on(CODEX));
    // Accessibility: no key has typed yet, so not checked (with Check…).
    assert_eq!(
        Access::parse(&recorded(RECORDED_PERM)),
        Some(Access::NotChecked)
    );
    // A deck never linked: the service keeps no strip for it, so nothing is
    // selected and nothing is drawn under the strip; Add a face is there,
    // faint until a deck links.
    let snapshot: Snapshot = serde_json::from_value(recorded(RECORDED_HELLO)["snapshot"].clone())
        .expect("the hello's snapshot");
    let shown = strip(&snapshot, false);
    assert!(shown.is_empty());
    assert_eq!(selection(shown, None), None);
    assert!(!addable(shown, false).is_empty());
    // The keys the service keeps for face 3 are there for when one is.
    let layout = snapshot.macros.get(&3).expect("face 3's keys");
    let keys = keys_shown(&layout.keys, 3, None, board_now());
    assert_eq!(keys.len(), 8);
    assert_eq!(
        (keys[0].label.as_str(), keys[0].keys.as_deref()),
        ("build", None)
    );
}
