//! The window's components, in the look [`crate::style`] describes: buttons,
//! rows and cards, badges, strips, a switch, chips, the code block and the
//! page frame every admin screen sits in.
//!
//! They are built from `div`s and not from gpui-component's widgets for two
//! reasons. gpui-component's `Button` turns its label red on hover and fixes
//! its own paddings, and its `Sheet`/`Dialog` paint the theme background,
//! which here is translucent glass. Everything below reads [`Palette`] and
//! nothing else, so a screen that uses them cannot pick up a stock colour.
//!
//! Sizes are Zeron's: 13/17 Medium titles over 12/16 muted meta lines,
//! 32 px controls (26 px small), 16 px icons muted at rest, 8 px radii on
//! controls and 12 px on cards, rows at least 60 px tall with their
//! dividers inset to the text edge.
use crate::style::{palette, Palette, MONO};
use deck_model::{Kind, Tone};
use gpui::{
    div, percentage, prelude::*, px, relative, svg, Animation, AnimationExt as _, AnyElement,
    AnyView, App, ClickEvent, Div, ElementId, FontWeight, Hsla, SharedString, Stateful, Svg,
    Transformation, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use std::rc::Rc;
use std::time::Duration;

/// The website's pixel radio, shared by every product identity surface.
pub fn brand_mark(size: f32) -> Div {
    div().size(px(size)).flex_none().child(
        gpui::img("branding/radio.png").size_full().object_fit(gpui::ObjectFit::Contain),
    )
}

pub fn project_credit(p: &Palette) -> Div {
    h_flex().gap(px(6.)).text_color(p.muted).ts(11., 16.)
        .child(icon("icons/cohdl-mark.svg", 17., p.muted))
        .child("A COHDL project")
}

pub fn brand_lockup(p: &Palette) -> Div {
    h_flex().gap(px(12.)).child(brand_mark(44.)).child(
        v_flex().gap(px(4.))
            .child(div().ts(17., 22.).medium().child("OpenWalkieTalkie"))
            .child(project_credit(p)),
    )
}

/// The icons the window draws, by asset path (see [`crate::assets`]).
pub mod ic {
    pub const ADD: &str = "icons/add-circle.svg";
    pub const ARCHIVE: &str = "icons/archive-minimalistic.svg";
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
    pub const MONITOR: &str = "icons/monitor.svg";
    pub const OPENAI: &str = "icons/openai-mark.svg";
    pub const PIN: &str = "icons/pin.svg";
    pub const PLAY: &str = "icons/action-play.svg";
    pub const PLUG: &str = "icons/plug.svg";
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
    pub const WIFI_OFF: &str = "icons/wifi-off.svg";
}

/// An icon at `size` px in `color`. An svg does not inherit the text colour
/// around it, so the colour is always said.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// Text sizes as size/line-height pairs in pixels, and the two weights the
/// window uses.
pub trait Type: Styled + Sized {
    fn ts(self, size: f32, line: f32) -> Self {
        self.text_size(px(size)).line_height(px(line))
    }

    /// The emphasis weight. Bold is for nothing.
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }

    fn mono(self) -> Self {
        self.font_family(MONO)
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

// ---- buttons ----------------------------------------------------------

/// What a [`Btn`] looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// The neutral primary: a text-colour plate. One per view, at most.
    Solid,
    /// Text only until the pointer is on it.
    Quiet,
    /// An opaque plate with a hairline.
    Outlined,
    /// A 6 % wash.
    Filled,
    /// The destructive confirm.
    Danger,
    /// A destructive action that is not the confirm: red text, no plate.
    DangerQuiet,
    /// The accent wash, for the one thing on a row that is new or live.
    Accent,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What a [`Switch`] calls with the value it is being moved to.
type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// A button. `Btn::new(id, label)` is a filled one; the look methods change
/// that, `icon` leads the label with a glyph, and [`Btn::icon_only`] is the
/// square icon button.
#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    look: Look,
    small: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Btn {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Btn {
        Btn {
            id: id.into(),
            label: Some(label.into()),
            icon: None,
            look: Look::Filled,
            small: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// A square button with a glyph and no label; say what it does with
    /// [`Btn::tooltip`].
    pub fn icon_only(id: impl Into<ElementId>, icon: &'static str) -> Btn {
        Btn {
            label: None,
            icon: Some(icon),
            look: Look::Quiet,
            ..Btn::new(id, "")
        }
    }

    pub fn icon(mut self, icon: &'static str) -> Btn {
        self.icon = Some(icon);
        self
    }

    pub fn look(mut self, look: Look) -> Btn {
        self.look = look;
        self
    }

    pub fn solid(self) -> Btn {
        self.look(Look::Solid)
    }

    pub fn quiet(self) -> Btn {
        self.look(Look::Quiet)
    }

    pub fn outlined(self) -> Btn {
        self.look(Look::Outlined)
    }

    pub fn danger(self) -> Btn {
        self.look(Look::Danger)
    }

    pub fn danger_quiet(self) -> Btn {
        self.look(Look::DangerQuiet)
    }

    pub fn accent(self) -> Btn {
        self.look(Look::Accent)
    }

    pub fn small(mut self) -> Btn {
        self.small = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Btn {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Btn {
        self.tooltip = Some(text.into());
        self
    }

    /// What a click does. The click stops here: a button on a clickable
    /// row does not also select the row.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Btn {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let clear = gpui::transparent_black();
        let (h, pad, radius, size, glyph) = if self.small {
            (26., 9., 7., 12., 14.)
        } else {
            (32., 12., 8., 12.5, 16.)
        };
        // Rest fill, rest text, hover fill, hover text, hairline.
        let (bg, fg, hover_bg, hover_fg, line) = match self.look {
            Look::Solid => (p.solid, p.on_solid, p.solid.opacity(0.9), p.on_solid, None),
            Look::Quiet => (clear, p.muted, p.hover, p.text, None),
            Look::Outlined => (p.plate, p.text, p.plate, p.text, Some(p.border)),
            Look::Filled => (p.wash(0.06), p.text, p.wash(0.10), p.text, None),
            Look::Danger => (
                p.danger,
                gpui::white(),
                p.danger.opacity(0.9),
                gpui::white(),
                None,
            ),
            Look::DangerQuiet => (clear, p.danger_muted, p.danger.alpha(0.10), p.danger, None),
            Look::Accent => (
                p.accent_wash(),
                p.accent,
                p.accent.alpha(if p.dark { 0.30 } else { 0.18 }),
                p.accent,
                None,
            ),
        };
        let strong = matches!(self.look, Look::Solid | Look::Danger | Look::Accent);
        let icon_only = self.label.is_none();
        let enabled = !self.disabled && self.on_click.is_some();
        div()
            .id(self.id)
            .group("btn")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(h))
            .rounded(px(radius))
            .ts(size, 16.)
            .bg(bg)
            .text_color(fg)
            .when(strong, |this| this.medium())
            .map(|this| {
                if icon_only {
                    this.w(px(h))
                } else {
                    this.px(px(pad))
                }
            })
            .when_some(line, |this, line| {
                this.border_1().border_color(line).when(enabled, |this| {
                    this.hover(|s| s.border_color(p.border_strong))
                })
            })
            .when(self.disabled, |this| this.opacity(0.4))
            .when(enabled, |this| {
                this.cursor_pointer()
                    .when(line.is_none(), |this| {
                        this.hover(|s| s.bg(hover_bg).text_color(hover_fg))
                    })
                    .active(|s| s.opacity(0.85))
            })
            .when_some(self.icon, |this, path| {
                this.child(icon(path, glyph, fg).group_hover("btn", |s| {
                    if enabled {
                        s.text_color(hover_fg)
                    } else {
                        s
                    }
                }))
            })
            .when_some(self.label.filter(|l| !l.is_empty()), |this, label| {
                this.child(label)
            })
            .when_some(self.tooltip, |this, text| this.tooltip(tip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, f| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    f(event, window, cx);
                })
            })
    }
}

// ---- switch -----------------------------------------------------------

/// An on/off switch: a 36×20 track, the accent when on, with a pill thumb.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    disabled: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Switch {
        Switch {
            id: id.into(),
            on,
            disabled: false,
            on_toggle: None,
        }
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
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let on = self.on;
        let enabled = !self.disabled && self.on_toggle.is_some();
        div()
            .id(self.id)
            .flex_none()
            .relative()
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .bg(if on { p.switch_on } else { p.switch_off })
            .when(self.disabled, |this| this.opacity(0.4))
            .when(enabled, |this| this.cursor_pointer())
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(if on { 14. } else { 2. }))
                    .w(px(20.))
                    .h(px(16.))
                    .rounded_full()
                    .bg(gpui::white().opacity(if p.dark { 0.94 } else { 1.0 }))
                    .border_1()
                    .border_color(gpui::black().opacity(0.08)),
            )
            .when_some(self.on_toggle.filter(|_| enabled), |this, f| {
                this.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    f(&!on, window, cx);
                })
            })
    }
}

