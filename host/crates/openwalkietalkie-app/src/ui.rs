//! The window's parts, in the look [`crate::style`] describes: the type
//! ramp, state words, keycaps and modifier marks, rows, cards and sections,
//! the page frame, and the controls a page puts on a row.
//!
//! They are built from `div`s and not from gpui-component's widgets: its
//! `Button` fixes its own paddings and recolours its label on hover, and its
//! `Sheet`/`Dialog` paint the theme's background. Everything below reads
//! [`Palette`] and nothing else, so a page that uses them cannot pick up a
//! stock colour.
//!
//! The numbers are the design's kit: rows 44 high with one line and 56 with
//! a sub-line, radii 6 on keycaps, 8 on controls and tiles, 12 on cards,
//! type at 11, 12, 13, 22 and 30 px, and a 764 px content column.
//! Everything is in pixels, which is also what the design's CSS is in;
//! heights include borders, as both the design's `border-box` and taffy
//! count them.
//!
//! Which part is which: the words ([`label`], [`mono`], [`hint`],
//! [`status`], [`code_block`]), the marks ([`dot`], [`keycap`],
//! [`radio_dot`], [`combo`]), the controls ([`Btn`], [`CopyButton`],
//! [`Switch`], [`Level`], [`Segmented`], [`PopupButton`], [`Chip`],
//! [`field`], and [`Press`] for anything else that is pressed), the
//! containers ([`section`], [`card`], [`Row`], [`Group`], [`tile`],
//! [`bare`], [`attention`], and a table's [`table_row`], [`cell`] and
//! [`cell_end`]) and the frame ([`brand`], [`nav_item`], [`link_out`],
//! [`Header`], [`page`]). The deck's own pictures — the device, the face
//! plates, the battery — are in [`crate::art`].
//!
//! **Every control is a stop on the window's Tab order.** It keeps a focus
//! handle of its own under its id, Return and Space press it (a menu button
//! opens its menu), a [`Level`] also takes the arrow keys, Home and End, and
//! while it has the keyboard it draws the one blue, `info`, as a 2 px ring
//! 2 px outside itself ([`ring`]). A click with the pointer never takes the
//! keyboard, so a ring always means the keyboard is there. Selection is the
//! other ring, in `outline`.
//!
//! **Controls that are one choice or one set are one stop** ([`Group`]): a
//! segmented control's options, the sidebar's pages, the face strip, the
//! key grid, a card of radio choices. Tab enters a group on its selected
//! member, the arrow keys (Home and End too) move the keyboard from member
//! to member, Return or Space presses the one that has it, and Tab leaves.
//! The arrows only move the ring: choosing is a press, because a choice
//! writes a setting or, for a model, starts a download.
//!
//! Two things follow for a page: a control's id must be unique among the
//! ids around it (its focus is kept under it), and a control needs 4 px of
//! room before anything that clips (8 around a selected tile), or its ring
//! is cut.
use crate::style::{deck_colors, hex, palette, Palette, MONO};
use deck_model::Tone;
use gpui::{
    canvas, div, percentage, prelude::*, px, svg, AlignItems, Animation, AnimationExt as _,
    AnyElement, AnyView, App, Bounds, ClickEvent, ClipboardItem, Context, Corner, DismissEvent,
    Div, DragMoveEvent, ElementId, Entity, EntityId, FocusHandle, Focusable as _, FontFeatures,
    FontWeight, Hsla, Interactivity, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, Pixels,
    SharedString, Stateful, StyleRefinement, Svg, Transformation, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenu;
use gpui_component::popover::Popover;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex, Selectable, StyledExt as _};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The icons the window draws, by asset path (see [`crate::assets`]).
pub mod ic {
    pub const ADD: &str = "icons/add-circle.svg";
    pub const ARCHIVE: &str = "icons/archive-minimalistic.svg";
    pub const ARROW_RIGHT: &str = "icons/arrow-right.svg";
    pub const ARROW_UP: &str = "icons/arrow-up.svg";
    pub const BELL: &str = "icons/bell.svg";
    pub const BLUETOOTH: &str = "icons/bluetooth.svg";
    pub const BOT: &str = "icons/bot.svg";
    pub const CONOL: &str = "icons/conol-mark.svg";
    pub const CHAT: &str = "icons/chat-round-line.svg";
    pub const CHECK: &str = "icons/check.svg";
    pub const CHECK_CIRCLE: &str = "icons/check-circle.svg";
    pub const CHEVRON_DOWN: &str = "icons/alt-arrow-down.svg";
    pub const CHEVRON_LEFT: &str = "icons/alt-arrow-left.svg";
    pub const CHEVRON_RIGHT: &str = "icons/alt-arrow-right.svg";
    pub const CHEVRON_UP: &str = "icons/alt-arrow-up.svg";
    pub const CLAUDE: &str = "icons/claude-mark.svg";
    pub const CLOCK: &str = "icons/clock-circle.svg";
    pub const CLOSE: &str = "icons/close.svg";
    pub const CLOSE_CIRCLE: &str = "icons/close-circle.svg";
    pub const COPY: &str = "icons/copy.svg";
    pub const DANGER: &str = "icons/danger-triangle.svg";
    /// The deck: a portrait slab, a square screen and the split key.
    pub const DECK: &str = "icons/deck.svg";
    /// The deck with a dot at its corner: it needs the owner.
    pub const DECK_CARE: &str = "icons/deck-care.svg";
    /// The deck struck through: not linked.
    pub const DECK_OFF: &str = "icons/deck-off.svg";
    pub const DIAL: &str = "icons/dial.svg";
    pub const DOCUMENT: &str = "icons/document.svg";
    pub const EXTERNAL: &str = "icons/arrow-up-right.svg";
    pub const EYE: &str = "icons/eye.svg";
    pub const EYE_CLOSED: &str = "icons/eye-closed.svg";
    pub const FOLDER: &str = "icons/folder.svg";
    pub const GEMINI: &str = "icons/gemini-mark.svg";
    pub const GLOBAL: &str = "icons/global.svg";
    pub const INFO: &str = "icons/info-circle.svg";
    pub const KEY: &str = "icons/key-minimalistic.svg";
    pub const KEYBOARD: &str = "icons/keyboard.svg";
    pub const LAPTOP: &str = "icons/laptop.svg";
    pub const LIST: &str = "icons/list.svg";
    pub const LOGOUT: &str = "icons/logout-2.svg";
    pub const MAGIC: &str = "icons/magic-stick-3.svg";
    pub const MICROPHONE: &str = "icons/microphone.svg";
    /// The Mac's modifier marks, as line icons: Geist has none of ⌃⌥⇧⌘.
    pub const MOD_COMMAND: &str = "icons/mod-command.svg";
    pub const MOD_CONTROL: &str = "icons/mod-control.svg";
    pub const MOD_OPTION: &str = "icons/mod-option.svg";
    pub const MOD_SHIFT: &str = "icons/mod-shift.svg";
    pub const MONITOR: &str = "icons/monitor.svg";
    pub const OPENAI: &str = "icons/openai-mark.svg";
    pub const PIN: &str = "icons/pin.svg";
    /// The timer plate's ring, and its lit half drawn over it: gpui gives a
    /// circle one border colour, so the two halves are two masks.
    pub const PLATE_RING: &str = "icons/plate-ring.svg";
    pub const PLATE_RING_HALF: &str = "icons/plate-ring-half.svg";
    pub const PLAY: &str = "icons/action-play.svg";
    pub const PLUG: &str = "icons/plug.svg";
    pub const PLUS: &str = "icons/plus.svg";
    pub const REFRESH: &str = "icons/refresh.svg";
    pub const RETURN: &str = "icons/return.svg";
    pub const SERVER: &str = "icons/server.svg";
    pub const SETTINGS: &str = "icons/settings.svg";
    pub const SMARTPHONE: &str = "icons/smartphone.svg";
    pub const STAR: &str = "icons/star.svg";
    pub const STOP: &str = "icons/stop.svg";
    pub const TAG: &str = "icons/tag.svg";
    pub const TERMINAL: &str = "icons/terminal.svg";
    pub const TRASH: &str = "icons/trash-bin-minimalistic.svg";
    pub const TUNING: &str = "icons/tuning.svg";
    pub const VOLUME: &str = "icons/volume-loud.svg";
    pub const WIDGET: &str = "icons/widget.svg";
    pub const WIFI_OFF: &str = "icons/wifi-off.svg";
}

/// The brand tile beside the app's name, by asset path (see
/// [`crate::assets`]).
pub const BRAND_TILE: &str = "device/brand-tile.png";

/// A row with one line of text: the kit's rows are 44 or 56 high.
pub const ROW_ONE_LINE: f32 = 44.;
/// A row with a sub-line under its title.
pub const ROW_TWO_LINES: f32 = 56.;

/// An icon at `size` px in `color`. An svg does not inherit the text colour
/// around it, so the colour is always said.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// Text sizes as size/line-height pairs in pixels, the three weights the
/// window uses, the mono family and tabular figures — and the one way of
/// laying out a row that gpui has no word for ([`Type::items_stretch`]).
pub trait Type: Styled + Sized {
    fn ts(self, size: f32, line: f32) -> Self {
        self.text_size(px(size)).line_height(px(line))
    }

    /// Row titles, buttons, the page in the sidebar it is not.
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }

    /// Page titles, section labels, the page in the sidebar it is.
    fn semibold(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }

    fn mono(self) -> Self {
        self.font_family(MONO)
    }

    /// Figures of one width, so a percentage or a count that changes does
    /// not shift what follows it. Geist has `tnum`; Geist Mono needs none.
    /// gpui has no setter for font features, so this writes the text
    /// style's own field; it changes the features alone and keeps the
    /// family and the fallbacks it inherits.
    fn tnum(mut self) -> Self {
        self.text_style()
            .get_or_insert_with(Default::default)
            .font_features = Some(FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
        self
    }

    /// Stretch what a flex row holds to the row's own height: cards side by
    /// side as tall as each other, the shorter one's last row ([`Row::grow`])
    /// taking the rest. It is what a `div().flex()` row does when nothing
    /// says otherwise (an `h_flex` centres), said so that it is meant.
    fn items_stretch(mut self) -> Self {
        self.style().align_items = Some(AlignItems::Stretch);
        self
    }
}

impl<T: Styled> Type for T {}

/// A text tooltip in the window's own size, for an element with an id.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| {
        Tooltip::new(text.clone())
            .text_size(px(12.))
            .line_height(px(16.))
            .py(px(5.))
            .px(px(8.))
            .build(window, cx)
    }
}

// ---- words and marks --------------------------------------------------

/// A section's label: 12/16, semibold, in the label step.
pub fn label(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .ts(12., 16.)
        .semibold()
        .text_color(p.text3)
        .child(text.into())
}

/// A version, a path, a key name or a value, in Geist Mono 12/16 on one
/// line, in `color` (most often the sub-line colour, `text2`).
pub fn mono(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .ts(12., 16.)
        .mono()
        .whitespace_nowrap()
        .text_color(color)
        .child(text.into())
}

/// A sentence of explanation under or beside something, wrapping.
pub fn hint(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().ts(12., 16.).text_color(p.text2).child(para(text))
}

/// Something to read exactly or to paste — a command left to run, a `curl`,
/// a token shown once — in Geist Mono 12/18 on a well, wrapping where it
/// must. It is as wide as what holds it; a command beside its
/// [`CopyButton`] takes the rest of the row with `flex_1().min_w_0()`.
pub fn code_block(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .px(px(12.))
        .py(px(10.))
        .rounded(px(8.))
        .bg(p.well)
        .ts(12., 18.)
        .mono()
        .text_color(p.text)
        .child(para(text))
}

/// A status dot.
pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// A state in a word: a dot and the word for a healthy or neutral one, a
/// tinted pill for one that needs the owner (`Amber`, `Red`, `Blue`). Only
/// trouble gets colour behind it.
pub fn status(text: impl Into<SharedString>, tone: Tone, p: &Palette) -> Div {
    let state = h_flex()
        .flex_none()
        .gap(px(7.))
        .ts(12., 16.)
        .whitespace_nowrap()
        .child(dot(p.tone(tone), 6.));
    // The word on a pill takes its state's text step; beside a bare dot it
    // is a sub-line, and only the dot says the state.
    let state = match p.tone_wash(tone) {
        Some(wash) => {
            let word = p.tone_text(tone);
            state
                .h(px(22.))
                .rounded(px(11.))
                .bg(wash)
                .medium()
                .text_color(word)
                // With Increase contrast the word's colour rims the pill,
                // taking a pixel of each side, so the pill keeps its size.
                .map(|pill| {
                    if p.contrast {
                        pill.px(px(8.)).border_1().border_color(word)
                    } else {
                        pill.px(px(9.))
                    }
                })
        }
        None => state.text_color(p.text2),
    };
    state.child(text.into())
}

/// A key's name on a cap: PTT in a sentence's place, a key in a hint.
pub fn keycap(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(22.))
        .h(px(22.))
        .px(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(p.edge)
        .bg(p.well)
        .ts(12., 16.)
        .mono()
        .medium()
        .text_color(p.text)
        .child(text.into())
}

/// A radio's dot: a ring in a control's edge, or the selection outline with
/// a dot in it. A choice is a [`Row`] led by [`bare`]`(radio_dot(..))`, and
/// the whole row is what is clicked.
pub fn radio_dot(on: bool, p: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded_full()
        .border(px(1.5))
        .border_color(if on { p.outline } else { p.edge })
        .when(on, |radio| radio.child(dot(p.outline, 8.)))
}

