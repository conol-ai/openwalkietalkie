//! The Macropad's keys on the Faces & Keys page (boards FacesKeys,
//! FacesKeysLight): the face's nine keys as a 3×3 grid, the key selected in
//! it, and the model both are drawn from.
//!
//! **What a face's keys are.** Nine slots, slot 0 first. An empty label is an
//! empty slot, which keeps the keys after it in their places, and the list
//! the service is given ends at its last key ([`trimmed`]). A key has a
//! label of at most twelve bytes — twelve letters or four Chinese
//! characters ([`label_fits`]) — the deck control that presses it or none
//! (touch only), and the shortcut it types or none: `ctrl+opt+shift+cmd+<key>`
//! with any of the four modifiers, written in that order, and one key of
//! the deck's 73 ([`KEYS`], [`Shortcut`]).
//!
//! **Every change sends the face's whole list** (`device.macro.set`): the
//! deck takes no single key. The service keeps the list and says so with
//! `device.macros`, which the mirror folds; until it does, the grid shows
//! what was sent ([`Sent`]), so a key does not flick back for a round trip.
//! A control another key had moves to the key it is given ([`placed`]).
//! Editing needs the service and not the deck: the service keeps the keys
//! and gives them to the deck when it links.
//!
//! **Record** takes the next key pressed in the window, before any binding
//! of the window sees it, so Tab and Return can be recorded too; Escape
//! alone cancels, and a key the deck cannot send is said and not taken
//! ([`from_gpui`]).
//!
//! **Accessibility** is what the service last learned from a key it typed,
//! or from asking macOS (`perm.status` / `perm.request`): the deck types a
//! shortcut only with it.
use crate::present;
use crate::shell::{Screen, Shell};
use crate::style::{deck_colors, hex, Palette};
use crate::ui::{self, canonical, Btn, Chip, Layout, Mod, PopupButton, Press, Row, Type as _};
use deck_gui_core::i18n::Language;
use deck_gui_core::UiAction;
use deck_model::{Binding, MacroKey, Template, Tone, Ts};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use std::time::Duration;

/// A Macropad face's slots.
pub const SLOTS: usize = 9;

/// The longest label the deck draws, in bytes of UTF-8: twelve letters, or
/// four Chinese characters.
pub const LABEL_BYTES: usize = 12;

/// The name the service keeps for a face whose keys were set here.
pub const LAYOUT: &str = "app";

/// How long the grid shows the keys it sent without the mirror agreeing.
pub const KEYS_HOLD: Duration = Duration::from_millis(2_000);

/// The keys a shortcut may name, as the deck names them (the deck's own
/// `KEYS` table, which the service checks every shortcut against).
pub const KEYS: [&str; 73] = [
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "up",
    "down",
    "left",
    "right",
    "enter",
    "esc",
    "tab",
    "space",
    "backspace",
    "delete",
    "home",
    "end",
    "pageup",
    "pagedown",
    "minus",
    "equal",
    "lbracket",
    "rbracket",
    "backslash",
    "semicolon",
    "quote",
    "comma",
    "period",
    "slash",
    "grave",
];

/// The deck's controls a key can be pressed by, in the order its own editor
/// offers them. CANCEL and PTT never press a key.
pub const BINDINGS: [Binding; 8] = [
    Binding::Left,
    Binding::Right,
    Binding::Enter,
    Binding::Cw,
    Binding::Ccw,
    Binding::ShiftLeft,
    Binding::ShiftRight,
    Binding::ShiftEnter,
];

// The grid and the inspector, in the design's numbers.
/// The grid's column, beside the inspector.
const GRID_WIDTH: f32 = 420.;
/// Keys to a row of the grid, as on the deck's face.
const COLUMNS: u16 = 3;
/// A key: its height and its corner.
const KEY_HEIGHT: f32 = 80.;
const KEY_RADIUS: f32 = 14.;
/// An empty slot's dashed edge.
const ASSIGN_EDGE: f32 = 1.5;
/// The inspector's names, in a column of their own.
const NAME_WIDTH: f32 = 84.;
/// An inspector line, and one with a note under it. The card's rule is
/// drawn over a line's last pixel, as the design's own border is.
const FIELD: f32 = 44.;
const FIELD_WITH_NOTE: f32 = 60.;
/// The two menu buttons' widths.
const BINDING_WIDTH: f32 = 132.;
const KEY_WIDTH: f32 = 96.;

/// Where a denied Accessibility grant is lifted.
pub const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

// ---- shortcuts ---------------------------------------------------------------

/// A shortcut as the deck spells it: modifiers in the Mac's order, then one
/// of [`KEYS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub mods: Vec<Mod>,
    pub key: &'static str,
}

impl Shortcut {
    /// `ctrl+opt+shift+cmd+<key>`, the modifiers in any order (`alt` is
    /// `opt`), exactly one key of [`KEYS`]. `None` for anything else.
    pub fn parse(text: &str) -> Option<Shortcut> {
        let mut mods = Vec::new();
        let mut key = None;
        for part in text.trim().split('+') {
            if let Some(m) = modifier(part) {
                mods.push(m);
                continue;
            }
            let known = KEYS.iter().copied().find(|name| *name == part)?;
            if key.replace(known).is_some() {
                return None;
            }
        }
        Some(Shortcut {
            mods: canonical(&mods),
            key: key?,
        })
    }

