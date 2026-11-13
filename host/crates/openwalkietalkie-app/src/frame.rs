//! The frame around the pages: the sidebar — the traffic lights' place, the
//! brand, the five pages and, at its foot, whether the deck is linked — and
//! the strips over the main column while no service answers (not running,
//! never set up, or held by macOS for the owner's approval, each with what
//! can be done about it) or the mirror is being read again. The status item
//! is next door, in [`crate::menu`].
//!
//! The frame keeps its part of the contract with the shell: the shell asks
//! it for [`Shell::render_sidebar`] and [`Shell::render_strips`] and nothing
//! else. What either says is decided by a pure function of the mirror —
//! [`foot`] for the sidebar's foot card, [`strip`] for the strips — so the
//! words are tested without a window.
//!
//! The foot card says once what the Deck page shows in full: on the Deck it
//! is only whether the deck is linked, and on every other page it adds how
//! (the transport, the charge, the firmware), so the deck stays in sight
//! wherever the window is.
use crate::art;
use crate::deck::{no_service_title, NoService};
use crate::present;
use crate::shell::{Screen, Shell, SIDEBAR_WIDTH};
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Layout, Row, Type as _, PAGE_END, PAGE_SIDE, PAGE_WIDTH};
use deck_gui_core::i18n::Language;
use deck_model::{DeviceState, Link, Tone, Transport};
use gpui::{div, prelude::*, px, AnyElement, Context, Div};
use gpui_component::{h_flex, v_flex, StyledExt as _};

/// The traffic lights' row at the sidebar's top: macOS draws the lights,
/// the sidebar keeps their place (12 px circles in the design, centred 26
/// px down: 20 of padding, then this row).
const LIGHTS_ROW: f32 = 12.;

/// Geist's ascent and descent in ems (its `hhea`, which the boards'
/// browser and gpui both read).
const ASCENT: f32 = 1.005;
const DESCENT: f32 = 0.295;

/// How far below the top of a line `line` high text of `size` has its
/// baseline: half the leading, then the ascent.
const fn baseline(size: f32, line: f32) -> f32 {
    (line - size * (ASCENT + DESCENT)) / 2. + size * ASCENT
}

/// The foot card's firmware line. The design sets the 12/16 mono span in
/// a line of the window's own 13/18 text, so the line is 18 high and the
/// span sits on that line's baseline, this far below its top.
const FIRMWARE_LINE: f32 = 18.;
const FIRMWARE_DROP: f32 = baseline(13., FIRMWARE_LINE) - baseline(12., 16.);

/// What the sidebar's foot card says about the deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foot {
    /// The service is there and the deck's link is up — degraded counts,
    /// as everywhere: a deck that is linked and silent is linked.
    pub linked: bool,
    /// The dot: green while linked, amber while linked and not answering,
    /// the neutral grey while not linked.
    pub tone: Tone,
    /// How the deck is linked, under the title: on every page but the
    /// Deck, which shows all of it, and only while it is linked.
    pub details: Option<FootDetails>,
}

/// The foot card's second and third lines: how the deck is linked, its
/// charge and its firmware, each only as far as the deck has said it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FootDetails {
    /// The bus the deck is on; `None` while it has not said (a service
    /// from before the deck reported it), which draws no word.
    pub transport: Option<Transport>,
    /// The charge in percent; `None` while the deck does not know, which
    /// draws neither the glyph nor the figure.
    pub battery: Option<u8>,
    /// `0.13.0 · Rev B` ([`present::fw_text`]); `None` while the deck has
    /// named neither its firmware nor its board.
    pub firmware: Option<String>,
}

impl Foot {
    /// The card's title, in `language`.
    pub fn title(&self, language: Language) -> &'static str {
        if self.linked {
            language.text("Deck linked")
        } else {
            language.text("Not linked")
        }
    }
}

impl FootDetails {
    /// The transport, in a word: "Bluetooth" or "Cable".
    pub fn transport_word(&self, language: Language) -> Option<&'static str> {
        self.transport.map(|transport| match transport {
            Transport::Bluetooth => language.text("Bluetooth"),
            Transport::Usb => language.text("Cable"),
        })
    }

    /// The charge as the figure beside the glyph: `85%`.
    pub fn percent(&self, language: Language) -> Option<String> {
        self.battery
            .map(|battery| language.format("{0}%", &[&battery.to_string()]))
    }
}

/// The foot card for `device` as the service last reported it, on
/// `screen`. `connected` is the socket, not the mirror: with the service
/// gone the mirror is the last one it sent, and a deck it says is linked
/// is linked to nothing now.
pub fn foot(device: &DeviceState, connected: bool, screen: Screen) -> Foot {
    let linked = connected && device.link != Link::Offline;
    if !linked {
        return Foot {
            linked,
            tone: Tone::Gray,
            details: None,
        };
    }
    let tone = if device.link == Link::Degraded {
        Tone::Amber
    } else {
        Tone::Green
    };
    let details = (screen != Screen::Deck).then(|| FootDetails {
        transport: device.transport,
        battery: device.battery,
        firmware: (device.fw.is_some() || device.board.is_some()).then(|| {
            present::fw_text(device.fw.as_deref().unwrap_or_default(), device.board.as_deref())
        }),
    });
    Foot {
        linked,
        tone,
        details,
    }
}

/// A strip over the main column, above the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strip {
    /// The service is not answering, and why: not running (**Start**,
    /// **Open Log**), never set up (**Set Up…**), or held by macOS for the
    /// owner's approval (**Open Settings**).
    NoService(NoService),
    /// The mirror had a hole in it and is being read again.
    Resync,
}