/// A Mac modifier key, as a macro key's shortcut carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mod {
    Control,
    Option,
    Shift,
    Command,
}

impl Mod {
    /// The Mac's own order, the one a shortcut is written in: ⌃⌥⇧⌘.
    pub const ALL: [Mod; 4] = [Mod::Control, Mod::Option, Mod::Shift, Mod::Command];

    /// Its mark (see [`ic::MOD_SHIFT`]).
    pub const fn icon(self) -> &'static str {
        match self {
            Mod::Control => ic::MOD_CONTROL,
            Mod::Option => ic::MOD_OPTION,
            Mod::Shift => ic::MOD_SHIFT,
            Mod::Command => ic::MOD_COMMAND,
        }
    }

    /// Its name as macOS prints it. It is not translated: it is what is
    /// written on the key.
    pub const fn name(self) -> &'static str {
        match self {
            Mod::Control => "Control",
            Mod::Option => "Option",
            Mod::Shift => "Shift",
            Mod::Command => "Command",
        }
    }
}

/// `mods` in the Mac's order ([`Mod::ALL`]), each once, however they were
/// given.
pub fn canonical(mods: &[Mod]) -> Vec<Mod> {
    Mod::ALL.into_iter().filter(|m| mods.contains(m)).collect()
}

/// A shortcut the way a macro key's corner draws it: the modifier marks in
/// the Mac's order, then the key in mono. It sits on a key's own dark face,
/// so its colour is the deck's and not the window's.
pub fn combo(mods: &[Mod], key: impl Into<SharedString>) -> Div {
    let color = hex(deck_colors::KEY_COMBO);
    h_flex()
        .flex_none()
        .gap(px(1.))
        .children(
            canonical(mods)
                .into_iter()
                .map(|m| icon(m.icon(), 12., color)),
        )
        .child(
            div()
                .ml(px(1.))
                .ts(11., 14.)
                .mono()
                .text_color(color)
                .child(key.into()),
        )
}

/// A spinning refresh glyph, for "asking the service".
pub fn spinner(size: f32, color: Hsla) -> impl IntoElement {
    icon(ic::REFRESH, size, color).with_animation(
        "spinner",
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
}

// ---- the keyboard ---------------------------------------------------------

/// What a click calls.
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What a [`Switch`] or a [`Chip`] calls with the value it is being moved to.
type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// What a [`Level`] calls with the stop it was moved to.
type LevelHandler = Rc<dyn Fn(&u16, &mut Window, &mut App)>;

/// What a [`Segmented`] calls with the option chosen.
type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

/// What builds a [`PopupButton`]'s menu, each time it opens.
type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// Keyboard focus: the ring's width, and its distance outside a control.
pub const FOCUS_RING: f32 = 2.;
/// Around a selected tile, focus sits outside the selection's own ring.
pub const FOCUS_RING_SELECTED: f32 = 6.;

/// What a disabled control is drawn at.
const DISABLED: f32 = 0.4;

/// The focus handle of the control drawn under `id`: one for each control,
/// kept for as long as something keeps being drawn under that id.
fn focus_handle(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(id.clone(), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone()
}

/// Make `el` a stop on the window's Tab order through `focus`: that is how
/// the keyboard reaches a control, and how Return and Space press it (gpui
/// clicks a focused element on either key).
///
/// A disabled control keeps its handle and loses its stop: Tab passes it
/// by, and one that had the keyboard when it was disabled keeps it, so Tab
/// still moves on from there instead of from nowhere. A click with the
/// pointer does not take the keyboard — only Tab gives it — so a ring is
/// only ever drawn where the keyboard is.
fn keyboard_stop(el: Stateful<Div>, focus: &FocusHandle, enabled: bool) -> Stateful<Div> {
    el.track_focus(&focus.clone().tab_index(0).tab_stop(enabled))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

/// A ring around an element: `width` px of `color`, `gap` px outside it,
/// following its corners. It is the keyboard's (`info`) or the selection's
/// (`outline`). `radius` is the element's own corner radius and
/// `own_border` the width of its own border: an absolutely placed child is
/// measured from inside its parent's border, so the ring is pushed out by
/// that much more.
///
/// It is a child of the element and draws nothing but its rim, so an
/// ancestor that clips cuts it.
pub fn ring(gap: f32, width: f32, radius: f32, own_border: f32, color: Hsla) -> Div {
    let out = -(gap + width + own_border);
    div()
        .absolute()
        .top(px(out))
        .left(px(out))
        .right(px(out))
        .bottom(px(out))
        .rounded(px(radius + gap + width))
        .border(px(width))
        .border_color(color)
}

/// The keyboard's ring drawn `inset` px inside an element, for one as wide
/// as its card (a [`Row`]): a ring outside it would cross the card's rim.
fn inset_ring(inset: f32, radius: f32, color: Hsla) -> Div {
    div()
        .absolute()
        .top(px(inset))
        .left(px(inset))
        .right(px(inset))
        .bottom(px(inset))
        .rounded(px(radius))
        .border(px(FOCUS_RING))
        .border_color(color)
}

/// How a [`Group`]'s members lie, which decides the arrow keys that move the
/// keyboard between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Side by side: Left and Right.
    Row,
    /// One over another: Up and Down.
    Column,
    /// `columns` to a row, filled row by row: Left and Right along a row,
    /// Up and Down along a column.
    Grid { columns: usize },
}

/// Where `key` moves the keyboard from the member at `at` of a group laid
/// out as `layout`, whose members are each `enabled` or not: one member
/// along the layout, past the ones that are not enabled, and never round an
/// end (nor, in a grid, off the end of a row); Home and End to the first and
/// the last that are. `None` for any other key, and when the keyboard would
/// stay where it is.
pub fn group_step(key: &str, at: usize, layout: Layout, enabled: &[bool]) -> Option<usize> {
    let count = enabled.len();
    if at >= count {
        return None;
    }
    // The members the key passes over, nearest first.
    let path: Vec<usize> = match (layout, key) {
        (_, "home") => (0..count).collect(),
        (_, "end") => (0..count).rev().collect(),
        (Layout::Row, "left") | (Layout::Column, "up") => (0..at).rev().collect(),
        (Layout::Row, "right") | (Layout::Column, "down") => (at + 1..count).collect(),
        (Layout::Grid { columns }, _) => {
            let columns = columns.max(1);
            let row = at - at % columns;
            match key {
                "left" => (row..at).rev().collect(),
                "right" => (at + 1..count.min(row + columns)).collect(),
                "up" => (0..=at).rev().step_by(columns).skip(1).collect(),
                "down" => (at..count).step_by(columns).skip(1).collect(),
                _ => return None,
            }
        }
        _ => return None,
    };
    path.into_iter()
        .find(|&member| enabled[member])
        .filter(|&member| member != at)
}

/// The member of a group that is its one stop on the Tab order: the one
/// that has the keyboard if it is `enabled`, else the selected one if it
/// is, else the first that is. `None` while none is: Tab passes the group
/// by.
pub fn group_stop(
    focused: Option<usize>,
    selected: Option<usize>,
    enabled: &[bool],
) -> Option<usize> {
    let on = |at: &usize| enabled.get(*at).copied().unwrap_or(false);
    focused
        .filter(on)
        .or_else(|| selected.filter(on))
        .or_else(|| (0..enabled.len()).find(on))
}

/// A [`Group`]'s keyboard as one member of it has it: the focus handle the
/// group keeps for the member, and whether the member is the group's stop on
/// the Tab order. A member takes it with [`Press::roving`] or
/// [`Row::roving`].
#[derive(Clone)]
pub struct Roving {
    focus: FocusHandle,
    stop: bool,
}

/// Something pressed that is not a [`Btn`]: a face on the strip, a macro
/// key, a sidebar row, a link, a segment. It is the kit's focus in one
/// place — its own Tab stop while it has a click, pressed by Return and
/// Space, the `info` ring 2 px outside it while it has the keyboard — and
/// the rings of a tile that can be selected: a resting 1 px ring touching
/// it ([`Press::rest`]), the selection's `outline` ring 2 px outside it
/// ([`Press::selected`]), and then focus 6 px out.
///
/// Its look is otherwise the caller's: it is a `div` to style and to fill,
/// with every `Styled`, `ParentElement` and interactive method a `div` has.
/// Its click stops at it, so a press inside a clickable row does not also
/// press the row. In a [`Group`] it is one of the group's members
/// ([`Press::roving`]).
#[derive(IntoElement)]
pub struct Press {
    id: ElementId,
    base: Stateful<Div>,
    radius: f32,
    own_border: f32,
    rest: Option<Hsla>,
    selected: bool,
    disabled: bool,
    shown_focused: bool,
    on_click: Option<ClickHandler>,
    roving: Option<Roving>,
}

impl Press {
    pub fn new(id: impl Into<ElementId>) -> Press {
        let id = id.into();
        Press {
            base: div().id(id.clone()),
            id,
            radius: 8.,
            own_border: 0.,
            rest: None,
            selected: false,
            disabled: false,
            shown_focused: false,
            on_click: None,
            roving: None,
        }
    }

    /// The corner radius its rings follow: its own. 8 unless said.
    pub fn radius(mut self, radius: f32) -> Press {
        self.radius = radius;
        self
    }

    /// The width of its own border, so its rings sit outside the border
    /// and not on it. 0 unless said.
    pub fn own_border(mut self, width: f32) -> Press {
        self.own_border = width;
        self
    }

    /// The 1 px ring touching it while it is not selected: a face tile's
    /// edge (`Palette::tile_rest`).
    pub fn rest(mut self, color: Hsla) -> Press {
        self.rest = Some(color);
        self
    }

    /// Selected: the `outline` ring, 2 px wide and 2 px outside it.
    pub fn selected(mut self, selected: bool) -> Press {
        self.selected = selected;
        self
    }

    /// Drawn at 40 %, and neither pressed nor reached by Tab.
    pub fn disabled(mut self, disabled: bool) -> Press {
        self.disabled = disabled;
        self
    }

    /// What pressing it does, with the pointer or with Return or Space. A
    /// `Press` without one is a picture: no stop, no ring.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Press {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// Draw the keyboard's ring without giving it the keyboard: the Tokens
    /// scene shows the focused state this way.
    pub(crate) fn shown_focused(mut self) -> Press {
        self.shown_focused = true;
        self
    }

    /// One of a [`Group`]'s members: the group's focus handle in place of
    /// its own, and a stop on the Tab order only while it is the group's.
    pub fn roving(mut self, roving: &Roving) -> Press {
        self.roving = Some(roving.clone());
        self
    }
}

impl Styled for Press {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Press {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl InteractiveElement for Press {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Press {}

impl RenderOnce for Press {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let Press {
            id,
            base,
            radius,
            own_border,
            rest,
            selected,
            disabled,
            shown_focused,
            on_click,
            roving,
        } = self;
        let enabled = on_click.is_some() && !disabled;
        // A group's member is a stop only while it is the group's one stop.
        let stop = roving.as_ref().is_none_or(|roving| roving.stop);
        let focus = on_click.is_some().then(|| match roving {
            Some(roving) => roving.focus,
            None => focus_handle(&id, window, cx),
        });
        let focused =
            shown_focused || (enabled && focus.as_ref().is_some_and(|f| f.is_focused(window)));
        let base = match &focus {
            Some(focus) => keyboard_stop(base, focus, enabled && stop),
            None => base,
        };
        let gap = if selected {
            FOCUS_RING_SELECTED
        } else {
            FOCUS_RING
        };
        base.when(disabled, |el| el.opacity(DISABLED))
            .when(enabled, |el| el.cursor_pointer())
            .when_some(rest.filter(|_| !selected), |el, color| {
                el.child(ring(0., 1., radius, own_border, color))
            })
            .when(selected, |el| {
                el.child(ring(FOCUS_RING, FOCUS_RING, radius, own_border, p.outline))
            })
            .when(focused, |el| {
                el.child(ring(gap, FOCUS_RING, radius, own_border, p.info))
            })
            .when_some(on_click.filter(|_| enabled), |el, f| {
                el.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    f(event, window, cx);
                })
            })
    }
}

// ---- buttons ----------------------------------------------------------

/// What a [`Btn`] is, in the design's four kinds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// The neutral primary: the text colour's plate. One in a view at most.
    Primary,
    /// Every other button: a control's edge around the words.
    #[default]
    Secondary,
    /// A lesser action beside another: a well behind the words.
    Quiet,
    /// What deletes, removes or stops: the error's words inside an edge.
    /// Its label ends in an ellipsis, and the page asks first.
    Danger,
}

impl Kind {
    /// Its fill, its words and its rim, at rest. The rim is always drawn,
    /// clear where there is none, so every kind is the same size. With
    /// Increase contrast a quiet button is rimmed too: its well on a card is
    /// 1.1:1, so the fill alone does not show where the button is.
    pub fn colors(self, p: &Palette) -> (Hsla, Hsla, Hsla) {
        let clear = gpui::transparent_black();
        match self {
            Kind::Primary => (p.solid, p.on_solid, p.solid),
            Kind::Secondary => (clear, p.text, p.edge),
            Kind::Quiet => (p.well, p.text, if p.contrast { p.edge } else { clear }),
            Kind::Danger => (clear, p.bad_text, p.edge),
        }
    }
}

/// A button, 28 high: `Btn::new(id, label)` is a secondary one, and
/// [`Btn::primary`], [`Btn::quiet`] and [`Btn::danger`] are the other
/// kinds; [`Btn::icon`] leads the words with a 14 px glyph, and
/// [`Btn::icon_only`] is the 28 px square with a glyph alone.
#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    kind: Kind,
    disabled: bool,
    tooltip: Option<SharedString>,
    shown_focused: bool,
    on_click: Option<ClickHandler>,
}