    /// The shortcut the deck's way: its modifiers in the Mac's order, then
    /// the key, joined by `+`.
    pub fn print(&self) -> String {
        canonical(&self.mods)
            .into_iter()
            .map(modifier_word)
            .chain([self.key])
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// The deck's word for a modifier.
const fn modifier_word(m: Mod) -> &'static str {
    match m {
        Mod::Control => "ctrl",
        Mod::Option => "opt",
        Mod::Shift => "shift",
        Mod::Command => "cmd",
    }
}

/// The modifier a word of a shortcut names.
fn modifier(word: &str) -> Option<Mod> {
    match word {
        "ctrl" => Some(Mod::Control),
        "opt" | "alt" => Some(Mod::Option),
        "shift" => Some(Mod::Shift),
        "cmd" => Some(Mod::Command),
        _ => None,
    }
}

/// A key pressed in the window, as the deck's shortcut for it; `None` for
/// a key the deck cannot send (F13, Insert, a letter of another alphabet).
///
/// macOS names a shifted symbol by what it types and drops the Shift (Shift
/// and 4 arrive as `$`), so a symbol is turned back into its key with Shift
/// on, as the deck's US key names have it.
pub fn from_gpui(stroke: &gpui::Keystroke) -> Option<String> {
    let (key, shifted) = deck_key(&stroke.key)?;
    let held = &stroke.modifiers;
    let mods: Vec<Mod> = [
        (held.control, Mod::Control),
        (held.alt, Mod::Option),
        (held.shift || shifted, Mod::Shift),
        (held.platform, Mod::Command),
    ]
    .into_iter()
    .filter_map(|(on, m)| on.then_some(m))
    .collect();
    Some(Shortcut { mods, key }.print())
}

/// A modifier pressed and let go alone: gpui reports it as a key named
/// after the modifier, which is not a key to record.
pub fn modifier_only(stroke: &gpui::Keystroke) -> bool {
    matches!(
        stroke.key.as_str(),
        "shift" | "control" | "alt" | "platform" | "function"
    )
}

/// The deck's name for the key gpui calls `name`, and whether `name` was the
/// key's shifted symbol.
fn deck_key(name: &str) -> Option<(&'static str, bool)> {
    let plain = match name {
        "escape" => Some("esc"),
        "-" => Some("minus"),
        "=" => Some("equal"),
        "[" => Some("lbracket"),
        "]" => Some("rbracket"),
        "\\" => Some("backslash"),
        ";" => Some("semicolon"),
        "'" => Some("quote"),
        "," => Some("comma"),
        "." => Some("period"),
        "/" => Some("slash"),
        "`" => Some("grave"),
        // Every other key the deck has is named the same by gpui.
        name => KEYS.iter().copied().find(|key| *key == name),
    };
    if let Some(key) = plain {
        return Some((key, false));
    }
    let shifted = match name {
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        "_" => "minus",
        "+" => "equal",
        "{" => "lbracket",
        "}" => "rbracket",
        "|" => "backslash",
        ":" => "semicolon",
        "\"" => "quote",
        "<" => "comma",
        ">" => "period",
        "?" => "slash",
        "~" => "grave",
        // A capital letter, which some layouts report for Shift and a
        // letter.
        name => {
            let lower = name.to_ascii_lowercase();
            if lower == name {
                return None;
            }
            return KEYS
                .iter()
                .copied()
                .find(|key| key.len() == 1 && *key == lower)
                .map(|key| (key, true));
        }
    };
    KEYS.iter()
        .copied()
        .find(|key| *key == shifted)
        .map(|key| (key, true))
}

/// A key's name as the Key menu and its button show it: as the Mac prints
/// it on its key cap (letters, digits, F1 to F12, Return, Escape, Tab,
/// Delete, Home, End, Page Up, Page Down and the punctuation are not
/// translated), and in the window's language for Space, the arrows and
/// Forward Delete, which macOS names in words.
pub fn key_name(key: &str, language: Language) -> String {
    match key {
        "space" => language.text("Space").to_string(),
        "up" => language.text("Up Arrow").to_string(),
        "down" => language.text("Down Arrow").to_string(),
        "left" => language.text("Left Arrow").to_string(),
        "right" => language.text("Right Arrow").to_string(),
        "delete" => language.text("Forward Delete").to_string(),
        "enter" => "Return".to_string(),
        "esc" => "Escape".to_string(),
        "tab" => "Tab".to_string(),
        // The Mac's own Delete key is the one that deletes backwards.
        "backspace" => "Delete".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "Page Up".to_string(),
        "pagedown" => "Page Down".to_string(),
        other => key_print(other),
    }
}

/// A key as a macro key's corner draws it, in mono beside the modifier
/// marks: the character on its cap, or the Mac's mark for it where Geist
/// Mono has one.
pub fn key_cap(key: &str) -> String {
    match key {
        "up" => "\u{2191}".to_string(),
        "down" => "\u{2193}".to_string(),
        "left" => "\u{2190}".to_string(),
        "right" => "\u{2192}".to_string(),
        "enter" => "\u{21B5}".to_string(),
        "tab" => "\u{21E5}".to_string(),
        "space" => "\u{2423}".to_string(),
        "backspace" => "\u{232B}".to_string(),
        "delete" => "\u{2326}".to_string(),
        "home" => "\u{2196}".to_string(),
        "end" => "\u{2198}".to_string(),
        "esc" => "esc".to_string(),
        "pageup" => "PgUp".to_string(),
        "pagedown" => "PgDn".to_string(),
        other => key_print(other),
    }
}

/// A character key, a digit or an F key as its cap prints it.
fn key_print(key: &str) -> String {
    let symbol = match key {
        "minus" => "-",
        "equal" => "=",
        "lbracket" => "[",
        "rbracket" => "]",
        "backslash" => "\\",
        "semicolon" => ";",
        "quote" => "'",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "grave" => "`",
        other => return other.to_ascii_uppercase(),
    };
    symbol.to_string()
}

/// The mark a deck control has on the deck's own keys, as its Macropad face
/// draws it in a key's corner.
pub const fn binding_mark(binding: Binding) -> &'static str {
    match binding {
        Binding::Left => "\u{2039}",
        Binding::Right => "\u{203A}",
        Binding::Enter => "\u{21B5}",
        Binding::Cw => "CW",
        Binding::Ccw => "CCW",
        Binding::ShiftLeft => "SHIFT \u{2039}",
        Binding::ShiftRight => "SHIFT \u{203A}",
        Binding::ShiftEnter => "SHIFT \u{21B5}",
    }
}