// ---- small things -----------------------------------------------------

/// How a [`badge`] is tinted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Neutral,
    Success,
    Warning,
    Danger,
    Accent,
    Blue,
}

impl Tint {
    /// The firmware tone a badge stands for.
    pub fn of(tone: Tone) -> Tint {
        match tone {
            Tone::Green => Tint::Success,
            Tone::Amber => Tint::Warning,
            Tone::Red => Tint::Danger,
            Tone::Blue => Tint::Blue,
            Tone::Gray => Tint::Neutral,
        }
    }
}

/// A small filled pill: a status word, a slot, a scope.
pub fn badge(label: impl Into<SharedString>, tint: Tint, p: &Palette) -> Div {
    let (bg, fg) = match tint {
        Tint::Neutral => (p.wash(0.08), p.muted),
        Tint::Success => (p.success.alpha(0.12), p.success_muted),
        Tint::Warning => (p.warning.alpha(0.12), p.warning_muted),
        Tint::Danger => (p.danger.alpha(0.12), p.danger_muted),
        Tint::Accent => (p.accent_wash(), p.accent),
        Tint::Blue => (p.blue.alpha(0.14), p.blue),
    };
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded_full()
        .ts(11., 15.)
        .medium()
        .bg(bg)
        .text_color(fg)
        .whitespace_nowrap()
        .child(label.into())
}