impl Btn {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Btn {
        Btn {
            id: id.into(),
            label: Some(label.into()),
            icon: None,
            kind: Kind::Secondary,
            disabled: false,
            tooltip: None,
            shown_focused: false,
            on_click: None,
        }
    }

    /// A square quiet button with a glyph and no words; say what it does
    /// with [`Btn::tooltip`].
    pub fn icon_only(id: impl Into<ElementId>, icon: &'static str) -> Btn {
        Btn {
            label: None,
            icon: Some(icon),
            kind: Kind::Quiet,
            ..Btn::new(id, "")
        }
    }

    pub fn icon(mut self, icon: &'static str) -> Btn {
        self.icon = Some(icon);
        self
    }

    pub fn kind(mut self, kind: Kind) -> Btn {
        self.kind = kind;
        self
    }

    pub fn primary(self) -> Btn {
        self.kind(Kind::Primary)
    }

    pub fn quiet(self) -> Btn {
        self.kind(Kind::Quiet)
    }

    pub fn danger(self) -> Btn {
        self.kind(Kind::Danger)
    }

    pub fn disabled(mut self, disabled: bool) -> Btn {
        self.disabled = disabled;
        self
    }

    /// Said when the pointer rests on it, disabled or not: why a disabled
    /// button is.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Btn {
        self.tooltip = Some(text.into());
        self
    }

    /// What a press does. The press stops here: a button on a clickable row
    /// does not also press the row.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Btn {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`].
    pub(crate) fn shown_focused(mut self) -> Btn {
        self.shown_focused = true;
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let kind = self.kind;
        let (bg, fg, rim) = kind.colors(&p);
        let enabled = !self.disabled && self.on_click.is_some();
        let square = self.label.is_none();
        let button = Press::new(self.id)
            .own_border(1.)
            .disabled(self.disabled)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(28.))
            .rounded(px(8.))
            .border_1()
            .border_color(rim)
            .bg(bg)
            .ts(13., 18.)
            .medium()
            .whitespace_nowrap()
            .text_color(fg)
            .map(|b| if square { b.w(px(28.)) } else { b.px(px(12.)) })
            .when(enabled, |b| {
                // The design draws no hover: the plate fades a little, a
                // button with no fill takes a well, and the one already on a
                // well takes an edge instead. Pressed, any of them fades more.
                b.map(|b| match kind {
                    Kind::Primary => b.hover(|s| s.opacity(0.9)),
                    Kind::Quiet => b.hover(|s| s.border_color(p.edge)),
                    Kind::Secondary | Kind::Danger => b.hover(|s| s.bg(p.well)),
                })
                .active(|s| s.opacity(0.85))
            })
            .when_some(self.icon, |b, path| b.child(icon(path, 14., fg)))
            .when_some(self.label.filter(|l| !l.is_empty()), |b, label| {
                b.child(label)
            })
            .when_some(self.tooltip, |b, text| b.tooltip(tip(text)))
            .when(self.shown_focused, Press::shown_focused);
        match self.on_click {
            Some(f) => button.on_click(move |event, window, cx| f(event, window, cx)),
            None => button,
        }
    }
}

/// How long a [`CopyButton`] says it copied.
pub const COPIED_FOR: Duration = Duration::from_secs(2);

/// Whether a [`CopyButton`] that last copied at `at` still says so.
fn says_copied(at: Option<Instant>) -> bool {
    at.is_some_and(|at| at.elapsed() < COPIED_FOR)
}

/// What a [`CopyButton`] keeps between frames: when it last copied.
struct Copied(Option<Instant>);

/// **Copy Command**: a secondary [`Btn`] with the copy glyph that puts
/// `text` on the clipboard, then says so — `done` in place of `label` — for
/// [`COPIED_FOR`], so a press that changes nothing else on the window still
/// shows. The page gives both words in the window's language.
///
/// It keeps when it copied under its id, as a control keeps its focus, so a
/// page holds no state for it; a dialog closed and opened again starts it
/// afresh.
#[derive(IntoElement)]
pub struct CopyButton {
    id: ElementId,
    text: SharedString,
    label: SharedString,
    done: SharedString,
}

impl CopyButton {
    pub fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        label: impl Into<SharedString>,
        done: impl Into<SharedString>,
    ) -> CopyButton {
        CopyButton {
            id: id.into(),
            text: text.into(),
            label: label.into(),
            done: done.into(),
        }
    }
}

impl RenderOnce for CopyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let copied = window.use_keyed_state(self.id.clone(), cx, |_, _| Copied(None));
        let label = if says_copied(copied.read(cx).0) {
            self.done
        } else {
            self.label
        };
        let text = self.text;
        Btn::new(self.id, label)
            .icon(ic::COPY)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                copied.update(cx, |copied, cx| {
                    copied.0 = Some(Instant::now());
                    cx.notify();
                });
                // Its label again once the moment is over, whether or not
                // anything else redraws the window by then.
                let copied = copied.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    copied.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            })
    }
}

// ---- switch -----------------------------------------------------------

/// A switch's plate, rim and knob, on and off: the primary's plate with a
/// knob in its words' colour, or a control's edge with a knob in it.
pub fn switch_colors(on: bool, p: &Palette) -> (Hsla, Hsla, Hsla) {
    if on {
        (p.solid, p.solid, p.on_solid)
    } else {
        (gpui::transparent_black(), p.edge, p.edge)
    }
}

/// An on/off switch, 34×20, with the knob at the right when on. With
/// [`Switch::word`] it says its state first, in the sub-line colour ("On" /
/// "Paused"), and a press on the word moves it too.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    word: Option<(SharedString, SharedString)>,
    disabled: bool,
    shown_focused: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Switch {
        Switch {
            id: id.into(),
            on,
            word: None,
            disabled: false,
            shown_focused: false,
            on_toggle: None,
        }
    }

    /// The word before the switch: `on_text` while on, `off_text` while
    /// off, both in the window's language.
    pub fn word(
        mut self,
        on_text: impl Into<SharedString>,
        off_text: impl Into<SharedString>,
    ) -> Switch {
        self.word = Some((on_text.into(), off_text.into()));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Switch {
        self.disabled = disabled;
        self
    }

    /// Called with the value the switch is being moved **to**.
    pub fn on_toggle(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Switch {
        self.on_toggle = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`].
    pub(crate) fn shown_focused(mut self) -> Switch {
        self.shown_focused = true;
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let on = self.on;
        let enabled = !self.disabled && self.on_toggle.is_some();
        let focus = self
            .on_toggle
            .is_some()
            .then(|| focus_handle(&self.id, window, cx));
        let focused = self.shown_focused
            || (enabled && focus.as_ref().is_some_and(|f| f.is_focused(window)));
        let (plate, rim, knob) = switch_colors(on, &p);
        // The knob is placed by flex inside the 1 px rim, 2 px in from it:
        // 3 px from the outside on every side, and no absolute offset for
        // the rim to shift. The ring is the switch's, not the word's.
        let switch = h_flex()
            .flex_none()
            .w(px(34.))
            .h(px(20.))
            .px(px(2.))
            .rounded_full()
            .border_1()
            .border_color(rim)
            .bg(plate)
            .map(|s| if on { s.justify_end() } else { s.justify_start() })
            .child(dot(knob, 14.))
            .when(focused, |s| {
                s.child(ring(FOCUS_RING, FOCUS_RING, 10., 1., p.info))
            });
        let word = self.word.map(|(on_text, off_text)| {
            div()
                .ts(12., 16.)
                .text_color(p.text2)
                .whitespace_nowrap()
                .child(if on { on_text } else { off_text })
        });
        let el = h_flex()
            .id(self.id)
            .flex_none()
            .gap(px(8.))
            .children(word)
            .child(switch);
        let el = match &focus {
            Some(focus) => keyboard_stop(el, focus, enabled),
            None => el,
        };
        el.when(self.disabled, |el| el.opacity(DISABLED))
            .when(enabled, |el| el.cursor_pointer())
            .when_some(self.on_toggle.filter(|_| enabled), |el, f| {
                el.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    f(&!on, window, cx);
                })
            })
    }
}

// ---- level --------------------------------------------------------------

/// How far in from each end of a [`Level`] its first and last stops are:
/// the thumb's half, so the thumb never leaves the control.
const LEVEL_INSET: f32 = 8.;

/// A [`Level`]'s width unless said: the Deck page's.
pub const LEVEL_WIDTH: f32 = 168.;

/// The stop under a pointer `x` px from a level's left edge, for a level
/// `width` wide that runs from `min` to `max`. The ends are the first and
/// last stops' centres, `LEVEL_INSET` in, and a pointer past either is that
/// end's stop.
pub fn level_value_at(x: f32, width: f32, min: u16, max: u16) -> u16 {
    if max <= min {
        return min;
    }
    let travel = (width - 2. * LEVEL_INSET).max(1.);
    let t = ((x - LEVEL_INSET) / travel).clamp(0., 1.);
    // `t` is in 0..=1, so the step is in 0..=max-min and fits.
    min + (t * f32::from(max - min)).round() as u16
}

/// Where a key moves a level that shows `value`: Left and Down one stop
/// down, Right and Up one up, Home to `min`, End to `max`, never past
/// either end. `None` for any other key.
pub fn level_key(key: &str, value: u16, min: u16, max: u16) -> Option<u16> {
    let max = max.max(min);
    let value = value.clamp(min, max);
    match key {
        "left" | "down" => Some(value.saturating_sub(1).max(min)),
        "right" | "up" => Some(value.saturating_add(1).min(max)),
        "home" => Some(min),
        "end" => Some(max),
        _ => None,
    }
}

/// A level's thumb and fill, as the part of its travel `value` is at.
fn level_fraction(value: u16, min: u16, max: u16) -> f32 {
    if max <= min {
        return 0.;
    }
    f32::from(value.clamp(min, max) - min) / f32::from(max - min)
}

/// What a level keeps between frames: its focus, where it was laid out
/// (the pointer is read against it), and the stop it shows or last sent.
struct LevelState {
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    value: u16,
}

/// The payload of a level's drag; the thumb is the level itself, so it
/// draws nothing. It carries the level's state, so a drag moves one level
/// and not every level on the page.
#[derive(Clone)]
struct LevelDrag(EntityId);

impl Render for LevelDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// One of the deck's stepped settings (speaker, mic gain, brightness): a
/// rail with a tick for every stop, the part up to the value filled, a
/// thumb, and the value in mono as the deck prints it.
///
/// It holds no value of its own: the page passes the one it shows and gets
/// [`Level::on_change`] with each new stop, once per stop, as the pointer
/// presses or drags it or as Left, Right, Up, Down, Home and End move it.
/// A page shows the stop it sent until the deck agrees, or the thumb would
/// lag the pointer by a round trip.
#[derive(IntoElement)]
pub struct Level {
    id: ElementId,
    value: u16,
    min: u16,
    max: u16,
    width: f32,
    disabled: bool,
    shown_focused: bool,
    on_change: Option<LevelHandler>,
}

impl Level {
    pub fn new(id: impl Into<ElementId>, value: u16, min: u16, max: u16) -> Level {
        Level {
            id: id.into(),
            value,
            min,
            max,
            width: LEVEL_WIDTH,
            disabled: false,
            shown_focused: false,
            on_change: None,
        }
    }

    /// The rail's width, 168 unless said; the value takes 26 more.
    pub fn width(mut self, width: f32) -> Level {
        self.width = width;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Level {
        self.disabled = disabled;
        self
    }

    /// Called with each new stop the owner moves it to.
    pub fn on_change(mut self, f: impl Fn(&u16, &mut Window, &mut App) + 'static) -> Level {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`].
    pub(crate) fn shown_focused(mut self) -> Level {
        self.shown_focused = true;
        self
    }
}

/// Move the level whose state is `state` to `value`, and say so if it moved.
fn level_moved(
    state: &Entity<LevelState>,
    value: u16,
    on_change: &LevelHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let moved = state.update(cx, |level, _| {
        let moved = level.value != value;
        level.value = value;
        moved
    });
    if moved {
        on_change(&value, window, cx);
    }
}

impl RenderOnce for Level {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let (min, max, width) = (self.min, self.max.max(self.min), self.width);
        let value = self.value.clamp(min, max);
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| LevelState {
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            value,
        });
        // What the page shows now is what the next key moves from.
        state.update(cx, |level, _| level.value = value);
        let focus = state.read(cx).focus.clone();
        let enabled = !self.disabled && self.on_change.is_some();
        let focused = self.shown_focused || (enabled && focus.is_focused(window));
        let travel = (width - 2. * LEVEL_INSET).max(0.);
        let reach = travel * level_fraction(value, min, max);
        let steps = max - min;
        // Ticks first, so the rail covers their middle and they show a
        // pixel above and below it; then the fill and the thumb.
        let ticks = (0..=steps).map(|i| {
            let at = if steps == 0 {
                0.
            } else {
                travel * f32::from(i) / f32::from(steps)
            };
            div()
                .absolute()
                .top(px(9.))
                .left(px(LEVEL_INSET + at))
                .w(px(1.))
                .h(px(6.))
                .bg(p.edge)
        });
        let thumb = div()
            .absolute()
            .top(px(4.))
            .left(px(reach))
            .size(px(16.))
            .rounded_full()
            .bg(p.solid)
            .border_1()
            .border_color(p.win)
            .when(focused, |t| {
                t.child(ring(FOCUS_RING, FOCUS_RING, 8., 1., p.info))
            });
        let rail = div()
            .id(self.id)
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(24.))
            .children(ticks)
            .child(
                div()
                    .absolute()
                    .left(px(LEVEL_INSET))
                    .right(px(LEVEL_INSET))
                    .top(px(10.))
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(p.edge),
            )
            .child(
                div()
                    .absolute()
                    .left(px(LEVEL_INSET))
                    .top(px(10.))
                    .h(px(4.))
                    .w(px(reach))
                    .rounded(px(2.))
                    .bg(p.text),
            )
            .child(thumb)
            .child({
                // Where the level is, for the pointer: a press reads its x
                // against it.
                let state = state.clone();
                canvas(
                    move |bounds, _, cx| state.update(cx, |level, _| level.bounds = bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            });
        let rail = match self.on_change {
            // Disabled: it keeps the keyboard if it had it, and is passed by.
            Some(_) if !enabled => keyboard_stop(rail, &focus, false),
            Some(on_change) => {
                let drag = LevelDrag(state.entity_id());
                let press = {
                    let (state, on_change) = (state.clone(), on_change.clone());
                    move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                        let left = state.read(cx).bounds.left();
                        let x = f32::from(event.position.x - left);
                        let stop = level_value_at(x, width, min, max);
                        level_moved(&state, stop, &on_change, window, cx);
                    }
                };
                let dragged = {
                    let (state, on_change) = (state.clone(), on_change.clone());
                    move |event: &DragMoveEvent<LevelDrag>, window: &mut Window, cx: &mut App| {
                        if event.drag(cx).0 != state.entity_id() {
                            return;
                        }
                        let x = f32::from(event.event.position.x - event.bounds.left());
                        let stop = level_value_at(x, width, min, max);
                        level_moved(&state, stop, &on_change, window, cx);
                    }
                };
                let keys = {
                    let state = state.clone();
                    move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let shown = state.read(cx).value;
                        if let Some(stop) = level_key(&event.keystroke.key, shown, min, max) {
                            cx.stop_propagation();
                            level_moved(&state, stop, &on_change, window, cx);
                        }
                    }
                };
                keyboard_stop(rail, &focus, true)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, press)
                    .on_drag(drag, |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    })
                    .on_drag_move(dragged)
                    .on_key_down(keys)
            }
            None => rail,
        };
        h_flex()
            .flex_none()
            .gap(px(12.))
            .when(self.disabled, |el| el.opacity(DISABLED))
            .child(rail)
            .child(
                h_flex()
                    .flex_none()
                    .min_w(px(14.))
                    .justify_end()
                    .child(
                        div()
                            .ts(12., 16.)
                            .mono()
                            .text_color(p.text2)
                            .child(value.to_string()),
                    ),
            )
    }
}