/// What the Deck button menu says for a key: the control's mark, or "Touch
/// only".
pub fn binding_word(binding: Option<Binding>, language: Language) -> &'static str {
    match binding {
        Some(binding) => binding_mark(binding),
        None => language.text("Touch only"),
    }
}

// ---- a key, and a face's keys --------------------------------------------------

/// One key as the inspector edits it: its label, the deck control that
/// presses it, and its shortcut taken apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDraft {
    pub label: String,
    pub binding: Option<Binding>,
    pub shortcut: Option<Shortcut>,
}

impl KeyDraft {
    /// `key` taken apart. A shortcut outside the deck's grammar reads as
    /// none: the service refuses to store one, so none is there to read.
    pub fn of(key: &MacroKey) -> KeyDraft {
        KeyDraft {
            label: key.label.clone(),
            binding: key.binding,
            shortcut: key.keys.as_deref().and_then(Shortcut::parse),
        }
    }

    /// The key again, its shortcut written the deck's way.
    pub fn key(&self) -> MacroKey {
        MacroKey {
            label: self.label.clone(),
            binding: self.binding,
            keys: self.shortcut.as_ref().map(Shortcut::print),
        }
    }

    /// Type `key`, keeping the modifiers it had; `None` types nothing, and
    /// a shortcut without a key keeps no modifiers either.
    pub fn set_key(&mut self, key: Option<&'static str>) {
        self.shortcut = key.map(|key| Shortcut {
            mods: self
                .shortcut
                .as_ref()
                .map(|shortcut| shortcut.mods.clone())
                .unwrap_or_default(),
            key,
        });
    }

    /// Hold `m` with the key, or not. A key that types nothing has no
    /// modifiers to hold.
    pub fn set_mod(&mut self, m: Mod, on: bool) {
        if let Some(shortcut) = &mut self.shortcut {
            let mut mods: Vec<Mod> = shortcut.mods.iter().copied().filter(|x| *x != m).collect();
            if on {
                mods.push(m);
            }
            shortcut.mods = canonical(&mods);
        }
    }
}

/// The key in slot `at`, when the slot holds one.
pub fn slot(keys: &[MacroKey], at: usize) -> Option<&MacroKey> {
    keys.get(at).filter(|key| !key.label.is_empty())
}

/// An empty slot.
pub fn empty_key() -> MacroKey {
    MacroKey {
        label: String::new(),
        binding: None,
        keys: None,
    }
}

/// `keys` without the empty slots after its last key.
pub fn trimmed(mut keys: Vec<MacroKey>) -> Vec<MacroKey> {
    while keys.last().is_some_and(|key| key.label.is_empty()) {
        keys.pop();
    }
    keys
}

/// `keys` with `key` in slot `at`. The control `key` is pressed by is
/// taken from any other key that had it, as the deck's own editor does: one
/// control presses one key.
pub fn placed(keys: &[MacroKey], at: usize, key: MacroKey) -> Vec<MacroKey> {
    let mut keys = keys.to_vec();
    if at >= SLOTS {
        return trimmed(keys);
    }
    if keys.len() <= at {
        keys.resize(at + 1, empty_key());
    }
    if let Some(binding) = key.binding {
        for (i, other) in keys.iter_mut().enumerate() {
            if i != at && other.binding == Some(binding) {
                other.binding = None;
            }
        }
    }
    keys[at] = key;
    trimmed(keys)
}

/// `keys` with slot `at` emptied: the keys after it stay where they are.
pub fn cleared(keys: &[MacroKey], at: usize) -> Vec<MacroKey> {
    let mut keys = keys.to_vec();
    if let Some(key) = keys.get_mut(at) {
        *key = empty_key();
    }
    trimmed(keys)
}

/// Whether the deck draws `label` whole: twelve bytes of UTF-8 at most.
pub fn label_fits(label: &str) -> bool {
    label.len() <= LABEL_BYTES
}

/// What committing the Label field does with the words it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelCommit {
    /// They are the key's label already: nothing to send.
    Same,
    /// The field takes the key's label back: it was emptied (**Clear This
    /// Key** removes a key, an empty label does not), or its words were
    /// given for another key — another slot, another face, or the last
    /// deck in the hand — and never for this one.
    Restore,
    /// Longer than the deck draws: said, and not sent.
    TooLong,
    /// Send this label for the selected key.
    Send(String),
}