/// A status dot.
pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// A keycap: the choice number on a card, a shortcut in a hint.
pub fn keycap(label: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(5.))
        .rounded(px(5.))
        .bg(p.ink(0.06))
        .ts(11., 14.)
        .mono()
        .text_color(p.muted)
        .child(label.into())
}

/// A 36×36 tile with a glyph in it, at the head of a row.
pub fn tile(path: &'static str, color: Hsla, p: &Palette) -> Div {
    div()
        .flex_none()
        .size(px(36.))
        .rounded(px(10.))
        .bg(p.wash(0.06))
        .flex()
        .items_center()
        .justify_center()
        .child(icon(path, 18., color))
}

/// Fragments joined by a faint "·", wrapping as a sentence would.
pub fn meta_line(parts: Vec<AnyElement>, p: &Palette) -> Div {
    let mut line = h_flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(6.))
        .ts(12., 16.)
        .text_color(p.muted);
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            line = line.child(div().text_color(p.muted.opacity(0.45)).child("\u{b7}"));
        }
        line = line.child(part);
    }
    line
}

/// A plain fragment for [`meta_line`].
pub fn frag(text: impl Into<SharedString>) -> AnyElement {
    div().child(text.into()).into_any_element()
}

/// A fragment in a colour of its own: a status word inside a meta line.
pub fn word(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// A muted line of explanation.
pub fn hint(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().ts(12., 17.).text_color(p.muted).child(para(text))
}

/// The glyph and colour for an agent kind (§8.2's kind icon). Claude keeps
/// its own orange; the others are drawn in the text colour or their own.
pub fn kind_mark(kind: Kind, p: &Palette) -> (&'static str, Hsla) {
    match kind {
        Kind::Claude => (ic::CLAUDE, p.claude),
        Kind::Codex => (ic::OPENAI, p.text),
        Kind::Gemini => (ic::GEMINI, p.gemini),
        Kind::Other => (ic::BOT, p.muted),
        Kind::Conol => (ic::CONOL, if p.dark { gpui::rgb(0xFFE238).into() } else { p.text }),
    }
}

/// The glyph and colour for an integration target.
pub fn target_mark(target: &str, p: &Palette) -> (&'static str, Hsla) {
    match target {
        "claude" | "claude-desktop" => (ic::CLAUDE, p.claude),
        "codex" => (ic::OPENAI, p.text),
        "gemini" => (ic::GEMINI, p.gemini),
        _ => (ic::PLUG, p.muted),
    }
}

/// A spinning refresh glyph, for "asking the service".
pub fn spinner(size: f32, color: Hsla) -> impl IntoElement {
    icon(ic::REFRESH, size, color).with_animation(
        "spinner",
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
}

/// A 4 px bar, filled to `fraction` with the accent.
pub fn progress(fraction: f32, p: &Palette) -> Div {
    div().w_full().h(px(4.)).rounded_full().bg(p.border).child(
        div()
            .h_full()
            .w(relative(fraction.clamp(0.04, 1.0)))
            .rounded_full()
            .bg(p.accent),
    )
}

// ---- chips --------------------------------------------------------------

/// One segment of a filter row (the log levels). The caller adds the click.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    p: &Palette,
) -> Stateful<Div> {
    let p = *p;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(26.))
        .px(px(9.))
        .rounded(px(7.))
        .ts(12., 16.)
        .cursor_pointer()
        .map(|this| {
            if selected {
                this.bg(p.wash(0.10)).text_color(p.text).medium()
            } else {
                this.text_color(p.muted)
                    .hover(|s| s.bg(p.wash(0.06)).text_color(p.text))
            }
        })
        .child(label.into())
}