// ---- segmented ----------------------------------------------------------

/// A segment's fill, words and rim: the chosen one is a surface with the
/// selection's outline, the others are words on the well.
pub fn segment_colors(chosen: bool, p: &Palette) -> (Hsla, Hsla, Hsla) {
    let clear = gpui::transparent_black();
    if chosen {
        (p.surf, p.text, p.outline)
    } else {
        (clear, p.text2, clear)
    }
}

/// The well a [`Segmented`]'s options sit in, drawn on `track`: 2 px around
/// them, so 26 high around 22 px options. With Increase contrast a
/// control's edge rims it (a well on a card is 1.1:1) and takes a pixel of
/// that, so the options and the size stay where they are.
fn segment_track<E: Styled>(track: E, p: &Palette) -> E {
    let track = track
        .h_flex()
        .flex_none()
        .gap(px(2.))
        .rounded(px(8.))
        .bg(p.well);
    if p.contrast {
        track.p(px(1.)).border_1().border_color(p.edge)
    } else {
        track.p(px(2.))
    }
}

/// A short choice of one, side by side on a well (Text size, the case
/// colour, a provider). It is one stop on the Tab order, a [`Group`] of its
/// options: Tab reaches the chosen one, Left and Right (and Home and End)
/// move the keyboard along them, and a press, Return or Space chooses.
#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    disabled: bool,
    shown_focused: Option<usize>,
    on_select: Option<SelectHandler>,
}