/// The Label field's rule. `typed` is what it holds, `given` the face and
/// slot it was last given words for (none since another deck came into the
/// hand), `f` and `at` the key selected now, and `was` that key's label.
pub fn label_commit(
    typed: &str,
    given: Option<(u8, usize)>,
    f: u8,
    at: usize,
    was: &str,
) -> LabelCommit {
    let typed = typed.trim();
    if typed == was {
        LabelCommit::Same
    } else if typed.is_empty() || given != Some((f, at)) {
        LabelCommit::Restore
    } else if !label_fits(typed) {
        LabelCommit::TooLong
    } else {
        LabelCommit::Send(typed.to_string())
    }
}

/// What sets a face's keys: the whole list, the empty slots at its end
/// dropped, under this app's layout name and with the tag the deck shows
/// in the face's header as it was.
pub fn keys_write(f: u8, src: Option<String>, keys: Vec<MacroKey>) -> UiAction {
    UiAction::DeviceMacroSet {
        f,
        layout: LAYOUT.to_string(),
        src,
        keys: trimmed(keys),
    }
}

/// Keys sent for a face, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub f: u8,
    pub keys: Vec<MacroKey>,
    pub at: Ts,
}

impl Sent {
    /// Whether it is still shown at `now`: until [`KEYS_HOLD`] has passed.
    pub fn holds(&self, now: Ts) -> bool {
        u128::from(now.millis().saturating_sub(self.at.millis())) < KEYS_HOLD.as_millis()
    }
}

/// The keys the grid shows for face `f`: the ones sent for it while they
/// still hold, else the mirror's.
pub fn keys_shown<'a>(
    mirror: &'a [MacroKey],
    f: u8,
    sent: Option<&'a Sent>,
    now: Ts,
) -> &'a [MacroKey] {
    match sent {
        Some(sent) if sent.f == f && sent.holds(now) => &sent.keys,
        _ => mirror,
    }
}

// ---- Accessibility -----------------------------------------------------------

/// What the service knows of the Accessibility grant the deck's shortcuts
/// need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    /// Nothing has asked yet: no key has typed, and nobody pressed Check….
    NotChecked,
    Denied,
}

impl Access {
    /// `perm.status`'s `accessibility`. `None` from a service that does not
    /// report it: the row is left out rather than guessed.
    pub fn parse(result: &Value) -> Option<Access> {
        Some(match result.get("accessibility")?.as_str()? {
            "granted" => Access::Granted,
            "denied" => Access::Denied,
            _ => Access::NotChecked,
        })
    }
}

// ---- the pane ----------------------------------------------------------------

/// The key editor's own state, which the Faces page's pane holds.
pub struct KeysPane {
    /// The slot selected in the grid.
    slot: usize,
    /// The Label field.
    label: Entity<InputState>,
    /// The face and slot the Label field was last given words for, and the
    /// words.
    given: Option<(u8, usize, String)>,
    /// The label last committed was longer than the deck draws.
    too_long: bool,
    /// **Record** is waiting for a key.
    recording: bool,
    /// The keys last sent, shown until the mirror agrees.
    sent: Option<Sent>,
    /// What `perm.status` said of Accessibility.
    access: Option<Access>,
    /// A `perm.status` is on its way.
    asking: bool,
}

impl KeysPane {
    /// The pane, with its Label field and the two subscriptions it needs
    /// kept: the field's commit, and **Record**'s look at every key before
    /// the window's bindings see it.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> (KeysPane, Vec<Subscription>) {
        let label = cx.new(|cx| InputState::new(window, cx));
        // A label is checked when it is committed, never while it is typed:
        // an input method in the middle of a word is never interrupted.
        let commit = cx.subscribe_in(&label, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_label(window, cx);
            }
        });
        let shell = cx.weak_entity();
        let record = cx.intercept_keystrokes(move |event, window, cx| {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !shell.read(cx).faces.keys.recording {
                return;
            }
            let taken = shell.update(cx, |this, cx| this.record_key(&event.keystroke, window, cx));
            if taken {
                cx.stop_propagation();
            }
        });
        let pane = KeysPane {
            slot: 0,
            label,
            given: None,
            too_long: false,
            recording: false,
            sent: None,
            access: None,
            asking: false,
        };
        (pane, vec![commit, record])
    }

    /// Back to the first key, with nothing left waiting: another face was
    /// selected.
    pub(crate) fn reset(&mut self) {
        self.slot = 0;
        self.too_long = false;
        self.recording = false;
    }

    /// Another deck is in the hand, with keys of its own: `reset`, and the
    /// keys sent for the last one are no longer shown. The Label field is no
    /// longer counted as given words for any key, so what it holds is never
    /// committed to this deck ([`label_commit`]); the shell empties it and
    /// gives it this deck's label (`Shell::on_faces_event`).
    pub(crate) fn other_deck(&mut self) {
        self.reset();
        self.sent = None;
        self.given = None;
    }

    /// Empty the Label field.
    pub(crate) fn clear_label(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.label
            .update(cx, |field, cx| field.set_value("", window, cx));
    }
}

/// A Macropad face as the editor reads it: its id, the tag the deck shows in
/// its header, and the keys the grid shows.
pub(crate) struct MacroFace {
    pub f: u8,
    pub src: Option<String>,
    pub keys: Vec<MacroKey>,
}