/// A toggle that can be on alongside others (the token scopes): a check
/// and the accent when on. The caller adds the click.
pub fn toggle_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on: bool,
    p: &Palette,
) -> Stateful<Div> {
    let p = *p;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(26.))
        .px(px(9.))
        .rounded(px(7.))
        .ts(12., 16.)
        .mono()
        .cursor_pointer()
        .map(|this| {
            if on {
                this.bg(p.accent_wash())
                    .text_color(p.accent)
                    .child(icon(ic::CHECK, 12., p.accent))
            } else {
                this.bg(p.wash(0.05))
                    .text_color(p.muted)
                    .hover(|s| s.bg(p.wash(0.09)).text_color(p.text))
            }
        })
        .child(label.into())
}

// ---- page frame ---------------------------------------------------------

/// The measure an admin screen's column is set to.
pub const PAGE_MAX: f32 = 760.;

/// The scrolling column an admin screen sits in: `max` wide at most,
/// centred in `room` (the width the screen has), with Zeron's 40 px
/// gutters. `scroll` is the shell's, so a screen opens at its top.
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
    max: f32,
    body: impl IntoElement,
) -> Stateful<Div> {
    let width = room.min(max).max(0.);
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .child(
            v_flex()
                .w(px(width))
                .mx_auto()
                .px(px(40.))
                .pt(px(10.))
                .pb(px(56.))
                .child(body),
        )
}