impl Segmented {
    pub fn new(id: impl Into<ElementId>, options: Vec<SharedString>, selected: usize) -> Segmented {
        Segmented {
            id: id.into(),
            options,
            selected,
            disabled: false,
            shown_focused: None,
            on_select: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Segmented {
        self.disabled = disabled;
        self
    }

    /// Called with the index of the option chosen, when it is not the one
    /// already chosen.
    pub fn on_select(mut self, f: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Segmented {
        self.on_select = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`]: the option at `index`.
    pub(crate) fn shown_focused(mut self, index: usize) -> Segmented {
        self.shown_focused = Some(index);
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let selected = self.selected;
        let on_select = self.on_select.filter(|_| !self.disabled);
        let shown_focused = self.shown_focused;
        let segments = self.options.into_iter().enumerate().map(|(i, option)| {
            let chosen = i == selected;
            let (fill, words, rim) = segment_colors(chosen, &p);
            let segment = Press::new(i)
                .radius(6.)
                .own_border(1.)
                .flex()
                .flex_none()
                .items_center()
                .h(px(22.))
                .px(px(11.))
                .rounded(px(6.))
                .border_1()
                .border_color(rim)
                .bg(fill)
                .ts(13., 18.)
                .medium()
                .whitespace_nowrap()
                .text_color(words)
                .when(!chosen, |s| s.hover(|s| s.text_color(p.text)))
                .when(shown_focused == Some(i), Press::shown_focused)
                .child(option);
            match on_select.clone() {
                Some(f) => segment.on_click(move |_, window, cx| {
                    if !chosen {
                        f(&i, window, cx);
                    }
                }),
                None => segment,
            }
        });
        // The arrows move the keyboard and never choose: choosing writes
        // the setting, so it waits for a press.
        segment_track(Group::new(self.id, Layout::Row), &p)
            .when(self.disabled, |el| el.opacity(DISABLED))
            .selected(Some(selected))
            .members(segments)
    }
}

// ---- popup ------------------------------------------------------------------

/// The step a stepped setting shows as: the first of `steps` (ascending)
/// at or above `value`, the way the deck rounds a value it has no step for
/// up to its next one, and past the last, the last. With the deck's Idle
/// home steps `[0, 30, 60, 120]`: 0 is Never, 1 to 30 is 30 s, 31 to 60 is
/// 1 min and anything above is 2 min. An empty list is 0.
pub fn step_at(value: u16, steps: &[u16]) -> usize {
    steps
        .iter()
        .position(|&step| step >= value)
        .unwrap_or(steps.len().saturating_sub(1))
}

/// What a menu button keeps between frames: its focus, whether its menu is
/// up, the menu while it is, and whether the keyboard opened it (then the
/// keyboard comes back to the button when it closes).
struct PopupState {
    focus: FocusHandle,
    open: bool,
    menu: Option<Entity<PopupMenu>>,
    keyboard: bool,
}

/// What the menu's popover holds as its trigger: the button, drawn as it is
/// whether the menu is up or not.
#[derive(IntoElement)]
struct Trigger(Stateful<Div>);

impl Selectable for Trigger {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.0
    }
}

/// Whether a key pressed on a menu button opens its menu: Return or Space,
/// with no modifier, and not the repeat of a key held down (the Return
/// that chose an item, still down once the keyboard is back on the
/// button).
pub fn opens_menu(key: &str, modifiers: &Modifiers, held: bool) -> bool {
    matches!(key, "enter" | "space") && !modifiers.modified() && !held
}

/// A button that opens a menu of choices (Idle home, Language, the deck
/// button of a macro key): the value chosen and a chevron, on a well, 28
/// high. A press opens the menu under it, and so do Return and Space while
/// it has the keyboard ([`opens_menu`]); in the menu the arrow keys move,
/// Return chooses and Escape closes, and the keyboard comes back to the
/// button. The keys open it as they go down: gpui presses a focused control
/// as a key comes **up**, and the Return that chose an item comes up on the
/// button once the menu has handed the keyboard back, which would open the
/// menu again. The menu is gpui-component's, built each time it opens by
/// [`PopupButton::menu`]:
///
/// ```text
/// PopupButton::new("idle-home", self.tr("2 min")).width(132.).menu(move |menu, _, _| {
///     menu.item(PopupMenuItem::new(never).checked(at == 0).on_click(..))
/// })
/// ```
///
/// A menu button without a menu, or a disabled one, is drawn but opens
/// nothing and is no Tab stop.
#[derive(IntoElement)]
pub struct PopupButton {
    id: ElementId,
    value: SharedString,
    width: Option<f32>,
    disabled: bool,
    shown_focused: bool,
    menu: Option<MenuBuilder>,
}

impl PopupButton {
    pub fn new(id: impl Into<ElementId>, value: impl Into<SharedString>) -> PopupButton {
        PopupButton {
            id: id.into(),
            value: value.into(),
            width: None,
            disabled: false,
            shown_focused: false,
            menu: None,
        }
    }

    /// A fixed width (132 and 96 on Faces & Keys); a long value is cut at
    /// its end. Unless said, as wide as its value.
    pub fn width(mut self, width: f32) -> PopupButton {
        self.width = Some(width);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> PopupButton {
        self.disabled = disabled;
        self
    }

    /// The menu, built afresh each time it opens: add items with
    /// `PopupMenuItem::new(label).checked(..).on_click(..)`, separators,
    /// `min_w`. Choosing an item runs its click and closes the menu.
    pub fn menu(
        mut self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> PopupButton {
        self.menu = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`].
    pub(crate) fn shown_focused(mut self) -> PopupButton {
        self.shown_focused = true;
        self
    }
}

impl RenderOnce for PopupButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| PopupState {
            focus: cx.focus_handle(),
            open: false,
            menu: None,
            keyboard: false,
        });
        let enabled = !self.disabled && self.menu.is_some();
        if !enabled {
            // A menu left up by a button that has since been disabled goes,
            // and does not come back when it is enabled again.
            state.update(cx, |popup, _| {
                popup.open = false;
                popup.menu = None;
            });
        }
        let (focus, open) = {
            let popup = state.read(cx);
            (popup.focus.clone(), popup.open)
        };
        let focused = self.shown_focused || (enabled && focus.is_focused(window));
        let button = div()
            .id(self.id.clone())
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .h(px(28.))
            .pl(px(11.))
            .pr(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(p.edge)
            .bg(p.well)
            .ts(13., 18.)
            .medium()
            .text_color(p.text)
            .when_some(self.width, |b, width| b.w(px(width)))
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .when(self.width.is_some(), |v| v.flex_1().truncate())
                    .child(self.value),
            )
            .child(icon(ic::CHEVRON_DOWN, 13., p.text2))
            .when(focused, |b| {
                b.child(ring(FOCUS_RING, FOCUS_RING, 8., 1., p.info))
            });
        let has_menu = self.menu.is_some();
        let Some(build) = self.menu.filter(|_| enabled) else {
            // Disabled: it keeps the keyboard if it had it, and is passed by.
            let button = if has_menu {
                keyboard_stop(button, &focus, false)
            } else {
                button
            };
            return button
                .when(self.disabled, |b| b.opacity(DISABLED))
                .into_any_element();
        };
        // Return or Space going down while the button has the keyboard: the
        // menu opens with the keyboard in it. The pointer opens it through
        // the popover, on the press itself. The button has no click at all,
        // so a key that comes up on it presses nothing.
        let button = keyboard_stop(button, &focus, true)
            .cursor_pointer()
            .hover(|b| b.border_color(p.text3))
            .on_key_down({
                let state = state.clone();
                move |event, _, cx| {
                    let stroke = &event.keystroke;
                    if opens_menu(&stroke.key, &stroke.modifiers, event.is_held) {
                        cx.stop_propagation();
                        state.update(cx, |popup, cx| {
                            popup.open = true;
                            popup.keyboard = true;
                            cx.notify();
                        });
                    }
                }
            });
        Popover::new(SharedString::from(format!("popup:{:?}", self.id)))
            .appearance(false)
            .overlay_closable(false)
            .anchor(Corner::TopLeft)
            .open(open)
            .on_open_change({
                let state = state.clone();
                move |open, _, cx| {
                    state.update(cx, |popup, cx| {
                        popup.open = *open;
                        if *open {
                            // The pointer opened it.
                            popup.keyboard = false;
                        } else {
                            popup.menu = None;
                        }
                        cx.notify();
                    });
                }
            })
            .trigger(Trigger(button))
            .content(move |_, window, cx| {
                // The menu is built once each time it opens and kept while
                // it is up: the content is asked for on every frame.
                if let Some(menu) = state.read(cx).menu.clone() {
                    return menu;
                }
                let (keyboard, back) = {
                    let popup = state.read(cx);
                    (popup.keyboard, popup.focus.clone())
                };
                let build = build.clone();
                let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
                    // Opened from the keyboard, the keyboard comes back to
                    // the button; opened with the pointer, it goes back to
                    // the window (the shell keeps it there).
                    let menu = if keyboard {
                        menu.action_context(back)
                    } else {
                        menu
                    };
                    build(menu, window, cx)
                });
                state.update(cx, |popup, _| popup.menu = Some(menu.clone()));
                menu.focus_handle(cx).focus(window);
                // A choice, Escape or a press outside dismisses the menu:
                // the popover closes with it.
                let popover = cx.entity();
                window
                    .subscribe(&menu, cx, {
                        let state = state.clone();
                        move |_, _: &DismissEvent, window, cx| {
                            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                            state.update(cx, |popup, _| popup.menu = None);
                        }
                    })
                    .detach();
                menu
            })
            .into_any_element()
    }
}

// ---- chip ---------------------------------------------------------------

/// A chip's fill, content and rim: the primary's plate while on, a
/// control's edge around the sub-line colour while off.
pub fn chip_colors(on: bool, p: &Palette) -> (Hsla, Hsla, Hsla) {
    if on {
        (p.solid, p.on_solid, p.solid)
    } else {
        (gpui::transparent_black(), p.text2, p.edge)
    }
}

/// What a [`Chip`] carries.
enum ChipContent {
    Text(SharedString),
    Icon(&'static str),
    Word(SharedString),
}

/// A toggle 28 high in a control's edge, or on the primary's plate while
/// on: 30 wide with a glyph or a character or two (a modifier of a macro
/// key's shortcut), or as wide as a word in mono ([`Chip::word`], a token's
/// scope).
#[derive(IntoElement)]
pub struct Chip {
    id: ElementId,
    content: ChipContent,
    on: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    shown_focused: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Chip {
    /// A chip with a character or two on it, in mono.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>, on: bool) -> Chip {
        Chip {
            id: id.into(),
            content: ChipContent::Text(text.into()),
            on,
            disabled: false,
            tooltip: None,
            shown_focused: false,
            on_toggle: None,
        }
    }

    /// A chip with a 14 px glyph on it.
    pub fn icon(id: impl Into<ElementId>, path: &'static str, on: bool) -> Chip {
        Chip {
            content: ChipContent::Icon(path),
            ..Chip::new(id, "", on)
        }
    }

    /// A modifier's chip: its mark, and its name when the pointer rests on
    /// it (the name is macOS's own and is not translated).
    pub fn modifier(id: impl Into<ElementId>, m: Mod, on: bool) -> Chip {
        Chip::icon(id, m.icon(), on).tooltip(m.name())
    }

    /// A chip as wide as `word`, in mono 12/16 with 10 px either side.
    pub fn word(id: impl Into<ElementId>, word: impl Into<SharedString>, on: bool) -> Chip {
        Chip {
            content: ChipContent::Word(word.into()),
            ..Chip::new(id, "", on)
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Chip {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Chip {
        self.tooltip = Some(text.into());
        self
    }

    /// Called with the value the chip is being moved **to**.
    pub fn on_toggle(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Chip {
        self.on_toggle = Some(Rc::new(f));
        self
    }

    /// See [`Press::shown_focused`].
    pub(crate) fn shown_focused(mut self) -> Chip {
        self.shown_focused = true;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let on = self.on;
        let (fill, ink, rim) = chip_colors(on, &p);
        let chip = Press::new(self.id)
            .own_border(1.)
            .disabled(self.disabled)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(28.))
            .rounded(px(8.))
            .border_1()
            .border_color(rim)
            .bg(fill)
            .mono()
            .medium()
            .text_color(ink)
            .map(|c| match self.content {
                ChipContent::Text(text) => c.w(px(30.)).ts(13., 18.).child(text),
                ChipContent::Icon(path) => c.w(px(30.)).ts(13., 18.).child(icon(path, 14., ink)),
                ChipContent::Word(word) => c.px(px(10.)).ts(12., 16.).child(word),
            })
            .when_some(self.tooltip, |c, text| c.tooltip(tip(text)))
            .when(self.shown_focused, Press::shown_focused);
        match self.on_toggle {
            Some(f) => chip.on_click(move |_, window, cx| f(&!on, window, cx)),
            None => chip,
        }
    }
}

// ---- text field -------------------------------------------------------------

/// A text field, 28 high, on a well. gpui-component's `Input` keeps the
/// caret, the selection, the input method and undo; its edge is the
/// control's edge and, while it has the keyboard, the one blue (both from
/// the theme `style::install` writes). It is a Tab stop of its own. It is
/// as wide as what holds it.
pub fn field(state: &Entity<InputState>, p: &Palette) -> Input {
    // `Input::h` is the height of a multi-line field and hides the style's
    // own, so the style's is set through the trait.
    gpui::Styled::h(Input::new(state), px(28.))
        .py(px(0.))
        .px(px(10.))
        .rounded(px(8.))
        .bg(p.well)
        .text_size(px(13.))
}

// ---- containers -------------------------------------------------------

/// A 32 px tile with an 18 px glyph in it, at the head of a row. Always the
/// sub-line colour: a mark is not a state.
pub fn tile(path: &'static str, p: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded(px(8.))
        .bg(p.well)
        .child(icon(path, 18., p.text2))
}

/// A 20 px box that centres what it is given and draws nothing itself: the
/// lead of a row whose mark is an icon or a radio.
pub fn bare(el: impl IntoElement) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .child(el)
}

/// A label over its body, 8 px apart. Sections stack 20 px apart in the
/// page's column, so a section has no margin of its own.
pub fn section(title: impl Into<SharedString>, body: impl IntoElement, p: &Palette) -> Div {
    v_flex().gap(px(8.)).child(label(title, p)).child(body)
}

/// A card: a surface with a rule around it and one between each of
/// `children`.
///
/// The rules are the card's, so a row does not need to know whether it is
/// the last. Each is drawn over the bottom pixel of the child above it, as
/// a row's own bottom border would be, so a row stays 44 or 56 high with its
/// rule and a card of four rows is as tall as the design's. Only the last
/// child is not wrapped, which is what lets a last row that
/// [`grow`](Row::grow)s take a stretched card's spare height. A child with
/// a fill of its own at the card's corner rounds itself: gpui clips to
/// rectangles, never to a radius. A card whose rows are one choice is a
/// [`Group`] drawn as one ([`Group::card`]).
pub fn card(children: Vec<AnyElement>, p: &Palette) -> Div {
    card_frame(div(), p).children(ruled(children, p.line))
}

/// A card's own look, on `el`: a surface with a rule around it, 12 px
/// round, its children one under another.
fn card_frame<E: Styled>(el: E, p: &Palette) -> E {
    el.v_flex()
        .w_full()
        .rounded(px(12.))
        .bg(p.surf)
        .border_1()
        .border_color(p.line)
}

/// `children` as a card holds them: a `rule` over the bottom pixel of each
/// but the last, and the last as it is (see [`card`]).
fn ruled(children: Vec<AnyElement>, rule: Hsla) -> impl Iterator<Item = AnyElement> {
    let last = children.len().saturating_sub(1);
    children.into_iter().enumerate().map(move |(i, child)| {
        if i < last {
            v_flex()
                .relative()
                .w_full()
                .child(child)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(1.))
                        .bg(rule),
                )
                .into_any_element()
        } else {
            child
        }
    })
}

/// One row of a [`card`]: an optional lead (a [`tile`], a [`bare`] icon),
/// a title over an optional sub-line, and what the row offers on the right.
#[derive(IntoElement)]
pub struct Row {
    lead: Option<AnyElement>,
    title: Option<AnyElement>,
    sub: Option<AnyElement>,
    right: Vec<AnyElement>,
    pad: f32,
    grow: bool,
    click: Option<(ElementId, ClickHandler)>,
    roving: Option<Roving>,
}

impl Default for Row {
    fn default() -> Row {
        Row {
            lead: None,
            title: None,
            sub: None,
            right: Vec::new(),
            pad: 16.,
            grow: false,
            click: None,
            roving: None,
        }
    }
}

impl Row {
    pub fn new() -> Row {
        Row::default()
    }

    /// What leads the row: a [`tile`], or a [`bare`] icon or radio.
    pub fn lead(mut self, el: impl IntoElement) -> Row {
        self.lead = Some(el.into_any_element());
        self
    }

    /// A plain title, wrapping when it must.
    pub fn title(self, text: impl Into<SharedString>) -> Row {
        self.title_el(para(text))
    }

    /// A title made of more than words.
    pub fn title_el(mut self, el: impl IntoElement) -> Row {
        self.title = Some(el.into_any_element());
        self
    }

    /// A plain sub-line, wrapping when it must. A row with one is 56 high.
    /// (The design's name for it; a row is never subtracted from.)
    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, text: impl Into<SharedString>) -> Row {
        self.sub_el(para(text))
    }

    pub fn sub_el(mut self, el: impl IntoElement) -> Row {
        self.sub = Some(el.into_any_element());
        self
    }

    /// One more thing on the right, 10 px after the one before.
    pub fn right(mut self, el: impl IntoElement) -> Row {
        self.right.push(el.into_any_element());
        self
    }

    /// The row's sides, 16 px unless said (14 in the deck's own card).
    pub fn pad(mut self, pad: f32) -> Row {
        self.pad = pad;
        self
    }

    /// Take the spare height of a stretched card. Only a card's last row
    /// can (see [`card`]).
    pub fn grow(mut self) -> Row {
        self.grow = true;
        self
    }

    /// Make the whole row the thing pressed: a radio's choice. It is then a
    /// Tab stop of its own — or, with the other choices of its card, one
    /// member of a [`Group`] ([`Row::roving`]) — pressed by Return and Space,
    /// and its keyboard ring is drawn just inside it, where a ring outside
    /// would cross its card's rim. A button on the row keeps its own press.
    pub fn on_click(
        mut self,
        id: impl Into<ElementId>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Row {
        self.click = Some((id.into(), Rc::new(f)));
        self
    }

    /// One of a [`Group`]'s members: a row with a press takes the group's
    /// focus handle in place of its own, and is a stop on the Tab order only
    /// while it is the group's.
    pub fn roving(mut self, roving: &Roving) -> Row {
        self.roving = Some(roving.clone());
        self
    }
}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let height = if self.sub.is_some() {
            ROW_TWO_LINES
        } else {
            ROW_ONE_LINE
        };
        let title = self
            .title
            .map(|title| div().ts(13., 18.).medium().text_color(p.text).child(title));
        let sub = self
            .sub
            .map(|sub| div().ts(12., 16.).text_color(p.text2).child(sub));
        let row = h_flex()
            .w_full()
            .gap(px(12.))
            .min_h(px(height))
            .py(px(6.))
            .px(px(self.pad))
            .when(self.grow, |row| row.flex_1())
            .children(self.lead)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .children(title)
                    .children(sub),
            )
            .when(!self.right.is_empty(), |row| {
                row.child(h_flex().flex_none().gap(px(10.)).children(self.right))
            });
        match self.click {
            Some((id, f)) => {
                let (focus, stop) = match self.roving {
                    Some(roving) => (roving.focus, roving.stop),
                    None => (focus_handle(&id, window, cx), true),
                };
                let focused = focus.is_focused(window);
                keyboard_stop(row.id(id), &focus, stop)
                    .cursor_pointer()
                    .when(focused, |row| {
                        row.child(inset_ring(ROW_RING_INSET, ROW_RING_RADIUS, p.info))
                    })
                    .on_click(move |event, window, cx| f(event, window, cx))
                    .into_any_element()
            }
            None => row.into_any_element(),
        }
    }
}

/// How far inside a clickable [`Row`] its keyboard ring is drawn, and the
/// ring's radius: inside a card's 12 px corner at either end.
const ROW_RING_INSET: f32 = 2.;
const ROW_RING_RADIUS: f32 = 8.;

// ---- groups -----------------------------------------------------------------

/// What a [`Group`] takes as a member: a [`Press`], or a [`Row`] with a
/// press.
pub trait Member: IntoElement {
    /// The id it is drawn under, which the group keeps its keyboard by;
    /// `None` for a row with nothing to press.
    fn key(&self) -> Option<ElementId>;

    /// Whether the keyboard can reach it: it has a press and is not
    /// disabled.
    fn reachable(&self) -> bool;

    /// Take the group's keyboard ([`Press::roving`], [`Row::roving`]).
    fn roving(self, roving: &Roving) -> Self;
}

impl Member for Press {
    fn key(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn reachable(&self) -> bool {
        self.on_click.is_some() && !self.disabled
    }

    fn roving(self, roving: &Roving) -> Press {
        Press::roving(self, roving)
    }
}

impl Member for Row {
    fn key(&self) -> Option<ElementId> {
        self.click.as_ref().map(|(id, _)| id.clone())
    }

    fn reachable(&self) -> bool {
        self.click.is_some()
    }

    fn roving(self, roving: &Roving) -> Row {
        Row::roving(self, roving)
    }
}

/// What a [`Group`] keeps between frames: a focus handle for each member,
/// by the member's key, so the keyboard stays with a member that moves.
struct Rover(Vec<(ElementId, FocusHandle)>);

/// A member of a [`Group`] waiting to be drawn: its key, whether the
/// keyboard can reach it, and what draws it once it has its keyboard.
struct Seat {
    key: ElementId,
    enabled: bool,
    draw: Box<dyn FnOnce(&Roving) -> AnyElement>,
}

/// Controls that are one choice or one set — a segmented control's options,
/// the sidebar's pages, the face strip, the key grid, a card of radio
/// choices — as **one stop** on the window's Tab order.
///
/// Tab enters the group on its [`selected`](Group::selected) member (on
/// the first while none is), and from whichever member has the keyboard it
/// leaves the group for the control after it ([`group_stop`]). Inside, the
/// arrow keys of its [`Layout`] move the keyboard from member to member,
/// Home and End to the first and the last, past members that cannot be
/// pressed and never round an end ([`group_step`]); Return or Space presses
/// the member that has it, as on any control. The arrows never choose: a
/// choice writes a setting, or starts a download, so it waits for a press,
/// and a press with the pointer still leaves the keyboard where it was. A
/// key with a modifier is left to whatever holds the group (Option-Left on
/// the face strip).
///
/// Its members are [`Press`]es or [`Row`]s ([`Group::members`]), or one
/// drawn inside more than itself, as a face's plate is over its name
/// ([`Group::wrapped`]); [`Group::after`] adds what belongs with them but
/// is not one of them. It is a `div` to style as the container it replaces
/// was, or a card ([`Group::card`]). It keeps its members' focus by their
/// keys, under its id: the id is unique among the ids around it, and a key
/// is a member's own id, never its place, so the keyboard stays on a face
/// that moves.
#[derive(IntoElement)]
pub struct Group {
    id: ElementId,
    base: Div,
    layout: Layout,
    seats: Vec<Seat>,
    selected: Option<usize>,
    after: Vec<AnyElement>,
    rules: Option<Hsla>,
}

impl Group {
    pub fn new(id: impl Into<ElementId>, layout: Layout) -> Group {
        Group {
            id: id.into(),
            base: div(),
            layout,
            seats: Vec::new(),
            selected: None,
            after: Vec::new(),
            rules: None,
        }
    }

    /// One more member, after the ones before it.
    pub fn member(mut self, member: impl Member + 'static) -> Group {
        // A row with nothing to press is never reached; its key only has to
        // be one no member has.
        let key = member
            .key()
            .unwrap_or_else(|| ElementId::named_usize("unpressed", self.seats.len()));
        let enabled = member.reachable();
        self.seats.push(Seat {
            key,
            enabled,
            draw: Box::new(move |roving| member.roving(roving).into_any_element()),
        });
        self
    }

    /// Members, in the order they are drawn.
    pub fn members<M: Member + 'static>(self, members: impl IntoIterator<Item = M>) -> Group {
        members
            .into_iter()
            .fold(self, |group, member| group.member(member))
    }

    /// One more member drawn inside more than itself, as a face's plate is
    /// over its name: `draw` is given the member's keyboard and hands it to
    /// the part that is pressed ([`Press::roving`]). `key` is that part's
    /// id, and `enabled` whether it has a press.
    pub fn wrapped(
        mut self,
        key: impl Into<ElementId>,
        enabled: bool,
        draw: impl FnOnce(&Roving) -> AnyElement + 'static,
    ) -> Group {
        self.seats.push(Seat {
            key: key.into(),
            enabled,
            draw: Box::new(draw),
        });
        self
    }

    /// The member that is chosen, by its place: Tab enters the group on it.
    /// With `None`, or a place no member has, Tab enters on the first.
    pub fn selected(mut self, at: Option<usize>) -> Group {
        self.selected = at;
        self
    }

    /// Something drawn after the members that is not one of them (the
    /// switch under a card's choices): the arrows pass it by, and it keeps
    /// its own stop.
    pub fn after(mut self, el: impl IntoElement) -> Group {
        self.after.push(el.into_any_element());
        self
    }

    /// Draw it as a [`card`]: a surface with a rule around it and one
    /// between each two of its children, the last left as it is, so a row
    /// that [`grow`](Row::grow)s still takes a stretched card's spare
    /// height.
    pub fn card(mut self, p: &Palette) -> Group {
        self.base = card_frame(self.base, p);
        self.rules = Some(p.line);
        self
    }
}

impl Styled for Group {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for Group {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Group {
            id,
            base,
            layout,
            seats,
            selected,
            after,
            rules,
        } = self;
        // A handle for each member, kept by its key; a key no longer drawn
        // lets its handle go.
        let rover = window.use_keyed_state(id.clone(), cx, |_, _| Rover(Vec::new()));
        let handles: Vec<FocusHandle> = rover.update(cx, |rover, cx| {
            let kept = std::mem::take(&mut rover.0);
            rover.0 = seats
                .iter()
                .map(|seat| {
                    let handle = kept
                        .iter()
                        .find(|(key, _)| *key == seat.key)
                        .map_or_else(|| cx.focus_handle(), |(_, handle)| handle.clone());
                    (seat.key.clone(), handle)
                })
                .collect();
            rover.0.iter().map(|(_, handle)| handle.clone()).collect()
        });
        let enabled: Vec<bool> = seats.iter().map(|seat| seat.enabled).collect();
        let focused = handles.iter().position(|handle| handle.is_focused(window));
        let stop = group_stop(focused, selected, &enabled);
        let children: Vec<AnyElement> = seats
            .into_iter()
            .zip(&handles)
            .enumerate()
            .map(|(at, (seat, focus))| {
                (seat.draw)(&Roving {
                    focus: focus.clone(),
                    stop: stop == Some(at),
                })
            })
            .chain(after)
            .collect();
        // An arrow on a member moves the keyboard to its neighbour. A key
        // on anything else in the group (a button on a row), and a key with
        // a modifier, go on to whatever holds the group.
        let keys = move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
            if event.keystroke.modifiers.modified() {
                return;
            }
            let Some(at) = handles.iter().position(|handle| handle.is_focused(window)) else {
                return;
            };
            if let Some(next) = group_step(&event.keystroke.key, at, layout, &enabled) {
                cx.stop_propagation();
                window.focus(&handles[next]);
            }
        };
        let group = base.id(id).on_key_down(keys);
        match rules {
            Some(rule) => group.children(ruled(children, rule)),
            None => group.children(children),
        }
    }
}

/// Something that needs the owner, as a card of one row: the amber wash
/// and rim around it. The row brings its own mark (a [`bare`] warning
/// triangle in `warn`) and its button.
pub fn attention(row: Row, p: &Palette) -> Div {
    v_flex()
        .w_full()
        .rounded(px(12.))
        .bg(p.warn_wash)
        .border_1()
        .border_color(p.warn_line)
        .child(row)
}

// ---- tables ---------------------------------------------------------------

/// Between the cells of a [`table_row`].
pub const CELL_GAP: f32 = 12.;

/// A column of a table, `width` px that neither grows nor shrinks, so it
/// lines up with the same column of every other row and of the head. What
/// it holds starts at its left edge.
pub fn cell(width: f32) -> Div {
    div().flex_none().w(px(width))
}

/// A column of a table whose content stands at its right edge: a count, a
/// size, the row's control.
pub fn cell_end(width: f32) -> Div {
    h_flex().flex_none().w(px(width)).justify_end()
}

/// One row of a table on a [`card`] — the Integrations page's sources,
/// Footprint's items: its cells side by side, [`CELL_GAP`] apart inside the
/// row's 16 px sides, at least `height` high. Every cell but the one that
/// takes what is left (`flex_1().min_w_0()`) is a [`cell`] or a
/// [`cell_end`], so the columns line up row under row.
///
/// `ruled` is a row the card draws its rule under, over the row's last
/// pixel: every row but the card's last. That pixel is the rule's own, as a
/// bottom border is in the design's `border-box` rows, so a row its content
/// makes taller than `height` gets it back below the content, and one that
/// `height` holds keeps `height` with its rule in it.
pub fn table_row(height: f32, ruled: bool) -> Div {
    h_flex()
        .w_full()
        .gap(px(CELL_GAP))
        .min_h(px(height))
        .px(px(16.))
        .pt(px(6.))
        .pb(px(if ruled { 7. } else { 6. }))
}

// ---- the frame ----------------------------------------------------------

/// The width of a page's content: the main column's 828 less its 32 px
/// sides.
pub const PAGE_WIDTH: f32 = 764.;

/// The main column's padding: [`PAGE_SIDE`] left and right, [`PAGE_END`]
/// above and below.
pub const PAGE_SIDE: f32 = 32.;
pub const PAGE_END: f32 = 28.;

/// The app's tile and name, as the sidebar and first run's top bar show
/// them. The name is the product's and is never translated.
pub fn brand() -> Div {
    h_flex()
        .flex_none()
        .gap(px(9.))
        .child(
            gpui::img(BRAND_TILE)
                .flex_none()
                .size(px(22.))
                .rounded(px(6.)),
        )
        .child(div().ts(13., 18.).semibold().child("OpenWalkieTalkie"))
}

/// The width of a sidebar row's edge: the outline of the page that is up.
const NAV_EDGE: f32 = 1.5;

/// One page in the sidebar: its glyph and name, 32 high. The page that is
/// up is outlined and semibold; the others are in the sub-line colour. The
/// caller adds the press ([`Press::on_click`]) and anything after the name;
/// with a press it is a Tab stop.
pub fn nav_item(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    current: bool,
    p: &Palette,
) -> Press {
    let p = *p;
    let color = if current { p.text } else { p.text2 };
    Press::new(id)
        .own_border(NAV_EDGE)
        .h_flex()
        .flex_none()
        .h(px(32.))
        .px(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .border(px(NAV_EDGE))
        .ts(13., 18.)
        .text_color(color)
        .map(|item| {
            if current {
                item.semibold().border_color(p.outline)
            } else {
                item.medium()
                    .border_color(gpui::transparent_black())
                    .hover(|s| s.bg(p.well))
            }
        })
        .child(icon(icon_path, 16., color))
        .child(div().flex_1().min_w_0().child(label.into()))
}

/// A link out of the app: the words in the sub-line colour and a small
/// arrow. The caller adds the press that opens it ([`Press::on_click`]);
/// with one it is a Tab stop.
pub fn link_out(id: impl Into<ElementId>, text: impl Into<SharedString>, p: &Palette) -> Press {
    Press::new(id)
        .radius(4.)
        .h_flex()
        .flex_none()
        .gap(px(5.))
        .ts(13., 18.)
        .text_color(p.text2)
        .child(text.into())
        .child(icon(ic::EXTERNAL, 13., p.text3))
}

/// A page's head: an optional way back, the title (22/28 semibold), the
/// sentence under it, and the page's own controls on the right in a 28 px
/// band.
#[derive(IntoElement)]
pub struct Header {
    title: SharedString,
    sub: SharedString,
    right: Vec<AnyElement>,
    back: Option<(ElementId, SharedString, ClickHandler)>,
}

impl Header {
    /// A page's title and the sentence under it; an empty `sub` draws no
    /// sentence.
    pub fn new(title: impl Into<SharedString>, sub: impl Into<SharedString>) -> Header {
        Header {
            title: title.into(),
            sub: sub.into(),
            right: Vec::new(),
            back: None,
        }
    }

    /// One more control on the right, 12 px after the one before.
    pub fn right(mut self, el: impl IntoElement) -> Header {
        self.right.push(el.into_any_element());
        self
    }

    /// A sub-page's way back to its page, over the title: `label` is the
    /// page's name. It is a Tab stop, pressed by Return and Space too.
    pub fn back(
        mut self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Header {
        self.back = Some((id.into(), label.into(), Rc::new(f)));
        self
    }
}

impl RenderOnce for Header {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let back = self.back.map(|(id, label, f)| {
            // In a row of its own, so the press is as wide as the words.
            h_flex().child(
                Press::new(id)
                    .radius(4.)
                    .h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .ts(12., 16.)
                    .text_color(p.text2)
                    .child(icon(ic::CHEVRON_LEFT, 12., p.text2))
                    .child(label)
                    .on_click(move |event, window, cx| f(event, window, cx)),
            )
        });
        h_flex()
            .w_full()
            .items_start()
            .gap(px(16.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .children(back)
                    .child(
                        div()
                            .ts(22., 28.)
                            .semibold()
                            .text_color(p.text)
                            .child(self.title),
                    )
                    .when(!self.sub.is_empty(), |column| {
                        column.child(div().ts(13., 18.).text_color(p.text2).child(para(self.sub)))
                    }),
            )
            .when(!self.right.is_empty(), |header| {
                header.child(
                    h_flex()
                        .flex_none()
                        .h(px(28.))
                        .gap(px(12.))
                        .children(self.right),
                )
            })
    }
}

/// The scrolling column a page sits in, against the sidebar: its content
/// [`PAGE_WIDTH`] wide at most, inside the main column's padding, 20 px
/// between its blocks, and left-aligned, so a wider window leaves the extra
/// room empty on the right. `room` is the width right of the sidebar;
/// `scroll` is the shell's, so a page opens at its top.
///
/// The column's width is **a number of pixels** and not `w_full` capped by
/// `max_w`. With the cap, taffy 0.9 lays some of the column's descendants
/// out against the uncapped width in a pass gpui keeps (seen 2026-10-04:
/// cards at the cap and the paragraphs beside them at the full window, or
/// the whole column overflowing its own centred box). A definite width has
/// one answer in every pass.
pub fn page(
    id: impl Into<ElementId>,
    scroll: &gpui::ScrollHandle,
    room: f32,
    body: impl IntoElement,
) -> Stateful<Div> {
    let width = room.clamp(0., PAGE_WIDTH + 2. * PAGE_SIDE);
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .child(
            v_flex()
                .w(px(width))
                .px(px(PAGE_SIDE))
                .py(px(PAGE_END))
                .gap(px(20.))
                .child(body),
        )
}

// ---- paragraphs -----------------------------------------------------------

/// A paragraph of wrapping text whose painted lines always match the width
/// it was laid out at.
///
/// gpui's own text keeps the lines from whichever measurement ran **last**,
/// while taffy may lay the text out with a size it cached from an earlier
/// one; when the two widths differ the paragraph paints unwrapped over its
/// neighbours, or wrapped into a box sized for one line. This element shapes
/// again at prepaint whenever the final width is not the one its stored
/// lines were shaped for.
pub struct Para {
    text: SharedString,
}

/// See [`Para`].
pub fn para(text: impl Into<SharedString>) -> Para {
    Para { text: text.into() }
}

#[doc(hidden)]
pub struct ParaLayout {
    lines: Vec<gpui::WrappedLine>,
    wrap: Option<gpui::Pixels>,
    font_size: gpui::Pixels,
    line_height: gpui::Pixels,
    runs: Vec<gpui::TextRun>,
}

impl Element for Para {
    type RequestLayoutState = Rc<std::cell::RefCell<Option<ParaLayout>>>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let runs = vec![style.to_run(self.text.len())];
        let state: Self::RequestLayoutState = Rc::new(std::cell::RefCell::new(None));
        let stored = state.clone();
        let text = self.text.clone();
        let layout_id = window.request_measured_layout(
            gpui::Style::default(),
            move |known, available, window, _cx| {
                let wrap = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let lines = window
                    .text_system()
                    .shape_text(text.clone(), font_size, &runs, wrap, None)
                    .map(|lines| lines.into_vec())
                    .unwrap_or_default();
                let mut size = gpui::Size::<gpui::Pixels>::default();
                for line in &lines {
                    let line_size = line.size(line_height);
                    size.height += line_size.height;
                    size.width = size.width.max(line_size.width).ceil();
                }
                *stored.borrow_mut() = Some(ParaLayout {
                    lines,
                    wrap,
                    font_size,
                    line_height,
                    runs: runs.clone(),
                });
                size
            },
        );
        (layout_id, state)
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let mut stored = state.borrow_mut();
        let Some(layout) = stored.as_mut() else {
            return;
        };
        if layout.wrap != Some(bounds.size.width) {
            layout.lines = window
                .text_system()
                .shape_text(
                    self.text.clone(),
                    layout.font_size,
                    &layout.runs,
                    Some(bounds.size.width),
                    None,
                )
                .map(|lines| lines.into_vec())
                .unwrap_or_default();
            layout.wrap = Some(bounds.size.width);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        state: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let stored = state.borrow();
        let Some(layout) = stored.as_ref() else {
            return;
        };
        let mut origin = bounds.origin;
        for line in &layout.lines {
            line.paint(
                origin,
                layout.line_height,
                gpui::TextAlign::Left,
                Some(bounds),
                window,
                cx,
            )
            .ok();
            origin.y += line.size(layout.line_height).height;
        }
    }
}

impl IntoElement for Para {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_is_written_in_the_macs_order_whatever_order_it_was_given_in() {
        assert_eq!(
            Mod::ALL,
            [Mod::Control, Mod::Option, Mod::Shift, Mod::Command]
        );
        assert_eq!(
            canonical(&[Mod::Command, Mod::Shift]),
            [Mod::Shift, Mod::Command]
        );
        assert_eq!(
            canonical(&[Mod::Command, Mod::Option, Mod::Control, Mod::Shift]),
            Mod::ALL
        );
        // Each once, and none is none.
        assert_eq!(
            canonical(&[Mod::Option, Mod::Option, Mod::Control]),
            [Mod::Control, Mod::Option]
        );
        assert!(canonical(&[]).is_empty());
        // The derived order is the Mac's too, so a sorted set agrees.
        let mut sorted = vec![Mod::Command, Mod::Control, Mod::Shift, Mod::Option];
        sorted.sort();
        assert_eq!(sorted, Mod::ALL);
    }

    #[test]
    fn every_modifier_has_its_own_mark_and_the_name_macos_prints() {
        let names: Vec<&str> = Mod::ALL.iter().map(|m| m.name()).collect();
        assert_eq!(names, ["Control", "Option", "Shift", "Command"]);
        for (i, a) in Mod::ALL.iter().enumerate() {
            assert!(a.icon().starts_with("icons/mod-"), "{a:?}");
            for b in &Mod::ALL[i + 1..] {
                assert_ne!(a.icon(), b.icon(), "{a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn tabular_figures_and_the_weights_are_set_on_the_text_style() {
        let mut figures = div().tnum();
        let features = figures
            .style()
            .text
            .as_ref()
            .and_then(|text| text.font_features.clone())
            .expect("tnum sets the features");
        assert_eq!(features.tag_value_list(), [("tnum".to_string(), 1)]);
        // Nothing else of the inherited text style is touched: the family
        // and the CJK fallbacks come from the window's root.
        let text = figures.style().text.clone().unwrap_or_default();
        assert_eq!(text.font_family, None);
        assert_eq!(text.font_fallbacks, None);
        for (mut el, weight) in [
            (div().medium(), FontWeight::MEDIUM),
            (div().semibold(), FontWeight::SEMIBOLD),
        ] {
            let text = el.style().text.clone().unwrap_or_default();
            assert_eq!(text.font_weight, Some(weight));
        }
        let mut code = div().mono().ts(12., 16.);
        let text = code.style().text.clone().unwrap_or_default();
        assert_eq!(text.font_family, Some(MONO.into()));
        assert_eq!(text.font_size, Some(px(12.).into()));
    }

    #[test]
    fn the_kits_rows_are_the_designs_two_heights() {
        assert_eq!((ROW_ONE_LINE, ROW_TWO_LINES), (44., 56.));
        // The content column is the main column's width less its sides.
        assert_eq!(PAGE_WIDTH + 2. * PAGE_SIDE, 828.);
        assert_eq!(PAGE_END, 28.);
        assert_eq!(Row::new().pad, 16., "a row's sides, unless said");
    }

    #[test]
    fn a_table_row_holds_its_height_with_its_rule_and_gives_a_taller_one_the_rule_back() {
        // The Integrations board's rows: 64 at least, a pixel more for the
        // rule under every row but the card's last.
        let mut ruled = table_row(64., true);
        let style = ruled.style();
        assert_eq!(style.min_size.height, Some(px(64.).into()));
        assert_eq!(style.padding.top, Some(px(6.).into()));
        assert_eq!(style.padding.bottom, Some(px(7.).into()));
        assert_eq!(style.padding.left, Some(px(16.).into()));
        assert_eq!(style.gap.width, Some(px(CELL_GAP).into()));
        let mut last = table_row(64., false);
        assert_eq!(last.style().padding.bottom, Some(px(6.).into()));
        // A column is its width, never more or less; one at its end puts
        // what it holds there.
        for mut column in [cell(88.), cell_end(104.)] {
            let style = column.style();
            assert_eq!((style.flex_grow, style.flex_shrink), (Some(0.), Some(0.)));
        }
        assert_eq!(cell(88.).style().size.width, Some(px(88.).into()));
        assert_eq!(
            cell_end(104.).style().justify_content,
            Some(gpui::JustifyContent::End)
        );
        // Cards side by side are stretched on purpose.
        assert_eq!(
            div().flex().items_stretch().style().align_items,
            Some(AlignItems::Stretch)
        );
    }

    #[test]
    fn a_copy_button_says_it_copied_for_two_seconds_and_then_its_label_again() {
        assert_eq!(COPIED_FOR, Duration::from_secs(2));
        assert!(!says_copied(None), "not pressed yet");
        assert!(says_copied(Some(Instant::now())));
        if let Some(earlier) = Instant::now().checked_sub(COPIED_FOR) {
            assert!(!says_copied(Some(earlier)));
        }
    }

    #[test]
    fn a_level_takes_the_stop_under_the_pointer_and_its_ends_past_either_end() {
        // The design's speaker level: 168 wide, 0 to 10, the stops' centres
        // from 8 to 160.
        let at = |x: f32| level_value_at(x, LEVEL_WIDTH, 0, 10);
        assert_eq!((at(8.), at(160.)), (0, 10), "the two ends");
        assert_eq!(at(84.), 5, "the middle");
        assert_eq!(at(8. + 15.2 * 6.), 6, "a stop's centre");
        // Half a stop either way rounds to the nearer one.
        assert_eq!((at(8. + 15.2 * 5.6), at(8. + 15.2 * 6.4)), (6, 6));
        // Outside the rail, and outside the control: the nearer end.
        assert_eq!((at(0.), at(-40.)), (0, 0));
        assert_eq!((at(168.), at(400.)), (10, 10));
        // Brightness runs from 1 to 5.
        assert_eq!(level_value_at(8., LEVEL_WIDTH, 1, 5), 1);
        assert_eq!(level_value_at(84., LEVEL_WIDTH, 1, 5), 3);
        assert_eq!(level_value_at(160., LEVEL_WIDTH, 1, 5), 5);
        // A level with one stop, or none, is that stop wherever it is
        // pressed, and a level too narrow for its thumb does not divide by
        // nothing.
        assert_eq!(level_value_at(50., LEVEL_WIDTH, 4, 4), 4);
        assert_eq!(level_value_at(50., LEVEL_WIDTH, 4, 2), 4);
        assert_eq!(level_value_at(9., 10., 0, 10), 10);
        assert_eq!(level_value_at(7., 10., 0, 10), 0);
    }

    #[test]
    fn the_arrow_keys_home_and_end_move_a_level_a_stop_at_a_time_and_stop_at_its_ends() {
        assert_eq!(level_key("left", 6, 0, 10), Some(5));
        assert_eq!(level_key("down", 6, 0, 10), Some(5));
        assert_eq!(level_key("right", 6, 0, 10), Some(7));
        assert_eq!(level_key("up", 6, 0, 10), Some(7));
        assert_eq!(level_key("home", 6, 0, 10), Some(0));
        assert_eq!(level_key("end", 6, 0, 10), Some(10));
        // Never past an end.
        assert_eq!(level_key("left", 1, 1, 5), Some(1));
        assert_eq!(level_key("right", 5, 1, 5), Some(5));
        assert_eq!(level_key("down", 0, 0, 10), Some(0));
        // A value outside the range moves from the end it is past.
        assert_eq!(level_key("right", 9, 1, 5), Some(5));
        assert_eq!(level_key("left", 0, 1, 5), Some(1));
        // Every other key is left to whatever else wants it.
        for key in ["enter", "space", "tab", "a", "escape", ""] {
            assert_eq!(level_key(key, 6, 0, 10), None, "{key:?}");
        }
        // The thumb's place follows the same range.
        assert_eq!(level_fraction(6, 0, 10), 0.6);
        assert_eq!((level_fraction(1, 1, 5), level_fraction(5, 1, 5)), (0., 1.));
        assert_eq!(level_fraction(3, 3, 3), 0.);
    }

    #[test]
    fn idle_home_shows_the_step_the_deck_rounds_a_value_up_to() {
        // The deck keeps 0, 30, 60 or 120 seconds and rounds anything else
        // up to the next (`src/deck/model.rs`): Never, 30 s, 1 min, 2 min.
        let steps = [0, 30, 60, 120];
        for (value, step) in [
            (0, 0),
            (1, 1),
            (30, 1),
            (31, 2),
            (45, 2),
            (60, 2),
            (61, 3),
            (120, 3),
            (900, 3),
            (u16::MAX, 3),
        ] {
            assert_eq!(step_at(value, &steps), step, "{value} s");
        }
        // Each step shows as itself, so writing a choice and reading it
        // back agree.
        for (i, &step) in steps.iter().enumerate() {
            assert_eq!(step_at(step, &steps), i);
        }
        assert_eq!(step_at(5, &[]), 0);
    }

    #[test]
    fn the_four_buttons_are_the_designs_fills_words_and_rims() {
        let clear = gpui::transparent_black();
        for p in [Palette::dark(), Palette::light()] {
            assert_eq!(Kind::Primary.colors(&p), (p.solid, p.on_solid, p.solid));
            assert_eq!(Kind::Secondary.colors(&p), (clear, p.text, p.edge));
            assert_eq!(Kind::Quiet.colors(&p), (p.well, p.text, clear));
            assert_eq!(Kind::Danger.colors(&p), (clear, p.bad_text, p.edge));
            // A new button is the secondary one.
            assert_eq!(Kind::default(), Kind::Secondary);
            assert_eq!(Btn::new("b", "Check").kind, Kind::Secondary);
            assert_eq!(Btn::icon_only("c", ic::COPY).kind, Kind::Quiet);
            // On and off, the primary's plate or an edge around nothing.
            assert_eq!(switch_colors(true, &p), (p.solid, p.solid, p.on_solid));
            assert_eq!(switch_colors(false, &p), (clear, p.edge, p.edge));
            assert_eq!(chip_colors(true, &p), (p.solid, p.on_solid, p.solid));
            assert_eq!(chip_colors(false, &p), (clear, p.text2, p.edge));
            // The chosen segment is a surface with the selection's outline.
            assert_eq!(segment_colors(true, &p), (p.surf, p.text, p.outline));
            assert_eq!(segment_colors(false, &p), (clear, p.text2, clear));
        }
    }

    #[test]
    fn with_increase_contrast_a_quiet_button_is_outlined() {
        let clear = gpui::transparent_black();
        for dark in [true, false] {
            let more = Palette::new(dark, true);
            assert_eq!(Kind::Quiet.colors(&more), (more.well, more.text, more.edge));
            // The other three are drawn as they always are.
            assert_eq!(
                Kind::Primary.colors(&more),
                (more.solid, more.on_solid, more.solid)
            );
            assert_eq!(Kind::Secondary.colors(&more), (clear, more.text, more.edge));
            assert_eq!(
                Kind::Danger.colors(&more),
                (clear, more.bad_text, more.edge)
            );
        }
    }

    #[test]
    fn with_increase_contrast_a_pill_gains_its_words_colour_as_an_edge() {
        for dark in [true, false] {
            let (standard, more) = (Palette::new(dark, false), Palette::new(dark, true));
            for tone in [Tone::Red, Tone::Amber, Tone::Blue] {
                let mut pill = status("Failed", tone, &more);
                let style = pill.style();
                assert_eq!(style.border_widths.left, Some(px(1.).into()), "{tone:?}");
                assert_eq!(style.border_color, Some(more.tone_text(tone)), "{tone:?}");
                // The edge takes a pixel of each side: as wide and as high
                // as the pill without it.
                assert_eq!(style.padding.left, Some(px(8.).into()), "{tone:?}");
                assert_eq!(style.size.height, Some(px(22.).into()), "{tone:?}");
                let mut plain = status("Failed", tone, &standard);
                let style = plain.style();
                assert_eq!(style.border_widths.left, None, "{tone:?}");
                assert_eq!(style.border_color, None, "{tone:?}");
                assert_eq!(style.padding.left, Some(px(9.).into()), "{tone:?}");
            }
            // A healthy or neutral state is a dot and a word, with no pill
            // and no edge, either way.
            for p in [&standard, &more] {
                for tone in [Tone::Green, Tone::Gray] {
                    let mut word = status("Linked", tone, p);
                    let style = word.style();
                    assert_eq!(style.border_widths.left, None, "{tone:?}");
                    assert_eq!(style.border_color, None, "{tone:?}");
                    assert_eq!(style.size.height, None, "{tone:?}");
                }
            }
        }
    }

    #[test]
    fn with_increase_contrast_a_segmented_track_is_rimmed_and_keeps_its_size() {
        for dark in [true, false] {
            let more = Palette::new(dark, true);
            let mut track = segment_track(div(), &more);
            let style = track.style();
            assert_eq!(style.border_widths.left, Some(px(1.).into()));
            assert_eq!(style.border_color, Some(more.edge));
            // The rim takes a pixel of the padding: the options stay 2 px
            // in, and the track 26 high around them.
            assert_eq!(style.padding.left, Some(px(1.).into()));
            assert_eq!(style.padding.top, Some(px(1.).into()));
            let mut plain = segment_track(div(), &Palette::new(dark, false));
            let style = plain.style();
            assert_eq!(style.border_widths.left, None);
            assert_eq!(style.border_color, None);
            assert_eq!(style.padding.left, Some(px(2.).into()));
            assert_eq!(style.padding.top, Some(px(2.).into()));
        }
    }

    #[test]
    fn the_arrow_keys_move_along_a_row_a_column_or_a_grid_and_stop_at_the_ends() {
        let all = |count: usize| vec![true; count];
        // A row of five (a segmented control, the face strip): Left and
        // Right, Home and End, and never round an end.
        let row = |key: &str, at: usize| group_step(key, at, Layout::Row, &all(5));
        assert_eq!(row("right", 0), Some(1));
        assert_eq!(row("left", 3), Some(2));
        assert_eq!(row("left", 0), None);
        assert_eq!(row("right", 4), None);
        assert_eq!(row("end", 1), Some(4));
        assert_eq!(row("home", 3), Some(0));
        assert_eq!(row("home", 0), None, "already at the start");
        assert_eq!(row("up", 2), None, "a row has no up");
        assert_eq!(row("down", 2), None);
        // A column of five (the sidebar): Up and Down.
        let column = |key: &str, at: usize| group_step(key, at, Layout::Column, &all(5));
        assert_eq!(column("down", 0), Some(1));
        assert_eq!(column("up", 4), Some(3));
        assert_eq!(column("up", 0), None);
        assert_eq!(column("end", 0), Some(4));
        assert_eq!(column("left", 2), None, "a column has no left");
        assert_eq!(column("right", 2), None);
        // The key grid: nine keys, three to a row. Left and Right stay in
        // their row, Up and Down in their column.
        let grid = Layout::Grid { columns: 3 };
        let nine = |key: &str, at: usize| group_step(key, at, grid, &all(9));
        assert_eq!(nine("right", 0), Some(1));
        assert_eq!(nine("right", 2), None, "off the end of a row");
        assert_eq!(nine("left", 3), None, "off the start of a row");
        assert_eq!(nine("left", 5), Some(4));
        assert_eq!(nine("down", 4), Some(7));
        assert_eq!(nine("down", 7), None);
        assert_eq!(nine("up", 7), Some(4));
        assert_eq!(nine("up", 1), None);
        assert_eq!(nine("home", 8), Some(0));
        assert_eq!(nine("end", 0), Some(8));
        // A grid whose last row is short.
        let eight = |key: &str, at: usize| group_step(key, at, grid, &all(8));
        assert_eq!(eight("down", 5), None, "no key under it");
        assert_eq!(eight("down", 4), Some(7));
        assert_eq!(eight("right", 7), None);
        assert_eq!(eight("end", 0), Some(7));
        // A grid said to have no columns has one to a row.
        let none = Layout::Grid { columns: 0 };
        assert_eq!(group_step("down", 0, none, &all(3)), Some(1));
        assert_eq!(group_step("right", 0, none, &all(3)), None);
        // Every other key is left to whatever holds the group, and so is a
        // place no member has.
        for key in ["enter", "space", "tab", "escape", "pageup", "a", ""] {
            for layout in [Layout::Row, Layout::Column, grid] {
                assert_eq!(group_step(key, 1, layout, &all(9)), None, "{key:?}");
            }
        }
        assert_eq!(group_step("left", 3, Layout::Row, &all(3)), None);
        assert_eq!(group_step("home", 0, Layout::Row, &[]), None);
    }

    #[test]
    fn members_that_cannot_be_pressed_are_passed_by_and_an_end_is_never_crossed() {
        assert_eq!(
            group_step("right", 0, Layout::Row, &[true, false, true]),
            Some(2)
        );
        assert_eq!(
            group_step("right", 0, Layout::Row, &[true, false, false]),
            None
        );
        assert_eq!(
            group_step("left", 2, Layout::Row, &[false, false, true]),
            None
        );
        assert_eq!(
            group_step("home", 2, Layout::Row, &[false, true, true]),
            Some(1)
        );
        assert_eq!(
            group_step("end", 0, Layout::Column, &[true, true, false]),
            Some(1)
        );
        // In a grid: down a column past a key that cannot be pressed, and
        // across a row past one.
        let grid = Layout::Grid { columns: 3 };
        let mut keys = [true; 9];
        keys[4] = false;
        assert_eq!(group_step("down", 1, grid, &keys), Some(7));
        assert_eq!(group_step("right", 3, grid, &keys), Some(5));
        assert_eq!(group_step("up", 7, grid, &keys), Some(1));
        // A member disabled while it had the keyboard: the keys still move
        // on from it.
        assert_eq!(
            group_step("right", 1, Layout::Row, &[true, false, true]),
            Some(2)
        );
    }

    #[test]
    fn a_groups_tab_stop_is_the_keyboard_then_the_selection_then_the_first_member() {
        let all = [true; 3];
        assert_eq!(
            group_stop(Some(2), Some(0), &all),
            Some(2),
            "it has the keyboard"
        );
        assert_eq!(group_stop(None, Some(1), &all), Some(1), "the chosen one");
        assert_eq!(group_stop(None, None, &all), Some(0));
        // The firmware board picker's "not known" is a place no member has.
        assert_eq!(group_stop(None, Some(usize::MAX), &all), Some(0));
        // A member that cannot be pressed is never the stop.
        let middle_off = [true, false, true];
        assert_eq!(group_stop(Some(1), Some(2), &middle_off), Some(2));
        assert_eq!(group_stop(None, Some(1), &middle_off), Some(0));
        assert_eq!(group_stop(None, None, &[false, false, true]), Some(2));
        // Nothing to press: no stop, and Tab passes the group by.
        assert_eq!(group_stop(Some(0), Some(0), &[false, false]), None);
        assert_eq!(group_stop(None, None, &[]), None);
    }

    #[test]
    fn return_or_space_opens_a_menu_but_not_held_down_or_with_a_modifier() {
        let plain = Modifiers::default();
        assert!(opens_menu("enter", &plain, false));
        assert!(opens_menu("space", &plain, false));
        // The Return that chose an item, still down on the button.
        assert!(!opens_menu("enter", &plain, true));
        assert!(!opens_menu("space", &plain, true));
        for modified in [
            Modifiers::shift(),
            Modifiers::command(),
            Modifiers::alt(),
            Modifiers::control(),
        ] {
            assert!(!opens_menu("enter", &modified, false), "{modified:?}");
            assert!(!opens_menu("space", &modified, false), "{modified:?}");
        }
        for key in ["tab", "down", "escape", "a", ""] {
            assert!(!opens_menu(key, &plain, false), "{key:?}");
        }
    }

    #[test]
    fn a_group_is_drawn_exactly_as_the_container_it_replaces() {
        for p in [
            Palette::dark(),
            Palette::light(),
            Palette::new(true, true),
            Palette::new(false, true),
        ] {
            // A card of choices has a card's own frame, and its rules.
            let mut choices = Group::new("choices", Layout::Column).card(&p);
            assert_eq!(*choices.style(), *card(Vec::new(), &p).style());
            assert_eq!(choices.rules, Some(p.line));
            // A segmented control's options sit on the track it had.
            let mut options = segment_track(Group::new("options", Layout::Row), &p);
            assert_eq!(*options.style(), *segment_track(div(), &p).style());
            assert_eq!(options.rules, None);
        }
        // Styled like any div, and nothing else unless asked.
        let mut column = Group::new("nav", Layout::Column).v_flex().gap(px(2.));
        assert_eq!(*column.style(), *v_flex().gap(px(2.)).style());
        let group = Group::new("nav", Layout::Column);
        assert_eq!((group.selected, group.after.len()), (None, 0));
    }

    #[test]
    fn a_member_is_known_by_its_own_id_and_reached_only_while_it_has_a_press() {
        let nothing = |_: &ClickEvent, _: &mut Window, _: &mut App| {};
        let face = Press::new(("face", 3usize)).on_click(nothing);
        assert_eq!(face.key(), Some(ElementId::from(("face", 3usize))));
        assert!(face.reachable());
        assert!(!Press::new("plate").reachable(), "a picture");
        assert!(!Press::new("key")
            .on_click(nothing)
            .disabled(true)
            .reachable());
        let choice = Row::new().on_click("voice-stt-apple", nothing);
        assert_eq!(choice.key(), Some("voice-stt-apple".into()));
        assert!(choice.reachable());
        assert_eq!(Row::new().key(), None);
        assert!(!Row::new().reachable(), "a row with nothing to press");
        // The group takes them in order, each by its own id; a row with
        // nothing to press has a key no member can have, and is passed by.
        let group = Group::new("choices", Layout::Column)
            .member(Row::new().on_click("apple", nothing))
            .member(Row::new())
            .members([Press::new("later").on_click(nothing).disabled(true)])
            .wrapped(("face", 1usize), true, |_| div().into_any_element())
            .after(Row::new());
        let seats: Vec<(ElementId, bool)> = group
            .seats
            .iter()
            .map(|seat| (seat.key.clone(), seat.enabled))
            .collect();
        assert_eq!(
            seats,
            [
                ("apple".into(), true),
                (ElementId::named_usize("unpressed", 1), false),
                ("later".into(), false),
                (("face", 1usize).into(), true),
            ]
        );
        // What comes after the members is not one of them.
        assert_eq!(group.after.len(), 1);
    }

    #[test]
    fn a_ring_sits_its_gap_outside_the_element_and_follows_its_corners() {
        let color = Palette::dark().info;
        // A button's: 8 px corners, a 1 px border, the ring 2 px wide and
        // 2 px out, so from 2 to 4 px outside the button.
        let mut button = ring(2., 2., 8., 1., color);
        let style = button.style();
        for inset in [
            &style.inset.top,
            &style.inset.right,
            &style.inset.bottom,
            &style.inset.left,
        ] {
            assert_eq!(*inset, Some(px(-5.).into()));
        }
        assert_eq!(style.corner_radii.top_left, Some(px(12.).into()));
        assert_eq!(style.border_widths.top, Some(px(2.).into()));
        assert_eq!(style.border_color, Some(color));
        assert_eq!(style.position, Some(gpui::Position::Absolute));
        // A tile's resting ring touches it.
        let mut rest = ring(0., 1., 12., 0., color);
        assert_eq!(rest.style().inset.left, Some(px(-1.).into()));
        assert_eq!(rest.style().corner_radii.top_left, Some(px(13.).into()));
        // Focus on a selected tile sits outside the selection's ring.
        let mut both = ring(FOCUS_RING_SELECTED, FOCUS_RING, 12., 0., color);
        assert_eq!(both.style().inset.top, Some(px(-8.).into()));
        // A row's is inside it, clear of its card's corners.
        let mut row = inset_ring(ROW_RING_INSET, ROW_RING_RADIUS, color);
        assert_eq!(row.style().inset.top, Some(px(2.).into()));
        assert_eq!(row.style().border_widths.left, Some(px(FOCUS_RING).into()));
        assert_eq!(row.style().corner_radii.top_left, Some(px(8.).into()));
    }
}
