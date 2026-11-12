//! The Tokens scene ([`Screen::Kit`](crate::shell::Screen::Kit)): the kit's
//! parts in every state, drawn in the window for the harness to compare with
//! the Tokens board's parts, and beside them the face plates and the deck
//! in both cases.
//!
//! The parts are the board's, row for row and word for word: Buttons,
//! States, Switch and radio, Level, Segment and popup, Field and link,
//! State, Needs you, Keys and Tile. Two rows the board has not are added:
//! every other control drawn with the keyboard's ring, which is how each
//! ring is checked by eye. The token table and the rules are documentation
//! and are not drawn.
//!
//! **It is a live scene.** Every control does what it does on a page — the
//! switches switch, the level moves, the menu opens, Tab walks them — on a
//! little state of its own that starts as the board draws it, so the first
//! frame is the board and the window is also the place to try the kit by
//! hand. It never writes to the service. Its words are the board's and are
//! not translated: the scene is for the harness, not for the owner.
use crate::art::{plate, plate_title, Device, FACES};
use crate::prefs::Case;
use crate::shell::Shell;
use crate::style::{palette, Palette};
use crate::ui::{
    self, keycap, link_out, radio_dot, status, Btn, Chip, Level, Mod, PopupButton, Press,
    Segmented, Switch, Type as _,
};
use deck_model::{Template, Tone};
use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, SharedString, Window,
};
use gpui_component::input::InputState;
use gpui_component::menu::PopupMenuItem;
use gpui_component::{h_flex, v_flex};

/// The board's Idle home choices, for its menu button.
const IDLE: [&str; 4] = ["Never", "30 s", "1 min", "2 min"];

/// The board's text sizes, for its segmented control.
const SIZES: [&str; 3] = ["Small", "Medium", "Large"];

/// The parts' column: the board's, 424 wide with its 92 px names.
const PARTS_WIDTH: f32 = 424.;
const PART_NAME: f32 = 92.;
/// The pictures' column: what is left of the page's 764 after a 24 px gap.
const ART_WIDTH: f32 = 316.;
/// The plates are drawn at this, four to a row.
const PLATE: f32 = 70.;

/// What the scene's controls are set to. It starts as the board draws them.
pub struct KitPane {
    field: Entity<InputState>,
    switches: [bool; 2],
    level: u16,
    size: usize,
    idle: usize,
    chips: [bool; 2],
}

impl KitPane {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Shell>) -> KitPane {
        KitPane {
            field: cx.new(|cx| InputState::new(window, cx).default_value("screenshot")),
            switches: [true, false],
            level: 6,
            size: 1,
            idle: 3,
            chips: [true, false],
        }
    }
}

/// A press that does nothing: a part is drawn enabled, and is a Tab stop,
/// only when it has something to do.
fn nothing(_: &ClickEvent, _: &mut Window, _: &mut App) {}

/// One row of the board's parts: its name, then the parts 8 px apart.
fn part(name: &'static str, p: &Palette, parts: impl IntoIterator<Item = AnyElement>) -> Div {
    h_flex()
        .gap(px(12.))
        .min_h(px(40.))
        .child(
            div()
                .w(px(PART_NAME))
                .flex_none()
                .ts(12., 16.)
                .text_color(p.text3)
                .child(name),
        )
        .child(h_flex().flex_1().min_w_0().gap(px(8.)).children(parts))
}

/// The board's 6 px between two parts that read as one pair.
fn spacer() -> AnyElement {
    div().flex_none().w(px(6.)).into_any_element()
}