/// A page's title (20/26 Medium), its subtitle and the actions to its right.
pub fn page_header(
    title: impl Into<SharedString>,
    subtitle: Option<AnyElement>,
    actions: Vec<AnyElement>,
    p: &Palette,
) -> Div {
    h_flex()
        .items_start()
        .gap(px(16.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .ts(20., 26.)
                        .medium()
                        .text_color(p.text)
                        .child(title.into()),
                )
                .children(
                    subtitle.map(|s| div().mt(px(4.)).ts(13., 18.).text_color(p.muted).child(s)),
                ),
        )
        .when(!actions.is_empty(), |this| {
            this.child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .items_center()
                    .children(actions),
            )
        })
}

/// A labelled block: a muted sentence-case label over its card.
pub fn section(label: impl Into<SharedString>, body: impl IntoElement, p: &Palette) -> Div {
    section_with(label, Vec::new(), body, p)
}

/// [`section`] with things to the right of the label.
pub fn section_with(
    label: impl Into<SharedString>,
    trailing: Vec<AnyElement>,
    body: impl IntoElement,
    p: &Palette,
) -> Div {
    v_flex()
        .mt(px(28.))
        .gap(px(8.))
        .child(
            h_flex()
                .min_h(px(26.))
                .items_center()
                .gap(px(8.))
                .px(px(4.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .ts(13., 17.)
                        .text_color(p.muted)
                        .child(label.into()),
                )
                .children(trailing),
        )
        .child(body)
}

/// A section card holding `rows`, each inset 16 px with a divider above all
/// but the first.
pub fn card(rows: Vec<AnyElement>, p: &Palette) -> Div {
    let mut card = v_flex().w_full().rounded(px(12.)).bg(p.card());
    for (i, row) in rows.into_iter().enumerate() {
        card = card.child(
            v_flex()
                .mx(px(16.))
                .when(i > 0, |this| this.border_t_1().border_color(p.divider()))
                .child(row),
        );
    }
    card
}

/// A card with free content and its own padding, for what is not a list.
pub fn panel(p: &Palette) -> Div {
    v_flex().w_full().rounded(px(12.)).bg(p.card()).p(px(16.))
}

/// One row of a [`card`]: an optional tile, a title over a meta line, the
/// row's controls on the right, and details under the text column.
#[derive(IntoElement, Default)]
pub struct Row {
    tile: Option<AnyElement>,
    title: Option<AnyElement>,
    meta: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    details: Vec<AnyElement>,
}

impl Row {
    pub fn new() -> Row {
        Row::default()
    }

    pub fn tile(mut self, tile: impl IntoElement) -> Row {
        self.tile = Some(tile.into_any_element());
        self
    }

    /// A plain title.
    pub fn title(self, text: impl Into<SharedString>) -> Row {
        self.title_el(div().child(text.into()))
    }

    /// A title made of more than words (a name with badges after it).
    pub fn title_el(mut self, el: impl IntoElement) -> Row {
        self.title = Some(el.into_any_element());
        self
    }

    /// A plain meta line.
    pub fn meta(self, text: impl Into<SharedString>) -> Row {
        self.meta_el(div().child(text.into()))
    }

    pub fn meta_el(mut self, el: impl IntoElement) -> Row {
        self.meta = Some(el.into_any_element());
        self
    }

    pub fn trailing(mut self, el: impl IntoElement) -> Row {
        self.trailing.push(el.into_any_element());
        self
    }

    pub fn detail(mut self, el: impl IntoElement) -> Row {
        self.details.push(el.into_any_element());
        self
    }
}

impl RenderOnce for Row {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = *palette(cx);
        let indent = if self.tile.is_some() { 48. } else { 0. };
        v_flex()
            .w_full()
            .py(px(12.))
            .min_h(px(60.))
            .justify_center()
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .children(self.tile)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(120.))
                            .gap(px(2.))
                            .children(
                                self.title.map(|t| {
                                    div().ts(13., 17.).medium().text_color(p.text).child(t)
                                }),
                            )
                            .children(
                                self.meta
                                    .map(|m| div().ts(12., 16.).text_color(p.muted).child(m)),
                            ),
                    )
                    .when(!self.trailing.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .flex_none()
                                .gap(px(6.))
                                .items_center()
                                .children(self.trailing),
                        )
                    }),
            )
            .when(!self.details.is_empty(), |this| {
                this.child(
                    v_flex()
                        .mt(px(10.))
                        .pl(px(indent))
                        .gap(px(10.))
                        .children(self.details),
                )
            })
    }
}