/// The strip `screen` shows, if any; `absent` is why no service answers,
/// read only while none does. The Deck page says that in a section of its
/// own, so it has no strip for it, and the harness's own screens (the
/// Tokens and MenuBar boards) draw none. A mirror being read again is said
/// on every page, the Deck's included.
pub fn strip(connected: bool, resyncing: bool, absent: NoService, screen: Screen) -> Option<Strip> {
    match screen {
        Screen::Kit | Screen::MenuPreview => None,
        Screen::Deck if !connected => None,
        _ if !connected => Some(Strip::NoService(absent)),
        _ => resyncing.then_some(Strip::Resync),
    }
}

impl Shell {
    /// The sidebar: the lights' row, the brand, a row per page, and the
    /// deck at its foot. A page is fetched on arrival rather than polled,
    /// so a row's click is a [`Shell::go`].
    ///
    /// The five rows are one stop on the Tab order ([`ui::Group`]): Tab
    /// reaches the page that is up, Up and Down move the keyboard along the
    /// rows, and Return or Space opens the one that has it.
    pub(crate) fn render_sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let row = |screen: Screen| {
            ui::nav_item(
                screen.id(),
                screen.icon(),
                self.tr(screen.label()),
                self.screen.nav() == Some(screen),
                &p,
            )
            .on_click(cx.listener(move |this, _, window, cx| this.go(screen, window, cx)))
        };
        let current = Screen::NAV
            .iter()
            .position(|&screen| Some(screen) == self.screen.nav());
        let pages = ui::Group::new("nav", Layout::Column)
            .v_flex()
            .gap(px(2.))
            .selected(current)
            .members(Screen::NAV.map(row));
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .bg(p.side)
            .border_r_1()
            .border_color(p.line)
            .pt(px(20.))
            .px(px(12.))
            .pb(px(12.))
            .gap(px(18.))
            .child(div().flex_none().h(px(LIGHTS_ROW)))
            .child(div().px(px(6.)).child(ui::brand()))
            .child(pages)
            .child(div().flex_1())
            .child(self.render_foot(&p))
            .into_any_element()
    }

    /// The sidebar's foot card: a dot and whether the deck is linked, then
    /// (on every page but the Deck) how, its charge and its firmware.
    fn render_foot(&self, p: &Palette) -> Div {
        let language = self.language();
        let card = foot(
            &self.state.mirror().device,
            self.state.connected(),
            self.screen,
        );
        let head = h_flex()
            .gap(px(8.))
            .child(ui::dot(p.tone(card.tone), 7.))
            .child(
                div()
                    .ts(13., 18.)
                    .semibold()
                    .text_color(p.text)
                    .child(card.title(language)),
            );
        let (link, firmware) = match &card.details {
            Some(details) => (foot_link(details, language, p), details.firmware.clone()),
            None => (None, None),
        };
        v_flex()
            .flex_none()
            .gap(px(6.))
            .px(px(12.))
            .py(px(11.))
            .rounded(px(8.))
            .bg(p.surf)
            .border_1()
            .border_color(p.line)
            .child(head)
            .children(link)
            .children(firmware.map(|firmware| {
                div()
                    .h(px(FIRMWARE_LINE))
                    .pt(px(FIRMWARE_DROP))
                    .child(ui::mono(firmware, p.text3).truncate())
            }))
    }

    /// The strip over every page but the Deck while no service answers, in
    /// the Deck page's words for why ([`no_service_title`]) and with its
    /// buttons, and the quieter line while the mirror is being read again.
    /// Neither is an error state for the window: the mirror underneath
    /// stays on screen, dimmed.
    pub(crate) fn render_strips(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let p = *palette(cx);
        let shown = strip(
            self.state.connected(),
            self.state.resyncing(),
            self.service_absence(),
            self.screen,
        )?;
        // The strip sits where a page's column does: inside the main
        // column's padding, as wide as its content.
        let width = self.content_width.clamp(0., PAGE_WIDTH + 2. * PAGE_SIDE);
        let place = |body: Div| {
            div()
                .flex_none()
                .w(px(width))
                .px(px(PAGE_SIDE))
                .pt(px(PAGE_END))
                .child(body)
                .into_any_element()
        };
        Some(place(match shown {
            Strip::NoService(why) => {
                let row = Row::new()
                    .lead(ui::bare(ui::icon(ic::DANGER, 18., p.warn)))
                    .title(no_service_title(why, self.language()));
                let row = self
                    .no_service_buttons(why, "strip-service", false, cx)
                    .into_iter()
                    .fold(row, Row::right);
                ui::attention(row, &p)
            }
            Strip::Resync => h_flex()
                .w_full()
                .gap(px(8.))
                .px(px(12.))
                .h(px(30.))
                .rounded(px(8.))
                .bg(p.well)
                .ts(12., 16.)
                .text_color(p.text2)
                .child(ui::spinner(13., p.text2))
                .child(self.tr("Re-reading the service's state\u{2026}")),
        }))
    }
}

/// The foot card's line about the link: the transport on the left, the
/// battery and its figure on the right. Nothing when the deck has said
/// neither.
fn foot_link(details: &FootDetails, language: Language, p: &Palette) -> Option<Div> {
    let word = details.transport_word(language);
    let percent = details.percent(language);
    if word.is_none() && details.battery.is_none() {
        return None;
    }
    Some(
        h_flex()
            .gap(px(8.))
            .ts(12., 16.)
            .text_color(p.text2)
            .child(div().flex_1().min_w_0().truncate().children(word))
            .children(details.battery.map(|battery| art::battery(battery, p)))
            .children(percent.map(|percent| div().flex_none().tnum().child(percent))),
    )
}