impl Shell {
    /// [`Screen::Kit`](crate::shell::Screen::Kit).
    pub(crate) fn render_kit(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let header = ui::Header::new(
            "Tokens and parts",
            "One small system taken from the deck's own screen.",
        );
        let columns = h_flex()
            .items_start()
            .gap(px(24.))
            .child(self.kit_parts(&p, cx))
            .child(self.kit_art(&p));
        let body = v_flex().gap(px(20.)).child(header).child(columns);
        ui::page("kit", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// The board's parts, row for row, and the keyboard's rings.
    fn kit_parts(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let kit = &self.kit;
        let switch = |i: usize, id: &'static str| {
            Switch::new(id, kit.switches[i]).on_toggle(cx.listener(
                move |this, on: &bool, _, cx| {
                    this.kit.switches[i] = *on;
                    cx.notify();
                },
            ))
        };
        let chip = |i: usize, id: &'static str, m: Mod| {
            Chip::modifier(id, m, kit.chips[i]).on_toggle(cx.listener(
                move |this, on: &bool, _, cx| {
                    this.kit.chips[i] = *on;
                    cx.notify();
                },
            ))
        };
        let level = Level::new("tok-level", kit.level, 0, 10).on_change(cx.listener(
            |this, stop: &u16, _, cx| {
                this.kit.level = *stop;
                cx.notify();
            },
        ));
        let sizes = Segmented::new("tok-size", SIZES.map(SharedString::from).to_vec(), kit.size)
            .on_select(cx.listener(|this, at: &usize, _, cx| {
                this.kit.size = *at;
                cx.notify();
            }));
        let idle = self.kit_idle("tok-idle", cx);
        let field = div()
            .flex_none()
            .w(px(112.))
            .child(ui::field(&kit.field, p));
        // The board's four tiles: the plate alone, selected, with the
        // keyboard, and both.
        let tile = |id: &'static str| {
            Press::new(id)
                .radius(8.)
                .flex_none()
                .size(px(40.))
                .child(plate(Some(Template::Macropad), 40., Some(8.), None))
        };
        // Room above and below for the outer rings, 8 px out at most.
        let tiles = h_flex()
            .flex_1()
            .min_w_0()
            .py(px(8.))
            .gap(px(18.))
            .child(tile("tok-tile-rest").rest(p.tile_rest))
            .child(tile("tok-tile-selected").selected(true))
            .child(tile("tok-tile-focused").rest(p.tile_rest).shown_focused())
            .child(tile("tok-tile-both").selected(true).shown_focused())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ts(12., 16.)
                    .text_color(p.text3)
                    .child(ui::para("rest, selected, focused, both")),
            );
        // As the board stacks them: the label, 4 px, then a row every 40.
        v_flex()
            .flex_none()
            .w(px(PARTS_WIDTH))
            .child(ui::label("Parts", p))
            .child(div().h(px(4.)))
            .child(part(
                "Buttons",
                p,
                [
                    Btn::new("tok-repair", "Repair").primary().on_click(nothing),
                    Btn::new("tok-check", "Check").on_click(nothing),
                    Btn::new("tok-remove", "Remove\u{2026}")
                        .quiet()
                        .on_click(nothing),
                    Btn::new("tok-uninstall", "Uninstall\u{2026}")
                        .danger()
                        .on_click(nothing),
                ]
                .map(IntoElement::into_any_element),
            ))
            .child(part(
                "States",
                p,
                [
                    Btn::new("tok-rest", "Rest").on_click(nothing),
                    Btn::new("tok-focus", "Focus")
                        .on_click(nothing)
                        .shown_focused(),
                    Btn::new("tok-disabled", "Disabled")
                        .disabled(true)
                        .on_click(nothing),
                ]
                .map(IntoElement::into_any_element),
            ))
            .child(part(
                "Switch, radio",
                p,
                [
                    switch(0, "tok-switch-on").into_any_element(),
                    switch(1, "tok-switch-off").into_any_element(),
                    radio_dot(true, p).into_any_element(),
                    radio_dot(false, p).into_any_element(),
                ],
            ))
            .child(part("Level", p, [level.into_any_element()]))
            .child(part(
                "Segment, popup",
                p,
                [sizes.into_any_element(), idle.into_any_element()],
            ))
            .child(part(
                "Field, link",
                p,
                [
                    field.into_any_element(),
                    spacer(),
                    link_out("tok-link", "Join the waitlist", p)
                        .on_click(nothing)
                        .into_any_element(),
                ],
            ))
            .child(part(
                "State",
                p,
                [
                    status("Connected", Tone::Green, p).into_any_element(),
                    spacer(),
                    status("Not set up", Tone::Gray, p).into_any_element(),
                ],
            ))
            .child(part(
                "Needs you",
                p,
                [
                    status("Needs repair", Tone::Amber, p),
                    status("Failed", Tone::Red, p),
                    status("Looking", Tone::Blue, p),
                ]
                .map(IntoElement::into_any_element),
            ))
            .child(part(
                "Keys",
                p,
                [
                    keycap("PTT", p).into_any_element(),
                    chip(0, "tok-shift", Mod::Shift).into_any_element(),
                    chip(1, "tok-option", Mod::Option).into_any_element(),
                    keycap("4", p).into_any_element(),
                ],
            ))
            .child(part("Tile", p, [tiles.into_any_element()]))
            .child(part(
                "Focus",
                p,
                [
                    Switch::new("tok-focus-switch", true)
                        .on_toggle(|_, _, _| {})
                        .shown_focused()
                        .into_any_element(),
                    Chip::modifier("tok-focus-chip", Mod::Command, false)
                        .on_toggle(|_, _, _| {})
                        .shown_focused()
                        .into_any_element(),
                    self.kit_idle("tok-focus-idle", cx)
                        .shown_focused()
                        .into_any_element(),
                    Segmented::new("tok-focus-case", vec!["White".into(), "Black".into()], 0)
                        .on_select(|_, _, _| {})
                        .shown_focused(1)
                        .into_any_element(),
                ],
            ))
            .child(part(
                "Focus, level",
                p,
                [Level::new("tok-focus-level", 4, 1, 5)
                    .width(96.)
                    .on_change(|_, _, _| {})
                    .shown_focused()
                    .into_any_element()],
            ))
    }

    /// The board's menu button, "2 min", with its four choices.
    fn kit_idle(&self, id: &'static str, cx: &Context<Self>) -> PopupButton {
        let at = self.kit.idle;
        let shell = cx.entity();
        PopupButton::new(id, IDLE[at]).menu(move |mut menu, _, _| {
            for (i, choice) in IDLE.into_iter().enumerate() {
                let shell = shell.clone();
                menu = menu.item(PopupMenuItem::new(choice).checked(i == at).on_click(
                    move |_, _, cx| {
                        shell.update(cx, |this, cx| {
                            this.kit.idle = i;
                            cx.notify();
                        });
                    },
                ));
            }
            menu
        })
    }

    /// Every face's plate, and the deck in both cases: white and linked,
    /// showing Agents as the Deck page does (its plate titled at 104 px),
    /// and black with its PTT key lit and the splash, as first run shows it.
    fn kit_art(&self, p: &Palette) -> Div {
        let plates = div()
            .grid()
            .grid_cols(4)
            .gap(px(12.))
            .children(FACES.map(|face| {
                v_flex()
                    .gap(px(6.))
                    .child(plate(Some(face), PLATE, Some(12.), None))
                    .child(
                        div()
                            .ts(12., 16.)
                            .text_color(p.text3)
                            .child(format!("{face:?}")),
                    )
            }));
        let devices = h_flex()
            .items_start()
            .gap(px(12.))
            .child(
                Device::new(Case::White, Some(Template::Agents), 164.)
                    .title(plate_title(Template::Agents)),
            )
            .child(
                Device::new(Case::Black, None, 140.)
                    .ptt_lit(true)
                    .splash(true),
            );
        v_flex()
            .flex_none()
            .w(px(ART_WIDTH))
            .gap(px(20.))
            .child(ui::section("Plates", plates, p))
            .child(ui::section("Devices", devices, p))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tokens_scene_says_nothing_through_the_tables() {
        // Its words are the board's, for the harness: a lookup here would
        // put a row in a table the owner never reads.
        assert!(crate::i18n::literals(include_str!("kitboard.rs")).is_empty());
    }
}