/// A label and its value on one line, in a details block.
pub fn kv(label: impl Into<SharedString>, value: impl IntoElement, p: &Palette) -> Div {
    h_flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .w(px(112.))
                .flex_none()
                .ts(12., 18.)
                .text_color(p.faint)
                .child(label.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .ts(12.5, 18.)
                .text_color(p.text)
                .child(value),
        )
}

// ---- strips, code, empty states ----------------------------------------

/// What a [`strip`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Danger,
    Warning,
    Note,
}

/// A tinted strip for something the operator has to know or act on: a 6 %
/// tint of the status colour, a 20 % rim, and text in its muted shade.
pub fn strip(
    severity: Severity,
    title: Option<SharedString>,
    body: Option<AnyElement>,
    actions: Vec<AnyElement>,
    p: &Palette,
) -> Div {
    let (base, text, glyph) = match severity {
        Severity::Danger => (p.danger, p.danger_muted, ic::DANGER),
        Severity::Warning => (p.warning, p.warning_muted, ic::DANGER),
        Severity::Note => (p.accent, p.text, ic::INFO),
    };
    h_flex()
        .w_full()
        .items_start()
        .gap(px(10.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .bg(base.alpha(0.06))
        .border_1()
        .border_color(base.alpha(0.20))
        .child(div().pt(px(1.)).child(icon(glyph, 16., text.opacity(0.9))))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(3.))
                .children(title.map(|t| div().ts(13., 17.).medium().text_color(text).child(t)))
                .children(body.map(|b| {
                    div()
                        .ts(12.5, 18.)
                        .text_color(text.opacity(if severity == Severity::Note {
                            0.75
                        } else {
                            0.9
                        }))
                        .child(b)
                })),
        )
        .when(!actions.is_empty(), |this| {
            this.child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .items_center()
                    .children(actions),
            )
        })
}

/// A block of monospaced text on the terminal plate, for a command or a
/// file's worth of lines. `copy` adds an icon button that copies `text`.
pub fn code_block(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    copy: bool,
    p: &Palette,
) -> Stateful<Div> {
    let text: SharedString = text.into();
    let copied = text.clone();
    div()
        .id(id)
        .relative()
        .w_full()
        .rounded(px(10.))
        .bg(p.terminal)
        .border_1()
        .border_color(p.hairline(0.06))
        .px(px(14.))
        .py(px(11.))
        .ts(12., 19.)
        .mono()
        .text_color(p.text)
        .child(text)
        .when(copy, |this| {
            this.child(
                div().absolute().top(px(6.)).right(px(6.)).child(
                    Btn::icon_only("copy", ic::COPY)
                        .small()
                        .tooltip("Copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copied.to_string(),
                            ));
                        }),
                ),
            )
        })
}

/// A centred glyph, a title and a line of help, for a pane with nothing in it.
pub fn empty_state(
    path: &'static str,
    title: impl Into<SharedString>,
    help: impl Into<SharedString>,
    p: &Palette,
) -> Div {
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .px(px(24.))
        .py(px(40.))
        .child(icon(path, 28., p.muted.opacity(0.45)))
        .child(
            div()
                .mt(px(10.))
                .ts(14., 19.)
                .medium()
                .text_color(p.text)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(320.))
                .ts(12.5, 18.)
                .text_center()
                .text_color(p.muted)
                .child(help.into()),
        )
}

/// A text field in the window's style. Its own `Input` keeps the caret,
/// the selection and the focus ring (the accent, from the theme).
pub fn field(state: &gpui::Entity<InputState>, p: &Palette) -> Input {
    Input::new(state)
        .bg(p.field())
        .rounded(px(8.))
        .h(px(32.))
        .px(px(10.))
        .text_size(px(13.))
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