impl Shell {
    /// The selected face, when it is a Macropad, with the keys shown for it.
    pub(crate) fn macro_face(&self) -> Option<MacroFace> {
        let face = self.selected_face()?;
        if face.t != Template::Macropad {
            return None;
        }
        let layout = self.state.mirror().macros.get(&face.f);
        let mirror = layout.map_or(&[][..], |layout| &layout.keys[..]);
        let keys = keys_shown(
            mirror,
            face.f,
            self.faces.keys.sent.as_ref(),
            present::now(),
        );
        Some(MacroFace {
            f: face.f,
            src: layout.and_then(|layout| layout.src.clone()),
            keys: keys.to_vec(),
        })
    }

    /// Ask the service what it knows of Accessibility, unless it is being
    /// asked already.
    pub(crate) fn read_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.faces.keys.asking {
            return;
        }
        self.faces.keys.asking = true;
        self.ask(UiAction::PermStatus, window, cx, |this, result, _, _| {
            this.faces.keys.asking = false;
            // A failed read keeps what was known: the row says nothing it
            // has not read.
            if let Ok(result) = result {
                this.faces.keys.access = Access::parse(&result);
            }
        });
    }

    /// What the editor reads from the service: the Accessibility grant,
    /// unless it is known (`force` asks anyway).
    pub(crate) fn refresh_keys(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if force || self.faces.keys.access.is_none() {
            self.read_access(window, cx);
        }
    }

    /// After every event: keys the mirror now has are no longer held, and
    /// the Label field follows its key.
    pub(crate) fn on_keys_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agrees = self.faces.keys.sent.as_ref().is_some_and(|sent| {
            let mirror = self.state.mirror().macros.get(&sent.f);
            trimmed(mirror.map(|layout| layout.keys.clone()).unwrap_or_default()) == sent.keys
        });
        if agrees {
            self.faces.keys.sent = None;
        }
        if self.screen == Screen::FacesKeys {
            self.sync_label(window, cx);
        }
    }

    /// Once a second: sent keys the mirror never agreed with go, and
    /// **Record** stops waiting once the page is left.
    pub(crate) fn tick_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = present::now();
        if self
            .faces
            .keys
            .sent
            .as_ref()
            .is_some_and(|sent| !sent.holds(now))
        {
            self.faces.keys.sent = None;
            self.sync_label(window, cx);
        }
        if self.screen != Screen::FacesKeys {
            self.faces.keys.recording = false;
        }
    }

    /// A harness scene's key: slot `slot` selected, Accessibility as
    /// `perm_status` says, nothing waiting.
    pub(crate) fn stage_keys(
        &mut self,
        slot: usize,
        perm_status: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = &mut self.faces.keys;
        keys.reset();
        keys.slot = slot.min(SLOTS - 1);
        keys.sent = None;
        keys.access = Access::parse(perm_status);
        keys.given = None;
        self.sync_label(window, cx);
    }

    /// Nothing waits for a key any more.
    pub(crate) fn stop_recording(&mut self) {
        self.faces.keys.recording = false;
    }

    // ---- what a press does ---------------------------------------------------

    /// Select the key in slot `at`. What was typed for the key before it is
    /// committed first: a press elsewhere does not take the keyboard from
    /// the Label field, so it would not have been.
    pub(crate) fn select_key(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_label(window, cx);
        let keys = &mut self.faces.keys;
        keys.recording = false;
        keys.too_long = false;
        keys.slot = at.min(SLOTS - 1);
        self.sync_label(window, cx);
        cx.notify();
    }

    /// Give the Label field its key's label when another key was selected,
    /// or when the key's label changed while the field still holds what it
    /// was last given (nothing typed over it).
    pub(crate) fn sync_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(face) = self.macro_face() else {
            return;
        };
        let at = self.faces.keys.slot;
        let label = slot(&face.keys, at)
            .map(|key| key.label.clone())
            .unwrap_or_default();
        let field = self.faces.keys.label.clone();
        let typed = field.read(cx).value().to_string();
        let give = match &self.faces.keys.given {
            Some((f, given_at, given)) if *f == face.f && *given_at == at => {
                *given != label && typed == *given
            }
            _ => true,
        };
        if give {
            field.update(cx, |state, cx| state.set_value(label.clone(), window, cx));
        }
        // A label committed from the field is the field's words already.
        if give || typed == label {
            self.faces.keys.given = Some((face.f, at, label));
        }
    }

    /// The Label field's words, committed (Return, or the field let go of):
    /// a key that fits is sent, an empty slot given a label becomes a key,
    /// and a label too long for the deck is said and not sent. Emptying a
    /// key's label does not remove the key (**Clear This Key** does): the
    /// field takes the label back, as it does when its words were given for
    /// another key than the one selected ([`label_commit`]).
    pub(crate) fn commit_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(face) = self.macro_face() else {
            return;
        };
        let at = self.faces.keys.slot;
        let typed = self.faces.keys.label.read(cx).value().to_string();
        let key = slot(&face.keys, at).cloned();
        let was = key
            .as_ref()
            .map(|key| key.label.clone())
            .unwrap_or_default();
        let given = self.faces.keys.given.as_ref().map(|(f, at, _)| (*f, *at));
        match label_commit(&typed, given, face.f, at, &was) {
            LabelCommit::Same => self.faces.keys.too_long = false,
            LabelCommit::Restore => {
                self.faces.keys.too_long = false;
                self.faces.keys.given = None;
                self.sync_label(window, cx);
                cx.notify();
            }
            LabelCommit::TooLong => {
                self.faces.keys.too_long = true;
                cx.notify();
            }
            LabelCommit::Send(label) => {
                self.faces.keys.too_long = false;
                let key = match key {
                    Some(key) => MacroKey { label, ..key },
                    None => MacroKey {
                        label,
                        ..empty_key()
                    },
                };
                self.send_keys(&face, placed(&face.keys, at, key), window, cx);
            }
        }
    }

    /// Change the selected key, and send the face's keys.
    fn edit_key(
        &mut self,
        change: impl FnOnce(&mut KeyDraft),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(face) = self.macro_face() else {
            return;
        };
        let at = self.faces.keys.slot;
        let Some(key) = slot(&face.keys, at) else {
            return;
        };
        let mut draft = KeyDraft::of(key);
        change(&mut draft);
        self.send_keys(&face, placed(&face.keys, at, draft.key()), window, cx);
    }

    /// Send `keys` for `face`, unless they are the keys it has, and show
    /// them until the mirror agrees.
    fn send_keys(
        &mut self,
        face: &MacroFace,
        keys: Vec<MacroKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = trimmed(keys);
        if keys == trimmed(face.keys.clone()) {
            return;
        }
        let at = present::now();
        self.faces.keys.sent = Some(Sent {
            f: face.f,
            keys: keys.clone(),
            at,
        });
        let action = keys_write(face.f, face.src.clone(), keys);
        self.ask(action, window, cx, |this, result, window, cx| {
            if let Err(e) = result {
                this.faces.keys.sent = None;
                let what = this.tr("Change the macro keys");
                this.toast(
                    this.fmt(
                        "{0} failed: {1}",
                        &[what, &crate::updates::failure_text(&e)],
                    ),
                    window,
                    cx,
                );
                this.sync_label(window, cx);
            }
        });
        self.sync_label(window, cx);
        cx.notify();
    }

    /// **Clear This Key**: the slot is empty again, and the keys after it
    /// stay where they are.
    fn clear_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording();
        let Some(face) = self.macro_face() else {
            return;
        };
        self.faces.keys.too_long = false;
        let keys = cleared(&face.keys, self.faces.keys.slot);
        self.send_keys(&face, keys, window, cx);
    }

    /// **Record**: wait for the next key, or stop waiting.
    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        self.faces.keys.recording = !self.faces.keys.recording;
        cx.notify();
    }

    /// A key pressed while **Record** waits. `true` when it was taken, so
    /// nothing else in the window acts on it.
    fn record_key(
        &mut self,
        stroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.screen != Screen::FacesKeys {
            self.stop_recording();
            return false;
        }
        if modifier_only(stroke) {
            return false;
        }
        self.stop_recording();
        cx.notify();
        if stroke.key == "escape" && !stroke.modifiers.modified() {
            return true;
        }
        match from_gpui(stroke).as_deref().and_then(Shortcut::parse) {
            Some(shortcut) => self.edit_key(|draft| draft.shortcut = Some(shortcut), window, cx),
            None => self.toast(self.tr("The deck cannot send that key."), window, cx),
        }
        true
    }

    /// **Check…**: have the service ask macOS, then read what it learned.
    fn check_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ask = UiAction::PermRequest {
            which: "accessibility".into(),
        };
        self.ask(ask, window, cx, |this, result, window, cx| match result {
            Ok(_) => this.read_access(window, cx),
            Err(e) => {
                let what = this.tr("Ask for the permission");
                this.toast(
                    this.fmt("{0} failed: {1}", &[what, &e.to_string()]),
                    window,
                    cx,
                );
            }
        });
    }

    // ---- drawing -------------------------------------------------------------

    /// The Macropad `face`'s keys and the key selected among them. Editing
    /// needs the service and not the deck; with the deck away a line says
    /// where the keys go.
    pub(crate) fn render_keys(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let keys = self.macro_face().map(|face| face.keys).unwrap_or_default();
        h_flex()
            .w_full()
            .items_start()
            .gap(px(20.))
            .child(self.key_grid(&keys, p, cx))
            .child(self.key_inspector(&keys, !dim, p, cx))
    }

    /// The nine keys, three to a row: a key's control in its top-left
    /// corner, its shortcut in the top-right one and its label in the
    /// middle; an empty slot is a dashed **Assign**. They are one stop on
    /// the Tab order ([`ui::Group`]): Tab reaches the selected key, the
    /// arrows move the keyboard across and down the grid, and Return or
    /// Space selects the key that has it.
    fn key_grid(&self, keys: &[MacroKey], p: &Palette, cx: &Context<Self>) -> Div {
        let selected = self.faces.keys.slot;
        let cells = (0..SLOTS).map(|at| match slot(keys, at) {
            Some(key) => self.key_cell(at, key, at == selected, p, cx),
            None => self.assign_cell(at, at == selected, p, cx),
        });
        let columns = usize::from(COLUMNS);
        let grid = ui::Group::new("macro-keys", Layout::Grid { columns })
            .w_full()
            .grid()
            .grid_cols(COLUMNS)
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(12.))
            .bg(p.surf)
            .border_1()
            .border_color(p.line)
            .selected(Some(selected))
            .members(cells);
        let away =
            (!self.linked()).then(|| ui::hint(self.tr("Sent to the deck when it links."), p));
        v_flex()
            .flex_none()
            .w(px(GRID_WIDTH))
            .gap(px(8.))
            .child(ui::label(self.tr("Macropad keys"), p))
            .child(grid)
            .child(ui::hint(
                self.tr("Top left: the deck button. Top right: the shortcut it types."),
                p,
            ))
            .children(away)
    }

    /// A key on the grid, on the deck's own dark face.
    fn key_cell(
        &self,
        at: usize,
        key: &MacroKey,
        selected: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> Press {
        let shortcut = key.keys.as_deref().and_then(Shortcut::parse);
        let corner = key.binding.map(|binding| {
            div()
                .absolute()
                .top(px(8.))
                .left(px(11.))
                .ts(11., 14.)
                .whitespace_nowrap()
                .text_color(hex(deck_colors::KEY_CORNER))
                .child(binding_mark(binding))
        });
        let combo = shortcut.map(|shortcut| {
            div()
                .absolute()
                .top(px(8.))
                .right(px(10.))
                .child(ui::combo(&shortcut.mods, key_cap(shortcut.key)))
        });
        Press::new(("macro-key", at))
            .radius(KEY_RADIUS)
            .own_border(1.)
            .selected(selected)
            .flex()
            .items_center()
            .justify_center()
            .h(px(KEY_HEIGHT))
            .rounded(px(KEY_RADIUS))
            .border_1()
            .border_color(p.key_line)
            .bg(p.key_fill)
            .child(
                div()
                    .ts(13., 18.)
                    .semibold()
                    .whitespace_nowrap()
                    .text_color(hex(deck_colors::KEY_NAME))
                    .child(key.label.clone()),
            )
            .children(corner)
            .children(combo)
            .on_click(cx.listener(move |this, _, window, cx| this.select_key(at, window, cx)))
    }

    /// An empty slot: pressed, it is selected, and its label makes it a key.
    fn assign_cell(&self, at: usize, selected: bool, p: &Palette, cx: &Context<Self>) -> Press {
        Press::new(("macro-key", at))
            .radius(KEY_RADIUS)
            .own_border(ASSIGN_EDGE)
            .selected(selected)
            .flex()
            .items_center()
            .justify_center()
            .h(px(KEY_HEIGHT))
            .rounded(px(KEY_RADIUS))
            .border(px(ASSIGN_EDGE))
            .border_dashed()
            .border_color(p.edge)
            .ts(13., 18.)
            .medium()
            .text_color(p.text2)
            .child(self.tr("Assign"))
            .on_click(cx.listener(move |this, _, window, cx| this.select_key(at, window, cx)))
    }

    /// The selected key: its label, its control, its shortcut's modifiers
    /// and key, **Record**, the grant the deck's shortcuts need, and
    /// **Clear This Key**. An empty slot takes a label first.
    fn key_inspector(
        &self,
        keys: &[MacroKey],
        enabled: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> Div {
        let pane = &self.faces.keys;
        let draft = slot(keys, pane.slot).map(KeyDraft::of);
        let editable = enabled && draft.is_some();
        let binding = draft.as_ref().and_then(|draft| draft.binding);
        let shortcut = draft.as_ref().and_then(|draft| draft.shortcut.clone());
        let note = if pane.too_long {
            self.tr("Too long. Up to 12 letters, or 4 Chinese characters.")
        } else {
            self.tr("Up to 12 letters, or 4 Chinese characters.")
        };
        let note = div()
            .ts(12., 16.)
            .text_color(if pane.too_long { p.warn_text } else { p.text2 })
            .child(ui::para(note));
        let label = field(
            self.tr("Label"),
            div()
                .flex_1()
                .min_w_0()
                .child(ui::field(&pane.label, p).disabled(!enabled)),
            Some(note),
            p,
        );
        let control = field(
            self.tr("Deck button"),
            self.binding_popup(binding, editable, cx),
            None,
            p,
        );
        let held = shortcut.as_ref().map(|shortcut| shortcut.mods.clone());
        let chips = h_flex()
            .gap(px(4.))
            .children(Mod::ALL.into_iter().enumerate().map(|(i, m)| {
                let on = held.as_ref().is_some_and(|mods| mods.contains(&m));
                Chip::modifier(("key-mod", i), m, on)
                    .disabled(!editable || held.is_none())
                    .on_toggle(cx.listener(move |this, on: &bool, window, cx| {
                        this.stop_recording();
                        this.edit_key(|draft| draft.set_mod(m, *on), window, cx);
                    }))
            }));
        let modifiers = field(self.tr("Modifiers"), chips, None, p);
        let record = if pane.recording {
            self.tr("Press a key\u{2026}")
        } else {
            self.tr("Record")
        };
        let record = Btn::new("key-record", record)
            .quiet()
            .disabled(!editable)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx)));
        let key = field(
            self.tr("Key"),
            h_flex()
                .gap(px(12.))
                .child(self.key_popup(shortcut.as_ref(), editable, cx))
                .child(record),
            None,
            p,
        );
        let clear = h_flex()
            .w_full()
            .gap(px(12.))
            .min_h(px(48.))
            .px(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ts(12., 16.)
                    .text_color(p.text2)
                    .child(ui::para(
                        self.tr("Pick by hand the shortcuts\nmacOS reserves."),
                    )),
            )
            .child(
                Btn::new("key-clear", self.tr("Clear This Key"))
                    .quiet()
                    .disabled(!editable)
                    .on_click(cx.listener(|this, _, window, cx| this.clear_key(window, cx))),
            );
        let mut rows: Vec<AnyElement> = vec![
            label.into_any_element(),
            control.into_any_element(),
            modifiers.into_any_element(),
            key.into_any_element(),
        ];
        rows.extend(
            self.access_row(enabled, p, cx)
                .map(IntoElement::into_any_element),
        );
        rows.push(clear.into_any_element());
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(8.))
            .child(ui::label(self.tr("Selected key"), p))
            .child(ui::card(rows, p))
    }

    /// The Deck button menu: touch only, or one of the deck's controls.
    fn binding_popup(
        &self,
        binding: Option<Binding>,
        enabled: bool,
        cx: &Context<Self>,
    ) -> PopupButton {
        let language = self.language();
        let shell = cx.entity();
        PopupButton::new("key-binding", binding_word(binding, language))
            .width(BINDING_WIDTH)
            .disabled(!enabled)
            .menu(move |mut menu, _, _| {
                let choices = [None].into_iter().chain(BINDINGS.into_iter().map(Some));
                for choice in choices {
                    let shell = shell.clone();
                    let item = PopupMenuItem::new(binding_word(choice, language))
                        .checked(choice == binding)
                        .on_click(move |_, window, cx| {
                            shell.update(cx, |this, cx| {
                                this.stop_recording();
                                this.edit_key(|draft| draft.binding = choice, window, cx);
                            });
                        });
                    menu = menu.item(item);
                }
                menu
            })
    }

    /// The Key menu: none, or one of the deck's 73 keys.
    fn key_popup(
        &self,
        shortcut: Option<&Shortcut>,
        enabled: bool,
        cx: &Context<Self>,
    ) -> PopupButton {
        let language = self.language();
        let current = shortcut.map(|shortcut| shortcut.key);
        let shown = match current {
            Some(key) => key_name(key, language),
            None => language.text("None").to_string(),
        };
        let shell = cx.entity();
        PopupButton::new("key-key", shown)
            .width(KEY_WIDTH)
            .disabled(!enabled)
            .menu(move |menu, _, _| {
                let choices = [None].into_iter().chain(KEYS.into_iter().map(Some));
                choices.fold(menu.scrollable(true), |menu, choice| {
                    let shell = shell.clone();
                    let word = match choice {
                        Some(key) => key_name(key, language),
                        None => language.text("None").to_string(),
                    };
                    menu.item(
                        PopupMenuItem::new(word)
                            .checked(choice == current)
                            .on_click(move |_, window, cx| {
                                shell.update(cx, |this, cx| {
                                    this.stop_recording();
                                    this.edit_key(|draft| draft.set_key(choice), window, cx);
                                });
                            }),
                    )
                })
            })
    }

    /// The grant the deck's shortcuts need, and what can be done about it:
    /// ask (Check…) while nothing has, or go to System Settings once macOS
    /// said no. Left out while the service has said nothing of it.
    fn access_row(&self, enabled: bool, p: &Palette, cx: &Context<Self>) -> Option<Row> {
        let access = self.faces.keys.access?;
        let row = Row::new().title(self.tr("Accessibility"));
        let sub = self.tr("Lets the deck type shortcuts.");
        // A button beside the state would leave the sentence a sliver of
        // the inspector, so it goes under the sentence.
        let with = |button: Btn| {
            v_flex()
                .gap(px(6.))
                .child(ui::para(sub))
                .child(h_flex().child(button))
        };
        Some(match access {
            Access::Granted => row
                .sub(sub)
                .right(ui::status(self.tr("Granted"), Tone::Green, p)),
            Access::NotChecked => row
                .sub_el(with(
                    Btn::new("access-check", self.tr("Check\u{2026}"))
                        .disabled(!enabled)
                        .on_click(cx.listener(|this, _, window, cx| this.check_access(window, cx))),
                ))
                .right(ui::status(self.tr("Not checked"), Tone::Gray, p)),
            Access::Denied => row
                .sub_el(with(
                    Btn::new("access-settings", self.tr("Open Settings"))
                        .on_click(|_, _, cx: &mut App| cx.open_url(ACCESSIBILITY_SETTINGS)),
                ))
                .right(ui::status(self.tr("Not allowed"), Tone::Amber, p)),
        })
    }
}

/// One line of the inspector, as the design lays it out: its name in a
/// column of its own, the control beside it, and a note under both.
fn field(name: &str, control: impl IntoElement, note: Option<Div>, p: &Palette) -> Div {
    let height = if note.is_some() {
        FIELD_WITH_NOTE
    } else {
        FIELD
    };
    v_flex()
        .w_full()
        .justify_center()
        .gap(px(6.))
        .min_h(px(height))
        .px(px(16.))
        .pt(px(5.))
        // The card's rule is drawn over the line's last pixel.
        .pb(px(6.))
        .child(
            h_flex()
                .gap(px(12.))
                .child(
                    div()
                        .flex_none()
                        .w(px(NAME_WIDTH))
                        .ts(13., 18.)
                        .medium()
                        .text_color(p.text)
                        .child(name.to_string()),
                )
                .child(control),
        )
        .children(note)
}
